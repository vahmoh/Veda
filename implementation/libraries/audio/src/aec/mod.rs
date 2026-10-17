// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Acoustic echo cancellation.
//!
//! A voice assistant that keeps listening while it talks hears itself: the
//! microphone picks up what the loudspeakers play. [`EchoCanceller`] removes
//! that echo from the microphone signal (the *capture*) using what was sent
//! to the loudspeakers (the *reference*, or far end), so that only the
//! user's voice (the *near end*) and the room's background remain, and the
//! user can interrupt the assistant at any time.
//!
//! The canceller takes 16-bit mono frames of any length at 8 to 48 kHz
//! (16 kHz is the main target). It works on blocks of `B` samples, the
//! smallest power of two that lasts 1/128 s (128 at 16 kHz, 512 at 48 kHz),
//! with FFTs of `2B` points, in four parts.
//!
//! **Delay.** Reference and capture may be offset by up to 500 ms (output
//! buffers, the sound card, the way to the microphone). The delay estimator
//! compares *onsets*: per block and per band (32 bands from 200 Hz to 4 kHz)
//! the rise of the log energy since the previous block. A room smears the
//! ends of sounds but not their beginnings, and onsets do not depend on the
//! vowel or on the room's colouring. The capture's band energies are floored
//! at its noise, the reference's where their echo would meet that floor (so
//! that an onset starts in both at the same moment). The capture's onsets
//! are correlated (as covariances) with the reference's at every candidate
//! delay, over about a second and, to notice a new delay sooner, over about
//! 0.3 s. A peak gives the delay, to a fraction of a block, once it stands
//! well above the correlation at all other delays and above the best of
//! them (speech has a rhythm, and the delay plus a syllable can compete); a
//! clear new peak of the fast correlation replaces the slow one. The
//! reference is kept as a history of spectra, so a delay costs nothing: the
//! filters read older spectra. Once the filter has converged, its largest
//! tap gives the delay to the sample, and its energy profile keeps the echo
//! at the front of the span.
//!
//! **Linear filters.** Two partitioned-block frequency-domain adaptive
//! filters (multi-delay filters, overlap-save) model the echo path after
//! the bulk delay with `P = tail / B + 1` partitions of `B` taps. Their
//! updates are normalised per frequency bin, so that all frequencies
//! converge alike, and *proportionate*: half of each correction is shared by
//! the partitions' shares of the filter, half by a prior that decays like a
//! room from where the delay estimator expects the echo to start. The
//! gradient constraint (each partition's impulse response stays causal)
//! visits one partition per block in turn. The *shadow* filter adapts with a
//! large fixed step: it converges and follows a changed echo path fast, and
//! goes astray when the near end talks. The *main* filter, whose echo
//! estimate is subtracted, adapts with a controlled step (below) and takes
//! the shadow's coefficients when the shadow's error is smaller by more than
//! chance explains, as in Speex's echo canceller (the variance of the
//! difference of the two error energies grows with how much the two echo
//! estimates differ), and only if the shadow removes echo: its error must
//! be well below the capture, since unrelated sounds can seem to correlate
//! for a moment (while a far-end harmonic and a near-end one coincide). A
//! shadow far worse than the main filter takes the main filter's
//! coefficients. While the main filter does not work, the shadow *probes*
//! the delay that the fast correlation points to before that is certain, or
//! else searches all delays span by span: where the echo is, it converges,
//! and the main filter takes it with its delay.
//!
//! **Step control.** Near-end sound is noise to the main filter (double
//! talk). Its step is the optimal one, residual echo power over error power
//! per bin. The residual echo is the largest of three estimates: the echo
//! estimate times the echo reduction that bin achieved while the far end
//! talked alone (measured separately while the far end starts, holds and
//! ends); the echo estimate times the *leakage*, the regression of the error
//! power's fluctuations on the echo estimate's (residual echo follows the
//! echo, near-end sound does not); and the error times the squared
//! correlation of error and echo estimate over about 100 ms. A filter that
//! has just replaced the old one is assumed to leave all the echo at first.
//! Double talk is declared when, in two blocks in a row, the errors of both
//! filters exceed the residual echo expected from the echo reduction by
//! 9 dB (after an echo path change the shadow soon follows the new echo; it
//! cannot remove the near end either); the statistics then pause. Whether
//! the filter works (reduces the capture by 9 dB) is judged apart from
//! them, since an echo path change can pass for double talk. A filter that
//! makes the echo louder for 0.3 s is reset.
//!
//! **Residual echo suppression.** The filter leaves some echo: its
//! misadjustment (largest at the onsets of new sounds), loudspeaker
//! distortion and late reverberation. A spectral gain on Hann-windowed
//! frames of the error (50 % overlap-add) removes it: `1 - 3 x residual /
//! error` per bin (`2 x` while the near end may be talking: the error
//! exceeds the expected residual by 6 dB), released gently, at least -40 dB
//! and never below the background noise (no noise pumping). The residual is
//! the largest of the leakage times the echo estimate; the echo reduction
//! this bin achieves times the echo estimate or the capture, whichever is
//! larger, but the capture only as far as the far end can produce that much
//! echo through the measured echo path gain, which stays zero until echo
//! has been detected (without echo, say with headphones, nothing is
//! suppressed); and the late reverberation, the echo estimate's power
//! remembered with the room's decay times the share that the ends of echoes
//! leave behind. The windowed spectra come from the filter's zero-padded
//! block spectra (a sign flip and a three-tap kernel), so analysis costs no
//! FFT.
//!
//! The output lags the capture by `2B - 1` samples (16 ms at 16 kHz): `B -
//! 1` to collect blocks from frames of any length, `B` for the overlap-add.
//! While the far end plays, a block costs eleven real FFTs of `2B` points
//! and about `4P` complex multiply-adds per bin; while the loudspeakers are
//! silent there is no filtering, and one capture FFT every fourth block
//! keeps the noise estimate current.
//!
//! Input is DC-blocked (a 30 Hz high-pass on both signals), clipped capture
//! blocks do not adapt the filters, and non-finite values reset the
//! canceller, so the output is always finite.
//!
//! The code follows the parts: this module holds the canceller's state and
//! runs a block through them; `delay` estimates the delay and keeps the
//! filters aligned with it, `filter` adapts and compares the two filters,
//! `control` keeps the statistics their step and the suppressor follow, and
//! `suppress` makes the output.

