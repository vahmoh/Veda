// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Strokes: the pencil, the brush, the airbrush and the eraser.
//!
//! A stroke draws its coverage into a mask over the picture (tiles of it, allocated where the
//! stroke reaches), and the picture is the original pixels with the colour composited over
//! them at that coverage, times the opacity. Each pixel keeps the most coverage any part of
//! the stroke gave it, so a semi-transparent stroke that crosses itself does not darken where
//! it does, as a real pen's ink would not.
//!
//! * The **brush** and the **eraser** draw round tips along the pointer's path, smoothed into
//!   curves through the midpoints of the positions it passed (so that a quick stroke is a
//!   curve, not a polygon), anti-aliased, their edges as soft as their hardness says.
//! * The **pencil** sets whole pixels, without anti-aliasing: square tips along straight lines
//!   between the positions, as pixel art wants.
//! * The **airbrush** sprays fine dots around the pointer for as long as it is held, its paint
//!   building up where it stays.
//!
//! The eraser takes paint away, to transparency.

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;

use vgfx::color::{keep_scalar, over, scale};
use vgfx::{Bitmap, Color, Rect};

use crate::color::premultiplied;
use crate::history::{History, TILE};

/// The kind of tip a stroke is drawn with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tip {
    Pencil,
    Brush,
    Airbrush,
    Eraser,
}

/// How a stroke is drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Style {
    pub tip: Tip,
    /// The tip's width (pixels, from 1).
    pub size: f32,
    /// How sharp its edge is: 1 is a hard edge (anti-aliased over a pixel), 0 fades from the
    /// middle out.
    pub hardness: f32,
    /// 0 to 255.
    pub opacity: u8,
    pub color: Color,
}

/// The largest tip (pixels).
pub const MAX_SIZE: f32 = 500.0;

/// Coordinates beyond this (pixels) are as far as anything: rectangles of them still add up.
const FAR: f32 = 16_777_216.0;

/// A coordinate's whole pixel (rounded down), within [`FAR`].
fn pixel(v: f32) -> i32 {
    libm::floorf(v.clamp(-FAR, FAR)) as i32
}

/// A coordinate rounded up, within [`FAR`].
fn pixel_up(v: f32) -> i32 {
    libm::ceilf(v.clamp(-FAR, FAR)) as i32
}

/// The part of the segment from `a` to `b` inside `r` (Liang and Barsky's clipping), if any.
fn clip_segment(a: (f32, f32), b: (f32, f32), r: (f32, f32, f32, f32)) -> Option<((f32, f32), (f32, f32))> {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let (mut t0, mut t1) = (0.0f32, 1.0f32);
    for (p, q) in [(-dx, a.0 - r.0), (dx, r.2 - a.0), (-dy, a.1 - r.1), (dy, r.3 - a.1)] {
        if p == 0.0 {
            if q < 0.0 {
                return None;
            }
        } else {
            let t = q / p;
            if p < 0.0 {
                t0 = t0.max(t);
            } else {
                t1 = t1.min(t);
            }
        }
    }
    (t0 <= t1).then_some(((a.0 + t0 * dx, a.1 + t0 * dy), (a.0 + t1 * dx, a.1 + t1 * dy)))
}

const TILE_AREA: usize = (TILE * TILE) as usize;

/// Coverage over the picture, in tiles made as the stroke reaches them.
struct Mask {
    cols: i32,
    width: i32,
    height: i32,
    tiles: Vec<Option<Box<[u8; TILE_AREA]>>>,
}

impl Mask {
    fn new(width: i32, height: i32) -> Mask {
        let cols = (width + TILE - 1) / TILE;
        let rows = (height + TILE - 1) / TILE;
        Mask { cols, width, height, tiles: (0..(cols * rows).max(0)).map(|_| None).collect() }
    }

    #[inline]
    fn get(&self, x: i32, y: i32) -> u8 {
        match &self.tiles[((y / TILE) * self.cols + x / TILE) as usize] {
            Some(t) => t[((y % TILE) * TILE + x % TILE) as usize],
            None => 0,
        }
    }

    /// The tile's coverage at (`x`, `y`), the tile made if need be.
    #[inline]
    fn at(&mut self, x: i32, y: i32) -> &mut u8 {
        let tile =
            self.tiles[((y / TILE) * self.cols + x / TILE) as usize].get_or_insert_with(|| Box::new([0; TILE_AREA]));
        &mut tile[((y % TILE) * TILE + x % TILE) as usize]
    }

