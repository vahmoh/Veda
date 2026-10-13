// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Small pieces drawn over the video: a borderless button that reads on any
//! picture, the time above the seek bar, and the message that confirms a
//! key ("+10 s", "Volume 80%").

use vgfx::{Align, Color, Rect};
use vui::{Cursor, Font, Icon, Ui};

/// A button of an icon and a label on a translucent background, faded by
/// `alpha`. True when clicked.
pub fn ghost_button(ui: &mut Ui, r: Rect, icon: Icon, label: &str, alpha: f32) -> bool {
    let id = ui.id(label) ^ 0x6057;
    let resp = ui.interact(id, r);
    let hover = ui.animate(id, if resp.hovered { 1.0 } else { 0.0 }, 8.0);
    let bg = (40.0 + hover * 40.0) * alpha;
    ui.canvas.fill_rounded_rect(r, r.h as f32 / 2.0, Color::rgba(255, 255, 255, bg as u8));
    let fg = Color::rgba(255, 255, 255, (255.0 * alpha) as u8);
    ui.icon(Rect::new(r.x + 8, r.y, 20, r.h), icon, 16.0, fg);
    ui.label(Rect::new(r.x + 30, r.y, r.w - 38, r.h), label, Font::Bold, 13.0, fg, Align::Left);
    if resp.hovered {
        ui.set_cursor(Cursor::Hand);
    }
    resp.clicked && alpha > 0.5
}

/// A time in a pill centred on `x`, its top at `y`.
pub fn time_pill(ui: &mut Ui, x: i32, y: i32, label: &str, alpha: f32) {
    let w = ui.measure(label, Font::Bold, 12.0) as i32 + 16;
    let r = Rect::new(x - w / 2, y, w, 22);
    ui.canvas.fill_rounded_rect(r, 11.0, Color::rgba(20, 20, 26, (220.0 * alpha) as u8));
    ui.label(r, label, Font::Bold, 12.0, Color::rgba(255, 255, 255, (255.0 * alpha) as u8), Align::Center);
}

/// A short message in the middle of the upper part of the stage.
pub fn osd(ui: &mut Ui, stage: Rect, text: &str) {
    let w = ui.measure(text, Font::Bold, 18.0) as i32 + 40;
    let r = Rect::new(stage.x + (stage.w - w) / 2, stage.y + stage.h / 4 - 24, w, 48);
    ui.canvas.fill_rounded_rect(r, 14.0, Color::rgba(16, 16, 20, 190));
    ui.label(r, text, Font::Bold, 18.0, Color::WHITE, Align::Center);
}

/// A notice at the top of the stage with a button; true when the button
/// is clicked.
pub fn toast(ui: &mut Ui, stage: Rect, text: &str, button: &str, accent: Color) -> bool {
    let tw = ui.measure(text, Font::Regular, 13.5) as i32;
    let bw = ui.measure(button, Font::Bold, 13.5) as i32 + 24;
    let w = tw + bw + 44;
    let r = Rect::new(stage.x + (stage.w - w) / 2, stage.y + 72, w, 40);
    ui.canvas.draw_shadow(r, 10, 12, Color::rgba(0, 0, 0, 90));
    ui.canvas.fill_rounded_rect(r, 12.0, Color::rgba(28, 28, 34, 235));
    ui.label(Rect::new(r.x + 18, r.y, tw + 4, r.h), text, Font::Regular, 13.5, Color::WHITE, Align::Left);
    let b = Rect::new(r.right() - bw - 8, r.y + 6, bw, r.h - 12);
    let id = ui.id(button) ^ 0x70a5;
    let resp = ui.interact(id, b);
    let hover = ui.animate(id, if resp.hovered { 1.0 } else { 0.0 }, 8.0);
    ui.canvas.fill_rounded_rect(b, 8.0, Color::rgba(255, 255, 255, (16.0 + 24.0 * hover) as u8));
    ui.label(b, button, Font::Bold, 13.5, accent, Align::Center);
    if resp.hovered {
        ui.set_cursor(Cursor::Hand);
    }
    resp.clicked
}
