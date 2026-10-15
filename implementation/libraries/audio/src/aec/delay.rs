// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Finding the echo: the bulk delay estimator, and keeping the filters
//! aligned with what it finds (see the module documentation of [`super`]).

use alloc::vec;
use alloc::vec::Vec;

use num_traits::Float;

use super::{EchoCanceller, NOISE_MAX, smoothing};

/// Bands of the delay estimator.
const BANDS: usize = 32;
/// How many standard deviations above the correlation at the other delays
/// a peak must stand to give the delay, and to show a new one while the
/// filter works (the fast correlation is noisier); how many above the best
/// of them (speech has a rhythm, and the delay plus a syllable can compete
/// with the delay); and to be worth probing.
const Z_DELAY: f32 = 4.5;
const Z_CHANGE: f32 = 5.5;
const Z_LEAD: f32 = 2.5;
const Z_PROBE: f32 = 2.0;
/// Blocks between the start of the modelled span and an estimated delay
/// (which can be late by up to two and a half).
const ALIGN_MARGIN: usize = 3;

/// One onset correlation of the delay estimator, smoothed with its own time
/// constant: per candidate delay (in blocks), the smoothed products of the
/// capture's and the reference's onsets and the smoothed reference onset
/// energy at that delay; and the smoothed capture onset energy. Together
/// they give the normalised correlation per candidate.
struct Correlation {
    c: f32,
    cross: Vec<f32>,
    ref_energy: Vec<f32>,
    cap_energy: f32,
    /// Smoothed mean onsets of capture and reference per band.
    cap_mean: [f32; BANDS],
    ref_mean: [f32; BANDS],
    norm: Vec<f32>,
    /// Best candidate and for how many updates it has stayed the best, give
    /// or take a block (a delay between two blocks splits the peak).
    best: usize,
    stable: u32,
    /// How many standard deviations the peak stands above the candidates
    /// outside it, and above the best of them.
    z: f32,
    zm: f32,
}

impl Correlation {
    fn new(c: f32, candidates: usize) -> Correlation {
        Correlation {
            c,
            cross: vec![0.0; candidates],
            ref_energy: vec![0.0; candidates],
            cap_energy: 0.0,
            cap_mean: [0.0; BANDS],
            ref_mean: [0.0; BANDS],
            norm: vec![0.0; candidates],
            best: 0,
            stable: 0,
            z: 0.0,
            zm: 0.0,
        }
    }

    fn reset(&mut self) {
        self.cross.fill(0.0);
        self.ref_energy.fill(0.0);
        self.cap_energy = 0.0;
        self.cap_mean = [0.0; BANDS];
        self.ref_mean = [0.0; BANDS];
        self.norm.fill(0.0);
        self.best = 0;
        self.stable = 0;
        self.z = 0.0;
        self.zm = 0.0;
    }

    /// Takes the statistics of `other` (not its time constant).
    fn copy_from(&mut self, other: &Correlation) {
        self.cross.copy_from_slice(&other.cross);
        self.ref_energy.copy_from_slice(&other.ref_energy);
        self.cap_energy = other.cap_energy;
        self.cap_mean = other.cap_mean;
        self.ref_mean = other.ref_mean;
        self.norm.copy_from_slice(&other.norm);
        self.best = other.best;
        self.stable = other.stable;
        self.z = other.z;
        self.zm = other.zm;
    }

