// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! A decoding session: the protocol's calls ([`media::Server`]) and the work
//! the loop does between them, one piece at a time, as `mediadec`'s session
//! does them (`implementation/services/mediadec/src/session.rs`, which this
//! follows call for call). Its "decoders" make each picture and each record
//! of sound from the clip's description at once, where `mediadec`'s decode
//! packets.
//!
//! The work, in order of preference: put a decoded picture into a free slot
//! of the pool, write sound into the ring, decode a picture, decode sound.
//! Pictures stop when the pool is full and sound when the ring is: the
//! player frees room as it shows and plays them, and signals `wake`.
//!
//! The clip's faults strike where decoding meets them: a picture or a
//! record of sound holding the time crashes the decoder, or hangs it, and
//! each picture decoded takes `slow_ms`.
//!
//! Where it differs from `mediadec`: a new source does not inherit an exact
//! seek's target, the skip level holds for every source opened after it
//! (FFmpeg's decoders start again from none), the pictures it leaves out
//! count as skipped, and `decode_us` is measured.

use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use vabi::RawHandle;
use vgfx::Canvas;
use vproto::fs::file;
use vproto::media::pool::slot_flags;
use vproto::media::{
    self, AudioSpec, AudioStream, Colour, DecoderInfo, FramePool, Heartbeat, MediaError, MediaInfo, OpenOptions,
    PcmRing, PictureChecksum, Rational, SeekMode, SkipLevel, SlotMeta, Stats, Thumbnail, VideoSpec, VideoStream,
    open_flags, pcm,
};
use vrt::object::{Channel, Event, Vmo};
use vrt::println;
use vrt::time::Duration;

use crate::clip::{self, Clip, Fault, HEAD_BYTES, Picture, Secs};
use crate::{picture, sound};

/// The largest picture unless the player asks for more (as `mediadec`).
const DEFAULT_MAX: (u32, u32) = (4096, 2304);
/// Sound decoded ahead while stopped (µs).
const STOPPED_SOUND_US: i64 = 500_000;
/// Frames of sound in a record.
const RECORD_FRAMES: u64 = 1024;
/// Sound this close to where the last record ended carries on from it (µs).
const JITTER_US: i64 = 20_000;
/// Pictures this far behind the player's clock (µs) are not drawn.
const LATE_US: i64 = 100_000;
/// While playing, less sound than this queued (µs) comes before pictures.
const LOW_SOUND_US: i64 = 200_000;
/// Pictures a step may ask for at most.
const MAX_STEPS: u32 = 64;
/// The largest thumbnail, each way.
const MAX_THUMBNAIL: u32 = 2048;
/// How long one read of the source may take.
const READ_TIMEOUT_NS: u64 = 10_000_000_000;
/// The streams' numbers in a fake video.
const VIDEO_INDEX: u32 = 0;
const AUDIO_INDEX: u32 = 1;
/// The rate of the sound, as a fake video says it has it.
const AUDIO_RATE: u32 = 48_000;

/// The picture decoder: where it is in the clip.
struct VideoTrack {
    /// The next picture it decodes.
    next: u64,
    /// It gave its last picture.
    ended: bool,
}

/// The sound decoder.
struct AudioTrack {
    /// When the next sound it decodes is heard (µs).
    next_us: i64,
    /// It gave its last sound.
    ended: bool,
}

/// An open source: the clip it describes.
struct Opened {
    clip: Clip,
    info: MediaInfo,
    video: Option<VideoTrack>,
    audio: Option<AudioTrack>,
}

/// Where pictures go.
struct VideoOut {
    pool: FramePool,
    ready: Event,
    wake: Event,
    /// A picture being drawn, laid out as a slot (`stride` x `height`), then
    /// copied into one.
    canvas: Vec<u32>,
}

/// Sound decoded and not written yet: one record.
struct Chunk {
    samples: Vec<i16>,
    pts_us: i64,
    /// When it ends (µs).
    end_us: i64,
    flags: u32,
}

