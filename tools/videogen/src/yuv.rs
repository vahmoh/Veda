// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! RGB to Y'CbCr as the videos store it: BT.709 coefficients, limited
//! ("studio") range, 8 bits, 4:2:0, in fixed point.
//!
//! Luma is `16 + 219 (Kr R + Kg G + Kb B) / 255` with Kr = 0.2126 and
//! Kb = 0.0722; chroma is `128 + 224 (B - Y') / (2 (1 - Kb)) / 255` and
//! `128 + 224 (R - Y') / (2 (1 - Kr)) / 255`, taken from the average of
//! each 2x2 block (the edge pixel stands in for missing ones when a size is
//! odd). The coefficients are scaled by 2^16; the green ones are chosen so
//! that the sums are exact: white gives 235, black 16 and every grey 128
//! for both chroma planes.

/// A picture in 8-bit 4:2:0 Y'CbCr, planes without padding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Yuv420 {
    pub width: usize,
    pub height: usize,
    /// `width` x `height` samples.
    pub y: Vec<u8>,
    /// `ceil(width / 2)` x `ceil(height / 2)` samples each.
    pub u: Vec<u8>,
    pub v: Vec<u8>,
}

impl Yuv420 {
    /// The size of the chroma planes.
    pub fn chroma_size(&self) -> (usize, usize) {
        (self.width.div_ceil(2), self.height.div_ceil(2))
    }
}

/// Fraction bits of the coefficients.
const SHIFT: u32 = 16;

/// Luma: `0.2126 * 219 / 255`, `0.7152 * 219 / 255` and `0.0722 * 219 / 255`
/// (the sum is `219 / 255`, rounded).
const YR: i32 = 11_966;
const YG: i32 = 40_254;
const YB: i32 = 4_064;

/// Cb: `-0.2126 / 1.8556 * 224 / 255`, the rest and `0.5 * 224 / 255`.
const UR: i32 = -6_596;
const UG: i32 = -22_188;
const UB: i32 = 28_784;

/// Cr: `0.5 * 224 / 255`, the rest and `-0.0722 / 1.5748 * 224 / 255`.
const VR: i32 = 28_784;
const VG: i32 = -26_145;
const VB: i32 = -2_639;

/// The channels of a `0xAARRGGBB` pixel (alpha is ignored: pictures are opaque).
#[inline]
fn channels(p: u32) -> (i32, i32, i32) {
    (((p >> 16) & 0xFF) as i32, ((p >> 8) & 0xFF) as i32, (p & 0xFF) as i32)
}

/// Y' of one pixel.
#[inline]
pub fn luma(p: u32) -> u8 {
    let (r, g, b) = channels(p);
    ((YR * r + YG * g + YB * b + (16 << SHIFT) + (1 << (SHIFT - 1))) >> SHIFT) as u8
}

/// Cb and Cr from the sums of four pixels' channels.
#[inline]
fn chroma(r: i32, g: i32, b: i32) -> (u8, u8) {
    // The sums are four times the average: two more bits to shift.
    let s = SHIFT + 2;
    let round = (128 << s) + (1 << (s - 1));
    let u = (UR * r + UG * g + UB * b + round) >> s;
    let v = (VR * r + VG * g + VB * b + round) >> s;
    (u.clamp(0, 255) as u8, v.clamp(0, 255) as u8)
}

/// Converts `width` x `height` opaque `0xAARRGGBB` pixels (row-major).
///
/// # Panics
///
/// If `pixels` is not `width * height` long.
pub fn from_rgb(pixels: &[u32], width: usize, height: usize) -> Yuv420 {
    assert_eq!(pixels.len(), width * height, "the picture's size");
    let y = pixels.iter().map(|&p| luma(p)).collect();
    let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
    let mut u = Vec::with_capacity(cw * ch);
    let mut v = Vec::with_capacity(cw * ch);
    for cy in 0..ch {
        let (y0, y1) = (2 * cy, (2 * cy + 1).min(height - 1));
        for cx in 0..cw {
            let (x0, x1) = (2 * cx, (2 * cx + 1).min(width - 1));
            let (mut r, mut g, mut b) = (0, 0, 0);
            for (x, y) in [(x0, y0), (x1, y0), (x0, y1), (x1, y1)] {
                let (pr, pg, pb) = channels(pixels[y * width + x]);
                r += pr;
                g += pg;
                b += pb;
            }
            let (cb, cr) = chroma(r, g, b);
            u.push(cb);
            v.push(cr);
        }
    }
    Yuv420 { width, height, y, u, v }
}