    /// Adds a block: the products of the capture's onsets with the
    /// reference's at every candidate delay (`prod`), the reference onset
    /// energy there and the capture's onset energy. The runner-up must lie
    /// outside the best candidate's peak, which onsets (tens of
    /// milliseconds long) make `peak_width` blocks wide on either side.
    fn update(
        &mut self,
        prod: &[f32],
        ref_energy: &[f32],
        cap: &[f32; BANDS],
        cap_energy: f32,
        newest_ref: &[f32; BANDS],
        peak_width: usize,
    ) {
        let c = self.c;
        self.cap_energy += c * (cap_energy - self.cap_energy);
        for ((mc, mr), (&a, &b)) in self.cap_mean.iter_mut().zip(&mut self.ref_mean).zip(cap.iter().zip(newest_ref)) {
            *mc += c * (a - *mc);
            *mr += c * (b - *mr);
        }
        // Covariances rather than plain products: onsets are never
        // negative, and their means would add the same to every candidate.
        let (mut mm, mut cc, mut rr) = (0.0f32, 0.0f32, 0.0f32);
        for (&a, &b) in self.cap_mean.iter().zip(&self.ref_mean) {
            mm += a * b;
            cc += a * a;
            rr += b * b;
        }
        let cap_var = (self.cap_energy - cc).max(0.0);
        let mut best = 0;
        for q in 0..self.cross.len() {
            self.cross[q] += c * (prod[q] - self.cross[q]);
            self.ref_energy[q] += c * (ref_energy[q] - self.ref_energy[q]);
            let ref_var = (self.ref_energy[q] - rr).max(0.0);
            self.norm[q] = (self.cross[q] - mm) / Float::sqrt(ref_var * cap_var + 1e-20);
            if self.norm[q] > self.norm[best] {
                best = q;
            }
        }
        let (mut second, mut n, mut sum, mut sum2) = (f32::MIN, 0.0f32, 0.0f32, 0.0f32);
        for (q, &v) in self.norm.iter().enumerate() {
            if q.abs_diff(best) > peak_width {
                second = second.max(v);
                n += 1.0;
                sum += v;
                sum2 += v * v;
            }
        }
        let score = self.norm[best];
        let mean = sum / n.max(1.0);
        let sd = Float::sqrt((sum2 / n.max(1.0) - mean * mean).max(0.0));
        self.z = (score - mean) / (sd + 1e-6);
        self.zm = (score - second.max(0.0)) / (sd + 1e-6);
        if best.abs_diff(self.best) <= 1 {
            self.stable = self.stable.saturating_add(1);
        } else {
            self.stable = 0;
        }
        self.best = best;
    }

    /// The peak stands at least `z` standard deviations above the
    /// correlation at the delays outside it, and well above the best of them.
    fn clear(&self, z: f32) -> bool {
        self.z >= z && self.zm >= Z_LEAD
    }

    /// The best delay to a fraction of a block (the vertex of the parabola
    /// through the best candidate and its neighbours).
    fn fractional(&self) -> f32 {
        let b = self.best;
        if b == 0 || b + 1 >= self.norm.len() {
            return b as f32;
        }
        let (l, m, r) = (self.norm[b - 1], self.norm[b], self.norm[b + 1]);
        let den = l - 2.0 * m + r;
        let d = if den < 0.0 { (0.5 * (l - r) / den).clamp(-0.5, 0.5) } else { 0.0 };
        b as f32 + d
    }
}

/// Finds the bulk delay between reference and capture by correlating their
/// onsets (see the module documentation).
pub(super) struct DelayEstimator {
    /// Bin range of every band.
    bands: [(usize, usize); BANDS],
    /// Log2 of each band's lowest energy (-70 dBFS).
    floor_log: [f32; BANDS],
    /// Last band log-energies of reference and capture.
    ref_last: [f32; BANDS],
    cap_last: [f32; BANDS],
    /// Noise floor of the capture's band energies.
    cap_noise: [f32; BANDS],
    /// Smoothed band energies of reference and capture (while the reference
    /// plays), whose ratio carries the capture's floor over to the reference.
    ref_level: [f32; BANDS],
    cap_level: [f32; BANDS],
    c_level: f32,
    /// Reference onsets per block and their energy, newest at `head`.
    ref_flux: Vec<[f32; BANDS]>,
    ref_flux_energy: Vec<f32>,
    head: usize,
    /// This block's capture onsets times the reference onsets at every
    /// candidate delay, and the energy of those (scratch).
    prod: Vec<f32>,
    prod_energy: Vec<f32>,
    /// The correlation over about a second, which gives the delay, and over
    /// about 0.3 s, which notices a new delay sooner.
    slow: Correlation,
    fast: Correlation,
    updates: u32,
    /// Half width of a correlation peak in blocks (40 ms).
    peak_width: usize,
    /// Updates (blocks) in 0.25 s: how long a new delay must persist.
    change_updates: u32,
    floor_rise: f32,
}

