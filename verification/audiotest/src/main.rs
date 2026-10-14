// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `audiotest` — audio checks that run inside Veda.
//!
//! * `audiotest record SECS`: records SECS seconds from the microphone
//!   (16 kHz mono) and logs the level of every second as
//!   `audiotest: level -23.4 dBFS (peak -10.2 dBFS)`, then
//!   `audiotest: heard sound` if any second was louder than -45 dBFS and
//!   `audiotest: PASS`.
//! * `audiotest echo SECS`: plays a voice-like test signal through the
//!   speakers while recording with echo cancellation, and logs how much
//!   quieter the recording is than the signal it would contain without
//!   cancellation (`audiotest: echo suppressed by 24.0 dB`).
//!
//! Scripts feed the microphone with `say` and `mic-wav` (the `testmic`
//! device) or use the machine's real microphone.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use vproto::audio::{InputSpec, InputStream, OutputStream, StreamSpec, audio};
use vrt::println;
use vrt::time::Duration;

vrt::entry!(main);

const RATE: u32 = 16_000;

/// Level in dBFS of `samples` as (RMS, peak).
fn levels(samples: &[i16]) -> (f32, f32) {
    if samples.is_empty() {
        return (-120.0, -120.0);
    }
    let mut sum = 0f64;
    let mut peak = 0i32;
    for &s in samples {
        sum += (s as f64) * (s as f64);
        peak = peak.max((s as i32).abs());
    }
    let rms = libm::sqrt(sum / samples.len() as f64) / 32768.0;
    (db(rms), db(peak as f64 / 32768.0))
}

fn db(x: f64) -> f32 {
    if x <= 1e-6 { -120.0 } else { (20.0 * libm::log10(x)) as f32 }
}

fn connect() -> Option<audio::Client> {
    match vproto::connect(audio::NAME) {
        Ok(ch) => Some(audio::Client::new(ch)),
        Err(e) => {
            println!("audiotest: FAIL (no audio service: {:?})", e);
            None
        }
    }
}

/// Waits until an input device is attached (the test microphone connects
/// over the network after boot).
fn wait_for_input(client: &audio::Client) -> bool {
    for _ in 0..300 {
        if let Ok(s) = client.status()
            && s.input_device != "none"
        {
            println!("audiotest: input device {} ({} Hz, {} ch)", s.input_device, s.input_rate, s.input_channels);
            return true;
        }
        vrt::time::sleep(Duration::from_millis(100));
    }
    println!("audiotest: FAIL (no input device)");
    false
}

fn record(secs: u32) -> i32 {
    let Some(client) = connect() else { return 1 };
    if !wait_for_input(&client) {
        return 1;
    }
    let mic = match InputStream::open(&client, InputSpec::mono(RATE, 1000, "audiotest")) {
        Ok(m) => m,
        Err(e) => {
            println!("audiotest: FAIL (cannot open the microphone: {:?})", e);
            return 1;
        }
    };
    println!("audiotest: recording {} s", secs);
    let mut heard = false;
    let mut second = vec![0i16; RATE as usize];
    for _ in 0..secs {
        let mut filled = 0;
        let end = vrt::time::deadline_after(Duration::from_secs(3));
        while filled < second.len() {
            if mic.wait_readable(320, end) == 0 && vrt::time::now_ns() >= end {
                break;
            }
            filled += mic.read(&mut second[filled..]);
        }
        let (rms, peak) = levels(&second[..filled]);
        println!("audiotest: level {:.1} dBFS (peak {:.1} dBFS, {} samples)", rms, peak, filled);
        heard |= rms > -45.0;
    }
    if mic.overruns() > 0 {
        println!("audiotest: {} samples lost", mic.overruns());
    }
    if heard {
        println!("audiotest: heard sound");
    }
    println!("audiotest: PASS");
    0
}

/// A voice-like test signal: a pitch sweeping 120-220 Hz with harmonics,
/// in syllable-like bursts.
fn voice_signal(n: usize) -> Vec<i16> {
    let mut out = Vec::with_capacity(n);
    let mut phase = 0f64;
    for i in 0..n {
        let t = i as f64 / RATE as f64;
        let f0 = 170.0 + 50.0 * tri(t * 0.7);
        phase += f0 / RATE as f64;
        let mut s = 0.0;
        for h in 1..8 {
            s += sin_turns(phase * h as f64) / h as f64;
        }
        let env = (0.5 + 0.5 * sin_turns(t * 3.0)).max(0.0);
        out.push((s * env * 6000.0) as i16);
    }
    out
}

fn tri(x: f64) -> f64 {
    let f = x - (x as i64) as f64;
    if f < 0.5 { 4.0 * f - 1.0 } else { 3.0 - 4.0 * f }
}

/// sin(2 pi x).
fn sin_turns(x: f64) -> f64 {
    libm::sin(core::f64::consts::TAU * x)
}

fn echo(secs: u32) -> i32 {
    let Some(client) = connect() else { return 1 };
    if !wait_for_input(&client) {
        return 1;
    }
    let mut spec = InputSpec::mono(RATE, 1000, "audiotest-echo");
    spec.echo_cancel = true;
    let mic = match InputStream::open(&client, spec) {
        Ok(m) => m,
        Err(e) => {
            println!("audiotest: FAIL (cannot open the microphone: {:?})", e);
            return 1;
        }
    };
    let speaker = match OutputStream::open(
        &client,
        StreamSpec {
            rate: RATE,
            channels: 1,
            buffer_frames: RATE / 2,
            notify_frames: 0,
            name: String::from("audiotest-echo"),
            paused: false,
            volume: 1.0,
        },
    ) {
        Ok(s) => s,
        Err(e) => {
            println!("audiotest: FAIL (cannot play: {:?})", e);
            return 1;
        }
    };
    let signal = voice_signal((secs * RATE) as usize);
    let (sig_rms, _) = levels(&signal);
    let mut played = 0;
    let mut recorded: Vec<i16> = Vec::with_capacity(signal.len() + RATE as usize);
    let mut buf = vec![0i16; 1600];
    let end = vrt::time::deadline_after(Duration::from_secs(secs as u64 + 2));
    while vrt::time::now_ns() < end {
        if played < signal.len() {
            played += speaker.write(&signal[played..]);
        }
        let n = mic.read(&mut buf);
        recorded.extend_from_slice(&buf[..n]);
        vrt::time::sleep(Duration::from_millis(10));
    }
    // Skip the first two seconds while the canceller converges.
    let start = (2 * RATE as usize).min(recorded.len());
    let (rec_rms, _) = levels(&recorded[start..]);
    println!("audiotest: signal {:.1} dBFS, recording after cancellation {:.1} dBFS", sig_rms, rec_rms);
    println!("audiotest: echo suppressed by {:.1} dB", sig_rms - rec_rms);
    println!("audiotest: PASS");
    0
}

fn main() -> i32 {
    let args = vrt::env::args();
    let mode = args.get(1).map(String::as_str).unwrap_or("record");
    let secs = args.get(2).and_then(|s| s.parse::<u32>().ok()).unwrap_or(5).clamp(1, 600);
    match mode {
        "record" => record(secs),
        "echo" => echo(secs),
        other => {
            println!("audiotest: unknown mode {}", other);
            2
        }
    }
}
