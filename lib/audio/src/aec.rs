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

use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

use num_traits::Float;

use crate::fft::RealFft;
use crate::mix::f32_to_i16;

/// Supported sample rates.
const MIN_RATE: u32 = 8_000;
const MAX_RATE: u32 = 48_000;
/// Longest bulk delay between reference and capture that is searched.
const MAX_DELAY_MS: u32 = 500;
/// Limits of the modelled echo tail.
const MIN_TAIL_MS: u32 = 20;
const MAX_TAIL_MS: u32 = 1000;

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
/// Double talk: the error exceeds the expected residual echo by this power
/// ratio (9 dB). A filter works while it reduces the capture by as much.
const DT_EXCESS: f32 = 8.0;
/// The near end may be talking (6 dB): the suppressor stays gentle.
const NEAR_EXCESS: f32 = 4.0;
/// Residual echo over-estimation in the suppressor, and while the near end
/// may be talking.
const OVER_SUPPRESS: f32 = 3.0;
const OVER_SUPPRESS_DT: f32 = 2.0;
/// Lowest suppressor gain (-40 dB).
const GAIN_FLOOR: f32 = 0.01;
/// Initial share of remembered echo power that late reverberation leaves in
/// the error (-20 dB), until the ends of echoes have been measured.
const REV_RATIO: f32 = 0.01;
/// Far-end activity: mean square of the reference over the modelled span
/// (-66 dBFS).
const FAR_FLOOR: f32 = 2.5e-7;
/// Capture samples at or above this magnitude mark a clipped block.
const CLIP: i32 = 32_000;
/// Values below this are flushed to zero (avoids denormals).
const TINY: f32 = 1e-15;
/// Start (and ceiling) of the noise floor estimates, far above any block
/// power of 16-bit audio.
const NOISE_MAX: f32 = 1e9;
/// Bands of the echo path gain estimate.
const GAIN_BANDS: usize = 16;
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

/// A first-order DC-blocking high-pass filter.
#[derive(Debug, Clone, Copy)]
struct DcBlock {
    pole: f32,
    x1: f32,
    y1: f32,
}

impl DcBlock {
    fn new(rate: u32) -> DcBlock {
        DcBlock { pole: 1.0 - core::f32::consts::TAU * 30.0 / rate as f32, x1: 0.0, y1: 0.0 }
    }

