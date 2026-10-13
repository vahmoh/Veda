// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! A decoding session: the protocol's calls ([`media::Server`]) and the work
//! the decoder's loop does between them, one shim call at a time, so that a
//! request waits at most for one call into FFmpeg.
//!
//! The work, in order of preference: put a decoded picture into a free slot
//! of the pool, write converted sound into the ring, take a frame from a
//! decoder, give a decoder a packet, read a packet. Pictures stop when the
//! pool is full and sound when the ring is: the player frees room as it
//! shows and plays them, and signals `wake`.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::ffi::c_void;
use core::ptr;

use vabi::RawHandle;
use vproto::media::pool::slot_flags;
use vproto::media::{
    self, AudioSpec, DecoderInfo, FramePool, MediaError, MediaInfo, OpenOptions, PcmRing, PictureChecksum, SeekMode,
    SkipLevel, SlotMeta, Stats, Thumbnail, VideoSpec, open_flags, pcm, video_flags,
};
use vrt::object::{Channel, Event, Vmo};
use vrt::println;
use vrt::vm::Mapping;

use crate::ffi::{self, AGAIN, END, NO_TIME, OK};
use crate::info;
use crate::picture::{Orientation, turn};
use crate::schedule::{self, PacketQueue};
use crate::sound::{self, Follow};
use crate::source::{self, Source};

/// The most bytes FFmpeg allocates at once.
pub const MAX_ALLOC: u64 = 512 << 20;
/// The largest picture unless the player asks for more.
const DEFAULT_MAX: (u32, u32) = (4096, 2304);
/// Sound decoded ahead while stopped (µs).
const STOPPED_SOUND_US: i64 = 500_000;
/// While playing, less sound than this queued for the player (µs) comes
/// before pictures: a gap in the sound is worse than a picture dropped.
const LOW_SOUND_US: i64 = 200_000;
/// Room for the sound of one frame, converted (frames).
const SOUND_FRAMES: usize = 1 << 16;
/// Pictures this far behind the player's clock (µs) are not converted.
const LATE_US: i64 = 100_000;
/// Errors in a row after which a stream counts as undecodable.
const MAX_ERRORS: u32 = 200;
/// Pictures a step may ask for at most.
const MAX_STEPS: u32 = 64;
/// The largest thumbnail, each way.
const MAX_THUMBNAIL: u32 = 2048;
/// Pictures a thumbnail decodes at most from the key frame before its time.
const MAX_THUMBNAIL_PICTURES: u32 = 90;
/// Packets a thumbnail or the checksums read at most without a picture.
const MAX_SEARCH_PACKETS: u32 = 20_000;

/// A frame the shim decoded, freed when dropped.
struct Frame {
    ptr: *mut ffi::Frame,
    info: ffi::FrameInfo,
}

impl Drop for Frame {
    fn drop(&mut self) {
        // SAFETY: the shim made it; nothing else holds it.
        unsafe { ffi::vmd_frame_free(self.ptr) }
    }
}

/// A packet read, freed when dropped.
struct Packet(*mut ffi::Packet);

impl Drop for Packet {
    fn drop(&mut self) {
        // SAFETY: as for `Frame`.
        unsafe { ffi::vmd_packet_free(self.0) }
    }
}

/// A packet waiting for its decoder, and whether a key frame starts with it.
struct Queued {
    packet: Packet,
    key: bool,
}

/// A stream being decoded.
struct Track {
    index: u32,
    decoder: *mut ffi::Decoder,
    queue: PacketQueue<Queued>,
    /// The decoder wants a packet before it gives another frame.
    hungry: bool,
    /// The end of the source was sent to it.
    draining: bool,
    /// It gave its last frame.
    ended: bool,
    /// Errors in a row.
    errors: u32,
    /// What the decoder leaves out (`SkipLevel` up to key frames only).
    level: u32,
    /// It takes a new level only when opened: opened again at the next key
    /// frame (`reopen`), after giving the pictures it holds (`reopening`).
    reopen: bool,
    reopening: bool,
}

impl Track {
    fn new(index: u32, decoder: *mut ffi::Decoder, limit: usize, level: u32) -> Track {
        Track {
            index,
            decoder,
            queue: PacketQueue::new(limit),
            hungry: true,
            draining: false,
            ended: false,
            errors: 0,
            level,
            reopen: false,
            reopening: false,
        }
    }

    /// Back to the state after opening (a seek).
    fn reset(&mut self) {
        self.queue.clear(drop);
        // SAFETY: the decoder is ours.
        unsafe { ffi::vmd_flush(self.decoder) };
        self.hungry = true;
        self.draining = false;
        self.ended = false;
        self.errors = 0;
        // The next packet starts a key frame: opened again at once.
        self.reopen |= self.reopening;
        self.reopening = false;
    }

    /// A new decoder in place of this one's, leaving out `level`.
    fn open_again(&mut self, src: *mut ffi::Source, threads: u32, level: u32) -> Result<(), MediaError> {
        let dec = open_decoder(src, self.index, threads, ffi::decode_skip(level))?;
        // SAFETY: the old decoder is ours, and nothing else holds it.
        unsafe { ffi::vmd_decoder_close(self.decoder) };
        self.decoder = dec;
        self.level = level;
        self.reopen = false;
        self.reopening = false;
        self.draining = false;
        self.ended = false;
        self.hungry = true;
        Ok(())
    }
}

