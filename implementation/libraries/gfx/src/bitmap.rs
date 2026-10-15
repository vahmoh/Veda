// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Owned pixel images, and the arithmetic that scales and blurs them:
//! integers only, rows read in order, each pixel's channels summed and
//! divided by multiplying with a reciprocal (a division per pixel is slow
//! under CPU emulation).

use alloc::vec;
use alloc::vec::Vec;

use crate::canvas::{Canvas, Filter, Picture};
use crate::color::{keep_scalar, premultiply};
use crate::geom::Rect;

/// The largest radius [`Bitmap::blur`] takes: a larger one blurs as this
/// one does (a window of 131071 pixels, far beyond any picture's).
pub const MAX_BLUR_RADIUS: i32 = 65535;

/// An owned image of premultiplied `0xAARRGGBB` pixels.
#[derive(Clone, Default)]
pub struct Bitmap {
    pub width: i32,
    pub height: i32,
    pub pixels: Vec<u32>,
}

impl core::fmt::Debug for Bitmap {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Bitmap({}x{})", self.width, self.height)
    }
}

impl Bitmap {
    pub fn new(width: i32, height: i32) -> Bitmap {
        let (w, h) = (width.max(0), height.max(0));
        Bitmap { width: w, height: h, pixels: vec![0; (w * h) as usize] }
    }

    pub fn filled(width: i32, height: i32, px: u32) -> Bitmap {
        let mut b = Bitmap::new(width, height);
        b.pixels.fill(px);
        b
    }

    /// Wraps straight-alpha pixels (as produced by image decoders).
    pub fn from_straight(width: i32, height: i32, mut pixels: Vec<u32>) -> Bitmap {
        for p in pixels.iter_mut() {
            *p = premultiply(*p);
        }
        Bitmap { width, height, pixels }
    }

    pub fn rect(&self) -> Rect {
        Rect::new(0, 0, self.width, self.height)
    }

    /// The pixels as a [`Picture`] to draw or scale (none if they do not
    /// fill the bitmap's size).
    pub fn picture(&self) -> Picture<'_> {
        Picture::new(&self.pixels, self.width, self.height, self.width.max(0) as usize).unwrap_or(Picture::EMPTY)
    }

    #[inline]
    pub fn get(&self, x: i32, y: i32) -> u32 {
        if x < 0 || y < 0 || x >= self.width || y >= self.height {
            return 0;
        }
        self.pixels[(y * self.width + x) as usize]
    }

    /// High-quality resize: area averaging when shrinking ([`downscale`]),
    /// bilinear when growing.
    pub fn resized(&self, w: i32, h: i32) -> Bitmap {
        if self.width == 0 || self.height == 0 || w <= 0 || h <= 0 {
            return Bitmap::new(w, h);
        }
        if w <= self.width && h <= self.height {
            return downscale(self.picture(), w, h);
        }
        let mut out = Bitmap::new(w, h);
        Canvas::for_bitmap(&mut out).draw_bitmap_scaled(self, Rect::new(0, 0, w, h), Filter::Bilinear, 255);
        out
    }

    /// Scales to cover `w` x `h` (cropping the overflow, keeping the aspect).
    pub fn cover(&self, w: i32, h: i32) -> Bitmap {
        if self.width == 0 || self.height == 0 {
            return Bitmap::new(w, h);
        }
        let scale = (w as f32 / self.width as f32).max(h as f32 / self.height as f32);
        let sw = ((self.width as f32 * scale) as i32).max(w);
        let sh = ((self.height as f32 * scale) as i32).max(h);
        let scaled = self.resized(sw, sh);
        scaled.crop(Rect::new((sw - w) / 2, (sh - h) / 2, w, h))
    }

    pub fn crop(&self, r: Rect) -> Bitmap {
        let r = r.intersect(&self.rect());
        let mut out = Bitmap::new(r.w, r.h);
        for y in 0..r.h {
            let src = ((r.y + y) * self.width + r.x) as usize;
            out.pixels[(y * r.w) as usize..((y + 1) * r.w) as usize]
                .copy_from_slice(&self.pixels[src..src + r.w as usize]);
        }
        out
    }

    /// Separable box blur, repeated `passes` times (3 passes ≈ Gaussian).
    /// Radii above [`MAX_BLUR_RADIUS`] blur as that one does.
    pub fn blur(&mut self, radius: i32, passes: u32) {
        if radius <= 0
            || self.width == 0
            || self.height == 0
            || self.pixels.len() != (self.width * self.height) as usize
        {
            return;
        }
        let r = radius.min(MAX_BLUR_RADIUS);
        let divisor = Divisor::new((2 * r + 1) as u64);
        let mut tmp = vec![0u32; self.pixels.len()];
        for _ in 0..passes {
            blur_rows(&self.pixels, &mut tmp, self.width as usize, r, divisor);
            blur_columns(&tmp, &mut self.pixels, self.width as usize, self.height as usize, r, divisor);
        }
    }
}