mod control;
mod delay;
mod filter;
mod suppress;
#[cfg(test)]
mod tests;

use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

use num_traits::Float;

use crate::fft::{self, RealFft};
use crate::synth::filter::DcBlock;

use delay::DelayEstimator;
use filter::{Partitions, filter_error};

/// Supported sample rates.
const MIN_RATE: u32 = 8_000;
const MAX_RATE: u32 = 48_000;
/// Longest bulk delay between reference and capture that is searched.
const MAX_DELAY_MS: u32 = 500;
/// Limits of the modelled echo tail.
const MIN_TAIL_MS: u32 = 20;
const MAX_TAIL_MS: u32 = 1000;
/// Decay time constant (of the amplitude, in seconds) of the prior.
const PRIOR_S: f32 = 0.08;
/// How long (seconds of far-end sound) a filter that has just replaced the
/// old one is assumed to leave more echo than measured.
const YOUNG_S: f32 = 0.5;
/// Leakage limits (residual echo power over echo estimate power).
const LEAK_MIN: f32 = 1e-4;
const LEAK_MAX: f32 = 4.0;
/// The leakage regression runs low: the per-bin powers carry fluctuations
/// that the residual does not share. This compensates.
const LEAK_GAIN: f32 = 3.0;
/// Initial share of remembered echo power that late reverberation leaves in
/// the error (-20 dB), until the ends of echoes have been measured.
const REV_RATIO: f32 = 0.01;
/// Far-end activity: mean square of the reference over the modelled span
/// (-66 dBFS).
const FAR_FLOOR: f32 = 2.5e-7;
/// Capture samples at or above this magnitude mark a clipped block.
const CLIP: i32 = 32_000;
/// Both signals are high-passed above this (their DC and rumble).
const DC_CUTOFF_HZ: f32 = 30.0;
/// Values below this are flushed to zero (avoids denormals).
const TINY: f32 = 1e-15;
/// Start (and ceiling) of the noise floor estimates, far above any block
/// power of 16-bit audio.
const NOISE_MAX: f32 = 1e9;
/// Bands of the echo path gain estimate.
const GAIN_BANDS: usize = 16;

/// `1 - e^(-B / (seconds x rate))`: the per-block coefficient of a first
/// order smoother with the given time constant.
fn smoothing(block: usize, rate: u32, seconds: f32) -> f32 {
    1.0 - Float::exp(-(block as f32) / (seconds * rate as f32))
}

/// The default block: the smallest power of two that lasts at least 1/128 s
/// (128 samples at 16 kHz, 512 at 44.1 and 48 kHz).
fn block_size(rate: u32) -> usize {
    (rate as usize).div_ceil(128).next_power_of_two().clamp(64, 1024)
}

/// Proportionate weights before the filter has learnt anything: decaying
/// like a room's impulse response (by `r` per partition) from the partition
/// where the echo is expected to start, `first`, and twice as fast towards
/// the front of the span (the estimate may be late by a block or two);
/// summing to 1.
fn prior(out: &mut [f32], first: usize, r: f32) {
    for (p, w) in out.iter_mut().enumerate() {
        let d = if p >= first { p - first } else { 2 * (first - p) };
        *w = Float::powi(r, d as i32);
    }
    let sum: f32 = out.iter().sum();
    for w in out.iter_mut() {
        *w /= sum;
    }
}

/// The energy `sum |x|^2` of a complex vector in split form.
#[inline]
fn energy(re: &[f32], im: &[f32]) -> f32 {
    let n = re.len().min(im.len());
    let (rc, rt) = re[..n].as_chunks::<4>();
    let (ic, it) = im[..n].as_chunks::<4>();
    // Four running sums, so that the loop vectorises.
    let mut acc = [0.0f32; 4];
    for (r, i) in rc.iter().zip(ic) {
        for l in 0..4 {
            acc[l] += r[l] * r[l] + i[l] * i[l];
        }
    }
    let mut e = (acc[0] + acc[1]) + (acc[2] + acc[3]);
    for (r, i) in rt.iter().zip(it) {
        e += r * r + i * i;
    }
    e
}

/// A fixed-capacity ring of output samples (the capacity is a power of two).
struct Fifo {
    buf: Vec<i16>,
    read: usize,
    len: usize,
}

impl Fifo {
    fn new(capacity: usize) -> Fifo {
        Fifo { buf: vec![0; capacity.next_power_of_two()], read: 0, len: 0 }
    }

    fn clear(&mut self) {
        self.read = 0;
        self.len = 0;
    }

    fn push(&mut self, v: i16) {
        if self.len < self.buf.len() {
            let mask = self.buf.len() - 1;
            self.buf[(self.read + self.len) & mask] = v;
            self.len += 1;
        }
    }

    fn pop(&mut self) -> Option<i16> {
        if self.len == 0 {
            return None;
        }
        let v = self.buf[self.read];
        self.read = (self.read + 1) & (self.buf.len() - 1);
        self.len -= 1;
        Some(v)
    }
}