impl DelayEstimator {
    pub(super) fn new(rate: u32, block: usize, candidates: usize) -> DelayEstimator {
        let n = 2 * block;
        let bin_hz = rate as f32 / n as f32;
        let lo = 200.0 / bin_hz;
        let hi = (4000.0f32.min(rate as f32 * 0.45)) / bin_hz;
        let mut bands = [(0usize, 0usize); BANDS];
        let mut floor_log = [0.0f32; BANDS];
        for i in 0..BANDS {
            let a = ((lo + (hi - lo) * i as f32 / BANDS as f32) as usize).min(block);
            let b = ((lo + (hi - lo) * (i + 1) as f32 / BANDS as f32) as usize).clamp(a + 1, block + 1);
            bands[i] = (a, b);
            // A -70 dBFS signal: 1e-7 x 2B per bin.
            floor_log[i] = Float::log2(1e-7 * n as f32 * (b - a) as f32);
        }
        let mut d = DelayEstimator {
            bands,
            floor_log,
            ref_last: floor_log,
            cap_last: floor_log,
            cap_noise: [NOISE_MAX; BANDS],
            ref_level: [0.0; BANDS],
            cap_level: [0.0; BANDS],
            c_level: smoothing(block, rate, 1.0),
            ref_flux: vec![[0.0; BANDS]; candidates],
            ref_flux_energy: vec![0.0; candidates],
            head: 0,
            prod: vec![0.0; candidates],
            prod_energy: vec![0.0; candidates],
            slow: Correlation::new(smoothing(block, rate, 1.0), candidates),
            fast: Correlation::new(smoothing(block, rate, 0.3), candidates),
            updates: 0,
            peak_width: ((0.04 * rate as f32) as usize).div_ceil(block),
            change_updates: (0.25 * rate as f32 / block as f32) as u32,
            floor_rise: Float::powf(10.0, 0.3 * block as f32 / rate as f32),
        };
        d.reset();
        d
    }

    pub(super) fn reset(&mut self) {
        self.ref_last = self.floor_log;
        self.cap_last = self.floor_log;
        self.cap_noise = [NOISE_MAX; BANDS];
        self.ref_level = [0.0; BANDS];
        self.cap_level = [0.0; BANDS];
        self.ref_flux.fill([0.0; BANDS]);
        self.ref_flux_energy.fill(0.0);
        self.slow.reset();
        self.fast.reset();
        self.updates = 0;
    }

    /// Band log-energies of a power spectrum, floored.
    fn logs(&self, pow: &[f32], floor: &[f32; BANDS], out: &mut [f32; BANDS]) {
        for (o, (&(a, b), &f)) in out.iter_mut().zip(self.bands.iter().zip(floor)) {
            let e: f32 = pow[a..b].iter().sum();
            *o = Float::log2(e.max(1e-30)).max(f);
        }
    }

    /// Adds the next reference block (power spectrum of its 2B window;
    /// `silent` when that window is all zeros).
    pub(super) fn push_reference(&mut self, pow: &[f32], silent: bool) {
        let mut flux = [0.0; BANDS];
        let mut energy = 0.0;
        if silent {
            self.ref_last = self.floor_log;
        } else {
            // The capture's onsets start where its band energies rise above
            // its noise floor (see `push_capture`), the reference's where the
            // echo of its band energies would: at the capture's floor scaled
            // by how much louder the reference is. (Floored at different
            // levels, the onsets of a syllable's attack would show in the
            // reference a block or two before they show in the capture.)
            let mut floor = self.floor_log;
            for (i, f) in floor.iter_mut().enumerate() {
                let (a, b) = self.bands[i];
                let e: f32 = pow[a..b].iter().sum();
                self.ref_level[i] += self.c_level * (e - self.ref_level[i]);
                if self.cap_level[i] > 0.0 && self.cap_noise[i] < NOISE_MAX {
                    let scaled = 2.0 * self.cap_noise[i] * self.ref_level[i] / self.cap_level[i];
                    *f = f.max(Float::log2(scaled + 1e-30));
                }
            }
            let mut logs = [0.0; BANDS];
            self.logs(pow, &floor, &mut logs);
            for i in 0..BANDS {
                // Onsets only (rises of the band energy, at most 24 dB): the
                // room smears decays but keeps onsets in place.
                flux[i] = (logs[i] - self.ref_last[i]).clamp(0.0, 4.0);
                energy += flux[i] * flux[i];
            }
            self.ref_last = logs;
        }
        self.head = (self.head + 1) % self.ref_flux.len();
        self.ref_flux[self.head] = flux;
        self.ref_flux_energy[self.head] = energy;
    }

