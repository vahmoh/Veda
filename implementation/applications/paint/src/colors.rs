// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The colour dialog: a square of saturation and value for the hue chosen beside it,
//! transparency, the colour's hexadecimal number, the colour before and after, and the colours
//! made before.
//!
//! The square is drawn into a picture once for each hue (integer arithmetic for each of its
//! pixels), and the hues' bar once.

use gfx::{Align, Bitmap, Color, Rect};
use paint_core::color::{self, Hsv};
use paint_core::settings::Settings;
use proto::display::Cursor;
use proto::input::keys;
use ui::{Font, Ui};

use crate::toolbar::swatch;

/// Which colour the dialog chooses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Primary,
    Secondary,
}

/// What the pointer is dragging in the dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    Square,
    Hue,
}

/// The colour dialog's state.
pub struct ColorDialog {
    pub target: Target,
    hsv: Hsv,
    alpha: f32,
    original: Color,
    hex: String,
    /// The square drawn for a hue.
    square: Option<(f32, Bitmap)>,
    bar: Option<Bitmap>,
    drag: Option<Part>,
}

const SQUARE: (i32, i32) = (240, 200);

/// The square of saturation (across) and value (down) for `hue`.
fn draw_square(hue: f32) -> Bitmap {
    let (w, h) = SQUARE;
    let pure = Hsv { h: hue, s: 1.0, v: 1.0 }.to_color(255);
    let (pr, pg, pb) = (pure.r() as u32, pure.g() as u32, pure.b() as u32);
    let mut b = Bitmap::new(w, h);
    for y in 0..h {
        let v = 255 - (y * 255 / (h - 1)) as u32;
        for x in 0..w {
            let s = (x * 255 / (w - 1)) as u32;
            // White mixed with the hue by the saturation, darkened by the value.
            let mix = |c: u32| ((255 * (255 - s) + c * s) / 255 * v + 127) / 255;
            b.pixels[(y * w + x) as usize] = 0xFF00_0000 | mix(pr) << 16 | mix(pg) << 8 | mix(pb);
        }
    }
    b
}

/// The bar of hues, from red at the top round to red at the bottom.
fn draw_bar(h: i32) -> Bitmap {
    let mut b = Bitmap::new(16, h);
    for y in 0..h {
        let c = Hsv { h: 360.0 * y as f32 / (h - 1) as f32, s: 1.0, v: 1.0 }.to_color(255);
        for x in 0..16 {
            b.pixels[(y * 16 + x) as usize] = c.premul();
        }
    }
    b
}

impl ColorDialog {
    pub fn new(target: Target, current: Color) -> ColorDialog {
        ColorDialog {
            target,
            hsv: Hsv::of(current),
            alpha: current.a() as f32,
            original: current,
            hex: color::hex(current),
            square: None,
            bar: None,
            drag: None,
        }
    }

    /// The colour as chosen so far.
    pub fn color(&self) -> Color {
        self.hsv.to_color(libm::roundf(self.alpha.clamp(0.0, 255.0)) as u8)
    }

