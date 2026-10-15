// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Gains, volume curves, mixing and sample conversion.
//!
//! The mixer inside the audio service runs on every block of audio, so the
//! mixing helpers use integer arithmetic: samples are `i16`, the
//! accumulator is `i32`, and gains are Q15 fixed point (`32768` = 1.0).
//! Gain changes are ramped linearly across a block, which avoids the
//! clicks of an abrupt step.
//!
//! [`Downmix`] folds the channels of a surround file into stereo, for every
//! format the [`crate::source`]s decode.

/// Unity gain in Q15.
pub const UNITY_Q15: i32 = 1 << 15;

/// Converts a float sample in [-1, 1] to `i16` (rounded, clamped).
#[inline]
pub fn f32_to_i16(x: f32) -> i16 {
    let v = x * 32768.0;
    let v = if v >= 0.0 { v + 0.5 } else { v - 0.5 };
    (v as i32).clamp(-32768, 32767) as i16
}

/// Maps a volume slider position (0..=1) to an amplitude factor with a
/// perceptual (cubic) taper: half way is about -18 dB.
pub fn slider_to_gain(pos: f32) -> f32 {
    let p = pos.clamp(0.0, 1.0);
    p * p * p
}

/// A linear gain (clamped to 0..=1) in Q15.
#[inline]
pub fn gain_q15(gain: f32) -> i32 {
    (gain.clamp(0.0, 1.0) * UNITY_Q15 as f32 + 0.5) as i32
}

/// Adds `src` (interleaved, same layout as `acc`) scaled by a gain that
/// ramps linearly from `g0` to `g1` (Q15, 0..=32768) across the block.
/// `channels` is the number of interleaved channels (the ramp advances per
/// frame).
pub fn mix_into(acc: &mut [i32], src: &[i16], channels: usize, g0: i32, g1: i32) {
    let channels = channels.max(1);
    let frames = acc.len().min(src.len()) / channels;
    if frames == 0 {
        return;
    }
    let (g0, g1) = (g0.clamp(0, UNITY_Q15), g1.clamp(0, UNITY_Q15));
    let acc = &mut acc[..frames * channels];
    let src = &src[..frames * channels];
    if g0 == g1 {
        if g0 == 0 {
            return;
        }
        if g0 == UNITY_Q15 {
            for (a, &s) in acc.iter_mut().zip(src) {
                *a += s as i32;
            }
        } else {
            for (a, &s) in acc.iter_mut().zip(src) {
                *a += (s as i32 * g0) >> 15;
            }
        }
        return;
    }
    // 16.16 fixed-point ramp of the Q15 gain.
    let mut g = (g0 as i64) << 16;
    let step = (((g1 - g0) as i64) << 16) / frames as i64;
    for (a, s) in acc.chunks_exact_mut(channels).zip(src.chunks_exact(channels)) {
        let gi = (g >> 16) as i32;
        for (x, &y) in a.iter_mut().zip(s) {
            *x += (y as i32 * gi) >> 15;
        }
        g += step;
    }
}

/// Converts a mix accumulator to `i16` with a final Q15 gain, clamping
/// out-of-range values.
pub fn finish_mix(acc: &[i32], out: &mut [i16], gain: i32) {
    let gain = gain.clamp(0, UNITY_Q15);
    if gain == UNITY_Q15 {
        for (o, &a) in out.iter_mut().zip(acc) {
            *o = a.clamp(-32768, 32767) as i16;
        }
    } else {
        for (o, &a) in out.iter_mut().zip(acc) {
            *o = ((a as i64 * gain as i64) >> 15).clamp(-32768, 32767) as i16;
        }
    }
}

/// Converts interleaved stereo to mono (average) or mono to stereo
/// (duplicate) as needed; `out` must hold `frames * out_channels` samples.
pub fn convert_channels(src: &[i16], in_channels: usize, out: &mut [i16], out_channels: usize) -> usize {
    let (ic, oc) = (in_channels.max(1), out_channels.max(1));
    let frames = (src.len() / ic).min(out.len() / oc);
    for f in 0..frames {
        let s = &src[f * ic..f * ic + ic];
        let o = &mut out[f * oc..f * oc + oc];
        match (ic, oc) {
            (a, b) if a == b => o.copy_from_slice(s),
            (1, _) => o.fill(s[0]),
            (_, 1) => o[0] = ((s[0] as i32 + s[1] as i32) >> 1) as i16,
            _ => {
                for (c, v) in o.iter_mut().enumerate() {
                    *v = s[c.min(ic - 1)];
                }
            }
        }
    }
    frames
}

/// Folds the channels of a surround layout (the WAVE default order: FL,
/// FR, C, LFE, BL, BR, SL, SR) into stereo: each channel to the side it is
/// on, the centre (and the back of seven) to both at -3 dB, the low
/// frequency effects left out, so that a channel on its own is as loud as
/// its side.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Downmix {
    /// Left and right weights of each channel.
    weights: [(f32, f32); 8],
}

