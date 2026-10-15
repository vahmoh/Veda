// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Residual echo suppression: a spectral gain on Hann-windowed frames of
//! the filter's error, and the output (see the module documentation of
//! [`super`]).

use num_traits::Float;

use super::{EchoCanceller, GAIN_BANDS, LEAK_GAIN, TINY};
use crate::mix::f32_to_i16;

/// Residual echo over-estimation in the suppressor, and while the near end
/// may be talking.
const OVER_SUPPRESS: f32 = 3.0;
const OVER_SUPPRESS_DT: f32 = 2.0;
/// Lowest suppressor gain (-40 dB).
const GAIN_FLOOR: f32 = 0.01;

/// The spectrum of a Hann-windowed frame from the spectrum `f` of the same
/// unwindowed (rectangular) frame: `h[k] = f[k] / 2 - (f[k - 1] + f[k + 1]) / 4`,
/// with the mirror images `f[-1] = conj f[1]` and `f[n/2 + 1] = conj f[n/2 - 1]`.
fn hann_kernel(fr: &[f32], fi: &[f32], hr: &mut [f32], hi: &mut [f32]) {
    let last = fr.len() - 1;
    hr[0] = 0.5 * (fr[0] - fr[1]);
    hi[0] = 0.0;
    for k in 1..last {
        hr[k] = 0.5 * fr[k] - 0.25 * (fr[k - 1] + fr[k + 1]);
        hi[k] = 0.5 * fi[k] - 0.25 * (fi[k - 1] + fi[k + 1]);
    }
    hr[last] = 0.5 * (fr[last] - fr[last - 1]);
    hi[last] = 0.0;
}

impl EchoCanceller {
    /// Suppresses the residual echo in the frame that ends with this block
    /// and emits the previous block of output.
    pub(super) fn suppress_and_output(&mut self, filtering: bool) {
        let (b, nk) = (self.block, self.bins);
        if !(self.suppression && filtering) {
            // The Hann-windowed overlap-add of the unmodified error.
            for j in 0..b {
                let v = self.ola[j] + self.hann[j] * self.err_prev[j];
                self.out.push(f32_to_i16(v));
                self.ola[j] = self.hann[b + j] * self.err[j];
            }
            self.gain.fill(1.0);
            self.resid.fill(0.0);
            self.rev.fill(0.0);
            return;
        }
        // Rectangular 2B-frame spectra of error (s2) and capture (s3): the
        // previous block's zero-padded spectrum moves to the front half by a
        // factor of (-1)^k.
        for k in 0..nk {
            let s = if k & 1 == 0 { 1.0 } else { -1.0 };
            self.s2_re[k] = self.e_re[k] + s * self.ep_re[k];
            self.s2_im[k] = self.e_im[k] + s * self.ep_im[k];
            self.s3_re[k] = self.d_re[k] + s * self.dp_re[k];
            self.s3_im[k] = self.d_im[k] + s * self.dp_im[k];
        }
        // Hann-windowed frames: error in s1, capture in s2.
        hann_kernel(&self.s2_re, &self.s2_im, &mut self.s1_re, &mut self.s1_im);
        hann_kernel(&self.s3_re, &self.s3_im, &mut self.s2_re, &mut self.s2_im);
        let leak = (LEAK_GAIN * self.leak).min(1.0);
        // Echo path gain per band; frame powers are 3/4 of block powers, and
        // the bound allows 6 dB more than the gain.
        let mut path = [0.0f32; GAIN_BANDS];
        if self.echo_seen {
            for (p, (&d, &x)) in path.iter_mut().zip(self.gain_d.iter().zip(&self.gain_x)) {
                *p = if x > 0.0 { 3.0 * d / x } else { 0.0 };
            }
        }
        // Late reverberation that the filter does not reproduce: the echo
        // estimate's power remembered with the room's decay, scaled by what
        // the ends of echoes have been found to leave behind.
        let decay = self.room_decay();
        let rho = self.rev_ratio;
        // Near-end speech is kept by estimating gently during double talk;
        // otherwise, everything that may be echo goes.
        let over = if self.dt || self.near_hold > 0 { OVER_SUPPRESS_DT } else { OVER_SUPPRESS };
        let (mut tail_err, mut tail_mem, mut now_py) = (0.0f32, 0.0f32, 0.0f32);
        for k in 0..nk {
            let (er, ei) = (self.s1_re[k], self.s1_im[k]);
            let (dr, di) = (self.s2_re[k], self.s2_im[k]);
            let (yr, yi) = (dr - er, di - ei);
            let pe = er * er + ei * ei;
            let py = yr * yr + yi * yi;
            // Hann frame power is 0.75 of block power; the floor estimate
            // runs low, so allow some headroom above it.
            let noise = 2.0 * self.noise[k];
            // Residual echo: the leakage of the echo estimate (follows echo
            // path changes at once); the capture reduced by the echo
            // reduction this bin achieves (also covers echo that the filter
            // has not learnt to estimate yet) as far as the far end can
            // explain that much echo; the late reverberation.
            let echo_bound = path[k * GAIN_BANDS / nk] * self.normw[k];
            let pd = dr * dr + di * di;
            let r_cap = self.inv_erle(k) * py.max(pd.min(0.25 * echo_bound));
            let mut mem = decay * self.rev[k];
            let r_rev = rho * mem;
            tail_err += (pe - noise).max(0.0);
            tail_mem += mem;
            now_py += py;
            mem += py;
            self.rev[k] = if mem < TINY { 0.0 } else { mem };
            let mut r = (leak * py).max(r_cap).max(r_rev).max(self.resid_keep * self.resid[k]);
            if r < TINY {
                r = 0.0;
            }
            self.resid[k] = r;
            let floor = if pe > noise { Float::sqrt(noise / pe).max(GAIN_FLOOR) } else { 1.0 };
            let mut g = (1.0 - over * r / (pe + 1e-20)).clamp(floor, 1.0);
            let prev = self.gain[k];
            if g > prev {
                g = prev + 0.5 * (g - prev);
            }
            self.gain[k] = g;
            self.s3_re[k] = g * er;
            self.s3_im[k] = g * ei;
        }
        // Learn the reverberation ratio where an echo has ended: the echo
        // estimate is small compared with the remembered echo, so the error is
        // mostly late reverberation.
        if self.far_active && !self.dt && self.echo_seen && tail_mem > 0.0 && now_py < 0.25 * tail_mem {
            let ratio = (tail_err / tail_mem).clamp(1e-4, 0.5);
            self.rev_ratio += self.c_rev * (ratio - self.rev_ratio);
        }
        self.fft.inverse(&self.s3_re, &self.s3_im, &mut self.time);
        for j in 0..b {
            self.out.push(f32_to_i16(self.ola[j] + self.time[j]));
            self.ola[j] = self.time[b + j];
        }
    }

    /// Per-block power decay of the room: the filter's own decay after its
    /// strongest partition, but at least that of a 0.25 s reverberation time.
    fn room_decay(&self) -> f32 {
        let top = self.w.strongest();
        let s0: f32 = self.w.energy.get(top + 1..).map_or(0.0, |s| s.iter().sum());
        let s1: f32 = self.w.energy.get(top + 2..).map_or(0.0, |s| s.iter().sum());
        let q = if s0 > 0.0 { s1 / s0 } else { 0.0 };
        q.clamp(self.min_decay, 0.95)
    }
}
