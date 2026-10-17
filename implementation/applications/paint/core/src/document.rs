// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! A picture being edited: its pixels, its history, its selection, and the edit under way.
//!
//! Every change is one step of the history, which Undo takes back whole. Some edits stay open
//! while the pointer works on them, drawn into the picture as they will be once done, and
//! drawn again from the original pixels as they change: a stroke as it grows, a shape whose
//! corners still move, text as it is typed, and a piece of the picture lifted out of its
//! selection and moved (the place it left transparent). Starting anything else ends the edit
//! under way and keeps it ([`Document::commit`]); Undo takes it back ([`Document::cancel`]).

use alloc::string::String;

use gfx::{Bitmap, Canvas, Color, Filter, Rect, Text};
use image::Format;

use crate::brush::{self, Stroke, Tip, union};
use crate::color::{premultiplied, unpremultiply};
use crate::effects::{self, Effect};
use crate::fill::{self, Reach};
use crate::history::{History, Undone};
use crate::shapes::Shape;
use crate::text::TextBox;
use crate::transform::{self, Anchor, Resample};

/// A piece of the picture lifted out of the selection (or pasted), being moved.
#[derive(Debug, Clone)]
pub struct Floating {
    pub pixels: Bitmap,
    /// Where it is on the picture (it is scaled to fill it).
    pub rect: Rect,
    /// Where it was lifted from (left transparent), if it was lifted.
    source: Option<Rect>,
    /// Where it was last drawn.
    drawn: Rect,
}

/// The edit under way.
enum Live {
    None,
    Stroke(Stroke),
    Shape {
        shape: Shape,
        drawn: Rect,
    },
    Text {
        text: TextBox,
        drawn: Rect,
    },
    Float(Floating),
    /// An effect shown before it is applied (its dialog is open): on `area`.
    Effect {
        area: Rect,
    },
}

/// A picture being edited.
pub struct Document {
    picture: Bitmap,
    history: History,
    /// Its file, once opened from or saved to one.
    pub path: Option<String>,
    /// The format it is saved in.
    pub format: Format,
    selection: Option<Rect>,
    live: Live,
    /// Where the picture changed since it was last drawn: `Some(None)` is everywhere.
    changed: Option<Option<Rect>>,
    /// Where the last stroke ended, for Shift and a click to draw a line from.
    last_point: Option<(f32, f32)>,
}

impl Document {
    /// A new `w` by `h` picture of `background`, keeping its history within `budget` bytes.
    pub fn new(w: i32, h: i32, background: Color, budget: usize) -> Document {
        let (w, h) = (w.clamp(1, transform::MAX_SIDE), h.clamp(1, transform::MAX_SIDE));
        let picture = brush::filled(w, h, premultiplied(background, 255));
        Document::from_picture(picture, None, Format::Png, budget)
    }

    /// A picture opened from `path`.
    pub fn from_picture(picture: Bitmap, path: Option<String>, format: Format, budget: usize) -> Document {
        let mut history = History::new(budget);
        history.clear();
        Document {
            picture,
            history,
            path,
            format,
            selection: None,
            live: Live::None,
            changed: Some(None),
            last_point: None,
        }
    }

    pub fn picture(&self) -> &Bitmap {
        &self.picture
    }

    pub fn width(&self) -> i32 {
        self.picture.width
    }

    pub fn height(&self) -> i32 {
        self.picture.height
    }

    pub fn history(&self) -> &History {
        &self.history
    }

    /// Whether the picture differs from its file (or, new, from nothing).
    pub fn is_modified(&self) -> bool {
        self.history.is_modified() || !matches!(self.live, Live::None)
    }

    /// The picture as it is is now in `path`, in `format`.
    pub fn mark_saved(&mut self, path: &str, format: Format) {
        self.commit();
        self.history.mark_saved();
        self.path = Some(path.into());
        self.format = format;
    }

    /// The picture as it is now, with the edit under way done, and its number: what a save
    /// writes, and what [`Document::saved_version`] then takes.
    pub fn snapshot(&mut self) -> (Bitmap, u64) {
        self.commit();
        (self.picture.clone(), self.history.version())
    }