    /// Shows the dialog; returns the colour chosen when it is closed with OK (`Some(None)`
    /// when cancelled), `None` while it is open. The custom colours of `s` are offered (and
    /// the colour chosen joins them).
    pub fn show(&mut self, ui: &mut Ui, s: &mut Settings) -> Option<Option<Color>> {
        let t = ui.theme().clone();
        let mut outcome = None;
        let title = if self.target == Target::Primary { "Primary colour" } else { "Secondary colour" };
        ui.modal(520, 430, |ui, card| {
            if ui.input.key(keys::ESC) {
                outcome = Some(None);
            }
            let inner = card.inset(22, 18, 22, 18);
            ui.heading(Rect::new(inner.x, inner.y, inner.w, 26), title);

            // The square and the bar.
            let sq = Rect::new(inner.x, inner.y + 40, SQUARE.0, SQUARE.1);
            let bar_r = Rect::new(sq.right() + 14, sq.y, 16, sq.h);
            if self.square.as_ref().is_none_or(|(h, _)| *h != self.hsv.h) {
                self.square = Some((self.hsv.h, draw_square(self.hsv.h)));
            }
            if self.bar.is_none() {
                self.bar = Some(draw_bar(sq.h));
            }
            if let Some((_, b)) = &self.square {
                ui.canvas.draw_bitmap(b, sq.x, sq.y, 255);
            }
            if let Some(b) = &self.bar {
                ui.canvas.draw_bitmap(b, bar_r.x, bar_r.y, 255);
            }
            ui.canvas.stroke_rounded_rect(sq.inflate(1), 2.0, 1.0, t.border_strong);
            ui.canvas.stroke_rounded_rect(bar_r.inflate(1), 2.0, 1.0, t.border_strong);
            let pointer = ui.input.pointer;
            if ui.input.pressed[0] {
                self.drag = pointer.and_then(|(x, y)| {
                    if sq.contains(x, y) {
                        Some(Part::Square)
                    } else if bar_r.inflate(4).contains(x, y) {
                        Some(Part::Hue)
                    } else {
                        None
                    }
                });
            }
            if !ui.input.down[0] {
                self.drag = None;
            }
            if let (Some(part), Some((x, y))) = (self.drag, pointer) {
                match part {
                    Part::Square => {
                        self.hsv.s = ((x - sq.x) as f32 / (sq.w - 1) as f32).clamp(0.0, 1.0);
                        self.hsv.v = 1.0 - ((y - sq.y) as f32 / (sq.h - 1) as f32).clamp(0.0, 1.0);
                    }
                    Part::Hue => self.hsv.h = (360.0 * (y - bar_r.y) as f32 / (bar_r.h - 1) as f32).clamp(0.0, 359.9),
                }
                self.hex = color::hex(self.color());
            }
            if ui.hovered(sq) || ui.hovered(bar_r) {
                ui.set_cursor(Cursor::Crosshair);
            }
            // Where the colour is in each.
            let mx = sq.x as f32 + self.hsv.s * (sq.w - 1) as f32;
            let my = sq.y as f32 + (1.0 - self.hsv.v) * (sq.h - 1) as f32;
            ui.canvas.fill_circle(mx, my, 6.5, Color::rgba(0, 0, 0, 160));
            ui.canvas.fill_circle(mx, my, 5.0, Color::WHITE);
            ui.canvas.fill_circle(mx, my, 3.5, self.hsv.to_color(255));
            let hy = bar_r.y + (self.hsv.h / 360.0 * (bar_r.h - 1) as f32) as i32;
            ui.canvas.fill_rect(Rect::new(bar_r.x - 3, hy - 2, bar_r.w + 6, 4), Color::WHITE);

            // To the right: before and after, the number, transparency.
            let side = Rect::new(bar_r.right() + 22, sq.y, inner.right() - bar_r.right() - 22, sq.h);
            ui.label(Rect::new(side.x, side.y, side.w, 18), "New", Font::Regular, 12.0, t.text_dim, Align::Left);
            swatch(ui, Rect::new(side.x, side.y + 20, side.w, 40), self.color(), 4.0);
            ui.label(
                Rect::new(side.x, side.y + 66, side.w, 18),
                "Current",
                Font::Regular,
                12.0,
                t.text_dim,
                Align::Left,
            );
            let current_r = Rect::new(side.x, side.y + 86, side.w, 30);
            swatch(ui, current_r, self.original, 4.0);
            if ui.interact(ui.id("color-current"), current_r).clicked {
                *self = ColorDialog {
                    square: self.square.take(),
                    bar: self.bar.take(),
                    ..ColorDialog::new(self.target, self.original)
                };
            }
            ui.label(Rect::new(side.x, side.y + 128, side.w, 18), "Hex", Font::Regular, 12.0, t.text_dim, Align::Left);
            let hex = ui.text_input(Rect::new(side.x, side.y + 148, side.w, 32), "color-hex", &mut self.hex, "#RRGGBB");
            if (hex.changed || hex.submitted)
                && let Some(c) = color::parse(&self.hex)
            {
                self.hsv = Hsv::of(c);
                self.alpha = c.a() as f32;
            }

            let ay = sq.bottom() + 16;
            ui.label(Rect::new(inner.x, ay, 90, 24), "Opacity", Font::Regular, 13.0, t.text_dim, Align::Left);
            let before = self.alpha;
            ui.slider(Rect::new(inner.x + 90, ay, sq.w + 30 - 90, 24), "color-alpha", &mut self.alpha, 0.0, 255.0);
            if before != self.alpha {
                self.hex = color::hex(self.color());
            }
            let percent = format!("{}%", libm::roundf(self.alpha / 2.55) as i32);
            ui.label(Rect::new(sq.right() + 40, ay, 60, 24), &percent, Font::Bold, 13.0, t.text, Align::Left);

            // The colours made before.
            let cy = ay + 36;
            ui.label(Rect::new(inner.x, cy, 140, 22), "Your colours", Font::Regular, 13.0, t.text_dim, Align::Left);
            for (i, c) in s.custom.iter().enumerate() {
                let cell = Rect::new(inner.x + 110 + i as i32 * 24, cy + 1, 20, 20);
                match c {
                    Some(c) => {
                        swatch(ui, cell, *c, 4.0);
                        if ui.interact(ui.id("custom") ^ i as u64, cell).clicked {
                            self.hsv = Hsv::of(*c);
                            self.alpha = c.a() as f32;
                            self.hex = color::hex(*c);
                        }
                    }
                    None => ui.canvas.stroke_rounded_rect(cell, 4.0, 1.0, t.border),
                }
            }

            let by = inner.bottom() - 34;
            if ui.primary_button(Rect::new(inner.right() - 96, by, 96, 34), "OK") || ui.input.key(keys::ENTER) {
                outcome = Some(Some(self.color()));
            }
            if ui.button(Rect::new(inner.right() - 200, by, 96, 34), "Cancel") {
                outcome = Some(None);
            }
        });
        if let Some(Some(c)) = outcome {
            if c != self.original && !color::PALETTE.iter().any(|&p| Color::hex(p) == c) {
                s.add_custom(c);
            }
            ui.repaint();
        } else if outcome.is_some() {
            ui.repaint();
        }
        outcome
    }
}
