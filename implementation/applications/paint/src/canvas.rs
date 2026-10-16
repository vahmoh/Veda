// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The canvas: the picture as the view shows it, what the pointer does to it with each tool,
//! and what is drawn over it (the selection's frame and handles, a shape's or a text's frame
//! before they are placed, the brush's outline).
//!
//! The wheel scrolls (with Shift, sideways), Ctrl and the wheel zoom about the pointer, and the
//! middle button drags the picture around. Strokes follow every position the pointer passed,
//! not one a frame.

use paint_core::Document;
use paint_core::render::Renderer;
use paint_core::settings::{FontChoice, Settings, Tool};
use paint_core::shapes::{self, Paint, Shape};
use paint_core::text::TextBox;
use paint_core::view::View;
use vgfx::{Color, Path, Rect, StrokeStyle};
use vproto::display::Cursor;
use vui::Ui;

/// A handle of a frame: its corners and the middles of its sides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Handle {
    TopLeft,
    Top,
    TopRight,
    Right,
    BottomRight,
    Bottom,
    BottomLeft,
    Left,
}

impl Handle {
    const ALL: [Handle; 8] = [
        Handle::TopLeft,
        Handle::Top,
        Handle::TopRight,
        Handle::Right,
        Handle::BottomRight,
        Handle::Bottom,
        Handle::BottomLeft,
        Handle::Left,
    ];

    /// Where it is on `r` (fractions of its width and height).
    fn at(self) -> (f32, f32) {
        match self {
            Handle::TopLeft => (0.0, 0.0),
            Handle::Top => (0.5, 0.0),
            Handle::TopRight => (1.0, 0.0),
            Handle::Right => (1.0, 0.5),
            Handle::BottomRight => (1.0, 1.0),
            Handle::Bottom => (0.5, 1.0),
            Handle::BottomLeft => (0.0, 1.0),
            Handle::Left => (0.0, 0.5),
        }
    }

    fn cursor(self) -> Cursor {
        match self {
            Handle::Top | Handle::Bottom => Cursor::ResizeVertical,
            Handle::Left | Handle::Right => Cursor::ResizeHorizontal,
            Handle::TopLeft | Handle::BottomRight => Cursor::ResizeDiagonal,
            Handle::TopRight | Handle::BottomLeft => Cursor::ResizeAntiDiagonal,
        }
    }

    /// `r` (picture pixels) with this handle moved by (`dx`, `dy`).
    fn drag(self, r: Rect, dx: i32, dy: i32) -> Rect {
        let (fx, fy) = self.at();
        let (mut x0, mut y0, mut x1, mut y1) = (r.x, r.y, r.right(), r.bottom());
        if fx == 0.0 {
            x0 += dx;
        } else if fx == 1.0 {
            x1 += dx;
        }
        if fy == 0.0 {
            y0 += dy;
        } else if fy == 1.0 {
            y1 += dy;
        }
        Rect::new(x0.min(x1 - 1), y0.min(y1 - 1), (x1 - x0).abs().max(1), (y1 - y0).abs().max(1))
    }
}

/// What the pointer is doing.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Drag {
    None,
    /// Moving the picture with the middle button: where the pointer and the picture were.
    Pan {
        from: (i32, i32),
        origin: (f32, f32),
    },
    /// A stroke under way.
    Stroke,
    /// Picking colours (the left button: the primary).
    Pick {
        primary: bool,
    },
    /// Selecting from a corner.
    Marquee {
        from: (f32, f32),
    },
    /// Moving the piece lifted from the selection: where the pointer pressed and where the
    /// piece was then.
    Move {
        from: (f32, f32),
        rect: Rect,
    },
    /// Stretching the selection (a piece lifted, or not) by a handle.
    Stretch {
        handle: Handle,
        from: (f32, f32),
        rect: Rect,
        lifted: bool,
    },
    /// Drawing a new shape from its first corner.
    NewShape,
    /// Moving the shape not yet placed.
    MoveShape {
        from: (f32, f32),
        shape: Shape,
    },
    /// Moving a corner (or an end) of the shape not yet placed.
    ShapeCorner {
        second: bool,
        shape: Shape,
    },
    /// Stretching the shape not yet placed by a handle of its frame.
    ShapeHandle {
        handle: Handle,
        from: (f32, f32),
        shape: Shape,
    },
}

/// What the canvas tells the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// The colours changed (the picker).
    Colors,
}