/// An acoustic echo canceller for one mono capture stream (see the module
/// documentation).
///
/// ```
/// use audio::aec::EchoCanceller;
///
/// let mut aec = EchoCanceller::new(16_000, 250);
/// let (mic, played) = ([0i16; 160], [0i16; 160]);
/// let mut clean = [0i16; 160];
/// aec.process(&mic, &played, &mut clean);
/// assert_eq!(aec.echo_delay_ms(), None);
/// ```
pub struct EchoCanceller {
    rate: u32,
    /// Samples per block (B).
    block: usize,
    /// Bins of the 2B-point spectra (B + 1).
    bins: usize,
    /// Filter partitions (P).
    parts: usize,
    /// Largest bulk delay in blocks.
    max_delay: usize,
    /// Reference spectra kept (max_delay + P + 1).
    hist_len: usize,
    fft: RealFft,
    /// Periodic Hann window of 2B samples.
    hann: Vec<f32>,
    suppression: bool,
    /// Blocks processed (wrapping).
    blocks: u32,

    // Input collection and output.
    cap_in: Vec<f32>,
    ref_in: Vec<f32>,
    cap_peak: i32,
    fill: usize,
    out: Fifo,
    dc_cap: DcBlock,
    dc_ref: DcBlock,

    // Reference history: spectra of 2B-sample windows, newest at `head`.
    ref_prev: Vec<f32>,
    ref_prev_zero: bool,
    x_re: Vec<f32>,
    x_im: Vec<f32>,
    x_pow: Vec<f32>,
    /// Per history slot: the window was all zeros.
    x_silent: Vec<bool>,
    head: usize,
    /// Bulk delay applied to the reference, in blocks.
    delay: usize,

    /// The adaptive filter, its proportionate weights and their prior, and
    /// the partition where that expects the echo to start.
    w: Partitions,
    prop: Vec<f32>,
    prior: Vec<f32>,
    prior_first: usize,
    /// The prior's decay per partition.
    prior_r: f32,
    /// The shadow filter (fixed step), its bulk delay (the main filter's,
    /// except while it probes another one), its weights, normaliser and
    /// error.
    sh: Partitions,
    sh_delay: usize,
    sh_slots: Vec<usize>,
    /// Blocks before the shadow may probe another delay, and the last span
    /// it searched blind.
    probe_wait: u32,
    probe_next: usize,
    /// Blocks of far-end sound since a filter last removed echo.
    unexplained: u32,
    sh_prop: Vec<f32>,
    sh_prior: Vec<f32>,
    sh_prior_first: usize,
    sh_normw: Vec<f32>,
    sh_err: Vec<f32>,
    sh_e_re: Vec<f32>,
    sh_e_im: Vec<f32>,
    /// History slot of every partition's reference spectrum this block (and
    /// of the one that just left the span).
    slots: Vec<usize>,
    /// Smoothed differences of the two filters' error energies and their
    /// variances (the comparison of the filters).
    davg1: f32,
    davg2: f32,
    dvar1: f32,
    dvar2: f32,
    /// Smoothed block energies of the capture and of the shadow's error.
    cmp_d: f32,
    cmp_s: f32,

    // Zero-padded block spectra of capture and error, this and the last
    // block.
    d_re: Vec<f32>,
    d_im: Vec<f32>,
    dp_re: Vec<f32>,
    dp_im: Vec<f32>,
    e_re: Vec<f32>,
    e_im: Vec<f32>,
    ep_re: Vec<f32>,
    ep_im: Vec<f32>,
    /// The spectra of the last block were computed (not skipped while idle).
    spectra_valid: bool,
    // Scratch spectra.
    s1_re: Vec<f32>,
    s1_im: Vec<f32>,
    s2_re: Vec<f32>,
    s2_im: Vec<f32>,
    s3_re: Vec<f32>,
    s3_im: Vec<f32>,
    /// Reference power per bin summed over the partitions (updated as the
    /// history moves, recomputed now and then), the number of windows in
    /// the span that are not silent, and the power weighted by the
    /// proportionate weights (the normaliser).
    norm: Vec<f32>,
    norm_live: usize,
    norm_stale: bool,
    normw: Vec<f32>,
    /// Block powers of capture, error and echo estimate, and the smoothed
    /// powers of the last two.
    pd: Vec<f32>,
    pe: Vec<f32>,
    py: Vec<f32>,
    mpe: Vec<f32>,
    mpy: Vec<f32>,
    /// Noise floor of the error (block power scale, biased low).
    noise: Vec<f32>,
    time: Vec<f32>,
    err: Vec<f32>,
    err_prev: Vec<f32>,

    // Suppressor.
    gain: Vec<f32>,
    resid: Vec<f32>,
    /// Echo estimate power remembered with the room's decay, and the part
    /// of it that late reverberation leaves in the error.
    rev: Vec<f32>,
    rev_ratio: f32,
    ola: Vec<f32>,

    // Step control.
    /// Smoothed products of error and echo estimate (their correlation).
    corr_ey: f32,
    corr_ee: f32,
    corr_yy: f32,
    leak: f32,
    cov_ey: f32,
    var_yy: f32,
    leak_blocks: u32,
    /// The filter has reached 10 dB of echo return loss enhancement.
    adapted: bool,

