// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Shapes: lines, rectangles, ellipses, polygons, stars, hearts and arrows, drawn between two
//! corners as the pointer drags them, outlined, filled or both, anti-aliased.
//!
//! A shape is geometry ([`Shape::path`]) until it is drawn into the picture: until then its
//! corners can still move and its colours and width change (Paint draws it again each time).

use alloc::vec::Vec;

use vgfx::{Bitmap, Canvas, Color, FillRule, LineCap, LineJoin, Path, Rect, StrokeStyle};
use vraster::Point;

/// The kinds of shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Line,
    Arrow,
    Rectangle,
    RoundedRectangle,
    Ellipse,
    Triangle,
    RightTriangle,
    Diamond,
    Pentagon,
    Hexagon,
    Star,
    Heart,
}

impl Kind {
    pub const ALL: [Kind; 12] = [
        Kind::Line,
        Kind::Arrow,
        Kind::Rectangle,
        Kind::RoundedRectangle,
        Kind::Ellipse,
        Kind::Triangle,
        Kind::RightTriangle,
        Kind::Diamond,
        Kind::Pentagon,
        Kind::Hexagon,
        Kind::Star,
        Kind::Heart,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Kind::Line => "Line",
            Kind::Arrow => "Arrow",
            Kind::Rectangle => "Rectangle",
            Kind::RoundedRectangle => "Rounded rectangle",
            Kind::Ellipse => "Ellipse",
            Kind::Triangle => "Triangle",
            Kind::RightTriangle => "Right triangle",
            Kind::Diamond => "Diamond",
            Kind::Pentagon => "Pentagon",
            Kind::Hexagon => "Hexagon",
            Kind::Star => "Star",
            Kind::Heart => "Heart",
        }
    }

    /// The kind called `name` (its name, whatever the case and spacing; "circle", "square"
    /// and "oval" too).
    pub fn named(name: &str) -> Option<Kind> {
        let n: alloc::string::String =
            name.chars().filter(|c| c.is_alphanumeric()).collect::<alloc::string::String>().to_lowercase();
        match n.as_str() {
            "circle" | "oval" => return Some(Kind::Ellipse),
            "square" | "rect" => return Some(Kind::Rectangle),
            "roundedsquare" | "roundrect" => return Some(Kind::RoundedRectangle),
            "rhombus" => return Some(Kind::Diamond),
            _ => {}
        }
        Kind::ALL.into_iter().find(|k| k.name().replace(' ', "").eq_ignore_ascii_case(&n))
    }

    /// Whether the shape is only a line (it has no inside to fill).
    pub fn is_line(self) -> bool {
        self == Kind::Line
    }
}

/// How a shape is painted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Paint {
    /// Its outline, in the primary colour.
    #[default]
    Outline,
    /// Its inside, in the primary colour.
    Fill,
    /// Its outline in the primary colour, its inside in the secondary.
    Both,
}

/// A shape: its kind, its two corners (a line's ends), and how it is painted.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shape {
    pub kind: Kind,
    pub a: (f32, f32),
    pub b: (f32, f32),
    pub paint: Paint,
    /// The outline's width (pixels).
    pub width: f32,
    pub outline: Color,
    pub fill: Color,
}

/// Where `b` goes, held by Shift as a shape is dragged from `a`: a line at a multiple of 45°,
/// any other shape as wide as it is high.
pub fn constrain(kind: Kind, a: (f32, f32), b: (f32, f32)) -> (f32, f32) {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    if kind.is_line() {
        let angle = libm::atan2f(dy, dx);
        let step = core::f32::consts::FRAC_PI_4;
        let snapped = libm::roundf(angle / step) * step;
        let len = libm::hypotf(dx, dy);
        return (a.0 + len * libm::cosf(snapped), a.1 + len * libm::sinf(snapped));
    }
    let side = dx.abs().max(dy.abs());
    (a.0 + side.copysign(dx), a.1 + side.copysign(dy))
}

/// The points of a regular polygon of `n` sides inscribed in the ellipse of `r`, its first
/// point at the top.
fn regular(r: (f32, f32, f32, f32), n: usize, inner: Option<f32>) -> Vec<Point> {
    let (x, y, w, h) = r;
    let (cx, cy, rx, ry) = (x + w / 2.0, y + h / 2.0, w / 2.0, h / 2.0);
    let count = if inner.is_some() { n * 2 } else { n };
    (0..count)
        .map(|i| {
            let a = -core::f32::consts::FRAC_PI_2 + i as f32 * core::f32::consts::TAU / count as f32;
            let k = if i % 2 == 1 { inner.unwrap_or(1.0) } else { 1.0 };
            Point::new(cx + rx * k * libm::cosf(a), cy + ry * k * libm::sinf(a))
        })
        .collect()
}