/// Where sound goes.
struct AudioOut {
    ring: PcmRing,
    ready: Event,
    wake: Event,
    rate: u32,
    chunk: Option<Chunk>,
    /// Where the sound written so far ends (µs).
    next_us: Option<i64>,
    /// The generation's end record is written.
    ended: bool,
}

/// Sums behind [`Stats`].
#[derive(Default)]
struct Counters {
    stats: Stats,
    decode_ns: u64,
    convert_ns: u64,
    converted: u64,
}

/// The decoder: its source, where its pictures and sound go, and where
/// decoding is.
pub struct Session {
    opened: Option<Opened>,
    video_out: Option<VideoOut>,
    audio_out: Option<AudioOut>,
    generation: u32,
    running: bool,
    /// Pictures it may still decode while stopped.
    allowance: u32,
    skip: SkipLevel,
    /// An exact seek's target, until the pictures reach it.
    exact_video: Option<i64>,
    /// While seeking exactly: the latest picture before the target.
    candidate: Option<Picture>,
    /// Decoded pictures waiting for a free slot.
    pictures: VecDeque<Picture>,
    /// The last picture put in the pool (to draw again at a new size).
    last: Option<(Picture, SlotMeta)>,
    /// No picture (sound) of this generation was published yet.
    /// The pictures ended: the pool's end is set once those waiting are
    /// published (as `mediadec`).
    video_end: bool,
    first_picture: bool,
    first_sound: bool,
    counters: Counters,
    /// Where it counts its rounds, once the player gave it.
    heartbeat: Option<Heartbeat>,
}

impl Session {
    pub fn new() -> Session {
        Session {
            opened: None,
            video_out: None,
            audio_out: None,
            generation: 0,
            running: false,
            allowance: 0,
            skip: SkipLevel::Nothing,
            exact_video: None,
            candidate: None,
            pictures: VecDeque::new(),
            last: None,
            video_end: false,
            first_picture: true,
            first_sound: true,
            counters: Counters::default(),
            heartbeat: None,
        }
    }

    // ---- the work ------------------------------------------------------------

    /// The player is about to run out of sound: it comes before pictures
    /// (as in `mediadec`).
    fn sound_low(&self) -> bool {
        let Some(out) = &self.audio_out else { return false };
        let ended = self.opened.as_ref().and_then(|o| o.audio.as_ref()).is_none_or(|t| t.ended);
        // 4 bytes a frame.
        self.running && !ended && (out.ring.filled() as i64) < LOW_SOUND_US * out.rate as i64 / 250_000
    }

    /// Hidden, no picture is decoded: once the sound (if any) is past them,
    /// the pictures end (`mediadec` reads through to the end of the file).
    fn end_hidden(&mut self) -> bool {
        if self.skip != SkipLevel::Hidden {
            return false;
        }
        let Some(opened) = self.opened.as_mut() else { return false };
        let sound_done = opened.audio.as_ref().is_none_or(|a| a.ended);
        let frames = opened.clip.frames();
        let Some(video) = opened.video.as_mut() else { return false };
        if video.ended || !(sound_done || video.next >= frames) {
            return false;
        }
        video.ended = true;
        self.video_end = true;
        true
    }

    /// More pictures are wanted now.
    fn wants_picture(&self) -> bool {
        self.video_out.is_some()
            && self.skip != SkipLevel::Hidden
            && self.pictures.is_empty()
            && (self.running || self.allowance > 0 || self.first_picture)
    }

    /// More sound is wanted now.
    fn wants_sound(&self) -> bool {
        let Some(out) = &self.audio_out else { return false };
        // Stopped: about half a second (4 bytes a frame).
        out.chunk.is_none()
            && (self.running
                || self.first_sound
                || (out.ring.filled() as i64) < STOPPED_SOUND_US * out.rate as i64 / 250_000)
    }

