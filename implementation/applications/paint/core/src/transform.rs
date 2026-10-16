// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Turning, mirroring, cropping and resizing pictures, and changing their canvas.
//!
//! Resizing smoothly is `vimage`'s (fast_image_resize's Lanczos filter, which keeps
//! transparency apart from colour); pixelated resizing repeats or drops whole pixels, for
//! pixel art.

use alloc::vec::Vec;

use vgfx::{Bitmap, Rect};

use crate::color::unpremultiply;

/// The largest picture Paint makes (pixels a side).
pub const MAX_SIDE: i32 = 16384;

/// `b` turned a quarter turn clockwise.
pub fn rotate_cw(b: &Bitmap) -> Bitmap {
    let (w, h) = (b.width, b.height);
    let mut out = Bitmap::new(h, w);
    for y in 0..h {
        for x in 0..w {
            // (x, y) goes to (h - 1 - y, x).
            out.pixels[(x * h + (h - 1 - y)) as usize] = b.pixels[(y * w + x) as usize];
        }
    }
    out
}

/// `b` turned a quarter turn anticlockwise.
pub fn rotate_ccw(b: &Bitmap) -> Bitmap {
    let (w, h) = (b.width, b.height);
    let mut out = Bitmap::new(h, w);
    for y in 0..h {
        for x in 0..w {
            out.pixels[((w - 1 - x) * h + y) as usize] = b.pixels[(y * w + x) as usize];
        }
    }
    out
}

/// Turns `b` half a turn, in place.
pub fn rotate_half(b: &mut Bitmap) {
    b.pixels.reverse();
}

/// Mirrors `b` left to right, in place.
pub fn flip_horizontal(b: &mut Bitmap) {
    let w = b.width as usize;
    if w == 0 {
        return;
    }
    for row in b.pixels.chunks_exact_mut(w) {
        row.reverse();
    }
}

/// Mirrors `b` top to bottom, in place.
pub fn flip_vertical(b: &mut Bitmap) {
    let (w, h) = (b.width as usize, b.height as usize);
    for y in 0..h / 2 {
        let (top, bottom) = b.pixels.split_at_mut((h - 1 - y) * w);
        top[y * w..y * w + w].swap_with_slice(&mut bottom[..w]);
    }
}

/// Where a picture goes on a canvas of another size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Anchor {
    TopLeft,
    Top,
    TopRight,
    Left,
    #[default]
    Center,
    Right,
    BottomLeft,
    Bottom,
    BottomRight,
}

impl Anchor {
    pub const ALL: [Anchor; 9] = [
        Anchor::TopLeft,
        Anchor::Top,
        Anchor::TopRight,
        Anchor::Left,
        Anchor::Center,
        Anchor::Right,
        Anchor::BottomLeft,
        Anchor::Bottom,
        Anchor::BottomRight,
    ];

    /// Its column and row in a three-by-three grid.
    pub fn cell(self) -> (i32, i32) {
        let i = Anchor::ALL.iter().position(|a| *a == self).unwrap_or(4) as i32;
        (i % 3, i / 3)
    }

    /// The anchor called `name` ("top left", "center", "bottom").
    pub fn named(name: &str) -> Option<Anchor> {
        let n: alloc::string::String =
            name.chars().filter(|c| c.is_alphabetic()).collect::<alloc::string::String>().to_lowercase();
        Some(match n.as_str() {
            "topleft" => Anchor::TopLeft,
            "top" => Anchor::Top,
            "topright" => Anchor::TopRight,
            "left" => Anchor::Left,
            "center" | "centre" | "middle" => Anchor::Center,
            "right" => Anchor::Right,
            "bottomleft" => Anchor::BottomLeft,
            "bottom" => Anchor::Bottom,
            "bottomright" => Anchor::BottomRight,
            _ => return None,
        })
    }
}

/// `b` on a canvas of `w` by `h` (premultiplied `fill` where it does not reach), placed by
/// `anchor`: more canvas around it, or less of it.
pub fn canvas_size(b: &Bitmap, w: i32, h: i32, anchor: Anchor, fill: u32) -> Bitmap {
    let (w, h) = (w.clamp(1, MAX_SIDE), h.clamp(1, MAX_SIDE));
    let (col, row) = anchor.cell();
    let x = (w - b.width) * col / 2;
    let y = (h - b.height) * row / 2;
    let mut out = crate::brush::filled(w, h, fill);
    let dst = Rect::new(x, y, b.width, b.height).intersect(&out.rect());
    for oy in dst.y..dst.bottom() {
        let src = ((oy - y) * b.width + (dst.x - x)) as usize;
        let at = (oy * w + dst.x) as usize;
        out.pixels[at..at + dst.w as usize].copy_from_slice(&b.pixels[src..src + dst.w as usize]);
    }
    out
}

