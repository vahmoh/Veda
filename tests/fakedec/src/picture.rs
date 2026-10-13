// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The pictures, drawn rather than decoded. Each says which it is, so that
//! a test (or a person) can tell what is on the screen, and when:
//!
//! * a large timecode (`m:ss.mmm`, the picture's time) with the picture's
//!   number under it, in seven-segment digits of one width (no font: the
//!   decoder has no file system);
//! * a white marker sweeping across once a second: at the left edge on
//!   every whole second, when the sound clicks;
//! * the part played, as a bar along the bottom.
//!
//! Pictures are drawn upright at the size asked for (a rotated stream's
//! pool is already upright); every pixel is opaque.

use alloc::format;

use vgfx::{Canvas, Color, Rect};

use crate::clip::Picture;

/// The background, also of the pixels past a row's width.
pub const BACKGROUND: Color = Color(0xFF10_1418);
/// The part played.
const ACCENT: Color = Color(0xFF3C_C8F0);
/// The part not played yet.
const TRACK: Color = Color(0xFF2A_3038);
/// The picture's number.
const NUMBER: Color = Color(0xFF8C_98A4);

/// A seven-segment digit's segments, in units of their thickness (x, y,
/// width, height): top, top right, bottom right, bottom, bottom left, top
/// left, middle.
const SEGMENTS: [(i32, i32, i32, i32); 7] =
    [(0, 0, 6, 1), (5, 0, 1, 6), (5, 5, 1, 6), (0, 10, 6, 1), (0, 5, 1, 6), (0, 0, 1, 6), (0, 5, 6, 1)];
/// The segments each digit lights (bit `i`: `SEGMENTS[i]`).
const DIGITS: [u8; 10] = [0x3F, 0x06, 0x5B, 0x4F, 0x66, 0x6D, 0x7D, 0x07, 0x7F, 0x6F];
/// A character's height, and the space after it, in units.
const HEIGHT: i32 = 11;
const GAP: i32 = 2;

/// Draws `picture` of a clip of `duration_us` over the whole canvas.
pub fn draw(canvas: &mut Canvas, picture: &Picture, duration_us: i64) {
    let (w, h) = (canvas.width(), canvas.height());
    canvas.clear(BACKGROUND);
    let margin = (w.min(h) / 16).max(1);

    // The part played, along the bottom.
    let bar = Rect::new(margin, h - margin - (h / 40).max(2), w - 2 * margin, (h / 40).max(2));
    canvas.fill_rect(bar, TRACK);
    let duration = duration_us.max(1);
    let played = bar.w as i64 * picture.pts_us.clamp(0, duration) / duration;
    canvas.fill_rect(Rect::new(bar.x, bar.y, played as i32, bar.h), ACCENT);

    // The marker, across once a second.
    let band = Rect::new(0, margin, w, (h / 8).max(1));
    let mark = (w / 64).max(2);
    let x = (w - mark).max(0) as i64 * picture.pts_us.rem_euclid(1_000_000) / 1_000_000;
    canvas.fill_rect(Rect::new(x as i32, band.y, mark, band.h), Color::WHITE);

    // The timecode, the picture's number under it at half the size.
    let ms = picture.pts_us.max(0) / 1000;
    let time = format!("{}:{:02}.{:03}", ms / 60_000, ms / 1000 % 60, ms % 1000);
    let number = format!("{}", picture.index);
    let (time, number) = (time.as_bytes(), number.as_bytes());
    let top = band.bottom() + margin;
    let room = bar.y - margin - top;
    // The timecode is 11 units high, then 3 apart, then 5.5 for the number.
    let unit = ((w - 2 * margin) / width(time)).min(room * 2 / 39).max(1);
    let small = (unit / 2).max(1);
    let y = top + (room - (HEIGHT + 3) * unit - HEIGHT * small) / 2;
    text(canvas, time, (w - width(time) * unit) / 2, y, unit, Color::WHITE);
    text(canvas, number, (w - width(number) * small) / 2, y + (HEIGHT + 3) * unit, small, NUMBER);
}

/// A character's width in units (digits, `:` and `.`).
fn glyph_width(c: u8) -> i32 {
    if c.is_ascii_digit() { 6 } else { 1 }
}

/// The width of `s` in units.
fn width(s: &[u8]) -> i32 {
    s.iter().map(|&c| glyph_width(c) + GAP).sum::<i32>() - GAP
}

/// Draws `s` (digits, `:` and `.`) from (`x`, `y`) in segments `unit`
/// pixels thick.
fn text(canvas: &mut Canvas, s: &[u8], mut x: i32, y: i32, unit: i32, color: Color) {
    let at =
        |x: i32, (sx, sy, sw, sh): (i32, i32, i32, i32)| Rect::new(x + sx * unit, y + sy * unit, sw * unit, sh * unit);
    for &c in s {
        match c {
            // Centred, so that it does not read as a space and a 1.
            b'1' => canvas.fill_rect(Rect::new(x + 5 * unit / 2, y, unit, HEIGHT * unit), color),
            b'0'..=b'9' => {
                let lit = DIGITS[(c - b'0') as usize];
                for (i, &segment) in SEGMENTS.iter().enumerate() {
                    if lit & (1 << i) != 0 {
                        canvas.fill_rect(at(x, segment), color);
                    }
                }
            }
            b':' => {
                canvas.fill_rect(at(x, (0, 3, 1, 1)), color);
                canvas.fill_rect(at(x, (0, 7, 1, 1)), color);
            }
            b'.' => canvas.fill_rect(at(x, (0, 10, 1, 1)), color),
            _ => {}
        }
        x += (glyph_width(c) + GAP) * unit;
    }
}
