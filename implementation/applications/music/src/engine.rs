// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The playback engine: a background thread that loads the library,
//! decodes the current track and streams it to the audio service.
//!
//! The stream outlives the audio service (`videos_core::output`): while the
//! service is away the track waits where it was heard last, and goes on
//! from there in a new stream when the service is back.
//!
//! The UI sends [`Command`]s and reads [`Status`]; the engine signals the
//! `notify` event when something the UI shows changed (a track loaded or
//! ended, the library is ready). Decoded audio is also appended to a
//! [`VizHistory`], from which the UI computes the spectrum of the samples
//! that are being heard right now.

use std::collections::VecDeque;
use std::sync::Arc;

use abi::{WaitItem, signals};
use audio::source::Source;
use files::format::duration as format_time;
use proto::audio::{OutputStream, ring};
use proto::fs::vfs;
use rt::object::Event;
use rt::println;
use rt::sync::Mutex;
use rt::vm::Mapping;
use videos_core::output::{Kept, Output};

use crate::library::{self, Track};

/// Frames decoded per write.
const CHUNK: usize = 4096;
/// Stream buffering (the decoder runs this far ahead of playback).
const BUFFER_MS: u32 = 700;
/// Frames of mono history kept for the visualiser.
const HISTORY: usize = 1 << 16;
/// How often the position is published while playing (ns).
const TICK_NS: u64 = 40_000_000;

/// Requests from the UI.
#[derive(Debug, Clone)]
pub enum Command {
    /// Opens a file and starts at `start_ms` (playing or paused).
    Load {
        path: String,
        play: bool,
        start_ms: u64,
        /// Echoed in [`Status::load_id`] once the track is loaded (or failed).
        id: u32,
    },
    Play,
    Pause,
    /// Seeks within the current track.
    Seek {
        ms: u64,
        /// Echoed in [`Status::seek_id`] once done.
        id: u32,
    },
    /// Stream volume, linear 0..=1.
    Volume(f32),
    /// Adds a file to the library (opened from the command line); plays it
    /// too when `play` is set.
    AddFile {
        path: String,
        play: bool,
    },
    /// Looks for files added to or removed from the Music folder.
    Rescan,
}

/// The track list shared with the UI.
#[derive(Default)]
pub struct Library {
    pub tracks: Vec<Track>,
    /// Incremented whenever `tracks` changes.
    pub generation: u32,
    /// A file to play as soon as the UI has seen it (from `AddFile`).
    pub play_request: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayState {
    Idle,
    Playing,
    Paused,
    Error,
}

/// What the UI shows about playback.
#[derive(Debug, Clone)]
pub struct Status {
    pub state: PlayState,
    pub path: String,
    pub rate: u32,
    pub frames: u64,
    /// Track position in frames at `pos_ns`.
    pub pos_frames: u64,
    pub pos_ns: u64,
    /// Incremented whenever a track plays to its end.
    pub ended: u32,
    pub error: String,
    /// Output device name ("none" without sound hardware).
    pub device: String,
    /// A stream of the audio service is open (not while it restarts).
    pub output: bool,
    /// The id of the last `Load` command handled.
    pub load_id: u32,
    /// The id of the last `Seek` command handled.
    pub seek_id: u32,
}

impl Status {
    /// The track position (frames) extrapolated to `now`.
    pub fn position_at(&self, now: u64) -> u64 {
        let mut p = self.pos_frames;
        if self.state == PlayState::Playing && self.rate > 0 {
            let dt = now.saturating_sub(self.pos_ns).min(250_000_000);
            p += (dt as u128 * self.rate as u128 / 1_000_000_000) as u64;
        }
        p.min(self.frames)
    }

    pub fn position_ms(&self, now: u64) -> u64 {
        if self.rate == 0 { 0 } else { self.position_at(now) * 1000 / self.rate as u64 }
    }

    pub fn duration_ms(&self) -> u64 {
        if self.rate == 0 { 0 } else { self.frames * 1000 / self.rate as u64 }
    }
}

/// Recently decoded audio (mono), indexed by track frame.
pub struct VizHistory {
    buf: Vec<i16>,
    /// Track frame just after the newest sample.
    end: u64,
    len: u64,
}

impl VizHistory {
    fn new() -> VizHistory {
        VizHistory { buf: vec![0; HISTORY], end: 0, len: 0 }
    }