    /// True when the reference had onsets within the searched delays.
    pub(super) fn reference_active(&self) -> bool {
        self.ref_flux_energy.iter().any(|&e| e > 0.0)
    }

    /// Correlates the capture block's onsets (from the power spectrum of its
    /// 2B window) with the reference history. `searching`: the filters do not
    /// work, and a new delay is accepted sooner.
    pub(super) fn push_capture(&mut self, pow: &[f32], searching: bool) {
        // Background noise has no onsets: floor each band at twice its level.
        let mut floor = [0.0f32; BANDS];
        for (i, f) in floor.iter_mut().enumerate() {
            let (a, b) = self.bands[i];
            let e: f32 = pow[a..b].iter().sum();
            let n = &mut self.cap_noise[i];
            *n = if e < *n { e } else { (*n * self.floor_rise).min(NOISE_MAX) };
            *f = Float::log2(2.0 * *n + 1e-30).max(self.floor_log[i]);
            self.cap_level[i] += self.c_level * (e - self.cap_level[i]);
        }
        let mut logs = [0.0; BANDS];
        self.logs(pow, &floor, &mut logs);
        let mut flux = [0.0; BANDS];
        let mut energy = 0.0;
        for i in 0..BANDS {
            flux[i] = (logs[i] - self.cap_last[i]).clamp(0.0, 4.0);
            energy += flux[i] * flux[i];
        }
        self.cap_last = logs;
        let len = self.ref_flux.len();
        for q in 0..len {
            let i = (self.head + len - q) % len;
            let mut x = 0.0;
            for (a, b) in flux.iter().zip(&self.ref_flux[i]) {
                x += a * b;
            }
            self.prod[q] = x;
            self.prod_energy[q] = self.ref_flux_energy[i];
        }
        let newest = &self.ref_flux[self.head];
        self.slow.update(&self.prod, &self.prod_energy, &flux, energy, newest, self.peak_width);
        self.fast.update(&self.prod, &self.prod_energy, &flux, energy, newest, self.peak_width);
        self.updates = self.updates.saturating_add(1);
        // A clear peak of the fast correlation that has stayed away from the
        // slow one's for a while is a new delay (the echo path changed): the
        // slow correlation, which would take seconds to forget the old one,
        // starts again from the fast one. While the filters work, the peak
        // must stand out more and the slow correlation must have lost its
        // own (speech has a rhythm: the fast correlation can show the delay
        // plus a syllable for a moment).
        let accept =
            if searching { self.fast.clear(Z_DELAY) } else { self.fast.clear(Z_CHANGE) && !self.slow.clear(Z_DELAY) };
        if accept
            && self.fast.stable >= self.change_updates
            && self.fast.best.abs_diff(self.slow.best) > self.peak_width
        {
            self.slow.copy_from(&self.fast);
        }
    }

    /// The delay in blocks (to a fraction of one), if the estimate is clear.
    pub(super) fn estimate(&self) -> Option<f32> {
        (self.updates >= 20 && self.slow.stable >= 8 && self.slow.clear(Z_DELAY)).then(|| self.slow.fractional())
    }

    /// The delay (in blocks) the fast correlation points to, when its peak
    /// stands out somewhat and has for a while: worth probing.
    fn candidate(&self) -> Option<usize> {
        (self.updates >= 20 && self.fast.stable >= 8 && self.fast.z >= Z_PROBE).then_some(self.fast.best)
    }

    /// Forgets the delay learnt before the last few tenths of a second (the
    /// echo path has changed): the slow correlation starts again from the
    /// fast one.
    pub(super) fn forget(&mut self) {
        self.slow.copy_from(&self.fast);
    }
}