impl Downmix {
    /// The downmix of `channels` (3 to 8) channels.
    pub fn new(channels: usize) -> Downmix {
        const H: f32 = core::f32::consts::FRAC_1_SQRT_2;
        let layout: &[(f32, f32)] = match channels {
            3 => &[(1.0, 0.0), (0.0, 1.0), (H, H)],
            4 => &[(1.0, 0.0), (0.0, 1.0), (H, 0.0), (0.0, H)],
            5 => &[(1.0, 0.0), (0.0, 1.0), (H, H), (H, 0.0), (0.0, H)],
            6 => &[(1.0, 0.0), (0.0, 1.0), (H, H), (0.0, 0.0), (H, 0.0), (0.0, H)],
            7 => &[(1.0, 0.0), (0.0, 1.0), (H, H), (0.0, 0.0), (0.5, 0.5), (H, 0.0), (0.0, H)],
            _ => &[(1.0, 0.0), (0.0, 1.0), (H, H), (0.0, 0.0), (H, 0.0), (0.0, H), (H, 0.0), (0.0, H)],
        };
        let left: f32 = layout.iter().map(|p| p.0).sum();
        let norm = if left > 0.0 { 1.0 / left } else { 1.0 };
        let mut weights = [(0.0f32, 0.0f32); 8];
        for (w, &(l, r)) in weights.iter_mut().zip(layout) {
            *w = (l * norm, r * norm);
        }
        Downmix { weights }
    }

    /// One frame (its samples as floats in [-1, 1), at most eight) as a
    /// stereo pair.
    pub fn frame(&self, samples: impl Iterator<Item = f32>) -> [i16; 2] {
        let (mut l, mut r) = (0.0f32, 0.0f32);
        for (s, w) in samples.zip(&self.weights) {
            l += s * w.0;
            r += s * w.1;
        }
        [f32_to_i16(l), f32_to_i16(r)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn conversions() {
        assert_eq!(f32_to_i16(0.0), 0);
        assert_eq!(f32_to_i16(1.0), 32767);
        assert_eq!(f32_to_i16(-1.0), -32768);
        assert_eq!(f32_to_i16(-2.0), -32768);
        assert_eq!(f32_to_i16(0.5), 16384);
        assert!((slider_to_gain(0.5) - 0.125).abs() < 1e-6);
        assert_eq!(slider_to_gain(2.0), 1.0);
        assert_eq!(gain_q15(1.0), UNITY_Q15);
        assert_eq!(gain_q15(2.0), UNITY_Q15);
        assert_eq!(gain_q15(-1.0), 0);
    }

    #[test]
    fn surround_folds_into_stereo() {
        let d = Downmix::new(6);
        // Front left on its own stays left; the centre goes to both sides
        // alike; the low frequency effects are left out.
        let [l, r] = d.frame([0.5, 0.0, 0.0, 0.0, 0.0, 0.0].into_iter());
        assert!(l > 6_000 && r == 0, "{l} {r}");
        let [l, r] = d.frame([0.0, 0.0, 0.5, 0.0, 0.0, 0.0].into_iter());
        assert!(l == r && l > 4_000, "{l} {r}");
        assert_eq!(d.frame([0.0, 0.0, 0.0, 0.9, 0.0, 0.0].into_iter()), [0, 0]);
        // Full scale on every channel does not overflow.
        assert!(d.frame([1.0; 8].into_iter()).iter().all(|&s| s >= 32_000));
    }

    #[test]
    fn mixing_sums_and_clamps() {
        let mut acc = vec![0i32; 4];
        mix_into(&mut acc, &[30_000, -30_000, 100, 0], 2, UNITY_Q15, UNITY_Q15);
        mix_into(&mut acc, &[30_000, -30_000, 100, 0], 2, UNITY_Q15 / 2, UNITY_Q15 / 2);
        assert_eq!(acc, vec![45_000, -45_000, 150, 0]);
        let mut out = vec![0i16; 4];
        finish_mix(&acc, &mut out, UNITY_Q15);
        assert_eq!(out, vec![32767, -32768, 150, 0]);
        finish_mix(&acc, &mut out, UNITY_Q15 / 2);
        assert_eq!(out, vec![22_500, -22_500, 75, 0]);
    }

    #[test]
    fn gain_ramps_are_monotonic() {
        let mut acc = vec![0i32; 200];
        let src = vec![10_000i16; 200];
        mix_into(&mut acc, &src, 2, 0, UNITY_Q15);
        assert_eq!(acc[0], 0);
        assert!(acc.windows(2).all(|w| w[1] >= w[0]));
        assert!(acc[199] > 9_800);
    }

    #[test]
    fn channel_conversion() {
        let mut out = vec![0i16; 4];
        assert_eq!(convert_channels(&[1, 3, 5, 7], 2, &mut out, 1), 2);
        assert_eq!(&out[..2], &[2, 6]);
        assert_eq!(convert_channels(&[4, 8], 1, &mut out, 2), 2);
        assert_eq!(out, vec![4, 4, 8, 8]);
    }
}
