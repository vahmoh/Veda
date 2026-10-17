// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Notification bubbles, stacked above the taskbar in the bottom-right
//! corner. They disappear after a few seconds (not while hovered) or when
//! clicked. A notification an application posted shows that application's
//! tile and name, whatever it says of itself (see `Shell::notify`).
//!
//! The agent's approval requests appear here while its window is closed:
//! they carry Allow and Deny buttons and stay until answered (or until the
//! request expires). So do the firewall's questions ([`crate::firewall`]),
//! a few at a time, the others waiting their turn.

use vgfx::{Align, Rect};
use vnet::FirewallQuestion;
use vproto::display::{WindowKind, WindowSpec};
use vrt::println;
use vui::window::Display;
use vui::{Font, Host, Icon, Ui};

use vproto::agent::ApprovalRequest;

use crate::approval::{self, Choice, Look};
use crate::{Model, apps, chrome, firewall, taskbar};

const WIDTH: i32 = 360;
const HEIGHT: i32 = 88;
/// Height of an approval request (a firewall question's fits what it
/// says).
const APPROVAL_HEIGHT: i32 = 136;
const GAP: i32 = 10;
const LIFETIME_NS: u64 = 6_000_000_000;
const MAX_SHOWN: usize = 4;
/// Firewall questions shown at once.
const MAX_QUESTIONS_SHOWN: usize = 2;

/// The tile a notification shows and who it is from.
pub struct Sender<'a> {
    /// The tile's colour key (an application's id) and glyph.
    pub tile: (&'a str, Icon),
    /// The application that posted it (shown with its title), if not the
    /// system.
    pub name: Option<&'a str>,
}

/// What a notification shows, and what happened to it this frame.
struct View {
    title: String,
    body: String,
    icon: Icon,
    /// Tile colour key.
    color: String,
    /// The application that posted it.
    from: Option<String>,
    origin: (i32, i32),
    hovered: bool,
    clicked: bool,
    /// An approval request, and "Always allow" as ticked on it.
    approval: Option<(ApprovalRequest, Choice)>,
    /// A firewall question, "From anywhere" as ticked on it, and when its
    /// buttons start to take clicks.
    question: Option<(FirewallQuestion, bool, u64)>,
}

impl View {
    fn update(&mut self, ui: &mut Ui, m: &mut Model) {
        let t = ui.theme().clone();
        let (w, h) = (ui.width, ui.height);
        chrome::popup_background(&mut ui.canvas, &m.wallpaper.blurred, self.origin);
        if let Some((req, choice)) = &mut self.approval {
            let r = ui.rect();
            self.clicked |= approval::card(ui, m, req, r, choice, Look::Notification);
            chrome::popup_frame(&mut ui.canvas);
            return;
        }
        if let Some((q, anywhere, armed_at)) = &mut self.question {
            let r = ui.rect();
            self.clicked |= firewall::card(ui, m, q, r, anywhere, *armed_at);
            chrome::popup_frame(&mut ui.canvas);
            return;
        }
        let resp = ui.interact(ui.id("note"), ui.rect());
        self.hovered = resp.hovered;
        self.clicked |= resp.clicked;
        apps::draw_tile(&mut ui.canvas, Rect::new(18, (h - 42) / 2, 42, 42), &self.color, self.icon);
        let mut text_w = w - 76 - 36;
        // Who posted it, at the end of the title's line.
        if let Some(from) = &self.from {
            let font = ui.ctx.font(Font::Regular);
            let from = ui.ctx.text.ellipsize(font, 12.0, from, (text_w / 2) as f32);
            let fw = ui.measure(&from, Font::Regular, 12.0) as i32;
            ui.label(
                Rect::new(76 + text_w - fw, 16, fw + 2, 22),
                &from,
                Font::Regular,
                12.0,
                t.text_faint,
                Align::Left,
            );
            text_w -= fw + 8;
        }
        let font = ui.ctx.font(Font::Bold);
        let title = ui.ctx.text.ellipsize(font, 14.0, &self.title, text_w as f32);
        ui.label(Rect::new(76, 16, text_w, 22), &title, Font::Bold, 14.0, t.text, Align::Left);
        ui.paragraph(Rect::new(76, 40, w - 76 - 18, h - 46), &self.body, 13.0, t.text_dim);
        if resp.hovered {
            ui.icon(Rect::new(w - 34, 10, 22, 22), Icon::Close, 12.0, t.text_dim);
            ui.set_cursor(vui::Cursor::Hand);
        }
        chrome::popup_frame(&mut ui.canvas);
    }
}

