// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Music's own widgets drawn on top of `vui` (the seek bar, the play
//! button and the toggles are `vui::media`'s).

use num_traits::Float;
use vgfx::{Color, Rect};
use vui::Ui;

/// A small three-bar "now playing" equaliser glyph, animated by `t`
/// (seconds) when `moving`.
pub fn eq_glyph(ui: &mut Ui, r: Rect, t: f32, moving: bool, color: Color) {
    let bw = 3;
    let gap = 2;
    let total = bw * 3 + gap * 2;
    let x0 = r.x + (r.w - total) / 2;
    let base = r.y + r.h / 2 + 7;
    for i in 0..3 {
        let phase = i as f32 * 1.7;
        let h = if moving {
            4.0 + 9.0 * (0.5 + 0.5 * Float::sin(t * (5.0 + i as f32 * 1.3) + phase))
        } else {
            4.0 + i as f32 * 2.0
        };
        let h = h as i32;
        ui.canvas.fill_rounded_rect(Rect::new(x0 + i * (bw + gap), base - h, bw, h), 1.0, color);
    }
}
