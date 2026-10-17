// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The sounds, synthesised as the game plays: engines that follow their
//! revs and load, sirens wailing and yelping with the Doppler shift of a
//! passing police car, tyres squealing, crashes, wind, horns, the echo of
//! a tunnel, and the cues of the game itself.
//!
//! The game describes what is sounding each frame ([`Scene`]): where the
//! ears are, and each engine, siren and tyre near them, in place (a scene
//! goes to the mixer's thread and is dropped there without touching the
//! heap); it sends one-off [`Cue`]s as things happen. The [`Mixer`]
//! renders blocks of stereo samples from that, on its own thread, easing
//! every parameter across a block so that nothing clicks. Sounds are
//! placed by distance (falling off, and duller far away), direction
//! (panned) and motion (Doppler).
//!
//! Synthesis rather than recordings: an engine's sound is the sum of its
//! "orders", the harmonics of its crankshaft's rotation, weighted as its
//! layout fires (a V8's burble, a four's buzz, a V12's scream), brighter
//! under load, with the noise of its intake and exhaust.

pub mod engine;
pub mod music;
pub mod sfx;
pub mod siren;

use alloc::vec::Vec;

use audio::synth::fx::{Limiter, Reverb};
use audio::synth::osc::{Osc, Wave};
use audio::synth::voice::pan_gains;

use crate::prelude::*;

pub use engine::Layout;
pub use siren::Wail;

/// The rate the mixer renders at (Hz).
pub const RATE: u32 = 48_000;
/// The speed of sound (m/s).
const SOUND: f32 = 343.0;

/// Where the ears are, and how they move.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Listener {
    pub pos: Vec3,
    pub vel: Vec3,
    /// Unit vector to the right of the view.
    pub right: Vec3,
}

/// Where a sound comes from, and how it moves.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Emitter {
    pub pos: Vec3,
    pub vel: Vec3,
}

/// An engine running.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EngineState {
    /// Which car (so that a voice follows the same engine).
    pub key: u64,
    pub emitter: Emitter,
    pub layout: Layout,
    pub rpm: f32,
    pub redline: f32,
    /// The throttle, 0..1.
    pub load: f32,
    /// At the rev limiter.
    pub limiter: bool,
    /// The nitrous burning.
    pub boost: bool,
    /// The player's own (louder, nearer the ears).
    pub own: bool,
}

/// A siren sounding.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SirenState {
    pub key: u64,
    pub emitter: Emitter,
    pub wail: Wail,
    /// The player's own (quieter: heard all the time).
    pub own: bool,
}

/// A car's tyres sliding.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TireState {
    pub key: u64,
    pub emitter: Emitter,
    /// How fast they slide (m/s).
    pub slip: f32,
    /// How fast the car goes (m/s).
    pub speed: f32,
}

/// Up to `N` sounds of a kind, kept in place rather than on the heap.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sounds<T: Copy, const N: usize> {
    items: [Option<T>; N],
    len: usize,
}

impl<T: Copy, const N: usize> Sounds<T, N> {
    /// None.
    pub const fn new() -> Self {
        Sounds { items: [None; N], len: 0 }
    }

    /// Adds `s`, if there is room for it: true if there was.
    pub fn push(&mut self, s: T) -> bool {
        if self.len == N {
            return false;
        }
        self.items[self.len] = Some(s);
        self.len += 1;
        true
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.items[..self.len].iter().flatten()
    }
}

impl<T: Copy, const N: usize> Default for Sounds<T, N> {
    fn default() -> Self {
        Sounds::new()
    }
}

/// The first `N` of the sounds (as many as are heard at once).
impl<T: Copy, const N: usize> FromIterator<T> for Sounds<T, N> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let mut s = Sounds::new();
        for x in iter.into_iter().take(N) {
            s.push(x);
        }
        s
    }
}

/// Everything sounding now.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Scene {
    pub listener: Listener,
    pub engines: Sounds<EngineState, ENGINES>,
    pub sirens: Sounds<SirenState, SIRENS>,
    pub tires: Sounds<TireState, TIRES>,
    /// The player's car scraping along something (0..1).
    pub scrape: f32,
    /// The wind rushing past the ears (m/s).
    pub wind: f32,
    /// How much a tunnel's walls echo (0 in the open, 1 inside).
    pub tunnel: f32,
    /// The rain (0..1).
    pub rain: f32,
    /// The effects' volume (0..1); the world falls silent while paused.
    pub volume: f32,
    pub paused: bool,
}