    /// Whether the tile holding (`x`, `y`) has coverage.
    fn has_tile(&self, x: i32, y: i32) -> bool {
        self.tiles[((y / TILE) * self.cols + x / TILE) as usize].is_some()
    }

    fn bounds(&self) -> Rect {
        Rect::new(0, 0, self.width, self.height)
    }
}

/// Coverage (0 to 255) by squared distance from a tip's centre, sampled finely enough for any
/// size of tip.
struct Profile {
    /// Samples per square pixel of squared distance.
    scale: f32,
    table: Vec<u8>,
}

impl Profile {
    fn new(radius: f32, hardness: f32) -> Profile {
        let reach = radius + 1.0;
        let scale = (4096.0 / (reach * reach)).clamp(4.0, 64.0);
        let len = (reach * reach * scale) as usize + 2;
        // Full coverage out to the inner radius, then a smooth fall to nothing at the edge;
        // a hard tip falls over a pixel, which is its anti-aliasing.
        let inner = radius * hardness.clamp(0.0, 1.0);
        let fall = (radius - inner).max(1.0);
        let table = (0..len)
            .map(|i| {
                let d = libm::sqrtf(i as f32 / scale);
                let t = ((radius + 0.5 - d) / fall).clamp(0.0, 1.0);
                let t = if fall > 1.0 { t * t * (3.0 - 2.0 * t) } else { t };
                libm::roundf(t * 255.0) as u8
            })
            .collect();
        Profile { scale, table }
    }

    #[inline]
    fn at(&self, d2: f32) -> u8 {
        self.table.get((d2 * self.scale) as usize).copied().unwrap_or(0)
    }
}

/// A stroke being drawn.
pub struct Stroke {
    style: Style,
    mask: Mask,
    profile: Profile,
    /// The last position received, and the end of the path drawn so far (the smoothing draws
    /// up to the midpoint between the last two positions).
    last: Option<(f32, f32)>,
    drawn_to: (f32, f32),
    /// The airbrush's random numbers.
    seed: u64,
    /// Where the stroke has covered.
    bounds: Rect,
}

impl Stroke {
    /// A stroke of `style` on a picture of `width` by `height`.
    pub fn new(style: Style, width: i32, height: i32) -> Stroke {
        let size = style.size.clamp(1.0, MAX_SIZE);
        let style = Style { size, ..style };
        let profile = match style.tip {
            // The airbrush's dots are small and soft.
            Tip::Airbrush => Profile::new(Self::dot_radius(size), 0.0),
            _ => Profile::new(size / 2.0, style.hardness),
        };
        Stroke {
            style,
            mask: Mask::new(width, height),
            profile,
            last: None,
            drawn_to: (0.0, 0.0),
            seed: 0x9E37_79B9_7F4A_7C15,
            bounds: Rect::default(),
        }
    }

    fn dot_radius(size: f32) -> f32 {
        (size * 0.035).clamp(0.6, 3.0)
    }

    pub fn style(&self) -> &Style {
        &self.style
    }

    /// Where the stroke has covered so far.
    pub fn bounds(&self) -> Rect {
        self.bounds
    }

    /// Where the stroke was last.
    pub fn last(&self) -> Option<(f32, f32)> {
        self.last
    }

    /// The stroke reaches (`x`, `y`) (picture coordinates); returns where its coverage changed.
    pub fn to(&mut self, x: f32, y: f32) -> Rect {
        if !x.is_finite() || !y.is_finite() {
            return Rect::default();
        }
        let (x, y) = (x.clamp(-FAR, FAR), y.clamp(-FAR, FAR));
        let Some(last) = self.last else {
            self.last = Some((x, y));
            self.drawn_to = (x, y);
            return match self.style.tip {
                Tip::Pencil => self.pencil_line(x, y, x, y),
                Tip::Airbrush => self.spray_at(x, y, 6),
                _ => self.capsule((x, y), (x, y)),
            };
        };
        if last == (x, y) {
            return Rect::default();
        }
        self.last = Some((x, y));
        match self.style.tip {
            Tip::Pencil => self.pencil_line(last.0, last.1, x, y),
            Tip::Airbrush => {
                // Paint along the way, as much as the distance covered.
                let d = libm::hypotf(x - last.0, y - last.1);
                let steps = libm::ceilf(d / (self.style.size * 0.25).max(1.0)).clamp(1.0, 64.0) as i32;
                let mut changed = Rect::default();
                for i in 1..=steps {
                    let t = i as f32 / steps as f32;
                    changed = union(changed, self.spray_at(last.0 + (x - last.0) * t, last.1 + (y - last.1) * t, 2));
                }
                changed
            }
            _ => {
                // A curve from where the path was drawn to, through the last position, to the
                // midpoint after it.
                let mid = ((last.0 + x) / 2.0, (last.1 + y) / 2.0);
                let from = self.drawn_to;
                self.drawn_to = mid;
                self.curve(from, last, mid)
            }
        }
    }

