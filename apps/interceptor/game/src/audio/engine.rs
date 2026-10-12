//! Engines, modelled as what makes their sound: each cylinder's firing
//! sends a pulse of gas into its bank's exhaust, the pipes ring with it,
//! and the silencer takes the edge off.
//!
//! A four-stroke cylinder fires once every two turns of the crank, so an
//! engine of `n` cylinders fires `n` times a cycle, evenly spaced. Which
//! bank each firing goes to gives an engine its character: a cross-plane
//! V8 fires its banks unevenly (left, right, right, left, …), so each pipe
//! hears an uneven beat, the V8's burble; a flat six and a V12 alternate
//! evenly, smooth and rasping. No two firings are quite alike (stronger at
//! idle, its lope). Under load the pulses are strong and the silencer
//! opens; off the throttle they fall to the pumping of the pistons, and
//! from high revs unburnt fuel pops in the hot exhaust. The intake roars
//! with the load, and a supercharger whines with the revs.

use vaudio::synth::filter::{DcBlock, Mode, OnePole, Svf};

use super::{EngineState, Listener, Noise, Placing, RATE, SineTable, Voiced, pan_gains};
use num_traits::Float;

/// How an engine's cylinders are laid out (what it sounds like).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// A buzzing four (small cars).
    I4,
    /// A diesel four (vans): low and clattering.
    Diesel,
    /// A rasping flat six (sports cars).
    Flat6,
    /// A smooth V6 (big cars).
    V6,
    /// A burbling cross-plane V8 (muscle cars).
    V8,
    /// A supercharged V8, whining over its burble (the police).
    V8Charged,
    /// A screaming V12 (supercars).
    V12,
}

/// The sound of a layout.
struct Character {
    /// Cylinders (firings a cycle), and the bank each firing goes to.
    cylinders: usize,
    banks: [u8; 12],
    /// How much quieter the second bank's pipe is.
    second: f32,
    /// Each bank's pipe length (m).
    pipes: [f32; 2],
    /// The silencer's cut-off at idle and wide open (Hz).
    muffle: (f32, f32),
    /// A pulse's length (ms) and how alike the firings are (0 same).
    pulse: f32,
    jitter: f32,
    /// The combustion's own rasp, the intake's roar (centre Hz, level),
    /// the supercharger's whine (ratio to the crank, level) and the
    /// diesel's clatter.
    rasp: f32,
    intake: (f32, f32),
    whine: (f32, f32),
    clatter: f32,
    /// Loudness, levelling the layouts.
    gain: f32,
}

impl Layout {
    fn character(self) -> Character {
        let even = |n: usize| {
            let mut b = [0u8; 12];
            for (i, v) in b.iter_mut().take(n).enumerate() {
                *v = (i % 2) as u8;
            }
            b
        };
        match self {
            Layout::I4 => Character {
                cylinders: 4,
                banks: [0; 12],
                second: 1.0,
                pipes: [1.9, 1.9],
                muffle: (650.0, 3400.0),
                pulse: 1.0,
                jitter: 0.07,
                rasp: 0.35,
                intake: (1700.0, 0.35),
                whine: (0.0, 0.0),
                clatter: 0.0,
                gain: 0.75,
            },
            Layout::Diesel => Character {
                cylinders: 4,
                banks: [0; 12],
                second: 1.0,
                pipes: [2.4, 2.4],
                muffle: (420.0, 1700.0),
                pulse: 0.7,
                jitter: 0.12,
                rasp: 0.2,
                intake: (900.0, 0.3),
                whine: (0.0, 0.0),
                clatter: 0.45,
                gain: 1.25,
            },
            Layout::Flat6 => Character {
                cylinders: 6,
                banks: even(6),
                second: 0.92,
                pipes: [1.25, 1.31],
                muffle: (900.0, 5200.0),
                pulse: 0.8,
                jitter: 0.05,
                rasp: 0.5,
                intake: (2100.0, 0.4),
                whine: (0.0, 0.0),
                clatter: 0.0,
                gain: 1.05,
            },
            Layout::V6 => Character {
                cylinders: 6,
                banks: even(6),
                second: 0.9,
                pipes: [1.9, 2.02],
                muffle: (520.0, 2900.0),
                pulse: 1.0,
                jitter: 0.06,
                rasp: 0.3,
                intake: (1300.0, 0.3),
                whine: (0.0, 0.0),
                clatter: 0.0,
                gain: 1.15,
            },
            Layout::V8 | Layout::V8Charged => Character {
                cylinders: 8,
                // Firing order 1-8-4-3-6-5-7-2, odd cylinders on the left.
                banks: [0, 1, 1, 0, 1, 0, 0, 1, 0, 0, 0, 0],
                second: 0.8,
                pipes: [2.2, 2.38],
                muffle: (420.0, 2700.0),
                pulse: 1.25,
                jitter: 0.09,
                rasp: 0.3,
                intake: (1100.0, 0.3),
                // A Roots blower's lobes, geared up from the crank.
                whine: if self == Layout::V8Charged { (19.0, 0.035) } else { (0.0, 0.0) },
                clatter: 0.0,
                gain: 1.3,
            },
            Layout::V12 => Character {
                cylinders: 12,
                banks: even(12),
                second: 0.94,
                pipes: [1.45, 1.52],
                muffle: (1300.0, 7500.0),
                pulse: 0.55,
                jitter: 0.035,
                rasp: 0.55,
                intake: (2600.0, 0.4),
                whine: (0.0, 0.0),
                clatter: 0.0,
                gain: 1.75,
            },
        }
    }
}

