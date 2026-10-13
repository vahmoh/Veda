// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Voice activity detection.
//!
//! [`Vad`] decides every 10 ms whether a mono 16-bit signal (16 kHz is the
//! main target; 8 to 48 kHz work) holds speech, for example to gate what a
//! voice assistant sends to a speech recogniser. Frames may have any length.
//!
//! Every 10 ms hop it analyses the latest 16 ms at 16 kHz (the longest power
//! of two within 25 ms; Hann window, real FFT) in up to 17 bands from 100 Hz
//! to 8 kHz:
//!
//! * **Noise floor.** Each band's smoothed power is tracked by *minimum
//!   statistics*: the minimum over the last 1.6 s (in eight sub-windows), so
//!   that the floor follows the background through ongoing speech, which
//!   always has pauses. The minimum of a fluctuating power lies below its
//!   mean, which a per-band bias (from the band's width) corrects. At the
//!   start the powers are averaged until they are as steady as the smoothed
//!   ones, and only then enter the minimum (a single unlucky power would set
//!   a narrow band's floor far too low). A sound that is loud (6 dB above
//!   the floor) but steady for 0.4 s, like a fan switching on, becomes the
//!   new floor at once: speech is never that steady.
//! * **Speech likelihood.** Per band, the a-posteriori SNR (power over
//!   noise) and the a-priori SNR (decision-directed: mostly the speech power
//!   estimated in the previous hop) give the log-likelihood ratio of speech
//!   against noise under Gaussian models. The mean over the strongest third
//!   of the bands (speech fills only some bands at a time, and a noise that
//!   rules others, like a fan's rumble, must not drown it) becomes a
//!   probability through a logistic curve whose midpoint the sensitivity
//!   moves.
//! * **Clicks.** A hop whose peak stands far above its RMS, or far above
//!   the RMS of its samples more than a millisecond away from the peak (an
//!   impulse, even in loud low-pitched noise), does not count as speech, nor
//!   do the next hops that still see it.
//!
//! Speech starts in a hop with a probability above 0.5 and continues while
//! the probability stays above 0.1 (the fading ends of syllables);
//! [`VadState`] stays speech for a hangover of 100 ms after that, which
//! bridges the gaps between syllables. [`Vad::speaking`] adds hysteresis for
//! whole utterances: it turns on after 100 ms of speech hops within 150 ms
//! and off after 600 ms without speech (both adjustable).
//!
//! The cost is one FFT of 256 points per 10 ms at 16 kHz and a few
//! operations per band; no allocation after construction.

use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

use num_traits::Float;

use crate::fft::RealFft;

/// Band edges in Hz (bands above the Nyquist frequency are dropped).
const EDGES: [f32; 18] = [
    100.0, 250.0, 400.0, 550.0, 700.0, 900.0, 1100.0, 1350.0, 1650.0, 2000.0, 2400.0, 2900.0, 3500.0, 4200.0, 5000.0,
    6000.0, 7200.0, 8000.0,
];
/// Sub-windows of the minimum statistics and their length in hops (1.6 s).
const MIN_WINDOWS: usize = 8;
const MIN_HOPS: u32 = 20;
/// Decision-directed a-priori SNR smoothing.
const DD_ALPHA: f32 = 0.95;
/// Logistic slope of the probability (per unit of log-likelihood ratio).
const SLOPE: f32 = 3.0;
/// Speech that has started continues while the probability is above this.
const CONTINUE: f32 = 0.1;
/// Hops that the verdict stays speech after the last speech hop (100 ms).
const HANGOVER: u32 = 10;
/// Peak-to-RMS power ratio above which a hop is a click (14 dB), and the
/// ratio of the peak to the RMS of the hop's samples more than a millisecond
/// away from it (16 dB): a click's energy stays near its peak, even in loud
/// low-pitched noise, which can hide a click's crest.
const CLICK_CREST: f32 = 25.0;
const CLICK_CREST_AWAY: f32 = 40.0;
/// Sound whose smoothed level in the speech band has stayed within this
/// many dB for `STEADY_HOPS` hops is steady, and becomes the floor when it
/// stands this power ratio (6 dB) above it.
const STEADY_DB: f32 = 3.0;
const STEADY_HOPS: usize = 40;
const STEADY_ABOVE: f32 = 4.0;
/// Lowest band power considered (about -110 dBFS).
const FLOOR_POWER: f32 = 1e-11;