    fn reset(&mut self, at: u64) {
        self.end = at;
        self.len = 0;
    }

    fn push(&mut self, start: u64, stereo: &[i16]) {
        if start != self.end {
            self.reset(start);
        }
        let mask = HISTORY as u64 - 1;
        for f in stereo.as_chunks::<2>().0 {
            self.buf[(self.end & mask) as usize] = ((f[0] as i32 + f[1] as i32) >> 1) as i16;
            self.end += 1;
        }
        self.len = (self.len + (stereo.len() / 2) as u64).min(HISTORY as u64);
    }

    /// Fills `out` with the samples just before track frame `end` (as
    /// floats in -1..1; unknown samples are 0). Returns false if nothing
    /// around `end` is known.
    pub fn window(&self, end: u64, out: &mut [f32]) -> bool {
        let first_known = self.end - self.len;
        if end <= first_known || end > self.end + 2048 {
            out.fill(0.0);
            return false;
        }
        let mask = HISTORY as u64 - 1;
        let n = out.len() as u64;
        for (i, o) in out.iter_mut().enumerate() {
            let f = end as i64 - n as i64 + i as i64;
            *o = if f >= first_known as i64 && (f as u64) < self.end {
                self.buf[(f as u64 & mask) as usize] as f32 * (1.0 / 32768.0)
            } else {
                0.0
            };
        }
        true
    }
}

/// State shared between the UI and the engine thread.
pub struct Shared {
    commands: Mutex<VecDeque<Command>>,
    pub status: Mutex<Status>,
    pub library: Mutex<Library>,
    pub viz: Mutex<VizHistory>,
    /// UI → engine.
    wake: Event,
    /// Engine → UI.
    pub notify: Event,
}

impl Shared {
    pub fn new() -> Option<Arc<Shared>> {
        Some(Arc::new(Shared {
            commands: Mutex::new(VecDeque::new()),
            status: Mutex::new(Status {
                state: PlayState::Idle,
                path: String::new(),
                rate: 0,
                frames: 0,
                pos_frames: 0,
                pos_ns: 0,
                ended: 0,
                error: String::new(),
                device: String::new(),
                output: false,
                load_id: 0,
                seek_id: 0,
            }),
            library: Mutex::new(Library::default()),
            viz: Mutex::new(VizHistory::new()),
            wake: Event::create().ok()?,
            notify: Event::create().ok()?,
        }))
    }

    /// Queues a command for the engine.
    pub fn send(&self, c: Command) {
        self.commands.lock().push_back(c);
        let _ = self.wake.signal();
    }

    pub fn status(&self) -> Status {
        self.status.lock().clone()
    }
}

/// A file mapped from the VMO that vfs returned.
pub struct FileData {
    map: Mapping,
    len: usize,
}

impl AsRef<[u8]> for FileData {
    fn as_ref(&self) -> &[u8] {
        // SAFETY: the read-only mapping covers at least `len` bytes and lives
        // as long as `self`.
        unsafe { core::slice::from_raw_parts(self.map.as_ptr(), self.len) }
    }
}

struct Engine {
    shared: Arc<Shared>,
    vfs: Option<vfs::Client>,
    output: Output,
    /// Whether there was an output, as last published, and its losses as
    /// last seen.
    published_output: Option<bool>,
    losses: u32,
    source: Option<Source<FileData>>,
    playing: bool,
    eof: bool,
    /// Stream "played" count that corresponds to `track_base`.
    stream_base: u64,
    track_base: u64,
    /// The track's position (frames) as last heard: where it goes on from
    /// in a new stream.
    position: u64,
    buf: Vec<i16>,
    /// Title of the loaded track (for the log).
    title: String,
    /// The Music folder as of the last scan.
    listing: Vec<library::Entry>,
    /// Files added from outside the Music folder.
    extra: Vec<Track>,
}

impl Engine {
    fn read_file(&mut self, path: &str) -> Result<FileData, String> {
        if self.vfs.is_none() {
            self.vfs = proto::connect(vfs::NAME).ok().map(vfs::Client::new);
        }
        let vfs = self.vfs.as_ref().ok_or("the file system is unavailable")?;
        let (vmo, len) = match vfs.read_file(path.into()) {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => return Err(format!("{e}")),
            Err(_) => return Err("the file system is unavailable".into()),
        };
        let size = vmo.size().map_err(|_| "cannot map the file")?;
        let map = Mapping::new(vmo, size.max(4096), abi::map_flags::READ).map_err(|_| "cannot map the file")?;
        Ok(FileData { map, len: (len as usize).min(size) })
    }