/// Exact division of sums of channels by one divisor `d`, as a
/// multiplication: `floor(v / d) == (v * m) >> SHIFT` with
/// `m = ceil(2^SHIFT / d)` for every `v` up to `255 d`, as long as
/// `255 d² < 2^SHIFT` (the error `v (m - 2^SHIFT / d) / 2^SHIFT` then stays
/// below `1 / d`) and `255 · 2^SHIFT` fits in 64 bits, which holds for
/// `d` up to 2^24.
#[derive(Clone, Copy)]
struct Divisor {
    d: u64,
    m: u64,
}

impl Divisor {
    const SHIFT: u32 = 56;
    /// The largest divisor the multiplication is exact for; larger ones
    /// divide.
    const LARGEST: u64 = 1 << 24;

    fn new(d: u64) -> Divisor {
        let d = d.max(1);
        Divisor { d, m: (1u64 << Self::SHIFT).div_ceil(d) }
    }

    /// `v / d`, rounded down (`v` at most `255 d`).
    #[inline(always)]
    fn divide(self, v: u64) -> u64 {
        if self.d <= Self::LARGEST { (v * self.m) >> Self::SHIFT } else { v / self.d }
    }
}

/// The sums of the four channels of pixels, alpha first.
type Sums = [u64; 4];

#[inline(always)]
fn add(sums: &mut Sums, p: u32) {
    sums[0] += (p >> 24) as u64;
    sums[1] += ((p >> 16) & 0xFF) as u64;
    sums[2] += ((p >> 8) & 0xFF) as u64;
    sums[3] += (p & 0xFF) as u64;
}

#[inline(always)]
fn sub(sums: &mut Sums, p: u32) {
    sums[0] -= (p >> 24) as u64;
    sums[1] -= ((p >> 16) & 0xFF) as u64;
    sums[2] -= ((p >> 8) & 0xFF) as u64;
    sums[3] -= (p & 0xFF) as u64;
}

/// The pixel whose channels are `sums` divided by `divisor`.
#[inline(always)]
fn average(sums: &Sums, divisor: Divisor) -> u32 {
    let c = |i: usize| divisor.divide(sums[i]) as u32;
    c(0) << 24 | c(1) << 16 | c(2) << 8 | c(3)
}

/// One horizontal box-blur pass: each pixel of `dst` the average of the
/// `2r + 1` pixels of `src` around it in its row, the row's ends repeated.
fn blur_rows(src: &[u32], dst: &mut [u32], width: usize, r: i32, divisor: Divisor) {
    let last = width as i32 - 1;
    for (from, to) in src.chunks_exact(width).zip(dst.chunks_exact_mut(width)) {
        let at = |i: i32| from[i.clamp(0, last) as usize];
        let mut sums = [0; 4];
        for i in -r..=r {
            add(&mut sums, at(i));
        }
        for (i, out) in (0..).zip(to.iter_mut()) {
            *out = average(&sums, divisor);
            add(&mut sums, at(i + r + 1));
            sub(&mut sums, at(i - r));
            keep_scalar();
        }
    }
}

/// One vertical box-blur pass, as [`blur_rows`] down the columns; a row of
/// sums is carried down, so that rows are read in order.
fn blur_columns(src: &[u32], dst: &mut [u32], width: usize, height: usize, r: i32, divisor: Divisor) {
    let last = height as i32 - 1;
    let row = |i: i32| &src[i.clamp(0, last) as usize * width..][..width];
    let mut sums = vec![[0u64; 4]; width];
    for i in -r..=r {
        for (s, &p) in sums.iter_mut().zip(row(i)) {
            add(s, p);
            keep_scalar();
        }
    }
    for (y, to) in (0..).zip(dst.chunks_exact_mut(width)) {
        for (out, s) in to.iter_mut().zip(&sums) {
            *out = average(s, divisor);
            keep_scalar();
        }
        for ((s, &a), &b) in sums.iter_mut().zip(row(y + r + 1)).zip(row(y - r)) {
            add(s, a);
            sub(s, b);
            keep_scalar();
        }
    }
}