/// How a resized picture's pixels are made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Resample {
    /// Smoothly (a Lanczos filter).
    #[default]
    Smooth,
    /// Whole pixels repeated or dropped (pixel art).
    Pixelated,
}

/// `b` resized to `w` by `h`.
pub fn resize(b: &Bitmap, w: i32, h: i32, how: Resample) -> Bitmap {
    let (w, h) = (w.clamp(1, MAX_SIDE), h.clamp(1, MAX_SIDE));
    if (w, h) == (b.width, b.height) || b.width == 0 || b.height == 0 {
        return if b.width == 0 { Bitmap::new(w, h) } else { b.clone() };
    }
    match how {
        Resample::Pixelated => {
            let mut out = Bitmap::new(w, h);
            // The source pixel under each new pixel's centre.
            let nearest = |i: i32, from: i32, to: i32| ((2 * i as i64 + 1) * from as i64 / (2 * to as i64)) as usize;
            let cols: Vec<usize> = (0..w).map(|x| nearest(x, b.width, w)).collect();
            for y in 0..h {
                let sy = nearest(y, b.height, h).min(b.height as usize - 1);
                let src = &b.pixels[sy * b.width as usize..(sy + 1) * b.width as usize];
                let dst = &mut out.pixels[(y * w) as usize..((y + 1) * w) as usize];
                for (d, &c) in dst.iter_mut().zip(&cols) {
                    *d = src[c.min(src.len() - 1)];
                }
            }
            out
        }
        Resample::Smooth => {
            let straight: Vec<u32> = b.pixels.iter().map(|&p| unpremultiply(p)).collect();
            let image = vimage::Image { width: b.width as u32, height: b.height as u32, pixels: straight };
            let resized = vimage::resize(&image, w as u32, h as u32, vimage::Filter::Lanczos3);
            Bitmap::from_straight(w, h, resized.pixels)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 3 by 2 picture whose pixels are their indices.
    fn numbered() -> Bitmap {
        Bitmap { width: 3, height: 2, pixels: (0..6).map(|i| 0xFF00_0000 | i).collect() }
    }

    fn ids(b: &Bitmap) -> Vec<u32> {
        b.pixels.iter().map(|p| p & 0xFF).collect()
    }

    #[test]
    fn turns_and_mirrors() {
        // 0 1 2      3 0       2 5
        // 3 4 5  cw: 4 1  ccw: 1 4
        //            5 2       0 3
        let b = numbered();
        let cw = rotate_cw(&b);
        assert_eq!((cw.width, cw.height, ids(&cw)), (2, 3, alloc::vec![3, 0, 4, 1, 5, 2]));
        assert_eq!(ids(&rotate_ccw(&b)), [2, 5, 1, 4, 0, 3]);
        assert_eq!(ids(&rotate_ccw(&rotate_cw(&b))), ids(&b));
        let mut half = b.clone();
        rotate_half(&mut half);
        assert_eq!(ids(&half), [5, 4, 3, 2, 1, 0]);
        let mut h = b.clone();
        flip_horizontal(&mut h);
        assert_eq!(ids(&h), [2, 1, 0, 5, 4, 3]);
        let mut v = b.clone();
        flip_vertical(&mut v);
        assert_eq!(ids(&v), [3, 4, 5, 0, 1, 2]);
    }

    #[test]
    fn canvases_grow_and_shrink_round_the_anchor() {
        let b = numbered();
        let big = canvas_size(&b, 5, 4, Anchor::Center, 0);
        assert_eq!(big.get(1, 1), 0xFF00_0000, "centred: one pixel in, one down");
        assert_eq!(big.get(0, 0), 0);
        let corner = canvas_size(&b, 5, 4, Anchor::BottomRight, 0xFFFF_FFFF);
        assert_eq!(corner.get(4, 3), 0xFF00_0005);
        assert_eq!(corner.get(0, 0), 0xFFFF_FFFF);
        let small = canvas_size(&b, 2, 1, Anchor::TopLeft, 0);
        assert_eq!(ids(&small), [0, 1]);
        assert_eq!(Anchor::named("Top Right"), Some(Anchor::TopRight));
        assert_eq!(Anchor::named("middle"), Some(Anchor::Center));
    }

    #[test]
    fn resizing() {
        let b = numbered();
        let doubled = resize(&b, 6, 4, Resample::Pixelated);
        assert_eq!(ids(&doubled)[..6], [0, 0, 1, 1, 2, 2]);
        assert_eq!(ids(&doubled)[18..], [3, 3, 4, 4, 5, 5]);
        let flat = crate::brush::filled(40, 30, 0xFF33_6699);
        let smooth = resize(&flat, 17, 9, Resample::Smooth);
        assert_eq!((smooth.width, smooth.height), (17, 9));
        assert!(smooth.pixels.iter().all(|&p| p == 0xFF33_6699), "a flat colour stays itself");
        assert_eq!(resize(&b, 0, -5, Resample::Smooth).width, 1);
    }
}
