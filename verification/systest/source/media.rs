// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Tests of the media decoder (`/system/bin/mediadec`, FFmpeg and dav1d)
//! and the libraries it is built from, when the image has them (the cross
//! toolchain built them):
//!
//! * both libraries' checkasm, which checks every assembly function against
//!   its C version on the processor's state as Veda keeps it;
//! * the decoder started as Videos starts it, with nothing but its control
//!   channel: it answers, refuses what is not a video without failing, and
//!   ends when the channel closes;
//! * with the sample videos: every picture of the countdown decodes to the
//!   encoder's own reconstruction (AV1 is exact), the pictures and sound
//!   arrive in order through the pool and the ring, and an exact seek lands
//!   on the picture shown at the target.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use abi::{Error, map_flags};
use proto::fs::{open_flags, vfs};
use proto::media::{
    self, AudioSpec, FramePool, MediaError, OpenOptions, PcmRing, SeekMode, VideoSpec, media::Client, pcm, pool,
};
use rt::object::{Channel, Event, Process};
use rt::process::Spawn;
use rt::time::{Duration, deadline_after};
use rt::vm;

use crate::posix::run_c_for;
use crate::{TestResult, check, vfs_client};

const DECODER: &str = "/system/bin/mediadec";
const COUNTDOWN: &str = "/system/samples/Videos/Countdown.mkv";
const COUNTDOWN_MD5: &str = "/system/tests/videos/Countdown.md5";

fn s<E: core::fmt::Display>(e: E) -> String {
    e.to_string()
}

fn exists(fs: &vfs::Client, path: &str) -> bool {
    fs.stat(path.into()).is_ok_and(|r| r.is_ok())
}

/// FFmpeg's and dav1d's checkasm, if the image has them.
pub fn test_checkasm() -> TestResult {
    let fs = vfs_client()?;
    let mut ran = 0;
    for name in ["checkasm-dav1d", "checkasm-ffmpeg"] {
        let path = format!("/system/tests/media/{name}");
        if !exists(&fs, &path) {
            continue;
        }
        let (code, out) = run_c_for(&path, &[], "/tmp", 600)?;
        let summary = out.lines().rev().find(|l| l.contains("tests passed") || l.contains("FAIL"));
        // Its last line otherwise ("no tests to perform", "reference
        // functions unavailable"): nothing was checked.
        let said = summary.or_else(|| out.lines().rev().find(|l| !l.trim().is_empty())).unwrap_or("(nothing)");
        rt::println!("{name}: {}", said.trim());
        if code != 0 {
            for line in out.lines().filter(|l| l.contains("FAIL")).take(20) {
                rt::println!("{name}: {line}");
            }
        }
        check(code == 0, &format!("{name} exited with {code}"))?;
        check(summary.is_some_and(|l| l.contains("tests passed")), &format!("{name} checked nothing"))?;
        ran += 1;
    }
    if ran == 0 {
        rt::println!("checkasm: not in this image (no media libraries were built)");
    }
    Ok(())
}

/// A decoder started as Videos starts one: with its control channel only.
struct Decoder {
    client: Client,
    process: Process,
}

fn start_decoder(fs: &vfs::Client) -> Result<Decoder, String> {
    let (vmo, size) = fs.read_file(DECODER.into()).map_err(s)?.map_err(s)?;
    let image = vm::Mapping::new(vmo, size as usize, map_flags::READ).map_err(s)?;
    let (ours, theirs) = Channel::create().map_err(s)?;
    // SAFETY: a read-only mapping of the VFS's copy of the program.
    let process = Spawn::new("mediadec")
        .path(DECODER)
        .handle(media::CONTROL_ROLE, theirs.into_handle())
        .start(unsafe { image.as_slice() })
        .map_err(|e| format!("starting {DECODER}: {e}"))?;
    let client = Client::new(ours);
    client.set_timeout(30_000_000_000);
    Ok(Decoder { client, process })
}

fn open(fs: &vfs::Client, d: &Decoder, path: &str, flags: u32) -> Result<Result<media::MediaInfo, MediaError>, String> {
    let (file, _) = fs.open_file(path.into(), open_flags::READ).map_err(s)?.map_err(|e| format!("{path}: {e}"))?;
    d.client.open(file, OpenOptions { threads: 0, max_width: 0, max_height: 0, flags }).map_err(|e| format!("{e:?}"))
}

