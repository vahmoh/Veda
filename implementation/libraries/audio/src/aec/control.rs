// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Step control: what the echo reduction, the leakage and double talk say
//! about the residual echo, which the main filter's step and the
//! suppressor follow (see the module documentation of [`super`]).

use super::{EchoCanceller, GAIN_BANDS};

/// Double talk: the error exceeds the expected residual echo by this power
/// ratio (9 dB). A filter works while it reduces the capture by as much.
const DT_EXCESS: f32 = 8.0;
/// The near end may be talking (6 dB): the suppressor stays gentle.
const NEAR_EXCESS: f32 = 4.0;

impl EchoCanceller {
    /// Double talk, the echo reduction per bin, the echo path gain and the
    /// divergence test (while the far end is active).
    pub(super) fn update_statistics(&mut self, sd: f32, se: f32) {
        let nk = self.bins;
        // Double talk: in the speech band, the error exceeds by far the
        // residual echo that the filter leaves while the far end talks alone
        // (the echo estimate times this echo reduction), plus the noise.
        // (Not the leakage, which a single block of near-end sound can
        // swing.)
        let (mut band_e, mut band_s, mut band_x) = (0.0f32, 0.0f32, 0.0f32);
        for k in self.band_lo..self.band_hi {
            band_e += self.pe[k];
            band_s += self.sh_e_re[k] * self.sh_e_re[k] + self.sh_e_im[k] * self.sh_e_im[k];
            band_x += self.inv_erle(k) * self.py[k] + 3.0 * self.noise[k];
        }
        // Whether the filter works now, double talk or not.
        self.now_d += self.c_erle * (sd - self.now_d);
        self.now_e += self.c_erle * (se - self.now_e);
        self.working = self.adapted && self.now_d > DT_EXCESS * self.now_e;
        self.work_age = if self.working { self.work_age.saturating_add(1) } else { 0 };
        // (The shadow's error must exceed it too: after an echo path change
        // within the modelled span, the shadow soon follows the new echo,
        // while it cannot remove the near end either.)
        let excess = self.adapted && band_e > DT_EXCESS * band_x && band_s > DT_EXCESS * band_x;
        self.dt_run = if excess { self.dt_run.saturating_add(1) } else { 0 };
        // The near end may be talking: the suppressor stays gentle (for
        // as long as double talk is held).
        if self.adapted && band_e > NEAR_EXCESS * band_x && band_s > NEAR_EXCESS * band_x {
            self.near_hold = self.dt_hold_blocks;
        } else {
            self.near_hold = self.near_hold.saturating_sub(1);
        }
        // Double talk lasts: two blocks in a row (a burst of residual echo
        // where the filter is still learning a sound lasts one).
        if self.dt_run >= 2 {
            self.dt = true;
            self.dt_hold = self.dt_hold_blocks;
        } else if self.dt_hold > 0 {
            self.dt_hold -= 1;
        } else {
            self.dt = false;
        }
        if !self.dt {
            self.erle_d += self.c_erle * (sd - self.erle_d);
            self.erle_e += self.c_erle * (se - self.erle_e);
            // Echo reduction per bin, learnt where the capture stands out of
            // the noise while the far end talks alone. (After an echo path
            // change the error follows the old echo estimate, which the step
            // control sees; once the filter has caught up, the double-talk
            // verdict ends and this learns again.)
            let c = self.c_erle;
            for k in 0..nk {
                if self.pd[k] > 4.0 * self.noise[k] {
                    let i = k + self.state[k] as usize * nk;
                    self.bin_d[i] += c * (self.pd[k] - self.bin_d[i]);
                    self.bin_e[i] += c * (self.pe[k] - self.bin_e[i]);
                }
            }
        }
        // Evidence of echo: the filter reduces the capture. It holds for 5 s
        // of far-end sound after it was last seen (an echo path change needs
        // the suppressor most), and without it nothing is suppressed beyond
        // what the filter estimates (headphones: no echo).
        if self.adapted || (self.erle_db() > 3.0 && self.delay_est.estimate().is_some()) {
            self.echo_hold = self.echo_hold_blocks;
        } else {
            self.echo_hold = self.echo_hold.saturating_sub(1);
        }
        self.echo_seen = self.echo_hold > 0;
        // Without any echo the filters have nothing to learn but the near end:
        // after 2 s of far-end sound without echo, clear them.
        if !self.echo_seen && self.erle_db() < 1.0 {
            self.no_echo += 1;
            if self.no_echo >= self.no_echo_blocks {
                self.w.clear();
                self.sh.clear();
                self.no_echo = 0;
            }
        } else {
            self.no_echo = 0;
        }
        // Echo path gain per band: how loud the capture is for the far-end
        // power the filter sees.
        if self.echo_seen && !self.dt {
            let (mut gd, mut gx) = ([0.0f32; GAIN_BANDS], [0.0f32; GAIN_BANDS]);
            for k in 0..nk {
                let band = k * GAIN_BANDS / nk;
                gd[band] += self.pd[k];
                gx[band] += self.normw[k];
            }
            for i in 0..GAIN_BANDS {
                self.gain_d[i] += self.c_erle * (gd[i] - self.gain_d[i]);
                self.gain_x[i] += self.c_erle * (gx[i] - self.gain_x[i]);
            }
        }
        if !self.adapted && self.erle_db() > 10.0 {
            self.adapted = true;
        }
        // An error above the capture for 0.3 s means that the filter adds
        // echo: it has diverged, or the echo path changed beyond repair. (Not
        // judged below -60 dBFS: a nearly silent capture proves little.)
        let noise_sum: f32 = self.noise.iter().sum();
        self.div_d += self.c_div * (sd - self.div_d);
        self.div_e += self.c_div * (se - self.div_e);
        let floor = (self.block * self.block) as f32 * 1e-6;
        if self.div_e > 1.25 * self.div_d + 4.0 * noise_sum + floor {
            self.diverge += 1;
            if self.diverge >= self.diverge_blocks {
                self.reset_filter();
                self.div_e = self.div_d;
                self.delay_est.forget();
            }
        } else {
            self.diverge = 0;
        }
    }

    /// The inverse echo reduction (error over capture power) of bin `k` for
    /// the far end's current state there, between -50 dB and 0 dB; 1 while
    /// unknown.
    #[inline]
    pub(super) fn inv_erle(&self, k: usize) -> f32 {
        let i = k + self.state[k] as usize * self.bins;
        let measured = if self.bin_d[i] > 0.0 { (self.bin_e[i] / self.bin_d[i]).clamp(1e-5, 1.0) } else { 1.0 };
        measured.max(self.young)
    }
}