/// Something that happens once.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cue {
    /// A crash, as hard as `strength` (0..1).
    Crash {
        emitter: Emitter,
        strength: f32,
    },
    /// A car's horn, for `secs`.
    Horn {
        emitter: Emitter,
        secs: f32,
    },
    /// The menus: moving between items, choosing one.
    Move,
    Select,
    Back,
    /// A countdown's beat (`last`: the go).
    Count {
        last: bool,
    },
    /// The radio: a squelch and a chirp before a message.
    Radio,
    /// A pursuit won, or lost.
    Win,
    Lose,
    /// The nitrous lit.
    Boost,
}

/// How a sound at an emitter reaches the ears.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Placing {
    /// Loudness (0..1).
    pub gain: f32,
    /// -1 left .. 1 right.
    pub pan: f32,
    /// Pitch factor from the motion.
    pub doppler: f32,
    /// How bright it still is (a low-pass cutoff, Hz).
    pub cutoff: f32,
}

impl Listener {
    /// How a sound at `e` reaches these ears, `near` metres being full
    /// loudness.
    pub fn place(&self, e: &Emitter, near: f32) -> Placing {
        let d = e.pos - self.pos;
        let dist = d.length();
        if dist < 1e-3 {
            return Placing { gain: 1.0, pan: 0.0, doppler: 1.0, cutoff: 18_000.0 };
        }
        let u = d / dist;
        let gain = (near / (near + dist)).min(1.0);
        let pan = (u.dot(self.right) * (dist / (dist + 2.0))).clamp(-1.0, 1.0);
        // Ears moving towards the source, and the source moving away.
        let towards = self.vel.dot(u);
        let away = e.vel.dot(u);
        let doppler = ((SOUND + towards) / (SOUND + away)).clamp(0.5, 2.0);
        // The air dulls distant sounds.
        let cutoff = (18_000.0 / (1.0 + dist / 120.0)).max(900.0);
        Placing { gain, pan, doppler, cutoff }
    }
}

/// A pan moving across a block of samples: the equal-power gains (as the
/// synthesiser's voices pan) at the block's ends, eased between them, so
/// that the pan law's sines are worked out once a block, not once a
/// sample.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Panning {
    left: f32,
    right: f32,
    to_left: f32,
    to_right: f32,
}

impl Panning {
    /// From pan `from` to `to` (each -1 left .. 1 right).
    pub(crate) fn new(from: f32, to: f32) -> Panning {
        let ((left, right), (l1, r1)) = (pan_gains(from), pan_gains(to));
        Panning { left, right, to_left: l1 - left, to_right: r1 - right }
    }

    /// The gains (left, right) a fraction `t` (0..1) through the block.
    #[inline]
    pub(crate) fn at(&self, t: f32) -> (f32, f32) {
        (self.left + self.to_left * t, self.right + self.to_right * t)
    }
}

/// `v` as a level (0..1): anything not a number (from a garbled setting)
/// is silence, rather than making everything after it silent.
pub fn level(v: f32) -> f32 {
    if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) }
}

/// A table of one turn of a sine, for the oscillators.
pub struct SineTable {
    t: Vec<f32>,
}

/// Bits of a phase that index the table.
pub const TABLE_BITS: u32 = 11;

impl Default for SineTable {
    fn default() -> SineTable {
        SineTable::new()
    }
}

impl SineTable {
    pub fn new() -> SineTable {
        let n = 1usize << TABLE_BITS;
        SineTable { t: (0..n).map(|i| libm::sinf(i as f32 / n as f32 * core::f32::consts::TAU)).collect() }
    }

    /// The sine at a phase (a whole turn is 2^32).
    #[inline]
    pub fn at(&self, phase: u32) -> f32 {
        self.t[(phase >> (32 - TABLE_BITS)) as usize]
    }
}

/// A phase increment for `hz` at the mixer's rate.
#[inline]
pub fn increment(hz: f32) -> u32 {
    (hz.clamp(0.0, RATE as f32 * 0.49) / RATE as f32 * 4_294_967_296.0) as u32
}

/// A source of white noise (in -1..1, from [`Osc::noise`]) seeded by
/// `seed`.
pub(crate) fn noise(seed: u32) -> Osc {
    Osc::new(Wave::Noise, 0.0, seed)
}

/// A voice that plays one keyed sound of the scene at a time (an engine,
/// a siren, a car's tyres): it fades in when it takes a sound, follows it,
/// and fades out once the sound leaves the scene, freeing itself.
pub(crate) trait Voiced {
    /// The sound it plays, if any.
    fn key(&self) -> Option<u64>;
    /// Starts playing the sound `key`.
    fn start(&mut self, key: u64);
}