    /// Puts the next decoded picture into a free slot (or, after the last,
    /// says the pictures ended).
    fn place_picture(&mut self) -> bool {
        let Some(out) = &mut self.video_out else { return false };
        let Some(&picture) = self.pictures.front() else {
            if self.video_end && self.candidate.is_none() {
                self.video_end = false;
                out.pool.set_end(self.generation);
                let _ = out.ready.signal();
                return true;
            }
            return false;
        };
        let Some(opened) = &self.opened else { return false };
        // Too late to be seen: not worth drawing.
        if self.running
            && !self.first_picture
            && let Some((clock_us, at_ns)) = out.pool.player_clock()
        {
            let now = vrt::time::now_ns();
            let shown = clock_us.saturating_add((now.saturating_sub(at_ns) / 1000) as i64);
            if picture.pts_us.saturating_add(picture.duration_us.max(1)) < shown.saturating_sub(LATE_US) {
                self.pictures.pop_front();
                self.counters.stats.late += 1;
                out.pool.note_late();
                return true;
            }
        }
        let Some(slot) = out.pool.claim() else { return false };
        self.pictures.pop_front();
        let began = vrt::time::now_ns();
        draw_into(out, slot, &picture, opened.clip.duration_us);
        self.counters.convert_ns += vrt::time::now_ns().saturating_sub(began);
        self.counters.converted += 1;
        let mut flags = 0;
        if picture.key {
            flags |= slot_flags::KEY;
        }
        if self.first_picture {
            flags |= slot_flags::DISCONTINUITY;
        }
        let meta = SlotMeta {
            generation: self.generation,
            seq: 0,
            pts_us: picture.pts_us,
            duration_us: picture.duration_us,
            width: out.pool.width(),
            height: out.pool.height(),
            flags,
        };
        out.pool.publish(slot, &meta);
        let _ = out.ready.signal();
        self.first_picture = false;
        if !self.running {
            self.allowance = self.allowance.saturating_sub(1);
        }
        self.last = Some((picture, meta));
        true
    }

    /// Writes decoded sound into the ring, or the end of the generation.
    fn place_sound(&mut self) -> bool {
        let generation = self.generation;
        let Some(out) = &mut self.audio_out else { return false };
        if let Some(chunk) = &out.chunk {
            if !out.ring.write(generation, chunk.pts_us, chunk.flags, &chunk.samples) {
                return false;
            }
            out.next_us = Some(chunk.end_us);
            out.chunk = None;
            let _ = out.ready.signal();
            self.first_sound = false;
            return true;
        }
        let ended = self.opened.as_ref().and_then(|o| o.audio.as_ref()).is_some_and(|t| t.ended);
        if ended && !out.ended && out.ring.write_end(generation) {
            out.ended = true;
            let _ = out.ready.signal();
            return true;
        }
        false
    }

    /// Decodes a picture.
    fn receive_picture(&mut self) -> bool {
        if !self.wants_picture() {
            return false;
        }
        let skip = self.skip;
        let Some(opened) = self.opened.as_mut() else { return false };
        let Some(track) = opened.video.as_mut() else { return false };
        if track.ended {
            return false;
        }
        let clip = &opened.clip;
        // What the skip level leaves out is not decoded at all.
        let frames = clip.frames();
        while track.next < frames && !decodes(clip, track.next, skip) {
            track.next += 1;
            self.counters.stats.skipped += 1;
        }
        if track.next >= frames {
            track.ended = true;
            if let Some(c) = self.candidate.take() {
                self.pictures.push_back(c);
            }
            self.exact_video = None;
            self.video_end = true;
            return true;
        }
        let picture = clip.picture(track.next);
        track.next += 1;
        let began = vrt::time::now_ns();
        decode_picture(clip, &picture);
        self.counters.decode_ns += vrt::time::now_ns().saturating_sub(began);
        self.counters.stats.decoded += 1;
        self.take_picture(picture);
        true
    }

