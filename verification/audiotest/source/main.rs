// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `audiotest` — audio checks that run inside Veda.
//!
//! `audiotest record SECS` records SECS seconds from the microphone
//! (16 kHz mono) and logs the level of every second as `audiotest: level
//! -23.4 dBFS (peak -10.2 dBFS, 16000 samples)`, then `audiotest: heard
//! sound` if any second was louder than -45 dBFS and `audiotest: PASS`
//! (`drivervm-mic.vts` records the driver VM's sound card so).
//!
//! Scripts feed the microphone with `say` and `mic-wav` (the `testmic`
//! device) or use the machine's real microphone. Echo cancellation is
//! checked through the agent, which uses it (`verification/agent/echo.vts`,
//! the machine's own sound coming back into the microphone).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec;

use proto::audio::{InputSpec, InputStream, audio};
use rt::println;
use rt::time::Duration;

rt::entry!(main);

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
    match proto::connect(audio::NAME) {
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
        rt::time::sleep(Duration::from_millis(100));
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
        let end = rt::time::deadline_after(Duration::from_secs(3));
        while filled < second.len() {
            if mic.wait_readable(320, end) == 0 && rt::time::now_ns() >= end {
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

fn main() -> i32 {
    let args = rt::env::args();
    let mode = args.get(1).map(String::as_str).unwrap_or("record");
    let secs = args.get(2).and_then(|s| s.parse::<u32>().ok()).unwrap_or(5).clamp(1, 600);
    match mode {
        "record" => record(secs),
        other => {
            println!("audiotest: unknown mode {}", other);
            2
        }
    }
}
