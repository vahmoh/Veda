// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Filling an area: the pixels of one colour around a point, or every pixel of that colour,
//! with a tolerance for colours close to it.
//!
//! The area is found by a scanline flood fill (runs of a row at a time, with a stack of its
//! own, however large the area), and painted over the original pixels: a fill replaces the
//! colour there, so a fill with transparency makes a hole. With soft edges, the pixels just
//! outside the area that are nearly its colour (an anti-aliased outline's inner edge) take the
//! fill in part, so that a fill inside a smooth shape leaves no rough fringe.

use alloc::vec;
use alloc::vec::Vec;

use gfx::color::{keep_scalar, lerp_px};
use gfx::{Bitmap, Color, Rect};

use crate::color::{distance, premultiplied};
use crate::history::History;

/// Which pixels a fill reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Reach {
    /// Those of the colour connected to the point (through their sides).
    #[default]
    Contiguous,
    /// Every pixel of the colour.
    Global,
}

/// How far beyond the tolerance an edge pixel is still filled in part.
const EDGE: u32 = 96;

/// The area a fill covers: how much of each pixel (0 to 255) within its bounds.
pub struct Area {
    pub bounds: Rect,
    /// `bounds.w × bounds.h` coverages, row by row.
    cover: Vec<u8>,
}

impl Area {
    /// How much of the pixel at (`x`, `y`) the fill covers.
    pub fn at(&self, x: i32, y: i32) -> u8 {
        if !self.bounds.contains(x, y) {
            return 0;
        }
        self.cover[((y - self.bounds.y) * self.bounds.w + (x - self.bounds.x)) as usize]
    }

    /// How many pixels it covers in full.
    pub fn count(&self) -> usize {
        self.cover.iter().filter(|&&c| c == 255).count()
    }
}

/// The area a fill at (`x`, `y`) covers: pixels within `tolerance` (0 to 255, of any channel)
/// of that pixel's colour, connected to it or anywhere (`reach`), with their nearly matching
/// neighbours in part if `soft`. `None` if the point is off the picture.
pub fn area(image: &Bitmap, x: i32, y: i32, tolerance: u8, reach: Reach, soft: bool) -> Option<Area> {
    if !image.rect().contains(x, y) {
        return None;
    }
    let (w, h) = (image.width, image.height);
    let target = image.get(x, y);
    let tol = tolerance as u32;
    let matches = |px: u32| distance(px, target) <= tol;
    // One bit a pixel: filled.
    let mut filled = vec![0u64; (w as usize * h as usize).div_ceil(64)];
    let is = |filled: &[u64], i: usize| filled[i >> 6] >> (i & 63) & 1 == 1;
    let set = |filled: &mut [u64], i: usize| filled[i >> 6] |= 1 << (i & 63);
    let (mut x0, mut y0, mut x1, mut y1) = (x, y, x, y);
    match reach {
        Reach::Global => {
            for (i, &px) in image.pixels.iter().enumerate() {
                if matches(px) {
                    set(&mut filled, i);
                    let (px, py) = ((i % w as usize) as i32, (i / w as usize) as i32);
                    (x0, y0, x1, y1) = (x0.min(px), y0.min(py), x1.max(px), y1.max(py));
                }
                keep_scalar();
            }
        }
        Reach::Contiguous => {
            let mut stack = vec![(x, y)];
            let at = |x: i32, y: i32| (y * w + x) as usize;
            while let Some((sx, sy)) = stack.pop() {
                if is(&filled, at(sx, sy)) || !matches(image.pixels[at(sx, sy)]) {
                    continue;
                }
                // The run of the colour along the row.
                let (mut l, mut r) = (sx, sx);
                while l > 0 && !is(&filled, at(l - 1, sy)) && matches(image.pixels[at(l - 1, sy)]) {
                    l -= 1;
                }
                while r + 1 < w && !is(&filled, at(r + 1, sy)) && matches(image.pixels[at(r + 1, sy)]) {
                    r += 1;
                }
                for i in l..=r {
                    set(&mut filled, at(i, sy));
                }
                (x0, y0, x1, y1) = (x0.min(l), y0.min(sy), x1.max(r), y1.max(sy));
                // A seed for each run of the colour above and below it.
                for ny in [sy - 1, sy + 1] {
                    if ny < 0 || ny >= h {
                        continue;
                    }
                    let mut i = l;
                    while i <= r {
                        if !is(&filled, at(i, ny)) && matches(image.pixels[at(i, ny)]) {
                            stack.push((i, ny));
                            while i <= r && matches(image.pixels[at(i, ny)]) {
                                i += 1;
                            }
                        } else {
                            i += 1;
                        }
                    }
                }
            }
        }
    }
    // The bounds, a pixel wider for soft edges.
    let grow = soft as i32;
    let bounds =
        Rect::new(x0 - grow, y0 - grow, x1 - x0 + 1 + 2 * grow, y1 - y0 + 1 + 2 * grow).intersect(&image.rect());
    let mut cover = vec![0u8; (bounds.w * bounds.h) as usize];
    for by in 0..bounds.h {
        for bx in 0..bounds.w {
            let (px, py) = (bounds.x + bx, bounds.y + by);
            let i = (py * w + px) as usize;
            let c = if is(&filled, i) {
                255
            } else if soft {
                // Beside the area and nearly its colour: in part, the nearer the more.
                let beside = [(px - 1, py), (px + 1, py), (px, py - 1), (px, py + 1)]
                    .iter()
                    .any(|&(nx, ny)| nx >= 0 && ny >= 0 && nx < w && ny < h && is(&filled, (ny * w + nx) as usize));
                let d = distance(image.pixels[i], target);
                if beside && d <= tol + EDGE {
                    (255 - (d.saturating_sub(tol) * 255 / (EDGE + 1)).min(255)) as u8 / 2
                } else {
                    0
                }
            } else {
                0
            };
            cover[(by * bounds.w + bx) as usize] = c;
        }
    }
    Some(Area { bounds, cover })
}