    fn set_status(&self, f: impl FnOnce(&mut Status)) {
        f(&mut self.shared.status.lock());
    }

    fn fail(&mut self, msg: String) {
        println!("{}", msg);
        self.playing = false;
        self.set_status(|s| {
            s.state = PlayState::Error;
            s.error = msg;
        });
        let _ = self.shared.notify.signal();
    }

    /// The track's position (frames): what the stream played of it, or
    /// without a stream, where it was heard last.
    fn track_position(&mut self) -> u64 {
        let frames = self.source.as_ref().map(|s| s.frames()).unwrap_or(0);
        if let Some(s) = self.output.stream() {
            self.position = self.track_base + s.played_now().saturating_sub(self.stream_base);
        }
        self.position = self.position.min(frames);
        self.position
    }

    /// Makes sure a stream at `rate` exists (if the audio service is
    /// there) and is empty.
    fn prepare_stream(&mut self, rate: u32) {
        self.output.set_rate(rate);
        match self.output.keep(rt::time::now_ns()) {
            Kept::Opened => self.stream_base = 0,
            Kept::Same => self.stream_base = self.output.flush().unwrap_or(0),
            Kept::None => {}
        }
        self.publish_output();
    }

    /// A new stream (the audio service came back): the track goes on in it
    /// from where it was heard last.
    fn renewed(&mut self) {
        let at = self.position;
        let Some(src) = &mut self.source else { return };
        if self.output.losses() != self.losses {
            self.losses = self.output.losses();
            println!("sound output back ({})", self.output.device());
        }
        let at = at.min(src.frames());
        src.seek(at);
        self.track_base = at;
        self.stream_base = 0;
        self.eof = false;
        self.shared.viz.lock().reset(at);
        self.fill();
    }

    /// Says whether there is an output, and which device, when that
    /// changes.
    fn publish_output(&mut self) {
        let open = self.output.stream().is_some();
        if self.published_output == Some(open) {
            return;
        }
        self.published_output = Some(open);
        let device = String::from(self.output.device());
        self.set_status(|s| {
            s.output = open;
            s.device = device;
        });
        let _ = self.shared.notify.signal();
    }

    fn set_paused(&mut self, paused: bool) {
        self.output.set_paused(paused);
    }

    fn load(&mut self, path: String, play: bool, start_ms: u64) {
        let data = match self.read_file(&path) {
            Ok(d) => d,
            Err(e) => return self.fail(format!("cannot open {}: {}", path, e)),
        };
        let mut src = match Source::open(data) {
            Ok(s) => s,
            Err(e) => return self.fail(format!("cannot play {}: {}", path, e)),
        };
        let rate = src.rate();
        let start = (start_ms.saturating_mul(rate as u64) / 1000).min(src.frames());
        src.seek(start);
        // Pause the old stream first so its tail does not keep playing.
        self.set_paused(true);
        // Without the audio service the track waits, and plays once it is
        // back.
        self.prepare_stream(rate);
        self.track_base = start;
        self.position = start;
        self.eof = false;
        self.shared.viz.lock().reset(start);
        let frames = src.frames();
        let tags = src.tags();
        self.title =
            if tags.title.is_empty() { String::from(files::path::file_stem(&path)) } else { tags.title.clone() };
        println!(
            "{} {} ({}, {} Hz, {} ch, {})",
            if play { "playing" } else { "loaded" },
            self.title,
            src.format().name(),
            rate,
            src.file_channels(),
            format_time(frames * 1000 / rate as u64)
        );
        self.source = Some(src);
        self.playing = play;
        self.fill();
        if play {
            self.set_paused(false);
        }
        let now = rt::time::now_ns();
        self.set_status(|s| {
            s.state = if play { PlayState::Playing } else { PlayState::Paused };
            s.path = path;
            s.rate = rate;
            s.frames = frames;
            s.pos_frames = start;
            s.pos_ns = now;
            s.error.clear();
        });
        let _ = self.shared.notify.signal();
    }

