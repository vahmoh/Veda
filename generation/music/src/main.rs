// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `musicgen` — composes and renders the sample music shipped with Veda.
//!
//! ```text
//! musicgen OUT_DIR [--only N]
//! ```
//!
//! Writes `OUT_DIR/samples/Music/<Title>.qoa` for every track of the album
//! (QOA at 44.1 kHz, with a `VTAG` trailer holding title, artist, album,
//! genre, year, track number, duration, tempo and cover-art parameters),
//! the system's startup sound, `OUT_DIR/sounds/startup.wav`, and the music
//! of the games, `OUT_DIR/games/<game>/`, and lists them in `OUT_DIR`'s
//! manifest (see `gencommon::output`), removing what else is there.
//! `--only N` renders just track N (and leaves the manifest and the other
//! files as they are), and `MUSICGEN_PREVIEW=DIR` also writes 16-bit WAV
//! previews of the tracks to DIR. Rendering is deterministic.

mod dsl;
mod interceptor;
mod patches;
mod songs;

use std::path::{Path, PathBuf};
use std::time::Instant;

use audio::synth;
use audio::tags::Tags;
use gencommon::output::Output;

/// Converts the mix to 16-bit with triangular dither.
fn to_i16(mix: &[f32], seed: u64) -> Vec<i16> {
    let mut s = seed | 1;
    let mut rand = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        (s >> 40) as f32 / (1u64 << 24) as f32
    };
    mix.iter()
        .map(|&v| {
            let d = rand() - rand();
            (v * 32767.0 + d).round().clamp(-32768.0, 32767.0) as i16
        })
        .collect()
}

fn main() {
    let (mut out, mut only) = (None, None);
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--only" => only = Some(args.next().and_then(|n| n.parse::<u32>().ok()).unwrap_or_else(|| usage())),
            a if a.starts_with("--") || out.is_some() => usage(),
            a => out = Some(PathBuf::from(a)),
        }
    }
    let Some(out) = out else { usage() };
    let preview = std::env::var_os("MUSICGEN_PREVIEW").map(PathBuf::from);
    if let Err(e) = run(&out, only, preview.as_deref()) {
        eprintln!("musicgen: {e}");
        std::process::exit(1);
    }
}

fn usage() -> ! {
    eprintln!("usage: musicgen OUT_DIR [--only N]");
    std::process::exit(2);
}

/// Renders every track (or track `only`) into `out`, and previews of them
/// into `preview`.
fn run(out: &Path, only: Option<u32>, preview: Option<&Path>) -> Result<(), String> {
    let mut out = Output::new(out)?;
    let mut tracks = 0;
    let mut total_bytes = 0usize;
    for make in songs::all() {
        let t = make();
        if only.is_some_and(|n| n != t.number) {
            continue;
        }
        let started = Instant::now();
        let (mix, stats) = synth::render(&t.song);
        let pcm = to_i16(&mix, t.number as u64 * 7919);
        let frames = pcm.len() / 2;
        let duration_ms = frames as u64 * 1000 / patches::SR as u64;
        let clipped = pcm.iter().filter(|&&v| v == i16::MAX || v == i16::MIN).count();
        let mut tags = Tags {
            title: t.title.into(),
            artist: songs::ARTIST.into(),
            album: songs::ALBUM.into(),
            genre: t.genre.into(),
            year: songs::YEAR,
            track: t.number,
            duration_ms,
            extra: Vec::new(),
        };
        tags.set("bpm", &t.bpm.to_string());
        tags.set("art", t.art);
        let mut file = audio::qoa::encode(&pcm, 2, patches::SR as u32).map_err(|e| format!("QOA encoding: {e}"))?;
        file.extend(tags.encode_trailer());
        out.write(&format!("samples/Music/{}.qoa", t.title), &file)?;
        total_bytes += file.len();
        println!(
            "{:>2}. {:<16} {:>4}:{:02}  peak {:5.1} dBFS  loudness {:5.1} dB  gain {:+5.1} dB  limiter {:4.1} dB  clipped {}  {:5.2} MB  ({:.1} s)",
            t.number,
            t.title,
            duration_ms / 60_000,
            duration_ms / 1000 % 60,
            stats.peak_db,
            stats.rms_db,
            stats.gain_db,
            stats.limit_db,
            clipped,
            file.len() as f64 / 1e6,
            started.elapsed().as_secs_f32()
        );
        if std::env::var_os("MUSICGEN_PARTS").is_some() {
            for ((name, rms, peak), p) in synth::seq::part_levels(&t.song).iter().zip(&t.song.parts) {
                println!("      {:<8} RMS {:6.1} dB  peak {:6.1} dB  ({} notes)", name, rms, peak, p.notes.len());
            }
        }
        if let Some(dir) = preview {
            std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
            let w = audio::wav::write(&pcm, 2, patches::SR as u32, &tags);
            let path = dir.join(format!("{}.wav", t.title));
            std::fs::write(&path, w).map_err(|e| format!("writing {}: {e}", path.display()))?;
        }
        tracks += 1;
    }
    // A short WAV sample (mono, 22.05 kHz) next to the album.
    if only.is_none() || only == Some(7) {
        let (mix, stats) = synth::render(&songs::chime::song());
        let mono: Vec<f32> = mix.chunks(2).map(|f| (f[0] + f[1]) * 0.5).collect();
        let pcm = to_i16(&mono, 4242);
        let frames = pcm.len();
        let tags = Tags {
            title: "Startup Chime".into(),
            artist: songs::ARTIST.into(),
            album: songs::ALBUM.into(),
            genre: "Ambient".into(),
            year: songs::YEAR,
            track: 7,
            duration_ms: frames as u64 * 1000 / songs::chime::RATE as u64,
            extra: Vec::new(),
        };
        let file = audio::wav::write(&pcm, 1, songs::chime::RATE, &tags);
        out.write("samples/Music/Startup Chime.wav", &file)?;
        total_bytes += file.len();
        println!(
            " 7. {:<16} {:>4}:{:02}  peak {:5.1} dBFS  (mono WAV, {} Hz)  {:5.2} MB",
            "Startup Chime",
            0,
            frames / songs::chime::RATE as usize,
            stats.peak_db,
            songs::chime::RATE,
            file.len() as f64 / 1e6
        );
        tracks += 1;
    }
    // The startup sound (stereo, at the sound devices' rate).
    if only.is_none() {
        let (mix, stats) = synth::render(&songs::startup::song());
        let pcm = to_i16(&mix, 1977);
        let frames = pcm.len() / 2;
        let tags = Tags {
            title: "Startup".into(),
            artist: songs::ARTIST.into(),
            duration_ms: frames as u64 * 1000 / songs::startup::RATE as u64,
            ..Tags::default()
        };
        let file = audio::wav::write(&pcm, 2, songs::startup::RATE, &tags);
        out.write("sounds/startup.wav", &file)?;
        println!(
            "    {:<16} {:>4}.{:01} s  peak {:5.1} dBFS  loudness {:5.1} dB  (WAV, {} Hz)  {:5.2} MB",
            "Startup sound",
            frames / songs::startup::RATE as usize,
            frames * 10 / songs::startup::RATE as usize % 10,
            stats.peak_db,
            stats.rms_db,
            songs::startup::RATE,
            file.len() as f64 / 1e6
        );
    }
    // The games' music; then what was written is listed, and the rest of
    // earlier runs goes.
    if only.is_none() {
        println!("Interceptor:");
        total_bytes += interceptor::write_all(&mut out)?;
        out.finish()?;
        println!("{tracks} tracks, {:.2} MB", total_bytes as f64 / 1e6);
    }
    Ok(())
}