impl Drop for Track {
    fn drop(&mut self) {
        self.queue.clear(drop);
        // SAFETY: the decoder is ours.
        unsafe { ffi::vmd_decoder_close(self.decoder) }
    }
}

/// An open source.
struct Opened {
    src: *mut ffi::Source,
    /// What FFmpeg reads through (`Box::into_raw`; the shim holds it).
    io: *mut Source,
    info: MediaInfo,
    start_us: i64,
    video: Option<Track>,
    audio: Option<Track>,
    orientation: Orientation,
    /// The source has no more packets.
    eof: bool,
}

impl Drop for Opened {
    fn drop(&mut self) {
        self.video = None;
        self.audio = None;
        // SAFETY: the shim's source, then what it read through, which
        // nothing uses once it is closed.
        unsafe {
            ffi::vmd_close(self.src);
            drop(Box::from_raw(self.io));
        }
    }
}

/// Where pictures go.
struct VideoOut {
    pool: FramePool,
    ready: Event,
    wake: Event,
    flags: u32,
    scaler: *mut ffi::Scaler,
    /// Pictures before turning upright.
    scratch: Vec<u32>,
}

impl Drop for VideoOut {
    fn drop(&mut self) {
        // SAFETY: the shim's, or null.
        unsafe { ffi::vmd_scaler_free(self.scaler) }
    }
}

/// Converted sound not written yet.
struct Chunk {
    samples: Vec<i16>,
    /// Frames written of it.
    done: usize,
    pts_us: i64,
    flags: u32,
}

/// Where sound goes.
struct AudioOut {
    ring: PcmRing,
    ready: Event,
    wake: Event,
    rate: u32,
    resampler: *mut ffi::Resampler,
    buffer: Vec<i16>,
    chunk: Option<Chunk>,
    /// Where the sound written so far ends (µs).
    next_us: Option<i64>,
    /// The generation's end record is written.
    ended: bool,
}

impl Drop for AudioOut {
    fn drop(&mut self) {
        // SAFETY: the shim's, or null.
        unsafe { ffi::vmd_resampler_free(self.resampler) }
    }
}

/// Sums behind [`Stats`].
#[derive(Default)]
struct Counters {
    stats: Stats,
    convert_ns: u64,
    converted: u64,
}

pub struct Session {
    control: RawHandle,
    opened: Option<Opened>,
    video_out: Option<VideoOut>,
    audio_out: Option<AudioOut>,
    generation: u32,
    running: bool,
    /// Pictures it may still decode while stopped.
    allowance: u32,
    skip: SkipLevel,
    /// An exact seek's target, until the pictures and the sound reach it.
    exact_video: Option<i64>,
    exact_audio: Option<i64>,
    /// While seeking exactly: the latest picture before the target.
    candidate: Option<Frame>,
    /// Decoded pictures waiting for a free slot.
    pictures: VecDeque<Frame>,
    /// The last picture put in the pool (to make again at a new size).
    last: Option<(Frame, SlotMeta)>,
    /// The video decoder gave its last picture: the pool's end is set once
    /// the pictures waiting are published.
    video_end: bool,
    /// No picture (sound) of this generation was published yet.
    first_picture: bool,
    first_sound: bool,
    counters: Counters,
    /// Decoding threads (0: the decoders' choice).
    threads: u32,
}

impl Session {
    pub fn new(control: RawHandle) -> Session {
        Session {
            control,
            opened: None,
            video_out: None,
            audio_out: None,
            generation: 0,
            running: false,
            allowance: 0,
            skip: SkipLevel::Nothing,
            exact_video: None,
            exact_audio: None,
            candidate: None,
            pictures: VecDeque::new(),
            last: None,
            video_end: false,
            first_picture: true,
            first_sound: true,
            counters: Counters::default(),
            threads: 0,
        }
    }