/// The canvas's state.
pub struct Canvas {
    pub view: View,
    pub renderer: Renderer,
    /// Where the canvas was drawn last.
    pub viewport: Rect,
    /// The picture's size the view was fitted to, and whether to fit it again.
    fitted: (i32, i32),
    pub fit_next: bool,
    drag: Drag,
    /// Where the pointer is on the picture, if over the canvas.
    pub pointer: Option<(f32, f32)>,
    /// When the airbrush last sprayed.
    sprayed: u64,
}

/// The colours a shape takes: with the left button, the primary outline and the secondary
/// inside (or the primary inside, filled only); the right button swaps them.
fn shape_colors(s: &Settings, primary: bool) -> (Color, Color) {
    let (a, b) = if primary { (s.primary, s.secondary) } else { (s.secondary, s.primary) };
    match s.shape_paint {
        Paint::Fill => (a, a),
        _ => (a, b),
    }
}

/// The font of the interface for `choice` (indices of the text context's fonts).
pub fn font_of(ui: &Ui, choice: FontChoice) -> usize {
    ui.ctx.font(match choice {
        FontChoice::Sans => vui::Font::Regular,
        FontChoice::SansBold => vui::Font::Bold,
        FontChoice::Mono => vui::Font::Mono,
        FontChoice::MonoBold => vui::Font::MonoBold,
    })
}

impl Canvas {
    pub fn new() -> Canvas {
        Canvas {
            view: View::default(),
            renderer: Renderer::default(),
            viewport: Rect::default(),
            fitted: (0, 0),
            fit_next: true,
            drag: Drag::None,
            pointer: None,
            sprayed: 0,
        }
    }

    /// The window position of a picture point, and back.
    fn to_screen(&self, p: (f32, f32)) -> (f32, f32) {
        self.view.to_screen(self.viewport, p.0, p.1)
    }

    fn to_picture(&self, x: i32, y: i32) -> (f32, f32) {
        self.view.to_picture(self.viewport, x as f32, y as f32)
    }

    /// Zooms to `zoom` about the middle of the canvas.
    pub fn zoom_to(&mut self, zoom: f32, doc: &Document) {
        let (cx, cy) = self.viewport.center();
        self.view.zoom_at(self.viewport, zoom, cx as f32, cy as f32);
        self.view.clamp(self.viewport, doc.width(), doc.height());
    }

    /// Fits the picture in the window.
    pub fn fit(&mut self, doc: &Document) {
        self.view = View::fit(self.viewport, doc.width(), doc.height());
    }

    /// The frame (window pixels) of the picture rectangle `r`.
    fn screen_rect(&self, r: Rect) -> Rect {
        self.view.rect_to_screen(self.viewport, r)
    }

    /// Which handle of `frame` (window pixels) is at (`x`, `y`).
    fn handle_at(frame: Rect, x: i32, y: i32) -> Option<Handle> {
        Handle::ALL.into_iter().find(|h| {
            let (fx, fy) = h.at();
            let (hx, hy) = (frame.x as f32 + fx * frame.w as f32, frame.y as f32 + fy * frame.h as f32);
            (x as f32 - hx).abs() <= 7.0 && (y as f32 - hy).abs() <= 7.0
        })
    }

    /// The shape's frame (picture pixels) from its two corners.
    fn shape_frame(shape: &Shape) -> Rect {
        let (x0, y0) = (shape.a.0.min(shape.b.0), shape.a.1.min(shape.b.1));
        let (x1, y1) = (shape.a.0.max(shape.b.0), shape.a.1.max(shape.b.1));
        Rect::new(
            libm::floorf(x0) as i32,
            libm::floorf(y0) as i32,
            libm::ceilf(x1 - x0) as i32,
            libm::ceilf(y1 - y0) as i32,
        )
    }