/// Converts back to `0xFFRRGGBB` pixels (nearest chroma sample), for
/// previews of what the encoder made.
pub fn to_rgb(p: &Yuv420) -> Vec<u32> {
    let (cw, _) = p.chroma_size();
    let mut out = Vec::with_capacity(p.width * p.height);
    for y in 0..p.height {
        for x in 0..p.width {
            let l = (p.y[y * p.width + x] as f32 - 16.0) / 219.0;
            let c = (y / 2) * cw + x / 2;
            let cb = (p.u[c] as f32 - 128.0) / 224.0;
            let cr = (p.v[c] as f32 - 128.0) / 224.0;
            let r = l + 1.5748 * cr;
            let g = l - 0.187_324 * cb - 0.468_124 * cr;
            let b = l + 1.8556 * cb;
            let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
            out.push(0xFF00_0000 | q(r) << 16 | q(g) << 8 | q(b));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const KR: f64 = 0.2126;
    const KB: f64 = 0.0722;
    const KG: f64 = 1.0 - KR - KB;

    /// The conversion in floating point.
    fn reference(r: f64, g: f64, b: f64) -> (f64, f64, f64) {
        let y = KR * r + KG * g + KB * b;
        let cb = (b - y) / (2.0 * (1.0 - KB));
        let cr = (r - y) / (2.0 * (1.0 - KR));
        (16.0 + 219.0 * y / 255.0, 128.0 + 224.0 * cb / 255.0, 128.0 + 224.0 * cr / 255.0)
    }

    fn solid(p: u32) -> (u8, u8, u8) {
        let yuv = from_rgb(&[p; 4], 2, 2);
        (yuv.y[0], yuv.u[0], yuv.v[0])
    }

    #[test]
    fn the_coefficients_are_the_rounded_formula() {
        let scale = (1 << SHIFT) as f64;
        let near = |fixed: i32, exact: f64| (fixed as f64 - exact * scale).abs() <= 1.0;
        let (ys, cs) = (219.0 / 255.0, 224.0 / 255.0);
        assert!(near(YR, KR * ys) && near(YG, KG * ys) && near(YB, KB * ys));
        assert!(near(UR, -KR / (2.0 * (1.0 - KB)) * cs) && near(UG, -KG / (2.0 * (1.0 - KB)) * cs));
        assert!(near(UB, 0.5 * cs) && near(VR, 0.5 * cs));
        assert!(near(VG, -KG / (2.0 * (1.0 - KR)) * cs) && near(VB, -KB / (2.0 * (1.0 - KR)) * cs));
        // Greys have no colour.
        assert_eq!(UR + UG + UB, 0);
        assert_eq!(VR + VG + VB, 0);
    }

    #[test]
    fn white_black_and_the_primaries() {
        assert_eq!(solid(0xFFFF_FFFF), (235, 128, 128));
        assert_eq!(solid(0xFF00_0000), (16, 128, 128));
        // The usual BT.709 limited-range values.
        assert_eq!(solid(0xFFFF_0000), (63, 102, 240));
        assert_eq!(solid(0xFF00_FF00), (173, 42, 26));
        assert_eq!(solid(0xFF00_00FF), (32, 240, 118));
        assert_eq!(solid(0xFFFF_FF00), (219, 16, 138));
        assert_eq!(solid(0xFF00_FFFF), (188, 154, 16));
        assert_eq!(solid(0xFFFF_00FF), (78, 214, 230));
    }

    #[test]
    fn the_75_percent_bars() {
        // 191 of 255 is 75%: white bars are 180, and chroma stays within
        // the range the bars are made for.
        assert_eq!(solid(0xFFBF_BFBF), (180, 128, 128));
        assert_eq!(solid(0xFFBF_BF00), (168, 44, 136));
        assert_eq!(solid(0xFF00_BFBF), (145, 147, 44));
        assert_eq!(solid(0xFF00_BF00), (133, 63, 52));
        assert_eq!(solid(0xFFBF_00BF), (63, 193, 204));
        assert_eq!(solid(0xFFBF_0000), (51, 109, 212));
        assert_eq!(solid(0xFF00_00BF), (28, 212, 120));
    }

    #[test]
    fn greys_are_monotonic_and_colourless() {
        let mut last = 0;
        for v in 0..=255u32 {
            let (y, u, w) = solid(0xFF00_0000 | v << 16 | v << 8 | v);
            assert!(y >= last && (16..=235).contains(&y), "{v}");
            assert_eq!((u, w), (128, 128), "{v}");
            last = y;
        }
    }

    #[test]
    fn fixed_point_rounds_like_the_formula() {
        let mut s = 0x1234_5678u32;
        let mut off = 0;
        for _ in 0..100_000 {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            let p = s | 0xFF00_0000;
            let (r, g, b) = channels(p);
            let (ry, ru, rv) = reference(r as f64, g as f64, b as f64);
            let (y, u, v) = solid(p);
            for (got, want) in [(y, ry), (u, ru), (v, rv)] {
                let d = (got as f64 - want).abs();
                assert!(d <= 0.505, "{p:08x}: {got} for {want}");
                if (got as f64 - want.round()).abs() > 0.0 {
                    off += 1;
                }
            }
        }
        // Only values within a hair of .5 may round the other way.
        assert!(off < 300, "{off} values rounded differently");
    }

    #[test]
    fn chroma_is_the_average_of_each_block() {
        // Red and blue halves: the chroma of the average (purple).
        let pixels = [0xFFFF_0000, 0xFF00_00FF, 0xFFFF_0000, 0xFF00_00FF];
        let yuv = from_rgb(&pixels, 2, 2);
        assert_eq!(yuv.y, [63, 32, 63, 32]);
        let (_, ru, rv) = reference(127.5, 0.0, 127.5);
        assert_eq!((yuv.u[0], yuv.v[0]), (ru.round() as u8, rv.round() as u8));
    }

    #[test]
    fn odd_sizes_repeat_the_edge() {
        let mut pixels = vec![0xFF00_0000; 3 * 3];
        pixels[2] = 0xFFFF_FFFF;
        pixels[8] = 0xFF00_00FF;
        let yuv = from_rgb(&pixels, 3, 3);
        assert_eq!((yuv.y.len(), yuv.u.len(), yuv.v.len()), (9, 4, 4));
        assert_eq!(yuv.chroma_size(), (2, 2));
        // The right column's top block is white over black, twice.
        assert_eq!(yuv.u[1], 128);
        // The bottom right block is one blue pixel, four times.
        assert_eq!((yuv.u[3], yuv.v[3]), (240, 118));
    }

    #[test]
    fn previews_come_back_close() {
        // Gentle gradients: four pixels share their chroma.
        let pixels: Vec<u32> =
            (0..64u32).map(|i| 0xFF00_0000 | (40 + i % 8 * 8) << 16 | (40 + i / 8 * 8) << 8 | 0x40).collect();
        let back = to_rgb(&from_rgb(&pixels, 8, 8));
        for (a, b) in pixels.iter().zip(&back) {
            let (ar, ag, ab) = channels(*a);
            let (br, bg, bb) = channels(*b);
            assert!((ar - br).abs() <= 8 && (ag - bg).abs() <= 8 && (ab - bb).abs() <= 8, "{a:08x} {b:08x}");
        }
    }
}
