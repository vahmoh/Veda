// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! How the keys look and answer the pointer (which keys each mode has, and where, is
//! `calculator_core::keypad`'s).
//!
//! A key pressed on the keyboard lights up on screen for a moment, as if it had been clicked,
//! so that what was typed can be seen.

use calculator_core::keypad::{self, Label, Layout, Look, Pad};
use vgfx::{Align, Color, Rect};
use vproto::display::Cursor;
use vui::{Font, Icon, Ui};

/// How long a key lights up after its key on the keyboard is pressed (ns).
pub const FLASH_NS: u64 = 160_000_000;

/// Where each key of `layout` goes in `area`, with `gap` pixels between keys.
pub fn place(layout: &Layout, area: Rect, gap: i32) -> Vec<Rect> {
    keypad::place(layout, (area.x, area.y, area.w, area.h), gap)
        .into_iter()
        .map(|(x, y, w, h)| Rect::new(x, y, w, h))
        .collect()
}

/// The colours of a key: its face at rest, hovered, held, and its label's.
fn colors(ui: &Ui, look: Look, on: bool) -> (Color, Color, Color, Color) {
    let t = ui.theme();
    match look {
        _ if on => (t.accent.with_alpha(70), t.accent.with_alpha(95), t.accent.with_alpha(55), t.text),
        Look::Digit => (Color::hex(0x2F2F37), Color::hex(0x3A3A44), Color::hex(0x28282F), t.text),
        Look::Function => (Color::hex(0x25252C), Color::hex(0x31313A), Color::hex(0x202026), t.text),
        Look::Operator => {
            (t.accent.with_alpha(38), t.accent.with_alpha(62), t.accent.with_alpha(28), Color::hex(0xA9C2FF))
        }
        Look::Equals => (t.accent, t.accent_hover, t.accent_pressed, t.on_accent),
    }
}

/// Draws a key and returns whether it was clicked; `flash` lights it up as if held (its key
/// was pressed on the keyboard).
pub fn key(ui: &mut Ui, r: Rect, pad: &Pad, flash: bool) -> bool {
    let id = ui.id(pad.tip) ^ ((r.x as u64) << 32) ^ (r.y as u64) ^ 0x6b65_7970;
    let resp = if pad.enabled { ui.interact(id, r) } else { vui::Response::default() };
    let hover = ui.animate(id, if resp.hovered { 1.0 } else { 0.0 }, 9.0);
    let (rest, hovered, held, text) = colors(ui, pad.look, pad.on);
    let face = if resp.held || flash { held } else { rest.lerp(hovered, hover) };
    let face = if pad.enabled { face } else { face.fade(0.55) };
    let radius = (r.h.min(r.w) as f32 * 0.16).clamp(6.0, 10.0);
    if pad.look == Look::Equals && !(resp.held || flash) {
        ui.canvas.fill_rounded_rect_gradient(r, radius, face.shade(0.08), face.shade(-0.06));
    } else {
        ui.canvas.fill_rounded_rect(r, radius, face);
    }
    if flash || resp.held {
        // A ring, so that the key pressed shows even on a face that hardly darkens.
        ui.canvas.stroke_rounded_rect(r, radius, 1.0, Color::rgba(255, 255, 255, 40));
    }
    let color = if pad.enabled { text } else { ui.theme().text_faint };
    match pad.label {
        Label::Backspace | Label::Swap => {
            let icon = if pad.label == Label::Swap { Icon::Swap } else { Icon::Backspace };
            let size = (r.h.min(r.w) as f32 * 0.42).clamp(16.0, 24.0);
            ui.icon(r, icon, size, color);
        }
        Label::Text(label) => {
            let big = matches!(pad.look, Look::Digit | Look::Operator | Look::Equals) && label.chars().count() == 1;
            let size = if big { (r.h as f32 * 0.42).clamp(18.0, 28.0) } else { (r.h as f32 * 0.3).clamp(13.0, 18.0) };
            let font = if pad.look == Look::Digit && big { Font::Regular } else { Font::Bold };
            ui.label(r, label, font, size, color, Align::Center);
        }
    }
    if resp.hovered {
        ui.set_cursor(Cursor::Hand);
        if !pad.tip.is_empty() {
            ui.tooltip(id, r, pad.tip);
        }
    }
    resp.clicked
}