    /// Draws the canvas in `r` and does what the pointer does with the tool in hand.
    pub fn update(&mut self, ui: &mut Ui, r: Rect, doc: &mut Document, s: &mut Settings) -> Option<Event> {
        let mut event = None;
        self.viewport = r;
        ui.canvas.fill_rect(r, Color::hex(0x141418));
        let size = (doc.width(), doc.height());
        if self.fit_next || size != self.fitted {
            self.fit(doc);
            self.fitted = size;
            self.fit_next = false;
        }
        let hovered = ui.hovered(r);
        let input_pointer = ui.input.pointer;

        // The wheel: scrolling, or zooming with Ctrl.
        if hovered && (ui.input.scroll.0 != 0 || ui.input.scroll.1 != 0) {
            let (sx, sy) = ui.input.scroll;
            if ui.input.ctrl() {
                if let Some((px, py)) = input_pointer {
                    let zoom = self.view.step(sy > 0);
                    self.view.zoom_at(r, zoom, px as f32, py as f32);
                }
            } else if ui.input.shift() {
                self.view.scroll(sy as f32 * 60.0, 0.0);
            } else {
                self.view.scroll(sx as f32 * 60.0, sy as f32 * 60.0);
            }
        }
        // The middle button drags the picture around.
        if hovered
            && ui.input.pressed[2]
            && let Some(p) = input_pointer
        {
            self.drag = Drag::Pan { from: p, origin: self.view.origin };
        }
        if let Drag::Pan { from, origin } = self.drag {
            if let Some((px, py)) = input_pointer {
                self.view.origin = (origin.0 + (px - from.0) as f32, origin.1 + (py - from.1) as f32);
            }
            if !ui.input.down[2] {
                self.drag = Drag::None;
            }
        }
        self.view.clamp(r, doc.width(), doc.height());

        // The picture, with a shadow under it.
        if let Some(changed) = doc.take_changed() {
            self.renderer.changed(changed);
        }
        let shown = self.view.picture_rect(r, doc.width(), doc.height());
        ui.canvas.save();
        ui.canvas.clip_to(r);
        ui.canvas.draw_shadow(shown.translate(0, 3), 2, 14, Color::rgba(0, 0, 0, 150));
        let view = self.view;
        self.renderer.draw(&mut ui.canvas, doc.picture(), &view, r);
        ui.canvas.restore();

        self.pointer = input_pointer.filter(|_| hovered).map(|(x, y)| self.to_picture(x, y));
        let pressed = hovered && (ui.input.pressed[0] || ui.input.pressed[1]) && !ui.input.pressed[2];
        let primary = ui.input.pressed[0] || (!ui.input.pressed[1] && ui.input.down[0]);
        let held = ui.input.down[0] || ui.input.down[1];
        let now = ui.now();

        if let Some((px, py)) = input_pointer {
            let p = self.to_picture(px, py);
            match s.tool {
                Tool::Pencil | Tool::Brush | Tool::Airbrush | Tool::Eraser => {
                    if pressed {
                        let color = if primary { s.primary } else { s.secondary };
                        if let Some(style) = s.stroke(s.tool, color) {
                            if ui.input.shift() && doc.line_from_last(style, p.0, p.1) {
                                self.drag = Drag::None;
                            } else {
                                doc.stroke_begin(style, p.0, p.1);
                                self.drag = Drag::Stroke;
                                self.sprayed = now;
                            }
                        }
                    } else if self.drag == Drag::Stroke {
                        for &(mx, my) in &ui.input.moves {
                            let q = self.to_picture(mx, my);
                            doc.stroke_to(q.0, q.1);
                        }
                        if s.tool == Tool::Airbrush && held {
                            doc.stroke_spray(now.saturating_sub(self.sprayed).min(100_000_000));
                            self.sprayed = now;
                            ui.repaint_at(now + 30_000_000);
                        }
                        if !held {
                            doc.stroke_end();
                            self.drag = Drag::None;
                        }
                    }
                }
                Tool::Fill => {
                    if pressed {
                        let color = if primary { s.primary } else { s.secondary };
                        doc.fill(
                            libm::floorf(p.0) as i32,
                            libm::floorf(p.1) as i32,
                            color,
                            255,
                            s.tolerance,
                            s.reach,
                            s.soft_fill,
                        );
                    }
                }
                Tool::Picker => {
                    if pressed {
                        self.drag = Drag::Pick { primary };
                    }
                    if let Drag::Pick { primary } = self.drag {
                        if let Some(c) = doc.color_at(libm::floorf(p.0) as i32, libm::floorf(p.1) as i32) {
                            if primary {
                                s.primary = c;
                            } else {
                                s.secondary = c;
                            }
                            event = Some(Event::Colors);
                        }
                        if !held {
                            self.drag = Drag::None;
                        }
                    }
                }
                Tool::Text => {
                    if pressed {
                        let font = font_of(ui, s.font);
                        let inside = doc.text().filter(|t| {
                            ui.ctx.text.with(|ctx| t.bounds(ctx)).inflate(4).contains(p.0 as i32, p.1 as i32)
                        });
                        if let Some(t) = inside {
                            let mut t = t.clone();
                            t.cursor = ui.ctx.text.with(|ctx| t.index_at(ctx, p.0, p.1));
                            ui.ctx.text.with(|ctx| doc.set_text(t, ctx));
                        } else if doc.text().is_some() {
                            doc.commit();
                        } else if doc.picture().rect().contains(p.0 as i32, p.1 as i32) {
                            let width = (doc.width() - p.0 as i32).clamp(40, 600);
                            let mut t = TextBox::new((p.0 as i32, p.1 as i32), width, s.text_size, font, s.primary);
                            t.background = s.text_background.then_some(s.secondary);
                            ui.ctx.text.with(|ctx| doc.set_text(t, ctx));
                        }
                    }
                }
                Tool::Shape => self.shapes(ui, doc, s, p, pressed, primary, held),
                Tool::Select => self.select(ui, doc, p, pressed, held),
            }
        }
        if !held && matches!(self.drag, Drag::Pick { .. } | Drag::Marquee { .. }) {
            self.drag = Drag::None;
        }

        self.overlays(ui, r, doc, s);
        event
    }