/// The speed of sound in a hot exhaust (m/s).
const HOT: f32 = 520.0;
/// The longest pipe's echo, in samples (a power of two).
const PIPE: usize = 1024;

/// One bank's exhaust: the pipe's echo, damped and inverted at its open
/// end, which makes it ring at its resonances.
struct Pipe {
    line: [f32; PIPE],
    at: usize,
    delay: usize,
    damp: OnePole,
}

impl Pipe {
    fn new() -> Pipe {
        Pipe { line: [0.0; PIPE], at: 0, delay: 400, damp: OnePole::new(2400.0, RATE as f32) }
    }

    fn set_length(&mut self, metres: f32) {
        self.delay = ((2.0 * metres / HOT) * RATE as f32).clamp(8.0, (PIPE - 1) as f32) as usize;
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let back = self.line[(self.at + PIPE - self.delay) & (PIPE - 1)];
        let y = x - 0.5 * self.damp.process(back);
        self.line[self.at] = y;
        self.at = (self.at + 1) & (PIPE - 1);
        y
    }
}

/// A voice playing one engine.
pub struct Voice {
    key: Option<u32>,
    fresh: bool,
    layout: Layout,
    c: Character,
    /// The cycle's angle (two turns of the crank are 2^32) and the firing
    /// it is at.
    cycle: u32,
    slot: usize,
    /// The pulse sounding: its phase through half a sine, the step, its
    /// strength and bank.
    pulse: u32,
    pulse_step: u32,
    pulse_amp: f32,
    pulse_bank: usize,
    whine: u32,
    rpm: f32,
    load: f32,
    place: Placing,
    fade: f32,
    noise: Noise,
    pipes: [Pipe; 2],
    silencer: Svf,
    body: Svf,
    intake: Svf,
    clatter: Svf,
    air: Svf,
    dc: DcBlock,
    /// Pops left after lifting off at high revs (s); the time to the next.
    crackle: f32,
    pop: f32,
    pop_left: f32,
    last_load: f32,
    /// The rev limiter's cut (its phase).
    cut: f32,
}

impl Voice {
    pub fn new(seed: u32) -> Voice {
        let layout = Layout::V8;
        let mut v = Voice {
            key: None,
            fresh: true,
            layout,
            c: layout.character(),
            cycle: 0,
            slot: 0,
            pulse: 0,
            pulse_step: 0,
            pulse_amp: 0.0,
            pulse_bank: 0,
            whine: 0,
            rpm: 800.0,
            load: 0.0,
            place: Placing::default(),
            fade: 0.0,
            noise: Noise::new(seed),
            pipes: [Pipe::new(), Pipe::new()],
            silencer: Svf::new(Mode::LowPass),
            body: Svf::new(Mode::BandPass),
            intake: Svf::new(Mode::BandPass),
            clatter: Svf::new(Mode::HighPass),
            air: Svf::new(Mode::LowPass),
            dc: DcBlock::default(),
            crackle: 0.0,
            pop: 0.0,
            pop_left: 0.0,
            last_load: 0.0,
            cut: 0.0,
        };
        v.set_layout(layout);
        v
    }

