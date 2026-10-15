// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Painting the films' pictures: a floating-point RGB canvas (in the
//! generators' colours, `gencommon::colour`), vector paths composited
//! through `vraster` with a colour per pixel, soft lights, text outlines
//! from `vfont`, and the step down to 8-bit pixels.

pub use gencommon::colour::{Rgb, add, gradient, hex, mix, scale, smoothstep};
use gencommon::hash::hash01;
use vfont::{Font, PathSink};
use vraster::{Coverage, FillRule, Path, Point, Rasterizer, Transform};

/// Rolls bright light off smoothly instead of clipping it.
pub fn tonemap(c: Rgb) -> Rgb {
    c.map(|v| if v <= 0.75 { v.max(0.0) } else { 0.75 + 0.25 * (1.0 - (-(v - 0.75) / 0.25).exp()) })
}

/// Smooth 1D value noise in `[-1, 1]`.
pub fn noise1(x: f32, seed: u32) -> f32 {
    let i = x.floor();
    let f = x - i;
    let t = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
    let a = hash01(i as i32, 0, seed) * 2.0 - 1.0;
    let b = hash01(i as i32 + 1, 0, seed) * 2.0 - 1.0;
    a + (b - a) * t
}

/// Octaves of [`noise1`], each at twice the frequency and half the
/// amplitude, in `[-1, 1]`.
pub fn fbm1(x: f32, octaves: u32, seed: u32) -> f32 {
    let (mut sum, mut amp, mut norm, mut x) = (0.0, 1.0, 0.0, x);
    for i in 0..octaves {
        sum += amp * noise1(x, seed.wrapping_add(i * 7919));
        norm += amp;
        amp *= 0.5;
        x = x * 2.03 + 17.1;
    }
    sum / norm
}

/// A floating-point picture.
#[derive(Clone)]
pub struct Canvas {
    pub w: usize,
    pub h: usize,
    pub px: Vec<Rgb>,
}

/// How a shape's colour meets the picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Blend {
    /// Painted over, by its alpha.
    Over,
    /// Light added, scaled by its alpha.
    Add,
}

impl Canvas {
    pub fn new(w: usize, h: usize) -> Canvas {
        Canvas { w, h, px: vec![[0.0; 3]; w * h] }
    }

    /// Sets every pixel from its centre's coordinates.
    pub fn shade(&mut self, f: impl Fn(f32, f32) -> Rgb) {
        for (i, p) in self.px.iter_mut().enumerate() {
            *p = f((i % self.w) as f32 + 0.5, (i / self.w) as f32 + 0.5);
        }
    }

    /// Composites `path` (through `t`): `paint(x, y)` gives the colour and
    /// alpha of each pixel it covers, which the coverage scales.
    pub fn fill_path(
        &mut self,
        raster: &mut Rasterizer,
        path: &Path,
        t: &Transform,
        blend: Blend,
        mut paint: impl FnMut(usize, usize) -> (Rgb, f32),
    ) {
        let (w, px) = (self.w, &mut self.px);
        raster.fill(path, t, FillRule::NonZero, (self.w as u32, self.h as u32), |span| {
            let (y, x0) = (span.y as usize, span.x as usize);
            for i in 0..span.len as usize {
                let cov = match span.coverage {
                    Coverage::Solid(c) => c,
                    Coverage::Mask(m) => m[i],
                };
                if cov == 0 {
                    continue;
                }
                let (color, alpha) = paint(x0 + i, y);
                let a = alpha * cov as f32 / 255.0;
                let d = &mut px[y * w + x0 + i];
                *d = match blend {
                    Blend::Over => mix(*d, color, a),
                    Blend::Add => add(*d, scale(color, a)),
                };
            }
        });
    }

    /// Adds a soft round light with a Gaussian profile.
    pub fn splat(&mut self, cx: f32, cy: f32, sigma: f32, color: Rgb) {
        let r = (sigma * 3.0).ceil() as i32;
        let (ix, iy) = (cx.floor() as i32, cy.floor() as i32);
        let k = -1.0 / (2.0 * sigma * sigma);
        for y in (iy - r).max(0)..=(iy + r).min(self.h as i32 - 1) {
            let dy = y as f32 + 0.5 - cy;
            for x in (ix - r).max(0)..=(ix + r).min(self.w as i32 - 1) {
                let dx = x as f32 + 0.5 - cx;
                let g = ((dx * dx + dy * dy) * k).exp();
                let p = &mut self.px[y as usize * self.w + x as usize];
                *p = add(*p, scale(color, g));
            }
        }
    }

