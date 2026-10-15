// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The echo canceller's tests: scenes of a room (far-end speech, its echo,
//! the user talking, noise) built from `crate::testsig`, and what the
//! canceller must make of them.

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
    let cases: Vec<((f32, f32, f32), u64)> = changes.into_iter().flat_map(|c| seeds(5).map(move |s| (c, s))).collect();
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
    let (w, sh) = (aec.w.coefficients(), aec.sh.coefficients());
    assert!(finite(w.0) && finite(w.1) && finite(sh.0) && finite(sh.1));
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
