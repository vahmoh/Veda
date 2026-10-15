// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The card that asks the user to allow what the voice agent wants to do:
//! what it is, what it acts on, "Always allow" where that is offered, and
//! Deny and Allow. The agent's window shows it under its ring; while the
//! window is closed, a notification shows it beside a small ring.
//!
//! "Always allow" belongs to the request it was ticked on ([`Choice`]): a
//! request that expires or is answered elsewhere takes its tick with it,
//! and the next request, for whatever action, starts unticked.

use alloc::format;

use vgfx::{Align, Color, Rect};
use vproto::agent::ApprovalRequest;
use vui::{Font, Ui};

use crate::{Action, Model, agent};

/// "Always allow" as ticked on one request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Choice {
    /// The request it is for.
    id: u64,
    always: bool,
}

impl Choice {
    /// The tick of request `id`: unticked for a request other than the
    /// one it was ticked on.
    fn of(&mut self, id: u64) -> &mut bool {
        if self.id != id {
            *self = Choice { id, always: false };
        }
        &mut self.always
    }
}

/// How the card is laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Look {
    /// In the agent's window: a line saying who asks, and how many more
    /// requests wait.
    Window,
    /// In a notification: the agent's ring at the left.
    Notification,
}

/// What an approval asks for, in bold over up to two lines (the end of a
/// longer one is cut); returns the height used. `r` is the first line.
fn title(ui: &mut Ui, r: Rect, text: &str) -> i32 {
    let th = ui.theme().clone();
    let font = ui.ctx.font(Font::Bold);
    let lines = ui.ctx.text.wrap(font, 14.0, text, r.w as f32);
    if lines.len() <= 1 {
        ui.label(r, text, Font::Bold, 14.0, th.text, Align::Left);
        return r.h;
    }
    let first = text[lines[0].clone()].trim_end();
    let rest = text[lines[1].start..].trim();
    let second = ui.ctx.text.ellipsize(font, 14.0, rest, r.w as f32);
    ui.label(r, first, Font::Bold, 14.0, th.text, Align::Left);
    ui.label(r.translate(0, 18), &second, Font::Bold, 14.0, th.text, Align::Left);
    r.h + 18
}

/// Draws the card for `req` in `r` and handles its buttons: the user's
/// answer goes to `m` as [`Action::AgentDecide`]. Returns whether the user
/// answered.
pub fn card(ui: &mut Ui, m: &mut Model, req: &ApprovalRequest, r: Rect, choice: &mut Choice, look: Look) -> bool {
    let th = ui.theme().clone();
    let (x, top) = match look {
        Look::Window => {
            ui.canvas.fill_rounded_rect(r, 10.0, Color::rgba(255, 255, 255, 18));
            ui.canvas.stroke_rounded_rect(r, 10.0, 1.0, Color::rgba(255, 255, 255, 30));
            let ask = format!("{} needs your OK", m.agent.name());
            let line = Rect::new(r.x + 14, r.y + 10, r.w - 28, 16);
            ui.label(line, &ask, Font::Regular, 12.0, th.text_faint, Align::Left);
            (r.x + 14, r.y + 28)
        }
        Look::Notification => {
            let now = ui.now();
            agent::draw_presence(&mut ui.canvas, r.x as f32 + 38.0, r.y as f32 + 38.0, 13.0, &m.agent, now);
            (r.x + 68, r.y + 14)
        }
    };
    let w = r.right() - 14 - x;
    let used = title(ui, Rect::new(x, top, w, 20), &req.action);
    let dy = used - 20;
    ui.paragraph(Rect::new(x, top + 22 + dy, w, 40 - dy), &req.detail, 12.5, th.text_dim);
    let by = r.bottom() - 44;
    let always = choice.of(req.id);
    if req.allow_always {
        ui.checkbox(Rect::new(r.x + 12, by + 4, 130, 26), "Always allow", always);
    }
    let more = m.agent.approvals.len().saturating_sub(1);
    if look == Look::Window && more > 0 && !req.allow_always {
        let label = Rect::new(r.x + 14, by + 4, 120, 26);
        ui.label(label, &format!("+{more} more"), Font::Regular, 12.0, th.text_faint, Align::Left);
    }
    let decide = if ui.button(Rect::new(r.right() - 186, by, 84, 32), "Deny") {
        Some(false)
    } else if ui.primary_button(Rect::new(r.right() - 96, by, 84, 32), "Allow") {
        Some(true)
    } else {
        None
    };
    let Some(allow) = decide else { return false };
    // Only an allowed request is allowed for good, and only if offered.
    let always = allow && req.allow_always && *always;
    m.push(Action::AgentDecide { id: req.id, allow, always });
    *choice = Choice::default();
    true
}