struct Note {
    host: Host,
    view: View,
    expires: u64,
    height: i32,
}

impl Note {
    fn height(&self) -> i32 {
        self.height
    }

    /// The approval request it shows, if it is one.
    fn approval_id(&self) -> Option<u64> {
        self.view.approval.as_ref().map(|(r, _)| r.id)
    }

    /// The application the firewall question it shows is about, if it is
    /// one.
    fn question_app(&self) -> Option<&str> {
        self.view.question.as_ref().map(|(q, ..)| q.app.as_str())
    }

    /// A notification that goes away by itself.
    fn passing(&self) -> bool {
        self.view.approval.is_none() && self.view.question.is_none()
    }
}

pub struct Notifications {
    notes: Vec<Note>,
}

impl Notifications {
    pub fn new() -> Notifications {
        Notifications { notes: Vec::new() }
    }

    /// Where a note goes: stacked upwards from above the taskbar.
    fn origin_for(&self, screen: Rect, slot: usize, height: i32) -> (i32, i32) {
        let below: i32 = self.notes.iter().take(slot).map(|n| n.height() + GAP).sum();
        (screen.w - WIDTH - 14, screen.h - taskbar::HEIGHT - 14 - height - below)
    }

    /// Shows an approval request from the agent (until it is answered).
    pub fn post_approval(&mut self, display: &Display, m: &Model, req: &ApprovalRequest) {
        if self.notes.iter().any(|n| n.approval_id() == Some(req.id)) {
            return;
        }
        let origin = self.origin_for(m.screen, self.notes.len(), APPROVAL_HEIGHT);
        let mut spec = WindowSpec::new("Approval", WIDTH as u32, APPROVAL_HEIGHT as u32);
        spec.kind = WindowKind::Notification;
        spec.x = origin.0;
        spec.y = origin.1;
        spec.resizable = false;
        spec.app_id = "shell".into();
        match Host::new(display, spec) {
            Ok(host) => {
                let view = View {
                    title: req.action.clone(),
                    body: req.detail.clone(),
                    icon: Icon::Info,
                    color: String::new(),
                    from: None,
                    origin,
                    hovered: false,
                    clicked: false,
                    approval: Some((req.clone(), Choice::default())),
                    question: None,
                };
                self.notes.push(Note { host, view, expires: u64::MAX, height: APPROVAL_HEIGHT });
            }
            Err(e) => println!("cannot show an approval request: {:?}", e),
        }
    }

    /// Shows the firewall's questions as `m` has them, the oldest first and
    /// [`MAX_QUESTIONS_SHOWN`] at a time, and takes away those settled.
    pub fn show_questions(&mut self, display: &Display, m: &Model) {
        let before = self.notes.len();
        self.notes.retain(|n| n.question_app().is_none_or(|app| m.firewall.iter().any(|q| q.app == app)));
        if self.notes.len() != before {
            self.relayout(m.screen);
        }
        for q in &m.firewall {
            if self.notes.iter().filter(|n| n.question_app().is_some()).count() >= MAX_QUESTIONS_SHOWN {
                break;
            }
            if !self.notes.iter().any(|n| n.question_app() == Some(q.app.as_str())) {
                self.post_question(display, m, q);
            }
        }
    }