    /// A decoded picture: shown, or (seeking exactly) kept until the one
    /// at the target is known.
    fn take_picture(&mut self, picture: Picture) {
        let Some(target) = self.exact_video else {
            self.pictures.push_back(picture);
            return;
        };
        if picture.pts_us <= target {
            // The latest before the target so far; the one before is not
            // shown.
            if self.candidate.replace(picture).is_some() {
                self.counters.stats.skipped += 1;
            }
        } else {
            // Past the target: the picture shown at it is the candidate.
            if let Some(c) = self.candidate.take() {
                self.pictures.push_back(c);
            }
            self.pictures.push_back(picture);
            self.exact_video = None;
        }
    }

    /// Decodes a record of sound.
    fn receive_sound(&mut self) -> bool {
        if !self.wants_sound() {
            return false;
        }
        let hidden = self.skip == SkipLevel::Hidden;
        let Some(opened) = self.opened.as_mut() else { return false };
        let Some(track) = opened.audio.as_mut() else { return false };
        let Some(out) = self.audio_out.as_mut() else { return false };
        if track.ended {
            return false;
        }
        let clip = &opened.clip;
        let rate = out.rate;
        let (first, last) = (clip::sound_frame(track.next_us, rate), clip.sound_frames(rate));
        if first >= last {
            track.ended = true;
            return true;
        }
        let frames = (last - first).min(RECORD_FRAMES);
        let (pts, end_us) = (clip::sound_time(first, rate), clip::sound_time(first + frames, rate));
        decode(clip, pts, end_us);
        let mut samples = Vec::with_capacity(frames as usize * 2);
        sound::fill(&mut samples, first, frames, rate);
        track.next_us = end_us;
        // Hidden, the pictures' packets go by unread (as in `mediadec`):
        // shown again without a seek, they start at the next key frame.
        if hidden
            && let Some(video) = opened.video.as_mut()
            && !video.ended
        {
            video.next = video.next.max(clip.key_after(clip.frame_from(end_us)));
        }
        let follows = out.next_us.is_some_and(|t| (t - pts).abs() <= JITTER_US);
        let flags = if self.first_sound || !follows { pcm::record_flags::DISCONTINUITY } else { 0 };
        out.chunk = Some(Chunk { samples, pts_us: pts, end_us, flags });
        true
    }

    // ---- requests --------------------------------------------------------------

    fn drop_decoded(&mut self) {
        self.pictures.clear();
        self.candidate = None;
        self.video_end = false;
        if let Some(out) = &mut self.audio_out {
            out.chunk = None;
            out.next_us = None;
            out.ended = false;
        }
    }

    /// Where the player is (for a new stream selection): its clock, or the
    /// picture published last.
    fn position(&self) -> i64 {
        if let Some((clock_us, at_ns)) = self.video_out.as_ref().and_then(|o| o.pool.player_clock()) {
            let since = if self.running { vrt::time::now_ns().saturating_sub(at_ns) / 1000 } else { 0 };
            return clock_us.saturating_add(since as i64);
        }
        self.last
            .as_ref()
            .map(|(_, m)| m.pts_us)
            .or_else(|| self.audio_out.as_ref().and_then(|a| a.next_us))
            .unwrap_or(0)
    }

    /// Goes to `position_us` as `generation` (see `media::Client::seek`):
    /// pictures from the key frame at or before it (as FFmpeg seeks), sound
    /// from there too, or from the position itself when exact.
    fn go_to(&mut self, position_us: i64, mode: SeekMode, generation: u32) -> Result<(), MediaError> {
        let opened = self.opened.as_mut().ok_or(MediaError::State)?;
        let target = position_us.max(0);
        let clip = &opened.clip;
        let key = clip.key_before(clip.frame_at(target));
        let sound_us = if mode == SeekMode::Exact { target } else { clip.pts(key) };
        if let Some(video) = opened.video.as_mut() {
            *video = VideoTrack { next: key, ended: false };
        }
        if let Some(audio) = opened.audio.as_mut() {
            *audio = AudioTrack { next_us: sound_us, ended: false };
        }
        self.drop_decoded();
        self.start_generation(generation);
        self.exact_video = (mode == SeekMode::Exact).then_some(target);
        Ok(())
    }