    /// The shapes tool: drawing a shape, then moving it, its corners and its handles until it
    /// is placed.
    #[allow(clippy::too_many_arguments)]
    fn shapes(
        &mut self,
        ui: &mut Ui,
        doc: &mut Document,
        s: &Settings,
        p: (f32, f32),
        pressed: bool,
        primary: bool,
        held: bool,
    ) {
        let (mx, my) = ui.input.pointer.unwrap_or_default();
        if pressed {
            if let Some(&shape) = doc.shape() {
                let frame = self.screen_rect(Self::shape_frame(&shape));
                if shape.kind.is_line() {
                    let near = |q: (f32, f32)| {
                        let (sx, sy) = self.to_screen(q);
                        (sx - mx as f32).abs() <= 7.0 && (sy - my as f32).abs() <= 7.0
                    };
                    if near(shape.a) || near(shape.b) {
                        self.drag = Drag::ShapeCorner { second: near(shape.b), shape };
                        return;
                    }
                } else if let Some(handle) = Self::handle_at(frame, mx, my) {
                    self.drag = Drag::ShapeHandle { handle, from: p, shape };
                    return;
                }
                if frame.inflate(3).contains(mx, my) {
                    self.drag = Drag::MoveShape { from: p, shape };
                    return;
                }
                doc.commit();
            }
            let (outline, fill) = shape_colors(s, primary);
            let shape = Shape { kind: s.shape, a: p, b: p, paint: s.shape_paint, width: s.shape_width, outline, fill };
            doc.set_shape(shape);
            self.drag = Drag::NewShape;
            return;
        }
        match self.drag {
            Drag::NewShape => {
                if let Some(&shape) = doc.shape() {
                    let b = if ui.input.shift() { shapes::constrain(shape.kind, shape.a, p) } else { p };
                    if b != shape.b {
                        doc.set_shape(Shape { b, ..shape });
                    }
                    if !held {
                        // A click, not a drag: nothing to keep.
                        let (dx, dy) = (shape.b.0 - shape.a.0, shape.b.1 - shape.a.1);
                        if dx.abs() * self.view.zoom < 3.0 && dy.abs() * self.view.zoom < 3.0 {
                            doc.cancel();
                        }
                    }
                }
            }
            Drag::MoveShape { from, shape } => {
                let (dx, dy) = (libm::roundf(p.0 - from.0), libm::roundf(p.1 - from.1));
                doc.set_shape(Shape {
                    a: (shape.a.0 + dx, shape.a.1 + dy),
                    b: (shape.b.0 + dx, shape.b.1 + dy),
                    ..shape
                });
            }
            Drag::ShapeCorner { second, shape } => {
                let fixed = if second { shape.a } else { shape.b };
                let moved = if ui.input.shift() { shapes::constrain(shape.kind, fixed, p) } else { p };
                let next = if second { Shape { b: moved, ..shape } } else { Shape { a: moved, ..shape } };
                doc.set_shape(next);
            }
            Drag::ShapeHandle { handle, from, shape } => {
                let frame = Self::shape_frame(&shape);
                let r = handle.drag(frame, libm::roundf(p.0 - from.0) as i32, libm::roundf(p.1 - from.1) as i32);
                // The corners keep their sides: which corner was left, which right, stays so.
                let (left_a, top_a) = (shape.a.0 <= shape.b.0, shape.a.1 <= shape.b.1);
                let (x0, x1) = (r.x as f32, r.right() as f32);
                let (y0, y1) = (r.y as f32, r.bottom() as f32);
                let a = (if left_a { x0 } else { x1 }, if top_a { y0 } else { y1 });
                let b = (if left_a { x1 } else { x0 }, if top_a { y1 } else { y0 });
                doc.set_shape(Shape { a, b, ..shape });
            }
            _ => {}
        }
        if !held
            && matches!(
                self.drag,
                Drag::NewShape | Drag::MoveShape { .. } | Drag::ShapeCorner { .. } | Drag::ShapeHandle { .. }
            )
        {
            self.drag = Drag::None;
        }
    }