    #[inline]
    fn run(&mut self, x: f32) -> f32 {
        let mut y = x - self.x1 + self.pole * self.y1;
        if y.abs() < TINY {
            y = 0.0;
        }
        self.x1 = x;
        self.y1 = y;
        y
    }
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
struct DelayEstimator {
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
    fn new(rate: u32, block: usize, candidates: usize) -> DelayEstimator {
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

    fn reset(&mut self) {
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
    fn push_reference(&mut self, pow: &[f32], silent: bool) {
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
    fn reference_active(&self) -> bool {
        self.ref_flux_energy.iter().any(|&e| e > 0.0)
    }

    /// Correlates the capture block's onsets (from the power spectrum of its
    /// 2B window) with the reference history. `searching`: the filters do not
    /// work, and a new delay is accepted sooner.
    fn push_capture(&mut self, pow: &[f32], searching: bool) {
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
    fn estimate(&self) -> Option<f32> {
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
    fn forget(&mut self) {
        self.slow.copy_from(&self.fast);
    }
}

/// The coefficients of a partitioned frequency-domain filter (P spectra of
/// B + 1 bins) and what is known of them per partition: the energy, largest
/// tap and its position of the impulse response (refreshed as the gradient
/// constraint visits the partitions in turn) and the spectral energy (a
/// quarter refreshed per block).
struct Partitions {
    re: Vec<f32>,
    im: Vec<f32>,
    next_constrain: usize,
    energy: Vec<f32>,
    peak: Vec<f32>,
    peak_at: Vec<u32>,
    /// Partitions visited since the last reset or realignment.
    scanned: usize,
    norm: Vec<f32>,
}

impl Partitions {
    fn new(parts: usize, bins: usize) -> Partitions {
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

    fn parts(&self) -> usize {
        self.energy.len()
    }

    fn clear(&mut self) {
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
    fn copy_from(&mut self, other: &Partitions) {
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
    fn strongest(&self) -> usize {
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
    fn refresh_norms(&mut self, first: usize) {
        let nk = self.re.len() / self.parts();
        for p in (first..self.parts()).step_by(4) {
            self.norm[p] = energy(&self.re[p * nk..(p + 1) * nk], &self.im[p * nk..(p + 1) * nk]);
        }
    }

    /// Proportionate weights (summing to 1): half the prior, half the
    /// partitions' shares of the filter (by the square root of their
    /// spectral energy).
    fn weights(&self, prior: &[f32], out: &mut [f32]) {
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
    fn shift(&mut self, d: isize) {
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
    fn take(&mut self, p: usize, src: isize) {
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
    fn constrain_next(&mut self, fft: &mut RealFft, time: &mut [f32]) {
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
fn filter_error(
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

/// An acoustic echo canceller for one mono capture stream (see the module
/// documentation).
///
/// ```
/// use vaudio::aec::EchoCanceller;
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
        let hann =
            (0..n).map(|i| (0.5 - 0.5 * Float::cos(core::f64::consts::TAU * i as f64 / n as f64)) as f32).collect();
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
            dc_cap: DcBlock::new(rate),
            dc_ref: DcBlock::new(rate),
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
    /// suppression, no spectral processing of the near end.
    pub fn set_suppression(&mut self, on: bool) {
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
        self.dc_cap = DcBlock::new(self.rate);
        self.dc_ref = DcBlock::new(self.rate);
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

    /// True while the reference holds sound within the modelled echo span.
    pub fn far_end_active(&self) -> bool {
        self.far_active
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
                self.cap_in[self.fill + j] = self.dc_cap.run(c as f32 * (1.0 / 32768.0));
                self.ref_in[self.fill + j] = self.dc_ref.run(r as f32 * (1.0 / 32768.0));
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

    /// One step of the adaptive filter (see the module documentation on the
    /// step control).
    fn adapt(&mut self, far_energy: f32) {
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
    fn adapt_shadow(&mut self, far_energy: f32) {
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
    fn compare_filters(&mut self) {
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

    /// Double talk, the echo reduction per bin, the echo path gain and the
    /// divergence test (while the far end is active).
    fn update_statistics(&mut self, sd: f32, se: f32) {
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
    fn inv_erle(&self, k: usize) -> f32 {
        let i = k + self.state[k] as usize * self.bins;
        let measured = if self.bin_d[i] > 0.0 { (self.bin_e[i] / self.bin_d[i]).clamp(1e-5, 1.0) } else { 1.0 };
        measured.max(self.young)
    }

    /// Suppresses the residual echo in the frame that ends with this block
    /// and emits the previous block of output.
    fn suppress_and_output(&mut self, filtering: bool) {
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

    /// Moves the reference alignment so that the echo path fits the filters:
    /// by the converged filter's energy profile while it works, or else by
    /// the delay estimator when the echo now starts before the modelled span
    /// (where no filter can follow it) or late in it.
    fn control_alignment(&mut self) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testsig::*;
    use alloc::format;
    use rand::Rng;
    use std::println;

    /// The sound at the microphone, split by source.
    struct Scene {
        rate: u32,
        far: Vec<f32>,
        echo: Vec<f32>,
        near: Vec<f32>,
        noise: Vec<f32>,
    }

    impl Scene {
        fn capture(&self) -> Vec<i16> {
            let mix: Vec<f32> = (0..self.far.len()).map(|i| self.echo[i] + self.near[i] + self.noise[i]).collect();
            to_i16(&mix)
        }

        fn reference(&self) -> Vec<i16> {
            to_i16(&self.far)
        }

        fn secs(&self, t: f32) -> usize {
            ((t * self.rate as f32) as usize).min(self.far.len())
        }

        fn span(&self, t0: f32, t1: f32) -> core::ops::Range<usize> {
            self.secs(t0)..self.secs(t1)
        }
    }

    /// A mildly distorting loudspeaker (about -30 dB of harmonics at full
    /// voice).
    fn speaker(x: &[f32]) -> Vec<f32> {
        x.iter().map(|&v| v + 0.05 * v * v - 0.1 * v * v * v).collect()
    }

    /// The assistant talking through a loudspeaker into a room: far-end
    /// speech, its echo after a bulk delay, background noise at -60 dBFS.
    fn far_only(rate: u32, seconds: f32, delay_ms: f32, rt60: f32, seed: u64) -> Scene {
        let far = speech(rate, seconds, FEMALE, MONOLOGUE, seed).samples;
        let h = room(rate, rt60, rt60 * 1.1, seed + 100);
        let mut echo = convolve(&speaker(&far), &h);
        for v in &mut echo {
            *v *= 0.5;
        }
        let echo = delay(&echo, (delay_ms * rate as f32 / 1000.0) as usize);
        let n = far.len();
        Scene { rate, far, echo, near: vec![0.0; n], noise: white(n, 0.001, seed + 200) }
    }

    /// `far_only` plus the user talking from `t0` to `t1`.
    fn double_talk_scene(seconds: f32, t0: f32, t1: f32, near_level: f32, seed: u64) -> Scene {
        let mut scene = far_only(16_000, seconds, 40.0, 0.25, seed);
        let voice = Voice { level: near_level, ..MALE };
        let talk = speech(16_000, t1 - t0, voice, Pacing { lead_in: 0.0, ..CONVERSATION }, seed + 7).samples;
        let start = scene.secs(t0);
        for (n, v) in scene.near[start..].iter_mut().zip(&talk) {
            *n = *v;
        }
        scene
    }

    /// Far-end speech; at `t_change` the room changes (and with it the echo
    /// gain by `gain1`), and the bulk delay changes from `d0` to `d1` ms.
    fn path_change_scene(seconds: f32, t_change: f32, d0: f32, d1: f32, gain1: f32, seed: u64) -> Scene {
        let rate = 16_000;
        let far = speech(rate, seconds, FEMALE, MONOLOGUE, seed).samples;
        let n = far.len();
        let loud = speaker(&far);
        let ms = |d: f32| (d * rate as f32 / 1000.0) as usize;
        let e0 = delay(&convolve(&loud, &room(rate, 0.25, 0.27, seed + 100)), ms(d0));
        let e1 = delay(&convolve(&loud, &room(rate, 0.2, 0.22, seed + 300)), ms(d1));
        let cut = (t_change * rate as f32) as usize;
        let echo = (0..n).map(|i| if i < cut { 0.5 * e0[i] } else { 0.5 * gain1 * e1[i] }).collect();
        Scene { rate, far, echo, near: vec![0.0; n], noise: white(n, 0.001, seed + 200) }
    }

    /// Runs the canceller in 10 ms frames. Returns the output aligned with
    /// the capture, and per frame whether double talk was reported.
    fn run(aec: &mut EchoCanceller, capture: &[i16], reference: &[i16]) -> (Vec<f32>, Vec<bool>) {
        let frame = (aec.rate() / 100) as usize;
        let mut out = vec![0i16; capture.len()];
        let mut dt = Vec::new();
        for ((c, r), o) in capture.chunks(frame).zip(reference.chunks(frame)).zip(out.chunks_mut(frame)) {
            aec.process(c, r, o);
            dt.push(aec.double_talk());
        }
        let lat = aec.latency_samples();
        let mut aligned = to_f32(&out[lat.min(out.len())..]);
        aligned.resize(capture.len(), 0.0);
        (aligned, dt)
    }

    /// A scene processed with the suppressor (`total`, output `out`,
    /// double-talk reports `dt`) and without it (`linear`, output `lin`).
    struct Runs {
        total: EchoCanceller,
        out: Vec<f32>,
        dt: Vec<bool>,
        linear: EchoCanceller,
        lin: Vec<f32>,
    }

    fn run_both(scene: &Scene) -> Runs {
        let (cap, refr) = (scene.capture(), scene.reference());
        let mut total = EchoCanceller::new(scene.rate, 250);
        let (out, dt) = run(&mut total, &cap, &refr);
        let mut linear = EchoCanceller::new(scene.rate, 250);
        linear.set_suppression(false);
        let (lin, _) = run(&mut linear, &cap, &refr);
        Runs { total, out, dt, linear, lin }
    }

    /// Echo reduction (capture power over output power) in dB over [t0, t1).
    fn erle(scene: &Scene, out: &[f32], t0: f32, t1: f32) -> f64 {
        let r = scene.span(t0, t1);
        let cap = to_f32(&scene.capture()[r.clone()]);
        db(power(&cap) / power(&out[r]).max(1e-20))
    }

    /// The first time after `from` from which the echo reduction over each
    /// of the next four 0.5 s windows is at least `db` (`None` if never).
    fn converged_at(scene: &Scene, out: &[f32], from: f32, db: f64) -> Option<f32> {
        let end = scene.far.len() as f32 / scene.rate as f32;
        let mut t = from;
        while t + 2.0 <= end {
            if (0..4).all(|i| erle(scene, out, t + 0.5 * i as f32, t + 0.5 * (i + 1) as f32) >= db) {
                return Some(t);
            }
            t += 0.1;
        }
        None
    }

    #[test]
    fn cancels_echo_and_finds_the_delay() {
        let cases: Vec<(f32, u64)> =
            [0.0f32, 40.0, 120.0, 300.0].into_iter().flat_map(|d| seeds(1).map(move |s| (d, s))).collect();
        for_cases("far end only (bulk delay ms, seed)", &cases, |&(delay_ms, seed)| {
            let scene = far_only(16_000, 8.0, delay_ms, 0.2, seed);
            let Runs { total, out, linear, lin, .. } = run_both(&scene);
            let (e_total, e_linear) = (erle(&scene, &out, 2.5, 8.0), erle(&scene, &lin, 2.5, 8.0));
            let found = total.echo_delay_ms();
            let line = format!(
                "ERLE 2.5-8 s total {e_total:.1} dB, linear {e_linear:.1} dB (reported {:.1} dB); total >= 20 dB from \
                 {:?} s, linear >= 15 dB from {:?} s; delay found {found:?} ms",
                linear.erle_db(),
                converged_at(&scene, &out, 0.0, 20.0),
                converged_at(&scene, &lin, 0.0, 15.0),
            );
            let mut t = Targets::new();
            t.check(e_total >= 20.0, || format!("total ERLE {e_total:.1} dB"));
            let want = if delay_ms < 200.0 { 20.0 } else { 15.0 };
            t.check(e_linear >= want, || format!("linear ERLE {e_linear:.1} dB"));
            t.check(found.is_some_and(|f| (f - delay_ms).abs() <= 5.0), || format!("delay found {found:?} ms"));
            t.check(total.converged() && !total.double_talk(), || "not converged, or double talk reported".into());
            (line, t.0)
        });
    }

    #[test]
    fn double_talk_keeps_the_near_end_and_the_filter() {
        let cases: Vec<(f32, u64)> =
            [0.03f32, 0.06, 0.12].into_iter().flat_map(|l| seeds(3).map(move |s| (l, s))).collect();
        for_cases("double talk (near-end level, seed)", &cases, |&(near_level, seed)| {
            let scene = double_talk_scene(13.0, 5.0, 9.0, near_level, seed);
            let Runs { total, out, dt, linear, lin } = run_both(&scene);
            let talk = scene.span(5.0, 9.0);
            let ser = db(power(&scene.near[talk.clone()]) / power(&scene.echo[talk.clone()]));
            let corr = correlation(&out[talk.clone()], &scene.near[talk.clone()]);
            let before = erle(&scene, &out, 2.5, 5.0);
            let (after, after_linear) = (erle(&scene, &out, 9.5, 13.0), erle(&scene, &lin, 9.5, 13.0));
            // Frames are 10 ms; double talk lasts from 5 to 9 s.
            let during = dt[500..900].iter().filter(|&&d| d).count() as f32 / 400.0;
            let outside = dt[250..450].iter().chain(&dt[1000..1300]).filter(|&&d| d).count() as f32 / 500.0;
            let line = format!(
                "near end at {ser:.1} dB to the echo: near-end correlation {corr:.3}; ERLE before {before:.1} dB, \
                 after: total {after:.1} dB, linear {after_linear:.1} dB (reported {:.1} dB); double talk reported in \
                 {:.0} % of its frames, {:.0} % of others",
                linear.erle_db(),
                100.0 * during,
                100.0 * outside
            );
            let mut t = Targets::new();
            t.check(corr > 0.8, || format!("near-end correlation {corr:.3}"));
            t.check(after >= 20.0 && after_linear >= 15.0, || {
                format!("after double talk: {after:.1} / {after_linear:.1} dB")
            });
            t.check(during >= 0.3 && outside <= 0.15, || format!("double-talk flag {during} / {outside}"));
            t.check(total.converged(), || "not converged".into());
            (line, t.0)
        });
    }

    #[test]
    fn recovers_after_echo_path_changes() {
        // (delay before, delay after, echo gain after): a new room, a new
        // room twice as loud, a longer and a shorter bulk delay.
        let changes = [(40.0f32, 40.0f32, 1.0f32), (40.0, 40.0, 2.0), (40.0, 160.0, 1.0), (120.0, 20.0, 0.5)];
        let cases: Vec<((f32, f32, f32), u64)> =
            changes.into_iter().flat_map(|c| seeds(5).map(move |s| (c, s))).collect();
        for_cases(
            "echo path change at 6 s ((ms before, ms after, gain after), seed)",
            &cases,
            |&((d0, d1, gain1), seed)| {
                let scene = path_change_scene(12.0, 6.0, d0, d1, gain1, seed);
                let Runs { total, out, lin, .. } = run_both(&scene);
                let before = erle(&scene, &out, 4.0, 6.0);
                let soon = erle(&scene, &out, 7.5, 8.5);
                let (later, later_linear) = (erle(&scene, &out, 8.0, 12.0), erle(&scene, &lin, 8.0, 12.0));
                let found = total.echo_delay_ms();
                let line = format!(
                    "ERLE before {before:.1} dB; 1.5-2.5 s after {soon:.1} dB; 2-6 s after: total {later:.1} dB, linear \
                 {later_linear:.1} dB; total >= 20 dB again from {:?} s; delay found {found:?} ms",
                    converged_at(&scene, &out, 6.0, 20.0),
                );
                let mut t = Targets::new();
                t.check(before >= 20.0, || format!("before the change: {before:.1} dB"));
                t.check(soon >= 20.0, || format!("1.5 to 2.5 s after the change: {soon:.1} dB"));
                t.check(later >= 20.0 && later_linear >= 12.0, || {
                    format!("after the change: {later:.1} / {later_linear:.1} dB")
                });
                t.check(found.is_some_and(|f| (f - d1).abs() <= 5.0), || format!("delay {d1} ms: found {found:?} ms"));
                (line, t.0)
            },
        );
    }

    #[test]
    fn passes_the_near_end_when_there_is_no_echo() {
        // Headphones: the far end plays, the microphone hears only the user.
        let cases: Vec<u64> = seeds(11).collect();
        for_cases("no echo (seed)", &cases, |&seed| {
            let rate = 16_000;
            let n = 8 * rate as usize;
            let far = speech(rate, 8.0, FEMALE, MONOLOGUE, seed).samples;
            let near = speech(rate, 8.0, MALE, CONVERSATION, seed + 1).samples;
            let scene = Scene { rate, far, echo: vec![0.0; n], near, noise: white(n, 0.001, seed + 2) };
            let mut aec = EchoCanceller::new(rate, 250);
            let (out, _) = run(&mut aec, &scene.capture(), &scene.reference());
            let cap = to_f32(&scene.capture());
            let corr = correlation(&out[..n - 512], &cap[..n - 512]);
            let ratio = db(power(&out) / power(&cap));
            let line = format!(
                "output to capture correlation {corr:.4}, level {ratio:+.2} dB; echo seen {}, delay estimate {:?}",
                aec.echo_seen,
                aec.delay_est.estimate()
            );
            let mut t = Targets::new();
            t.check(corr > 0.99 && ratio.abs() < 0.5, || format!("correlation {corr:.4}, level {ratio:+.2} dB"));
            (line, t.0)
        });
    }

    #[test]
    fn works_at_48_khz() {
        let cases: Vec<u64> = seeds(9).collect();
        for_cases("48 kHz, bulk delay 60 ms (seed)", &cases, |&seed| {
            let scene = far_only(48_000, 6.0, 60.0, 0.25, seed);
            let Runs { total, out, lin, .. } = run_both(&scene);
            let (e_total, e_linear) = (erle(&scene, &out, 2.5, 6.0), erle(&scene, &lin, 2.5, 6.0));
            let found = total.echo_delay_ms();
            let line = format!(
                "block {}: ERLE 2.5-6 s total {e_total:.1} dB, linear {e_linear:.1} dB; delay found {found:?} ms",
                total.block_size()
            );
            let mut t = Targets::new();
            t.check(e_total >= 20.0 && e_linear >= 15.0, || format!("ERLE {e_total:.1} / {e_linear:.1} dB"));
            t.check(found.is_some_and(|f| (f - 60.0).abs() <= 5.0), || format!("delay found {found:?} ms"));
            (line, t.0)
        });
    }

    #[test]
    fn output_lags_by_the_documented_latency() {
        let mut aec = EchoCanceller::new(16_000, 250);
        let mut capture = vec![0i16; 4000];
        capture[1000] = 20_000;
        let mut out = vec![0i16; 4000];
        for (c, o) in capture.chunks(160).zip(out.chunks_mut(160)) {
            aec.process(c, &[], o);
        }
        let peak = (0..out.len()).max_by_key(|&i| out[i].unsigned_abs()).unwrap();
        assert_eq!(peak, 1000 + aec.latency_samples());
        assert_eq!(aec.latency_samples(), 255);
    }

    fn assert_sane(aec: &EchoCanceller) {
        let finite = |v: &[f32]| v.iter().all(|x| x.is_finite());
        assert!(finite(&aec.w.re) && finite(&aec.w.im) && finite(&aec.sh.re) && finite(&aec.sh.im));
        assert!(finite(&aec.gain) && finite(&aec.rev));
        assert!(aec.erle_db().is_finite() && aec.leak.is_finite() && aec.rev_ratio.is_finite());
        assert!(aec.echo_delay_ms().is_none_or(|d| d.is_finite()));
        assert!(aec.gain.iter().all(|&g| (0.0..=1.0).contains(&g)));
    }

    /// Feeds hostile input: silence, full-scale square waves with clipped
    /// echo, large DC offsets, unrelated full-scale noise, odd frame sizes.
    fn hostile(aec: &mut EchoCanceller) {
        let rate = aec.rate();
        let mut rng = generator(99);
        let feed = |aec: &mut EchoCanceller, cap: &[i16], refr: &[i16]| -> Vec<i16> {
            let mut out = vec![0i16; cap.len()];
            for ((c, r), o) in cap.chunks(160).zip(refr.chunks(160)).zip(out.chunks_mut(160)) {
                aec.process(c, r, o);
            }
            out
        };
        // Silence stays silence.
        let zeros = vec![0i16; 2 * rate as usize];
        assert!(feed(aec, &zeros, &zeros).iter().all(|&v| v == 0));
        // Full-scale square waves, clipped echo.
        let square: Vec<i16> = (0..3 * rate as usize).map(|i| if (i / 40) % 2 == 0 { 32767 } else { -32768 }).collect();
        let clipped: Vec<i16> = square.iter().map(|&v| (v as i32 * 3 / 2).clamp(-32768, 32767) as i16).collect();
        feed(aec, &clipped, &square);
        assert_sane(aec);
        // Large DC offsets are removed.
        let dc_cap = vec![20_000i16; rate as usize];
        let dc_ref = vec![-15_000i16; rate as usize];
        let out = feed(aec, &dc_cap, &dc_ref);
        let tail = to_f32(&out[rate as usize / 2..]);
        assert!(power(&tail) < 1e-4, "DC leaks: {}", power(&tail));
        assert_sane(aec);
        // Random full-scale noise on both, unrelated.
        let noise_a: Vec<i16> = (0..2 * rate as usize).map(|_| rng.next_u32() as i16).collect();
        let noise_b: Vec<i16> = (0..2 * rate as usize).map(|_| rng.next_u32() as i16).collect();
        feed(aec, &noise_a, &noise_b);
        assert_sane(aec);
        // Odd frame sizes and short or missing slices.
        for (i, len) in [0usize, 1, 7, 160, 333, 1000, 4096, 3].into_iter().cycle().take(40).enumerate() {
            let cap = &noise_a[..len];
            let refr = &noise_b[..len / (1 + i % 3)];
            let mut out = vec![1i16; len];
            aec.process(cap, refr, &mut out);
        }
        aec.process(&noise_a[..500], &noise_b[..500], &mut []);
        assert_sane(aec);
    }

    #[test]
    fn hostile_input_is_harmless() {
        // After all that, and after a reset, it still cancels echo.
        let cases: Vec<u64> = seeds(21).collect();
        for_cases("after hostile input and a reset (seed)", &cases, |&seed| {
            let rate = 16_000;
            let mut aec = EchoCanceller::new(rate, 250);
            hostile(&mut aec);
            aec.reset();
            let scene = far_only(rate, 5.0, 40.0, 0.2, seed);
            let (out, _) = run(&mut aec, &scene.capture(), &scene.reference());
            let e = erle(&scene, &out, 2.5, 5.0);
            let mut t = Targets::new();
            t.check(e >= 20.0, || format!("ERLE {e:.1} dB"));
            (format!("ERLE 2.5-5 s {e:.1} dB"), t.0)
        });
    }

    /// Processing time per second of audio on the host. Run with
    /// `cargo test --release -p vaudio aec::tests::bench -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn bench_cpu_time() {
        for (rate, block) in [(16_000u32, 128usize), (16_000, 256), (48_000, 512)] {
            let scene = far_only(rate, 10.0, 40.0, 0.25, 4);
            let (cap, refr) = (scene.capture(), scene.reference());
            let frame = (rate / 100) as usize;
            let mut out = vec![0i16; frame];
            let mut aec = EchoCanceller::with_block_size(rate, 250, block);
            let start = std::time::Instant::now();
            for (c, r) in cap.chunks(frame).zip(refr.chunks(frame)) {
                aec.process(c, r, &mut out[..c.len()]);
            }
            let busy = start.elapsed().as_secs_f64() / 10.0;
            // Idle: the loudspeakers are silent.
            let silent = vec![0i16; frame];
            let start = std::time::Instant::now();
            for c in cap.chunks(frame) {
                aec.process(c, &silent, &mut out[..c.len()]);
            }
            let idle = start.elapsed().as_secs_f64() / 10.0;
            println!(
                "{rate} Hz, block {block}, {} partitions (250 ms tail): {:.2} ms per second of audio while the far \
                 end plays, {:.3} ms while it is silent",
                aec.parts,
                busy * 1000.0,
                idle * 1000.0
            );
        }
    }
}
