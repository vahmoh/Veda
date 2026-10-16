// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The firewall's questions. An application started taking connections
//! from other machines, and the firewall's settings say nothing about it:
//! a notification asks whether it may, from the local network (or, ticked,
//! from anywhere) or not at all, and stays until it is answered, there or
//! in Settings. Until then nothing from other machines reaches the
//! application.
//!
//! The answer goes to the network service ([`Action::FirewallAnswer`]),
//! which takes answers from the shell alone, and only about the
//! applications it asked about. The buttons take clicks only a moment
//! after the question appears, so that a click meant for what was under
//! the pointer just before does not answer it.

use vgfx::Rect;
use vnet::{FirewallProtocol, FirewallQuestion, Scope};
use vui::{ButtonKind, Font, Icon, SharedText, Ui};

use crate::{Action, Model, approval, apps};

/// How long after a question appears its buttons start to take clicks.
pub const ARM_NS: u64 = 1_000_000_000;

/// The height the card about `q` needs, `width` wide: its title (on one
/// line or two) and what it says, over the buttons.
pub fn height(m: &Model, q: &FirewallQuestion, width: i32) -> i32 {
    let (text, fonts) = SharedText::process();
    let (title, body) = wording(m, q);
    let w = (width - 76 - 14) as f32;
    let title_h = if text.wrap(fonts[Font::Bold as usize], 14.0, &title, w).len() > 1 { 38 } else { 20 };
    let regular = fonts[Font::Regular as usize];
    let lines = text.wrap(regular, 12.5, &body, w).len().max(1) as f32;
    let body_h = (lines * text.metrics(regular, 12.5).line_height).ceil() as i32;
    14 + title_h + 4 + body_h + 10 + 44
}

/// The question about `q.app`, as the card asks it: its title and what it
/// says.
fn wording(m: &Model, q: &FirewallQuestion) -> (String, String) {
    let protocol = match q.protocol {
        FirewallProtocol::Tcp => "TCP",
        FirewallProtocol::Udp => "UDP",
    };
    if q.app == "terminal" {
        return (
            String::from("Let programs run in the Terminal accept connections?"),
            format!(
                "A program run in the Terminal wants other computers to reach it ({protocol} port {}). What you allow, every program run there may do.",
                q.port
            ),
        );
    }
    let name = m.app(&q.app).map_or(q.app.clone(), |a| a.name.clone());
    (
        format!("Let {name} accept connections?"),
        format!("{name} wants other computers to reach it ({protocol} port {}). Until you answer, none can.", q.port),
    )
}

/// Draws the question `q` in `r` and handles its buttons: the answer goes
/// to `m` as [`Action::FirewallAnswer`]. `anywhere` is the tick on it;
/// the buttons take clicks from `armed_at` on. Returns whether the user
/// answered.
pub fn card(ui: &mut Ui, m: &mut Model, q: &FirewallQuestion, r: Rect, anywhere: &mut bool, armed_at: u64) -> bool {
    let th = ui.theme().clone();
    let (key, glyph) = match m.app(&q.app) {
        Some(app) => (app.id.clone(), apps::icon_for(app)),
        None => (String::from("program"), Icon::Lock),
    };
    apps::draw_tile(&mut ui.canvas, Rect::new(r.x + 18, r.y + 18, 42, 42), &key, glyph);
    let (title, body) = wording(m, q);
    let x = r.x + 76;
    let w = r.right() - 14 - x;
    let used = approval::title(ui, Rect::new(x, r.y + 14, w, 20), &title);
    let top = r.y + 14 + used + 4;
    let by = r.bottom() - 44;
    ui.paragraph(Rect::new(x, top, w, by - 6 - top), &body, 12.5, th.text_dim);
    ui.checkbox(Rect::new(r.x + 12, by + 4, 136, 26), "From anywhere", anywhere);
    let armed = ui.now() >= armed_at;
    if !armed {
        ui.repaint_at(armed_at);
    }
    let allow_kind = if armed { ButtonKind::Primary } else { ButtonKind::Secondary };
    let deny = ui.button_full(Rect::new(r.right() - 204, by, 100, 32), None, "Don't allow", ButtonKind::Secondary);
    let allow = ui.button_full(Rect::new(r.right() - 96, by, 84, 32), None, "Allow", allow_kind);
    if !armed || !(deny || allow) {
        return false;
    }
    let from = match (allow, *anywhere) {
        (false, _) => None,
        (true, false) => Some(Scope::LocalNetwork {}),
        (true, true) => Some(Scope::Anywhere {}),
    };
    m.push(Action::FirewallAnswer { app: q.app.clone(), from });
    true
}
