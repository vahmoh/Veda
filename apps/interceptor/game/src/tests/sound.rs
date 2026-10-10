//! The sounds of a chase: a pursuit through traffic drives the mixer as
//! the game does, and what comes out is sound (never silence, never
//! clipping, never broken), with every voice within its limits.
//!
//! `INTERCEPTOR_SOUNDS=dir` also writes what was heard as WAV files with
//! their spectrograms (PNG, time across, frequency up on a log scale), and
//! a bench of the voices on their own: each engine revving through its
//! range, a police car going by with its siren, and every cue.

use alloc::format;
use alloc::vec;
use alloc::vec::Vec;

use vaudio::fft::{RealFft, hann};

use crate::audio::{
    Cue, ENGINES, Emitter, EngineState, Layout, Listener, Mixer, RATE, SIRENS, Scene, SirenState, TIRES, TireState,
    Wail,
};
use crate::carmodel::Kind;
use crate::env::{Environment, TimeOfDay, Weather};
use crate::pursuit::{Pursuit, Wanted};
use crate::sound::Sound;
use crate::vehicle::Spec;
use crate::world::World;
use vmath::Vec3;

/// Writes stereo `pcm` as a WAV file and its spectrogram as a PNG.
fn save(dir: &str, name: &str, pcm: &[i16]) {
    let wav = vaudio::wav::write(pcm, 2, RATE, &vaudio::Tags::default());
    std::fs::write(format!("{dir}/{name}.wav"), wav).unwrap();
    let image = spectrogram(pcm);
    std::fs::write(format!("{dir}/{name}.png"), vimage::png::encode(&image, 6).unwrap()).unwrap();
}

/// The spectrogram of stereo `pcm` (its mid channel).
fn spectrogram(pcm: &[i16]) -> vimage::Image {
    const N: usize = 4096;
    const HEIGHT: u32 = 480;
    let (lo, hi) = (30.0f32, 20_000.0f32);
    let mono: Vec<f32> = pcm.as_chunks::<2>().0.iter().map(|f| (f[0] as f32 + f[1] as f32) / 65536.0).collect();
    let hop = (mono.len() / 1600).max(240);
    let columns = mono.len().saturating_sub(N) / hop;
    let window = hann(N);
    let norm = 2.0 / window.iter().sum::<f32>();
    let mut fft = RealFft::new(N);
    let (mut frame, mut re, mut im) = (vec![0.0; N], vec![0.0; N / 2 + 1], vec![0.0; N / 2 + 1]);
    let mut px = vec![0xFF00_0000u32; columns * HEIGHT as usize];
    // Each row's band of bins, on a log scale.
    let freq = |y: f32| lo * (hi / lo).powf(1.0 - y / HEIGHT as f32);
    let bin = |f: f32| ((f * N as f32 / RATE as f32) as usize).min(N / 2);
    for x in 0..columns {
        for (k, f) in frame.iter_mut().enumerate() {
            *f = mono[x * hop + k] * window[k];
        }
        fft.forward(&frame, &mut re, &mut im);
        for y in 0..HEIGHT {
            let (a, b) = (bin(freq(y as f32 + 1.0)), bin(freq(y as f32)));
            let m = (a..=b.max(a)).map(|k| (re[k] * re[k] + im[k] * im[k]).sqrt()).fold(0.0f32, f32::max) * norm;
            let db = 20.0 * m.max(1e-9).log10();
            px[y as usize * columns + x] = color(((db + 100.0) / 100.0).clamp(0.0, 1.0));
        }
    }
    // Lines at 100 Hz, 1 kHz and 10 kHz.
    for f in [100.0f32, 1000.0, 10_000.0] {
        let y = ((1.0 - (f / lo).ln() / (hi / lo).ln()) * HEIGHT as f32) as usize;
        for x in (0..columns).step_by(3) {
            px[y * columns + x] = 0xFF80_8080;
        }
    }
    vimage::Image { width: columns as u32, height: HEIGHT, pixels: px }
}