    /// Closes the source and forgets what was decoded from it.
    fn close_source(&mut self) {
        self.drop_decoded();
        self.last = None;
        self.opened = None;
        self.running = false;
        self.allowance = 0;
        self.exact_video = None;
    }

    fn start_generation(&mut self, generation: u32) {
        self.generation = generation;
        self.first_picture = true;
        self.first_sound = true;
        if let Some(out) = &self.video_out {
            out.pool.set_generation(generation);
            out.pool.withdraw_other_generations(generation);
        }
    }
}

impl media::Work for Session {
    /// Once round the loop.
    fn beat(&self) {
        if let Some(h) = &self.heartbeat {
            h.beat();
        }
    }

    /// Does one piece of work; false if there is nothing to do until the
    /// player frees room or asks something.
    fn work(&mut self) -> bool {
        if self.opened.is_none() {
            return false;
        }
        self.end_hidden()
            || (self.sound_low() && (self.place_sound() || self.receive_sound()))
            || self.place_picture()
            || self.place_sound()
            || self.receive_picture()
            || self.receive_sound()
    }

    /// The events the loop waits on besides the control channel.
    fn wakes(&self) -> Vec<RawHandle> {
        let mut v = Vec::new();
        if let Some(o) = &self.video_out {
            v.push(o.wake.raw());
        }
        if let Some(o) = &self.audio_out
            && self.video_out.as_ref().is_none_or(|v| v.wake.raw() != o.wake.raw())
        {
            v.push(o.wake.raw());
        }
        v
    }

    /// Clears the wake events (after waiting on them).
    fn clear_wakes(&self) {
        if let Some(o) = &self.video_out {
            let _ = o.wake.clear();
        }
        if let Some(o) = &self.audio_out {
            let _ = o.wake.clear();
        }
    }
}

/// Whether picture `n` is decoded at `skip` (as FFmpeg's `skip_frame`
/// leaves pictures out).
fn decodes(clip: &Clip, n: u64, skip: SkipLevel) -> bool {
    match skip {
        SkipLevel::NonReference => clip.is_reference(n),
        SkipLevel::KeyframesOnly => clip.is_key(n),
        _ => true,
    }
}

/// Decodes what is seen or heard from `from_us` until `to_us`: where the
/// clip asks for a fault there, the decoder crashes, or hangs for good.
fn decode(clip: &Clip, from_us: i64, to_us: i64) {
    match clip.fault(from_us, to_us) {
        Some((Fault::Crash, at)) => {
            println!("crashing at {}, as the clip asks", Secs(at));
            vrt::sys::process_exit(vabi::EXIT_CODE_CRASHED)
        }
        Some((Fault::Hang, at)) => {
            println!("hanging at {}, as the clip asks", Secs(at));
            loop {
                vrt::time::sleep(Duration::from_secs(3600));
            }
        }
        None => {}
    }
}

/// Decodes `picture`, which takes the clip's `slow_ms`.
fn decode_picture(clip: &Clip, picture: &Picture) {
    decode(clip, picture.pts_us, picture.end_us());
    if clip.slow_ms > 0 {
        vrt::time::sleep(Duration::from_millis(clip.slow_ms as u64));
    }
}

/// Draws `picture` of a clip of `duration_us` into `slot` of the pool.
fn draw_into(out: &mut VideoOut, slot: u32, picture: &Picture, duration_us: i64) {
    let (w, h, stride) = (out.pool.width() as i32, out.pool.height() as i32, out.pool.stride() as i32);
    picture::draw(&mut Canvas::new(&mut out.canvas, w, h, stride), picture, duration_us);
    // SAFETY: the slot holds `height` rows of `stride` pixels, as many as
    // the canvas (the pool's layout was checked when it was mapped); it is
    // claimed, and only ever written through the pointer.
    unsafe { core::ptr::copy_nonoverlapping(out.canvas.as_ptr(), out.pool.slot_pixels(slot), out.canvas.len()) };
}

