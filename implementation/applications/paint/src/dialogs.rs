// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The dialogs that ask for numbers: a new picture's size, resizing, the canvas's size, and the
//! effects with settings (shown on the picture as they change).

use paint_core::Document;
use paint_core::effects::Effect;
use paint_core::transform::{Anchor, MAX_SIDE, Resample};
use vgfx::{Align, Color, Rect};
use vproto::display::Cursor;
use vproto::input::keys;
use vui::{Font, Ui};

use crate::toolbar::segmented;

/// What a dialog's buttons said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Ok,
    Cancel,
}

/// A whole number in `text` from 1 to the largest side, or why it is not one.
fn side(text: &str, what: &str) -> Result<i32, String> {
    match text.trim().parse::<i64>() {
        Ok(n) if (1..=MAX_SIDE as i64).contains(&n) => Ok(n as i32),
        Ok(_) => Err(format!("The {what} must be from 1 to {MAX_SIDE} pixels.")),
        Err(_) => Err(format!("The {what} must be a whole number of pixels.")),
    }
}

/// A labelled number field; returns its response.
fn field(ui: &mut Ui, x: i32, y: i32, label: &str, id: &str, value: &mut String, unit: &str) -> vui::TextInputResponse {
    let t = ui.theme().clone();
    ui.label(Rect::new(x, y, 80, 34), label, Font::Regular, t.font_size, t.text_dim, Align::Left);
    let resp = ui.text_input(Rect::new(x + 80, y, 110, 34), id, value, "");
    ui.label(Rect::new(x + 198, y, 60, 34), unit, Font::Regular, t.font_size, t.text_faint, Align::Left);
    resp
}

/// OK and Cancel at the bottom right of `inner`, Enter and Esc for them.
fn buttons(ui: &mut Ui, inner: Rect, ok: &str) -> Option<Answer> {
    let by = inner.bottom() - 34;
    if ui.primary_button(Rect::new(inner.right() - 104, by, 104, 34), ok) || ui.input.key(keys::ENTER) {
        return Some(Answer::Ok);
    }
    if ui.button(Rect::new(inner.right() - 214, by, 104, 34), "Cancel") || ui.input.key(keys::ESC) {
        return Some(Answer::Cancel);
    }
    None
}

/// What a new picture's background is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Background {
    White,
    Transparent,
    Secondary,
}

/// The new picture dialog.
pub struct NewPicture {
    pub width: String,
    pub height: String,
    pub background: Background,
    error: Option<String>,
    focus: bool,
}

/// Sizes a new picture often has: a name and its size.
const PRESETS: [(&str, i32, i32); 5] = [
    ("960 × 540", 960, 540),
    ("1280 × 720", 1280, 720),
    ("1920 × 1080", 1920, 1080),
    ("800 × 600", 800, 600),
    ("512 × 512", 512, 512),
];

impl NewPicture {
    pub fn new(w: i32, h: i32) -> NewPicture {
        NewPicture {
            width: w.to_string(),
            height: h.to_string(),
            background: Background::White,
            error: None,
            focus: true,
        }
    }

