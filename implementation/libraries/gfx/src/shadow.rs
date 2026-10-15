// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Soft drop shadows, rendered as cached nine-slice images.
//!
//! A shadow is the blurred silhouette of a rounded rectangle. Its corners
//! are rendered once per (radius, blur) pair into a small alpha mask; along
//! the edges the shadow is constant, so drawing a shadow of any size costs
//! four corner blits plus a few row/column fills — cheap enough to redo
//! every frame. [`Canvas::draw_shadow`] keeps the templates it makes in the
//! canvas's [`DrawCache`](crate::DrawCache).

use alloc::vec::Vec;

// Float math under `no_std` (std, linked into host tests, has these inherently).
#[allow(unused_imports)]
use num_traits::Float;

use crate::bitmap::Bitmap;
use crate::canvas::{Canvas, blend_cov};
use crate::color::{Color, blend_run, keep_scalar, scale};
use crate::geom::Rect;

/// Smooth falloff approximating a Gaussian-blurred edge: 1 well inside,
/// 0.5 at the edge, 0 at `blur` pixels outside.
fn falloff(signed_distance: f32, blur: f32) -> f32 {
    if blur <= 0.0 {
        return if signed_distance <= 0.0 { 1.0 } else { 0.0 };
    }
    let t = (0.5 - signed_distance / (2.0 * blur)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Pre-rendered shadow pieces for one (corner radius, blur) pair.
pub struct ShadowTemplate {
    radius: i32,
    blur: i32,
    /// `size` x `size` alpha mask of the top-left corner of the shadow.
    corner: Vec<u8>,
    /// Alpha across an edge, from the outer border inwards (`size` values).
    edge: Vec<u8>,
    size: i32,
}

impl ShadowTemplate {
    pub fn new(radius: i32, blur: i32) -> ShadowTemplate {
        let (radius, blur) = (radius.max(0), blur.max(0));
        // The template reaches `blur` pixels inside the shape, where the
        // shadow becomes fully opaque.
        let size = (radius + 2 * blur).max(1);
        let (r, b) = (radius as f32, blur as f32);
        let c = (blur + radius) as f32; // corner circle centre, both axes
        let mut corner = alloc::vec![0u8; (size * size) as usize];
        for y in 0..size {
            for x in 0..size {
                // Rounded-rectangle signed distance in the corner quadrant.
                let qx = c - (x as f32 + 0.5);
                let qy = c - (y as f32 + 0.5);
                let (mx, my) = (qx.max(0.0), qy.max(0.0));
                let sd = (mx * mx + my * my).sqrt() + qx.max(qy).min(0.0) - r;
                corner[(y * size + x) as usize] = (falloff(sd, b) * 255.0 + 0.5) as u8;
            }
        }
        let edge = (0..size).map(|i| (falloff(b - (i as f32 + 0.5), b) * 255.0 + 0.5) as u8).collect();
        ShadowTemplate { radius, blur, corner, edge, size }
    }

    pub fn radius(&self) -> i32 {
        self.radius
    }

    pub fn blur(&self) -> i32 {
        self.blur
    }

    /// Draws the shadow of the rounded rectangle `r` (it extends `blur`
    /// pixels beyond `r`). With `fill_interior` false the part well inside
    /// `r` is skipped, which is correct under an opaque window.
    ///
    /// Only what lies in the canvas's clip is drawn, row by row: the corners
    /// (the template's, mirrored), the edges between them and the interior.
    pub fn draw(&self, c: &mut Canvas, r: Rect, color: Color, fill_interior: bool) {
        let px = color.premul();
        let outer = r.inflate(self.blur);
        let s = self.size.min(outer.w / 2).min(outer.h / 2);
        let visible = c.device(outer);
        if s <= 0 || px == 0 || visible.is_empty() {
            return;
        }
        let (x0, y0) = c.to_device(outer.x, outer.y);
        let (x1, y1) = (x0 + outer.w, y0 + outer.h);
        // The columns of the left corner or edge, of what is between, and of
        // the right corner or edge, as far as they show.
        let part = |a: i32, b: i32| (a.max(visible.x), b.min(visible.right()));
        let (left, middle, right) = (part(x0, x0 + s), part(x0 + s, x1 - s), part(x1 - s, x1));
        let stride = self.size as usize;
        for y in visible.y..visible.bottom() {
            let row = c.device_row(y, visible.x, visible.right());
            let at = |x: i32| (x - visible.x) as usize;
            // The template's row for a row of the top or the bottom corners.
            let corner_row = if y - y0 < s {
                Some(y - y0)
            } else if y1 - 1 - y < s {
                Some(y1 - 1 - y)
            } else {
                None
            };
            match corner_row {
                Some(ty) => {
                    let corner = &self.corner[ty as usize * stride..];
                    for x in left.0..left.1 {
                        blend_cov(&mut row[at(x)], px, corner[(x - x0) as usize] as u32);
                        keep_scalar();
                    }
                    let a = self.edge[ty as usize] as u32;
                    if a > 0 && middle.1 > middle.0 {
                        blend_run(&mut row[at(middle.0)..at(middle.1)], scale(px, a));
                    }
                    for x in right.0..right.1 {
                        blend_cov(&mut row[at(x)], px, corner[(x1 - 1 - x) as usize] as u32);
                        keep_scalar();
                    }
                }
                None => {
                    for x in left.0..left.1 {
                        blend_cov(&mut row[at(x)], px, self.edge[(x - x0) as usize] as u32);
                        keep_scalar();
                    }
                    if fill_interior && middle.1 > middle.0 {
                        blend_run(&mut row[at(middle.0)..at(middle.1)], px);
                    }
                    for x in right.0..right.1 {
                        blend_cov(&mut row[at(x)], px, self.edge[(x1 - 1 - x) as usize] as u32);
                        keep_scalar();
                    }
                }
            }
        }
    }
}

/// A blurred alpha silhouette of `src` (for shadows under icons and text).
pub fn silhouette_shadow(src: &Bitmap, blur: i32) -> Bitmap {
    let pad = blur * 2;
    let mut b = Bitmap::new(src.width + 2 * pad, src.height + 2 * pad);
    for y in 0..src.height {
        for x in 0..src.width {
            let a = src.get(x, y) >> 24;
            b.pixels[((y + pad) * b.width + x + pad) as usize] = a << 24;
        }
    }
    b.blur(blur.max(1) / 2 + 1, 3);
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shadow as it was first drawn: every pixel of the corners on its
    /// own, the edges as one-pixel rectangles.
    fn reference(t: &ShadowTemplate, c: &mut Canvas, r: Rect, color: Color, fill_interior: bool) {
        let px = color.premul();
        let blend_at = |c: &mut Canvas, x: i32, y: i32, px: u32, cov: u32| {
            let (dx, dy) = c.to_device(x, y);
            if c.device(Rect::new(x, y, 1, 1)) == Rect::new(dx, dy, 1, 1) {
                blend_cov(&mut c.device_row(dy, dx, dx + 1)[0], px, cov);
            }
        };
        let outer = r.inflate(t.blur);
        let s = t.size.min(outer.w / 2).min(outer.h / 2);
        if s <= 0 {
            return;
        }
        let (x0, y0, x1, y1) = (outer.x, outer.y, outer.right(), outer.bottom());
        for y in 0..s {
            for x in 0..s {
                let a = t.corner[(y * t.size + x) as usize] as u32;
                blend_at(c, x0 + x, y0 + y, px, a);
                blend_at(c, x1 - 1 - x, y0 + y, px, a);
                blend_at(c, x0 + x, y1 - 1 - y, px, a);
                blend_at(c, x1 - 1 - x, y1 - 1 - y, px, a);
            }
        }
        let (inner_w, inner_h) = (outer.w - 2 * s, outer.h - 2 * s);
        for i in 0..s {
            let p = scale(px, t.edge[i as usize] as u32);
            c.fill_rect_px(Rect::new(x0 + s, y0 + i, inner_w, 1), p);
            c.fill_rect_px(Rect::new(x0 + s, y1 - 1 - i, inner_w, 1), p);
            c.fill_rect_px(Rect::new(x0 + i, y0 + s, 1, inner_h), p);
            c.fill_rect_px(Rect::new(x1 - 1 - i, y0 + s, 1, inner_h), p);
        }
        if fill_interior {
            c.fill_rect_px(Rect::new(x0 + s, y0 + s, inner_w, inner_h), px);
        }
    }

    #[test]
    fn shadows_draw_only_their_clipped_part_and_match_the_whole() {
        let color = Color::rgba(10, 20, 40, 150);
        for (radius, blur) in [(0, 0), (6, 8), (12, 24), (3, 1), (20, 2)] {
            let t = ShadowTemplate::new(radius, blur);
            for r in
                [Rect::new(20, 15, 50, 40), Rect::new(-10, 30, 30, 30), Rect::new(40, 40, 3, 70), Rect::new(5, 5, 1, 1)]
            {
                for clip in [
                    Rect::new(0, 0, 90, 80),
                    Rect::new(7, 9, 20, 15),
                    Rect::new(60, 0, 30, 80),
                    Rect::new(0, 50, 90, 3),
                ] {
                    for fill in [true, false] {
                        let base: Vec<u32> =
                            (0..90 * 80u32).map(|i| 0xFF00_0000 | i.wrapping_mul(0x9E37_79B9) >> 8).collect();
                        let (mut want, mut got) = (base.clone(), base);
                        for (pixels, new) in [(&mut want, false), (&mut got, true)] {
                            let mut c = Canvas::new(pixels, 90, 80, 90);
                            c.translate(3, -2);
                            c.clip_to(clip.translate(-3, 2));
                            if new {
                                t.draw(&mut c, r, color, fill);
                            } else {
                                reference(&t, &mut c, r, color, fill);
                            }
                        }
                        assert!(want == got, "radius {radius} blur {blur} rect {r:?} clip {clip:?} fill {fill}");
                    }
                }
            }
        }
    }
}