/// Black through blue, magenta and orange to pale yellow.
fn color(v: f32) -> u32 {
    let stops = [[0.0, 0.0, 0.0], [0.1, 0.05, 0.45], [0.65, 0.1, 0.55], [0.98, 0.5, 0.1], [1.0, 0.98, 0.7]];
    let x = v * (stops.len() - 1) as f32;
    let i = (x as usize).min(stops.len() - 2);
    let t = x - i as f32;
    let c = |k: usize| ((stops[i][k] + (stops[i + 1][k] - stops[i][k]) * t) * 255.0) as u32;
    0xFF00_0000 | c(0) << 16 | c(1) << 8 | c(2)
}

/// The RMS of stereo `pcm`, 0..1.
fn level(pcm: &[i16]) -> f32 {
    let s: f64 = pcm.iter().map(|&v| f64::from(v) * f64::from(v)).sum();
    ((s / pcm.len().max(1) as f64).sqrt() / 32768.0) as f32
}

#[test]
fn a_chase_sounds() {
    let mut game = crate::game::Game::new(World::new(), Environment::new(TimeOfDay::Night, Weather::Clear));
    game.start_traffic();
    let p = game.player;
    game.cars[p].siren = true;
    let wanted = Wanted {
        kind: Kind::Muscle,
        spec: Spec::sports(),
        paint: Vec3::new(0.5, 0.02, 0.02),
        skill: 0.95,
        aggression: 0.6,
    };
    let mut chase = Pursuit::start(&mut game, &wanted, 60.0, 25.0);
    game.set_autopilot(true, Some(chase.suspect));
    let mut sound = Sound::new();
    let mut mixer = Mixer::new();
    let (fps, secs) = (60usize, 40usize);
    let dt = 1.0 / fps as f32;
    let mut block = vec![0i16; RATE as usize / fps * 2];
    let mut pcm = Vec::with_capacity(RATE as usize * secs * 2);
    let mut crashes = 0;
    let mut most = [0usize; 3];
    for _ in 0..secs * fps {
        game.update(dt);
        chase.update(&mut game, dt);
        game.frame(dt);
        let (scene, cues) = sound.update(&game, 1.0, dt);
        most = [most[0].max(scene.engines.len()), most[1].max(scene.sirens.len()), most[2].max(scene.tires.len())];
        mixer.set_scene(scene);
        for c in cues {
            crashes += usize::from(matches!(c, Cue::Crash { .. }));
            mixer.cue(c);
        }
        mixer.render(&mut block);
        pcm.extend_from_slice(&block);
    }
    let rms = level(&pcm);
    let clipped = pcm.iter().filter(|&&v| v.unsigned_abs() >= 32_000).count();
    // The quietest second: the engines never all fall silent.
    let quietest = pcm.chunks(RATE as usize * 2).map(level).fold(1.0f32, f32::min);
    std::println!(
        "{secs} s of a chase: RMS {rms:.3}, quietest second {quietest:.3}, {clipped} samples near full scale, \
         {crashes} crash sounds, at most {most:?} engines, sirens and tyres heard; the chase {:?}",
        chase.state
    );
    assert!(most[0] <= ENGINES && most[1] <= SIRENS && most[2] <= TIRES);
    assert!(most[0] >= 2 && most[1] >= 1, "{most:?}");
    assert!(rms > 0.03 && rms < 0.5, "RMS {rms}");
    assert!(quietest > 0.005, "a second of near silence ({quietest})");
    assert!(clipped < pcm.len() / 1000, "{clipped} samples near full scale");
    if let Ok(dir) = std::env::var("INTERCEPTOR_SOUNDS") {
        save(&dir, "chase", &pcm);
    }
}

/// Records `secs` seconds of what `at(t, mixer)` sets the mixer to do,
/// in blocks of 10 ms.
fn record(secs: f32, mut at: impl FnMut(f32, &mut Mixer)) -> Vec<i16> {
    let mut mixer = Mixer::new();
    let frames = RATE as usize / 100;
    let mut block = vec![0i16; frames * 2];
    let mut pcm = Vec::new();
    for k in 0..(secs * 100.0) as usize {
        at(k as f32 / 100.0, &mut mixer);
        mixer.render(&mut block);
        pcm.extend_from_slice(&block);
    }
    pcm
}