    /// The events the loop waits on besides the control channel.
    pub fn wakes(&self) -> Vec<RawHandle> {
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
    pub fn clear_wakes(&self) {
        if let Some(o) = &self.video_out {
            let _ = o.wake.clear();
        }
        if let Some(o) = &self.audio_out {
            let _ = o.wake.clear();
        }
    }

    /// Once round the loop.
    pub fn beat(&self) {
        if let Some(o) = &self.video_out {
            o.pool.beat();
        }
    }

    // ---- the work ------------------------------------------------------------

    /// Does one piece of work; false if there is nothing to do until the
    /// player frees room or asks something.
    pub fn work(&mut self) -> bool {
        if self.opened.is_none() {
            return false;
        }
        self.end_hidden()
            || (self.sound_low() && (self.place_sound() || self.receive_sound() || self.feed(false) || self.read()))
            || self.place_picture()
            || self.place_sound()
            || self.receive_picture()
            || self.receive_sound()
            || self.feed(true)
            || self.feed(false)
            || self.read()
    }

    /// The player is about to run out of sound.
    fn sound_low(&self) -> bool {
        let Some(out) = &self.audio_out else { return false };
        let ended = self.opened.as_ref().and_then(|o| o.audio.as_ref()).is_none_or(|t| t.ended);
        // 4 bytes a frame.
        self.running && !ended && (out.ring.filled() as i64) < LOW_SOUND_US * out.rate as i64 / 250_000
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

    /// Hidden, no picture is decoded: at the end of the source the pictures
    /// end too.
    fn end_hidden(&mut self) -> bool {
        if self.skip != SkipLevel::Hidden {
            return false;
        }
        let Some(opened) = self.opened.as_mut() else { return false };
        let eof = opened.eof;
        let Some(t) = opened.video.as_mut() else { return false };
        if !eof || t.ended {
            return false;
        }
        t.ended = true;
        self.video_end = true;
        true
    }

    /// Puts the next decoded picture into a free slot (or, after the last,
    /// says the pictures ended).
    fn place_picture(&mut self) -> bool {
        let Some(out) = &mut self.video_out else { return false };
        let Some(frame) = self.pictures.front() else {
            if self.video_end && self.candidate.is_none() {
                self.video_end = false;
                out.pool.set_end(self.generation);
                let _ = out.ready.signal();
                return true;
            }
            return false;
        };
        let start = self.opened.as_ref().map_or(0, |o| o.start_us);
        let pts = if frame.info.pts_us == NO_TIME { 0 } else { frame.info.pts_us.saturating_sub(start) };
        let duration = frame.info.duration_us.max(0);
        // Too late to be seen: not worth converting.
        if self.running
            && !self.first_picture
            && let Some((clock_us, at_ns)) = out.pool.player_clock()
        {
            let now = vrt::time::now_ns();
            let shown = clock_us.saturating_add((now.saturating_sub(at_ns) / 1000) as i64);
            if pts.saturating_add(duration.max(1)) < shown.saturating_sub(LATE_US) {
                self.pictures.pop_front();
                self.counters.stats.late += 1;
                out.pool.note_late();
                return true;
            }
        }
        let Some(slot) = out.pool.claim() else { return false };
        let frame = self.pictures.pop_front().unwrap();
        let o = self.opened.as_ref().map_or(Orientation::default(), |o| o.orientation);
        let began = vrt::time::now_ns();
        let r = convert(out, slot, &frame, o);
        self.counters.convert_ns += vrt::time::now_ns().saturating_sub(began);
        self.counters.converted += 1;
        if r != OK {
            out.pool.abandon(slot);
            self.counters.stats.errors += 1;
            return true;
        }
        let mut flags = 0;
        if frame.info.key != 0 {
            flags |= slot_flags::KEY;
        }
        if frame.info.corrupt != 0 {
            flags |= slot_flags::CORRUPT;
        }
        if self.first_picture {
            flags |= slot_flags::DISCONTINUITY;
        }
        let meta = SlotMeta {
            generation: self.generation,
            seq: 0,
            pts_us: pts,
            duration_us: duration,
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
        self.last = Some((frame, meta));
        true
    }

    /// Writes converted sound into the ring, or the end of the generation.
    fn place_sound(&mut self) -> bool {
        let generation = self.generation;
        let Some(out) = &mut self.audio_out else { return false };
        if let Some(chunk) = &mut out.chunk {
            let total = chunk.samples.len() / 2;
            let n = (total - chunk.done).min(pcm::MAX_FRAMES as usize);
            let pts = chunk.pts_us.saturating_add(sound::duration_us(chunk.done as u64, out.rate));
            let flags = if chunk.done == 0 { chunk.flags } else { 0 };
            if !out.ring.write(generation, pts, flags, &chunk.samples[chunk.done * 2..(chunk.done + n) * 2]) {
                return false;
            }
            chunk.done += n;
            out.next_us = Some(pts.saturating_add(sound::duration_us(n as u64, out.rate)));
            if chunk.done >= total {
                out.chunk = None;
            }
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

    /// Takes a picture from the video decoder.
    fn receive_picture(&mut self) -> bool {
        if !self.wants_picture() {
            return false;
        }
        let (threads, level) = (self.threads, decode_level(self.skip));
        let Some(opened) = self.opened.as_mut() else { return false };
        let src = opened.src;
        let Some(track) = opened.video.as_mut() else { return false };
        if track.ended {
            return false;
        }
        let mut ptr = ptr::null_mut();
        let mut info = ffi::FrameInfo::default();
        // SAFETY: the decoder is ours; the shim fills both.
        let r = unsafe { ffi::vmd_receive(track.decoder, &mut ptr, &mut info) };
        match r {
            OK => {
                track.errors = 0;
                self.counters.stats.decoded += 1;
                let frame = Frame { ptr, info };
                self.take_picture(frame);
                true
            }
            AGAIN => {
                track.hungry = true;
                false
            }
            END if track.reopening => {
                // The old decoder gave all it held: a new one, leaving out
                // what is now asked, from the key frame waiting.
                if let Err(e) = track.open_again(src, threads, level) {
                    println!("cannot open the video decoder again: {e}");
                    track.ended = true;
                    self.video_end = true;
                }
                true
            }
            END => {
                track.ended = true;
                if let Some(c) = self.candidate.take() {
                    self.pictures.push_back(c);
                }
                self.exact_video = None;
                self.video_end = true;
                true
            }
            _ => {
                track.errors += 1;
                track.hungry = true;
                self.counters.stats.errors += 1;
                if track.errors > MAX_ERRORS {
                    track.ended = true;
                    self.video_end = true;
                }
                true
            }
        }
    }

    /// A decoded picture: shown, or (seeking exactly) kept until the one
    /// at the target is known.
    fn take_picture(&mut self, frame: Frame) {
        let Some(target) = self.exact_video else {
            self.pictures.push_back(frame);
            return;
        };
        let start = self.opened.as_ref().map_or(0, |o| o.start_us);
        let pts = if frame.info.pts_us == NO_TIME { i64::MIN } else { frame.info.pts_us.saturating_sub(start) };
        if pts <= target {
            // The latest before the target so far; the one before is not
            // shown.
            if self.candidate.replace(frame).is_some() {
                self.counters.stats.skipped += 1;
            }
        } else {
            // Past the target: the picture shown at it is the candidate.
            if let Some(c) = self.candidate.take() {
                self.pictures.push_back(c);
            }
            self.pictures.push_back(frame);
            self.exact_video = None;
        }
    }

    /// Takes sound from the audio decoder and converts it.
    fn receive_sound(&mut self) -> bool {
        if !self.wants_sound() {
            return false;
        }
        let Some(opened) = self.opened.as_mut() else { return false };
        let start = opened.start_us;
        let Some(track) = opened.audio.as_mut() else { return false };
        let Some(out) = self.audio_out.as_mut() else { return false };
        if track.ended {
            return false;
        }
        let mut ptr = ptr::null_mut();
        let mut info = ffi::FrameInfo::default();
        // SAFETY: as in `receive_picture`.
        let r = unsafe { ffi::vmd_receive(track.decoder, &mut ptr, &mut info) };
        let frame = match r {
            OK => {
                track.errors = 0;
                Some(Frame { ptr, info })
            }
            AGAIN => {
                track.hungry = true;
                return false;
            }
            END => {
                track.ended = true;
                None
            }
            _ => {
                track.errors += 1;
                track.hungry = true;
                self.counters.stats.errors += 1;
                if track.errors > MAX_ERRORS {
                    track.ended = true;
                }
                return true;
            }
        };
        // Converted (or, at the end, what the resampler holds back).
        let (mut frames, mut pts) = (0u32, NO_TIME);
        let (fp, fpts) = frame.as_ref().map_or((ptr::null(), NO_TIME), |f| (f.ptr as *const _, f.info.pts_us));
        // SAFETY: the buffer holds SOUND_FRAMES stereo frames.
        let r = unsafe {
            ffi::vmd_sound(
                &mut out.resampler,
                fp,
                fpts,
                out.rate,
                out.buffer.as_mut_ptr(),
                SOUND_FRAMES as u32,
                &mut frames,
                &mut pts,
            )
        };
        drop(frame);
        if r != OK || frames == 0 {
            if r != OK {
                self.counters.stats.errors += 1;
            }
            return true;
        }
        let mut samples = out.buffer[..frames as usize * 2].to_vec();
        let mut pts = if pts == NO_TIME { out.next_us.unwrap_or(0) } else { pts.saturating_sub(start) };
        let mut flags = 0;
        if let Some(target) = self.exact_audio {
            let drop = sound::frames_before(pts, frames, out.rate, target) as usize;
            if drop >= frames as usize {
                return true;
            }
            samples.drain(..drop * 2);
            pts = pts.saturating_add(sound::duration_us(drop as u64, out.rate));
            self.exact_audio = None;
        }
        if self.first_sound || out.next_us.is_none() {
            flags |= pcm::record_flags::DISCONTINUITY;
        } else {
            match sound::follow(out.next_us, pts, (samples.len() / 2) as u32, out.rate) {
                Follow::Continue => pts = out.next_us.unwrap_or(pts),
                Follow::Gap(silence) => {
                    let mut filled = vec![0i16; silence as usize * 2];
                    filled.extend_from_slice(&samples);
                    samples = filled;
                    pts = out.next_us.unwrap_or(pts);
                }
                Follow::Overlap(n) => {
                    samples.drain(..(n as usize * 2).min(samples.len()));
                    pts = out.next_us.unwrap_or(pts);
                }
                Follow::Jump => flags |= pcm::record_flags::DISCONTINUITY,
            }
        }
        if !samples.is_empty() {
            out.chunk = Some(Chunk { samples, done: 0, pts_us: pts, flags });
        }
        true
    }

    /// Gives a decoder its next packet, or the end of the source.
    fn feed(&mut self, video: bool) -> bool {
        let Some(opened) = self.opened.as_mut() else { return false };
        let eof = opened.eof;
        let track = if video { opened.video.as_mut() } else { opened.audio.as_mut() };
        let Some(track) = track else { return false };
        if !track.hungry || track.ended {
            return false;
        }
        if let Some(q) = track.queue.front() {
            if video && track.reopen && q.key && !track.draining {
                // Opened again here: first the pictures the old one holds.
                // SAFETY: the decoder is ours; NULL says there are no more.
                unsafe { ffi::vmd_send(track.decoder, ptr::null()) };
                track.draining = true;
                track.reopen = false;
                track.reopening = true;
                track.hungry = false;
                return true;
            }
            // SAFETY: both are ours; the packet stays ours.
            let r = unsafe { ffi::vmd_send(track.decoder, q.packet.0) };
            match r {
                OK => {
                    track.queue.pop();
                    track.hungry = false;
                }
                // Full: frames first.
                AGAIN | END => track.hungry = false,
                // A damaged packet: past it.
                _ => {
                    track.queue.pop();
                    self.counters.stats.errors += 1;
                }
            }
            return true;
        }
        if eof && !track.draining {
            // SAFETY: the decoder is ours; NULL says there are no more.
            unsafe { ffi::vmd_send(track.decoder, ptr::null()) };
            track.draining = true;
            track.hungry = false;
            return true;
        }
        false
    }

    /// Reads a packet for a decoder that waits for one.
    fn read(&mut self) -> bool {
        let hidden = self.skip == SkipLevel::Hidden;
        let Some(opened) = self.opened.as_mut() else { return false };
        if opened.eof {
            return false;
        }
        let tracks = [opened.video.as_ref(), opened.audio.as_ref()];
        let wanting: [bool; 2] = tracks.map(|t| t.is_some_and(|t| t.hungry && !t.ended));
        let empty: [bool; 2] = tracks.map(|t| t.is_some_and(|t| t.queue.is_empty()));
        let full: [bool; 2] = tracks.map(|t| t.is_some_and(|t| t.queue.is_full()));
        if !schedule::should_read(&wanting, &empty, &full) {
            return false;
        }
        let mut ptr = ptr::null_mut();
        let mut info = ffi::PacketInfo::default();
        // SAFETY: the source is ours; the shim fills both.
        let r = unsafe { ffi::vmd_read(opened.src, &mut ptr, &mut info) };
        match r {
            OK => {
                let packet = Queued { packet: Packet(ptr), key: info.key != 0 };
                let size = info.size as usize;
                if let Some(t) = opened.video.as_mut().filter(|t| t.index == info.stream) {
                    if !hidden {
                        self.counters.stats.queued_video = (t.queue.bytes() + size) as u32;
                        t.queue.push(packet, size);
                    }
                } else if let Some(t) = opened.audio.as_mut().filter(|t| t.index == info.stream) {
                    self.counters.stats.queued_audio = (t.queue.bytes() + size) as u32;
                    t.queue.push(packet, size);
                }
            }
            END => opened.eof = true,
            ffi::EEXIT => {
                // The player went away: the loop notices.
                opened.eof = true;
            }
            _ => {
                // A damaged file: as far as it goes.
                self.counters.stats.errors += 1;
                opened.eof = true;
            }
        }
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
            // SAFETY: the shim's, or null; made again on first use.
            unsafe { ffi::vmd_resampler_free(out.resampler) };
            out.resampler = ptr::null_mut();
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

    /// Goes to `position_us` as `generation` (see `media::Client::seek`).
    fn go_to(&mut self, position_us: i64, mode: SeekMode, generation: u32) -> Result<(), MediaError> {
        let opened = self.opened.as_mut().ok_or(MediaError::State)?;
        let target = position_us.max(0);
        // SAFETY: the source is open.
        let r = unsafe { ffi::vmd_seek(opened.src, target.saturating_add(opened.start_us)) };
        if r != OK {
            return Err(ffi::error(r));
        }
        for t in [opened.video.as_mut(), opened.audio.as_mut()].into_iter().flatten() {
            t.reset();
        }
        if let Some(t) = opened.video.as_mut().filter(|t| t.reopen) {
            // From a key frame anyway: the new level from here.
            t.open_again(opened.src, self.threads, decode_level(self.skip))?;
        }
        opened.eof = false;
        self.drop_decoded();
        self.start_generation(generation);
        let exact = (mode == SeekMode::Exact).then_some(target);
        self.exact_video = exact;
        self.exact_audio = exact;
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
        self.exact_audio = None;
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

/// Converts `frame` into `slot` of the pool, turned upright by `o`.
fn convert(out: &mut VideoOut, slot: u32, frame: &Frame, o: Orientation) -> i32 {
    let (w, h, stride) = (out.pool.width(), out.pool.height(), out.pool.stride());
    let dst = out.pool.slot_pixels(slot);
    if o.is_upright() {
        // SAFETY: the slot holds `h` rows of `stride` pixels.
        return unsafe { ffi::vmd_picture(&mut out.scaler, frame.ptr, dst, stride, w, h, out.flags) };
    }
    let (sw, sh) = o.source_size(w, h);
    out.scratch.resize(sw as usize * sh as usize, 0);
    // SAFETY: the scratch holds `sh` rows of `sw` pixels.
    let r = unsafe { ffi::vmd_picture(&mut out.scaler, frame.ptr, out.scratch.as_mut_ptr(), sw, sw, sh, out.flags) };
    if r == OK {
        // SAFETY: the slot holds the upright `h` rows of `stride` pixels.
        unsafe { turn(&out.scratch, sw as usize, sh as usize, sw as usize, dst, stride as usize, o) };
    }
    r
}

/// What a video decoder leaves out at `skip` (hidden: as before).
fn decode_level(skip: SkipLevel) -> u32 {
    (skip as u32).min(SkipLevel::KeyframesOnly as u32)
}

/// Opens the decoder of `stream`.
fn open_decoder(src: *mut ffi::Source, stream: u32, threads: u32, flags: u32) -> Result<*mut ffi::Decoder, MediaError> {
    let mut dec = ptr::null_mut();
    // SAFETY: the source is open; the shim fills `dec`.
    let r = unsafe { ffi::vmd_decoder_open(src, stream, threads, flags, &mut dec) };
    if r == OK { Ok(dec) } else { Err(ffi::error(r)) }
}

impl media::media::Server for Session {
    fn hello(&mut self, _version: u32) -> DecoderInfo {
        // SAFETY: a static, NUL-terminated string.
        let build = unsafe { core::ffi::CStr::from_ptr(ffi::vmd_build()) }.to_string_lossy().into_owned();
        DecoderInfo { version: media::VERSION, build, threads: self.threads }
    }

    fn open(&mut self, source: Channel, options: OpenOptions) -> Result<MediaInfo, MediaError> {
        self.close_source();
        self.threads = options.threads.min(64);
        let max_w = if options.max_width == 0 { DEFAULT_MAX.0 } else { options.max_width.min(media::MAX_SIDE) };
        let max_h = if options.max_height == 0 { DEFAULT_MAX.1 } else { options.max_height.min(media::MAX_SIDE) };
        let io = Box::into_raw(Box::new(Source::new(source, self.control)));
        let shim_io = ffi::Io {
            opaque: io as *mut c_void,
            read: source::read,
            seek: source::seek,
            interrupted: source::interrupted,
        };
        let mut src = ptr::null_mut();
        // SAFETY: `shim_io` is copied by the shim; `io` lives until closed.
        let r = unsafe { ffi::vmd_open(&shim_io, c"".as_ptr(), max_w, max_h, &mut src) };
        if r != OK {
            // SAFETY: the shim did not keep it.
            drop(unsafe { Box::from_raw(io) });
            return Err(ffi::error(r));
        }
        let mut opened = Opened {
            src,
            io,
            info: empty_info(),
            start_us: 0,
            video: None,
            audio: None,
            orientation: Orientation::default(),
            eof: false,
        };
        let mut shim_info = ffi::Info::zeroed();
        // SAFETY: the source is open.
        unsafe { ffi::vmd_info(src, &mut shim_info) };
        let mut streams = Vec::new();
        for i in 0..shim_info.streams.min(64) {
            let mut s = ffi::Stream::zeroed();
            // SAFETY: as above.
            if unsafe { ffi::vmd_stream(src, i, &mut s) } == OK {
                streams.push(s);
            }
        }
        let mut chapters = Vec::new();
        for i in 0..shim_info.chapters.min(media::MAX_CHAPTERS as u32) {
            let (mut at, mut title) = (0i64, vec![0u8; 256]);
            // SAFETY: `title` holds 256 bytes.
            if unsafe { ffi::vmd_chapter(src, i, &mut at, title.as_mut_ptr()) } == OK {
                chapters.push((at, title));
            }
        }
        let thumbnails = options.flags & open_flags::THUMBNAILS != 0;
        // SAFETY: as above.
        let video = info::index(unsafe { ffi::vmd_best(src, ffi::KIND_VIDEO) });
        let audio = if thumbnails { None } else { info::index(unsafe { ffi::vmd_best(src, ffi::KIND_AUDIO) }) };
        let media_info = info::media_info(&shim_info, &streams, &chapters, video, audio);
        info::playable(&media_info)?;
        let (video, audio) = (media_info.selected_video, media_info.selected_audio);
        // SAFETY: as above.
        unsafe { ffi::vmd_use(src, video.map_or(-1, |v| v as i32), audio.map_or(-1, |a| a as i32)) };
        opened.start_us = if shim_info.start_us == NO_TIME { 0 } else { shim_info.start_us };
        // One picture at a time for thumbnails: frame threads would hold
        // several back.
        let threads = if thumbnails { 1 } else { self.threads };
        if let Some(v) = video {
            let level = if thumbnails { 0 } else { decode_level(self.skip) };
            let dec = open_decoder(src, v, threads, ffi::decode_skip(level))?;
            opened.video = Some(Track::new(v, dec, schedule::VIDEO_BYTES, level));
            if let Some(vs) = media_info.video_stream() {
                opened.orientation = Orientation::new(vs.rotation, vs.mirrored);
            }
        }
        if let Some(a) = audio {
            match open_decoder(src, a, 1, 0) {
                Ok(dec) => opened.audio = Some(Track::new(a, dec, schedule::AUDIO_BYTES, 0)),
                // Pictures without their sound are better than nothing.
                Err(e) if video.is_some() => println!("no sound: {e}"),
                Err(e) => return Err(e),
            }
        }
        opened.info = media_info.clone();
        log_opened(&media_info);
        self.opened = Some(opened);
        self.start_generation(self.generation);
        self.drop_decoded();
        Ok(media_info)
    }

    fn select(&mut self, video: Option<u32>, audio: Option<u32>, generation: u32) -> Result<(), MediaError> {
        let position = self.position();
        let (threads, level) = (self.threads, decode_level(self.skip));
        let opened = self.opened.as_mut().ok_or(MediaError::State)?;
        let known_v = video.is_none_or(|v| opened.info.video.iter().any(|s| s.index == v && s.decodable));
        let known_a = audio.is_none_or(|a| opened.info.audio.iter().any(|s| s.index == a && s.decodable));
        if !known_v || !known_a {
            return Err(MediaError::Invalid);
        }
        opened.video = None;
        opened.audio = None;
        let src = opened.src;
        // SAFETY: the source is open.
        unsafe { ffi::vmd_use(src, video.map_or(-1, |v| v as i32), audio.map_or(-1, |a| a as i32)) };
        if let Some(v) = video {
            let dec = open_decoder(src, v, threads, ffi::decode_skip(level))?;
            opened.video = Some(Track::new(v, dec, schedule::VIDEO_BYTES, level));
        }
        if let Some(a) = audio {
            opened.audio = Some(Track::new(a, open_decoder(src, a, 1, 0)?, schedule::AUDIO_BYTES, 0));
        }
        opened.info.selected_video = video;
        opened.info.selected_audio = audio;
        opened.orientation =
            opened.info.video_stream().map_or(Orientation::default(), |v| Orientation::new(v.rotation, v.mirrored));
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
        let flags = if spec.flags & video_flags::BITEXACT != 0 { ffi::PICTURE_BITEXACT } else { 0 };
        pool.set_generation(self.generation);
        let mut out = VideoOut { pool, ready, wake, flags, scaler: ptr::null_mut(), scratch: Vec::new() };
        // The last picture again, at the new size.
        if let Some((frame, meta)) = &self.last
            && let Some(slot) = out.pool.claim()
        {
            let o = self.opened.as_ref().map_or(Orientation::default(), |o| o.orientation);
            if convert(&mut out, slot, frame, o) == OK {
                let meta = SlotMeta {
                    width: out.pool.width(),
                    height: out.pool.height(),
                    flags: meta.flags | slot_flags::REPEAT,
                    ..*meta
                };
                out.pool.publish(slot, &meta);
                let _ = out.ready.signal();
            } else {
                out.pool.abandon(slot);
            }
        }
        self.video_out = Some(out);
        Ok(())
    }

    fn set_audio(&mut self, spec: AudioSpec, ring: Vmo, ready: Event, wake: Event) -> Result<(), MediaError> {
        let ring = PcmRing::map(ring).map_err(|_| MediaError::Invalid)?;
        if ring.rate() != spec.rate {
            return Err(MediaError::Invalid);
        }
        self.audio_out = Some(AudioOut {
            ring,
            ready,
            wake,
            rate: spec.rate,
            resampler: ptr::null_mut(),
            buffer: vec![0; SOUND_FRAMES * 2],
            chunk: None,
            next_us: None,
            ended: false,
        });
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
        let wanted = decode_level(level);
        if let Some(t) = self.opened.as_mut().and_then(|o| o.video.as_mut()) {
            if level == SkipLevel::Hidden {
                // Nothing more to decode: the player seeks when it shows
                // pictures again.
                t.queue.clear(drop);
            } else if wanted != t.level {
                // SAFETY: the decoder is ours.
                match unsafe { ffi::vmd_skip(t.decoder, wanted) } {
                    OK => {
                        t.level = wanted;
                        t.reopen = false;
                    }
                    // At the next key frame.
                    _ => t.reopen = !t.reopening,
                }
            } else {
                t.reopen = false;
            }
        }
        if level == SkipLevel::Hidden {
            self.pictures.clear();
            self.candidate = None;
        }
        Ok(())
    }

    fn thumbnail(&mut self, at_us: i64, max_width: u32, max_height: u32) -> Result<Thumbnail, MediaError> {
        let opened = self.opened.as_mut().ok_or(MediaError::State)?;
        let stream = opened.info.video_stream().cloned().ok_or(MediaError::Unsupported)?;
        let o = opened.orientation;
        let (dw, dh) = stream.display_size();
        let (w, h) = media::fit(dw, dh, max_width.clamp(1, MAX_THUMBNAIL), max_height.clamp(1, MAX_THUMBNAIL));
        let frame = picture_at(opened, at_us.max(0))?;
        let len = (w as usize * h as usize * 4).next_multiple_of(4096);
        let vmo = Vmo::create(len).map_err(|_| MediaError::NoMemory)?;
        let theirs = vmo.0.duplicate(None).map(Vmo::from_handle).map_err(|_| MediaError::NoMemory)?;
        let map =
            Mapping::new(vmo, len, vabi::map_flags::READ | vabi::map_flags::WRITE).map_err(|_| MediaError::NoMemory)?;
        let mut scaler = ptr::null_mut();
        let dst = map.as_ptr() as *mut u32;
        let r = if o.is_upright() {
            // SAFETY: the mapping holds `h` rows of `w` pixels.
            unsafe { ffi::vmd_picture(&mut scaler, frame.ptr, dst, w, w, h, 0) }
        } else {
            let (sw, sh) = o.source_size(w, h);
            let mut scratch = vec![0u32; sw as usize * sh as usize];
            // SAFETY: the scratch holds `sh` rows of `sw` pixels.
            let r = unsafe { ffi::vmd_picture(&mut scaler, frame.ptr, scratch.as_mut_ptr(), sw, sw, sh, 0) };
            if r == OK {
                // SAFETY: the mapping holds the upright `h` rows of `w`.
                unsafe { turn(&scratch, sw as usize, sh as usize, sw as usize, dst, w as usize, o) };
            }
            r
        };
        // SAFETY: the shim's.
        unsafe { ffi::vmd_scaler_free(scaler) };
        if r != OK {
            return Err(ffi::error(r));
        }
        let pts = if frame.info.pts_us == NO_TIME { at_us } else { frame.info.pts_us.saturating_sub(opened.start_us) };
        Ok(Thumbnail { pixels: theirs, width: w, height: h, stride: w, pts_us: pts })
    }

    fn checksums(&mut self, max_pictures: u32) -> Result<Vec<PictureChecksum>, MediaError> {
        let opened = self.opened.as_mut().ok_or(MediaError::State)?;
        let max = max_pictures.min(media::MAX_CHECKSUMS) as usize;
        let start = opened.start_us;
        let track = opened.video.as_mut().ok_or(MediaError::Unsupported)?;
        track.reset();
        // SAFETY: the source is open.
        unsafe { ffi::vmd_seek(opened.src, start) };
        opened.eof = false;
        let mut sums = Vec::new();
        let mut read = 0;
        while sums.len() < max {
            match next_picture(opened, &mut read)? {
                Some(frame) => {
                    let mut md5 = [0u8; 16];
                    // SAFETY: a decoded picture; `md5` holds 16 bytes.
                    let r = unsafe { ffi::vmd_frame_md5(frame.ptr, md5.as_mut_ptr()) };
                    if r != OK {
                        return Err(ffi::error(r));
                    }
                    let pts = if frame.info.pts_us == NO_TIME { -1 } else { frame.info.pts_us.saturating_sub(start) };
                    sums.push(PictureChecksum { pts_us: pts, md5 });
                }
                None => break,
            }
        }
        Ok(sums)
    }

    fn stats(&mut self) -> Stats {
        let c = &self.counters;
        Stats { convert_us: (c.convert_ns / c.converted.max(1) / 1000) as u32, ..c.stats }
    }

    fn close(&mut self) {
        self.close_source();
    }
}

/// The picture shown at `at_us` (a thumbnail): decoded from the key frame
/// before it, at most [`MAX_THUMBNAIL_PICTURES`] pictures on (the last of
/// them, if it is that far).
fn picture_at(opened: &mut Opened, at_us: i64) -> Result<Frame, MediaError> {
    let start = opened.start_us;
    let track = opened.video.as_mut().ok_or(MediaError::Unsupported)?;
    track.reset();
    // SAFETY: the source is open.
    let r = unsafe { ffi::vmd_seek(opened.src, at_us.saturating_add(start)) };
    if r != OK {
        return Err(ffi::error(r));
    }
    opened.eof = false;
    let mut read = 0;
    let mut shown: Option<Frame> = None;
    for _ in 0..MAX_THUMBNAIL_PICTURES {
        let Some(frame) = next_picture(opened, &mut read)? else { break };
        let pts = if frame.info.pts_us == NO_TIME { i64::MIN } else { frame.info.pts_us.saturating_sub(start) };
        if pts > at_us {
            // Past it: the one before is shown at `at_us` (or this one, the
            // first there is).
            shown.get_or_insert(frame);
            break;
        }
        shown = Some(frame);
    }
    shown.ok_or(MediaError::Corrupt)
}

/// Decodes the next picture of the video stream, reading as needed (sound
/// packets are passed over): `None` at the end.
fn next_picture(opened: &mut Opened, read: &mut u32) -> Result<Option<Frame>, MediaError> {
    let audio = opened.audio.as_ref().map(|t| t.index);
    let track = opened.video.as_mut().ok_or(MediaError::Unsupported)?;
    loop {
        let mut ptr = ptr::null_mut();
        let mut info = ffi::FrameInfo::default();
        // SAFETY: the decoder is ours; the shim fills both.
        match unsafe { ffi::vmd_receive(track.decoder, &mut ptr, &mut info) } {
            OK => return Ok(Some(Frame { ptr, info })),
            END => return Ok(None),
            AGAIN => {}
            _ => {
                track.errors += 1;
                if track.errors > MAX_ERRORS {
                    return Err(MediaError::Corrupt);
                }
            }
        }
        if opened.eof {
            if track.draining {
                return Ok(None);
            }
            // SAFETY: as above; NULL says there are no more.
            unsafe { ffi::vmd_send(track.decoder, ptr::null()) };
            track.draining = true;
            continue;
        }
        *read += 1;
        if *read > MAX_SEARCH_PACKETS {
            return Err(MediaError::Corrupt);
        }
        let mut p = ptr::null_mut();
        let mut pinfo = ffi::PacketInfo::default();
        // SAFETY: the source is open.
        match unsafe { ffi::vmd_read(opened.src, &mut p, &mut pinfo) } {
            OK => {
                let packet = Packet(p);
                if pinfo.stream == track.index {
                    // SAFETY: both are ours.
                    let r = unsafe { ffi::vmd_send(track.decoder, packet.0) };
                    if r != OK && r != AGAIN {
                        track.errors += 1;
                    }
                } else {
                    debug_assert!(Some(pinfo.stream) == audio);
                }
            }
            END => opened.eof = true,
            ffi::EEXIT => return Err(MediaError::Io),
            _ => opened.eof = true,
        }
    }
}

fn empty_info() -> MediaInfo {
    MediaInfo {
        container: String::new(),
        duration_us: None,
        seekable: false,
        bit_rate: 0,
        title: String::new(),
        artist: String::new(),
        video: Vec::new(),
        audio: Vec::new(),
        subtitles: 0,
        chapters: Vec::new(),
        selected_video: None,
        selected_audio: None,
    }
}

/// One line saying what was opened.
fn log_opened(info: &MediaInfo) {
    let video = info.video_stream().map(|v| {
        let fps = if v.frame_rate.is_known() { (v.frame_rate.num * 100 / v.frame_rate.den.max(1)) as u64 } else { 0 };
        alloc::format!("{} {}x{} {}.{:02} fps", v.codec, v.width, v.height, fps / 100, fps % 100)
    });
    let audio = info.audio_stream().map(|a| alloc::format!("{} {} Hz {} ch", a.codec, a.rate, a.channels));
    let duration = info.duration_us.map_or(String::from("?"), |d| {
        let s = d / 1_000_000;
        alloc::format!("{}:{:02}", s / 60, s % 60)
    });
    println!(
        "opened {}: {}, {}, {}",
        info.container,
        video.as_deref().unwrap_or("no picture"),
        audio.as_deref().unwrap_or("no sound"),
        duration
    );
}
