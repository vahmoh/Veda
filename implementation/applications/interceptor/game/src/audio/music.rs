// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The music: tunes of a few stems recorded in step (the same tempo and
//! length), looping. A tune's first stem always plays; the others join as
//! the game's intensity rises (in a pursuit: the lead and the heavier
//! drums when the suspect is close), so the music follows the action
//! without ever losing its beat. Changing tunes crossfades.

use alloc::vec::Vec;

use vaudio::AudioError;
use vaudio::source::Source;

/// A tune: its stems, playing in step.
struct Tune<D: AsRef<[u8]>> {
    stems: Vec<Source<D>>,
    /// How much of the tune is heard (eased from 0 to 1 and back), and of
    /// each stem.
    level: f32,
    layers: Vec<f32>,
}

/// The music player.
pub struct Music<D: AsRef<[u8]>> {
    tunes: Vec<Tune<D>>,
    rate: u32,
    current: Option<usize>,
    volume: f32,
    intensity: f32,
    /// The volume, eased.
    gain: f32,
    buf: Vec<i16>,
    mix: Vec<f32>,
}

/// How quickly tunes fade in and out (per sample at 44.1 kHz: about a
/// second and a half), and layers come and go (about two seconds).
const FADE: f32 = 1.0 / 66_000.0;
const LAYER: f32 = 1.0 / 88_000.0;

impl<D: AsRef<[u8]>> Music<D> {
    pub fn new() -> Music<D> {
        Music {
            tunes: Vec::new(),
            rate: 0,
            current: None,
            volume: 1.0,
            intensity: 0.0,
            gain: 1.0,
            buf: Vec::new(),
            mix: Vec::new(),
        }
    }

    /// Adds a tune of `stems` (files at the same rate and of the same
    /// length); returns its number.
    pub fn add(&mut self, stems: Vec<D>) -> Result<usize, AudioError> {
        let mut sources = Vec::with_capacity(stems.len());
        for d in stems {
            let s = Source::open(d)?;
            if self.rate == 0 {
                self.rate = s.rate();
            }
            if s.rate() != self.rate || sources.first().is_some_and(|f: &Source<D>| f.frames() != s.frames()) {
                return Err(AudioError::Unsupported);
            }
            sources.push(s);
        }
        if sources.is_empty() {
            return Err(AudioError::Unsupported);
        }
        let n = sources.len();
        self.tunes.push(Tune { stems: sources, level: 0.0, layers: alloc::vec![0.0; n] });
        Ok(self.tunes.len() - 1)
    }

    /// The tunes' sample rate (0 before any is added).
    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// Plays `tune` from its start (crossfading from the one playing), or
    /// fades to silence; carries on if it is playing already.
    pub fn play(&mut self, tune: Option<usize>) {
        let tune = tune.filter(|&t| t < self.tunes.len());
        if tune == self.current {
            return;
        }
        if let Some(t) = tune {
            let new = &mut self.tunes[t];
            if new.level == 0.0 {
                for s in &mut new.stems {
                    s.seek(0);
                }
            }
        }
        self.current = tune;
    }

    /// How intense the action is (0..1): the stems after the first join
    /// in turn as it rises.
    pub fn set_intensity(&mut self, x: f32) {
        self.intensity = super::level(x);
    }

    /// The volume (0..1; anything not a number, silence).
    pub fn set_volume(&mut self, v: f32) {
        self.volume = super::level(v);
    }

