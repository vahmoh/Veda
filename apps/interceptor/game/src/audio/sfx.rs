//! The world's other sounds and the game's cues.
//!
//! **Tyres** roar on the tarmac with the speed (most of what a passing car
//! sounds like) and squeal when they slide: a narrow band of noise with a
//! wavering tone in it. **Wind** rushes past the ears with the square of
//! the speed. A car **scraping** along a wall grinds and rings, and the
//! **rain** hisses.
//!
//! The **cues** happen once: a crash is a thump, a crunch of crumpling
//! metal, panels ringing and, when hard, glass; a horn sounds two buzzing
//! tones; the menus click, the countdown beeps, the radio squelches and
//! chirps, a pursuit's end is a chime or a falling sting, and the nitrous
//! whooshes.

use alloc::vec::Vec;

use vaudio::synth::filter::{Mode, Svf};

use super::{Cue, Listener, Noise, Placing, RATE, Scene, SineTable, TireState, Voiced, increment, pan_gains};
use num_traits::Float;

/// A voice playing one car's tyres.
pub struct Tire {
    key: Option<u32>,
    fresh: bool,
    place: Placing,
    fade: f32,
    slip: f32,
    speed: f32,
    noise: Noise,
    roar: Svf,
    squeal: Svf,
    air: Svf,
    tone: u32,
    /// The squeal's tone wanders.
    wander: f32,
}

impl Tire {
    pub fn new(seed: u32) -> Tire {
        Tire {
            key: None,
            fresh: true,
            place: Placing::default(),
            fade: 0.0,
            slip: 0.0,
            speed: 0.0,
            noise: Noise::new(seed),
            roar: Svf::new(Mode::LowPass),
            squeal: Svf::new(Mode::BandPass),
            air: Svf::new(Mode::LowPass),
            tone: 0,
            wander: 0.0,
        }
    }

    /// Mixes this block of the tyres it plays into `l` and `r`.
    pub fn render(&mut self, sines: &SineTable, ears: &Listener, tires: &[TireState], l: &mut [f32], r: &mut [f32]) {
        let Some(key) = self.key else { return };
        let rate = RATE as f32;
        let (to_fade, slip1, speed1, place1) = match tires.iter().find(|t| t.key == key) {
            Some(t) => (1.0, t.slip.max(0.0), t.speed.abs(), ears.place(&t.emitter, 8.0)),
            None => (0.0, 0.0, 0.0, self.place),
        };
        if self.fresh {
            (self.slip, self.speed, self.place) = (slip1, speed1, place1);
            self.fresh = false;
        }
        let (slip0, speed0, place0) = (self.slip, self.speed, self.place);
        self.roar.set(220.0 + speed1 * 16.0, 0.6, rate);
        let squeal_hz = 760.0 + slip1.min(20.0) * 22.0;
        self.squeal.set(squeal_hz, 9.0, rate);
        self.air.set(place1.cutoff.min(12_000.0), 0.7, rate);
        let roar_of = |speed: f32| Float::powf(speed / 30.0, 1.4).min(1.6) * 0.42;
        // Tyres squeal past the slip angle of their best grip, and the
        // harder the faster they slide.
        let squeal_of = |slip: f32, speed: f32| {
            let x = ((slip / speed.max(4.0) - 0.06) / 0.14).clamp(0.0, 1.0);
            x * x * (3.0 - 2.0 * x) * (slip / 3.0).min(1.0) * 0.55
        };
        let (roar0, roar1) = (roar_of(speed0), roar_of(speed1));
        let (sq0, sq1) = (squeal_of(slip0, speed0), squeal_of(slip1, speed1));
        let n = l.len();
        for i in 0..n {
            let t = (i + 1) as f32 / n as f32;
            self.fade += (to_fade - self.fade) * 0.0006;
            let roar = self.roar.process(self.noise.sample()) * (roar0 + (roar1 - roar0) * t);
            let sq = sq0 + (sq1 - sq0) * t;
            let mut v = roar;
            if sq > 1e-4 {
                self.wander += (self.noise.sample() * 0.5 - self.wander) * 0.0015;
                let doppler = place0.doppler + (place1.doppler - place0.doppler) * t;
                self.tone = self.tone.wrapping_add(increment(squeal_hz * (1.0 + self.wander * 0.06) * doppler));
                let s = self.squeal.process(self.noise.sample()) * 2.5 + sines.at(self.tone) * 0.35;
                v += s / (1.0 + s.abs()) * sq;
            }
            let gain = place0.gain + (place1.gain - place0.gain) * t;
            let v = self.air.process(v) * gain * self.fade;
            let (gl, gr) = pan_gains(place0.pan + (place1.pan - place0.pan) * t);
            l[i] += v * gl;
            r[i] += v * gr;
        }
        (self.slip, self.speed, self.place) = (slip1, speed1, place1);
        if to_fade == 0.0 && self.fade < 1e-4 {
            self.key = None;
        }
    }
}

