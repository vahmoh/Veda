// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Colors and pixel arithmetic.
//!
//! API colors ([`Color`]) use straight alpha. Pixels in surfaces are
//! premultiplied `0xAARRGGBB`; [`Color::premul`] converts between the two.
//!
//! # Keeping the blending loops scalar
//!
//! A pixel's four channels are worked on together in one `u64`, each in a
//! 16-bit lane of its own (`spread`), so that scaling a pixel takes one
//! multiplication. Loops over pixels must stay scalar: Veda usually runs
//! under QEMU's TCG, which emulates SSE's integer multiplications and
//! shuffles (`pmuludq`, `pshufd`, `punpck*`) with slow helper calls, so a
//! loop that LLVM vectorises becomes several times slower than its scalar
//! form there. A colour filling a run goes through [`blend_run`], whose
//! cache of the last pixel blended (it saves the arithmetic over a flat
//! background) makes each step depend on the one before; every other loop
//! over pixels calls [`keep_scalar`] in its body.
//!
//! To check the generated code, build a program that draws and list the
//! functions of `gfx` that use those instructions:
//!
//! ```text
//! cargo build --release --target x86_64-unknown-linux-musl -p compositor
//! objdump -d --no-show-raw-insn -C target/x86_64-unknown-linux-musl/release/compositor \
//!   | awk '/^[0-9a-f]+ <.*>:$/ { f = $0 } /pmuludq|pshufd|punpck/ && f ~ /gfx/ { print f }' | sort | uniq -c
//! ```
//!
//! What it lists may only use them outside its loops: a `pshufd` that
//! copies a colour into a register once, for `fill` to store four pixels at
//! a time, and the comparisons of rectangles.

/// A straight-alpha `0xAARRGGBB` color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Color(pub u32);

impl Color {
    pub const TRANSPARENT: Color = Color(0);
    pub const BLACK: Color = Color(0xFF00_0000);
    pub const WHITE: Color = Color(0xFFFF_FFFF);

    pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
        Color(0xFF00_0000 | (r as u32) << 16 | (g as u32) << 8 | b as u32)
    }

    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color {
        Color((a as u32) << 24 | (r as u32) << 16 | (g as u32) << 8 | b as u32)
    }

    /// `0xRRGGBB` with full opacity.
    pub const fn hex(rgb: u32) -> Color {
        Color(0xFF00_0000 | (rgb & 0xFF_FFFF))
    }

    pub const fn a(self) -> u8 {
        (self.0 >> 24) as u8
    }
    pub const fn r(self) -> u8 {
        (self.0 >> 16) as u8
    }
    pub const fn g(self) -> u8 {
        (self.0 >> 8) as u8
    }
    pub const fn b(self) -> u8 {
        self.0 as u8
    }

    /// The same color with alpha `a`.
    pub const fn with_alpha(self, a: u8) -> Color {
        Color((self.0 & 0x00FF_FFFF) | (a as u32) << 24)
    }

    /// Multiplies the alpha by `f` (0.0 ..= 1.0).
    pub fn fade(self, f: f32) -> Color {
        let a = (self.a() as f32 * f.clamp(0.0, 1.0) + 0.5) as u8;
        self.with_alpha(a)
    }

    /// Linear interpolation between two colors (in sRGB space).
    pub fn lerp(self, other: Color, t: f32) -> Color {
        let t = (t.clamp(0.0, 1.0) * 256.0) as u32;
        let mix = |s: u32| -> u32 {
            let (x, y) = ((self.0 >> s) & 0xFF, (other.0 >> s) & 0xFF);
            ((x * (256 - t) + y * t) >> 8) << s
        };
        Color(mix(24) | mix(16) | mix(8) | mix(0))
    }

    /// Lighter (`amount` > 0) or darker (`amount` < 0) version.
    pub fn shade(self, amount: f32) -> Color {
        if amount >= 0.0 {
            self.lerp(Color::WHITE.with_alpha(self.a()), amount)
        } else {
            self.lerp(Color::BLACK.with_alpha(self.a()), -amount)
        }
    }

    /// Premultiplied pixel value.
    pub fn premul(self) -> u32 {
        premultiply(self.0)
    }
}

/// Keeps the loop it is called in scalar (see the [module
/// documentation](self)): LLVM vectorises no loop with an assembly statement
/// in it, and this one is empty, so it costs nothing.
#[inline(always)]
pub fn keep_scalar() {
    // SAFETY: an empty assembly statement that touches no memory, stack or
    // flags does nothing.
    unsafe { core::arch::asm!("", options(nomem, nostack, preserves_flags)) }
}

/// The low byte of each 16-bit lane of a `u64`.
const LANES: u64 = 0x00FF_00FF_00FF_00FF;

/// The channels of `px` in the 16-bit lanes of a `u64`: blue, red, green
/// and alpha from the bottom, each with eight bits of room above it.
#[inline(always)]
fn spread(px: u32) -> u64 {
    let x = px as u64;
    (x & 0x00FF_00FF) | ((x & 0xFF00_FF00) << 24)
}

/// The pixel whose channels are the low bytes of the lanes of `lanes`
/// (the inverse of [`spread`]).
#[inline(always)]
fn gather(lanes: u64) -> u32 {
    (lanes as u32 & 0x00FF_00FF) | ((lanes >> 24) as u32 & 0xFF00_FF00)
}