impl EchoCanceller {
    /// Moves the reference alignment so that the echo path fits the filters:
    /// by the converged filter's energy profile while it works, or else by
    /// the delay estimator when the echo now starts before the modelled span
    /// (where no filter can follow it) or late in it.
    pub(super) fn control_alignment(&mut self) {
        self.probe();
        if self.realign_wait > 0 {
            self.realign_wait -= 1;
            return;
        }
        let wait = (0.5 * self.rate as f32 / self.block as f32) as u32;
        let profile = self.w.scanned >= self.parts;
        if profile && self.adapted && self.w.strongest() == 0 && self.delay > 0 {
            // The response may start before the span: move it a block
            // earlier (the filters keep what they learnt).
            self.realign(self.delay - 1);
            self.realign_wait = wait;
        } else if profile && self.working && self.work_age >= wait {
            // A filter that has worked for a while knows best: keep its
            // strongest partition near the front of the span. (A young
            // filter's partitions hold more of what it has yet to learn than
            // of the echo path.)
            let p = self.w.strongest();
            if p >= 3 {
                self.realign(self.delay + p - 1);
                self.realign_wait = wait;
            }
        } else if let Some(q) = self.delay_est.estimate() {
            // Else, while the filters do not work, the span starts a few
            // blocks before the estimated delay when the echo starts before
            // it (by more than the estimate's precision: what the filters
            // learnt then belongs to another echo path) or late in it (the
            // filters learn an echo at the front of the span fastest, and
            // the shift keeps what they learnt).
            let (start, target) = (self.delay as f32, (q as usize).saturating_sub(ALIGN_MARGIN));
            if self.working {
                // (A young filter that works is left to settle.)
            } else if q + 1.0 < start {
                self.realign(target);
                self.reset_filter();
                self.realign_wait = wait;
            } else if q > start + (self.parts / 2) as f32 {
                self.realign(target);
                self.realign_wait = wait;
                // (What the filters achieved before they stopped working no
                // longer tells how much echo they leave.)
                self.forget_reduction();
            }
        }
    }

    /// While the main filter does not work, the shadow probes the delay that
    /// the fast onset correlation points to, before that is certain: if the
    /// echo is there, the shadow converges, and the main filter takes it
    /// with its delay (see `compare_filters`). Once the main filter works,
    /// the shadow follows it again.
    fn probe(&mut self) {
        if self.working {
            if self.sh_delay != self.delay {
                self.sh.copy_from(&self.w);
                self.sh_delay = self.delay;
            }
            self.unexplained = 0;
            return;
        }
        if self.probe_wait > 0 {
            self.probe_wait -= 1;
            return;
        }
        // (A shadow that has started to remove echo stays where it is.)
        let explains = self.cmp_s < 0.5 * self.cmp_d;
        if explains || !self.far_active {
            if explains {
                self.unexplained = 0;
            }
            return;
        }
        self.unexplained = self.unexplained.saturating_add(1);
        // The delay the onsets point to, or else (or if the shadow is
        // already there) the next of the spans that together cover all the
        // searched delays, overlapping by a few blocks.
        let near = |a: usize, b: usize| a.abs_diff(b) <= 1;
        let mut start = self.delay_est.candidate().map(|q| q.saturating_sub(ALIGN_MARGIN).min(self.max_delay));
        // (The blind search waits until no filter has removed echo for a
        // while: at the start, the echo is most likely in the first span.)
        let blind = self.unexplained > (0.6 * self.rate as f32 / self.block as f32) as u32;
        if blind && start.is_none_or(|s| near(s, self.sh_delay) || near(s, self.delay)) {
            let step = self.parts.saturating_sub(4).max(1);
            let spans = self.max_delay / step + 1;
            for _ in 0..spans {
                self.probe_next = (self.probe_next + 1) % spans;
                let s = (self.probe_next * step).min(self.max_delay);
                if !near(s, self.sh_delay) && !near(s, self.delay) {
                    start = Some(s);
                    break;
                }
            }
        }
        if let Some(start) = start.filter(|&s| !near(s, self.sh_delay)) {
            self.sh.clear();
            self.sh_delay = start;
            // Time for the shadow to converge before it moves again.
            self.probe_wait = (0.3 * self.rate as f32 / self.block as f32) as u32;
        }
    }

    /// Changes the bulk delay to `delay` blocks, shifting the filters'
    /// partitions so that what they learnt stays in place.
    fn realign(&mut self, delay: usize) {
        let delay = delay.min(self.max_delay);
        if delay == self.delay {
            return;
        }
        let d = delay as isize - self.delay as isize;
        self.w.shift(d);
        if self.sh_delay == self.delay {
            self.sh.shift(d);
            self.sh_delay = delay;
        }
        self.delay = delay;
        self.norm_stale = true;
    }
}
