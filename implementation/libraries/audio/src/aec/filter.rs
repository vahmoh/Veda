// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The linear filters: partitioned-block frequency-domain adaptive filters,
//! the main one with a controlled step and the shadow with a fixed one, and
//! how the two are compared (see the module documentation of [`super`]).

use alloc::vec;
use alloc::vec::Vec;

use num_traits::Float;

use super::{EchoCanceller, LEAK_GAIN, energy};
use crate::fft::RealFft;

/// Largest step of the main filter, and the shadow filter's step (both
/// normalised).
const MU_MAX: f32 = 0.8;
const MU_SHADOW: f32 = 0.9;
/// Regularisation of the normalised steps relative to the far end's average
/// bin power (-15 dB): bins with less far-end sound learn more slowly, as
/// their updates are mostly noise.
const REG_REL: f32 = 0.03;
/// Share of the correction distributed by the partitions' share of the
/// filter (the rest follows the prior).
const PROPORTION: f32 = 0.5;

/// `acc += a * b` for complex vectors in split form.
#[inline]
fn cmac(acc_re: &mut [f32], acc_im: &mut [f32], a_re: &[f32], a_im: &[f32], b_re: &[f32], b_im: &[f32]) {
    let n = acc_re.len();
    let (acc_im, a_re, a_im, b_re, b_im) = (&mut acc_im[..n], &a_re[..n], &a_im[..n], &b_re[..n], &b_im[..n]);
    for k in 0..n {
        acc_re[k] += a_re[k] * b_re[k] - a_im[k] * b_im[k];
        acc_im[k] += a_re[k] * b_im[k] + a_im[k] * b_re[k];
    }
}

/// `acc += g * conj(a) * b` for complex vectors in split form.
#[inline]
fn cmac_conj(acc_re: &mut [f32], acc_im: &mut [f32], a_re: &[f32], a_im: &[f32], b_re: &[f32], b_im: &[f32], g: f32) {
    let n = acc_re.len();
    let (acc_im, a_re, a_im, b_re, b_im) = (&mut acc_im[..n], &a_re[..n], &a_im[..n], &b_re[..n], &b_im[..n]);
    for k in 0..n {
        acc_re[k] += g * (a_re[k] * b_re[k] + a_im[k] * b_im[k]);
        acc_im[k] += g * (a_re[k] * b_im[k] - a_im[k] * b_re[k]);
    }
}

/// The coefficients of a partitioned frequency-domain filter (P spectra of
/// B + 1 bins) and what is known of them per partition: the energy, largest
/// tap and its position of the impulse response (refreshed as the gradient
/// constraint visits the partitions in turn) and the spectral energy (a
/// quarter refreshed per block).
pub(super) struct Partitions {
    re: Vec<f32>,
    im: Vec<f32>,
    next_constrain: usize,
    /// Per partition, as the constraint last saw its impulse response: its
    /// energy, its largest tap's magnitude and where that is (in samples
    /// from the partition's start).
    pub(super) energy: Vec<f32>,
    pub(super) peak: Vec<f32>,
    pub(super) peak_at: Vec<u32>,
    /// Partitions visited since the last reset or realignment.
    pub(super) scanned: usize,
    norm: Vec<f32>,
}

impl Partitions {
    /// The filter's coefficients: real and imaginary parts, partition after
    /// partition (for the tests).
    #[cfg(test)]
    pub(super) fn coefficients(&self) -> (&[f32], &[f32]) {
        (&self.re, &self.im)
    }

    pub(super) fn new(parts: usize, bins: usize) -> Partitions {
        Partitions {
            re: vec![0.0; parts * bins],
            im: vec![0.0; parts * bins],
            next_constrain: 0,
            energy: vec![0.0; parts],
            peak: vec![0.0; parts],
            peak_at: vec![0; parts],
            scanned: 0,
            norm: vec![0.0; parts],
        }
    }

    pub(super) fn parts(&self) -> usize {
        self.energy.len()
    }

    pub(super) fn clear(&mut self) {
        self.re.fill(0.0);
        self.im.fill(0.0);
        self.next_constrain = 0;
        self.energy.fill(0.0);
        self.peak.fill(0.0);
        self.peak_at.fill(0);
        self.scanned = 0;
        self.norm.fill(0.0);
    }

