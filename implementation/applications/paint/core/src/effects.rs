// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Effects on a part of a picture: inverting its colours, grey or sepia tones, blurring and
//! sharpening, and brightness and contrast.
//!
//! Colour effects work on each pixel's straight colour (its transparency stays as it is);
//! blurring works on premultiplied pixels, so transparent ones lend no colour to their
//! neighbours. Integer arithmetic throughout, as the loops over pixels must be fast under
//! emulation.

use alloc::vec::Vec;

use vgfx::color::keep_scalar;
use vgfx::{Bitmap, Rect};

use crate::color::unpremultiply;

/// An effect.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Effect {
    /// Every colour's opposite.
    Invert,
    /// Shades of grey, by each colour's lightness.
    Grayscale,
    /// The browns of an old photograph.
    Sepia,
    /// A Gaussian-like blur of this radius (pixels).
    Blur(i32),
    /// Edges made crisper, by this amount (1 is moderate).
    Sharpen(f32),
    /// Brighter or darker (−100 to 100), more or less contrast (−100 to 100).
    BrightnessContrast(i32, i32),
}

impl Effect {
    /// Its name, as the Effects menu and Undo say it.
    pub fn name(&self) -> &'static str {
        match self {
            Effect::Invert => "Invert colours",
            Effect::Grayscale => "Black and white",
            Effect::Sepia => "Sepia",
            Effect::Blur(_) => "Blur",
            Effect::Sharpen(_) => "Sharpen",
            Effect::BrightnessContrast(..) => "Brightness and contrast",
        }
    }

    /// How far beyond the area the effect looks (blurs read their neighbours).
    pub fn reach(&self) -> i32 {
        match self {
            Effect::Blur(r) => *r * 3,
            Effect::Sharpen(_) => 3,
            _ => 0,
        }
    }
}

/// The straight-alpha pixel `s` premultiplied again.
fn premultiply(s: u32) -> u32 {
    vgfx::color::premultiply(s)
}

/// Applies `f` to the straight colour (red, green, blue, 0 to 255 each) of every pixel of `r`.
fn per_pixel(b: &mut Bitmap, r: Rect, f: impl Fn(u32, u32, u32) -> (u32, u32, u32)) {
    let w = b.width;
    for y in r.y..r.bottom() {
        for p in &mut b.pixels[(y * w + r.x) as usize..(y * w + r.right()) as usize] {
            let a = *p >> 24;
            if a != 0 {
                let s = unpremultiply(*p);
                let (red, green, blue) = f((s >> 16) & 0xFF, (s >> 8) & 0xFF, s & 0xFF);
                *p = premultiply(a << 24 | red.min(255) << 16 | green.min(255) << 8 | blue.min(255));
            }
            keep_scalar();
        }
    }
}

/// Applies `effect` to `r` of `b` (in place).
pub fn apply(b: &mut Bitmap, r: Rect, effect: Effect) {
    let r = r.intersect(&b.rect());
    if r.is_empty() {
        return;
    }
    match effect {
        Effect::Invert => per_pixel(b, r, |red, green, blue| (255 - red, 255 - green, 255 - blue)),
        Effect::Grayscale => per_pixel(b, r, |red, green, blue| {
            // Rec. 709's lightness, in 256ths.
            let l = (54 * red + 183 * green + 19 * blue + 128) >> 8;
            (l, l, l)
        }),
        Effect::Sepia => per_pixel(b, r, |red, green, blue| {
            // The usual sepia matrix, in 1024ths.
            let mix = |kr: u32, kg: u32, kb: u32| (kr * red + kg * green + kb * blue + 512) >> 10;
            (mix(402, 787, 194), mix(357, 702, 172), mix(279, 547, 134))
        }),
        Effect::BrightnessContrast(brightness, contrast) => {
            let shift = brightness.clamp(-100, 100) * 255 / 100;
            // Contrast as a factor in 256ths: from flat grey at −100 to fivefold at 100.
            let c = contrast.clamp(-100, 100);
            let factor = if c >= 0 { 256 + c * 1024 / 100 } else { 256 + c * 256 / 100 };
            let lut: Vec<u32> =
                (0..256).map(|v: i32| ((((v - 128) * factor) >> 8) + 128 + shift).clamp(0, 255) as u32).collect();
            per_pixel(b, r, |red, green, blue| (lut[red as usize], lut[green as usize], lut[blue as usize]))
        }
        Effect::Blur(radius) => {
            let radius = radius.clamp(1, 200);
            // Blurred with its surroundings, so the area's edge blends into them.
            let around = r.inflate(radius * 3).intersect(&b.rect());
            let mut part = b.crop(around);
            part.blur(radius, 3);
            copy_back(b, &part, around, r);
        }
        Effect::Sharpen(amount) => {
            let around = r.inflate(3).intersect(&b.rect());
            let original = b.crop(around);
            let mut soft = original.clone();
            soft.blur(1, 3);
            // Unsharp masking: the picture plus its difference from a blur of it.
            let k = (amount.clamp(0.0, 5.0) * 256.0) as i32;
            let mut sharp = original.clone();
            for ((o, s), d) in original.pixels.iter().zip(&soft.pixels).zip(sharp.pixels.iter_mut()) {
                let a = (o >> 24) as i32;
                let channel = |sh: u32| {
                    let (ov, sv) = (((o >> sh) & 0xFF) as i32, ((s >> sh) & 0xFF) as i32);
                    (ov + (((ov - sv) * k) >> 8)).clamp(0, a) as u32
                };
                *d = (a as u32) << 24 | channel(16) << 16 | channel(8) << 8 | channel(0);
                keep_scalar();
            }
            copy_back(b, &sharp, around, r);
        }
    }
}