    /// The stroke goes straight on to (`x`, `y`), unsmoothed: for lines given point by point
    /// (Shift and a click, the voice agent's). Returns where its coverage changed.
    pub fn line_to(&mut self, x: f32, y: f32) -> Rect {
        if !x.is_finite() || !y.is_finite() {
            return Rect::default();
        }
        let (x, y) = (x.clamp(-FAR, FAR), y.clamp(-FAR, FAR));
        match (self.style.tip, self.last) {
            (Tip::Brush | Tip::Eraser, Some(_)) => {
                let from = self.drawn_to;
                self.last = Some((x, y));
                self.drawn_to = (x, y);
                self.capsule(from, (x, y))
            }
            _ => self.to(x, y),
        }
    }

    /// Ends the stroke: the path the smoothing held back is drawn to its last position.
    pub fn finish(&mut self) -> Rect {
        match (self.style.tip, self.last) {
            (Tip::Brush | Tip::Eraser, Some(last)) if last != self.drawn_to => {
                let from = self.drawn_to;
                self.drawn_to = last;
                self.capsule(from, last)
            }
            _ => Rect::default(),
        }
    }

    /// The airbrush held still for `ns` nanoseconds: more paint where it is.
    pub fn spray(&mut self, ns: u64) -> Rect {
        let Some((x, y)) = self.last.filter(|_| self.style.tip == Tip::Airbrush) else { return Rect::default() };
        // As many dots a second as a disc of the tip holds pixels, a little each.
        let area = core::f32::consts::PI * (self.style.size / 2.0) * (self.style.size / 2.0);
        let dots = ((area * 0.9 + 400.0) * ns as f32 / 1e9) as i32;
        self.spray_at(x, y, dots.clamp(1, 4000))
    }

    /// Draws a quadratic curve as segments short enough to follow it.
    fn curve(&mut self, a: (f32, f32), c: (f32, f32), b: (f32, f32)) -> Rect {
        let len = libm::hypotf(c.0 - a.0, c.1 - a.1) + libm::hypotf(b.0 - c.0, b.1 - c.1);
        let step = (self.style.size * 0.4).clamp(1.5, 24.0);
        let n = libm::ceilf(len / step).clamp(1.0, 256.0) as i32;
        let mut prev = a;
        let mut changed = Rect::default();
        for i in 1..=n {
            let t = i as f32 / n as f32;
            let u = 1.0 - t;
            let p = (u * u * a.0 + 2.0 * u * t * c.0 + t * t * b.0, u * u * a.1 + 2.0 * u * t * c.1 + t * t * b.1);
            changed = union(changed, self.capsule(prev, p));
            prev = p;
        }
        changed
    }

    /// Covers the round-ended segment from `a` to `b`, each pixel by its distance from it.
    fn capsule(&mut self, a: (f32, f32), b: (f32, f32)) -> Rect {
        let r = self.style.size / 2.0;
        let reach = r + 1.0;
        let (left, top) = (pixel(a.0.min(b.0) - reach), pixel(a.1.min(b.1) - reach));
        let (right, bottom) = (pixel_up(a.0.max(b.0) + reach), pixel_up(a.1.max(b.1) + reach));
        let area = Rect::new(left, top, right - left, bottom - top).intersect(&self.mask.bounds());
        if area.is_empty() {
            return Rect::default();
        }
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len2 = dx * dx + dy * dy;
        let inv = if len2 > 0.0 { 1.0 / len2 } else { 0.0 };
        for y in area.y..area.bottom() {
            let cy = y as f32 + 0.5 - a.1;
            for x in area.x..area.right() {
                let cx = x as f32 + 0.5 - a.0;
                let t = ((cx * dx + cy * dy) * inv).clamp(0.0, 1.0);
                let (ex, ey) = (cx - t * dx, cy - t * dy);
                let cov = self.profile.at(ex * ex + ey * ey);
                if cov > 0 {
                    let m = self.mask.at(x, y);
                    *m = (*m).max(cov);
                }
                keep_scalar();
            }
        }
        self.bounds = union(self.bounds, area);
        area
    }

