// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Drawing the picture in its window, at any zoom.
//!
//! Each pixel of the window shows the picture's pixel under its centre: whole pixels when
//! zoomed in (so that pixel art shows its pixels), and when zoomed out, the pixel of a copy
//! of the picture halved as many times as fits (each pixel of a half the average of four),
//! so that what is drawn is never a sampling of far fewer pixels than it stands for. The
//! halves are made when first needed and kept, and only their parts of a change are made
//! again. Transparent pixels are drawn over a checkerboard.

use alloc::vec::Vec;

use gfx::color::{keep_scalar, over};
use gfx::{Bitmap, Canvas, Color, Rect};

use crate::view::View;

/// The checkerboard's squares (window pixels) and colours.
const CHECK: i32 = 8;
const LIGHT: u32 = 0xFFF2_F2F2;
const DARK: u32 = 0xFFCC_CCD2;
/// The zoom from which the pixel grid shows.
pub const GRID_FROM: f32 = 8.0;

/// The halves of the picture kept for zooming out, and what of them is out of date.
#[derive(Default)]
pub struct Renderer {
    /// Halves, quarters, ... of the picture.
    levels: Vec<Bitmap>,
    /// The part of the picture changed since each level was made (`None`: nothing).
    stale: Vec<Option<Rect>>,
    /// The picture's size the levels were made for.
    size: (i32, i32),
}

/// The average of four premultiplied pixels.
#[inline]
fn average(a: u32, b: u32, c: u32, d: u32) -> u32 {
    let channel = |s: u32| (((a >> s) & 0xFF) + ((b >> s) & 0xFF) + ((c >> s) & 0xFF) + ((d >> s) & 0xFF) + 2) >> 2;
    channel(24) << 24 | channel(16) << 16 | channel(8) << 8 | channel(0)
}

/// Makes `r` (in `dst`'s pixels) of `dst`, a half of `src`.
fn halve(src: &Bitmap, dst: &mut Bitmap, r: Rect) {
    let r = r.intersect(&dst.rect());
    for y in r.y..r.bottom() {
        let (y0, y1) = ((2 * y).min(src.height - 1), (2 * y + 1).min(src.height - 1));
        for x in r.x..r.right() {
            let (x0, x1) = ((2 * x).min(src.width - 1), (2 * x + 1).min(src.width - 1));
            dst.pixels[(y * dst.width + x) as usize] =
                average(src.get(x0, y0), src.get(x1, y0), src.get(x0, y1), src.get(x1, y1));
            keep_scalar();
        }
    }
}

impl Renderer {
    /// The picture changed in `r` (`None`: all of it, perhaps its size).
    pub fn changed(&mut self, r: Option<Rect>) {
        match r {
            None => {
                self.levels.clear();
                self.stale.clear();
            }
            Some(r) => {
                for s in &mut self.stale {
                    *s = Some(match *s {
                        Some(old) => old.union(&r),
                        None => r,
                    });
                }
            }
        }
    }

    /// The level to draw at `zoom` (0 is the picture), its halves made or brought up to date.
    fn level(&mut self, picture: &Bitmap, zoom: f32) -> usize {
        if (picture.width, picture.height) != self.size {
            self.changed(None);
            self.size = (picture.width, picture.height);
        }
        let mut want = 0;
        let mut z = zoom;
        while z <= 0.5 && want < 12 {
            z *= 2.0;
            want += 1;
        }
        for k in 1..=want {
            if self.levels.len() < k {
                let src = if k == 1 { picture } else { &self.levels[k - 2] };
                let (w, h) = ((src.width + 1) / 2, (src.height + 1) / 2);
                if w < 1 || h < 1 || (src.width == 1 && src.height == 1) {
                    return k - 1;
                }
                let mut half = Bitmap::new(w, h);
                let all = half.rect();
                halve(src, &mut half, all);
                self.levels.push(half);
                self.stale.push(None);
            } else if let Some(r) = self.stale[k - 1].take() {
                // The change, in this level's pixels (rounded out).
                let scale = 1 << k;
                let part = Rect::new(r.x / scale, r.y / scale, r.w / scale + 2, r.h / scale + 2);
                let (before, rest) = self.levels.split_at_mut(k - 1);
                let src = if k == 1 { picture } else { &before[k - 2] };
                halve(src, &mut rest[0], part);
            }
        }
        want.min(self.levels.len())
    }