    fn set_layout(&mut self, layout: Layout) {
        self.layout = layout;
        self.c = layout.character();
        for (p, &m) in self.pipes.iter_mut().zip(&self.c.pipes) {
            p.set_length(m);
        }
        self.clatter.set(2500.0, 0.8, RATE as f32);
    }

    /// Mixes this block of the engine it plays into `l` and `r`.
    pub fn render(
        &mut self,
        sines: &SineTable,
        ears: &Listener,
        engines: &[EngineState],
        l: &mut [f32],
        r: &mut [f32],
    ) {
        let Some(key) = self.key else { return };
        let rate = RATE as f32;
        let state = engines.iter().find(|e| e.key == key);
        let (to_fade, rpm1, load1, place1, limiter, boost) = match state {
            Some(e) => {
                if e.layout != self.layout {
                    self.set_layout(e.layout);
                }
                let near = if e.own { 10.0 } else { 7.0 };
                (1.0, e.rpm.max(300.0), e.load.clamp(0.0, 1.0), ears.place(&e.emitter, near), e.limiter, e.boost)
            }
            None => (0.0, self.rpm, self.load, self.place, false, false),
        };
        if self.fresh {
            (self.rpm, self.load, self.place) = (rpm1, load1, place1);
            self.fresh = false;
        }
        // Lifting off from high revs: the exhaust pops for a moment.
        if self.last_load > 0.6 && load1 < 0.1 && rpm1 > 3800.0 {
            self.crackle = 0.6 + self.noise.sample().abs() * 0.5;
        }
        self.last_load = load1;
        let c = &self.c;
        // The silencer opens with the load and revs; the pipes' body rises
        // with the revs.
        let open = (load1 * 0.7 + 0.3) * Float::sqrt((rpm1 / 7000.0).clamp(0.1, 1.0));
        self.silencer.set(c.muffle.0 + (c.muffle.1 - c.muffle.0) * open, 0.9, rate);
        self.body.set(90.0 + rpm1 * 0.02, 1.2, rate);
        self.intake.set(c.intake.0 * (0.7 + rpm1 / 9000.0), 1.6, rate);
        self.air.set(place1.cutoff.min(12_000.0), 0.7, rate);
        // Pulses shorter than the gap between firings, and quieter the
        // faster they come, so that the revs raise the pitch more than the
        // loudness.
        let fires_hz = rpm1 / 120.0 * c.cylinders as f32;
        let width = (c.pulse * 0.001).min(0.6 / fires_hz.max(1.0));
        let pulse_step = (2_147_483_648.0 / (width * rate)) as u32;
        let density = Float::powf(60.0 / fires_hz.max(20.0), 0.35);
        let strength = (0.12 + 0.88 * Float::powf(load1, 0.8)) * density * if boost { 1.25 } else { 1.0 };
        // At idle the firings vary most.
        let jitter = c.jitter * (1.0 + 2.0 * (1.0 - rpm1 / 2000.0).clamp(0.0, 1.0));
        let rasp = c.rasp * (0.3 + 0.7 * load1);
        let intake = c.intake.1 * load1 * (rpm1 / 6000.0).min(1.2);
        let whine = c.whine.1 * Float::powi(rpm1 / 6000.0, 2) * (0.3 + 0.7 * load1) * if boost { 1.6 } else { 1.0 };
        let clatter = c.clatter * (0.35 + 0.65 * load1);
        let n = l.len();
        let (rpm0, place0) = (self.rpm, self.place);
        let gain_of = |p: &Placing| p.gain * c.gain * 0.5;
        let (g0, g1) = (gain_of(&place0), gain_of(&place1));
        for i in 0..n {
            let t = (i + 1) as f32 / n as f32;
            let rpm = rpm0 + (rpm1 - rpm0) * t;
            let doppler = place0.doppler + (place1.doppler - place0.doppler) * t;
            self.fade += (to_fade - self.fade) * 0.0004;
            let heard = rpm * doppler;
            self.cycle = self.cycle.wrapping_add(super::increment(heard / 120.0));
            // At the limiter, the fuel is cut a dozen times a second.
            if limiter {
                self.cut = Float::fract(self.cut + 12.0 / rate);
            }
            let slot = ((self.cycle as u64 * c.cylinders as u64) >> 32) as usize;
            if slot != self.slot {
                // A firing (or none, while the fuel is cut).
                self.slot = slot;
                let mut amp = strength * (1.0 + self.noise.sample() * jitter);
                if limiter && self.cut > 0.5 {
                    amp *= 0.06;
                }
                let bank = usize::from(c.banks[slot]);
                self.pulse = 0;
                self.pulse_step = pulse_step;
                self.pulse_amp = if bank == 1 { amp * c.second } else { amp };
                self.pulse_bank = bank;
            }
            // The pulse: half a sine, with the combustion's rasp on it.
            let mut gas = 0.0;
            let mut shape = 0.0;
            if self.pulse_step != 0 {
                shape = sines.at(self.pulse);
                gas = shape * self.pulse_amp * (1.0 + self.noise.sample() * rasp);
                let (next, over) = self.pulse.overflowing_add(self.pulse_step);
                if over || next >= 0x8000_0000 {
                    self.pulse_step = 0;
                } else {
                    self.pulse = next;
                }
            }
            // Pops: unburnt fuel lighting in the exhaust.
            if self.crackle > 0.0 {
                self.crackle -= 1.0 / rate;
                self.pop -= 1.0 / rate;
                if self.pop <= 0.0 {
                    self.pop = 0.04 + self.noise.sample().abs() * 0.11;
                    self.pop_left = 0.006 + self.noise.sample().abs() * 0.008;
                }
                if self.pop_left > 0.0 {
                    self.pop_left -= 1.0 / rate;
                    gas += self.noise.sample() * 1.4;
                }
            }
            let (a, b) = if self.pulse_bank == 0 { (gas, 0.0) } else { (0.0, gas) };
            let exhaust = self.pipes[0].process(a) + self.pipes[1].process(b);
            let muffled = self.silencer.process(exhaust);
            let mut v = muffled + self.body.process(muffled) * 1.5;
            // The intake's roar and the diesel's clatter follow the firings.
            v += self.intake.process(self.noise.sample()) * intake * (0.5 + shape);
            if clatter > 0.0 {
                v += self.clatter.process(self.noise.sample()) * clatter * shape * shape;
            }
            if whine > 0.0 {
                let w = self.whine.wrapping_add(super::increment(heard / 60.0 * c.whine.0));
                self.whine = w;
                v += sines.at(w) * whine;
            }
            let v = self.air.process(self.dc.process(v)) * (g0 + (g1 - g0) * t) * self.fade;
            let pan = place0.pan + (place1.pan - place0.pan) * t;
            let (gl, gr) = pan_gains(pan);
            l[i] += v * gl;
            r[i] += v * gr;
        }
        self.rpm = rpm1;
        self.load = load1;
        self.place = place1;
        if to_fade == 0.0 && self.fade < 1e-4 {
            self.key = None;
        }
    }
}