    /// Sets the whole pixels of a line from (`x0`, `y0`) to (`x1`, `y1`), a square of the
    /// tip's size at each.
    fn pencil_line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32) -> Rect {
        let n = libm::roundf(self.style.size).max(1.0) as i32;
        // Only the part of the line that can reach the picture is walked.
        let margin = n as f32 + 1.0;
        let bounds = (-margin, -margin, self.mask.width as f32 + margin, self.mask.height as f32 + margin);
        let Some(((x0, y0), (x1, y1))) = clip_segment((x0, y0), (x1, y1), bounds) else { return Rect::default() };
        let (mut x, mut y) = (pixel(x0), pixel(y0));
        let (tx, ty) = (pixel(x1), pixel(y1));
        // Bresenham's line.
        let (dx, dy) = ((tx - x).abs(), -(ty - y).abs());
        let (sx, sy) = (if x < tx { 1 } else { -1 }, if y < ty { 1 } else { -1 });
        let mut err = dx + dy;
        let mut changed = Rect::default();
        let bounds = self.mask.bounds();
        loop {
            let square = Rect::new(x - (n - 1) / 2, y - (n - 1) / 2, n, n).intersect(&bounds);
            for py in square.y..square.bottom() {
                for px in square.x..square.right() {
                    *self.mask.at(px, py) = 255;
                }
            }
            changed = union(changed, square);
            if x == tx && y == ty {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                x += sx;
            }
            if e2 <= dx {
                err += dx;
                y += sy;
            }
        }
        self.bounds = union(self.bounds, changed);
        changed
    }

    /// The next of the airbrush's random numbers, from 0 to 1.
    fn random(&mut self) -> f32 {
        // xorshift64*
        self.seed ^= self.seed >> 12;
        self.seed ^= self.seed << 25;
        self.seed ^= self.seed >> 27;
        (self.seed.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Sprays `dots` dots at random in the tip's disc around (`x`, `y`), each adding a little
    /// paint.
    fn spray_at(&mut self, x: f32, y: f32, dots: i32) -> Rect {
        let radius = self.style.size / 2.0;
        let dot = Self::dot_radius(self.style.size);
        let mut changed = Rect::default();
        let bounds = self.mask.bounds();
        for _ in 0..dots {
            // Uniform over the disc's area.
            let r = radius * libm::sqrtf(self.random());
            let a = self.random() * core::f32::consts::TAU;
            let (cx, cy) = (x + r * libm::cosf(a), y + r * libm::sinf(a));
            let reach = dot + 1.0;
            let side = (2.0 * reach) as i32 + 2;
            let area = Rect::new(pixel(cx - reach), pixel(cy - reach), side, side).intersect(&bounds);
            for py in area.y..area.bottom() {
                for px in area.x..area.right() {
                    let (ex, ey) = (px as f32 + 0.5 - cx, py as f32 + 0.5 - cy);
                    let cov = self.profile.at(ex * ex + ey * ey) as u32;
                    if cov > 0 {
                        let m = self.mask.at(px, py);
                        // A dot adds a fifth of its coverage: paint builds up.
                        *m = (*m as u32 + cov / 5 + 1).min(255) as u8;
                    }
                }
            }
            changed = union(changed, area);
        }
        self.bounds = union(self.bounds, changed);
        changed
    }

    /// The picture in `r`: its original pixels (as `history` has them) with the stroke over
    /// them. The edit must have kept `r`'s tiles first.
    pub fn composite(&self, image: &mut Bitmap, history: &History, r: Rect) {
        let r = r.intersect(&image.rect());
        if r.is_empty() {
            return;
        }
        let color = premultiplied(self.style.color, 255);
        let opacity = self.style.opacity as u32;
        let erase = self.style.tip == Tip::Eraser;
        let mut row = Vec::with_capacity(r.w as usize);
        let width = image.width;
        // A tile's columns at a time: a tile without coverage is left as it is.
        let mut x0 = r.x;
        while x0 < r.right() {
            let x1 = ((x0 / TILE + 1) * TILE).min(r.right());
            let mut y0 = r.y;
            while y0 < r.bottom() {
                let y1 = ((y0 / TILE + 1) * TILE).min(r.bottom());
                if self.mask.has_tile(x0, y0) {
                    for y in y0..y1 {
                        history.original_row(image, y, x0, x1, &mut row);
                        let out = &mut image.pixels[(y * width + x0) as usize..(y * width + x1) as usize];
                        for (i, (dst, &orig)) in out.iter_mut().zip(&row).enumerate() {
                            let m = self.mask.get(x0 + i as i32, y) as u32;
                            let a = (m * opacity + 127) / 255;
                            *dst = match (a, erase) {
                                (0, _) => orig,
                                (_, true) => scale(orig, 255 - a),
                                (_, false) => over(scale(color, a), orig),
                            };
                            keep_scalar();
                        }
                    }
                }
                y0 = y1;
            }
            x0 = x1;
        }
    }
}

/// The smallest rectangle holding both (an empty one holds nothing).
pub fn union(a: Rect, b: Rect) -> Rect {
    match (a.is_empty(), b.is_empty()) {
        (true, _) => b,
        (_, true) => a,
        _ => a.union(&b),
    }
}

/// A straight line of `style` from `a` to `b`, as Shift and a click draw one from where the
/// last stroke ended: its stroke, already ended.
pub fn line(style: Style, width: i32, height: i32, a: (f32, f32), b: (f32, f32)) -> Stroke {
    let mut s = Stroke::new(style, width, height);
    s.to(a.0, a.1);
    s.line_to(b.0, b.1);
    s
}

/// A picture of `w` by `h` all of the premultiplied pixel `px`.
pub fn filled(w: i32, h: i32, px: u32) -> Bitmap {
    Bitmap { width: w, height: h, pixels: vec![px; (w.max(0) * h.max(0)) as usize] }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHITE: u32 = 0xFFFF_FFFF;

    fn draw(style: Style, points: &[(f32, f32)], picture: &mut Bitmap, history: &mut History) -> Rect {
        history.begin("Stroke", picture);
        let mut s = Stroke::new(style, picture.width, picture.height);
        for &(x, y) in points {
            let r = s.to(x, y);
            history.touch(picture, r);
            s.composite(picture, history, r);
        }
        let r = s.finish();
        history.touch(picture, r);
        s.composite(picture, history, r);
        history.commit();
        s.bounds()
    }

    fn brush(size: f32, opacity: u8) -> Style {
        Style { tip: Tip::Brush, size, hardness: 1.0, opacity, color: Color::hex(0xFF0000) }
    }

    #[test]
    fn a_brush_stroke_covers_its_path() {
        let mut p = filled(100, 60, WHITE);
        let mut h = History::new(1 << 24);
        // Off the pixel grid, so that the edge crosses pixels.
        let bounds = draw(brush(10.0, 255), &[(10.0, 30.3), (50.0, 30.3), (90.0, 30.3)], &mut p, &mut h);
        // Along the path: the colour; well off it: untouched; at its edge: blended.
        assert_eq!(p.get(50, 30), 0xFFFF_0000);
        assert_eq!(p.get(12, 30), 0xFFFF_0000);
        assert_eq!(p.get(89, 30), 0xFFFF_0000);
        assert_eq!(p.get(50, 10), WHITE);
        assert_eq!(p.get(50, 37), WHITE);
        let edge = p.get(50, 35);
        assert!(edge != WHITE && edge != 0xFFFF_0000, "{edge:08x}");
        assert!(bounds.contains(10, 30) && bounds.contains(90, 30) && !bounds.contains(50, 50));
        h.undo(&mut p);
        assert!(p.pixels.iter().all(|&px| px == WHITE));
    }

    #[test]
    fn a_translucent_stroke_crossing_itself_does_not_darken() {
        let mut p = filled(80, 80, WHITE);
        let mut h = History::new(1 << 24);
        let path = [(10.0, 40.0), (70.0, 40.0), (70.0, 41.0), (10.0, 41.0), (40.0, 10.0), (40.0, 70.0)];
        draw(brush(6.0, 128), &path, &mut p, &mut h);
        let crossing = p.get(40, 40);
        let once = p.get(20, 40);
        assert_eq!(crossing, once, "{crossing:08x} {once:08x}");
        assert_eq!((once >> 16) & 0xFF, 255);
        assert!((once & 0xFF).abs_diff(127) <= 1, "half red over white: {once:08x}");
    }

    #[test]
    fn the_eraser_takes_paint_away() {
        let mut p = filled(40, 40, WHITE);
        let mut h = History::new(1 << 24);
        let eraser = Style { tip: Tip::Eraser, ..brush(8.0, 255) };
        draw(eraser, &[(5.0, 20.0), (35.0, 20.0)], &mut p, &mut h);
        assert_eq!(p.get(20, 20), 0, "transparent");
        assert_eq!(p.get(20, 5), WHITE);
        let half = Style { opacity: 128, ..eraser };
        let mut p = filled(40, 40, WHITE);
        draw(half, &[(5.0, 20.0), (35.0, 20.0)], &mut p, &mut h);
        assert!((p.get(20, 20) >> 24).abs_diff(127) <= 1);
    }

    #[test]
    fn the_pencil_sets_whole_pixels() {
        let mut p = filled(20, 20, WHITE);
        let mut h = History::new(1 << 24);
        let pencil = Style { tip: Tip::Pencil, size: 1.0, hardness: 1.0, opacity: 255, color: Color::BLACK };
        draw(pencil, &[(2.5, 2.5), (12.5, 7.5)], &mut p, &mut h);
        let black = p.pixels.iter().filter(|&&px| px == 0xFF00_0000).count();
        let other = p.pixels.iter().filter(|&&px| px != 0xFF00_0000 && px != WHITE).count();
        assert_eq!((black, other), (11, 0), "one pixel a column, nothing in between");
        assert_eq!(p.get(2, 2), 0xFF00_0000);
        assert_eq!(p.get(12, 7), 0xFF00_0000);
    }

    #[test]
    fn the_airbrush_builds_up_paint() {
        let mut p = filled(60, 60, WHITE);
        let mut h = History::new(1 << 24);
        h.begin("Spray", &p);
        let style = Style { tip: Tip::Airbrush, size: 20.0, hardness: 0.0, opacity: 255, color: Color::BLACK };
        let mut s = Stroke::new(style, 60, 60);
        let r = s.to(30.0, 30.0);
        h.touch(&p, r);
        s.composite(&mut p, &h, r);
        let darkness = |p: &Bitmap| p.pixels.iter().map(|px| 255 - (px & 0xFF)).sum::<u32>();
        let first = darkness(&p);
        let r = s.spray(500_000_000);
        h.touch(&p, r);
        s.composite(&mut p, &h, r);
        assert!(darkness(&p) > first * 3, "{} after {first}", darkness(&p));
        // Nothing beyond the tip.
        assert_eq!(p.get(30, 5), WHITE);
        assert_eq!(p.get(5, 30), WHITE);
    }

    #[test]
    fn soft_tips_fade_to_their_edge() {
        let mut p = filled(60, 60, WHITE);
        let mut h = History::new(1 << 24);
        let soft = Style { hardness: 0.0, ..brush(40.0, 255) };
        draw(soft, &[(30.0, 30.0)], &mut p, &mut h);
        let red = |x: i32| 255 - (p.get(x, 30) & 0xFF);
        assert_eq!(red(30), 255, "the middle");
        assert!(red(30) > red(40) && red(40) > red(46) && red(46) > red(49), "{} {} {}", red(40), red(46), red(49));
        assert_eq!(red(51), 0, "beyond the edge");
    }

    #[test]
    fn strokes_stay_on_the_picture() {
        let mut p = filled(30, 20, WHITE);
        let mut h = History::new(1 << 24);
        for tip in [Tip::Pencil, Tip::Brush, Tip::Airbrush, Tip::Eraser] {
            let style = Style { tip, size: 25.0, hardness: 0.5, opacity: 255, color: Color::BLACK };
            draw(style, &[(-50.0, -50.0), (15.0, 10.0), (500.0, 300.0), (-9e9, 9e9)], &mut p, &mut h);
        }
        let s = line(brush(4.0, 255), 30, 20, (0.0, 0.0), (29.0, 19.0));
        assert!(s.bounds().contains(15, 10));
        assert_eq!(clip_segment((-10.0, 5.0), (30.0, 5.0), (0.0, 0.0, 20.0, 10.0)), Some(((0.0, 5.0), (20.0, 5.0))));
        assert_eq!(clip_segment((-10.0, 50.0), (30.0, 50.0), (0.0, 0.0, 20.0, 10.0)), None);
    }
}