/// `len` pixels of the background, or `None` without the memory.
fn blank(len: usize) -> Option<Vec<u32>> {
    let mut v = Vec::new();
    v.try_reserve_exact(len).ok()?;
    v.resize(len, picture::BACKGROUND.0);
    Some(v)
}

/// The first line of `source` (a `file` channel), or as much as there is:
/// at most [`HEAD_BYTES`].
fn read_head(source: Channel) -> Result<Vec<u8>, MediaError> {
    let file = file::Client::new(source);
    file.set_timeout(READ_TIMEOUT_NS);
    let mut head = Vec::new();
    while head.len() < HEAD_BYTES && !head.contains(&b'\n') {
        let want = HEAD_BYTES - head.len();
        let bytes =
            file.read_at(head.len() as u64, want as u32).map_err(|_| MediaError::Io)?.map_err(|_| MediaError::Io)?;
        if bytes.0.is_empty() {
            break;
        }
        head.extend_from_slice(&bytes.0[..bytes.0.len().min(want)]);
    }
    Ok(head)
}

/// What a fake video holds, as the protocol says it.
fn media_info(clip: &Clip, thumbnails: bool) -> MediaInfo {
    let video = VideoStream {
        index: VIDEO_INDEX,
        codec: "fake".into(),
        width: clip.width,
        height: clip.height,
        sample_aspect: Rational::new(1, 1),
        rotation: clip.rotation,
        mirrored: false,
        frame_rate: Rational::new(clip.fps_num, clip.fps_den),
        bit_depth: 8,
        // BT.709, limited range.
        colour: Colour { primaries: 1, transfer: 1, matrix: 1, full_range: false },
        hdr: false,
        decodable: true,
        default: true,
        language: String::new(),
        title: String::new(),
    };
    let audio = clip.audio.then(|| AudioStream {
        index: AUDIO_INDEX,
        codec: "pcm_s16le".into(),
        rate: AUDIO_RATE,
        channels: 2,
        decodable: true,
        default: true,
        language: String::new(),
        title: String::new(),
    });
    MediaInfo {
        container: "fake".into(),
        duration_us: Some(clip.duration_us),
        seekable: true,
        bit_rate: 0,
        title: clip.title.clone(),
        artist: String::new(),
        video: vec![video],
        audio: audio.into_iter().collect(),
        subtitles: 0,
        chapters: Vec::new(),
        selected_video: Some(VIDEO_INDEX),
        selected_audio: (clip.audio && !thumbnails).then_some(AUDIO_INDEX),
    }
}

/// One line saying what was opened.
fn log_opened(info: &MediaInfo, clip: &Clip) {
    let video = info.video_stream().map(|v| {
        let fps = v.frame_rate.num as u64 * 100 / v.frame_rate.den.max(1) as u64;
        format!("{} {}x{} {}.{:02} fps", v.codec, v.width, v.height, fps / 100, fps % 100)
    });
    let audio = info.audio_stream().map(|a| format!("{} {} Hz {} ch", a.codec, a.rate, a.channels));
    let s = clip.duration_us / 1_000_000;
    let mut faults = String::new();
    if let Some(t) = clip.crash_us {
        faults += &format!(", crashes at {}", Secs(t));
    }
    if let Some(t) = clip.hang_us {
        faults += &format!(", hangs at {}", Secs(t));
    }
    if clip.slow_ms > 0 {
        faults += &format!(", {} ms a picture", clip.slow_ms);
    }
    println!(
        "opened {}: {}, {}, {}:{:02}{}",
        info.container,
        video.as_deref().unwrap_or("no picture"),
        audio.as_deref().unwrap_or("no sound"),
        s / 60,
        s % 60,
        faults
    );
}

impl media::media::Server for Session {
    fn hello(&mut self, _version: u32) -> DecoderInfo {
        DecoderInfo { version: media::VERSION, build: "fakedec".into(), threads: 1 }
    }