/// The verdict for the latest 10 ms.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VadState {
    /// No speech.
    Silence,
    /// Speech, or less than 100 ms after it. `probability` is that of the
    /// latest hop, at least 0.5.
    Speech { probability: f32 },
}

impl VadState {
    /// True for [`VadState::Speech`].
    pub fn is_speech(&self) -> bool {
        matches!(self, VadState::Speech { .. })
    }
}

/// A voice activity detector (see the module documentation).
///
/// ```
/// use vaudio::vad::{Vad, VadState};
///
/// let mut vad = Vad::new(16_000);
/// assert_eq!(vad.process(&[0i16; 160]), VadState::Silence);
/// assert!(!vad.speaking());
/// ```
pub struct Vad {
    rate: u32,
    /// Samples per hop (10 ms) and per analysis window (a power of two).
    hop: usize,
    window: usize,
    fft: RealFft,
    hann: Vec<f32>,
    /// The latest `window` samples (a ring) and where the next one goes.
    ring: Vec<f32>,
    pos: usize,
    /// Samples received (saturating), samples into the current hop, and its
    /// sum of squares and peak.
    seen: usize,
    fill: usize,
    hop_energy: f32,
    hop_peak: f32,
    input: Vec<f32>,
    re: Vec<f32>,
    im: Vec<f32>,
    /// Bin range of every band, whether it lies in the 200 Hz to 4 kHz
    /// speech band, and its noise bias.
    bands: Vec<(usize, usize)>,
    speech_band: Vec<bool>,
    bias: Vec<f32>,
    /// Power normalisation (window energy).
    scale: f32,
    // Per band.
    power: Vec<f32>,
    smooth: Vec<f32>,
    noise: Vec<f32>,
    sub_min: Vec<f32>,
    mins: Vec<f32>,
    speech_power: Vec<f32>,
    /// The smoothed speech band level (dB) of the last `STEADY_HOPS` hops.
    levels: Vec<f32>,
    level_pos: usize,
    // Minimum statistics bookkeeping: hops after which the smoothed powers
    // enter the minimum (their running mean is as steady as the smoothing).
    settle_hops: u64,
    min_count: u32,
    min_slot: usize,
    hops: u64,
    // Decisions.
    probability: f32,
    snr_db: f32,
    click_hold: u32,
    hang: u32,
    history: u64,
    quiet_run: u32,
    speaking: bool,
    // Settings.
    threshold: f32,
    sensitivity: f32,
    start_hops: u32,
    start_window: u32,
    end_hops: u32,
    /// Per-hop coefficient of the band power smoothing.
    c_smooth: f32,
}

impl fmt::Debug for Vad {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Vad({} Hz, p {:.2}, SNR {:.1} dB, speaking {})",
            self.rate, self.probability, self.snr_db, self.speaking
        )
    }
}