    /// Shows the dialog; returns the size and background chosen with OK, `Some(None)` if
    /// cancelled.
    pub fn show(&mut self, ui: &mut Ui) -> Option<Option<(i32, i32, Background)>> {
        let t = ui.theme().clone();
        let mut out = None;
        ui.modal(480, 330, |ui, card| {
            let inner = card.inset(24, 20, 24, 20);
            ui.heading(Rect::new(inner.x, inner.y, inner.w, 26), "New picture");
            if self.focus {
                ui.focus_text_input("new-width");
                self.focus = false;
            }
            field(ui, inner.x, inner.y + 44, "Width", "new-width", &mut self.width, "pixels");
            field(ui, inner.x, inner.y + 86, "Height", "new-height", &mut self.height, "pixels");
            // Sizes to start from.
            let mut px = inner.x;
            for (name, w, h) in PRESETS {
                let bw = ui.measure(name, Font::Regular, 12.0) as i32 + 16;
                if px + bw > inner.right() {
                    break;
                }
                let r = Rect::new(px, inner.y + 132, bw, 26);
                let id = ui.id(name) ^ 0x9e5;
                let resp = ui.interact(id, r);
                ui.canvas.fill_rounded_rect(r, 13.0, if resp.hovered { t.control_hover } else { t.control });
                ui.label(r, name, Font::Regular, 12.0, t.text_dim, Align::Center);
                if resp.hovered {
                    ui.set_cursor(Cursor::Hand);
                }
                if resp.clicked {
                    self.width = w.to_string();
                    self.height = h.to_string();
                }
                px += bw + 6;
            }
            ui.label(
                Rect::new(inner.x, inner.y + 172, 100, 34),
                "Background",
                Font::Regular,
                t.font_size,
                t.text_dim,
                Align::Left,
            );
            let chosen = match self.background {
                Background::White => 0,
                Background::Transparent => 1,
                Background::Secondary => 2,
            };
            let row = Rect::new(inner.x + 100, inner.y + 172, 300, 34);
            let (_, picked) =
                segmented(ui, row.x, row, "new-background", &["White", "Transparent", "Secondary colour"], chosen);
            if let Some(p) = picked {
                self.background = [Background::White, Background::Transparent, Background::Secondary][p];
            }
            if let Some(e) = &self.error {
                ui.label(
                    Rect::new(inner.x, inner.bottom() - 74, inner.w, 24),
                    e,
                    Font::Regular,
                    t.small_size + 1.0,
                    t.danger,
                    Align::Left,
                );
            }
            match buttons(ui, inner, "Create") {
                Some(Answer::Ok) => match (side(&self.width, "width"), side(&self.height, "height")) {
                    (Ok(w), Ok(h)) => out = Some(Some((w, h, self.background))),
                    (Err(e), _) | (_, Err(e)) => self.error = Some(e),
                },
                Some(Answer::Cancel) => out = Some(None),
                None => {}
            }
        });
        out
    }
}

/// The resize dialog.
pub struct ResizePicture {
    original: (i32, i32),
    by_percent: bool,
    width: String,
    height: String,
    keep_ratio: bool,
    smooth: bool,
    error: Option<String>,
}

impl ResizePicture {
    pub fn new(w: i32, h: i32) -> ResizePicture {
        ResizePicture {
            original: (w, h),
            by_percent: true,
            width: String::from("100"),
            height: String::from("100"),
            keep_ratio: true,
            smooth: true,
            error: None,
        }
    }

    /// The size the fields say.
    fn size(&self) -> Result<(i32, i32), String> {
        if self.by_percent {
            let pct = |t: &str| t.trim().parse::<f32>().ok().filter(|p| *p > 0.0 && *p <= 10_000.0);
            match (pct(&self.width), pct(&self.height)) {
                (Some(pw), Some(ph)) => {
                    let w = libm::roundf(self.original.0 as f32 * pw / 100.0).max(1.0) as i32;
                    let h = libm::roundf(self.original.1 as f32 * ph / 100.0).max(1.0) as i32;
                    if w > MAX_SIDE || h > MAX_SIDE {
                        Err(format!("A picture is at most {MAX_SIDE} pixels a side."))
                    } else {
                        Ok((w, h))
                    }
                }
                _ => Err(String::from("The percentages must be numbers above 0.")),
            }
        } else {
            Ok((side(&self.width, "width")?, side(&self.height, "height")?))
        }
    }