    /// The picture numbered `version` (from [`Document::snapshot`]) is now in `path`, in
    /// `format`; edits made since keep the picture modified.
    pub fn saved_version(&mut self, version: u64, path: &str, format: Format) {
        self.history.mark_saved_at(version);
        self.path = Some(path.into());
        self.format = format;
    }

    /// Where the picture changed since this was last asked (`Some(None)`: everywhere).
    pub fn take_changed(&mut self) -> Option<Option<Rect>> {
        self.changed.take()
    }

    fn note(&mut self, r: Rect) {
        if r.is_empty() {
            return;
        }
        self.changed = Some(match self.changed {
            Some(None) => None,
            Some(Some(old)) => Some(old.union(&r)),
            None => Some(r),
        });
    }

    fn note_all(&mut self) {
        self.changed = Some(None);
    }

    /// Ends the edit under way, keeping what it did.
    pub fn commit(&mut self) {
        match core::mem::replace(&mut self.live, Live::None) {
            Live::None => {}
            Live::Stroke(mut s) => {
                let r = s.finish();
                self.composite(&s, r);
                self.last_point = s.last();
                self.history.commit();
            }
            Live::Text { text, .. } if text.text.trim().is_empty() => {
                if let Some(u) = self.history.abort(&mut self.picture) {
                    self.note_undone(&u);
                }
            }
            Live::Shape { .. } | Live::Text { .. } | Live::Effect { .. } => {
                self.history.commit();
            }
            Live::Float(f) => {
                self.history.commit();
                self.selection = Some(f.rect);
            }
        }
    }

    /// Ends the edit under way, taking back what it did. Returns whether there was one.
    pub fn cancel(&mut self) -> bool {
        let was = core::mem::replace(&mut self.live, Live::None);
        if matches!(was, Live::None) {
            return false;
        }
        if let Some(u) = self.history.abort(&mut self.picture) {
            self.note_undone(&u);
        }
        if let Live::Float(f) = was {
            // The selection goes back to where the piece came from.
            self.selection = f.source;
        }
        true
    }

    fn note_undone(&mut self, u: &Undone) {
        match u.area {
            Some(r) => self.note(r),
            None => self.note_all(),
        }
    }

    /// Composites a stroke's `r` (keeping its tiles first).
    fn composite(&mut self, s: &Stroke, r: Rect) {
        if r.is_empty() {
            return;
        }
        self.history.touch(&self.picture, r);
        s.composite(&mut self.picture, &self.history, r);
        self.note(r);
    }

    // ---- strokes -----------------------------------------------------------------

    /// Begins a stroke of `style` at (`x`, `y`) (picture coordinates).
    pub fn stroke_begin(&mut self, style: brush::Style, x: f32, y: f32) {
        self.commit();
        let name = match style.tip {
            Tip::Pencil => "Pencil",
            Tip::Brush => "Brush stroke",
            Tip::Airbrush => "Airbrush",
            Tip::Eraser => "Eraser",
        };
        self.history.begin(name, &self.picture);
        let mut s = Stroke::new(style, self.picture.width, self.picture.height);
        let r = s.to(x, y);
        self.composite(&s, r);
        self.live = Live::Stroke(s);
    }

    /// The stroke under way reaches (`x`, `y`).
    pub fn stroke_to(&mut self, x: f32, y: f32) {
        let Live::Stroke(mut s) = core::mem::replace(&mut self.live, Live::None) else { return };
        let r = s.to(x, y);
        self.composite(&s, r);
        self.live = Live::Stroke(s);
    }

    /// The airbrush under way is held still for `ns` nanoseconds.
    pub fn stroke_spray(&mut self, ns: u64) {
        let Live::Stroke(mut s) = core::mem::replace(&mut self.live, Live::None) else { return };
        let r = s.spray(ns);
        self.composite(&s, r);
        self.live = Live::Stroke(s);
    }