    /// Takes the coefficients of `other` (no allocation).
    pub(super) fn copy_from(&mut self, other: &Partitions) {
        self.re.copy_from_slice(&other.re);
        self.im.copy_from_slice(&other.im);
        self.next_constrain = other.next_constrain;
        self.energy.copy_from_slice(&other.energy);
        self.peak.copy_from_slice(&other.peak);
        self.peak_at.copy_from_slice(&other.peak_at);
        self.scanned = other.scanned;
        self.norm.copy_from_slice(&other.norm);
    }

    /// The partition whose impulse response holds the most energy.
    pub(super) fn strongest(&self) -> usize {
        let mut top = 0;
        for (p, &e) in self.energy.iter().enumerate() {
            if e > self.energy[top] {
                top = p;
            }
        }
        top
    }

    /// Recomputes the spectral energy of every fourth partition, starting
    /// with `first`.
    pub(super) fn refresh_norms(&mut self, first: usize) {
        let nk = self.re.len() / self.parts();
        for p in (first..self.parts()).step_by(4) {
            self.norm[p] = energy(&self.re[p * nk..(p + 1) * nk], &self.im[p * nk..(p + 1) * nk]);
        }
    }

    /// Proportionate weights (summing to 1): half the prior, half the
    /// partitions' shares of the filter (by the square root of their
    /// spectral energy).
    pub(super) fn weights(&self, prior: &[f32], out: &mut [f32]) {
        let total: f32 = self.norm.iter().map(|&v| Float::sqrt(v)).sum();
        for ((g, &v), &prior) in out.iter_mut().zip(&self.norm).zip(prior) {
            let share = if total > 0.0 { Float::sqrt(v) / total } else { prior };
            *g = (1.0 - PROPORTION) * prior + PROPORTION * share;
        }
    }

    /// Moves every partition `d` places towards the front (it takes what
    /// partition `p + d` held; zeros where there is no such partition), so
    /// that the response stays in place when the bulk delay grows by `d`
    /// blocks.
    pub(super) fn shift(&mut self, d: isize) {
        let np = self.parts();
        // (In an order that reads every partition before it is overwritten.)
        if d > 0 {
            for p in 0..np {
                self.take(p, p as isize + d);
            }
        } else {
            for p in (0..np).rev() {
                self.take(p, p as isize + d);
            }
        }
        self.scanned = 0;
    }

    /// Partition `p` takes partition `src`'s coefficients, or zeros if there
    /// is no such partition.
    pub(super) fn take(&mut self, p: usize, src: isize) {
        let nk = self.re.len() / self.parts();
        let dst = p * nk;
        if let Some(src) = usize::try_from(src).ok().filter(|&s| s < self.parts()) {
            self.re.copy_within(src * nk..(src + 1) * nk, dst);
            self.im.copy_within(src * nk..(src + 1) * nk, dst);
            self.energy[p] = self.energy[src];
            self.peak[p] = self.peak[src];
            self.peak_at[p] = self.peak_at[src];
            self.norm[p] = self.norm[src];
        } else {
            self.re[dst..dst + nk].fill(0.0);
            self.im[dst..dst + nk].fill(0.0);
            self.energy[p] = 0.0;
            self.peak[p] = 0.0;
            self.norm[p] = 0.0;
        }
    }

    /// Applies the gradient constraint to the next partition in turn (its
    /// impulse response keeps only the causal half) and records the
    /// response's energy and largest tap.
    pub(super) fn constrain_next(&mut self, fft: &mut RealFft, time: &mut [f32]) {
        let np = self.parts();
        let nk = self.re.len() / np;
        let b = nk - 1;
        let p = self.next_constrain;
        self.next_constrain = (p + 1) % np;
        let w = p * nk;
        fft.inverse(&self.re[w..w + nk], &self.im[w..w + nk], time);
        let (mut energy, mut peak, mut at) = (0.0f32, 0.0f32, 0usize);
        for (j, &v) in time[..b].iter().enumerate() {
            energy += v * v;
            if v.abs() > peak {
                peak = v.abs();
                at = j;
            }
        }
        time[b..].fill(0.0);
        fft.forward(time, &mut self.re[w..w + nk], &mut self.im[w..w + nk]);
        self.energy[p] = energy;
        self.peak[p] = peak;
        self.peak_at[p] = at as u32;
        self.scanned = (self.scanned + 1).min(np);
    }
}

