// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Something like a voice, for the tests that need sound a voice detector
//! takes for speech but nothing need understand: a buzz rich in harmonics
//! (like vocal cords) at a wandering pitch, in syllables four times a
//! second. The test microphone's user (`mic-tone`) and the stand-in for
//! Deepgram's agent (`agent-speak`) speak it, each in a voice of its own.

use std::f64::consts::TAU;

/// A voice: its pitch and how it wanders, and its brightness.
#[derive(Clone, Copy)]
pub struct Voice {
    /// The pitch it wanders about (Hz), how far (Hz) and how fast (Hz).
    pub pitch: f64,
    pub wander: f64,
    pub wander_rate: f64,
    /// The highest harmonic (Hz).
    pub ceiling: f64,
    /// Where its syllables start (radians of their cycle).
    pub syllable_phase: f64,
}

/// The test microphone's user: a low voice.
pub const USER: Voice = Voice { pitch: 130.0, wander: 25.0, wander_rate: 1.3, ceiling: 4000.0, syllable_phase: 0.0 };

/// The simulated agent: a higher, brighter one.
pub const AGENT: Voice = Voice { pitch: 190.0, wander: 30.0, wander_rate: 1.1, ceiling: 5000.0, syllable_phase: 1.0 };

/// `secs` of `voice` at `rate` Hz, mono, its loudest syllables near `peak`.
pub fn buzz(voice: Voice, rate: u32, secs: f64, peak: f64) -> Vec<i16> {
    let rate = rate as f64;
    let n = (secs * rate) as usize;
    let mut phase = 0.0f64;
    (0..n)
        .map(|i| {
            let t = i as f64 / rate;
            let pitch = voice.pitch + voice.wander * (t * voice.wander_rate * TAU).sin();
            phase += TAU * pitch / rate;
            let mut sound = 0.0;
            let mut k = 1.0;
            while k * pitch < voice.ceiling {
                sound += (phase * k).sin() / k;
                k += 1.0;
            }
            let syllable = (t * 4.0 * TAU + voice.syllable_phase).sin().max(0.0).sqrt();
            (sound * (0.1 + 0.9 * syllable) * peak).clamp(i16::MIN as f64, i16::MAX as f64) as i16
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_voice_comes_in_syllables() {
        let rate = 16_000;
        let sound = buzz(USER, rate, 1.0, 6000.0);
        assert_eq!(sound.len(), 16_000);
        // Four syllables a second: loud quarters, quiet ones between.
        let level = |from: f64, to: f64| {
            let part = &sound[(from * rate as f64) as usize..(to * rate as f64) as usize];
            (part.iter().map(|&s| (s as f64).powi(2)).sum::<f64>() / part.len() as f64).sqrt()
        };
        assert!(level(0.05, 0.1) > 4.0 * level(0.15, 0.2), "{} {}", level(0.05, 0.1), level(0.15, 0.2));
        assert!(sound.iter().any(|&s| s.unsigned_abs() > 3000));
        // The agent's voice is another.
        assert_ne!(buzz(AGENT, rate, 0.1, 6000.0), sound[..1600]);
    }
}