    fn open(&mut self, source: Channel, options: OpenOptions) -> Result<MediaInfo, MediaError> {
        self.close_source();
        let clip = Clip::parse(&read_head(source)?).map_err(|e| {
            println!("not a fake video: {e}");
            MediaError::Corrupt
        })?;
        // More pixels than the player takes are refused (FFmpeg's
        // `max_pixels`, as `mediadec` sets it).
        let max_w = if options.max_width == 0 { DEFAULT_MAX.0 } else { options.max_width.min(media::MAX_SIDE) };
        let max_h = if options.max_height == 0 { DEFAULT_MAX.1 } else { options.max_height.min(media::MAX_SIDE) };
        if clip.width as u64 * clip.height as u64 > max_w as u64 * max_h as u64 {
            return Err(MediaError::TooLarge);
        }
        let info = media_info(&clip, options.flags & open_flags::THUMBNAILS != 0);
        if !info.is_valid() {
            return Err(MediaError::Internal);
        }
        log_opened(&info, &clip);
        let video = Some(VideoTrack { next: 0, ended: false });
        let audio = info.selected_audio.map(|_| AudioTrack { next_us: 0, ended: false });
        self.opened = Some(Opened { clip, info: info.clone(), video, audio });
        self.start_generation(self.generation);
        self.drop_decoded();
        Ok(info)
    }

    fn select(&mut self, video: Option<u32>, audio: Option<u32>, generation: u32) -> Result<(), MediaError> {
        let position = self.position();
        let opened = self.opened.as_mut().ok_or(MediaError::State)?;
        let known_v = video.is_none_or(|v| opened.info.video.iter().any(|s| s.index == v && s.decodable));
        let known_a = audio.is_none_or(|a| opened.info.audio.iter().any(|s| s.index == a && s.decodable));
        if !known_v || !known_a {
            return Err(MediaError::Invalid);
        }
        opened.video = video.map(|_| VideoTrack { next: 0, ended: false });
        opened.audio = audio.map(|_| AudioTrack { next_us: 0, ended: false });
        opened.info.selected_video = video;
        opened.info.selected_audio = audio;
        self.go_to(position, SeekMode::Exact, generation)
    }

    fn set_video(
        &mut self,
        spec: VideoSpec,
        state: Vmo,
        pixels: Vmo,
        ready: Event,
        wake: Event,
    ) -> Result<(), MediaError> {
        let pool = FramePool::map(state, pixels).map_err(|_| MediaError::Invalid)?;
        if pool.width() != spec.width || pool.height() != spec.height {
            return Err(MediaError::Invalid);
        }
        let canvas = blank(pool.stride() as usize * pool.height() as usize).ok_or(MediaError::NoMemory)?;
        pool.set_generation(self.generation);
        let mut out = VideoOut { pool, ready, wake, canvas };
        // The last picture again, at the new size.
        if let Some((picture, meta)) = &self.last
            && let Some(opened) = &self.opened
            && let Some(slot) = out.pool.claim()
        {
            draw_into(&mut out, slot, picture, opened.clip.duration_us);
            let meta = SlotMeta {
                width: out.pool.width(),
                height: out.pool.height(),
                flags: meta.flags | slot_flags::REPEAT,
                ..*meta
            };
            out.pool.publish(slot, &meta);
            let _ = out.ready.signal();
        }
        self.video_out = Some(out);
        Ok(())
    }

    fn set_audio(&mut self, spec: AudioSpec, ring: Vmo, ready: Event, wake: Event) -> Result<(), MediaError> {
        let ring = PcmRing::map(ring).map_err(|_| MediaError::Invalid)?;
        if ring.rate() != spec.rate {
            return Err(MediaError::Invalid);
        }
        self.audio_out =
            Some(AudioOut { ring, ready, wake, rate: spec.rate, chunk: None, next_us: None, ended: false });
        self.first_sound = true;
        Ok(())
    }