impl Vad {
    /// A detector for `rate` Hz (clamped to 8000..=48000) with the default
    /// sensitivity (0.5) and timing (start after 100 ms, end after 600 ms).
    pub fn new(rate: u32) -> Vad {
        let rate = rate.clamp(8_000, 48_000);
        let hop = (rate / 100) as usize;
        // The largest power of two that lasts at most 25 ms (16 ms at 16 kHz).
        let mut window = 64;
        while window * 2 <= rate as usize / 40 {
            window *= 2;
        }
        let bin_hz = rate as f32 / window as f32;
        let nyquist = window / 2;
        let mut bands = Vec::new();
        let mut speech_band = Vec::new();
        let mut bias = Vec::new();
        for pair in EDGES.windows(2) {
            let a = (Float::round(pair[0] / bin_hz) as usize).max(1);
            let b = (Float::round(pair[1] / bin_hz) as usize).min(nyquist);
            if b <= a {
                continue;
            }
            bands.push((a, b));
            speech_band.push(pair[0] >= 200.0 && pair[1] <= 4200.0);
            // The minimum of a band power that fluctuates like a chi-square
            // of 2 x bins x ~3 (smoothing) degrees of freedom, over 160
            // hops, lies about 2.6 standard deviations below the mean.
            let dof = 6.0 * (b - a) as f32;
            bias.push(1.0 / (1.0 - 2.6 / Float::sqrt(dof)).max(0.25));
        }
        let hann: Vec<f32> = (0..window)
            .map(|i| (0.5 - 0.5 * Float::cos(core::f64::consts::TAU * i as f64 / window as f64)) as f32)
            .collect();
        // Power of a full-scale-normalised sine bin: undo the window and FFT
        // gain, so that band powers are mean squares.
        let wsum: f32 = hann.iter().map(|w| w * w).sum();
        let nb = bands.len();
        let per_hop = |seconds: f32| 1.0 - Float::exp(-0.01 / seconds);
        let c_smooth = per_hop(0.03);
        let mut vad = Vad {
            rate,
            hop,
            window,
            fft: RealFft::new(window),
            hann,
            ring: vec![0.0; window],
            pos: 0,
            seen: 0,
            fill: 0,
            hop_energy: 0.0,
            hop_peak: 0.0,
            input: vec![0.0; window],
            re: vec![0.0; window / 2 + 1],
            im: vec![0.0; window / 2 + 1],
            bands,
            speech_band,
            bias,
            scale: 2.0 / (wsum * window as f32),
            power: vec![0.0; nb],
            smooth: vec![0.0; nb],
            noise: vec![0.0; nb],
            sub_min: vec![0.0; nb],
            mins: vec![0.0; nb * MIN_WINDOWS],
            speech_power: vec![0.0; nb],
            levels: vec![0.0; STEADY_HOPS],
            level_pos: 0,
            // A running mean of n powers fluctuates like the smoothed power
            // when n = (2 - c) / c.
            settle_hops: (2.0 / c_smooth) as u64,
            min_count: 0,
            min_slot: 0,
            hops: 0,
            probability: 0.0,
            snr_db: 0.0,
            click_hold: 0,
            hang: 0,
            history: 0,
            quiet_run: 0,
            speaking: false,
            threshold: 0.0,
            sensitivity: 0.5,
            start_hops: 10,
            start_window: 15,
            end_hops: 60,
            c_smooth,
        };
        vad.set_sensitivity(0.5);
        vad.reset();
        vad
    }

    /// Sample rate in Hz.
    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// Forgets the noise floor and all decisions.
    pub fn reset(&mut self) {
        self.ring.fill(0.0);
        self.pos = 0;
        self.seen = 0;
        self.fill = 0;
        self.hop_energy = 0.0;
        self.hop_peak = 0.0;
        self.power.fill(0.0);
        self.smooth.fill(0.0);
        self.noise.fill(0.0);
        self.sub_min.fill(f32::MAX);
        self.mins.fill(f32::MAX);
        self.speech_power.fill(0.0);
        self.levels.fill(0.0);
        self.level_pos = 0;
        self.min_count = 0;
        self.min_slot = 0;
        self.hops = 0;
        self.probability = 0.0;
        self.snr_db = 0.0;
        self.click_hold = 0;
        self.hang = 0;
        self.history = 0;
        self.quiet_run = 0;
        self.speaking = false;
    }

    /// Sets the sensitivity in 0..=1 (default 0.5): higher detects quieter
    /// speech and accepts more false alarms.
    pub fn set_sensitivity(&mut self, sensitivity: f32) {
        let s = if sensitivity.is_finite() { sensitivity.clamp(0.0, 1.0) } else { 0.5 };
        self.sensitivity = s;
        // Log-likelihood ratio at which the probability is one half.
        self.threshold = 2.4 - 2.4 * s;
    }

    /// The sensitivity (see [`set_sensitivity`](Vad::set_sensitivity)).
    pub fn sensitivity(&self) -> f32 {
        self.sensitivity
    }

    /// Sets how much speech starts an utterance (`start_ms`, default 100)
    /// and how much silence ends it (`end_ms`, default 600) for
    /// [`speaking`](Vad::speaking). Both are rounded up to 10 ms and limited
    /// to 10..=400 ms and 10 ms..=10 s; the start tolerates gaps of half its
    /// length.
    pub fn set_timing(&mut self, start_ms: u32, end_ms: u32) {
        self.start_hops = start_ms.div_ceil(10).clamp(1, 40);
        // (At most 60 hops of the 64-bit history.)
        self.start_window = self.start_hops + self.start_hops / 2;
        self.end_hops = end_ms.div_ceil(10).clamp(1, 1000);
    }