    /// Shows the dialog; returns the size and resampling chosen with OK, `Some(None)` if
    /// cancelled.
    pub fn show(&mut self, ui: &mut Ui) -> Option<Option<(i32, i32, Resample)>> {
        let t = ui.theme().clone();
        let mut out = None;
        ui.modal(480, 360, |ui, card| {
            let inner = card.inset(24, 20, 24, 20);
            ui.heading(Rect::new(inner.x, inner.y, inner.w, 26), "Resize");
            let (ow, oh) = self.original;
            let row = Rect::new(inner.x, inner.y + 40, inner.w, 34);
            let (_, picked) =
                segmented(ui, row.x, row, "resize-by", &["Percentage", "Pixels"], if self.by_percent { 0 } else { 1 });
            if let Some(p) = picked {
                // The fields in the new unit, the size they gave kept.
                if let Ok((w, h)) = self.size() {
                    self.by_percent = p == 0;
                    if self.by_percent {
                        self.width = format!("{}", libm::roundf(w as f32 * 100.0 / ow as f32));
                        self.height = format!("{}", libm::roundf(h as f32 * 100.0 / oh as f32));
                    } else {
                        self.width = w.to_string();
                        self.height = h.to_string();
                    }
                }
            }
            let unit = if self.by_percent { "%" } else { "pixels" };
            let wr = field(ui, inner.x, inner.y + 86, "Width", "resize-width", &mut self.width, unit);
            let hr = field(ui, inner.x, inner.y + 128, "Height", "resize-height", &mut self.height, unit);
            if self.keep_ratio {
                // The other side follows, in proportion.
                let ratio = if self.by_percent { 1.0 } else { oh as f32 / ow as f32 };
                if wr.changed
                    && let Ok(v) = self.width.trim().parse::<f32>()
                {
                    self.height = format!("{}", libm::roundf(v * ratio).max(1.0));
                } else if hr.changed
                    && let Ok(v) = self.height.trim().parse::<f32>()
                {
                    self.width = format!("{}", libm::roundf(v / ratio).max(1.0));
                }
            }
            let mut keep = self.keep_ratio;
            if ui.checkbox(Rect::new(inner.x, inner.y + 172, 260, 30), "Keep the proportions", &mut keep) {
                self.keep_ratio = keep;
            }
            let mut smooth = self.smooth;
            ui.checkbox(
                Rect::new(inner.x, inner.y + 204, 360, 30),
                "Smooth (off: whole pixels, for pixel art)",
                &mut smooth,
            );
            self.smooth = smooth;
            let now = match self.size() {
                Ok((w, h)) => format!("{ow} × {oh} → {w} × {h} pixels"),
                Err(_) => format!("Now {ow} × {oh} pixels"),
            };
            ui.label(
                Rect::new(inner.x, inner.y + 240, inner.w, 24),
                &now,
                Font::Regular,
                13.0,
                t.text_faint,
                Align::Left,
            );
            if let Some(e) = &self.error {
                ui.label(
                    Rect::new(inner.x, inner.bottom() - 72, inner.w, 24),
                    e,
                    Font::Regular,
                    13.0,
                    t.danger,
                    Align::Left,
                );
            }
            match buttons(ui, inner, "Resize") {
                Some(Answer::Ok) => match self.size() {
                    Ok((w, h)) => {
                        out = Some(Some((w, h, if self.smooth { Resample::Smooth } else { Resample::Pixelated })))
                    }
                    Err(e) => self.error = Some(e),
                },
                Some(Answer::Cancel) => out = Some(None),
                None => {}
            }
        });
        out
    }
}

/// The canvas size dialog.
pub struct CanvasSize {
    width: String,
    height: String,
    pub anchor: Anchor,
    pub transparent: bool,
    error: Option<String>,
}

impl CanvasSize {
    pub fn new(w: i32, h: i32) -> CanvasSize {
        CanvasSize {
            width: w.to_string(),
            height: h.to_string(),
            anchor: Anchor::Center,
            transparent: false,
            error: None,
        }
    }

    /// Shows the dialog; returns the size, anchor and whether the new canvas is transparent
    /// (or the secondary colour) with OK, `Some(None)` if cancelled.
    pub fn show(&mut self, ui: &mut Ui) -> Option<Option<(i32, i32, Anchor, bool)>> {
        let t = ui.theme().clone();
        let mut out = None;
        ui.modal(480, 350, |ui, card| {
            let inner = card.inset(24, 20, 24, 20);
            ui.heading(Rect::new(inner.x, inner.y, inner.w, 26), "Canvas size");
            field(ui, inner.x, inner.y + 44, "Width", "canvas-width", &mut self.width, "pixels");
            field(ui, inner.x, inner.y + 86, "Height", "canvas-height", &mut self.height, "pixels");
            // Where the picture goes: a square of nine.
            ui.label(
                Rect::new(inner.x + 290, inner.y + 40, 120, 20),
                "Anchor",
                Font::Regular,
                13.0,
                t.text_dim,
                Align::Left,
            );
            for a in Anchor::ALL {
                let (c, r) = a.cell();
                let cell = Rect::new(inner.x + 290 + c * 34, inner.y + 64 + r * 34, 30, 30);
                let id = ui.id("anchor") ^ ((c * 3 + r) as u64 + 1);
                let resp = ui.interact(id, cell);
                let on = a == self.anchor;
                let fill = if on {
                    t.accent
                } else if resp.hovered {
                    t.control_hover
                } else {
                    t.control
                };
                ui.canvas.fill_rounded_rect(cell, 4.0, fill);
                if on {
                    ui.canvas.fill_rounded_rect(cell.inset(9, 9, 9, 9), 2.0, Color::WHITE);
                }
                if resp.hovered {
                    ui.set_cursor(Cursor::Hand);
                }
                if resp.clicked {
                    self.anchor = a;
                }
            }
            let mut transparent = self.transparent;
            ui.checkbox(
                Rect::new(inner.x, inner.y + 180, 360, 30),
                "New canvas transparent (else the secondary colour)",
                &mut transparent,
            );
            self.transparent = transparent;
            if let Some(e) = &self.error {
                ui.label(
                    Rect::new(inner.x, inner.bottom() - 72, inner.w, 24),
                    e,
                    Font::Regular,
                    13.0,
                    t.danger,
                    Align::Left,
                );
            }
            match buttons(ui, inner, "Apply") {
                Some(Answer::Ok) => match (side(&self.width, "width"), side(&self.height, "height")) {
                    (Ok(w), Ok(h)) => out = Some(Some((w, h, self.anchor, self.transparent))),
                    (Err(e), _) | (_, Err(e)) => self.error = Some(e),
                },
                Some(Answer::Cancel) => out = Some(None),
                None => {}
            }
        });
        out
    }
}