    /// The selection tool: selecting, moving and stretching.
    fn select(&mut self, ui: &mut Ui, doc: &mut Document, p: (f32, f32), pressed: bool, held: bool) {
        let (mx, my) = ui.input.pointer.unwrap_or_default();
        let pixel = |v: f32| libm::roundf(v) as i32;
        if pressed {
            if let Some(sel) = doc.selection() {
                let frame = self.screen_rect(sel);
                if let Some(handle) = Self::handle_at(frame, mx, my) {
                    let lifted = doc.floating().is_some();
                    self.drag = Drag::Stretch { handle, from: p, rect: sel, lifted };
                    return;
                }
                if frame.contains(mx, my) {
                    if ui.input.ctrl() && doc.floating().is_none() {
                        // Ctrl: a copy moves, the original stays.
                        if let Some(piece) = doc.copy() {
                            doc.paste(piece, (sel.x, sel.y));
                        }
                    } else {
                        doc.lift();
                    }
                    self.drag = Drag::Move { from: p, rect: doc.selection().unwrap_or(sel) };
                    return;
                }
                doc.deselect();
            }
            self.drag = Drag::Marquee { from: p };
            return;
        }
        match self.drag {
            Drag::Marquee { from } => {
                let (x0, y0) = (pixel(from.0.min(p.0)), pixel(from.1.min(p.1)));
                let (x1, y1) = (pixel(from.0.max(p.0)), pixel(from.1.max(p.1)));
                if x1 - x0 >= 1 && y1 - y0 >= 1 {
                    doc.select(Rect::new(x0, y0, x1 - x0, y1 - y0));
                } else if !held {
                    doc.deselect();
                }
            }
            Drag::Move { from, rect } => {
                let r = rect.translate(pixel(p.0 - from.0), pixel(p.1 - from.1));
                if doc.selection() != Some(r) {
                    doc.move_float(r);
                }
            }
            Drag::Stretch { handle, from, rect, lifted } => {
                let r = handle.drag(rect, pixel(p.0 - from.0), pixel(p.1 - from.1));
                if lifted || doc.floating().is_some() {
                    doc.move_float(r);
                } else if ui.input.alt() {
                    // Alt: the frame alone changes, as a new selection.
                    doc.select(r);
                } else if doc.lift() {
                    doc.move_float(r);
                }
            }
            _ => {}
        }
        if !held && matches!(self.drag, Drag::Move { .. } | Drag::Stretch { .. } | Drag::Marquee { .. }) {
            self.drag = Drag::None;
        }
    }

    /// A dashed frame around `r` (window pixels), black and white so it shows on anything.
    fn dashed(ui: &mut Ui, r: Rect) {
        let dash = 4;
        let edge = |ui: &mut Ui, x: i32, y: i32, w: i32, h: i32| {
            let horizontal = w > h;
            let len = if horizontal { w } else { h };
            let mut i = 0;
            while i < len {
                let n = dash.min(len - i);
                let color =
                    if (i / dash) % 2 == 0 { Color::rgba(0, 0, 0, 220) } else { Color::rgba(255, 255, 255, 230) };
                let seg = if horizontal { Rect::new(x + i, y, n, 1) } else { Rect::new(x, y + i, 1, n) };
                ui.canvas.fill_rect(seg, color);
                i += dash;
            }
        };
        edge(ui, r.x, r.y - 1, r.w, 1);
        edge(ui, r.x, r.bottom(), r.w, 1);
        edge(ui, r.x - 1, r.y, 1, r.h);
        edge(ui, r.right(), r.y, 1, r.h);
    }

    /// Small squares at a frame's handles (window pixels).
    fn handles(ui: &mut Ui, r: Rect) {
        for h in Handle::ALL {
            let (fx, fy) = h.at();
            let (x, y) = (r.x as f32 + fx * r.w as f32, r.y as f32 + fy * r.h as f32);
            let sq = Rect::new(x as i32 - 4, y as i32 - 4, 8, 8);
            ui.canvas.fill_rect(sq, Color::rgba(0, 0, 0, 200));
            ui.canvas.fill_rect(sq.inset(1, 1, 1, 1), Color::WHITE);
        }
    }