    /// Analyses `frame` (any length) and returns the verdict for the latest
    /// 10 ms. A frame shorter than what completes a hop returns the previous
    /// verdict.
    pub fn process(&mut self, frame: &[i16]) -> VadState {
        let mask = self.window - 1;
        for &s in frame {
            let x = s as f32 * (1.0 / 32768.0);
            self.ring[self.pos] = x;
            self.pos = (self.pos + 1) & mask;
            self.hop_energy += x * x;
            self.hop_peak = self.hop_peak.max(x * x);
            self.fill += 1;
            self.seen = self.seen.saturating_add(1);
            if self.fill == self.hop {
                // (Only once a whole window has arrived: the zeros the ring
                // starts with would make the first noise floor too low.)
                if self.seen >= self.window {
                    self.analyse();
                }
                self.fill = 0;
                self.hop_energy = 0.0;
                self.hop_peak = 0.0;
            }
        }
        self.state()
    }

    /// The verdict for the latest 10 ms.
    pub fn state(&self) -> VadState {
        if self.hang > 0 { VadState::Speech { probability: self.probability.max(0.5) } } else { VadState::Silence }
    }

    /// True during an utterance: from 100 ms of speech to 600 ms of silence
    /// (see [`set_timing`](Vad::set_timing)).
    pub fn speaking(&self) -> bool {
        self.speaking
    }

    /// The speech probability of the latest hop (0..=1).
    pub fn probability(&self) -> f32 {
        self.probability
    }

    /// The signal-to-noise ratio of the latest hop in the speech band, in dB.
    pub fn snr_db(&self) -> f32 {
        self.snr_db
    }

    /// The estimated background noise level in dBFS (mean square over all
    /// bands, relative to a full-scale sine).
    pub fn noise_db(&self) -> f32 {
        let n: f32 = self.noise.iter().sum();
        10.0 * Float::log10(n.max(FLOOR_POWER))
    }

    /// The latest hop's peak power over its mean power, and over the mean
    /// power of its samples more than a millisecond away from the peak.
    fn hop_crests(&self) -> (f32, f32) {
        let mask = self.window - 1;
        let first = (self.pos + self.window - self.hop) & mask;
        let sample = |j: usize| self.ring[(first + j) & mask];
        let (mut at, mut peak) = (0, 0.0f32);
        for j in 0..self.hop {
            let v = sample(j);
            if v * v > peak {
                peak = v * v;
                at = j;
            }
        }
        let reach = self.rate as usize / 1000;
        let (lo, hi) = (at.saturating_sub(reach), (at + reach + 1).min(self.hop));
        let near: f32 = (lo..hi).map(|j| sample(j) * sample(j)).sum();
        let away = (self.hop - (hi - lo)) as f32;
        let crest = peak * self.hop as f32 / self.hop_energy.max(1e-20);
        let crest_away = peak * away / (self.hop_energy - near).max(1e-20);
        (crest, crest_away)
    }