#[test]
fn sound_bench() {
    let Ok(dir) = std::env::var("INTERCEPTOR_SOUNDS") else { return };
    let ears = Listener { pos: Vec3::ZERO, vel: Vec3::ZERO, right: Vec3::X };
    // Each engine: idling, revved flat out to the limiter, let go (its
    // pops), idling again; heard from 8 m.
    let layouts = [Layout::I4, Layout::Diesel, Layout::Flat6, Layout::V6, Layout::V8, Layout::V8Charged, Layout::V12];
    let each = 9.0;
    let pcm = record(each * layouts.len() as f32, |t, m| {
        let k = (t / each) as usize;
        let t = t - k as f32 * each;
        let red = if layouts[k] == Layout::Diesel { 4500.0 } else { 7000.0 };
        let (rpm, load) = if t < 1.5 {
            (850.0, 0.05)
        } else if t < 4.5 {
            (850.0 + (red - 850.0) * ((t - 1.5) / 3.0), 1.0)
        } else if t < 5.2 {
            (red, 1.0)
        } else if t < 7.5 {
            (850.0 + (red - 850.0) * (-(t - 5.2) * 1.4).exp(), 0.0)
        } else {
            (850.0, 0.05)
        };
        let engine = EngineState {
            key: k as u32 + 1,
            emitter: Emitter { pos: Vec3::new(0.0, 0.5, -8.0), vel: Vec3::ZERO },
            layout: layouts[k],
            rpm,
            redline: red,
            load,
            limiter: rpm >= red - 1.0,
            boost: false,
            own: true,
        };
        m.set_scene(Scene { listener: ears, engines: vec![engine], volume: 1.0, ..Scene::default() });
    });
    save(&dir, "engines", &pcm);
    // A police car going by at 35 m/s, 6 m away, its siren wailing then
    // yelping.
    let pcm = record(16.0, |t, m| {
        let x = -280.0 + 35.0 * t;
        let car = Emitter { pos: Vec3::new(x, 0.6, -6.0), vel: Vec3::new(35.0, 0.0, 0.0) };
        let roof = Emitter { pos: car.pos + Vec3::Y * 1.2, ..car };
        m.set_scene(Scene {
            listener: ears,
            engines: vec![EngineState {
                key: 1,
                emitter: car,
                layout: Layout::V8Charged,
                rpm: 4600.0,
                redline: 6800.0,
                load: 0.8,
                limiter: false,
                boost: false,
                own: false,
            }],
            sirens: vec![SirenState {
                key: 1,
                emitter: roof,
                wail: if t < 9.0 { Wail::Wail } else { Wail::Yelp },
                own: false,
            }],
            tires: vec![TireState { key: 1, emitter: car, slip: 0.0, speed: 35.0 }],
            wind: 0.0,
            volume: 1.0,
            ..Scene::default()
        });
    });
    save(&dir, "passby", &pcm);
    // Every cue, a second and a half apart.
    let near = Emitter { pos: Vec3::new(2.0, 0.5, -6.0), vel: Vec3::ZERO };
    let cues = [
        Cue::Move,
        Cue::Select,
        Cue::Back,
        Cue::Count { last: false },
        Cue::Count { last: true },
        Cue::Radio,
        Cue::Crash { emitter: near, strength: 0.25 },
        Cue::Crash { emitter: near, strength: 1.0 },
        Cue::Horn { emitter: near, secs: 0.7 },
        Cue::Boost,
        Cue::Win,
        Cue::Lose,
    ];
    let pcm = record(1.5 * cues.len() as f32 + 1.0, |t, m| {
        let k = (t / 1.5) as usize;
        if (t - k as f32 * 1.5).abs() < 0.005 && k < cues.len() {
            m.cue(cues[k]);
        }
    });
    save(&dir, "cues", &pcm);
    // The wind, a scrape and the rain, rising and falling.
    let pcm = record(8.0, |t, m| {
        let x = (t / 4.0).min(2.0 - t / 4.0).max(0.0);
        m.set_scene(Scene {
            listener: ears,
            wind: 70.0 * x,
            scrape: if (2.0..4.0).contains(&t) { 1.0 } else { 0.0 },
            rain: x,
            volume: 1.0,
            ..Scene::default()
        });
    });
    save(&dir, "world", &pcm);
}