impl Voiced for Tire {
    fn key(&self) -> Option<u32> {
        self.key
    }

    fn start(&mut self, key: u32) {
        self.key = Some(key);
        self.fresh = true;
        self.fade = 0.0;
    }
}

/// The wind, a scrape and the rain: sounds of the ears' surroundings.
pub struct World {
    noise: Noise,
    wind: [Svf; 2],
    gust: f32,
    gust_to: f32,
    gust_left: u32,
    scrape_hp: Svf,
    ring: [Svf; 3],
    grit: f32,
    grit_left: u32,
    rain: [Svf; 2],
    /// The levels, eased.
    levels: [f32; 3],
}

impl World {
    pub fn new() -> World {
        let rate = RATE as f32;
        let svf = |mode, hz, q| {
            let mut f = Svf::new(mode);
            f.set(hz, q, rate);
            f
        };
        World {
            noise: Noise::new(0xA11CE),
            wind: [svf(Mode::LowPass, 400.0, 0.5), svf(Mode::LowPass, 400.0, 0.5)],
            gust: 1.0,
            gust_to: 1.0,
            gust_left: 0,
            scrape_hp: svf(Mode::HighPass, 1300.0, 0.7),
            ring: [
                svf(Mode::BandPass, 1870.0, 14.0),
                svf(Mode::BandPass, 3150.0, 16.0),
                svf(Mode::BandPass, 4630.0, 18.0),
            ],
            grit: 0.0,
            grit_left: 0,
            rain: [svf(Mode::HighPass, 2600.0, 0.6), svf(Mode::HighPass, 2600.0, 0.6)],
            levels: [0.0; 3],
        }
    }

    /// Mixes this block of the wind, scraping and rain into `l` and `r`.
    pub fn render(&mut self, scene: &Scene, l: &mut [f32], r: &mut [f32]) {
        let rate = RATE as f32;
        let wind = Float::powi(scene.wind / 45.0, 2).min(1.3);
        let cutoff = 260.0 + scene.wind.clamp(0.0, 90.0) * 32.0;
        for f in &mut self.wind {
            f.set(cutoff, 0.5, rate);
        }
        let targets = [wind, scene.scrape.clamp(0.0, 1.0), scene.rain.clamp(0.0, 1.0)];
        if targets.iter().chain(&self.levels).all(|&v| v < 1e-4) {
            return;
        }
        for i in 0..l.len() {
            for (v, &to) in self.levels.iter_mut().zip(&targets) {
                *v += (to - *v) * 0.0008;
            }
            let [wind, scrape, rain] = self.levels;
            let (mut a, mut b) = (0.0, 0.0);
            if wind > 1e-4 {
                // Gusts: the level wanders every quarter of a second or so.
                if self.gust_left == 0 {
                    self.gust_to = 0.75 + (self.noise.sample() * 0.5 + 0.5) * 0.5;
                    self.gust_left = (rate * 0.25) as u32 + (self.noise.sample().abs() * rate * 0.3) as u32;
                }
                self.gust_left -= 1;
                self.gust += (self.gust_to - self.gust) * 0.0002;
                let g = wind * self.gust * 0.3;
                a += self.wind[0].process(self.noise.sample()) * g;
                b += self.wind[1].process(self.noise.sample()) * g;
            }
            if scrape > 1e-4 {
                // Metal grinding: bursts of grit, and the panels ringing.
                if self.grit_left == 0 {
                    self.grit = 0.3 + self.noise.sample().abs() * 0.7;
                    self.grit_left = (rate * 0.003) as u32 + (self.noise.sample().abs() * rate * 0.009) as u32;
                }
                self.grit_left -= 1;
                let x = self.scrape_hp.process(self.noise.sample()) * self.grit;
                let ring: f32 = self.ring.iter_mut().map(|f| f.process(x)).sum();
                let v = (x * 0.4 + ring * 1.2) * scrape * 0.45;
                a += v;
                b += v;
            }
            if rain > 1e-4 {
                a += self.rain[0].process(self.noise.sample()) * rain * 0.05;
                b += self.rain[1].process(self.noise.sample()) * rain * 0.05;
            }
            l[i] += a;
            r[i] += b;
        }
    }
}