    /// Rounds to `0xFFRRGGBB` pixels.
    pub fn to_pixels(&self) -> Vec<u32> {
        let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
        self.px.iter().map(|c| 0xFF00_0000 | q(c[0]) << 16 | q(c[1]) << 8 | q(c[2])).collect()
    }
}

/// A line of text as a path: the pen starts at `(x, baseline)` and moves
/// by the advance widths (kerned). Returns the path and the advance.
pub fn text_path(font: &Font<'_>, size: f32, text: &str, x: f32, baseline: f32) -> (Path, f32) {
    let scale = size / font.units_per_em() as f32;
    let mut path = Path::new();
    let mut pen = x;
    let mut prev = None;
    for c in text.chars() {
        let Some(g) = font.glyph_index(c) else { continue };
        if let Some(p) = prev {
            pen += font.kerning(p, g) as f32 * scale;
        }
        let mut sink = PathSink::new(&mut path, scale, Point::new(pen, baseline));
        // A glyph that fails to load is left out.
        let _ = font.outline(g, &mut sink);
        pen += font.advance_width(g) as f32 * scale;
        prev = Some(g);
    }
    (path, pen - x)
}

/// A line of text placed by `anchor`: 0 puts its left edge at `x`, 0.5
/// its centre and 1 its right edge.
pub fn text_at(font: &Font<'_>, size: f32, text: &str, x: f32, baseline: f32, anchor: f32) -> Path {
    let (_, width) = text_path(font, size, text, 0.0, 0.0);
    text_path(font, size, text, x - width * anchor, baseline).0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_composite_with_their_coverage() {
        let mut c = Canvas::new(20, 10);
        let mut r = Rasterizer::new();
        let mut p = Path::new();
        p.rect(2.0, 2.0, 10.0, 5.5);
        c.fill_path(&mut r, &p, &Transform::IDENTITY, Blend::Over, |_, _| ([1.0, 0.5, 0.0], 1.0));
        assert_eq!(c.px[3 * 20 + 5], [1.0, 0.5, 0.0]);
        assert_eq!(c.px[3 * 20 + 15], [0.0; 3]);
        // The bottom row is half covered.
        let half = c.px[7 * 20 + 5][0];
        assert!((half - 0.5).abs() < 0.02, "{half}");
        c.fill_path(&mut r, &p, &Transform::translate(1.0, 0.0), Blend::Add, |_, _| ([0.0, 0.0, 1.0], 0.25));
        assert_eq!(c.px[3 * 20 + 5], [1.0, 0.5, 0.25]);
    }

    #[test]
    fn text_has_ink_and_width() {
        let font = Font::from_bytes(crate::INTER_SEMIBOLD).unwrap();
        let (path, width) = text_path(&font, 100.0, "10", 0.0, 100.0);
        let b = path.bounds().unwrap();
        assert!(width > 80.0 && width < 160.0, "{width}");
        // Digits stand on the baseline and are about 70% of the size tall.
        assert!((b.y1 - 100.0).abs() < 2.0 && b.y0 > 20.0 && b.y0 < 35.0, "{b:?}");
        let centred = text_at(&font, 100.0, "10", 50.0, 100.0, 0.5).bounds().unwrap();
        assert!(((centred.x0 + centred.x1) / 2.0 - 50.0).abs() < 8.0, "{centred:?}");
    }

    #[test]
    fn noise_is_smooth_and_bounded() {
        let mut last = noise1(0.0, 3);
        for i in 1..1000 {
            let v = fbm1(i as f32 * 0.01, 4, 3);
            assert!((-1.0..=1.0).contains(&v));
            let n = noise1(i as f32 * 0.01, 3);
            assert!((n - last).abs() < 0.05, "smooth");
            last = n;
        }
        assert_eq!(gradient(&[(0.0, [0.0; 3]), (1.0, [1.0; 3])], 0.5), [0.5; 3]);
    }
}