    /// The current track position as `m:ss` (for the log).
    fn position_text(&mut self) -> String {
        let rate = self.source.as_ref().map(|s| s.rate()).unwrap_or(1).max(1);
        format_time(self.track_position() * 1000 / rate as u64)
    }

    fn seek(&mut self, ms: u64) {
        let Some(src) = &mut self.source else { return };
        let target = (ms.saturating_mul(src.rate() as u64) / 1000).min(src.frames().saturating_sub(1));
        println!("seek to {}", format_time(target * 1000 / src.rate().max(1) as u64));
        src.seek(target);
        if let Some(pos) = self.output.flush() {
            self.stream_base = pos;
        }
        self.track_base = target;
        self.position = target;
        self.eof = false;
        self.shared.viz.lock().reset(target);
        self.fill();
    }

    /// Decodes into the stream until its ring is full.
    fn fill(&mut self) {
        let (Some(stream), Some(src)) = (self.output.stream(), &mut self.source) else { return };
        if self.eof {
            return;
        }
        let _ = stream.event().clear();
        loop {
            let free = stream.free() as usize;
            if free < 256 {
                break;
            }
            let n = free.min(CHUNK);
            let start = src.position();
            let got = src.read(&mut self.buf[..n * 2]);
            if got == 0 {
                self.eof = true;
                stream.ring().set_producer_flags(ring::flags::END);
                break;
            }
            stream.write(&self.buf[..got * 2]);
            self.shared.viz.lock().push(start, &self.buf[..got * 2]);
        }
    }

    fn handle(&mut self, c: Command) {
        match c {
            Command::Load { path, play, start_ms, id } => {
                self.load(path, play, start_ms);
                self.set_status(|s| s.load_id = id);
                let _ = self.shared.notify.signal();
            }
            Command::Play => {
                if self.source.is_some() && !self.playing {
                    if self.eof && self.output.stream().is_some_and(drained) {
                        // Finished: play again from the start.
                        self.seek(0);
                    }
                    println!("resumed at {}", self.position_text());
                    self.playing = true;
                    self.set_paused(false);
                    self.set_status(|s| s.state = PlayState::Playing);
                }
            }
            Command::Pause => {
                if self.source.is_some() && self.playing {
                    println!("paused at {}", self.position_text());
                    self.playing = false;
                    self.set_paused(true);
                    self.set_status(|s| s.state = PlayState::Paused);
                }
            }
            Command::Seek { ms, id } => {
                self.seek(ms);
                // The new position goes out with the id: the UI must never
                // take the old one for the answer.
                let (pos, now) = (self.track_position(), rt::time::now_ns());
                self.set_status(|s| {
                    s.pos_frames = pos;
                    s.pos_ns = now;
                    s.seek_id = id;
                });
            }
            Command::Volume(v) => self.output.set_volume(v.clamp(0.0, 1.0)),
            Command::AddFile { path, play } => {
                let known = self.shared.library.lock().tracks.iter().any(|t| t.path == path);
                if !known {
                    match self.vfs.as_ref().and_then(|v| library::probe(v, &path)) {
                        Some(t) => {
                            self.extra.retain(|x| x.path != t.path);
                            self.extra.push(t.clone());
                            let mut lib = self.shared.library.lock();
                            lib.tracks.push(t);
                            lib.generation = lib.generation.wrapping_add(1);
                        }
                        None => {
                            println!("cannot play {}: not a supported audio file", path);
                            return;
                        }
                    }
                }
                if play {
                    self.shared.library.lock().play_request = Some(path);
                }
                let _ = self.shared.notify.signal();
            }
            Command::Rescan => self.rescan(false),
        }
    }