    /// Renders `out.len() / 2` stereo frames at [`Music::rate`].
    pub fn render(&mut self, out: &mut [i16]) {
        let frames = out.len() / 2;
        self.mix.clear();
        self.mix.resize(frames * 2, 0.0);
        self.buf.resize(frames * 2, 0);
        for (i, tune) in self.tunes.iter_mut().enumerate() {
            let on = f32::from(u8::from(Some(i) == self.current));
            if on == 0.0 && tune.level == 0.0 {
                continue;
            }
            // The tune fades across the block; its stems join and leave
            // with the intensity.
            let level0 = tune.level;
            let level1 = level0 + (on - level0).clamp(-FADE * frames as f32, FADE * frames as f32);
            tune.level = level1;
            let extra = tune.stems.len().saturating_sub(1).max(1) as f32;
            for (k, stem) in tune.stems.iter_mut().enumerate() {
                // Read the block, looping at the end.
                let mut done = 0;
                while done < frames {
                    let n = stem.read(&mut self.buf[done * 2..frames * 2]);
                    if n == 0 {
                        if stem.position() == 0 {
                            break;
                        }
                        stem.seek(0);
                        continue;
                    }
                    done += n;
                }
                self.buf[done * 2..].fill(0);
                let want = if k == 0 { 1.0 } else { (self.intensity * extra - (k - 1) as f32).clamp(0.0, 1.0) };
                let mut layer = tune.layers[k];
                for (j, (m, pair)) in
                    self.mix.as_chunks_mut::<2>().0.iter_mut().zip(self.buf.as_chunks::<2>().0).enumerate()
                {
                    layer += (want - layer).clamp(-LAYER, LAYER);
                    let level = level0 + (level1 - level0) * (j + 1) as f32 / frames as f32;
                    let g = layer * level * (1.0 / 32768.0);
                    m[0] += f32::from(pair[0]) * g;
                    m[1] += f32::from(pair[1]) * g;
                }
                tune.layers[k] = layer;
            }
        }
        let mut gain = self.gain;
        for (o, m) in out.as_chunks_mut::<2>().0.iter_mut().zip(self.mix.as_chunks::<2>().0) {
            gain += (self.volume - gain) * 0.001;
            o[0] = ((m[0] * gain).clamp(-1.0, 1.0) * 32767.0) as i16;
            o[1] = ((m[1] * gain).clamp(-1.0, 1.0) * 32767.0) as i16;
        }
        self.gain = gain;
    }
}

impl<D: AsRef<[u8]>> Default for Music<D> {
    fn default() -> Music<D> {
        Music::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// A stereo WAV file of `periods` periods of a sine `period` frames
    /// long, at 44.1 kHz.
    fn tone(period: usize, periods: usize) -> Vec<u8> {
        let pcm: Vec<i16> = (0..period * periods)
            .flat_map(|i| {
                let v = (libm::sinf(i as f32 / period as f32 * core::f32::consts::TAU) * 12_000.0) as i16;
                [v, v]
            })
            .collect();
        vaudio::wav::write(&pcm, 2, 44_100, &vaudio::Tags::default())
    }

    /// The largest step between neighbouring samples of the left channel.
    fn largest_step(pcm: &[i16]) -> i32 {
        let frames = pcm.as_chunks::<2>().0;
        frames.iter().zip(&frames[1..]).map(|(a, b)| (b[0] as i32 - a[0] as i32).abs()).max().unwrap_or(0)
    }

    #[test]
    fn tunes_loop_seamlessly_and_layer() {
        let mut m = Music::new();
        // Two stems of 1000 frames: 10 periods of one tone, 20 of another.
        let tune = m.add(vec![tone(100, 10), tone(50, 20)]).unwrap();
        assert_eq!(m.rate(), 44_100);
        m.play(Some(tune));
        let mut out = vec![0i16; 2 * 4410];
        // Fading in, then the first stem alone, looping round every 1000
        // frames without a step larger than the tone's own.
        for _ in 0..20 {
            m.render(&mut out);
        }
        let alone = largest_step(&out);
        let peak = out.iter().map(|v| v.unsigned_abs()).max().unwrap();
        assert!(peak > 9000 && alone < 900, "peak {peak}, step {alone}");
        // The second stem joins with the intensity.
        m.set_intensity(1.0);
        for _ in 0..30 {
            m.render(&mut out);
        }
        let both = out.iter().map(|v| v.unsigned_abs()).max().unwrap();
        assert!(both > peak + 4000, "{both} with both stems, {peak} with one");
        // Silence fades out to nothing.
        m.play(None);
        for _ in 0..30 {
            m.render(&mut out);
        }
        assert!(out.iter().all(|&v| v == 0));
    }
}