    /// Whether a stroke is under way.
    pub fn is_stroking(&self) -> bool {
        matches!(self.live, Live::Stroke(_))
    }

    /// Ends the stroke under way.
    pub fn stroke_end(&mut self) {
        if self.is_stroking() {
            self.commit();
        }
    }

    /// Draws a straight line of `style` from where the last stroke ended to (`x`, `y`), as
    /// Shift and a click do. Returns whether there was a stroke to start from.
    pub fn line_from_last(&mut self, style: brush::Style, x: f32, y: f32) -> bool {
        let Some(from) = self.last_point else { return false };
        self.commit();
        self.history.begin("Line", &self.picture);
        let s = brush::line(style, self.picture.width, self.picture.height, from, (x, y));
        self.composite(&s, s.bounds());
        self.history.commit();
        self.last_point = Some((x, y));
        true
    }

    /// A line of `style` straight from point to point of `points` (the voice agent's lines).
    pub fn stroke_path(&mut self, style: brush::Style, points: &[(f32, f32)]) {
        let Some(&(x, y)) = points.first() else { return };
        self.stroke_begin(style, x, y);
        let Live::Stroke(mut s) = core::mem::replace(&mut self.live, Live::None) else { return };
        for &(x, y) in &points[1..] {
            let r = s.line_to(x, y);
            self.composite(&s, r);
        }
        self.live = Live::Stroke(s);
        self.stroke_end();
    }

    // ---- fills and colours ---------------------------------------------------------

    /// Fills the area at (`x`, `y`) (see [`fill::area`]). Returns whether it was on the
    /// picture.
    pub fn fill(&mut self, x: i32, y: i32, color: Color, opacity: u8, tolerance: u8, reach: Reach, soft: bool) -> bool {
        self.commit();
        let Some(area) = fill::area(&self.picture, x, y, tolerance, reach, soft) else { return false };
        self.history.begin("Fill", &self.picture);
        let r = fill::paint(&mut self.picture, &mut self.history, &area, color, opacity);
        self.history.commit();
        self.note(r);
        true
    }

    /// The colour of the pixel at (`x`, `y`) (straight alpha), if it is on the picture.
    pub fn color_at(&self, x: i32, y: i32) -> Option<Color> {
        self.picture.rect().contains(x, y).then(|| Color(unpremultiply(self.picture.get(x, y))))
    }

    // ---- shapes ------------------------------------------------------------------

    /// The shape not yet placed, if any.
    pub fn shape(&self) -> Option<&Shape> {
        match &self.live {
            Live::Shape { shape, .. } => Some(shape),
            _ => None,
        }
    }

    /// Draws `shape` as the shape not yet placed (beginning one, or changing it).
    pub fn set_shape(&mut self, shape: Shape) {
        let drawn = match core::mem::replace(&mut self.live, Live::None) {
            Live::Shape { drawn, .. } => drawn,
            other => {
                self.live = other;
                self.commit();
                self.history.begin(shape.kind.name(), &self.picture);
                Rect::default()
            }
        };
        self.history.restore(&mut self.picture, drawn);
        let bounds = shape.bounds().intersect(&self.picture.rect());
        self.history.touch(&self.picture, bounds);
        let mut canvas = Canvas::for_bitmap(&mut self.picture);
        canvas.clip_to(bounds);
        shape.draw(&mut canvas);
        self.note(union(drawn, bounds));
        self.live = Live::Shape { shape, drawn: bounds };
    }

    /// Draws a shape and places it at once (the voice agent's shapes).
    pub fn draw_shape(&mut self, shape: Shape) {
        self.set_shape(shape);
        self.commit();
    }

    // ---- text --------------------------------------------------------------------

    /// The text not yet placed, if any.
    pub fn text(&self) -> Option<&TextBox> {
        match &self.live {
            Live::Text { text, .. } => Some(text),
            _ => None,
        }
    }