/// Every lane (at most 255) times `a / 255` (`a` at most 255), rounded to
/// the nearest: `(v + 128 + ((v + 128) >> 8)) >> 8` of each lane's product
/// `v`, which is exact for all of them.
#[inline(always)]
fn mul_div255(lanes: u64, a: u32) -> u64 {
    let t = lanes * a as u64 + 0x0080_0080_0080_0080;
    ((t + ((t >> 8) & LANES)) >> 8) & LANES
}

/// Scales all four channels of a pixel by `a / 255` (`a` at most 255).
#[inline(always)]
pub fn scale(px: u32, a: u32) -> u32 {
    gather(mul_div255(spread(px), a))
}

/// Source-over compositing of premultiplied pixels.
#[inline(always)]
pub fn over(src: u32, dst: u32) -> u32 {
    let sa = src >> 24;
    if sa == 255 {
        return src;
    }
    if src == 0 {
        return dst;
    }
    src.wrapping_add(scale(dst, 255 - sa))
}

/// Composites the premultiplied pixel `px` over every pixel of `row`.
///
/// One colour scales every pixel by the same factor, which LLVM would
/// vectorise (see the [module documentation](self)). Instead, the last
/// pixel blended and its result are kept: a run of equal pixels (a flat
/// background) is blended once, and each step depends on the one before,
/// so the loop stays scalar.
pub fn blend_run(row: &mut [u32], px: u32) {
    let alpha = px >> 24;
    if alpha == 255 {
        row.fill(px);
        return;
    }
    if px == 0 {
        return;
    }
    let keep = 255 - alpha;
    let Some(&first) = row.first() else { return };
    // A pixel no step sees first, so that the first one is blended.
    let (mut last, mut blended) = (!first, 0);
    for d in row {
        if *d != last {
            last = *d;
            blended = px.wrapping_add(scale(last, keep));
        }
        *d = blended;
    }
}

/// Straight → premultiplied alpha.
#[inline]
pub fn premultiply(px: u32) -> u32 {
    let a = px >> 24;
    match a {
        255 => px,
        0 => 0,
        _ => (scale(px, a) & 0x00FF_FFFF) | (a << 24),
    }
}

/// Linear interpolation between two premultiplied pixels: `t` parts of 256
/// of `b` (`t` in 0..=256), each channel rounded down.
#[inline(always)]
pub fn lerp_px(a: u32, b: u32, t: u32) -> u32 {
    gather(((spread(a) * (256 - t) as u64 + spread(b) * t as u64) >> 8) & LANES)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Channel `s` (bit offset) of `px`.
    fn ch(px: u32, s: u32) -> u32 {
        (px >> s) & 0xFF
    }

    #[test]
    fn blending_is_exact_at_extremes() {
        assert_eq!(over(0xFF11_2233, 0xFF44_5566), 0xFF11_2233);
        assert_eq!(over(0, 0xFF44_5566), 0xFF44_5566);
        // 50% black over white is mid grey.
        let half_black = premultiply(0x8000_0000);
        let r = over(half_black, 0xFFFF_FFFF);
        assert_eq!(r >> 24, 0xFF);
        assert!(((r >> 16) & 0xFF).abs_diff(127) <= 1);
    }

    #[test]
    fn premultiplying_rounds_each_channel() {
        for a in 0..=255u32 {
            for c in [0u32, 1, 2, 77, 128, 200, 254, 255] {
                let px = a << 24 | c << 16 | (255 - c) << 8 | (c / 2);
                let p = premultiply(px);
                assert_eq!(p >> 24, a);
                for s in [0, 8, 16] {
                    assert_eq!(ch(p, s), (ch(px, s) * a + 127) / 255, "{px:08x} -> {p:08x}");
                }
            }
        }
    }

    #[test]
    fn scale_rounds_every_channel_to_the_nearest() {
        for a in 0..=255u32 {
            for c in 0..=255u32 {
                // Every channel differs, so that lanes cannot leak into each other unseen.
                let px = c << 24 | (255 - c) << 16 | (c ^ 0x5A) << 8 | (c / 3);
                let got = scale(px, a);
                for s in [0, 8, 16, 24] {
                    assert_eq!(ch(got, s), (ch(px, s) * a + 127) / 255, "c={c} a={a} channel {s}");
                }
            }
        }
    }

    #[test]
    fn lerp_mixes_every_channel() {
        let (a, b) = (0x80FF_4010u32, 0xFF00_C0F0u32);
        for t in 0..=256u32 {
            let got = lerp_px(a, b, t);
            for s in [0, 8, 16, 24] {
                assert_eq!(ch(got, s), (ch(a, s) * (256 - t) + ch(b, s) * t) >> 8, "t={t} channel {s}");
            }
        }
    }

    #[test]
    fn runs_blend_like_single_pixels() {
        let row: [u32; 9] = [0xFF10_2030, 0xFF10_2030, 0, 0x8040_4040, 0x8040_4040, 0xFFFF_FFFF, 0, 0, 0x0102_0304];
        for px in [0x8000_0000u32, premultiply(0x40FF_8000), 0xFF12_3456, 0, 0x0101_0101] {
            let mut run = row;
            blend_run(&mut run, px);
            let want = row.map(|d| over(px, d));
            assert_eq!(run, want, "{px:08x}");
        }
        blend_run(&mut [], 0x8000_0000);
    }
}