    fn play(&mut self) -> Result<(), MediaError> {
        self.opened.as_ref().ok_or(MediaError::State)?;
        self.running = true;
        Ok(())
    }

    fn stop(&mut self) -> Result<(), MediaError> {
        self.running = false;
        self.allowance = 0;
        Ok(())
    }

    fn seek(&mut self, position_us: i64, mode: SeekMode, generation: u32) -> Result<(), MediaError> {
        self.go_to(position_us, mode, generation)
    }

    fn step(&mut self, pictures: u32) -> Result<(), MediaError> {
        self.opened.as_ref().ok_or(MediaError::State)?;
        self.allowance = self.allowance.saturating_add(pictures).min(MAX_STEPS);
        Ok(())
    }

    fn set_skip(&mut self, level: SkipLevel) -> Result<(), MediaError> {
        self.skip = level;
        if level == SkipLevel::Hidden {
            self.pictures.clear();
            self.candidate = None;
        }
        Ok(())
    }

    fn thumbnail(&mut self, at_us: i64, max_width: u32, max_height: u32) -> Result<Thumbnail, MediaError> {
        let opened = self.opened.as_mut().ok_or(MediaError::State)?;
        let stream = opened.info.video_stream().ok_or(MediaError::Unsupported)?;
        let (dw, dh) = stream.display_size();
        let (w, h) = media::fit(dw, dh, max_width.clamp(1, MAX_THUMBNAIL), max_height.clamp(1, MAX_THUMBNAIL));
        let clip = &opened.clip;
        // The picture shown at `at_us`, decoded from the key frame before it
        // (as `mediadec` decodes it, at most 90 pictures on).
        let key = clip.key_before(clip.frame_at(at_us.max(0)));
        let picture = clip.picture(clip.frame_at(at_us.max(0)).min(key + 89));
        decode_picture(clip, &picture);
        // As `mediadec`'s, which seeks its source to the picture: decoding
        // carries on from there, in the same generation.
        if let Some(video) = opened.video.as_mut() {
            *video = VideoTrack { next: picture.index + 1, ended: false };
        }
        if let Some(audio) = opened.audio.as_mut().filter(|a| !a.ended) {
            audio.next_us = picture.pts_us;
        }
        let len = w as usize * h as usize;
        let mut px = blank(len).ok_or(MediaError::NoMemory)?;
        picture::draw(&mut Canvas::new(&mut px, w as i32, h as i32, w as i32), &picture, clip.duration_us);
        let vmo = Vmo::create((len * 4).next_multiple_of(4096)).map_err(|_| MediaError::NoMemory)?;
        // SAFETY: the `len` pixels are `len * 4` initialised bytes, any of
        // which is a valid `u8`.
        let bytes = unsafe { core::slice::from_raw_parts(px.as_ptr() as *const u8, len * 4) };
        vmo.write(0, bytes).map_err(|_| MediaError::NoMemory)?;
        Ok(Thumbnail { pixels: vmo, width: w, height: h, stride: w, pts_us: picture.pts_us })
    }

    fn checksums(&mut self, _max_pictures: u32) -> Result<Vec<PictureChecksum>, MediaError> {
        // Drawn pictures have no planes to fingerprint.
        self.opened.as_ref().ok_or(MediaError::State)?;
        Err(MediaError::Unsupported)
    }

    fn stats(&mut self) -> Stats {
        let c = &self.counters;
        Stats {
            decode_us: (c.decode_ns / c.stats.decoded.max(1) / 1000) as u32,
            convert_us: (c.convert_ns / c.converted.max(1) / 1000) as u32,
            ..c.stats
        }
    }

    fn close(&mut self) {
        self.close_source();
    }

    fn set_heartbeat(&mut self, heartbeat: Vmo) -> Result<(), MediaError> {
        self.heartbeat = Some(Heartbeat::map(heartbeat).map_err(|_| MediaError::Invalid)?);
        Ok(())
    }
}