    fn analyse(&mut self) {
        let mask = self.window - 1;
        for (i, (v, &w)) in self.input.iter_mut().zip(&self.hann).enumerate() {
            *v = self.ring[(self.pos + i) & mask] * w;
        }
        self.fft.forward(&self.input, &mut self.re, &mut self.im);
        for (p, &(a, b)) in self.power.iter_mut().zip(&self.bands) {
            let mut e = 0.0;
            for (&r, &i) in self.re[a..b].iter().zip(&self.im[a..b]) {
                e += r * r + i * i;
            }
            *p = (e * self.scale).max(FLOOR_POWER);
        }
        self.hops += 1;

        // Noise floor by minimum statistics of the smoothed band powers. The
        // first hops are averaged (a running mean, until the smoothing takes
        // over), and their mean is the floor until it is as steady as the
        // smoothed powers: only then do they enter the minimum. A single
        // power fluctuates far more than a smoothed one, and an unluckily low
        // one would set the floor of a narrow band (two FFT bins at 100 Hz)
        // far too low for the next 1.6 s, where ordinary noise looks like
        // speech.
        let nb = self.bands.len();
        let c = self.c_smooth.max(1.0 / self.hops as f32);
        for (s, &p) in self.smooth.iter_mut().zip(&self.power) {
            *s += c * (p - *s);
        }
        if self.hops < self.settle_hops {
            for (n, &s) in self.noise.iter_mut().zip(&self.smooth) {
                *n = s.max(FLOOR_POWER);
            }
        } else {
            for (m, &s) in self.sub_min.iter_mut().zip(&self.smooth) {
                *m = m.min(s);
            }
            self.min_count += 1;
            if self.min_count >= MIN_HOPS {
                self.mins[self.min_slot * nb..(self.min_slot + 1) * nb].copy_from_slice(&self.sub_min);
                self.min_slot = (self.min_slot + 1) % MIN_WINDOWS;
                self.sub_min.fill(f32::MAX);
                self.min_count = 0;
            }
            for (i, noise) in self.noise.iter_mut().enumerate() {
                let mut m = self.sub_min[i];
                for w in 0..MIN_WINDOWS {
                    m = m.min(self.mins[w * nb + i]);
                }
                *noise = (m * self.bias[i]).max(FLOOR_POWER);
            }
        }

        // A loud but steady sound becomes the floor: speech is never steady
        // for long. Steady means that the smoothed level of the speech band
        // has stayed within a few dB for 0.4 s (syllables make it swing by
        // 10 dB and more; after a step in level the window holds only the
        // new level 0.4 s later).
        let (mut total, mut floor_total) = (0.0f32, 0.0f32);
        for i in 0..nb {
            if self.speech_band[i] {
                total += self.smooth[i];
                floor_total += self.noise[i];
            }
        }
        self.levels[self.level_pos] = 10.0 * Float::log10(total.max(FLOOR_POWER));
        self.level_pos = (self.level_pos + 1) % self.levels.len();
        let (lo, hi) = self.levels.iter().fold((f32::MAX, f32::MIN), |(lo, hi), &l| (lo.min(l), hi.max(l)));
        // (Closer to the floor, quiet speech, whose syllables the noise
        // smooths out, can be as steady.)
        if self.hops >= self.levels.len() as u64 && hi - lo < STEADY_DB && total > STEADY_ABOVE * floor_total {
            // The new floor: this spectrum at the lowest level of the window.
            let scale = Float::powf(10.0, (lo - 10.0 * Float::log10(total.max(FLOOR_POWER))) / 10.0);
            for i in 0..nb {
                let floor = self.smooth[i] * scale;
                self.noise[i] = floor;
                self.sub_min[i] = floor / self.bias[i];
                for w in 0..MIN_WINDOWS {
                    self.mins[w * nb + i] = floor / self.bias[i];
                }
            }
        }

        // Speech likelihood: the log-likelihood ratio of speech against noise
        // per band (Gaussian models, decision-directed a-priori SNR).
        let mut llrs = [0.0f32; EDGES.len()];
        let (mut sig, mut nse) = (0.0f32, 0.0f32);
        for (i, band_llr) in llrs[..nb].iter_mut().enumerate() {
            let gamma = (self.power[i] / self.noise[i]).min(1e4);
            let prior = self.speech_power[i] / self.noise[i];
            let xi = (DD_ALPHA * prior + (1.0 - DD_ALPHA) * (gamma - 1.0).max(0.0)).clamp(1e-3, 1e3);
            let gain = xi / (1.0 + xi);
            self.speech_power[i] = gain * gain * self.power[i];
            *band_llr = gamma * gain - Float::ln(1.0 + xi);
            if self.speech_band[i] {
                sig += self.power[i];
                nse += self.noise[i];
            }
        }
        // The mean over the strongest third of the bands: speech fills only
        // some bands at a time (formants, fricatives), and noise that rules
        // the others (a fan's rumble) must not drown it.
        let llrs = &mut llrs[..nb];
        llrs.sort_unstable_by(|a, b| b.total_cmp(a));
        let top = nb.div_ceil(3).max(1);
        let llr = llrs[..top].iter().sum::<f32>() / top as f32;
        self.snr_db = 10.0 * Float::log10((sig / nse.max(FLOOR_POWER)).max(1e-6));
        let mut p = 1.0 / (1.0 + Float::exp(-SLOPE * (llr - self.threshold)));

        // Clicks: an impulse is not speech (here and while the window still
        // holds it).
        let (crest, away) = self.hop_crests();
        if (crest > CLICK_CREST || away > CLICK_CREST_AWAY) && self.hop_peak > 1e-6 {
            self.click_hold = (self.window / self.hop) as u32 + 1;
        }
        if self.click_hold > 0 {
            self.click_hold -= 1;
            p = p.min(0.25);
        }
        if !p.is_finite() {
            p = 0.0;
        }
        self.probability = p;

        // Decisions: speech starts above a probability of 0.5 and continues
        // above `CONTINUE` (the fading ends of syllables) and for the hangover
        // after that; hysteresis per utterance.
        let speech = p > 0.5 || (self.hang > 0 && p > CONTINUE);
        self.hang = if speech { HANGOVER } else { self.hang.saturating_sub(1) };
        self.history = (self.history << 1) | u64::from(speech);
        self.quiet_run = if speech { 0 } else { self.quiet_run.saturating_add(1) };
        let recent = self.history & ((1u64 << self.start_window) - 1);
        if !self.speaking && recent.count_ones() >= self.start_hops {
            self.speaking = true;
        } else if self.speaking && self.quiet_run >= self.end_hops {
            self.speaking = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testsig::*;
    use alloc::format;
    use alloc::string::String;
    use std::println;

    /// Runs the detector in 10 ms frames; returns the verdict and the
    /// utterance state per frame.
    fn run(vad: &mut Vad, x: &[f32]) -> (Vec<bool>, Vec<bool>) {
        let pcm = to_i16(x);
        let (mut frames, mut speaking) = (Vec::new(), Vec::new());
        for f in pcm.chunks(vad.rate() as usize / 100) {
            frames.push(vad.process(f).is_speech());
            speaking.push(vad.speaking());
        }
        (frames, speaking)
    }

    /// Per 10 ms frame: is the frame mostly inside an utterance?
    fn truth(active: &[bool], rate: u32) -> Vec<bool> {
        active.chunks(rate as usize / 100).map(|c| 2 * c.iter().filter(|&&a| a).count() > c.len()).collect()
    }

    fn mix(a: &[f32], b: &[f32]) -> Vec<f32> {
        a.iter().zip(b).map(|(x, y)| x + y).collect()
    }

    /// Isolated clicks every 0.7 s: a full-scale sample, or a short ringing
    /// burst.
    fn add_clicks(x: &mut [f32]) {
        let mut i = 3000;
        while i + 64 < x.len() {
            if (i / 11_200) % 2 == 0 {
                x[i] = 0.9;
            } else {
                for j in 0..48 {
                    x[i + j] += 0.6 * (-(j as f32) / 8.0).exp() * if j % 2 == 0 { 1.0 } else { -1.0 };
                }
            }
            i += 11_200;
        }
    }

    #[derive(Debug, Clone, Copy)]
    enum Noise {
        White,
        Pink,
        Fan,
    }

    #[test]
    fn detects_speech_in_noise_at_10_db_snr() {
        let rate = 16_000;
        let seconds = 30.0;
        let n = (seconds * rate as f32) as usize;
        let cases: Vec<(Noise, u64)> =
            [Noise::White, Noise::Pink, Noise::Fan].into_iter().flat_map(|k| seeds(0).map(move |s| (k, s))).collect();
        for_cases("VAD, speech in noise at 10 dB SNR (noise, seed)", &cases, |&(kind, seed)| {
            // Speech at -20 dBFS (over the utterances), noise 10 dB below.
            let level = 0.1 / 10f32.sqrt();
            let noise = match kind {
                Noise::White => white(n, level, seed + 1),
                Noise::Pink => pink(n, level, seed + 2),
                Noise::Fan => fan(rate, n, level, seed + 3),
            };
            let (mut right, mut total, mut missed, mut false_alarms) = (0usize, 0usize, 0usize, 0usize);
            for (voice, s) in [(MALE, seed + 10), (FEMALE, seed + 11)] {
                let talk = speech(rate, seconds, voice, CONVERSATION, s);
                let mut vad = Vad::new(rate);
                let (frames, _) = run(&mut vad, &mix(&talk.samples, &noise));
                let want = truth(&talk.active, rate);
                for (&got, &want) in frames.iter().zip(&want) {
                    right += usize::from(got == want);
                    missed += usize::from(want && !got);
                    false_alarms += usize::from(got && !want);
                    total += 1;
                }
            }
            let accuracy = right as f32 / total as f32;
            let line = format!(
                "{:.1} % of frames right ({:.1} % missed speech, {:.1} % false alarms)",
                100.0 * accuracy,
                100.0 * missed as f32 / total as f32,
                100.0 * false_alarms as f32 / total as f32
            );
            let mut t = Targets::new();
            t.check(accuracy >= 0.95, || format!("accuracy {:.1} %", 100.0 * accuracy));
            (line, t.0)
        });
    }

    #[test]
    fn steady_noise_and_clicks_are_not_speech() {
        let rate = 16_000;
        let n = 10 * rate as usize;
        let cases: Vec<(Noise, u64)> =
            [Noise::White, Noise::Pink, Noise::Fan].into_iter().flat_map(|k| seeds(0).map(move |s| (k, s))).collect();
        for_cases("VAD, steady noise with clicks (noise, seed)", &cases, |&(kind, seed)| {
            let mut noise = match kind {
                Noise::White => white(n, 0.03, seed + 4),
                Noise::Pink => pink(n, 0.03, seed + 5),
                Noise::Fan => fan(rate, n, 0.05, seed + 6),
            };
            add_clicks(&mut noise);
            let mut vad = Vad::new(rate);
            let (frames, speaking) = run(&mut vad, &noise);
            let alarms = frames.iter().filter(|&&f| f).count();
            let utterance = speaking.iter().filter(|&&s| s).count();
            let line = format!("{alarms} of {} frames called speech, speaking {utterance} frames", frames.len());
            let mut t = Targets::new();
            t.check(utterance == 0, || "false utterance".into());
            t.check(alarms * 50 < frames.len(), || format!("{alarms} speech frames"));
            (line, t.0)
        });
    }

    #[test]
    fn a_fan_switching_on_becomes_the_floor() {
        let rate = 16_000;
        let n = 12 * rate as usize;
        let cases: Vec<u64> = seeds(7).collect();
        for_cases("VAD, fan switching on at 3 s (seed)", &cases, |&seed| {
            let mut x = white(n, 0.0005, seed);
            let fan = fan(rate, n, 0.05, seed + 1);
            let on = 3 * rate as usize;
            for (v, f) in x[on..].iter_mut().zip(&fan[on..]) {
                *v += f;
            }
            // And someone talks at 8 s.
            let pacing = Pacing { lead_in: 0.2, syllables: (6, 6), pause: (5.0, 5.0) };
            let talk = speech(rate, 4.0, MALE, pacing, seed + 2);
            for (v, s) in x[8 * rate as usize..].iter_mut().zip(&talk.samples) {
                *v += s * 2.0;
            }
            let mut vad = Vad::new(rate);
            let (_, speaking) = run(&mut vad, &x);
            let first = speaking.iter().position(|&s| s);
            let last_false = speaking[..800].iter().rposition(|&s| s);
            let line = format!("utterance frames before 8 s end at {last_false:?}, first at {first:?}");
            let mut t = Targets::new();
            // The fan may look like speech for a moment, but not for long.
            t.check(last_false.is_none_or(|f| f < 300 + 200), || {
                format!("fan heard as speech until frame {last_false:?}")
            });
            t.check(speaking[820..1000].iter().any(|&s| s), || "speech after the fan not heard".into());
            (line, t.0)
        });
    }

    /// A steady vowel: harmonics of 150 Hz with a falling spectrum.
    fn vowel(rate: u32, seconds: f32) -> Vec<f32> {
        let n = (seconds * rate as f32) as usize;
        (0..n)
            .map(|i| {
                let t = i as f32 / rate as f32;
                let pitch = 150.0 * (1.0 + 0.03 * (core::f32::consts::TAU * 5.0 * t).sin());
                (1..20).map(|h| (core::f32::consts::TAU * pitch * h as f32 * t).sin() * 0.06 / h as f32).sum::<f32>()
            })
            .collect()
    }

    #[test]
    fn hysteresis_follows_the_timing() {
        let rate = 16_000;
        let cases: Vec<u64> = seeds(12).collect();
        for_cases("VAD timing (seed)", &cases, |&seed| {
            let mut x = white(4 * rate as usize, 0.001, seed);
            let place = |x: &mut [f32], at: f32, sound: &[f32]| {
                let a = (at * rate as f32) as usize;
                for (v, s) in x[a..].iter_mut().zip(sound) {
                    *v += s;
                }
            };
            // A 60 ms blip (too short), then one second of voice with a 300 ms
            // pause in the middle.
            place(&mut x, 0.5, &vowel(rate, 0.06));
            place(&mut x, 1.0, &vowel(rate, 0.35));
            place(&mut x, 1.65, &vowel(rate, 0.35));
            let mut vad = Vad::new(rate);
            let (frames, speaking) = run(&mut vad, &x);
            let on = speaking.iter().position(|&s| s);
            let off = on.and_then(|on| speaking[on..].iter().position(|&s| !s).map(|o| on + o));
            let last_voice = frames.iter().rposition(|&f| f);
            // Shorter timing ends sooner.
            let mut quick = Vad::new(rate);
            quick.set_timing(50, 200);
            let (_, quick_speaking) = run(&mut quick, &x);
            let quick_on = quick_speaking.iter().position(|&s| s);
            let line = format!(
                "utterance from frame {on:?} to {off:?} (voice 100..200, verdicts end at {last_voice:?}); with \
                 quicker timing from {quick_on:?}"
            );
            let mut t = Targets::new();
            t.check(on.is_some_and(|f| (108..=115).contains(&f)), || format!("start at frame {on:?}"));
            t.check(off.is_some_and(|f| (260..=275).contains(&f)), || format!("end at frame {off:?}"));
            t.check(speaking[..100].iter().all(|&s| !s), || "the blip started an utterance".into());
            t.check(quick_on.is_some_and(|f| (50..=60).contains(&f)), || {
                format!("the blip did not start a quick utterance: {quick_on:?}")
            });
            (line, t.0)
        });
    }

    #[test]
    fn sensitivity_and_odd_input() {
        let rate = 16_000;
        let n = 10 * rate as usize;
        let cases: Vec<u64> = seeds(13).collect();
        for_cases("VAD sensitivity and other rates (seed)", &cases, |&seed| {
            let talk = speech(rate, 10.0, FEMALE, CONVERSATION, seed);
            // Quiet speech: 3 dB SNR.
            let x = mix(&talk.samples, &white(n, 0.07, seed + 1));
            let count = |s: f32| {
                let mut vad = Vad::new(rate);
                vad.set_sensitivity(s);
                run(&mut vad, &x).0.iter().filter(|&&f| f).count()
            };
            let (low, mid, high) = (count(0.1), count(0.5), count(0.9));
            let mut t = Targets::new();
            t.check(low < mid && mid < high, || format!("speech frames {low} / {mid} / {high}"));
            let mut rates = String::new();
            for r in [8_000u32, 48_000] {
                let talk = speech(r, 4.0, MALE, CONVERSATION, seed + 2);
                let mut vad = Vad::new(r);
                let (frames, _) = run(&mut vad, &mix(&talk.samples, &white(talk.samples.len(), 0.003, seed + 3)));
                let want = truth(&talk.active, r);
                let right = frames.iter().zip(&want).filter(|(a, b)| a == b).count();
                t.check(right as f32 > 0.9 * want.len() as f32, || format!("{r} Hz: {right} of {}", want.len()));
                rates += &format!(", {r} Hz {:.1} % right", 100.0 * right as f32 / want.len() as f32);
            }
            (format!("at 3 dB SNR: speech frames {low} / {mid} / {high} at sensitivity 0.1 / 0.5 / 0.9{rates}"), t.0)
        });
        // Silence, DC, odd and empty frames.
        let mut vad = Vad::new(16_000);
        for len in [0usize, 1, 7, 160, 333, 4096] {
            assert_eq!(vad.process(&vec![0i16; len]), VadState::Silence);
        }
        let dc = vec![12_000i16; 32_000];
        for f in dc.chunks(320) {
            vad.process(f);
        }
        assert!(!vad.speaking() && !vad.state().is_speech());
        assert!(vad.probability().is_finite() && vad.noise_db().is_finite() && vad.snr_db().is_finite());
    }

    /// Processing time per second of audio on the host, with the level
    /// meter's for comparison. Run with
    /// `cargo test --release -p vaudio vad::tests::bench -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn bench_cpu_time() {
        for rate in [16_000u32, 48_000] {
            let talk = speech(rate, 10.0, MALE, CONVERSATION, 17);
            let pcm = to_i16(&mix(&talk.samples, &white(talk.samples.len(), 0.01, 18)));
            let frame = rate as usize / 100;
            let mut vad = Vad::new(rate);
            let start = std::time::Instant::now();
            for f in pcm.chunks(frame) {
                core::hint::black_box(vad.process(f));
            }
            let t_vad = start.elapsed().as_secs_f64() / 10.0;
            let mut meter = crate::level::LevelMeter::new(rate);
            let start = std::time::Instant::now();
            for f in pcm.chunks(frame) {
                core::hint::black_box(meter.process(f));
            }
            let t_meter = start.elapsed().as_secs_f64() / 10.0;
            println!(
                "{rate} Hz: VAD {:.3} ms, level meter {:.4} ms per second of audio",
                t_vad * 1000.0,
                t_meter * 1000.0
            );
        }
    }
}