/// How many cues sound at once.
const CUES: usize = 12;

/// A cue sounding.
struct Active {
    cue: Cue,
    /// Samples since it began, the time that is (s), and when it ends.
    step: u32,
    t: f32,
    end: f32,
    gain: f32,
    gl: f32,
    gr: f32,
    hz: [f32; 6],
    phases: [u32; 6],
    filters: [Svf; 2],
    air: Svf,
    noise: Noise,
    /// A crash's crumpling: the current burst's strength and its length
    /// left (s).
    burst: f32,
    burst_left: f32,
}

/// The one-off sounds.
pub struct Cues {
    active: Vec<Active>,
    seed: u32,
}

impl Cues {
    pub fn new() -> Cues {
        Cues { active: Vec::with_capacity(CUES), seed: 0x5EED }
    }

    /// Starts `cue`, as heard by `ears`.
    pub fn start(&mut self, cue: Cue, ears: &Listener) {
        let rate = RATE as f32;
        self.seed = self.seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let mut noise = Noise::new(self.seed);
        let mut rand = |lo: f32, hi: f32| lo + (noise.sample() * 0.5 + 0.5) * (hi - lo);
        let centre = Placing { gain: 1.0, pan: 0.0, doppler: 1.0, cutoff: 18_000.0 };
        let mut hz = [0.0f32; 6];
        let (place, end, gain) = match cue {
            Cue::Crash { emitter, strength } => {
                // The panels' partials, inharmonic; the glass's pings.
                let base = rand(260.0, 360.0);
                hz = [
                    0.0,
                    base,
                    base * rand(2.5, 2.8),
                    base * rand(4.4, 5.0),
                    rand(3000.0, 4500.0),
                    rand(4800.0, 6800.0),
                ];
                (ears.place(&emitter, 12.0), 1.3, 0.35 + 0.65 * strength.clamp(0.0, 1.0))
            }
            Cue::Horn { emitter, secs } => {
                hz[0] = 405.0;
                hz[1] = 508.0;
                (ears.place(&emitter, 10.0), secs.max(0.1) + 0.08, 0.3)
            }
            Cue::Move => (centre, 0.12, 0.09),
            Cue::Select => (centre, 0.32, 0.11),
            Cue::Back => (centre, 0.28, 0.09),
            Cue::Count { last } => (centre, if last { 0.75 } else { 0.2 }, 0.13),
            Cue::Radio => (centre, 0.5, 0.16),
            Cue::Win => (centre, 2.4, 0.14),
            Cue::Lose => (centre, 2.2, 0.15),
            Cue::Boost => (centre, 1.0, 0.32),
        };
        let mut filters = [Svf::new(Mode::BandPass), Svf::new(Mode::HighPass)];
        match cue {
            Cue::Crash { .. } => {
                filters[0].set(1100.0, 0.8, rate);
                filters[1].set(4200.0, 0.7, rate);
            }
            Cue::Horn { .. } => filters[0].set(950.0, 0.8, rate),
            Cue::Radio => {
                filters[0].set(1800.0, 1.4, rate);
                filters[1].set(600.0, 0.7, rate);
            }
            Cue::Lose => {
                filters[0].mode = Mode::LowPass;
                filters[0].set(1400.0, 0.7, rate);
            }
            _ => {}
        }
        let mut air = Svf::new(Mode::LowPass);
        air.set(place.cutoff.min(16_000.0), 0.7, rate);
        let (gl, gr) = pan_gains(place.pan);
        let a = Active {
            cue,
            step: 0,
            t: 0.0,
            end,
            gain: gain * place.gain,
            gl,
            gr,
            hz,
            phases: [0; 6],
            filters,
            air,
            noise,
            burst: 0.0,
            burst_left: 0.0,
        };
        if self.active.len() == CUES {
            // The one nearest its end makes way.
            let oldest = (0..CUES).max_by(|&i, &j| {
                let f = |a: &Active| a.t / a.end;
                f(&self.active[i]).total_cmp(&f(&self.active[j]))
            });
            if let Some(i) = oldest {
                self.active.swap_remove(i);
            }
        }
        self.active.push(a);
    }

