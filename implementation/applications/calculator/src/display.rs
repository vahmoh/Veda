// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The display: the expression and its result, as large as the width allows.
//!
//! Three lines: what `=` worked out (small, above), the expression or the result (large), and
//! the result so far while typing, or what went wrong (below). The large line shrinks to fit;
//! too long even at its smallest, it shows its end (or the caret's surroundings), cut at the
//! left. Parentheses the expression leaves open are shown faintly after it, as it will be
//! worked out.

use calculator_core::Screen;
use vgfx::{Align, Color, Rect};
use vproto::display::Cursor;
use vui::{Font, Ui};

/// The large line's sizes, from its largest to its smallest.
const MAIN_MAX: f32 = 46.0;
const MAIN_MIN: f32 = 20.0;

/// Where the display's large line was drawn: its text's left edge, its size and the text
/// (to put the caret where it is clicked).
#[derive(Debug, Clone, Default)]
pub struct Drawn {
    pub line: Rect,
    pub x: f32,
    pub size: f32,
    pub text: alloc::string::String,
}

/// The size the large line is drawn at to fit `width`, and its width at that size.
fn fit(ui: &Ui, text: &str, max: f32, width: f32) -> (f32, f32) {
    let mut size = max;
    loop {
        let w = ui.measure(text, Font::Regular, size);
        if w <= width || size <= MAIN_MIN {
            return (size, w);
        }
        size = (size - 2.0).max(MAIN_MIN);
    }
}

/// Draws `screen` in `r`; returns where the large line went. `memory` shows the memory's mark.
pub fn draw(ui: &mut Ui, r: Rect, screen: &Screen, memory: bool) -> Drawn {
    let t = ui.theme().clone();
    let pad = 18;
    let inner = r.inset(pad, 8, pad, 6);
    let above_h = 24;
    let below_h = 26;
    let main_h = (inner.h - above_h - below_h).max(36);
    let above = Rect::new(inner.x, inner.y, inner.w, above_h);
    let main = Rect::new(inner.x, above.bottom(), inner.w, main_h);
    let below = Rect::new(inner.x, main.bottom(), inner.w, below_h);

    if memory {
        ui.label(Rect::new(inner.x, inner.y, 30, above_h), "M", Font::Bold, t.small_size, t.accent, Align::Left);
    }
    if !screen.above.is_empty() {
        let font = ui.ctx.font(Font::Regular);
        let shown = ui.ctx.text.ellipsize(font, 15.0, &screen.above, (above.w - 30) as f32);
        ui.label(above, &shown, Font::Regular, 15.0, t.text_dim, Align::Right);
    }

    // The large line: the expression with its open parentheses, or the result.
    let text = if screen.main.is_empty() { "0" } else { screen.main.as_str() };
    let ghost: alloc::string::String = ")".repeat(screen.ghost);
    let max = (main.h as f32 * 0.82).min(MAIN_MAX);
    let whole = alloc::format!("{text}{ghost}");
    let (size, width) = fit(ui, &whole, max, main.w as f32);
    let text_w = ui.measure(text, Font::Regular, size);
    // Too wide even at the smallest size: the end shows, or the caret.
    let mut x = main.right() as f32 - width;
    if let Some(c) = screen.caret {
        let caret_x = x + ui.measure(&text[..c.min(text.len())], Font::Regular, size);
        if caret_x < main.x as f32 + 8.0 {
            x += main.x as f32 + 8.0 - caret_x;
        }
    }
    let color = if screen.main.is_empty() { t.text_faint } else { t.text };
    ui.canvas.save();
    ui.canvas.clip_to(main);
    let line = Rect::new(x as i32, main.y, (text_w as i32) + 2, main.h);
    ui.label(line, text, Font::Regular, size, color, Align::Left);
    if !ghost.is_empty() {
        let g = Rect::new((x + text_w) as i32, main.y, (width - text_w) as i32 + 2, main.h);
        ui.label(g, &ghost, Font::Regular, size, t.text_faint, Align::Left);
    }
    if let Some(c) = screen.caret {
        let cx = x + ui.measure(&text[..c.min(text.len())], Font::Regular, size);
        let h = (size * 1.05) as i32;
        let cy = main.y + (main.h - h) / 2;
        ui.canvas.fill_rect(Rect::new(cx as i32, cy, 2, h), t.accent);
    }
    if let Some(at) = screen.error_at.filter(|_| screen.error.is_some()) {
        // A red mark under where the expression goes wrong.
        let at = at.min(text.len());
        let x0 = x + ui.measure(&text[..at], Font::Regular, size);
        let next = text[at..].chars().next().map_or(0, char::len_utf8);
        let w = ui.measure(&text[at..at + next], Font::Regular, size).max(size * 0.4);
        let y = main.y + main.h / 2 + (size * 0.62) as i32;
        ui.canvas.fill_rounded_rect(Rect::new(x0 as i32, y, w as i32, 3), 1.5, t.danger);
    }
    ui.canvas.restore();

    // The line below: what went wrong, or the result so far.
    if let Some(e) = &screen.error {
        ui.label(below, e, Font::Bold, 15.0, t.danger, Align::Right);
    } else if let Some(p) = &screen.preview {
        let font = ui.ctx.font(Font::Regular);
        let shown = ui.ctx.text.ellipsize(font, 20.0, &alloc::format!("= {p}"), below.w as f32);
        ui.label(below, &shown, Font::Regular, 20.0, t.text_dim, Align::Right);
    }
    if ui.hovered(main) {
        ui.set_cursor(Cursor::Text);
    }
    Drawn { line: main, x, size, text: alloc::string::String::from(text) }
}

/// The byte of the large line nearest `px` (a window x), to put the caret there.
pub fn index_at(ui: &Ui, drawn: &Drawn, px: i32) -> usize {
    let font = ui.ctx.font(Font::Regular);
    ui.ctx.text.index_for_x(font, drawn.size, &drawn.text, px as f32 - drawn.x)
}

/// A pill that says something briefly (`Copied`), at the bottom right of `r`.
pub fn toast(ui: &mut Ui, r: Rect, text: &str) {
    let t = ui.theme().clone();
    let w = ui.measure(text, Font::Regular, 13.0) as i32 + 28;
    let pill = Rect::new(r.right() - w - 12, r.bottom() - 40, w, 30);
    ui.canvas.draw_shadow(pill.translate(0, 3), 15, 10, Color::rgba(0, 0, 0, 100));
    ui.canvas.fill_rounded_rect(pill, 15.0, Color::hex(0x34343D));
    ui.canvas.stroke_rounded_rect(pill, 15.0, 1.0, t.border_strong);
    ui.label(pill, text, Font::Regular, 13.0, t.text, Align::Center);
}
