// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `vgfx` — Veda 2D graphics.
//!
//! * [`Canvas`]: drawing on a borrowed buffer of premultiplied `0xAARRGGBB`
//!   pixels — rectangles, gradients, anti-aliased rounded rectangles and
//!   circles, vector paths (through `vraster`), coverage masks, bitmaps
//!   (with scaling) and pictures with rounded corners ([`Picture`]) — all
//!   clipped and translated, with what drawing keeps between frames in a
//!   [`DrawCache`].
//! * [`Bitmap`]: owned images with resizing, downscaling of any picture
//!   ([`bitmap::downscale`]) and blurring.
//! * [`ShadowTemplate`]: fast soft shadows.
//! * [`Text`]: fonts and cached glyph rendering (through `vfont`).
//! * [`Damage`]: changed rectangles, also found by comparing two frames.
//!
//! Everything is integer- and cache-friendly because Veda usually runs
//! under CPU emulation, and the loops over pixels stay scalar
//! ([`color`] says why and how to check).

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod bitmap;
pub mod canvas;
pub mod color;
pub mod geom;
pub mod shadow;
pub mod text;

pub use bitmap::Bitmap;
pub use canvas::{Canvas, DrawCache, Filter, Picture};
pub use color::Color;
pub use geom::{Damage, Rect};
pub use shadow::ShadowTemplate;
pub use text::{Align, LineMetrics, Text};
pub use vraster::{FillRule, LineCap, LineJoin, Path, StrokeStyle, Transform};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disjoint_rectangles_intersect_to_canonical_empty() {
        let a = Rect::new(0, 0, 100, 100);
        assert_eq!(a.intersect(&Rect::new(1960, 10, 50, 50)), Rect::default());
        assert!(!a.intersects(&Rect::new(-60, 0, 50, 50)));
        assert_eq!(a.intersect(&Rect::new(50, 50, 100, 100)), Rect::new(50, 50, 50, 50));
    }

    #[test]
    fn resizing_keeps_colours_and_premultiplication() {
        // A uniform picture keeps its colour exactly, shrunk or grown.
        let flat = Bitmap::filled(97, 61, 0xFFC8_6432);
        for (w, h) in [(13, 9), (200, 150), (97, 30), (1, 1)] {
            let r = flat.resized(w, h);
            assert_eq!((r.width, r.height), (w, h));
            assert!(r.pixels.iter().all(|&p| p == 0xFFC8_6432), "{w}x{h}");
        }
        // A gradient keeps its direction: brighter to the right, the same
        // down each column.
        let mut ramp = Bitmap::new(64, 16);
        for (i, p) in ramp.pixels.iter_mut().enumerate() {
            *p = 0xFF00_0000 | (i % 64 * 4) as u32;
        }
        for (w, h) in [(16, 4), (100, 20)] {
            let r = ramp.resized(w, h);
            let at = |x: i32, y: i32| r.pixels[(y * w + x) as usize];
            assert!((1..w).all(|x| at(x, 0) >= at(x - 1, 0)) && at(w - 1, 0) > at(0, 0), "{w}x{h}");
            assert!((0..w).all(|x| (1..h).all(|y| at(x, y) == at(x, 0))), "{w}x{h}");
        }
        // Opaque white beside transparency: no channel exceeds the alpha.
        let mut half = Bitmap::new(16, 4);
        for (i, p) in half.pixels.iter_mut().enumerate() {
            if i % 16 < 8 {
                *p = 0xFFFF_FFFF;
            }
        }
        for (w, h) in [(3, 1), (40, 7)] {
            for p in half.resized(w, h).pixels {
                assert!([16, 8, 0].iter().all(|s| (p >> s) & 0xFF <= p >> 24), "{p:08x}");
            }
        }
        // Nothing to scale, or nothing to scale to.
        assert!(Bitmap::new(0, 5).resized(4, 4).pixels.iter().all(|&p| p == 0));
        assert!(flat.resized(0, 3).pixels.is_empty());
        assert!(flat.resized(-2, 3).pixels.is_empty());
    }

    #[test]
    fn rectangle_containment() {
        let a = Rect::new(10, 10, 100, 50);
        assert!(a.contains_rect(&a));
        assert!(a.contains_rect(&Rect::new(20, 20, 10, 10)));
        assert!(!a.contains_rect(&Rect::new(5, 20, 10, 10)), "sticks out on the left");
        assert!(!a.contains_rect(&Rect::new(100, 50, 20, 20)), "sticks out at the bottom right");
        assert!(a.contains_rect(&Rect::default()), "empty rectangles are inside");
        assert!(!Rect::default().contains_rect(&Rect::default()), "nothing is inside an empty rectangle");
    }

    #[test]
    fn drawing_entirely_outside_the_canvas_is_a_no_op() {
        // Regression: a fill far right of the clip used to slice past a row.
        let mut b = Bitmap::new(16, 8);
        let mut c = Canvas::for_bitmap(&mut b);
        c.fill_rect(Rect::new(40, 2, 10, 3), Color::WHITE);
        c.fill_rect(Rect::new(-40, 2, 10, 3), Color::WHITE);
        c.fill_rounded_rect(Rect::new(30, -20, 10, 10), 3.0, Color::WHITE);
        c.draw_shadow(Rect::new(60, 60, 20, 20), 4, 6, Color::BLACK);
        let src = Bitmap::filled(4, 4, 0xFFFF_FFFF);
        c.draw_bitmap(&src, 100, 0, 255);
        c.blit(&src, src.rect(), -50, -50);
        drop(c);
        assert!(b.pixels.iter().all(|&p| p == 0));
    }

    #[test]
    fn scaled_drawing_samples_pixel_centres() {
        let src = Bitmap::from_straight(2, 1, alloc::vec![0xFF00_0000, 0xFFFF_FFFF]);
        let scaled = |filter| {
            let mut b = Bitmap::new(4, 1);
            Canvas::for_bitmap(&mut b).draw_bitmap_scaled(&src, Rect::new(0, 0, 4, 1), filter, 255);
            b.pixels
        };
        assert_eq!(scaled(Filter::Nearest), [0xFF00_0000, 0xFF00_0000, 0xFFFF_FFFF, 0xFFFF_FFFF]);
        // Bilinear: edges clamp; inner pixels are a quarter and three
        // quarters of the way from black to white (255 × ¼ and 255 × ¾,
        // truncated by the 8-bit blend).
        let p = scaled(Filter::Bilinear);
        assert_eq!((p[0], p[3]), (0xFF00_0000, 0xFFFF_FFFF));
        assert_eq!((p[1] & 0xFF, p[2] & 0xFF), (63, 191));
    }

    #[test]
    fn fill_and_clip() {
        let mut b = Bitmap::new(20, 10);
        let mut c = Canvas::for_bitmap(&mut b);
        c.clip_to(Rect::new(5, 0, 10, 10));
        c.fill_rect(Rect::new(0, 0, 20, 10), Color::rgb(255, 0, 0));
        assert_eq!(b.get(4, 5), 0);
        assert_eq!(b.get(5, 5), 0xFFFF_0000);
        assert_eq!(b.get(14, 5), 0xFFFF_0000);
        assert_eq!(b.get(15, 5), 0);
    }

    #[test]
    fn rounded_rect_corners_are_antialiased() {
        let mut b = Bitmap::new(40, 40);
        let mut c = Canvas::for_bitmap(&mut b);
        c.fill_rounded_rect(Rect::new(0, 0, 40, 40), 10.0, Color::WHITE);
        assert_eq!(b.get(0, 0) >> 24, 0, "corner outside the arc");
        assert_eq!(b.get(20, 20), 0xFFFF_FFFF, "interior");
        assert_eq!(b.get(20, 0), 0xFFFF_FFFF, "straight top edge");
        let partial = b.get(3, 2) >> 24;
        assert!(partial > 0 && partial < 255, "arc pixel is partially covered: {partial}");
    }

    #[test]
    fn shadow_is_symmetric_and_fades_out() {
        let mut b = Bitmap::new(100, 100);
        let mut c = Canvas::for_bitmap(&mut b);
        ShadowTemplate::new(8, 10).draw(&mut c, Rect::new(20, 20, 60, 60), Color::BLACK, true);
        assert_eq!(b.get(0, 0) >> 24, 0);
        assert_eq!(b.get(50, 50) >> 24, 255);
        assert_eq!(b.get(15, 50) >> 24, b.get(84, 50) >> 24);
        assert_eq!(b.get(50, 15) >> 24, b.get(50, 84) >> 24);
        assert!(b.get(50, 12) >> 24 < b.get(50, 22) >> 24);
    }

    #[test]
    fn damage_merges_overlaps() {
        let mut d = Damage::new();
        d.add(Rect::new(0, 0, 10, 10));
        d.add(Rect::new(5, 5, 10, 10));
        d.add(Rect::new(100, 100, 5, 5));
        assert_eq!(d.rects().len(), 2);
        assert_eq!(d.bounds(), Rect::new(0, 0, 105, 105));
    }

    #[test]
    fn changes_between_frames_are_found_in_bands() {
        // 61 pixels wide (an odd one at the end of each row), rows 64 apart.
        let (w, h, stride) = (61, 50, 64usize);
        let old: alloc::vec::Vec<u32> = (0..stride * h as usize).map(|i| i as u32).collect();
        assert!(Damage::of_changes(&old, &old, w, h, stride).is_empty(), "the same frame");
        let mut new = old.clone();
        // A caret: two columns over rows 10..30.
        for y in 10..30 {
            new[y * stride + 7] ^= 1;
            new[y * stride + 8] ^= 0x100;
        }
        // The last pixel of row 45 and the first of row 46; past the width, nothing counts.
        new[45 * stride + 60] = 0;
        new[46 * stride] = 7;
        new[47 * stride + 62] = 9;
        let d = Damage::of_changes(&old, &new, w, h, stride);
        assert_eq!(d.rects(), &[Rect::new(7, 10, 2, 20), Rect::new(0, 45, 61, 2)]);
        // One pixel of a pair, either one.
        let mut one = old.clone();
        one[3 * stride + 33] = 0;
        assert_eq!(Damage::of_changes(&old, &one, w, h, stride).rects(), &[Rect::new(33, 3, 1, 1)]);
        one[3 * stride + 33] = old[3 * stride + 33];
        one[3 * stride + 34] = 0;
        assert_eq!(Damage::of_changes(&old, &one, w, h, stride).rects(), &[Rect::new(34, 3, 1, 1)]);
        // Buffers too short for their size are wholly changed.
        assert_eq!(Damage::of_changes(&old[..100], &new, w, h, stride).rects(), &[Rect::new(0, 0, w, h)]);
    }
}