    /// Mixes this block of the cues sounding into `l` and `r`, at `volume`.
    pub fn render(&mut self, sines: &SineTable, volume: f32, l: &mut [f32], r: &mut [f32]) {
        for a in &mut self.active {
            for i in 0..l.len() {
                if a.t >= a.end {
                    break;
                }
                let x = a.sample(sines);
                let v = a.air.process(x) * a.gain * volume;
                l[i] += v * a.gl;
                r[i] += v * a.gr;
                a.step += 1;
                a.t = a.step as f32 / RATE as f32;
            }
        }
        self.active.retain(|a| a.t < a.end);
    }
}

/// A decay from 1 with time constant `tau` (s), after `t` seconds.
fn decay(t: f32, tau: f32) -> f32 {
    if t < 0.0 { 0.0 } else { Float::exp(-t / tau) }
}

/// An attack over `a` seconds, held until `hold`, released over `rel`.
fn gate(t: f32, a: f32, hold: f32, rel: f32) -> f32 {
    if t < 0.0 {
        0.0
    } else if t < a {
        t / a
    } else if t < hold {
        1.0
    } else {
        (1.0 - (t - hold) / rel).max(0.0)
    }
}

impl Active {
    /// Advances oscillator `k` at `hz` and returns its sine.
    #[inline]
    fn osc(&mut self, sines: &SineTable, k: usize, hz: f32) -> f32 {
        self.phases[k] = self.phases[k].wrapping_add(increment(hz));
        sines.at(self.phases[k])
    }

    /// Harmonics 1..=`n` of oscillator `k` (falling as 1/h: a buzz).
    #[inline]
    fn buzz(&mut self, sines: &SineTable, k: usize, hz: f32, n: u32) -> f32 {
        self.phases[k] = self.phases[k].wrapping_add(increment(hz));
        let mut s = 0.0;
        let mut ph = 0u32;
        for h in 1..=n {
            ph = ph.wrapping_add(self.phases[k]);
            if hz * h as f32 > RATE as f32 * 0.45 {
                break;
            }
            s += sines.at(ph) / h as f32;
        }
        s
    }

    /// A bell-like note at `hz` struck at `at` seconds, ringing for `tau`.
    fn chime(&mut self, sines: &SineTable, k: usize, hz: f32, at: f32, tau: f32) -> f32 {
        let t = self.t - at;
        if t < 0.0 {
            return 0.0;
        }
        self.phases[k] = self.phases[k].wrapping_add(increment(hz));
        let p = self.phases[k];
        let tone = sines.at(p) + 0.35 * sines.at(p.wrapping_mul(2)) + 0.12 * sines.at(p.wrapping_mul(3));
        tone * decay(t, tau) * (t / 0.003).min(1.0)
    }