/// Paints `area` with `color` at `opacity` (0 to 255) over the original pixels: the edit must
/// be under way (its tiles are kept here). Returns where the picture changed.
pub fn paint(image: &mut Bitmap, history: &mut History, area: &Area, color: Color, opacity: u8) -> Rect {
    history.touch(image, area.bounds);
    let px = premultiplied(color, 255);
    let r = area.bounds;
    let mut row = Vec::with_capacity(r.w as usize);
    for y in r.y..r.bottom() {
        history.original_row(image, y, r.x, r.right(), &mut row);
        let width = image.width;
        let out = &mut image.pixels[(y * width + r.x) as usize..(y * width + r.right()) as usize];
        for (i, (dst, &orig)) in out.iter_mut().zip(&row).enumerate() {
            let c = area.cover[((y - r.y) * r.w) as usize + i] as u32;
            if c > 0 {
                // A fill replaces the colour: in 256ths, how much of the new one.
                let t = (c * opacity as u32 + 127) / 255;
                *dst = lerp_px(orig, px, (t * 256 + 127) / 255);
            }
            keep_scalar();
        }
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brush::filled;

    /// A white picture with a black square outline from (10, 10) to (29, 29).
    fn boxed() -> Bitmap {
        let mut b = filled(40, 40, 0xFFFF_FFFF);
        for i in 10..30 {
            for (x, y) in [(i, 10), (i, 29), (10, i), (29, i)] {
                b.pixels[(y * 40 + x) as usize] = 0xFF00_0000;
            }
        }
        b
    }

    #[test]
    fn a_fill_stays_inside_its_outline() {
        let mut b = boxed();
        let a = area(&b, 20, 20, 0, Reach::Contiguous, false).unwrap();
        assert_eq!(a.count(), 18 * 18);
        assert_eq!(a.bounds, Rect::new(11, 11, 18, 18));
        let mut h = History::new(1 << 20);
        h.begin("Fill", &b);
        paint(&mut b, &mut h, &a, Color::hex(0xFF0000), 255);
        h.commit();
        assert_eq!(b.get(20, 20), 0xFFFF_0000);
        assert_eq!(b.get(11, 28), 0xFFFF_0000);
        assert_eq!(b.get(10, 20), 0xFF00_0000, "the outline");
        assert_eq!(b.get(5, 5), 0xFFFF_FFFF, "outside");
        // Outside, the fill goes round the box.
        let a = area(&b, 0, 0, 0, Reach::Contiguous, false).unwrap();
        assert_eq!(a.count(), 40 * 40 - 20 * 20);
        assert!(area(&b, 40, 0, 0, Reach::Contiguous, false).is_none());
    }

    #[test]
    fn tolerance_and_reach() {
        let mut b = boxed();
        b.pixels[0] = 0xFFFA_FAFA;
        assert_eq!(area(&b, 1, 1, 0, Reach::Contiguous, false).unwrap().count(), 40 * 40 - 400 - 1);
        assert_eq!(area(&b, 1, 1, 10, Reach::Contiguous, false).unwrap().count(), 40 * 40 - 400);
        // Every white pixel, inside the box too.
        assert_eq!(area(&b, 1, 1, 10, Reach::Global, false).unwrap().count(), 40 * 40 - 76);
    }

    #[test]
    fn soft_edges_fill_nearly_matching_neighbours_in_part() {
        let mut b = boxed();
        // An anti-aliased edge: grey inside the outline.
        for i in 11..29 {
            b.pixels[(11 * 40 + i) as usize] = 0xFFE0_E0E0;
        }
        let a = area(&b, 20, 20, 0, Reach::Contiguous, true).unwrap();
        let edge = a.at(20, 11);
        assert!(edge > 0 && edge < 255, "{edge}");
        assert_eq!(a.at(20, 10), 0, "the outline itself is far from white");
        assert_eq!(a.at(20, 20), 255);
    }

    #[test]
    fn a_translucent_fill_mixes_and_a_transparent_one_clears() {
        let mut b = boxed();
        let mut h = History::new(1 << 20);
        let a = area(&b, 20, 20, 0, Reach::Contiguous, false).unwrap();
        h.begin("Fill", &b);
        paint(&mut b, &mut h, &a, Color::BLACK, 128);
        assert!((b.get(20, 20) & 0xFF).abs_diff(127) <= 1);
        h.abort(&mut b);
        h.begin("Fill", &b);
        paint(&mut b, &mut h, &a, Color::TRANSPARENT, 255);
        assert_eq!(b.get(20, 20), 0);
    }
}