/// Gives each sound in `keys` a voice: those playing keep theirs, new ones
/// take a free voice, or wait for one to fade out (so that no voice is
/// cut off mid-sound).
fn assign<V: Voiced>(voices: &mut [V], keys: impl Iterator<Item = u64>) {
    for k in keys {
        if voices.iter().any(|v| v.key() == Some(k)) {
            continue;
        }
        if let Some(v) = voices.iter_mut().find(|v| v.key().is_none()) {
            v.start(k);
        }
    }
}

/// How many engines, sirens and cars' tyres sound at once.
pub const ENGINES: usize = 6;
pub const SIRENS: usize = 3;
pub const TIRES: usize = 4;

/// The mixer: every voice, the tunnel's echo, and a limiter.
pub struct Mixer {
    scene: Scene,
    sines: SineTable,
    engines: Vec<engine::Voice>,
    sirens: Vec<siren::Voice>,
    tires: Vec<sfx::Tire>,
    world: sfx::World,
    cues: sfx::Cues,
    reverb: Reverb,
    limiter: Limiter,
    /// The world's level, eased (silence while paused), and the echo's.
    level: f32,
    send: f32,
    l: Vec<f32>,
    r: Vec<f32>,
}

impl Mixer {
    pub fn new() -> Mixer {
        let rate = RATE as f32;
        Mixer {
            scene: Scene { volume: 1.0, ..Scene::default() },
            sines: SineTable::new(),
            engines: (0..ENGINES).map(|i| engine::Voice::new(i as u32 * 7919 + 1)).collect(),
            sirens: (0..SIRENS).map(|_| siren::Voice::new()).collect(),
            tires: (0..TIRES).map(|i| sfx::Tire::new(i as u32 * 104_729 + 7)).collect(),
            world: sfx::World::new(),
            cues: sfx::Cues::new(),
            reverb: Reverb::new(0.82, 0.35, 1.0, 0.012, rate),
            limiter: Limiter::new(-0.8, 3.0, 120.0, rate),
            level: 0.0,
            send: 0.0,
            l: Vec::new(),
            r: Vec::new(),
        }
    }

    /// What is sounding from now on.
    pub fn set_scene(&mut self, scene: Scene) {
        assign(&mut self.engines, scene.engines.iter().map(|e| e.key));
        assign(&mut self.sirens, scene.sirens.iter().map(|s| s.key));
        assign(&mut self.tires, scene.tires.iter().map(|t| t.key));
        self.scene = Scene { volume: level(scene.volume), ..scene };
    }

    /// Something happens.
    pub fn cue(&mut self, c: Cue) {
        self.cues.start(c, &self.scene.listener);
    }

    /// Renders `out.len() / 2` stereo frames.
    pub fn render(&mut self, out: &mut [i16]) {
        let frames = out.len() / 2;
        let (l, r) = (&mut self.l, &mut self.r);
        l.clear();
        l.resize(frames, 0.0);
        r.clear();
        r.resize(frames, 0.0);
        let scene = &self.scene;
        let ears = scene.listener;
        for v in &mut self.engines {
            v.render(&self.sines, &ears, &scene.engines, l, r);
        }
        for v in &mut self.sirens {
            v.render(&self.sines, &ears, &scene.sirens, l, r);
        }
        for v in &mut self.tires {
            v.render(&self.sines, &ears, &scene.tires, l, r);
        }
        self.world.render(scene, l, r);
        // The world eases in and out of a pause; a tunnel echoes it.
        let target = if scene.paused { 0.0 } else { scene.volume };
        let send_to = scene.tunnel.clamp(0.0, 1.0) * 0.5;
        for i in 0..frames {
            self.level += (target - self.level) * 0.0008;
            self.send += (send_to - self.send) * 0.0005;
            let (wl, wr) = (l[i] * self.level, r[i] * self.level);
            let (el, er) = self.reverb.process(wl * self.send, wr * self.send);
            l[i] = wl + el;
            r[i] = wr + er;
        }
        // The game's own cues, heard through pauses and tunnels alike.
        self.cues.render(&self.sines, scene.volume, l, r);
        for i in 0..frames {
            let (a, b) = self.limiter.process(l[i], r[i]);
            out[i * 2] = (a.clamp(-1.0, 1.0) * 32767.0) as i16;
            out[i * 2 + 1] = (b.clamp(-1.0, 1.0) * 32767.0) as i16;
        }
    }
}

impl Default for Mixer {
    fn default() -> Mixer {
        Mixer::new()
    }
}