    /// Draws `text` as the text not yet placed (beginning it, or changing it), with the fonts
    /// of `ctx`.
    pub fn set_text(&mut self, text: TextBox, ctx: &mut Text) {
        let drawn = match core::mem::replace(&mut self.live, Live::None) {
            Live::Text { drawn, .. } => drawn,
            other => {
                self.live = other;
                self.commit();
                self.history.begin("Text", &self.picture);
                Rect::default()
            }
        };
        self.history.restore(&mut self.picture, drawn);
        let reach = text.reach(ctx).intersect(&self.picture.rect());
        self.history.touch(&self.picture, reach);
        text.draw(ctx, &mut self.picture, reach);
        self.note(union(drawn, reach));
        self.live = Live::Text { text, drawn: reach };
    }

    // ---- selection -----------------------------------------------------------------

    /// The selection (picture pixels), if any.
    pub fn selection(&self) -> Option<Rect> {
        match &self.live {
            Live::Float(f) => Some(f.rect),
            _ => self.selection,
        }
    }

    /// The piece being moved, if any.
    pub fn floating(&self) -> Option<&Floating> {
        match &self.live {
            Live::Float(f) => Some(f),
            _ => None,
        }
    }

    /// Selects `r` (what of it is on the picture; nothing if none is).
    pub fn select(&mut self, r: Rect) {
        self.commit();
        let r = r.intersect(&self.picture.rect());
        self.selection = (!r.is_empty()).then_some(r);
    }

    pub fn select_all(&mut self) {
        self.select(self.picture.rect());
    }

    /// Places a piece being moved, and selects nothing.
    pub fn deselect(&mut self) {
        self.commit();
        self.selection = None;
    }

    /// Lifts the selection's pixels into a piece to move (where they were becomes
    /// transparent). Returns whether there is a piece (one may be lifted already).
    pub fn lift(&mut self) -> bool {
        if matches!(self.live, Live::Float(_)) {
            return true;
        }
        let Some(sel) = self.selection else { return false };
        self.commit();
        let r = sel.intersect(&self.picture.rect());
        if r.is_empty() {
            return false;
        }
        self.history.begin("Move selection", &self.picture);
        let pixels = self.picture.crop(r);
        self.history.touch(&self.picture, r);
        clear_rect(&mut self.picture, r, 0);
        self.live = Live::Float(Floating { pixels, rect: r, source: Some(r), drawn: Rect::default() });
        self.draw_float();
        true
    }

    /// Draws the piece being moved where it is now, over the picture as it is without it.
    fn draw_float(&mut self) {
        let Live::Float(mut f) = core::mem::replace(&mut self.live, Live::None) else { return };
        let old = f.drawn;
        self.history.restore(&mut self.picture, old);
        if let Some(src) = f.source {
            clear_rect(&mut self.picture, src.intersect(&old), 0);
        }
        let target = f.rect.intersect(&self.picture.rect());
        self.history.touch(&self.picture, target);
        let mut canvas = Canvas::for_bitmap(&mut self.picture);
        if (f.rect.w, f.rect.h) == (f.pixels.width, f.pixels.height) {
            canvas.draw_bitmap(&f.pixels, f.rect.x, f.rect.y, 255);
        } else {
            canvas.draw_bitmap_scaled(&f.pixels, f.rect, Filter::Bilinear, 255);
        }
        f.drawn = target;
        self.note(union(old, target));
        self.live = Live::Float(f);
    }

    /// Moves (and scales) the piece being moved to `r`.
    pub fn move_float(&mut self, r: Rect) {
        if r.w < 1 || r.h < 1 {
            return;
        }
        if let Live::Float(f) = &mut self.live {
            f.rect = r;
            self.draw_float();
        }
    }

    /// Pastes `pixels` as a piece to move, its top left at `at`.
    pub fn paste(&mut self, pixels: Bitmap, at: (i32, i32)) {
        self.commit();
        self.history.begin("Paste", &self.picture);
        let rect = Rect::new(at.0, at.1, pixels.width, pixels.height);
        self.live = Live::Float(Floating { pixels, rect, source: None, drawn: Rect::default() });
        self.draw_float();
    }