    /// What is drawn over the picture, and the pointer's shape.
    fn overlays(&mut self, ui: &mut Ui, r: Rect, doc: &Document, s: &Settings) {
        ui.canvas.save();
        ui.canvas.clip_to(r);
        let pointer = ui.input.pointer.filter(|_| ui.hovered(r));
        let mut cursor = Cursor::Crosshair;
        if let Some(sel) = doc.selection() {
            let frame = self.screen_rect(sel);
            Self::dashed(ui, frame);
            if s.tool == Tool::Select {
                Self::handles(ui, frame);
                if let Some((x, y)) = pointer {
                    if let Some(h) = Self::handle_at(frame, x, y) {
                        cursor = h.cursor();
                    } else if frame.contains(x, y) {
                        cursor = Cursor::Move;
                    }
                }
            }
        }
        if let Some(shape) = doc.shape() {
            if shape.kind.is_line() {
                for q in [shape.a, shape.b] {
                    let (x, y) = self.to_screen(q);
                    let sq = Rect::new(x as i32 - 4, y as i32 - 4, 8, 8);
                    ui.canvas.fill_rect(sq, Color::rgba(0, 0, 0, 200));
                    ui.canvas.fill_rect(sq.inset(1, 1, 1, 1), Color::WHITE);
                }
            } else {
                let frame = self.screen_rect(Self::shape_frame(shape));
                Self::dashed(ui, frame);
                Self::handles(ui, frame);
                if let Some((x, y)) = pointer {
                    if let Some(h) = Self::handle_at(frame, x, y) {
                        cursor = h.cursor();
                    } else if frame.contains(x, y) {
                        cursor = Cursor::Move;
                    }
                }
            }
        }
        if let Some(t) = doc.text() {
            let (bounds, caret) = ui.ctx.text.with(|ctx| (t.bounds(ctx), t.caret(ctx)));
            Self::dashed(ui, self.screen_rect(bounds).inflate(2));
            let (cx, cy) = self.to_screen((caret.0, caret.1));
            let h = (caret.2 * self.view.zoom).max(8.0) as i32;
            let accent = ui.theme().accent;
            ui.canvas.fill_rect(Rect::new(cx as i32, cy as i32, 2, h), accent);
            if pointer.is_some_and(|(x, y)| self.screen_rect(bounds).inflate(4).contains(x, y)) {
                cursor = Cursor::Text;
            }
        }
        if s.tool == Tool::Text && doc.text().is_none() {
            cursor = Cursor::Text;
        }
        // The outline of the brush's tip where it would paint.
        if let (Some((x, y)), Some(size)) =
            (pointer, s.size().filter(|_| matches!(s.tool, Tool::Brush | Tool::Airbrush | Tool::Eraser | Tool::Pencil)))
        {
            let radius = size * self.view.zoom / 2.0;
            if radius >= 3.0 {
                let mut circle = Path::new();
                circle.circle(x as f32 + 0.5, y as f32 + 0.5, radius);
                ui.canvas.stroke_path(&circle, &StrokeStyle::new(2.2), Color::rgba(0, 0, 0, 150));
                ui.canvas.stroke_path(&circle, &StrokeStyle::new(1.0), Color::rgba(255, 255, 255, 220));
            }
        }
        ui.canvas.restore();
        if pointer.is_some() {
            if matches!(self.drag, Drag::Pan { .. }) {
                cursor = Cursor::Move;
            }
            ui.set_cursor(cursor);
        }
    }

    /// The colours of the shape not yet placed follow the colours chosen and the shape's
    /// settings (`Restyle`).
    pub fn restyle(ui: &mut Ui, doc: &mut Document, s: &Settings) {
        if let Some(&shape) = doc.shape() {
            let (outline, fill) = shape_colors(s, true);
            doc.set_shape(Shape { kind: s.shape, paint: s.shape_paint, width: s.shape_width, outline, fill, ..shape });
        }
        if let Some(t) = doc.text() {
            let mut t = t.clone();
            t.font = font_of(ui, s.font);
            t.size = s.text_size;
            t.color = s.primary;
            t.background = s.text_background.then_some(s.secondary);
            ui.ctx.text.with(|ctx| doc.set_text(t, ctx));
        }
    }
}