    // Statistics.
    erle_d: f32,
    erle_e: f32,
    /// Smoothed capture and error power while the far end plays, double
    /// talk or not: whether the filter works now (double talk pauses the
    /// statistics, and an echo path change can pass for double talk).
    now_d: f32,
    now_e: f32,
    working: bool,
    /// Blocks of far-end sound the filter has kept working.
    work_age: u32,
    /// Smoothed capture and error power per bin while the far end talks
    /// alone (the echo reduction per bin), for each state of the far end
    /// (B + 1 bins each): steady, starting (where the filter does worse) and
    /// ending (where what it leaves decays slower than its estimate).
    bin_d: Vec<f32>,
    bin_e: Vec<f32>,
    /// Per bin, the state of the far end where the echo comes from: its
    /// power there well above its average over the modelled span (1: it has
    /// just started), well below (2: it has ended), or neither (0).
    state: Vec<u8>,
    /// How much a filter that has just replaced the old one is assumed to
    /// leave: 1 when replaced, fading as its echo reduction is measured.
    young: f32,
    young_keep: f32,
    /// Smoothed capture and weighted far-end power per band while the far
    /// end talks alone, once echo has been seen (the echo path gain).
    gain_d: [f32; GAIN_BANDS],
    gain_x: [f32; GAIN_BANDS],
    /// There is evidence of echo (the filter reduces the capture), held for
    /// `echo_hold` more blocks of far-end sound.
    echo_seen: bool,
    echo_hold: u32,
    /// Blocks of far-end sound without any echo.
    no_echo: u32,
    far_active: bool,
    dt: bool,
    dt_hold: u32,
    /// Consecutive blocks whose error exceeded the expected residual.
    dt_run: u32,
    /// Blocks the suppressor stays gentle: the near end may be talking.
    near_hold: u32,
    diverge: u32,
    /// Smoothed capture and error power (divergence test).
    div_d: f32,
    div_e: f32,
    realign_wait: u32,

    // Per-block coefficients.
    c_mean: f32,
    c_corr: f32,
    c_div: f32,
    c_erle: f32,
    c_rev: f32,
    leak_keep: f32,
    noise_rise: f32,
    resid_keep: f32,
    /// Power decay per block of a room with a 0.25 s reverberation time.
    min_decay: f32,
    diverge_blocks: u32,
    dt_hold_blocks: u32,
    echo_hold_blocks: u32,
    no_echo_blocks: u32,
    /// Speech band bins (double-talk detection).
    band_lo: usize,
    band_hi: usize,

    delay_est: DelayEstimator,
}

impl fmt::Debug for EchoCanceller {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "EchoCanceller({} Hz, block {}, {} partitions, delay {} blocks, ERLE {:.1} dB)",
            self.rate,
            self.block,
            self.parts,
            self.delay,
            self.erle_db()
        )
    }
}

impl EchoCanceller {
    /// A canceller for `rate` Hz (clamped to 8000..=48000) that models
    /// `tail_ms` milliseconds of echo path after the bulk delay (clamped to
    /// 20..=1000; 200 to 300 suits a room). Bulk delays of up to 500 ms are
    /// found automatically and do not count towards the tail.
    pub fn new(rate: u32, tail_ms: u32) -> EchoCanceller {
        Self::with_block_size(rate, tail_ms, block_size(rate.clamp(MIN_RATE, MAX_RATE)))
    }