    /// The selection's pixels (the piece being moved, as it is drawn).
    pub fn copy(&self) -> Option<Bitmap> {
        if let Live::Float(f) = &self.live {
            return Some(if (f.rect.w, f.rect.h) == (f.pixels.width, f.pixels.height) {
                f.pixels.clone()
            } else {
                f.pixels.resized(f.rect.w, f.rect.h)
            });
        }
        let r = self.selection?.intersect(&self.picture.rect());
        (!r.is_empty()).then(|| self.picture.crop(r))
    }

    /// Takes the selection's pixels away (to transparency). Returns whether there was a
    /// selection.
    pub fn delete_selection(&mut self) -> bool {
        if let Live::Float(f) = &self.live {
            // The piece goes; where it was lifted from stays clear.
            let (drawn, source) = (f.drawn, f.source);
            self.history.restore(&mut self.picture, drawn);
            if let Some(src) = source {
                clear_rect(&mut self.picture, src.intersect(&drawn), 0);
            }
            self.note(drawn);
            self.live = Live::None;
            self.history.commit();
            self.selection = None;
            return true;
        }
        self.commit();
        let Some(r) = self.selection.map(|s| s.intersect(&self.picture.rect())).filter(|r| !r.is_empty()) else {
            return false;
        };
        self.history.begin("Delete", &self.picture);
        self.history.touch(&self.picture, r);
        clear_rect(&mut self.picture, r, 0);
        self.history.commit();
        self.note(r);
        true
    }

    /// Crops the picture to the selection. Returns whether there was one.
    pub fn crop_to_selection(&mut self) -> bool {
        self.commit();
        let Some(r) = self.selection.map(|s| s.intersect(&self.picture.rect())).filter(|r| !r.is_empty()) else {
            return false;
        };
        self.crop(r);
        true
    }

    /// Crops the picture to `r`.
    pub fn crop(&mut self, r: Rect) -> bool {
        self.commit();
        let r = r.intersect(&self.picture.rect());
        if r.is_empty() {
            return false;
        }
        let cropped = self.picture.crop(r);
        self.replace("Crop", cropped);
        true
    }

    /// An edit `name` that replaces the picture with `picture` (of any size).
    fn replace(&mut self, name: &str, picture: Bitmap) {
        self.commit();
        self.history.begin(name, &self.picture);
        let old = core::mem::replace(&mut self.picture, picture);
        self.history.replace_whole(old);
        self.history.commit();
        self.selection = None;
        self.note_all();
    }

    // ---- the picture, or the selection --------------------------------------------

    /// Turns the selection (lifting it) or else the picture by `quarters` quarter turns
    /// clockwise (negative: anticlockwise).
    pub fn rotate(&mut self, quarters: i32) {
        let q = quarters.rem_euclid(4);
        if q == 0 {
            return;
        }
        let turn = |b: &Bitmap| match q {
            1 => transform::rotate_cw(b),
            3 => transform::rotate_ccw(b),
            _ => {
                let mut c = b.clone();
                transform::rotate_half(&mut c);
                c
            }
        };
        if self.selection.is_some() && self.lift() {
            if let Live::Float(f) = &mut self.live {
                f.pixels = turn(&f.pixels);
                if q % 2 == 1 {
                    let (cx, cy) = (f.rect.x + f.rect.w / 2, f.rect.y + f.rect.h / 2);
                    f.rect = Rect::new(cx - f.rect.h / 2, cy - f.rect.w / 2, f.rect.h, f.rect.w);
                }
                self.draw_float();
            }
            return;
        }
        let turned = turn(&self.picture);
        self.replace(if q == 2 { "Rotate 180°" } else { "Rotate" }, turned);
    }

