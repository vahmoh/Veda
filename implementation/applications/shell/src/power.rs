// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Restarting and shutting down without losing work.
//!
//! Restart and Shut down first ask every application window to close, as
//! its close button would: an application with unsaved work asks the user
//! what to do with it (the Text Editor's "Save changes?"). Once no window
//! is left, the home folder is written to disk and the machine powers off
//! (`Shell::power_off`). Windows still open after [`CLOSE_WAIT_NS`] are
//! listed on a card, from which the user cancels, or powers off anyway.

use vgfx::{Align, Rect};
use vproto::display::{WindowKind, WindowSpec};
use vproto::init::power;
use vui::window::Display;
use vui::{ButtonKind, Font, Host, Ui};

use crate::{Model, chrome, taskbar};

/// How long the applications have to close before the card shows what is
/// still open.
pub const CLOSE_WAIT_NS: u64 = 5_000_000_000;

const WIDTH: i32 = 440;
const HEIGHT: i32 = 250;
/// The windows the card lists by name (the rest are counted).
const LISTED: usize = 4;

/// What the user chose on the card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    /// Stay on: the windows that closed stay closed.
    Cancel,
    /// Power off with the windows still open.
    Force,
}

/// A restart or shutdown waiting for the applications to close.
pub struct PowerOff {
    /// `power::REBOOT` or `power::SHUTDOWN`.
    pub what: u32,
    /// When the card is due.
    card_at: u64,
    /// The card: its window and where it is on the screen.
    card: Option<(Host, (i32, i32))>,
}

impl PowerOff {
    pub fn new(what: u32, now: u64) -> PowerOff {
        PowerOff { what, card_at: now + CLOSE_WAIT_NS, card: None }
    }

    /// When it next needs a look (the card is due).
    pub fn deadline(&self) -> u64 {
        if self.card.is_some() { vabi::DEADLINE_INFINITE } else { self.card_at }
    }

    /// The card's window, once it shows.
    pub fn host(&self) -> Option<&Host> {
        self.card.as_ref().map(|(h, _)| h)
    }

    /// Shows the card once it is due and handles it; returns what the user
    /// chose.
    pub fn pump(&mut self, display: &Display, m: &Model, now: u64) -> Option<Choice> {
        if self.card.is_none() && now >= self.card_at {
            let x = (m.screen.w - WIDTH) / 2;
            let y = m.screen.h - taskbar::HEIGHT - 24 - HEIGHT;
            let mut spec = WindowSpec::new("Apps are still open", WIDTH as u32, HEIGHT as u32);
            spec.kind = WindowKind::Notification;
            spec.x = x;
            spec.y = y;
            spec.resizable = false;
            spec.app_id = "shell".into();
            match Host::new(display, spec) {
                Ok(host) => self.card = Some((host, (x, y))),
                Err(e) => println!("cannot show the apps still open: {:?}", e),
            }
        }
        let (host, origin) = self.card.as_mut()?;
        let (what, origin) = (self.what, *origin);
        let mut choice = None;
        host.pump(|ui| choice = card(ui, m, what, origin));
        choice
    }

    /// Redraws the card (the windows changed).
    pub fn invalidate(&mut self) {
        if let Some((h, _)) = &mut self.card {
            h.invalidate();
        }
    }
}

/// Draws the card: what is still open, Cancel and the power button.
fn card(ui: &mut Ui, m: &Model, what: u32, origin: (i32, i32)) -> Option<Choice> {
    let t = ui.theme().clone();
    let w = ui.width;
    chrome::popup_background(&mut ui.canvas, &m.wallpaper.blurred, origin);
    let restart = what == power::REBOOT;
    let heading = if restart { "Restarting\u{2026}" } else { "Shutting down\u{2026}" };
    ui.label(Rect::new(20, 16, w - 40, 24), heading, Font::Bold, 15.0, t.text, Align::Left);
    let note = "These apps are still open. Save what you need in them and close them, or go on without them.";
    ui.paragraph(Rect::new(20, 44, w - 40, 40), note, 12.5, t.text_dim);
    let mut y = 88;
    for win in m.windows.iter().take(LISTED) {
        let font = ui.ctx.font(Font::Regular);
        let title = if win.title.is_empty() { String::from("(untitled window)") } else { win.title.clone() };
        let title = ui.ctx.text.ellipsize(font, 13.0, &title, (w - 56) as f32);
        ui.label(Rect::new(28, y, w - 56, 20), &format!("\u{2022} {title}"), Font::Regular, 13.0, t.text, Align::Left);
        y += 20;
    }
    if m.windows.len() > LISTED {
        let more = format!("and {} more", m.windows.len() - LISTED);
        ui.label(Rect::new(28, y, w - 56, 20), &more, Font::Regular, 12.5, t.text_faint, Align::Left);
    }
    let by = HEIGHT - 50;
    let force = if restart { "Restart anyway" } else { "Shut down anyway" };
    let mut choice = None;
    if ui.button(Rect::new(w - 20 - 150 - 8 - 100, by, 100, 34), "Cancel") {
        choice = Some(Choice::Cancel);
    }
    if ui.button_full(Rect::new(w - 20 - 150, by, 150, 34), None, force, ButtonKind::Danger) {
        choice = Some(Choice::Force);
    }
    chrome::popup_frame(&mut ui.canvas);
    choice
}
