// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Sirens: a tone swept up and down, rich in odd harmonics as from an
//! electronic siren's square wave, through the narrow band of its horn
//! speaker.
//!
//! The **wail** rises over two seconds and falls over two and a half; the
//! **yelp** does the same eight times faster, for clearing a way through
//! traffic. Sirens started together drift apart, as real ones do, and each
//! is heard with the Doppler shift of its car going by.

use audio::synth::filter::{Mode, Svf};
use num_traits::Float;

use super::{Listener, Panning, Placing, RATE, SIRENS, SineTable, SirenState, Sounds, Voiced};

/// What a siren plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wail {
    Wail,
    Yelp,
}

impl Wail {
    /// The pattern's period (s).
    fn period(self) -> f32 {
        match self {
            Wail::Wail => 4.6,
            Wail::Yelp => 0.36,
        }
    }

    /// The pitch (Hz) a fraction `p` of the way through the pattern.
    fn pitch(self, p: f32) -> f32 {
        let sweep = |lo: f32, hi: f32| {
            // Up quickly at first and slowing to the top, then down.
            let rise = 0.45;
            let x = if p < rise {
                let u = p / rise;
                1.0 - (1.0 - u) * (1.0 - u)
            } else {
                let u = (p - rise) / (1.0 - rise);
                1.0 - u * u * (3.0 - 2.0 * u)
            };
            lo + (hi - lo) * x
        };
        match self {
            Wail::Wail => sweep(640.0, 1480.0),
            Wail::Yelp => sweep(700.0, 1550.0),
        }
    }
}

/// The square wave's odd harmonics, a little of the even ones, as the
/// horn's driver adds.
const HARMONICS: [f32; 8] = [1.0, 0.06, 0.33, 0.04, 0.2, 0.03, 0.14, 0.02];

/// A voice playing one siren.
pub struct Voice {
    key: Option<u64>,
    fresh: bool,
    wail: Wail,
    /// How far through its pattern (0..1).
    at: f32,
    /// The tone's phase.
    phase: u32,
    pitch: f32,
    place: Placing,
    fade: f32,
    horn: Svf,
    air: Svf,
}

impl Voice {
    pub fn new() -> Voice {
        let mut horn = Svf::new(Mode::BandPass);
        horn.set(1500.0, 0.55, RATE as f32);
        Voice {
            key: None,
            fresh: true,
            wail: Wail::Wail,
            at: 0.0,
            phase: 0,
            pitch: 640.0,
            place: Placing::default(),
            fade: 0.0,
            horn,
            air: Svf::new(Mode::LowPass),
        }
    }

    /// Mixes this block of the siren it plays into `l` and `r`.
    pub fn render(
        &mut self,
        sines: &SineTable,
        ears: &Listener,
        sirens: &Sounds<SirenState, SIRENS>,
        l: &mut [f32],
        r: &mut [f32],
    ) {
        let Some(key) = self.key else { return };
        let rate = RATE as f32;
        let (to_fade, place1) = match sirens.iter().find(|s| s.key == key) {
            Some(s) => {
                if s.wail != self.wail {
                    // A new pattern starts from its bottom, like a real one.
                    self.wail = s.wail;
                    self.at = 0.0;
                }
                let mut place = ears.place(&s.emitter, 30.0);
                if s.own {
                    place.gain *= 0.45;
                }
                (1.0, place)
            }
            None => (0.0, self.place),
        };
        if self.fresh {
            self.place = place1;
            // Sirens switched on together soon drift apart (by their cars'
            // numbers, the keys' low halves).
            self.at = Float::fract((key as u32) as f32 * 0.618_034);
            self.fresh = false;
        }
        self.air.set(place1.cutoff.min(10_000.0), 0.7, rate);
        let place0 = self.place;
        let pan = Panning::new(place0.pan, place1.pan);
        let step = 1.0 / (self.wail.period() * rate);
        let n = l.len();
        for i in 0..n {
            let t = (i + 1) as f32 / n as f32;
            self.fade += (to_fade - self.fade) * 0.0006;
            self.at = Float::fract(self.at + step);
            // Glide to the pattern's pitch (a new pattern starts without a
            // click).
            self.pitch += (self.wail.pitch(self.at) - self.pitch) * 0.02;
            let doppler = place0.doppler + (place1.doppler - place0.doppler) * t;
            self.phase = self.phase.wrapping_add(super::increment(self.pitch * doppler));
            let mut s = 0.0;
            let mut ph = 0u32;
            let top = ((rate * 0.45) / (self.pitch * doppler)) as usize;
            for &h in HARMONICS.iter().take(top) {
                ph = ph.wrapping_add(self.phase);
                s += h * sines.at(ph);
            }
            // The horn's band, and the speaker driven a little hard.
            let v = self.horn.process(s) * 2.2;
            let v = v / (1.0 + v.abs() * 0.6);
            let gain = place0.gain + (place1.gain - place0.gain) * t;
            let v = self.air.process(v) * gain * self.fade * 0.32;
            let (gl, gr) = pan.at(t);
            l[i] += v * gl;
            r[i] += v * gr;
        }
        self.place = place1;
        if to_fade == 0.0 && self.fade < 1e-4 {
            self.key = None;
        }
    }
}

impl Voiced for Voice {
    fn key(&self) -> Option<u64> {
        self.key
    }

    fn start(&mut self, key: u64) {
        self.key = Some(key);
        self.fresh = true;
        self.fade = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wail_sweeps_its_range() {
        let (lo, hi) = (0..1000)
            .map(|i| Wail::Wail.pitch(i as f32 / 1000.0))
            .fold((f32::MAX, 0.0f32), |(a, b), f| (a.min(f), b.max(f)));
        assert!((630.0..660.0).contains(&lo) && (1450.0..1500.0).contains(&hi), "{lo}..{hi}");
        // Continuous across the pattern's end.
        assert!((Wail::Wail.pitch(0.0) - Wail::Wail.pitch(0.9999)).abs() < 2.0);
        assert!((Wail::Yelp.pitch(0.0) - Wail::Yelp.pitch(0.9999)).abs() < 2.0);
    }
}