    /// Mirrors the selection (lifting it) or else the picture.
    pub fn flip(&mut self, horizontal: bool) {
        let mirror = |b: &mut Bitmap| {
            if horizontal { transform::flip_horizontal(b) } else { transform::flip_vertical(b) }
        };
        if self.selection.is_some() && self.lift() {
            if let Live::Float(f) = &mut self.live {
                mirror(&mut f.pixels);
                self.draw_float();
            }
            return;
        }
        self.commit();
        let r = self.picture.rect();
        self.history.begin(if horizontal { "Flip horizontal" } else { "Flip vertical" }, &self.picture);
        self.history.touch(&self.picture, r);
        mirror(&mut self.picture);
        self.history.commit();
        self.note_all();
    }

    /// Resizes the picture to `w` by `h`.
    pub fn resize(&mut self, w: i32, h: i32, how: Resample) {
        self.commit();
        let resized = transform::resize(&self.picture, w, h, how);
        self.replace("Resize", resized);
    }

    /// Makes the canvas `w` by `h`, the picture placed by `anchor`, `fill` where it was not.
    pub fn canvas_size(&mut self, w: i32, h: i32, anchor: Anchor, fill: Color) {
        self.commit();
        let sized = transform::canvas_size(&self.picture, w, h, anchor, premultiplied(fill, 255));
        self.replace("Canvas size", sized);
    }

    /// The selection, or the whole picture.
    fn target(&self) -> Rect {
        self.selection.map_or(self.picture.rect(), |s| s.intersect(&self.picture.rect()))
    }

    /// Applies `effect` to the selection, or the whole picture.
    pub fn effect(&mut self, effect: Effect) {
        self.preview_effect(effect);
        self.commit();
    }

    /// Shows `effect` on the selection, or the whole picture, as it would be applied (again,
    /// with other settings, as its dialog's change): [`Document::commit`] applies it, and
    /// [`Document::cancel`] takes it back.
    pub fn preview_effect(&mut self, effect: Effect) {
        let area = match core::mem::replace(&mut self.live, Live::None) {
            Live::Effect { area } => {
                self.history.restore(&mut self.picture, area);
                area
            }
            other => {
                self.live = other;
                self.commit();
                let r = self.target();
                self.history.begin(effect.name(), &self.picture);
                self.history.touch(&self.picture, r);
                r
            }
        };
        effects::apply(&mut self.picture, area, effect);
        self.note(area);
        self.live = Live::Effect { area };
    }

    /// Fills the selection, or the whole picture, with `color`.
    pub fn clear(&mut self, color: Color) {
        self.commit();
        let r = self.target();
        self.history.begin("Clear", &self.picture);
        self.history.touch(&self.picture, r);
        clear_rect(&mut self.picture, r, premultiplied(color, 255));
        self.history.commit();
        self.note(r);
    }

    // ---- undo ----------------------------------------------------------------------

    /// Undoes the last step: the edit under way, if there is one.
    pub fn undo(&mut self) -> Option<String> {
        if !matches!(self.live, Live::None) {
            let name = match &self.live {
                Live::Shape { shape, .. } => String::from(shape.kind.name()),
                Live::Text { .. } => String::from("Text"),
                Live::Float(_) => String::from("Move selection"),
                Live::Effect { .. } => String::from("Effect"),
                _ => String::from("Stroke"),
            };
            self.cancel();
            return Some(name);
        }
        let u = self.history.undo(&mut self.picture)?;
        self.note_undone(&u);
        self.selection = None;
        Some(u.name)
    }

    /// Redoes the last step undone.
    pub fn redo(&mut self) -> Option<String> {
        if !matches!(self.live, Live::None) {
            return None;
        }
        let u = self.history.redo(&mut self.picture)?;
        self.note_undone(&u);
        self.selection = None;
        Some(u.name)
    }
}