    /// Like [`new`](EchoCanceller::new) with an explicit internal block size
    /// (rounded up to a power of two in 64..=1024). Larger blocks cost less
    /// CPU time and add latency (`2 x block - 1` samples).
    pub fn with_block_size(rate: u32, tail_ms: u32, block: usize) -> EchoCanceller {
        let rate = rate.clamp(MIN_RATE, MAX_RATE);
        let tail_ms = tail_ms.clamp(MIN_TAIL_MS, MAX_TAIL_MS);
        let block = block.clamp(64, 1024).next_power_of_two();
        let bins = block + 1;
        let n = 2 * block;
        let tail = (tail_ms as u64 * rate as u64 / 1000) as usize;
        // One extra partition: the alignment leaves a margin in front of the
        // direct path.
        let parts = tail.div_ceil(block) + 1;
        let max_delay = ((MAX_DELAY_MS as u64 * rate as u64 / 1000) as usize).div_ceil(block);
        let hist_len = max_delay + parts + 1;
        let hann = fft::hann_periodic(n);
        let bin_hz = rate as f32 / n as f32;
        let spec = || vec![0.0f32; bins];
        let mut aec = EchoCanceller {
            rate,
            block,
            bins,
            parts,
            max_delay,
            hist_len,
            fft: RealFft::new(n),
            hann,
            suppression: true,
            blocks: 0,
            cap_in: vec![0.0; block],
            ref_in: vec![0.0; block],
            cap_peak: 0,
            fill: 0,
            out: Fifo::new(4 * block),
            dc_cap: DcBlock::with_cutoff(DC_CUTOFF_HZ, rate),
            dc_ref: DcBlock::with_cutoff(DC_CUTOFF_HZ, rate),
            ref_prev: vec![0.0; block],
            ref_prev_zero: true,
            x_re: vec![0.0; hist_len * bins],
            x_im: vec![0.0; hist_len * bins],
            x_pow: vec![0.0; hist_len * bins],
            x_silent: vec![true; hist_len],
            head: 0,
            delay: 0,
            w: Partitions::new(parts, bins),
            prop: vec![0.0; parts],
            prior: vec![0.0; parts],
            prior_first: usize::MAX,
            prior_r: 1.0 - smoothing(block, rate, PRIOR_S),
            sh: Partitions::new(parts, bins),
            sh_delay: 0,
            sh_slots: vec![0; parts],
            probe_wait: 0,
            probe_next: 0,
            unexplained: 0,
            sh_prop: vec![0.0; parts],
            sh_prior: vec![0.0; parts],
            sh_prior_first: usize::MAX,
            sh_normw: spec(),
            sh_err: vec![0.0; block],
            sh_e_re: spec(),
            sh_e_im: spec(),
            slots: vec![0; parts + 1],
            davg1: 0.0,
            davg2: 0.0,
            dvar1: 0.0,
            dvar2: 0.0,
            cmp_d: 0.0,
            cmp_s: 0.0,
            d_re: spec(),
            d_im: spec(),
            dp_re: spec(),
            dp_im: spec(),
            e_re: spec(),
            e_im: spec(),
            ep_re: spec(),
            ep_im: spec(),
            spectra_valid: true,
            s1_re: spec(),
            s1_im: spec(),
            s2_re: spec(),
            s2_im: spec(),
            s3_re: spec(),
            s3_im: spec(),
            norm: spec(),
            norm_live: 0,
            norm_stale: true,
            normw: spec(),
            pd: spec(),
            pe: spec(),
            py: spec(),
            mpe: spec(),
            mpy: spec(),
            noise: spec(),
            time: vec![0.0; n],
            err: vec![0.0; block],
            err_prev: vec![0.0; block],
            gain: vec![1.0; bins],
            resid: spec(),
            rev: spec(),
            rev_ratio: REV_RATIO,
            ola: vec![0.0; block],
            corr_ey: 0.0,
            corr_ee: 0.0,
            corr_yy: 0.0,
            leak: 1.0,
            cov_ey: 0.0,
            var_yy: 0.0,
            leak_blocks: 0,
            adapted: false,
            erle_d: 0.0,
            erle_e: 0.0,
            now_d: 0.0,
            now_e: 0.0,
            working: false,
            work_age: 0,
            bin_d: vec![0.0; 3 * bins],
            bin_e: vec![0.0; 3 * bins],
            state: vec![0; bins],
            young: 0.0,
            young_keep: 1.0 - smoothing(block, rate, YOUNG_S),
            gain_d: [0.0; GAIN_BANDS],
            gain_x: [0.0; GAIN_BANDS],
            echo_seen: false,
            echo_hold: 0,
            no_echo: 0,
            far_active: false,
            dt: false,
            dt_hold: 0,
            dt_run: 0,
            near_hold: 0,
            diverge: 0,
            div_d: 0.0,
            div_e: 0.0,
            realign_wait: 0,
            c_mean: smoothing(block, rate, 0.06),
            c_corr: smoothing(block, rate, 0.1),
            c_div: smoothing(block, rate, 0.1),
            c_erle: smoothing(block, rate, 0.3),
            c_rev: smoothing(block, rate, 0.5),
            leak_keep: 1.0 - smoothing(block, rate, 0.4),
            noise_rise: Float::powf(10.0, 0.3 * block as f32 / rate as f32),
            resid_keep: 1.0 - smoothing(block, rate, 0.025),
            // 60 dB per 0.25 s.
            min_decay: Float::powf(10.0, -6.0 * block as f32 / (0.25 * rate as f32)),
            diverge_blocks: (0.3 * rate as f32 / block as f32) as u32 + 1,
            dt_hold_blocks: (0.1 * rate as f32 / block as f32) as u32 + 1,
            echo_hold_blocks: (5.0 * rate as f32 / block as f32) as u32,
            no_echo_blocks: (2.0 * rate as f32 / block as f32) as u32,
            band_lo: ((200.0 / bin_hz) as usize).min(block),
            band_hi: ((4000.0 / bin_hz) as usize).min(block),
            delay_est: DelayEstimator::new(rate, block, max_delay + 1),
        };
        aec.reset();
        aec
    }

    /// Sample rate in Hz.
    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// Samples per internal block.
    pub fn block_size(&self) -> usize {
        self.block
    }

    /// How many samples the output lags behind the capture.
    pub fn latency_samples(&self) -> usize {
        2 * self.block - 1
    }

    /// Turns the residual echo suppressor on (the default) or off. Without
    /// it the output is the linear filter's error signal: less echo
    /// suppression, no spectral processing of the near end (the tests
    /// compare the two).
    #[cfg(test)]
    fn set_suppression(&mut self, on: bool) {
        self.suppression = on;
    }

    /// Forgets everything learnt: the echo path, the delay and the noise
    /// estimate. The output latency is restored as well.
    pub fn reset(&mut self) {
        self.fill = 0;
        self.cap_peak = 0;
        self.out.clear();
        for _ in 0..self.block - 1 {
            self.out.push(0);
        }
        self.dc_cap = DcBlock::with_cutoff(DC_CUTOFF_HZ, self.rate);
        self.dc_ref = DcBlock::with_cutoff(DC_CUTOFF_HZ, self.rate);
        self.ref_prev.fill(0.0);
        self.ref_prev_zero = true;
        self.x_re.fill(0.0);
        self.x_im.fill(0.0);
        self.x_pow.fill(0.0);
        self.x_silent.fill(true);
        self.head = 0;
        self.delay = 0;
        self.sh_delay = 0;
        self.probe_wait = 0;
        self.probe_next = 0;
        self.unexplained = 0;
        self.prior_first = usize::MAX;
        self.sh_prior_first = usize::MAX;
        for v in [&mut self.d_re, &mut self.d_im, &mut self.dp_re, &mut self.dp_im] {
            v.fill(0.0);
        }
        for v in [&mut self.e_re, &mut self.e_im, &mut self.ep_re, &mut self.ep_im] {
            v.fill(0.0);
        }
        self.spectra_valid = true;
        for v in [&mut self.mpe, &mut self.mpy, &mut self.resid, &mut self.ola, &mut self.err, &mut self.err_prev] {
            v.fill(0.0);
        }
        self.norm.fill(0.0);
        self.norm_live = 0;
        self.norm_stale = true;
        self.rev.fill(0.0);
        self.rev_ratio = REV_RATIO;
        self.noise.fill(NOISE_MAX);
        self.gain.fill(1.0);
        self.erle_d = 0.0;
        self.erle_e = 0.0;
        self.bin_d.fill(0.0);
        self.bin_e.fill(0.0);
        self.state.fill(0);
        self.gain_d = [0.0; GAIN_BANDS];
        self.gain_x = [0.0; GAIN_BANDS];
        self.echo_seen = false;
        self.echo_hold = 0;
        self.no_echo = 0;
        self.far_active = false;
        self.dt = false;
        self.dt_hold = 0;
        self.dt_run = 0;
        self.near_hold = 0;
        self.div_d = 0.0;
        self.div_e = 0.0;
        self.realign_wait = 0;
        self.delay_est.reset();
        self.reset_filter();
    }