    /// Re-reads the Music folder; probes only new or changed files.
    fn rescan(&mut self, initial: bool) {
        let Some(vfs) = &self.vfs else { return };
        let listing = library::listing(vfs);
        if !initial && listing == self.listing {
            return;
        }
        let old: Vec<Track> = self.shared.library.lock().tracks.clone();
        let mut tracks = Vec::with_capacity(listing.len());
        for e in &listing {
            let path = e.path();
            let unchanged = self.listing.iter().any(|o| o == e);
            match old.iter().find(|t| t.path == path) {
                Some(t) if unchanged => tracks.push(t.clone()),
                _ => {
                    if let Some(t) = library::probe(vfs, &path) {
                        tracks.push(t);
                    }
                }
            }
        }
        library::sort(&mut tracks);
        // Files opened from elsewhere stay at the end.
        for t in &self.extra {
            if !tracks.iter().any(|x| x.path == t.path) {
                tracks.push(t.clone());
            }
        }
        if initial {
            println!("library: {} track(s) in {}", tracks.len(), library::music_dir());
        } else {
            println!("library updated: {} track(s)", tracks.len());
        }
        self.listing = listing;
        {
            let mut lib = self.shared.library.lock();
            lib.tracks = tracks;
            lib.generation = lib.generation.wrapping_add(1);
        }
        let _ = self.shared.notify.signal();
    }

    fn run(&mut self) -> ! {
        self.vfs = proto::connect(vfs::NAME).ok().map(vfs::Client::new);
        self.rescan(true);
        loop {
            let now = rt::time::now_ns();
            let mut deadline = if self.playing { now + TICK_NS } else { abi::DEADLINE_INFINITE };
            // Without a stream for the track loaded: the output is tried again.
            if self.source.is_some()
                && let Some(at) = self.output.retry_at()
            {
                deadline = deadline.min(at.max(now + TICK_NS));
            }
            let mut items =
                vec![WaitItem { handle: self.shared.wake.raw(), signals: signals::SIGNALED, ..Default::default() }];
            self.output.wait_items(&mut items);
            let _ = rt::object::wait_many(&mut items, deadline);
            let _ = self.shared.wake.clear();
            if let Some(s) = self.output.stream() {
                let _ = s.event().clear();
            }
            self.output.observe();
            if self.source.is_some() && self.output.keep(rt::time::now_ns()) == Kept::Opened {
                self.renewed();
            }
            self.publish_output();
            loop {
                let next = self.shared.commands.lock().pop_front();
                match next {
                    Some(c) => self.handle(c),
                    None => break,
                }
            }
            if self.playing {
                self.fill();
            }
            // Publish the position; detect the end of the track.
            let pos = self.track_position();
            let now = rt::time::now_ns();
            let finished = self.playing && self.eof && self.output.stream().is_some_and(drained);
            if finished {
                println!("finished {}", self.title);
                self.playing = false;
                self.set_paused(true);
            }
            let playing = self.playing;
            self.set_status(|s| {
                s.pos_frames = pos;
                s.pos_ns = now;
                if finished {
                    s.state = PlayState::Idle;
                    s.ended = s.ended.wrapping_add(1);
                } else if s.state == PlayState::Playing && !playing {
                    s.state = PlayState::Paused;
                }
            });
            if finished {
                let _ = self.shared.notify.signal();
            }
        }
    }
}

/// Starts the engine thread.
pub fn start(shared: Arc<Shared>, volume: f32) -> bool {
    let builder = rt::thread::Builder::new().name("decoder").priority(abi::priority::NORMAL + 4);
    builder
        .spawn(move || -> () {
            let mut e = Engine {
                shared,
                vfs: None,
                output: Output::new("Music", 44_100, BUFFER_MS, volume.clamp(0.0, 1.0), true),
                published_output: None,
                losses: 0,
                source: None,
                playing: false,
                eof: false,
                stream_base: 0,
                track_base: 0,
                position: 0,
                buf: vec![0; CHUNK * 2],
                title: String::new(),
                listing: Vec::new(),
                extra: Vec::new(),
            };
            e.run()
        })
        .is_ok()
}

/// True when everything written to the stream has been played (within
/// 10 ms, the resolution of the service's position reports).
fn drained(s: &OutputStream) -> bool {
    s.queued() == 0 && s.played_now() + (s.rate() / 100) as u64 >= s.written()
}