/// The error of the filter `f` for this block: the capture `cap` minus the
/// echo estimate (partition `p` times the reference spectrum in history slot
/// `slots[p]`, overlap-save), in time (`err`) and as the spectrum of the
/// block zero-padded in front (`er`, `ei`). `acc` and `time` are scratch.
pub(super) fn filter_error(
    fft: &mut RealFft,
    f: &Partitions,
    x_re: &[f32],
    x_im: &[f32],
    slots: &[usize],
    cap: &[f32],
    acc_re: &mut [f32],
    acc_im: &mut [f32],
    time: &mut [f32],
    err: &mut [f32],
    er: &mut [f32],
    ei: &mut [f32],
) {
    let nk = acc_re.len();
    let b = nk - 1;
    acc_re.fill(0.0);
    acc_im.fill(0.0);
    for (p, &slot) in slots[..f.parts()].iter().enumerate() {
        let (s, w) = (slot * nk, p * nk);
        cmac(acc_re, acc_im, &f.re[w..w + nk], &f.im[w..w + nk], &x_re[s..s + nk], &x_im[s..s + nk]);
    }
    fft.inverse(acc_re, acc_im, time);
    for ((e, &c), &y) in err.iter_mut().zip(cap).zip(&time[b..]) {
        *e = c - y;
    }
    time[..b].fill(0.0);
    time[b..].copy_from_slice(err);
    fft.forward(time, er, ei);
}

impl EchoCanceller {
    /// One step of the adaptive filter (see the module documentation on the
    /// step control).
    pub(super) fn adapt(&mut self, far_energy: f32) {
        let (b, nk, np) = (self.block, self.bins, self.parts);
        // Regularisation: relative to the far end's average bin power, and
        // absolute (a -70 dBFS reference).
        let reg = (REG_REL * far_energy / nk as f32 + np as f32 * (2 * b) as f32 * 1e-7) / np as f32;
        // Correlation of error and echo estimate, smoothed over 100 ms so
        // that chance correlations of short blocks average out.
        let (mut ey, mut ee, mut yy) = (0.0f32, 0.0f32, 0.0f32);
        for (&e, &c) in self.err.iter().zip(&self.cap_in) {
            let y = c - e;
            ey += e * y;
            ee += e * e;
            yy += y * y;
        }
        let cc = self.c_corr;
        self.corr_ey += cc * (ey - self.corr_ey);
        self.corr_ee += cc * (ee - self.corr_ee);
        self.corr_yy += cc * (yy - self.corr_yy);
        let rho2 = (self.corr_ey * self.corr_ey / (self.corr_ee * self.corr_yy + 1e-30)).min(1.0);
        // (A residual larger than the echo estimate itself is left to the
        // other estimates: without echo, the error holds the filter's own
        // output, the regression is not diluted, and an inflated leakage
        // would feed what the filter wrongly learnt.)
        let leak = (LEAK_GAIN * self.leak).min(1.0);
        for k in 0..nk {
            let den = self.pe[k].max(self.mpe[k]) + self.noise[k] + 1e-12;
            let r = (leak.max(self.inv_erle(k)) * self.mpy[k]).max(rho2 * den);
            // Echo is not much louder than the far-end sound it comes from:
            // an error far above the far-end power the filter sees is mostly
            // near-end sound, which the normalised update would otherwise turn
            // into large coefficient jumps. (A 2B window holds twice the
            // power of a block, so equal levels give a ratio of 2; the step is
            // not limited for echo up to 3 dB louder than the far end.)
            let plausible = self.normw[k] / den;
            let mu = (r / den).min(MU_MAX).min(plausible);
            let m = mu / (self.normw[k] + reg);
            self.s1_re[k] = m * self.e_re[k];
            self.s1_im[k] = m * self.e_im[k];
        }
        for p in 0..np {
            let (s, w) = (self.slots[p] * nk, p * nk);
            cmac_conj(
                &mut self.w.re[w..w + nk],
                &mut self.w.im[w..w + nk],
                &self.x_re[s..s + nk],
                &self.x_im[s..s + nk],
                &self.s1_re,
                &self.s1_im,
                self.prop[p],
            );
        }
        self.w.constrain_next(&mut self.fft, &mut self.time);
    }

    /// One step of the shadow filter: the step is fixed (normalised per bin
    /// and proportionate like the main filter's), except where the error is
    /// far louder than the far end could make it (see `adapt`).
    pub(super) fn adapt_shadow(&mut self, far_energy: f32) {
        let (b, nk, np) = (self.block, self.bins, self.parts);
        let reg = (REG_REL * far_energy / nk as f32 + np as f32 * (2 * b) as f32 * 1e-7) / np as f32;
        for k in 0..nk {
            let (er, ei) = (self.sh_e_re[k], self.sh_e_im[k]);
            let den = er * er + ei * ei + self.noise[k] + 1e-12;
            let mu = MU_SHADOW.min(self.sh_normw[k] / den);
            let m = mu / (self.sh_normw[k] + reg);
            self.s1_re[k] = m * er;
            self.s1_im[k] = m * ei;
        }
        for p in 0..np {
            let (s, w) = (self.sh_slots[p] * nk, p * nk);
            cmac_conj(
                &mut self.sh.re[w..w + nk],
                &mut self.sh.im[w..w + nk],
                &self.x_re[s..s + nk],
                &self.x_im[s..s + nk],
                &self.s1_re,
                &self.s1_im,
                self.sh_prop[p],
            );
        }
        self.sh.constrain_next(&mut self.fft, &mut self.time);
    }