impl Shape {
    /// The rectangle between the corners: left, top, width, height.
    fn frame(&self) -> (f32, f32, f32, f32) {
        let (x0, x1) = (self.a.0.min(self.b.0), self.a.0.max(self.b.0));
        let (y0, y1) = (self.a.1.min(self.b.1), self.a.1.max(self.b.1));
        (x0, y0, x1 - x0, y1 - y0)
    }

    /// The shape's outline (a line's centre line).
    pub fn path(&self) -> Path {
        let mut p = Path::new();
        let (x, y, w, h) = self.frame();
        let (cx, cy) = (x + w / 2.0, y + h / 2.0);
        match self.kind {
            Kind::Line => p.line(self.a.0, self.a.1, self.b.0, self.b.1),
            Kind::Rectangle => p.rect(x, y, w, h),
            Kind::RoundedRectangle => {
                let r = (w.min(h) * 0.2).max(0.0);
                p.rounded_rect(x, y, w, h, [r; 4]);
            }
            Kind::Ellipse => p.ellipse(cx, cy, w / 2.0, h / 2.0),
            Kind::Triangle => p.polygon(&[Point::new(cx, y), Point::new(x + w, y + h), Point::new(x, y + h)]),
            Kind::RightTriangle => p.polygon(&[Point::new(x, y), Point::new(x + w, y + h), Point::new(x, y + h)]),
            Kind::Diamond => {
                p.polygon(&[Point::new(cx, y), Point::new(x + w, cy), Point::new(cx, y + h), Point::new(x, cy)])
            }
            Kind::Pentagon => p.polygon(&regular((x, y, w, h), 5, None)),
            Kind::Hexagon => p.polygon(&regular((x, y, w, h), 6, None)),
            Kind::Star => p.polygon(&regular((x, y, w, h), 5, Some(0.4))),
            Kind::Arrow => {
                // A block arrow pointing the way the shape was dragged across.
                let right = self.b.0 >= self.a.0;
                let head = (w * 0.45).min(h * 1.2).max(w.min(h) * 0.2);
                let (shaft_top, shaft_bottom) = (y + h * 0.3, y + h * 0.7);
                let pts = [
                    (0.0, shaft_top),
                    (w - head, shaft_top),
                    (w - head, y),
                    (w, cy),
                    (w - head, y + h),
                    (w - head, shaft_bottom),
                    (0.0, shaft_bottom),
                ];
                let points: Vec<Point> =
                    pts.iter().map(|&(dx, py)| Point::new(if right { x + dx } else { x + w - dx }, py)).collect();
                p.polygon(&points);
            }
            Kind::Heart => {
                let (top, bottom) = (y + h * 0.3, y + h);
                p.move_to(cx, bottom);
                p.cubic_to(x + w * 0.1, y + h * 0.7, x - w * 0.05, y + h * 0.25, x + w * 0.25, y + h * 0.06);
                p.cubic_to(x + w * 0.38, y - h * 0.02, cx, y + h * 0.08, cx, top);
                p.cubic_to(cx, y + h * 0.08, x + w * 0.62, y - h * 0.02, x + w * 0.75, y + h * 0.06);
                p.cubic_to(x + w * 1.05, y + h * 0.25, x + w * 0.9, y + h * 0.7, cx, bottom);
                p.close();
            }
        }
        p
    }

    /// The pixels the shape can paint (its frame and the outline's width around it).
    pub fn bounds(&self) -> Rect {
        let (x, y, w, h) = self.frame();
        let m = self.width.max(1.0) * if self.kind == Kind::Star { 2.0 } else { 1.0 } + 2.0;
        let clamp = |v: f32| v.clamp(-16_777_216.0, 16_777_216.0);
        let (x0, y0) = (libm::floorf(clamp(x - m)) as i32, libm::floorf(clamp(y - m)) as i32);
        let (x1, y1) = (libm::ceilf(clamp(x + w + m)) as i32, libm::ceilf(clamp(y + h + m)) as i32);
        Rect::new(x0, y0, x1 - x0, y1 - y0)
    }