/// The decoder: sandboxed, it answers, refuses what is not a video and
/// ends with its channel.
pub fn test_decoder() -> TestResult {
    let fs = vfs_client()?;
    if !exists(&fs, DECODER) {
        rt::println!("media decoder: not in this image (no media libraries were built)");
        return Ok(());
    }
    let d = start_decoder(&fs)?;
    let hello = d.client.hello(media::VERSION).map_err(|e| format!("hello: {e:?}"))?;
    rt::println!("media decoder: {} (protocol {})", hello.build, hello.version);
    check(hello.version == media::VERSION && hello.build.starts_with("FFmpeg"), "the decoder says what it is")?;

    // What is not a video is refused, and the decoder carries on.
    let font = open(&fs, &d, "/system/fonts/Inter-Regular.otf", 0)?;
    check(font.is_err(), &format!("a font opened as a video: {font:?}"))?;
    let garbage: Vec<u8> = (0..65_536u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect();
    let vmo = rt::object::Vmo::create(garbage.len()).map_err(s)?;
    vmo.write(0, &garbage).map_err(s)?;
    fs.write_file("/tmp/garbage.mkv".into(), vmo, garbage.len() as u64).map_err(s)?.map_err(s)?;
    let bad = open(&fs, &d, "/tmp/garbage.mkv", 0)?;
    check(bad.is_err(), &format!("garbage opened as a video: {bad:?}"))?;
    let _ = fs.remove("/tmp/garbage.mkv".into());
    check(d.client.hello(media::VERSION).is_ok(), "the decoder answers after refusing")?;

    if exists(&fs, COUNTDOWN) {
        decode_countdown(&fs, &d)?;
    } else {
        rt::println!("media decoder: no sample videos in this image");
    }

    // The player goes away: the decoder ends.
    let Decoder { client, process } = d;
    drop(client);
    let code =
        process.join(deadline_after(Duration::from_secs(5))).map_err(|e| format!("the decoder lives on: {e}"))?;
    check(code == 0, &format!("the decoder exited with {code}"))
}

/// The countdown sample, decoded every way.
fn decode_countdown(fs: &vfs::Client, d: &Decoder) -> TestResult {
    let info = open(fs, d, COUNTDOWN, 0)?.map_err(|e| format!("opening the countdown: {e}"))?;
    check(info.is_valid(), "its description keeps to the protocol")?;
    let v = info.video_stream().ok_or("the countdown has no picture")?.clone();
    let a = info.audio_stream().ok_or("the countdown has no sound")?.clone();
    rt::println!(
        "countdown: {} {}x{}, {} {} Hz, {} us",
        v.codec,
        v.width,
        v.height,
        a.codec,
        a.rate,
        info.duration_us.unwrap_or(-1)
    );
    check(v.codec == "av1" && a.codec == "flac", "the countdown is AV1 and FLAC")?;

    // Every picture as the encoder made it.
    if exists(fs, COUNTDOWN_MD5) {
        let (vmo, size) = fs.read_file(COUNTDOWN_MD5.into()).map_err(s)?.map_err(s)?;
        let mut text = alloc::vec![0u8; size as usize];
        vmo.read(0, &mut text).map_err(s)?;
        let expected: Vec<String> = String::from_utf8_lossy(&text).lines().map(|l| l.trim().to_string()).collect();
        let sums = d.client.checksums(media::MAX_CHECKSUMS).map_err(|e| format!("{e:?}"))?.map_err(s)?;
        let got: Vec<String> = sums.iter().map(|c| c.md5.iter().map(|b| format!("{b:02x}")).collect()).collect();
        check(got.len() == expected.len(), &format!("{} pictures decoded, {} expected", got.len(), expected.len()))?;
        if let Some(i) = got.iter().zip(&expected).position(|(g, e)| g != e) {
            return Err(format!("picture {i} decodes to {}, the encoder made {}", got[i], expected[i]));
        }
        rt::println!("countdown: {} pictures decode exactly", got.len());
        // Back to the start for what follows.
        open(fs, d, COUNTDOWN, 0)?.map_err(s)?;
    }

    // Through the pool and the ring, at a quarter of the size.
    let (w, h) = media::fit(v.width, v.height, v.width / 4, v.height / 4);
    let (frames, state, pixels) = FramePool::create(4, w, h).map_err(|e| format!("{e:?}"))?;
    let (ring, ring_vmo) = PcmRing::create(256 << 10, 48_000).map_err(|e| format!("{e:?}"))?;
    let (ready, wake) = (Event::create().map_err(s)?, Event::create().map_err(s)?);
    let dup = |e: &Event| e.0.duplicate(None).map(Event::from_handle).map_err(s);
    d.client
        .set_video(VideoSpec { width: w, height: h, flags: 0 }, state, pixels, dup(&ready)?, dup(&wake)?)
        .map_err(|e| format!("{e:?}"))?
        .map_err(s)?;
    d.client
        .set_audio(AudioSpec { rate: 48_000 }, ring_vmo, dup(&ready)?, dup(&wake)?)
        .map_err(|e| format!("{e:?}"))?
        .map_err(s)?;
    d.client.seek(0, SeekMode::Exact, 1).map_err(|e| format!("{e:?}"))?.map_err(s)?;
    d.client.play().map_err(|e| format!("{e:?}"))?.map_err(s)?;
    let collected = collect(&frames, &ring, &ready, &wake, 1, usize::MAX)?;
    rt::println!(
        "countdown: {} pictures and {} frames of sound in order",
        collected.pictures.len(),
        collected.sound_frames
    );
    check(collected.pictures.windows(2).all(|p| p[0] < p[1]), "pictures in order")?;
    check(collected.pictures.len() >= 300, "every picture of the countdown")?;
    // 12 s at 48 kHz, give or take a frame of the codec.
    check(
        (570_000..=578_000).contains(&collected.sound_frames),
        &format!("{} frames of sound", collected.sound_frames),
    )?;
    check(collected.opaque, "pictures are opaque")?;

    // An exact seek: the picture shown at 5.5 s, then sound from it on.
    d.client.seek(5_500_000, SeekMode::Exact, 2).map_err(|e| format!("{e:?}"))?.map_err(s)?;
    let after = collect(&frames, &ring, &ready, &wake, 2, 1)?;
    let first = *after.pictures.first().ok_or("no picture after the seek")?;
    check((5_466_000..=5_500_000).contains(&first), &format!("the picture after seeking to 5.5 s is at {first} us"))?;
    check(after.first_sound.is_some_and(|t| (5_500_000..5_510_000).contains(&t)), "sound from 5.5 s on")?;
    d.client.close().map_err(|e| format!("{e:?}"))
}

struct Collected {
    pictures: Vec<i64>,
    sound_frames: u64,
    first_sound: Option<i64>,
    opaque: bool,
}

/// Takes the pictures and sound of `generation` as a player does, until
/// both ended (or `max_pictures` and some sound arrived).
fn collect(
    frames: &FramePool,
    ring: &PcmRing,
    ready: &Event,
    wake: &Event,
    generation: u32,
    max_pictures: usize,
) -> Result<Collected, String> {
    let deadline = deadline_after(Duration::from_secs(60));
    let mut c = Collected { pictures: Vec::new(), sound_frames: 0, first_sound: None, opaque: true };
    let (mut video_end, mut sound_end) = (false, false);
    let mut buf = alloc::vec![0i16; 2 * pcm::MAX_FRAMES as usize];
    while !(video_end && sound_end) && (c.pictures.len() < max_pictures || (c.first_sound.is_none() && !sound_end)) {
        let mut progress = false;
        for slot in 0..frames.slots() {
            let Some(meta) = frames.snapshot(slot) else { continue };
            if frames.state(slot) != pool::state::READY {
                continue;
            }
            if meta.generation == generation {
                c.pictures.push(meta.pts_us);
                // SAFETY: a published slot of the pool's size.
                let corner = unsafe { frames.slot_pixels(slot).read() };
                c.opaque &= corner >> 24 == 0xFF;
            }
            frames.release(slot);
            progress = true;
        }
        if !video_end
            && frames.end() == Some(generation)
            && (0..frames.slots()).all(|s| frames.state(s) != pool::state::READY)
        {
            // Not to be waited for again: the decoder says no more.
            video_end = true;
            progress = true;
        }
        while let Some(r) = ring.peek().map_err(|e| format!("the ring: {e:?}"))? {
            progress = true;
            if r.kind == pcm::kind::END {
                sound_end |= r.generation == generation;
                ring.take(&r, 0);
                continue;
            }
            if r.generation == generation {
                c.first_sound.get_or_insert(r.next_pts(48_000));
                c.sound_frames += ring.copy(&r, &mut buf) as u64;
            }
            ring.take(&r, r.frames);
        }
        if progress {
            let _ = wake.signal();
            continue;
        }
        if ready.wait(abi::signals::SIGNALED, deadline) == Err(Error::TimedOut) {
            return Err(format!(
                "stalled after {} pictures (end {}: pool end {:?}, {} produced) and {} frames of sound (end {}: {} \
                 bytes in the ring)",
                c.pictures.len(),
                if video_end { "seen" } else { "not seen" },
                frames.end(),
                frames.produced(),
                c.sound_frames,
                if sound_end { "seen" } else { "not seen" },
                ring.filled()
            ));
        }
        let _ = ready.clear();
    }
    Ok(c)
}