impl Voiced for Voice {
    fn key(&self) -> Option<u32> {
        self.key
    }

    fn start(&mut self, key: u32) {
        self.key = Some(key);
        self.fresh = true;
        self.fade = 0.0;
        self.crackle = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{Emitter, rms};
    use alloc::vec;
    use glam::Vec3;

    fn run(layout: Layout, rpm: f32, load: f32) -> f32 {
        let sines = SineTable::new();
        let ears = Listener { pos: Vec3::ZERO, vel: Vec3::ZERO, right: Vec3::X };
        let e = EngineState {
            key: 1,
            emitter: Emitter { pos: Vec3::new(0.0, 0.0, -10.0), vel: Vec3::ZERO },
            layout,
            rpm,
            redline: 7000.0,
            load,
            limiter: false,
            boost: false,
            own: false,
        };
        let mut v = Voice::new(3);
        v.start(1);
        let (mut l, mut r) = (vec![0.0; 1024], vec![0.0; 1024]);
        for _ in 0..30 {
            l.fill(0.0);
            r.fill(0.0);
            v.render(&sines, &ears, &[e], &mut l, &mut r);
        }
        assert!(l.iter().chain(&r).all(|x| x.is_finite()));
        rms(&l)
    }

    #[test]
    fn engines_rev_and_take_load() {
        for layout in
            [Layout::I4, Layout::Diesel, Layout::Flat6, Layout::V6, Layout::V8, Layout::V8Charged, Layout::V12]
        {
            let idle = run(layout, 850.0, 0.05);
            let cruise = run(layout, 3000.0, 0.4);
            let flat_out = run(layout, 6500.0, 1.0);
            std::println!("{layout:?}: idle {idle:.3}, cruising {cruise:.3}, flat out {flat_out:.3}");
            assert!(idle > 0.002 && idle < cruise && cruise < flat_out && flat_out < 0.5, "{layout:?}");
        }
    }
}