    /// Clears the adaptive filters and their step control.
    fn reset_filter(&mut self) {
        self.w.clear();
        self.sh.clear();
        self.davg1 = 0.0;
        self.davg2 = 0.0;
        self.dvar1 = 0.0;
        self.dvar2 = 0.0;
        self.cmp_d = 0.0;
        self.cmp_s = 0.0;
        self.corr_ey = 0.0;
        self.corr_ee = 0.0;
        self.corr_yy = 0.0;
        self.leak = 1.0;
        self.cov_ey = 0.0;
        self.var_yy = 0.0;
        self.leak_blocks = 0;
        self.adapted = false;
        self.diverge = 0;
        self.forget_reduction();
    }

    /// Forgets the echo reduction measured for the filter (it was replaced):
    /// the suppressor removes all the echo the far end can explain until the
    /// new filter has been measured.
    fn forget_reduction(&mut self) {
        self.erle_d = 0.0;
        self.erle_e = 0.0;
        self.now_d = 0.0;
        self.now_e = 0.0;
        self.working = false;
        self.work_age = 0;
        self.bin_d.fill(0.0);
        self.bin_e.fill(0.0);
        self.young = 1.0;
    }

    /// Echo return loss enhancement of the linear filter in dB: how much
    /// weaker the error is than the capture while the far end talks (and
    /// the near end does not), smoothed over about 0.3 s. 0 until the far
    /// end has been heard.
    pub fn erle_db(&self) -> f32 {
        if self.erle_e <= 0.0 || self.erle_d <= 0.0 {
            return 0.0;
        }
        (10.0 * Float::log10(self.erle_d / self.erle_e)).clamp(-30.0, 80.0)
    }

    /// True while both ends talk at once: the far end plays and the capture
    /// holds clearly more than the residual echo and noise that are
    /// expected, which neither filter can remove.
    pub fn double_talk(&self) -> bool {
        self.dt
    }

    /// True once the filter has converged (10 dB of echo reduction reached;
    /// cleared when the filter is reset after diverging).
    pub fn converged(&self) -> bool {
        self.adapted
    }

    /// The delay from the reference to its echo in the capture, in
    /// milliseconds: to the sample from the converged filter's largest tap,
    /// from the delay estimator before that (to a fraction of a block),
    /// `None` while unknown.
    pub fn echo_delay_ms(&self) -> Option<f32> {
        let samples = if self.adapted && self.w.scanned >= self.parts && self.erle_db() > 6.0 {
            let mut p = 0;
            for (i, &v) in self.w.peak.iter().enumerate() {
                if v > self.w.peak[p] {
                    p = i;
                }
            }
            (self.delay + p) * self.block + self.w.peak_at[p] as usize
        } else {
            return Some(self.delay_est.estimate()? * (self.block as f32 * 1000.0 / self.rate as f32));
        };
        Some(samples as f32 * 1000.0 / self.rate as f32)
    }

    /// Cancels the echo of `reference` (what the loudspeakers played at the
    /// same time, give or take up to 500 ms) in `capture` (the microphone
    /// signal) and writes the result to `out`, which lags the capture by
    /// [`latency_samples`](EchoCanceller::latency_samples). Frames may have
    /// any length; `out.len()` samples are processed, and missing capture
    /// or reference samples count as silence.
    pub fn process(&mut self, capture: &[i16], reference: &[i16], out: &mut [i16]) {
        let n = out.len();
        let (mut i, mut o) = (0, 0);
        while i < n {
            let take = (self.block - self.fill).min(n - i);
            for j in 0..take {
                let c = capture.get(i + j).copied().unwrap_or(0);
                let r = reference.get(i + j).copied().unwrap_or(0);
                self.cap_peak = self.cap_peak.max((c as i32).abs());
                self.cap_in[self.fill + j] = self.dc_cap.process(c as f32 * (1.0 / 32768.0));
                self.ref_in[self.fill + j] = self.dc_ref.process(r as f32 * (1.0 / 32768.0));
            }
            self.fill += take;
            i += take;
            if self.fill == self.block {
                self.process_block();
                self.fill = 0;
                self.cap_peak = 0;
            }
            while o < i {
                let Some(v) = self.out.pop() else { break };
                out[o] = v;
                o += 1;
            }
        }
        out[o..].fill(0);
    }

    /// History slot of the reference spectrum that partition `p` multiplies.
    #[inline]
    fn slot(&self, p: usize) -> usize {
        self.slot_at(self.delay, p)
    }

    /// History slot of the reference spectrum that partition `p` multiplies
    /// with the bulk delay `delay`.
    #[inline]
    fn slot_at(&self, delay: usize, p: usize) -> usize {
        (self.head + 2 * self.hist_len - delay - p) % self.hist_len
    }