/// Copies the part of `part` (which covers `around` of `b`) that is in `r` back into `b`.
fn copy_back(b: &mut Bitmap, part: &Bitmap, around: Rect, r: Rect) {
    for y in r.y..r.bottom() {
        let src = ((y - around.y) * part.width + (r.x - around.x)) as usize;
        let dst = (y * b.width + r.x) as usize;
        b.pixels[dst..dst + r.w as usize].copy_from_slice(&part.pixels[src..src + r.w as usize]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brush::filled;

    #[test]
    fn colour_effects() {
        let mut b = filled(4, 4, 0xFF20_8040);
        apply(&mut b, Rect::new(0, 0, 2, 4), Effect::Invert);
        assert_eq!(b.get(0, 0), 0xFFDF_7FBF);
        assert_eq!(b.get(3, 0), 0xFF20_8040, "outside the area");
        apply(&mut b, Rect::new(0, 0, i32::MAX, i32::MAX), Effect::Invert);
        assert_eq!(b.get(0, 0), 0xFF20_8040, "twice is nothing");
        let mut g = filled(1, 1, 0xFFFF_0000);
        apply(&mut g, Rect::new(0, 0, i32::MAX, i32::MAX), Effect::Grayscale);
        assert_eq!(g.get(0, 0), 0xFF36_3636);
        let mut s = filled(1, 1, 0xFF80_8080);
        apply(&mut s, Rect::new(0, 0, i32::MAX, i32::MAX), Effect::Sepia);
        let p = s.get(0, 0);
        assert!((p >> 16 & 0xFF) > (p >> 8 & 0xFF) && (p >> 8 & 0xFF) > (p & 0xFF), "warm: {p:08x}");
        // Transparency stays; colours under it change as their straight colours would.
        let mut t = filled(1, 1, 0x8080_0000);
        apply(&mut t, Rect::new(0, 0, i32::MAX, i32::MAX), Effect::Invert);
        assert_eq!(t.get(0, 0), 0x8000_8080);
    }

    #[test]
    fn brightness_and_contrast() {
        let mut b = filled(1, 1, 0xFF80_4020);
        apply(&mut b, Rect::new(0, 0, i32::MAX, i32::MAX), Effect::BrightnessContrast(20, 0));
        assert_eq!(b.get(0, 0), 0xFFB3_7353);
        let mut flat = filled(1, 1, 0xFF20_E080);
        apply(&mut flat, Rect::new(0, 0, i32::MAX, i32::MAX), Effect::BrightnessContrast(0, -100));
        assert_eq!(flat.get(0, 0), 0xFF80_8080, "no contrast: grey");
        let mut none = filled(1, 1, 0xFF20_E080);
        apply(&mut none, Rect::new(0, 0, i32::MAX, i32::MAX), Effect::BrightnessContrast(0, 0));
        assert_eq!(none.get(0, 0), 0xFF20_E080);
    }

    #[test]
    fn blur_and_sharpen() {
        // A white square on black: blurring softens its edge, sharpening steepens it.
        let mut b = filled(40, 40, 0xFF00_0000);
        for y in 10..30 {
            for x in 10..30 {
                b.pixels[y * 40 + x] = 0xFFFF_FFFF;
            }
        }
        let mut blurred = b.clone();
        apply(&mut blurred, Rect::new(0, 0, i32::MAX, i32::MAX), Effect::Blur(3));
        let edge = blurred.get(10, 20) & 0xFF;
        assert!(edge > 60 && edge < 230, "{edge}");
        assert_eq!(blurred.get(20, 20), 0xFFFF_FFFF);
        let mut sharpened = blurred.clone();
        apply(&mut sharpened, Rect::new(0, 0, i32::MAX, i32::MAX), Effect::Sharpen(1.0));
        assert!((sharpened.get(5, 20) & 0xFF) < (blurred.get(5, 20) & 0xFF), "darker on the dark side");
        assert!((sharpened.get(13, 20) & 0xFF) > (blurred.get(13, 20) & 0xFF), "lighter on the light side");
        // Only the area changes.
        let mut part = b.clone();
        apply(&mut part, Rect::new(0, 0, 20, 40), Effect::Blur(4));
        assert_eq!(part.get(29, 20), 0xFFFF_FFFF);
        assert_ne!(part.get(10, 20), 0xFFFF_FFFF);
    }
}