    fn post_question(&mut self, display: &Display, m: &Model, q: &FirewallQuestion) {
        let height = firewall::height(m, q, WIDTH);
        let origin = self.origin_for(m.screen, self.notes.len(), height);
        let mut spec = WindowSpec::new("Firewall", WIDTH as u32, height as u32);
        spec.kind = WindowKind::Notification;
        spec.x = origin.0;
        spec.y = origin.1;
        spec.resizable = false;
        spec.app_id = "shell".into();
        match Host::new(display, spec) {
            Ok(host) => {
                let view = View {
                    title: String::new(),
                    body: String::new(),
                    icon: Icon::Lock,
                    color: String::new(),
                    from: None,
                    origin,
                    hovered: false,
                    clicked: false,
                    approval: None,
                    question: Some((q.clone(), false, vrt::time::now_ns() + firewall::ARM_NS)),
                };
                self.notes.push(Note { host, view, expires: u64::MAX, height });
            }
            Err(e) => println!("cannot show the firewall's question: {:?}", e),
        }
    }

    /// Takes an answered or expired approval request away.
    pub fn remove_approval(&mut self, id: u64, screen: Rect) {
        let before = self.notes.len();
        self.notes.retain(|n| n.approval_id() != Some(id));
        if self.notes.len() != before {
            self.relayout(screen);
        }
    }

    /// Takes every approval request away (the agent's window shows them).
    pub fn remove_approvals(&mut self, screen: Rect) {
        let before = self.notes.len();
        self.notes.retain(|n| n.view.approval.is_none());
        if self.notes.len() != before {
            self.relayout(screen);
        }
    }

    /// Shows a notification from `sender`.
    pub fn post(&mut self, display: &Display, m: &Model, title: &str, body: &str, sender: Sender) {
        if self.notes.len() >= MAX_SHOWN
            && let Some(i) = self.notes.iter().position(Note::passing)
        {
            self.notes.remove(i);
            self.relayout(m.screen);
        }
        let origin = self.origin_for(m.screen, self.notes.len(), HEIGHT);
        let mut spec = WindowSpec::new(title, WIDTH as u32, HEIGHT as u32);
        spec.kind = WindowKind::Notification;
        spec.x = origin.0;
        spec.y = origin.1;
        spec.resizable = false;
        spec.app_id = "shell".into();
        match Host::new(display, spec) {
            Ok(host) => {
                let view = View {
                    title: title.into(),
                    body: body.into(),
                    icon: sender.tile.1,
                    color: sender.tile.0.into(),
                    from: sender.name.map(String::from),
                    origin,
                    hovered: false,
                    clicked: false,
                    approval: None,
                    question: None,
                };
                self.notes.push(Note { host, view, expires: vrt::time::now_ns() + LIFETIME_NS, height: HEIGHT });
            }
            Err(e) => println!("cannot show a notification: {:?}", e),
        }
    }

    /// Moves the notifications to their slots (after one went away).
    fn relayout(&mut self, screen: Rect) {
        let origins: Vec<(i32, i32)> =
            (0..self.notes.len()).map(|i| self.origin_for(screen, i, self.notes[i].height())).collect();
        for (n, o) in self.notes.iter_mut().zip(origins) {
            if o != n.view.origin {
                n.view.origin = o;
                n.host.window.set_position(o.0, o.1);
                n.host.invalidate();
            }
        }
    }

    pub fn pump(&mut self, m: &mut Model) {
        let now = vrt::time::now_ns();
        for n in &mut self.notes {
            n.host.pump(|ui| n.view.update(ui, m));
            if n.view.hovered {
                n.expires = n.expires.max(now + 2_000_000_000);
            }
        }
        let before = self.notes.len();
        self.notes.retain(|n| !n.view.clicked && now < n.expires && !n.host.window.closed);
        if self.notes.len() != before {
            self.relayout(m.screen);
        }
    }

    pub fn hosts(&self) -> impl Iterator<Item = &Host> {
        self.notes.iter().map(|n| &n.host)
    }

    /// When the next notification expires.
    pub fn deadline(&self) -> u64 {
        self.notes.iter().map(|n| n.expires).min().unwrap_or(vabi::DEADLINE_INFINITE)
    }

    pub fn invalidate_all(&mut self) {
        for n in &mut self.notes {
            n.host.invalidate();
        }
    }
}