/// Shrinks `picture` to `w` x `h` (no larger than it: [`Bitmap::resized`]
/// grows pictures): each pixel the average of the block of the picture's
/// pixels it covers. Integer arithmetic throughout, the picture's rows read
/// once, in order, and the divisions done as multiplications.
pub fn downscale(picture: Picture<'_>, w: i32, h: i32) -> Bitmap {
    let (sw, sh) = (picture.width(), picture.height());
    let (w, h) = (w.min(sw), h.min(sh));
    let mut out = Bitmap::new(w, h);
    if w <= 0 || h <= 0 {
        return out;
    }
    // The blocks: output column `x` covers columns `cols[x]..cols[x + 1]`,
    // row `y` rows `rows[y]..rows[y + 1]` (no fewer than one each).
    let edges = |n: i32, of: i32| -> Vec<i32> { (0..=n).map(|i| (i as i64 * of as i64 / n as i64) as i32).collect() };
    let (cols, rows) = (edges(w, sw), edges(h, sh));
    let mut sums = vec![[0u64; 4]; w as usize];
    let mut known = [Divisor::new(1); 4];
    for (y, out_row) in out.pixels.chunks_exact_mut(w as usize).enumerate() {
        sums.fill([0; 4]);
        for sy in rows[y]..rows[y + 1] {
            let src = picture.row(sy);
            for (x, s) in sums.iter_mut().enumerate() {
                for &p in &src[cols[x] as usize..cols[x + 1] as usize] {
                    add(s, p);
                    keep_scalar();
                }
            }
        }
        let height = (rows[y + 1] - rows[y]) as u64;
        for (x, (out, s)) in out_row.iter_mut().zip(&sums).enumerate() {
            let n = (cols[x + 1] - cols[x]) as u64 * height;
            // The blocks are of two widths and two heights at most: their
            // divisors are kept at hand.
            let divisor = match known.iter().position(|k| k.d == n) {
                Some(i) => known[i],
                None => {
                    known.rotate_right(1);
                    known[0] = Divisor::new(n);
                    known[0]
                }
            };
            *out = average(s, divisor);
            keep_scalar();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pixels that differ in every channel.
    fn varied(w: i32, h: i32) -> Bitmap {
        let mut b = Bitmap::new(w, h);
        for (i, p) in (0u32..).zip(b.pixels.iter_mut()) {
            let a = 0x80 + (i * 7 % 128);
            *p = premultiply(a << 24 | (i * 37 % 256) << 16 | (i * 11 % 256) << 8 | (i * 3 % 256));
        }
        b
    }

    #[test]
    fn divisions_by_multiplication_are_exact() {
        for d in (1..2000u64).chain([65535, 131071, 1 << 20, (1 << 24) - 1, 1 << 24, (1 << 24) + 1, 1 << 31]) {
            let divisor = Divisor::new(d);
            for v in [0, 1, d - 1, d, d + 1, 127 * d + d / 2, 255 * d - 1, 255 * d] {
                assert_eq!(divisor.divide(v), v / d, "{v} / {d}");
            }
        }
    }

    #[test]
    fn blurring_averages_the_window_around_each_pixel() {
        let b = varied(13, 9);
        for r in [1, 2, 5, 40] {
            let mut blurred = b.clone();
            blurred.blur(r, 1);
            // The picture's ends repeat beyond it.
            let at = |x: i32, y: i32, sh: u32| (b.pixels[(y.clamp(0, 8) * 13 + x.clamp(0, 12)) as usize] >> sh) & 0xFF;
            let div = (2 * r + 1) as u32;
            // Along the rows first, rounded down, then down the columns.
            let across = |x: i32, y: i32, sh: u32| (-r..=r).map(|i| at(x + i, y, sh)).sum::<u32>() / div;
            for y in 0..9 {
                for x in 0..13 {
                    let channel = |sh: u32| (-r..=r).map(|j| across(x, (y + j).clamp(0, 8), sh)).sum::<u32>() / div;
                    let want = [24, 16, 8, 0].iter().fold(0, |a, &sh| a | channel(sh) << sh);
                    assert_eq!(blurred.pixels[(y * 13 + x) as usize], want, "radius {r} at ({x}, {y})");
                }
            }
        }
    }

    #[test]
    fn downscaling_averages_blocks_of_strided_pictures() {
        let b = varied(17, 11);
        // The same pixels with eight more to a row, which must not show.
        let mut wide = vec![0xFFFF_FFFFu32; 25 * 11];
        for y in 0..11 {
            wide[y * 25..y * 25 + 17].copy_from_slice(&b.pixels[y * 17..y * 17 + 17]);
        }
        let picture = Picture::new(&wide, 17, 11, 25).unwrap();
        for (w, h) in [(17, 11), (5, 3), (16, 11), (1, 1), (8, 10)] {
            let small = downscale(picture, w, h);
            assert_eq!((small.width, small.height), (w, h));
            for y in 0..h {
                let (y0, y1) = (y * 11 / h, (y + 1) * 11 / h);
                for x in 0..w {
                    let (x0, x1) = (x * 17 / w, (x + 1) * 17 / w);
                    let n = ((y1 - y0) * (x1 - x0)) as u32;
                    let sum = |sh: u32| -> u32 {
                        (y0..y1)
                            .flat_map(|yy| (x0..x1).map(move |xx| (yy, xx)))
                            .map(|(yy, xx)| (b.get(xx, yy) >> sh) & 0xFF)
                            .sum()
                    };
                    let want = [24, 16, 8, 0].iter().fold(0, |a, &sh| a | (sum(sh) / n) << sh);
                    assert_eq!(small.pixels[(y * w + x) as usize], want, "{w}x{h} at ({x}, {y})");
                }
            }
        }
        // Not larger than the picture, nor less than nothing.
        assert_eq!((downscale(picture, 40, 3).width, downscale(picture, 40, 3).height), (17, 3));
        assert!(downscale(picture, -1, 3).pixels.is_empty());
        assert!(downscale(Picture::EMPTY, 4, 4).pixels.is_empty());
    }
}