    /// Compares the two filters on this block, as Speex's echo canceller
    /// does: the main filter takes the shadow's coefficients when the
    /// shadow's error is smaller by more than chance explains, and the
    /// shadow takes the main filter's when its error is larger by far more
    /// (it went astray, during double talk say). The difference of the two
    /// error energies varies with how much the two echo estimates differ,
    /// which sets what chance explains: a shadow that has learnt unrelated
    /// sound (the near end, without echo) is not taken for an improvement.
    /// Short-term and longer-term (smoothed) differences both count.
    pub(super) fn compare_filters(&mut self) {
        let (mut sff, mut see, mut dbf, mut sdd) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
        for ((&m, &s), &c) in self.err.iter().zip(&self.sh_err).zip(&self.cap_in) {
            sff += m * m;
            see += s * s;
            dbf += (m - s) * (m - s);
            sdd += c * c;
        }
        let cc = self.c_corr;
        self.cmp_d += cc * (sdd - self.cmp_d);
        self.cmp_s += cc * (see - self.cmp_s);
        let d = sff - see;
        self.davg1 = 0.6 * self.davg1 + 0.4 * d;
        self.davg2 = 0.85 * self.davg2 + 0.15 * d;
        self.dvar1 = 0.36 * self.dvar1 + 0.16 * sff * dbf;
        self.dvar2 = 0.7225 * self.dvar2 + 0.0225 * sff * dbf;
        // `x` exceeds what chance explains: x |x| > k var.
        let beyond = |x: f32, var: f32, k: f32| x * x.abs() > k * var;
        // Only a shadow that removes echo is taken: its error must be well
        // below the capture too. (Unrelated sounds can seem to correlate:
        // while a far-end harmonic and a near-end one coincide, a filter can
        // predict the one from the other for a moment.)
        // A shadow that probes another delay is not taken while the delay
        // estimator clearly finds the echo where the main filter is (the
        // near end talks, and the shadow learnt it by chance).
        let confirmed = |delay: usize| {
            self.delay_est.estimate().is_some_and(|q| q + 1.0 >= delay as f32 && q < (delay + self.parts) as f32)
        };
        let explains = self.cmp_s < 0.5 * self.cmp_d
            && (self.sh_delay == self.delay || !confirmed(self.delay) || confirmed(self.sh_delay));
        let take = explains
            && (beyond(d, sff * dbf, 1.0)
                || beyond(self.davg1, self.dvar1, 0.5)
                || beyond(self.davg2, self.dvar2, 0.25));
        let back =
            beyond(-d, sff * dbf, 4.0) || beyond(-self.davg1, self.dvar1, 4.0) || beyond(-self.davg2, self.dvar2, 4.0);
        if take {
            self.w.copy_from(&self.sh);
            self.err.copy_from_slice(&self.sh_err);
            self.e_re.copy_from_slice(&self.sh_e_re);
            self.e_im.copy_from_slice(&self.sh_e_im);
            if self.sh_delay != self.delay {
                // A probe found the echo: the main filter moves there too, and
                // what the old filter achieved is no longer known.
                self.delay = self.sh_delay;
                self.slots[..self.parts].copy_from_slice(&self.sh_slots);
                self.slots[self.parts] = self.slot(self.parts);
                self.norm_stale = true;
                self.forget_reduction();
            }
        } else if back {
            self.sh.copy_from(&self.w);
            self.sh_delay = self.delay;
            self.sh_slots.copy_from_slice(&self.slots[..self.parts]);
            self.sh_err.copy_from_slice(&self.err);
            self.sh_e_re.copy_from_slice(&self.e_re);
            self.sh_e_im.copy_from_slice(&self.e_im);
        }
        if take || back {
            self.davg1 = 0.0;
            self.davg2 = 0.0;
            self.dvar1 = 0.0;
            self.dvar2 = 0.0;
        }
    }
}