/// Sets every pixel of `r` (within the picture) to `px`.
fn clear_rect(b: &mut Bitmap, r: Rect, px: u32) {
    let r = r.intersect(&b.rect());
    for y in r.y..r.bottom() {
        let start = (y * b.width + r.x) as usize;
        b.pixels[start..start + r.w as usize].fill(px);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shapes::{Kind, Paint};

    const WHITE: u32 = 0xFFFF_FFFF;
    const RED: u32 = 0xFFFF_0000;

    fn doc() -> Document {
        Document::new(100, 80, Color::WHITE, 64 << 20)
    }

    fn brush() -> brush::Style {
        brush::Style { tip: Tip::Brush, size: 6.0, hardness: 1.0, opacity: 255, color: Color::hex(0xFF0000) }
    }

    fn rect_shape(a: (f32, f32), b: (f32, f32)) -> Shape {
        Shape {
            kind: Kind::Rectangle,
            a,
            b,
            paint: Paint::Fill,
            width: 2.0,
            outline: Color::hex(0xFF0000),
            fill: Color::BLACK,
        }
    }

    #[test]
    fn strokes_are_steps() {
        let mut d = doc();
        assert!(!d.is_modified());
        d.stroke_begin(brush(), 10.0, 40.0);
        d.stroke_to(90.0, 40.0);
        assert!(d.is_modified());
        d.stroke_end();
        assert_eq!(d.picture().get(50, 40), RED);
        assert_eq!(d.history().undo_name(), Some("Brush stroke"));
        assert_eq!(d.undo().as_deref(), Some("Brush stroke"));
        assert_eq!(d.picture().get(50, 40), WHITE);
        assert!(!d.is_modified());
        assert_eq!(d.redo().as_deref(), Some("Brush stroke"));
        assert_eq!(d.picture().get(50, 40), RED);
        // Shift and a click: a line from where the stroke ended.
        assert!(d.line_from_last(brush(), 90.0, 70.0));
        assert_eq!(d.picture().get(90, 60), RED);
        // A path given point by point is straight between them: through its corner.
        d.stroke_path(brush(), &[(10.0, 70.0), (30.0, 10.0), (50.0, 70.0)]);
        assert_eq!(d.picture().get(30, 11), RED);
    }

    #[test]
    fn a_shape_moves_until_it_is_placed() {
        let mut d = doc();
        d.set_shape(rect_shape((10.0, 10.0), (30.0, 30.0)));
        assert_eq!(d.picture().get(20, 20), RED);
        d.set_shape(rect_shape((50.0, 10.0), (70.0, 30.0)));
        assert_eq!(d.picture().get(20, 20), WHITE, "drawn again where it moved");
        assert_eq!(d.picture().get(60, 20), RED);
        assert!(d.shape().is_some());
        d.commit();
        assert!(d.shape().is_none());
        assert_eq!(d.history().names().count(), 1);
        // Esc takes a shape not yet placed away.
        d.set_shape(rect_shape((10.0, 50.0), (20.0, 60.0)));
        assert!(d.cancel());
        assert_eq!(d.picture().get(15, 55), WHITE);
        assert_eq!(d.history().names().count(), 1);
    }

    #[test]
    fn a_selection_lifts_moves_and_lands() {
        let mut d = doc();
        d.draw_shape(rect_shape((10.0, 10.0), (20.0, 20.0)));
        d.select(Rect::new(5, 5, 20, 20));
        assert!(d.lift());
        d.move_float(Rect::new(55, 45, 20, 20));
        assert_eq!(d.picture().get(15, 15), 0, "where it was is transparent");
        assert_eq!(d.picture().get(65, 55), RED);
        d.move_float(Rect::new(10, 8, 20, 20));
        assert_eq!(d.picture().get(15, 13), RED, "moved over where it came from");
        assert_eq!(d.picture().get(65, 55), WHITE, "and gone from where it was");
        d.deselect();
        assert_eq!(d.selection(), None);
        d.undo();
        assert_eq!(d.picture().get(15, 15), RED, "the whole move is one step");
        assert_eq!(d.picture().get(15, 9), WHITE);
        // Esc puts a lifted piece back.
        d.select(Rect::new(5, 5, 20, 20));
        d.lift();
        d.move_float(Rect::new(60, 40, 20, 20));
        d.cancel();
        assert_eq!(d.picture().get(15, 15), RED);
        assert_eq!(d.selection(), Some(Rect::new(5, 5, 20, 20)));
    }

    #[test]
    fn copy_paste_delete_and_crop() {
        let mut d = doc();
        d.draw_shape(rect_shape((0.0, 0.0), (10.0, 10.0)));
        d.select(Rect::new(0, 0, 10, 10));
        let piece = d.copy().unwrap();
        assert_eq!((piece.width, piece.height, piece.get(5, 5)), (10, 10, RED));
        d.paste(piece, (50, 50));
        assert_eq!(d.picture().get(55, 55), RED);
        d.deselect();
        d.select(Rect::new(0, 0, 10, 10));
        assert!(d.delete_selection());
        assert_eq!(d.picture().get(5, 5), 0);
        d.select(Rect::new(40, 40, 30, 20));
        assert!(d.crop_to_selection());
        assert_eq!((d.width(), d.height()), (30, 20));
        assert_eq!(d.picture().get(15, 15), RED);
        d.undo();
        assert_eq!((d.width(), d.height()), (100, 80));
    }

    #[test]
    fn turns_flips_and_effects_on_the_picture_or_the_selection() {
        let mut d = doc();
        d.draw_shape(rect_shape((0.0, 0.0), (10.0, 10.0)));
        d.rotate(1);
        assert_eq!((d.width(), d.height()), (80, 100));
        assert_eq!(d.picture().get(75, 5), RED);
        d.rotate(-1);
        d.flip(true);
        assert_eq!(d.picture().get(95, 5), RED);
        d.undo();
        assert_eq!(d.picture().get(5, 5), RED);
        // A selection turns in place.
        d.select(Rect::new(0, 0, 20, 10));
        d.rotate(1);
        assert_eq!(d.floating().map(|f| f.rect), Some(Rect::new(5, -5, 10, 20)));
        d.deselect();
        d.effect(Effect::Invert);
        assert_eq!(d.picture().get(50, 50), 0xFF00_0000, "white inverted is black");
        d.select(Rect::new(0, 0, 10, 10));
        d.clear(Color::hex(0x00FF00));
        assert_eq!(d.picture().get(5, 5), 0xFF00_FF00);
        assert_eq!(d.picture().get(50, 50), 0xFF00_0000);
    }

    #[test]
    fn resize_canvas_and_fill() {
        let mut d = doc();
        d.resize(50, 40, Resample::Smooth);
        assert_eq!((d.width(), d.height()), (50, 40));
        d.canvas_size(60, 40, Anchor::Left, Color::hex(0x0000FF));
        assert_eq!(d.picture().get(55, 20), 0xFF00_00FF);
        assert!(d.fill(10, 10, Color::hex(0xFF0000), 255, 0, Reach::Contiguous, false));
        assert_eq!(d.picture().get(49, 39), RED);
        assert_eq!(d.picture().get(55, 20), 0xFF00_00FF);
        assert_eq!(d.color_at(55, 20), Some(Color::hex(0x0000FF)));
        assert_eq!(d.color_at(60, 20), None);
        assert!(d.take_changed().is_some());
        assert!(d.take_changed().is_none());
    }

    #[test]
    fn text_is_typed_into_the_picture() {
        const INTER: &[u8] = include_bytes!("../../../../ports/fonts/Inter-Regular.otf");
        let mut ctx = Text::new();
        ctx.add_font(INTER);
        let mut d = doc();
        let mut t = TextBox::new((10, 10), 80, 20.0, 0, Color::BLACK);
        t.insert("Hi");
        d.set_text(t.clone(), &mut ctx);
        let inked = |d: &Document| d.picture().pixels.iter().filter(|&&p| p != WHITE).count();
        let some = inked(&d);
        assert!(some > 20);
        t.insert(" there");
        d.set_text(t, &mut ctx);
        assert!(inked(&d) > some);
        d.commit();
        assert_eq!(d.history().undo_name(), Some("Text"));
        // An empty box leaves nothing.
        let empty = TextBox::new((10, 40), 80, 20.0, 0, Color::BLACK);
        d.set_text(empty, &mut ctx);
        d.commit();
        assert_eq!(d.history().names().count(), 1);
    }
}