    /// Draws `picture` as `view` shows it in `viewport` (window coordinates) of `canvas`.
    pub fn draw(&mut self, canvas: &mut Canvas, picture: &Bitmap, view: &View, viewport: Rect) {
        let level = self.level(picture, view.zoom);
        let src = if level == 0 { picture } else { &self.levels[level - 1] };
        let scale = view.zoom * (1 << level) as f32;
        let shown = view.picture_rect(viewport, picture.width, picture.height);
        let area = shown.intersect(&viewport).intersect(&canvas.clip_rect());
        if area.is_empty() || src.width == 0 || src.height == 0 {
            return;
        }
        // The canvas's own pixels, in device coordinates.
        let offset = canvas.bounds();
        let (ox, oy) = (-offset.x, -offset.y);
        let (origin_x, origin_y) = (viewport.x as f32 + view.origin.0, viewport.y as f32 + view.origin.1);
        let sample = |screen: i32, origin: f32, len: i32| {
            (libm::floorf((screen as f32 + 0.5 - origin) / scale) as i32).clamp(0, len - 1) as usize
        };
        let cols: Vec<usize> = (area.x..area.right()).map(|x| sample(x, origin_x, src.width)).collect();
        let (pixels, stride) = canvas.pixels_mut();
        for y in area.y..area.bottom() {
            let sy = sample(y, origin_y, src.height);
            let row = &src.pixels[sy * src.width as usize..(sy + 1) * src.width as usize];
            let start = ((y + oy) * stride + area.x + ox) as usize;
            let out = &mut pixels[start..start + area.w as usize];
            let band = (y.div_euclid(CHECK)) & 1;
            for (i, (dst, &c)) in out.iter_mut().zip(&cols).enumerate() {
                let px = row[c];
                *dst = if px >> 24 == 255 {
                    px
                } else {
                    let x = area.x + i as i32;
                    let check = if (x.div_euclid(CHECK) & 1) == band { LIGHT } else { DARK };
                    over(px, check)
                };
                keep_scalar();
            }
        }
        if view.zoom >= GRID_FROM {
            draw_grid(canvas, view, viewport, area);
        }
    }
}

/// Lines between the picture's pixels in `area`, faint on light and dark alike.
fn draw_grid(canvas: &mut Canvas, view: &View, viewport: Rect, area: Rect) {
    let line = Color::rgba(128, 128, 136, 70);
    let (x0, _) = view.to_picture(viewport, area.x as f32, area.y as f32);
    let mut px = libm::floorf(x0) as i32;
    loop {
        let (sx, _) = view.to_screen(viewport, px as f32, 0.0);
        let sx = libm::roundf(sx) as i32;
        if sx >= area.right() {
            break;
        }
        if sx >= area.x {
            canvas.fill_rect(Rect::new(sx, area.y, 1, area.h), line);
        }
        px += 1;
    }
    let (_, y0) = view.to_picture(viewport, area.x as f32, area.y as f32);
    let mut py = libm::floorf(y0) as i32;
    loop {
        let (_, sy) = view.to_screen(viewport, 0.0, py as f32);
        let sy = libm::roundf(sy) as i32;
        if sy >= area.bottom() {
            break;
        }
        if sy >= area.y {
            canvas.fill_rect(Rect::new(area.x, sy, area.w, 1), line);
        }
        py += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brush::filled;

    fn render(r: &mut Renderer, picture: &Bitmap, view: View) -> Bitmap {
        let mut out = filled(100, 80, 0xFF11_1111);
        r.draw(&mut Canvas::for_bitmap(&mut out), picture, &view, Rect::new(10, 10, 80, 60));
        out
    }

    #[test]
    fn pixels_at_one_to_one_and_zoomed_in() {
        let mut p = filled(20, 10, 0xFF00_00FF);
        p.pixels[0] = 0xFFFF_0000;
        let mut r = Renderer::default();
        let out = render(&mut r, &p, View { zoom: 1.0, origin: (5.0, 5.0) });
        assert_eq!(out.get(15, 15), 0xFFFF_0000);
        assert_eq!(out.get(16, 15), 0xFF00_00FF);
        assert_eq!(out.get(14, 15), 0xFF11_1111, "outside the picture");
        assert_eq!(out.get(9, 40), 0xFF11_1111, "outside the viewport");
        let out = render(&mut r, &p, View { zoom: 4.0, origin: (0.0, 0.0) });
        assert_eq!(out.get(13, 13), 0xFFFF_0000);
        assert_eq!(out.get(14, 13), 0xFF00_00FF);
    }

    #[test]
    fn zoomed_out_pixels_are_averages_and_follow_changes() {
        // Black and white stripes a pixel wide: grey at a quarter.
        let mut p = Bitmap::new(64, 64);
        for (i, px) in p.pixels.iter_mut().enumerate() {
            *px = if i % 2 == 0 { 0xFF00_0000 } else { 0xFFFF_FFFF };
        }
        let mut r = Renderer::default();
        let out = render(&mut r, &p, View { zoom: 0.25, origin: (0.0, 0.0) });
        let grey = out.get(12, 12);
        assert!((grey & 0xFF).abs_diff(128) <= 1, "{grey:08x}");
        // A change shows once it is reported.
        for px in &mut p.pixels[..64 * 8] {
            *px = 0xFFFF_0000;
        }
        r.changed(Some(Rect::new(0, 0, 64, 8)));
        let out = render(&mut r, &p, View { zoom: 0.25, origin: (0.0, 0.0) });
        assert_eq!(out.get(10, 10), 0xFFFF_0000);
        assert!((out.get(12, 14) & 0xFF).abs_diff(128) <= 1, "below the change");
    }

    #[test]
    fn transparency_shows_a_checkerboard() {
        let p = filled(40, 40, 0);
        let mut r = Renderer::default();
        let out = render(&mut r, &p, View { zoom: 1.0, origin: (0.0, 0.0) });
        assert_eq!(out.get(10, 10), LIGHT);
        assert_eq!(out.get(18, 10), DARK);
        assert_eq!(out.get(18, 18), LIGHT);
    }
}