    fn process_block(&mut self) {
        let (b, nk, np) = (self.block, self.bins, self.parts);
        self.blocks = self.blocks.wrapping_add(1);

        // The reference spectrum of the last 2B samples joins the history.
        self.head = (self.head + 1) % self.hist_len;
        let slot = self.head * nk;
        let ref_zero = self.ref_in.iter().all(|&v| v == 0.0);
        let silent = ref_zero && self.ref_prev_zero;
        if silent {
            self.x_re[slot..slot + nk].fill(0.0);
            self.x_im[slot..slot + nk].fill(0.0);
            self.x_pow[slot..slot + nk].fill(0.0);
        } else {
            self.time[..b].copy_from_slice(&self.ref_prev);
            self.time[b..].copy_from_slice(&self.ref_in);
            self.fft.forward(&self.time, &mut self.x_re[slot..slot + nk], &mut self.x_im[slot..slot + nk]);
            let (re, im) = (&self.x_re[slot..slot + nk], &self.x_im[slot..slot + nk]);
            for ((p, &r), &i) in self.x_pow[slot..slot + nk].iter_mut().zip(re).zip(im) {
                *p = r * r + i * i;
            }
        }
        self.x_silent[self.head] = silent;
        self.ref_prev.copy_from_slice(&self.ref_in);
        self.ref_prev_zero = ref_zero;
        self.delay_est.push_reference(&self.x_pow[slot..slot + nk], silent);

        // Reference power over the span the filter sees: the window that
        // enters as partition 0 is added and the one that leaves removed;
        // now and then (and after a realignment) everything is summed anew.
        if self.norm_stale || self.blocks.is_multiple_of(64) {
            self.norm.fill(0.0);
            self.norm_live = 0;
            for p in 0..np {
                let s = self.slot(p);
                self.norm_live += usize::from(!self.x_silent[s]);
                for (a, &v) in self.norm.iter_mut().zip(&self.x_pow[s * nk..(s + 1) * nk]) {
                    *a += v;
                }
            }
            self.norm_stale = false;
        } else {
            let (enter, leave) = (self.slot(0), self.slot(np));
            if !self.x_silent[enter] {
                self.norm_live += 1;
                for (a, &v) in self.norm.iter_mut().zip(&self.x_pow[enter * nk..(enter + 1) * nk]) {
                    *a += v;
                }
            }
            if !self.x_silent[leave] {
                self.norm_live -= 1;
                for (a, &v) in self.norm.iter_mut().zip(&self.x_pow[leave * nk..(leave + 1) * nk]) {
                    *a = (*a - v).max(0.0);
                }
            }
            if self.norm_live == 0 {
                self.norm.fill(0.0);
            }
        }
        let filtering = self.norm_live > 0;
        let far_energy: f32 = if filtering { self.norm.iter().sum() } else { 0.0 };
        // Parseval: bins 0..=B of a 2B window hold about half of 2B x energy.
        let far_ms = far_energy / (np as f32 * (2 * b * b) as f32);
        self.far_active = far_ms > FAR_FLOOR;

        // While nothing has played for a while the canceller idles: no
        // filtering, and the capture spectrum only every fourth block, for
        // the noise estimate.
        let idle = !filtering && !self.delay_est.reference_active();
        core::mem::swap(&mut self.d_re, &mut self.dp_re);
        core::mem::swap(&mut self.d_im, &mut self.dp_im);
        core::mem::swap(&mut self.e_re, &mut self.ep_re);
        core::mem::swap(&mut self.e_im, &mut self.ep_im);
        core::mem::swap(&mut self.err, &mut self.err_prev);
        if idle && !self.blocks.is_multiple_of(4) {
            self.err.copy_from_slice(&self.cap_in);
            self.spectra_valid = false;
            self.dt = false;
            self.dt_hold = 0;
            self.suppress_and_output(false);
            self.control_alignment();
            return;
        }
        if !self.spectra_valid && !idle {
            // The last block was skipped, and its spectra are needed for the
            // windowed frames and the delay estimator (its error was the
            // capture).
            self.time[..b].fill(0.0);
            self.time[b..].copy_from_slice(&self.err_prev);
            self.fft.forward(&self.time, &mut self.dp_re, &mut self.dp_im);
            self.ep_re.copy_from_slice(&self.dp_re);
            self.ep_im.copy_from_slice(&self.dp_im);
        }
        self.spectra_valid = true;

        // Capture spectrum (block zero-padded in front).
        self.time[..b].fill(0.0);
        self.time[b..].copy_from_slice(&self.cap_in);
        self.fft.forward(&self.time, &mut self.d_re, &mut self.d_im);

        // Echo estimates (overlap-save) and errors of both filters, which are
        // then compared.
        if filtering {
            for p in 0..=np {
                self.slots[p] = self.slot(p);
            }
            for p in 0..np {
                self.sh_slots[p] = self.slot_at(self.sh_delay, p);
            }
            let quarter = (self.blocks % 4) as usize;
            self.w.refresh_norms(quarter);
            self.sh.refresh_norms(quarter);
            filter_error(
                &mut self.fft,
                &self.w,
                &self.x_re,
                &self.x_im,
                &self.slots,
                &self.cap_in,
                &mut self.s1_re,
                &mut self.s1_im,
                &mut self.time,
                &mut self.err,
                &mut self.e_re,
                &mut self.e_im,
            );
            filter_error(
                &mut self.fft,
                &self.sh,
                &self.x_re,
                &self.x_im,
                &self.sh_slots,
                &self.cap_in,
                &mut self.s1_re,
                &mut self.s1_im,
                &mut self.time,
                &mut self.sh_err,
                &mut self.sh_e_re,
                &mut self.sh_e_im,
            );
            self.compare_filters();
        } else {
            self.err.copy_from_slice(&self.cap_in);
            self.e_re.copy_from_slice(&self.d_re);
            self.e_im.copy_from_slice(&self.d_im);
        }

        // Block powers: capture, error and echo estimate (capture - error).
        let (mut sd, mut se, mut sy) = (0.0f32, 0.0f32, 0.0f32);
        for k in 0..nk {
            let (dr, di, er, ei) = (self.d_re[k], self.d_im[k], self.e_re[k], self.e_im[k]);
            let (yr, yi) = (dr - er, di - ei);
            let (pd, pe, py) = (dr * dr + di * di, er * er + ei * ei, yr * yr + yi * yi);
            self.pd[k] = pd;
            self.pe[k] = pe;
            self.py[k] = py;
            sd += pd;
            se += pe;
            sy += py;
        }
        if !(sd.is_finite() && se.is_finite() && sy.is_finite()) {
            // Cannot happen with finite input; never let it reach the output.
            self.reset();
            for _ in 0..b {
                self.out.push(0);
            }
            return;
        }

        // Leakage: regress the error power's fluctuations on the echo
        // estimate's. Not during double talk: the near end's fluctuations
        // (and its starts and ends) would only add noise.
        if self.far_active && filtering && !self.dt {
            let (mut cey, mut cyy) = (0.0f32, 0.0f32);
            for k in 0..nk {
                let de = self.pe[k] - self.mpe[k];
                let dy = self.py[k] - self.mpy[k];
                cey += de * dy;
                cyy += dy * dy;
            }
            self.cov_ey = self.leak_keep * self.cov_ey + cey;
            self.var_yy = self.leak_keep * self.var_yy + cyy;
            self.leak_blocks += 1;
            if self.leak_blocks > 8 && self.var_yy > 0.0 {
                self.leak = (self.cov_ey / self.var_yy).clamp(LEAK_MIN, LEAK_MAX);
            }
        }
        let c = self.c_mean;
        for k in 0..nk {
            let (pe, py) = (self.pe[k], self.py[k]);
            let mut me = self.mpe[k] + c * (pe - self.mpe[k]);
            let mut my = self.mpy[k] + c * (py - self.mpy[k]);
            if me < TINY {
                me = 0.0;
            }
            if my < TINY {
                my = 0.0;
            }
            self.mpe[k] = me;
            self.mpy[k] = my;
            // Noise floor: follow the smoothed error power down at once, up
            // slowly.
            let n = &mut self.noise[k];
            *n = if me < *n { me } else { (*n * self.noise_rise).min(NOISE_MAX) };
            *n = n.max(TINY);
        }

        if filtering {
            // Proportionate weights, and the far-end power weighted alike: the
            // normaliser, and the far-end power that the echo comes from.
            // Each filter's prior expects the echo where the delay estimator
            // finds it (at the front of the span while that is unknown).
            let est = self.delay_est.estimate();
            let first = |delay: usize| est.map_or(0, |q| (q as usize).saturating_sub(delay).min(np - 1));
            let (f, g) = (first(self.delay), first(self.sh_delay));
            if f != self.prior_first {
                prior(&mut self.prior, f, self.prior_r);
                self.prior_first = f;
            }
            if g != self.sh_prior_first {
                prior(&mut self.sh_prior, g, self.prior_r);
                self.sh_prior_first = g;
            }
            self.w.weights(&self.prior, &mut self.prop);
            self.sh.weights(&self.sh_prior, &mut self.sh_prop);
            self.normw.fill(0.0);
            self.sh_normw.fill(0.0);
            let mut top = 0;
            for p in 0..np {
                let (s, g) = (self.slots[p] * nk, self.prop[p]);
                for (a, &v) in self.normw.iter_mut().zip(&self.x_pow[s..s + nk]) {
                    *a += g * v;
                }
                let (s, h) = (self.sh_slots[p] * nk, self.sh_prop[p]);
                for (c, &v) in self.sh_normw.iter_mut().zip(&self.x_pow[s..s + nk]) {
                    *c += h * v;
                }
                if self.prop[p] > self.prop[top] {
                    top = p;
                }
            }
            // The far end's state: at the main partition well above or below
            // its average over the span.
            let s = self.slot(top) * nk;
            let avg = 1.0 / np as f32;
            for ((o, &x), &n) in self.state.iter_mut().zip(&self.x_pow[s..s + nk]).zip(&self.norm) {
                *o = if x > 3.0 * avg * n {
                    1
                } else if 3.0 * x < avg * n {
                    2
                } else {
                    0
                };
            }
        }

        // Adaptation.
        let clipped = self.cap_peak >= CLIP;
        if self.far_active && !clipped {
            self.adapt(far_energy);
            self.adapt_shadow(far_energy);
        }

        // Statistics: double talk, echo reduction, echo path gain,
        // divergence.
        if self.far_active {
            self.young *= self.young_keep;
            self.update_statistics(sd, se);
        } else {
            self.dt = false;
            self.dt_hold = 0;
        }

        // Delay estimation from the capture's 2B window.
        if self.delay_est.reference_active() {
            for k in 0..nk {
                let s = if k & 1 == 0 { 1.0 } else { -1.0 };
                let (r, i) = (self.d_re[k] + s * self.dp_re[k], self.d_im[k] + s * self.dp_im[k]);
                self.s2_re[k] = r * r + i * i;
            }
            self.delay_est.push_capture(&self.s2_re, !self.working);
        }

        self.suppress_and_output(filtering);
        self.control_alignment();
    }
}