    /// Draws the shape on `canvas`.
    pub fn draw(&self, canvas: &mut Canvas) {
        let path = self.path();
        let (_, _, w, h) = self.frame();
        if self.kind.is_line() {
            let style = StrokeStyle::new(self.width.max(1.0)).with_cap(LineCap::Round);
            canvas.stroke_path(&path, &style, self.outline);
            return;
        }
        if w < 0.5 && h < 0.5 {
            return;
        }
        match self.paint {
            Paint::Fill => canvas.fill_path(&path, self.outline, FillRule::NonZero),
            Paint::Both => canvas.fill_path(&path, self.fill, FillRule::NonZero),
            Paint::Outline => {}
        }
        if self.paint != Paint::Fill {
            let join = match self.kind {
                Kind::Rectangle | Kind::Diamond | Kind::RightTriangle => LineJoin::Miter,
                _ => LineJoin::Round,
            };
            let style = StrokeStyle::new(self.width.max(1.0)).with_join(join).with_miter_limit(4.0);
            canvas.stroke_path(&path, &style, self.outline);
        }
    }

    /// Draws the shape into `image`.
    pub fn draw_into(&self, image: &mut Bitmap) {
        self.draw(&mut Canvas::for_bitmap(image));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brush::filled;

    fn shape(kind: Kind, paint: Paint) -> Shape {
        Shape {
            kind,
            a: (10.0, 10.0),
            b: (50.0, 40.0),
            paint,
            width: 3.0,
            outline: Color::hex(0xFF0000),
            fill: Color::hex(0x0000FF),
        }
    }

    #[test]
    fn every_shape_paints_within_its_bounds() {
        for kind in Kind::ALL {
            for paint in [Paint::Outline, Paint::Fill, Paint::Both] {
                let s = shape(kind, paint);
                let mut b = filled(80, 60, 0);
                s.draw_into(&mut b);
                let painted: Vec<(i32, i32)> =
                    (0..60).flat_map(|y| (0..80).map(move |x| (x, y))).filter(|&(x, y)| b.get(x, y) != 0).collect();
                assert!(painted.len() > 30, "{kind:?} {paint:?}: {} pixels", painted.len());
                let bounds = s.bounds();
                assert!(painted.iter().all(|&(x, y)| bounds.contains(x, y)), "{kind:?} {paint:?} beyond {bounds:?}");
            }
        }
    }

    #[test]
    fn outlines_and_insides() {
        let mut b = filled(80, 60, 0);
        shape(Kind::Rectangle, Paint::Both).draw_into(&mut b);
        assert_eq!(b.get(30, 25), 0xFF00_00FF, "the inside, in the second colour");
        assert_eq!(b.get(10, 25), 0xFFFF_0000, "the outline");
        assert_eq!(b.get(5, 25), 0, "outside");
        let mut b = filled(80, 60, 0);
        shape(Kind::Ellipse, Paint::Outline).draw_into(&mut b);
        assert_eq!(b.get(30, 25), 0, "an outline only");
        let mut b = filled(80, 60, 0);
        shape(Kind::Ellipse, Paint::Fill).draw_into(&mut b);
        assert_eq!(b.get(30, 25), 0xFFFF_0000, "filled with the first colour");
    }

    #[test]
    fn shift_makes_squares_and_straight_lines() {
        assert_eq!(constrain(Kind::Rectangle, (0.0, 0.0), (30.0, -10.0)), (30.0, -30.0));
        assert_eq!(constrain(Kind::Ellipse, (5.0, 5.0), (-5.0, 25.0)), (-15.0, 25.0));
        let (x, y) = constrain(Kind::Line, (0.0, 0.0), (10.0, 1.0));
        assert!((x - 10.05).abs() < 0.01 && y.abs() < 1e-4, "horizontal: {x} {y}");
        let (x, y) = constrain(Kind::Line, (0.0, 0.0), (10.0, 8.0));
        assert!((x - y).abs() < 1e-4, "diagonal: {x} {y}");
    }

    #[test]
    fn shapes_by_name() {
        assert_eq!(Kind::named("rounded rectangle"), Some(Kind::RoundedRectangle));
        assert_eq!(Kind::named("Circle"), Some(Kind::Ellipse));
        assert_eq!(Kind::named("right-triangle"), Some(Kind::RightTriangle));
        assert_eq!(Kind::named("STAR"), Some(Kind::Star));
        assert_eq!(Kind::named("blob"), None);
    }

    #[test]
    fn arrows_point_the_way_they_were_dragged() {
        let mut right = shape(Kind::Arrow, Paint::Fill);
        let mut b = filled(80, 60, 0);
        right.draw_into(&mut b);
        // The tip is at the far end of the drag, mid-height.
        assert_ne!(b.get(48, 25), 0);
        assert_eq!(b.get(12, 12), 0, "the shaft is narrower than the head");
        (right.a, right.b) = (right.b, right.a);
        let mut b = filled(80, 60, 0);
        right.draw_into(&mut b);
        assert_ne!(b.get(12, 25), 0);
    }
}