/// The root mean square of a signal.
#[cfg(test)]
pub(crate) fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rms16(x: &[i16]) -> f32 {
        let s: f64 = x.iter().map(|&v| (v as f64) * (v as f64)).sum();
        ((s / x.len().max(1) as f64).sqrt() / 32768.0) as f32
    }

    #[test]
    fn placing_falls_off_pans_and_shifts() {
        let ears = Listener { pos: Vec3::ZERO, vel: Vec3::ZERO, right: Vec3::X };
        let near = ears.place(&Emitter { pos: Vec3::new(0.0, 0.0, -5.0), vel: Vec3::ZERO }, 10.0);
        let far = ears.place(&Emitter { pos: Vec3::new(0.0, 0.0, -200.0), vel: Vec3::ZERO }, 10.0);
        assert!(near.gain > far.gain * 5.0);
        assert!(far.cutoff < near.cutoff);
        let right = ears.place(&Emitter { pos: Vec3::new(20.0, 0.0, 0.0), vel: Vec3::ZERO }, 10.0);
        assert!(right.pan > 0.8);
        // Coming closer at 30 m/s: higher; going away: lower.
        let coming = ears.place(&Emitter { pos: Vec3::new(0.0, 0.0, -50.0), vel: Vec3::new(0.0, 0.0, 30.0) }, 10.0);
        let going = ears.place(&Emitter { pos: Vec3::new(0.0, 0.0, -50.0), vel: Vec3::new(0.0, 0.0, -30.0) }, 10.0);
        assert!(coming.doppler > 1.08 && going.doppler < 0.93, "{} {}", coming.doppler, going.doppler);
    }

    #[test]
    fn a_scene_makes_sound_and_silence_is_silent() {
        let mut m = Mixer::new();
        let mut out = alloc::vec![0i16; 4096];
        m.render(&mut out);
        assert_eq!(rms16(&out), 0.0, "sound out of nothing");
        let ears = Listener { pos: Vec3::ZERO, vel: Vec3::ZERO, right: Vec3::X };
        let car = Emitter { pos: Vec3::new(0.0, 0.5, -6.0), vel: Vec3::ZERO };
        let engine = EngineState {
            key: 1,
            emitter: car,
            layout: Layout::V8,
            rpm: 3000.0,
            redline: 6800.0,
            load: 0.7,
            limiter: false,
            boost: false,
            own: true,
        };
        let scene = Scene {
            listener: ears,
            engines: [engine].into_iter().collect(),
            sirens: [SirenState { key: 1, emitter: car, wail: Wail::Wail, own: true }].into_iter().collect(),
            volume: 1.0,
            ..Scene::default()
        };
        m.set_scene(scene);
        for _ in 0..40 {
            m.render(&mut out);
        }
        let level = rms16(&out);
        assert!(level > 0.02 && level < 0.9, "level {level}");
        assert!(out.iter().all(|&v| v != i16::MIN), "clipped");
        m.cue(Cue::Crash { emitter: car, strength: 1.0 });
        m.cue(Cue::Win);
        for _ in 0..20 {
            m.render(&mut out);
        }
        // A volume that is not a number (a garbled setting) is silence,
        // and does not silence what comes after it.
        m.set_scene(Scene { volume: f32::NAN, ..scene });
        for _ in 0..200 {
            m.render(&mut out);
        }
        assert_eq!(rms16(&out), 0.0);
        m.set_scene(scene);
        for _ in 0..40 {
            m.render(&mut out);
        }
        assert!(rms16(&out) > 0.02, "silent after a volume that was not a number");
    }

    #[test]
    fn a_pan_moves_across_a_block_between_its_ends() {
        let p = Panning::new(-0.6, 0.8);
        assert_eq!(p.at(0.0), pan_gains(-0.6));
        let ((l, r), (l1, r1)) = (p.at(1.0), pan_gains(0.8));
        assert!((l - l1).abs() < 1e-6 && (r - r1).abs() < 1e-6);
        // Between, as the pan law has it for as far as a pan moves in a
        // block (a car crossing in front of the ears at speed).
        let (l, r) = Panning::new(0.10, 0.16).at(0.5);
        let (l0, r0) = pan_gains(0.13);
        assert!((l - l0).abs() < 1e-3 && (r - r0).abs() < 1e-3, "{l} {r} against {l0} {r0}");
    }

    #[test]
    fn a_scene_holds_as_many_sounds_as_are_heard() {
        let tire = |key| TireState { key, emitter: Emitter::default(), slip: 0.0, speed: 10.0 };
        let tires: Sounds<TireState, TIRES> = (0..10).map(tire).collect();
        assert_eq!(tires.len(), TIRES);
        assert!(tires.iter().map(|t| t.key).eq(0..TIRES as u64));
        let mut few = Sounds::<TireState, 2>::new();
        assert!(few.push(tire(1)) && few.push(tire(2)) && !few.push(tire(3)));
    }
}