/// An effect's dialog, its settings shown on the picture as they change.
pub struct Adjust {
    pub kind: AdjustKind,
    a: f32,
    b: f32,
    /// The settings shown on the picture.
    shown: Option<(i32, i32)>,
}

/// The effects with settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdjustKind {
    Blur,
    Sharpen,
    BrightnessContrast,
}

impl Adjust {
    pub fn new(kind: AdjustKind) -> Adjust {
        let (a, b) = match kind {
            AdjustKind::Blur => (4.0, 0.0),
            AdjustKind::Sharpen => (100.0, 0.0),
            AdjustKind::BrightnessContrast => (0.0, 0.0),
        };
        Adjust { kind, a, b, shown: None }
    }

    fn effect(&self) -> Effect {
        match self.kind {
            AdjustKind::Blur => Effect::Blur(self.a as i32),
            AdjustKind::Sharpen => Effect::Sharpen(self.a / 100.0),
            AdjustKind::BrightnessContrast => Effect::BrightnessContrast(self.a as i32, self.b as i32),
        }
    }

    /// Shows the dialog, and the effect on the picture; returns the answer once given (the
    /// effect applied with OK, taken back with Cancel).
    pub fn show(&mut self, ui: &mut Ui, doc: &mut Document) -> Option<Answer> {
        let t = ui.theme().clone();
        let mut out = None;
        let (title, rows): (&str, &[(&str, f32, f32, &str)]) = match self.kind {
            AdjustKind::Blur => ("Blur", &[("Radius", 1.0, 50.0, " px")]),
            AdjustKind::Sharpen => ("Sharpen", &[("Amount", 10.0, 400.0, "%")]),
            AdjustKind::BrightnessContrast => {
                ("Brightness and contrast", &[("Brightness", -100.0, 100.0, ""), ("Contrast", -100.0, 100.0, "")])
            }
        };
        let h = 150 + rows.len() as i32 * 44;
        // Over the window's corner, so the picture shows beside it.
        ui.modal(440, h, |ui, card| {
            let inner = card.inset(24, 20, 24, 20);
            ui.heading(Rect::new(inner.x, inner.y, inner.w, 26), title);
            for (i, &(label, min, max, unit)) in rows.iter().enumerate() {
                let y = inner.y + 44 + i as i32 * 44;
                ui.label(Rect::new(inner.x, y, 100, 32), label, Font::Regular, t.font_size, t.text_dim, Align::Left);
                let value = if i == 0 { &mut self.a } else { &mut self.b };
                ui.slider(Rect::new(inner.x + 100, y + 4, inner.w - 170, 24), label, value, min, max);
                *value = libm::roundf(*value);
                let text = format!("{}{unit}", *value as i32);
                ui.label(
                    Rect::new(inner.right() - 60, y, 60, 32),
                    &text,
                    Font::Bold,
                    t.font_size,
                    t.text,
                    Align::Right,
                );
            }
            out = buttons(ui, inner, "Apply");
        });
        // The picture shows the settings once they rest (a blur of a large picture takes a
        // while under emulation).
        let settings = (self.a as i32, self.b as i32);
        if out.is_none() && self.shown != Some(settings) && !ui.input.down[0] {
            doc.preview_effect(self.effect());
            self.shown = Some(settings);
        }
        match out {
            Some(Answer::Ok) => {
                if self.shown != Some(settings) {
                    doc.preview_effect(self.effect());
                }
                doc.commit();
            }
            Some(Answer::Cancel) => {
                doc.cancel();
            }
            None => {}
        }
        if out.is_some() {
            ui.repaint();
        }
        out
    }
}