    fn sample(&mut self, sines: &SineTable) -> f32 {
        let t = self.t;
        match self.cue {
            Cue::Crash { strength, .. } => {
                let s = strength.clamp(0.0, 1.0);
                // The thump: a low tone falling.
                let thump = self.osc(sines, 0, 38.0 + 55.0 * decay(t, 0.07)) * decay(t, 0.14) * 0.9;
                // The crunch: bursts of noise, crumpling.
                self.burst_left -= 1.0 / RATE as f32;
                if self.burst_left <= 0.0 && t < 0.06 + 0.3 * s {
                    self.burst = 0.35 + self.noise.sample().abs() * 0.65;
                    self.burst_left = 0.004 + self.noise.sample().abs() * 0.025;
                }
                let crunch_env = self.burst * decay(t, 0.12 + 0.2 * s);
                let crunch = self.filters[0].process(self.noise.sample()) * crunch_env * 1.4;
                // The panels ringing.
                let (a, b, c) = (self.hz[1], self.hz[2], self.hz[3]);
                let metal = (self.osc(sines, 1, a) + 0.6 * self.osc(sines, 2, b) + 0.35 * self.osc(sines, 3, c))
                    * decay(t, 0.3)
                    * 0.16;
                let mut v = thump + crunch + metal;
                if s > 0.5 {
                    // Glass: a shatter and its pieces.
                    let shatter = self.filters[1].process(self.noise.sample()) * decay(t - 0.01, 0.09) * 0.5;
                    let (p, q) = (self.hz[4], self.hz[5]);
                    let pings = self.chime(sines, 4, p, 0.03, 0.06) * 0.15 + self.chime(sines, 5, q, 0.09, 0.05) * 0.12;
                    v += (shatter + pings) * (s - 0.5) * 2.0;
                }
                v
            }
            Cue::Horn { secs, .. } => {
                let env = gate(t, 0.012, secs, 0.06);
                let (a, b) = (self.hz[0], self.hz[1]);
                let tone = self.buzz(sines, 0, a, 8) + self.buzz(sines, 1, b, 8);
                (self.filters[0].process(tone) * 1.6 + tone * 0.25) * env
            }
            Cue::Move => self.osc(sines, 0, 1318.5) * decay(t, 0.025) * (t / 0.001).min(1.0),
            Cue::Select => self.chime(sines, 0, 987.8, 0.0, 0.05) + self.chime(sines, 1, 1318.5, 0.07, 0.07),
            Cue::Back => self.chime(sines, 0, 1174.7, 0.0, 0.045) + self.chime(sines, 1, 880.0, 0.06, 0.06),
            Cue::Count { last } => {
                let (hz, hold) = if last { (1760.0, 0.5) } else { (880.0, 0.13) };
                let env = gate(t, 0.003, hold, if last { 0.2 } else { 0.05 });
                self.phases[0] = self.phases[0].wrapping_add(increment(hz));
                let p = self.phases[0];
                (sines.at(p) + 0.3 * sines.at(p.wrapping_mul(3)) + 0.12 * sines.at(p.wrapping_mul(5))) * env
            }
            Cue::Radio => {
                // Squelch, a chirp, then the open channel's hiss.
                let squelch = self.filters[0].process(self.noise.sample()) * gate(t, 0.004, 0.06, 0.02) * 0.9;
                let chirp_hz = 1300.0 + 1100.0 * ((t - 0.09) / 0.06).clamp(0.0, 1.0);
                let chirp = self.osc(sines, 0, chirp_hz) * gate(t - 0.09, 0.003, 0.06, 0.01) * 0.6;
                let hiss = self.filters[1].process(self.noise.sample()) * gate(t - 0.16, 0.02, 0.24, 0.1) * 0.08;
                squelch + chirp + hiss
            }
            Cue::Win => {
                // An arpeggio up to a ringing chord.
                let notes = [(523.25, 0.0), (659.25, 0.11), (783.99, 0.22), (1046.5, 0.33)];
                let mut v = 0.0;
                for (k, &(hz, at)) in notes.iter().enumerate() {
                    v += self.chime(sines, k, hz, at, if k == 3 { 0.9 } else { 0.5 });
                }
                v + self.chime(sines, 4, 1318.5, 0.33, 0.8) * 0.6 + self.chime(sines, 5, 1568.0, 0.33, 0.7) * 0.5
            }
            Cue::Lose => {
                // Falling, darker.
                let notes = [(392.0, 0.0, 0.35), (311.13, 0.22, 0.35), (261.63, 0.44, 0.9)];
                let mut v = 0.0;
                for (k, &(hz, at, tau)) in notes.iter().enumerate() {
                    let tn = t - at;
                    if tn >= 0.0 {
                        v += self.buzz(sines, k, hz, 7) * decay(tn, tau) * (tn / 0.01).min(1.0);
                    }
                }
                self.filters[0].process(v)
            }
            Cue::Boost => {
                // A whoosh rising, on a low thump.
                if self.step.is_multiple_of(16) {
                    let hz = 250.0 * Float::powf(12.0f32, (t / 0.5).min(1.0));
                    self.filters[0].set(hz, 1.6, RATE as f32);
                }
                let env = if t < 0.06 { t / 0.06 } else { decay(t - 0.06, 0.35) };
                let whoosh = self.filters[0].process(self.noise.sample()) * env * 1.6;
                let thump = self.osc(sines, 0, 55.0) * decay(t, 0.1) * 0.6;
                whoosh + thump
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{Emitter, rms};
    use alloc::vec;
    use glam::Vec3;

    #[test]
    fn every_cue_sounds_and_ends() {
        let sines = SineTable::new();
        let ears = Listener { pos: Vec3::ZERO, vel: Vec3::ZERO, right: Vec3::X };
        let near = Emitter { pos: Vec3::new(3.0, 0.0, -5.0), vel: Vec3::ZERO };
        let cues = [
            Cue::Crash { emitter: near, strength: 1.0 },
            Cue::Crash { emitter: near, strength: 0.2 },
            Cue::Horn { emitter: near, secs: 0.4 },
            Cue::Move,
            Cue::Select,
            Cue::Back,
            Cue::Count { last: false },
            Cue::Count { last: true },
            Cue::Radio,
            Cue::Win,
            Cue::Lose,
            Cue::Boost,
        ];
        for cue in cues {
            let mut c = Cues::new();
            c.start(cue, &ears);
            let (mut l, mut r) = (vec![0.0; 4800], vec![0.0; 4800]);
            c.render(&sines, 1.0, &mut l, &mut r);
            let level = rms(&l).max(rms(&r));
            let peak = l.iter().chain(&r).fold(0.0f32, |m, v| m.max(v.abs()));
            std::println!("{cue:?}: RMS {level:.3}, peak {peak:.3}");
            assert!(level > 0.003 && peak < 1.5, "{cue:?}: RMS {level}, peak {peak}");
            for _ in 0..60 {
                c.render(&sines, 1.0, &mut l, &mut r);
            }
            assert!(c.active.is_empty(), "{cue:?} never ended");
        }
    }

    #[test]
    fn tyres_squeal_when_they_slide() {
        let sines = SineTable::new();
        let ears = Listener { pos: Vec3::ZERO, vel: Vec3::ZERO, right: Vec3::X };
        let level = |slip: f32| {
            let mut t = Tire::new(9);
            t.start(4);
            let s = TireState {
                key: 4,
                emitter: Emitter { pos: Vec3::new(0.0, 0.0, -6.0), vel: Vec3::ZERO },
                slip,
                speed: 20.0,
            };
            let (mut l, mut r) = (vec![0.0; 2048], vec![0.0; 2048]);
            for _ in 0..20 {
                l.fill(0.0);
                r.fill(0.0);
                t.render(&sines, &ears, &[s], &mut l, &mut r);
            }
            rms(&l)
        };
        let (rolling, sliding) = (level(0.0), level(8.0));
        std::println!("rolling {rolling:.3}, sliding {sliding:.3}");
        assert!(rolling > 0.005 && sliding > rolling * 2.0);
    }
}
