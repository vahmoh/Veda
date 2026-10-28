// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The engine thread: the decoder's keeper.
//!
//! It starts the decoder (`mediadec`, or a stand-in named on the command
//! line) with nothing but its control channel, opens videos with it, makes
//! the frame pool the window shows pictures from and the ring the audio
//! thread plays sound from, and carries out what the window asks (seek,
//! play, pause, a new size). It gives every seek it has the decoder make a
//! new generation (the window's, opening's and a restart's), and says in
//! its state which generation is decoded now and which of the window's
//! seeks it carried out last, or could not.
//!
//! It watches the decoder: one that crashes, does not answer a call, stops
//! counting its heartbeat or takes too much of the machine's memory is
//! killed and started again at the picture on the screen (at most a few
//! times a minute), and asked again what the window last asked of it: how
//! much to leave out to keep up, or that the window is hidden.
//!
//! Calls to the decoder block, so they are made here, never on the
//! window's thread.

use core::sync::atomic::{AtomicI64, Ordering};
use std::collections::VecDeque;
use std::sync::Arc;

use abi::{WaitItem, signals};
use proto::fs::{FsError, file, open_flags, vfs};
use proto::media::{
    self, AudioSpec, FramePool, MediaError, MediaInfo, OpenOptions, PcmRing, SeekMode, SkipLevel, VideoSpec,
    media::Client,
};
use runtime::object::{Channel, Event, Vmo};
use runtime::println;
use runtime::sync::Mutex;
use videos_core::layout;
use videos_core::playback::{EnginePhase, EngineState};

use crate::audio;
use crate::decoder::{self, Decoder, Image};

/// The decoder, unless the command line names another.
pub const DECODER: &str = "/system/bin/mediadec";
/// How long a call may take: opening reads and probes the file.
const OPEN_TIMEOUT_NS: u64 = 15_000_000_000;
const CALL_TIMEOUT_NS: u64 = 3_000_000_000;
/// How often the decoder is looked at while a video is open, and while
/// none is.
const WATCH_NS: u64 = 250_000_000;
const IDLE_WATCH_NS: u64 = 2_000_000_000;
/// A heartbeat still for this long: hung (the decoder beats every second).
const HANG_NS: u64 = 5_000_000_000;
/// A decoder may take this share of the machine's memory (a half), and no
/// less and no more than these: more is something wrong with the file.
const MEMORY_SHARE: u64 = 2;
const MIN_MEMORY_LIMIT: u64 = 256 << 20;
const MAX_MEMORY_LIMIT: u64 = 1536 << 20;
/// Restarts allowed within a minute.
const MAX_RESTARTS: usize = 3;
/// The sound ring's size.
const RING_BYTES: u32 = 256 << 10;
/// Stopped twice this close together (µs): something there breaks the
/// decoder, and playback goes on [`SKIP_US`] later.
const SAME_SPOT_US: i64 = 2_000_000;
const SKIP_US: i64 = 10_000_000;

pub enum Command {
    /// Opens `path`, from `start_us`, playing if `play`; `id` names the
    /// request in the state. Its pictures are made to fit `stage` (the
    /// window) on a `screen` of that size, until the window asks for
    /// another size.
    Open {
        path: String,
        start_us: i64,
        play: bool,
        id: u32,
        stage: (u32, u32),
        screen: (u32, u32),
    },
    /// Pictures are shown at `width` x `height`.
    Output {
        width: u32,
        height: u32,
    },
    /// Goes to `position_us`; `id` says which seek is done in the state.
    Seek {
        position_us: i64,
        mode: SeekMode,
        id: u32,
    },
    Play,
    Pause,
    /// Decodes this many more pictures while paused (stepping).
    Step(u32),
    Skip(SkipLevel),
    Close,
}

pub struct Shared {
    commands: Mutex<VecDeque<Command>>,
    wake: Event,
    /// Signalled whenever the state changes or a pool is waiting.
    pub notify: Event,
    /// The state, and its version (counted up with every change).
    state: Mutex<(u64, EngineState)>,
    /// A new frame pool for the window to take.
    pool: Mutex<Option<FramePool>>,
    /// The time of the picture on the screen (µs), where a restarted
    /// decoder starts.
    pub shown_us: AtomicI64,
    /// The event the decoder signals with pictures (the window waits on
    /// it), and the one it waits on (the window and the audio thread signal
    /// it after taking something).
    pub video_ready: Event,
    pub decoder_wake: Event,
}

impl Shared {
    pub fn send(&self, c: Command) {
        self.commands.lock().push_back(c);
        let _ = self.wake.signal();
    }

    /// The state, if it changed since version `seen` (which it updates).
    pub fn state_since(&self, seen: &mut u64) -> Option<EngineState> {
        let s = self.state.lock();
        (s.0 != *seen).then(|| {
            *seen = s.0;
            s.1.clone()
        })
    }

    pub fn take_pool(&self) -> Option<FramePool> {
        self.pool.lock().take()
    }
}

/// Starts the engine thread, with `decoder` the program to start.
pub fn start(decoder: String, sound: Option<Arc<audio::Shared>>) -> Option<Arc<Shared>> {
    let shared = Arc::new(Shared {
        commands: Mutex::new(VecDeque::new()),
        wake: Event::create().ok()?,
        notify: Event::create().ok()?,
        state: Mutex::new((1, EngineState::default())),
        pool: Mutex::new(None),
        shown_us: AtomicI64::new(0),
        video_ready: Event::create().ok()?,
        decoder_wake: Event::create().ok()?,
    });
    let ours = shared.clone();
    runtime::thread::Builder::new().name("engine").spawn(move || Engine::new(ours, decoder, sound).run()).ok()?;
    Some(shared)
}

/// What is open: kept to open it again in a restarted decoder.
struct Open {
    file: file::Client,
    info: Arc<MediaInfo>,
    /// The pool's VMOs and the engine's own view of it (to tidy up after a
    /// decoder that failed).
    pool: Option<(Vmo, Vmo, FramePool)>,
    ring: Option<Vmo>,
    playing: bool,
}

/// A call to the decoder that did not succeed.
enum Failure {
    /// The decoder answered that it could not.
    Refused(MediaError),
    /// It did not answer as it should (it crashed, hangs or broke the
    /// protocol): it was ended, and this is why.
    Dead(String),
}

/// Why a decoder started again did not take up the video.
enum NotResumed {
    /// It stopped again, as [`Failure::Dead`] says why.
    Stopped(String),
    /// It could not be started, or it refused: the error for the window.
    Failed(String),
}

struct Engine {
    shared: Arc<Shared>,
    sound: Option<Arc<audio::Shared>>,
    decoder_path: String,
    image: Option<Image>,
    vfs: Option<vfs::Client>,
    decoder: Option<Decoder>,
    open: Option<Open>,
    output: (u32, u32),
    /// The window's size and the screen's, for the first pictures.
    stage: (u32, u32),
    screen: (u32, u32),
    restarts: VecDeque<u64>,
    /// Where the decoder last stopped (µs).
    stopped_at: Option<i64>,
    /// The decoder's heartbeat when last seen moving (or answering), and
    /// when.
    beat: (u64, u64),
    /// The generation the decoder decodes, counted up with every seek.
    generation: u32,
    /// What the window last asked the decoder to leave out (asked again of
    /// a restarted one).
    skip: SkipLevel,
    /// The decoder's memory may grow to this.
    memory_limit: u64,
    /// The last restarts were for memory.
    memory: bool,
}

impl Engine {
    fn new(shared: Arc<Shared>, decoder_path: String, sound: Option<Arc<audio::Shared>>) -> Engine {
        let total = runtime::object::system_info().map_or(0, |i| i.total_memory);
        Engine {
            shared,
            sound,
            decoder_path,
            image: None,
            vfs: None,
            decoder: None,
            open: None,
            output: (0, 0),
            stage: (0, 0),
            screen: (0, 0),
            restarts: VecDeque::new(),
            stopped_at: None,
            beat: (0, 0),
            generation: 0,
            skip: SkipLevel::Nothing,
            memory_limit: (total / MEMORY_SHARE).clamp(MIN_MEMORY_LIMIT, MAX_MEMORY_LIMIT),
            memory: false,
        }
    }

    fn set_state(&self, f: impl FnOnce(&mut EngineState)) {
        {
            let mut s = self.shared.state.lock();
            s.0 += 1;
            f(&mut s.1);
        }
        let _ = self.shared.notify.signal();
    }

    fn vfs(&mut self) -> Option<&vfs::Client> {
        if self.vfs.is_none() {
            self.vfs = proto::connect(vfs::NAME).ok().map(vfs::Client::new);
        }
        self.vfs.as_ref()
    }

    fn run(&mut self) {
        match self.spawn() {
            Ok(build) => {
                println!("decoder: {build}");
                self.set_state(|s| s.phase = EnginePhase::Idle);
            }
            Err(e) => {
                println!("decoder unavailable: {e}");
                self.set_state(|s| {
                    s.phase = EnginePhase::Missing;
                    s.error = Some(e);
                });
            }
        }
        loop {
            let mut items = Vec::with_capacity(2);
            items.push(WaitItem { handle: self.shared.wake.raw(), signals: signals::SIGNALED, ..Default::default() });
            if let Some(d) = &self.decoder {
                items.push(WaitItem { handle: d.process.raw(), signals: signals::TERMINATED, ..Default::default() });
            }
            let deadline = match (&self.decoder, &self.open) {
                (Some(_), Some(_)) => runtime::time::now_ns() + WATCH_NS,
                (Some(_), None) => runtime::time::now_ns() + IDLE_WATCH_NS,
                (None, _) => abi::DEADLINE_INFINITE,
            };
            let _ = runtime::object::wait_many(&mut items, deadline);
            let _ = self.shared.wake.clear();
            loop {
                let Some(c) = self.shared.commands.lock().pop_front() else { break };
                self.handle(c);
            }
            self.watch();
        }
    }

    /// Reads the decoder's program (once) and starts it.
    fn spawn(&mut self) -> Result<String, String> {
        self.decoder = None;
        if self.image.is_none() {
            let path = self.decoder_path.clone();
            let vfs = self.vfs().ok_or("the file system is not running")?;
            self.image = Some(Image::read(vfs, &path)?);
        }
        let Some(image) = &self.image else { return Err("the decoder could not be read".into()) };
        let d = decoder::start(image, "mediadec", CALL_TIMEOUT_NS)?;
        let build = d.build.clone();
        self.beat = (d.heartbeat.count(), runtime::time::now_ns());
        self.decoder = Some(d);
        let b = build.clone();
        self.set_state(|s| s.decoder = Some(b));
        Ok(build)
    }

    /// Why the decoder is gone or useless, after a call it did not answer
    /// as it should with `e`.
    fn why_dead(&self, e: ipc::IpcError) -> String {
        if let Some(d) = &self.decoder
            && d.process.wait(signals::TERMINATED, 0).is_ok()
        {
            return exit_reason(d.process.info().map_or(0, |i| i.exit_code));
        }
        match e {
            ipc::IpcError::Kernel(abi::Error::TimedOut) => String::from("it stopped answering"),
            ipc::IpcError::PeerClosed => String::from("it went away"),
            e => format!("it answered wrongly: {e}"),
        }
    }

    /// Calls the decoder. A decoder that does not answer as it should is
    /// ended at once.
    fn call<T>(
        &mut self,
        f: impl FnOnce(&Client) -> Result<Result<T, MediaError>, ipc::IpcError>,
    ) -> Result<T, Failure> {
        let Some(d) = &self.decoder else { return Err(Failure::Dead(String::from("it is not running"))) };
        let r = f(&d.client);
        match r {
            Ok(r) => {
                // It answered: alive now, whatever its heartbeat did while
                // it worked on the call.
                self.beat = (d.heartbeat.count(), runtime::time::now_ns());
                r.map_err(Failure::Refused)
            }
            Err(e) => {
                let why = self.why_dead(e);
                // Killed with what it started.
                self.decoder = None;
                Err(Failure::Dead(why))
            }
        }
    }

    fn handle(&mut self, c: Command) {
        match c {
            Command::Open { path, start_us, play, id, stage, screen } => {
                self.stage = stage;
                self.screen = screen;
                self.open(path, start_us, play, id)
            }
            Command::Output { width, height } => {
                self.output = (width, height);
                if self.open.as_ref().is_some_and(|o| {
                    o.pool.as_ref().is_some_and(|(_, _, p)| (p.width(), p.height()) != (width, height))
                }) && let Err(Failure::Dead(why)) = self.make_pool()
                {
                    self.restart(&why, None, false);
                }
            }
            Command::Seek { position_us, mode, id } => {
                let failed = if self.open.is_none() {
                    true
                } else {
                    match self.seek(position_us, mode) {
                        Ok(()) => false,
                        Err(Failure::Refused(e)) => {
                            println!("the decoder refused to go to {} ({e})", time(position_us));
                            true
                        }
                        Err(Failure::Dead(why)) => {
                            // Started again where the seek goes.
                            self.restart(&why, Some(position_us), false);
                            false
                        }
                    }
                };
                self.set_state(|s| {
                    s.seeked = id;
                    s.seek_failed = failed;
                });
            }
            Command::Play => {
                if let Some(o) = &mut self.open {
                    o.playing = true;
                }
                self.command(|c| c.play());
            }
            Command::Pause => {
                if let Some(o) = &mut self.open {
                    o.playing = false;
                }
                self.command(|c| c.stop());
            }
            Command::Step(n) => self.command(|c| c.step(n)),
            Command::Skip(level) => {
                self.skip = level;
                self.command(|c| c.set_skip(level));
            }
            Command::Close => {
                let had = self.open.take().is_some();
                if had && let Err(Failure::Dead(why)) = self.call(|c| c.close().map(Ok)) {
                    println!("decoder stopped ({why})");
                }
                if let Some(s) = &self.sound {
                    s.send(audio::Command::Detach);
                }
                self.set_state(|s| {
                    s.phase = EnginePhase::Idle;
                    s.path = None;
                    s.info = None;
                    s.error = None;
                });
            }
        }
    }

    /// Carries out a command of the window's for the video open: a decoder
    /// that cannot is started again.
    fn command(&mut self, f: impl FnOnce(&Client) -> Result<Result<(), MediaError>, ipc::IpcError>) {
        if self.open.is_none() {
            return;
        }
        match self.call(f) {
            Ok(()) => {}
            Err(Failure::Refused(e)) => println!("the decoder refused a request ({e})"),
            Err(Failure::Dead(why)) => self.restart(&why, None, false),
        }
    }

    /// Has the decoder go to `position_us` as a new generation, and the
    /// audio thread play that generation's sound.
    fn seek(&mut self, position_us: i64, mode: SeekMode) -> Result<(), Failure> {
        // Counted up whatever happens: a number is never used twice.
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        self.call(|c| c.seek(position_us.max(0), mode, generation))?;
        if let Some(s) = &self.sound {
            s.send(audio::Command::Flush { generation });
        }
        self.set_state(|s| s.generation = generation);
        Ok(())
    }

    fn open(&mut self, path: String, start_us: i64, play: bool, id: u32) {
        self.open = None;
        self.output = (0, 0);
        // Restarts are counted for each video.
        self.stopped_at = None;
        self.restarts.clear();
        self.memory = false;
        self.set_state(|s| {
            s.phase = EnginePhase::Opening;
            s.id = id;
            s.path = Some(path.clone());
            s.info = None;
            s.error = None;
        });
        let began = runtime::time::now_ns();
        match self.try_open(&path, start_us, play) {
            Ok(info) => {
                let ms = ((runtime::time::now_ns() - began) / 1_000_000) as u32;
                self.set_state(|s| {
                    s.phase = EnginePhase::Ready;
                    s.info = Some(info);
                    s.open_ms = ms;
                });
            }
            Err(e) => {
                self.open = None;
                self.set_state(|s| {
                    s.phase = EnginePhase::Failed;
                    s.error = Some(e);
                });
            }
        }
    }

    fn try_open(&mut self, path: &str, start_us: i64, play: bool) -> Result<Arc<MediaInfo>, String> {
        // A decoder that ended while nothing was open is started again.
        if self.decoder.as_ref().is_some_and(|d| d.process.wait(signals::TERMINATED, 0).is_ok()) {
            self.decoder = None;
        }
        if self.decoder.is_none() {
            self.spawn().map_err(|e| format!("Video playback isn't available: {e}"))?;
        }
        let vfs = self.vfs().ok_or("The file system is not running.")?;
        let (channel, _) = match vfs.open_file(path.into(), open_flags::READ) {
            Ok(Ok(r)) => r,
            Ok(Err(FsError::NotFound)) => return Err("The file is no longer there.".into()),
            Ok(Err(e)) => return Err(format!("The file could not be opened ({e}).")),
            Err(e) => return Err(format!("The file could not be opened ({e:?}).")),
        };
        let file = file::Client::new(channel);
        let failed = |f: Failure| match f {
            Failure::Refused(e) => String::from(sentence(e)),
            Failure::Dead(why) => format!("The decoder stopped while opening this video ({why})."),
        };
        // What the window asked the decoder to leave out holds for its video
        // only (the decoder keeps it for what it opens next).
        if self.skip != SkipLevel::Nothing {
            self.skip = SkipLevel::Nothing;
            self.call(|c| c.set_skip(SkipLevel::Nothing)).map_err(failed)?;
        }
        let Ok(Ok(source)) = file.duplicate() else { return Err("The file could not be read.".into()) };
        let info = Arc::new(self.open_in_decoder(source).map_err(failed)?);
        self.open = Some(Open { file, info: info.clone(), pool: None, ring: None, playing: play });
        self.make_pool().map_err(failed)?;
        self.attach_sound().map_err(failed)?;
        let mode = if start_us > 0 { SeekMode::Keyframe } else { SeekMode::Exact };
        self.seek(start_us.max(0), mode).map_err(failed)?;
        if play {
            self.call(|c| c.play()).map_err(failed)?;
        }
        Ok(info)
    }

    /// Opens `source` (a channel to the file) in the decoder: what it
    /// holds, if the decoder describes it within the protocol's limits.
    fn open_in_decoder(&mut self, source: Channel) -> Result<MediaInfo, Failure> {
        let options = OpenOptions { threads: 0, max_width: 0, max_height: 0, flags: 0 };
        if let Some(d) = &self.decoder {
            d.client.set_timeout(OPEN_TIMEOUT_NS);
        }
        let r = self.call(|c| c.open(source, options));
        if let Some(d) = &self.decoder {
            d.client.set_timeout(CALL_TIMEOUT_NS);
        }
        let info = r?;
        if !info.is_valid() {
            // A decoder that breaks the protocol is not believed again.
            self.decoder = None;
            return Err(Failure::Dead(String::from("it described the video wrongly")));
        }
        Ok(info)
    }

    /// Makes a frame pool at the output size and gives it to the decoder
    /// and the window. Only a decoder that failed is an error: without the
    /// memory for a pool, the video plays without pictures.
    fn make_pool(&mut self) -> Result<(), Failure> {
        let Some(open) = &mut self.open else { return Ok(()) };
        let Some(v) = open.info.video_stream() else { return Ok(()) };
        let (w, h) = if self.output.0 > 0 {
            self.output
        } else {
            // As the window will ask: no new pool once it does.
            let (dw, dh) = v.display_size();
            let frame = layout::fit_frame(dw, dh, self.stage.0, self.stage.1);
            if frame.w > 0 {
                layout::output_size(frame, self.screen.0, self.screen.1)
            } else {
                proto::media::fit(dw, dh, 1280, 720)
            }
        };
        let slots = layout::slots(w, h, v.frame_rate.num, v.frame_rate.den);
        let Ok((pool, state, pixels)) = FramePool::create(slots, w, h) else {
            println!("no memory for {slots} pictures of {w}x{h}");
            return Ok(());
        };
        let dup = |v: &Vmo| v.0.duplicate(None).ok().map(Vmo::from_handle);
        let (Some(state2), Some(pixels2)) = (dup(&state), dup(&pixels)) else { return Ok(()) };
        let Ok(view) = FramePool::map(state2, pixels2) else { return Ok(()) };
        let (Some(theirs_state), Some(theirs_pixels)) = (dup(&state), dup(&pixels)) else { return Ok(()) };
        match self.set_video(theirs_state, theirs_pixels, w, h) {
            Ok(()) => {}
            Err(Failure::Refused(e)) => {
                println!("the decoder refused a pool of {w}x{h}: {e}");
                return Ok(());
            }
            Err(dead) => return Err(dead),
        }
        if let Some(open) = &mut self.open {
            open.pool = Some((state, pixels, view));
        }
        println!("frame pool {slots} x {w}x{h} = {} KiB", FramePool::pixel_bytes(slots, w, h) / 1024);
        *self.shared.pool.lock() = Some(pool);
        self.set_state(|s| s.output = (w, h));
        Ok(())
    }

    /// Gives the decoder the pool of `w` x `h` pictures whose VMOs are
    /// `state` and `pixels` (handles for it).
    fn set_video(&mut self, state: Vmo, pixels: Vmo, w: u32, h: u32) -> Result<(), Failure> {
        let ev = |e: &Event| e.0.duplicate(None).map(Event::from_handle);
        let spec = VideoSpec { width: w, height: h, flags: 0 };
        let (Ok(r), Ok(k)) = (ev(&self.shared.video_ready), ev(&self.shared.decoder_wake)) else {
            return Err(Failure::Refused(MediaError::NoMemory));
        };
        self.call(|c| c.set_video(spec, state, pixels, r, k))
    }

    /// Gives the decoder a sound ring and the audio thread its other end,
    /// for a video with sound. Sound the decoder cannot take is left out.
    fn attach_sound(&mut self) -> Result<(), Failure> {
        let Some(sound) = self.sound.clone() else { return Ok(()) };
        let Some(open) = &self.open else { return Ok(()) };
        if open.info.selected_audio.is_none() {
            sound.send(audio::Command::Detach);
            return Ok(());
        }
        // The output's rate, or the usual one until the audio service is
        // reached (it then resamples).
        let rate = sound.rate();
        let Ok((ring, vmo)) = PcmRing::create(RING_BYTES, rate) else {
            println!("no memory for the sound");
            sound.send(audio::Command::Detach);
            return Ok(());
        };
        let Ok(ready) = Event::create() else {
            sound.send(audio::Command::Detach);
            return Ok(());
        };
        match self.set_audio(&vmo, rate, &ready) {
            Ok(()) => {}
            Err(Failure::Refused(e)) => {
                println!("the decoder refused the sound: {e}");
                sound.send(audio::Command::Detach);
                return Ok(());
            }
            Err(dead) => return Err(dead),
        }
        let Ok(wake) = self.shared.decoder_wake.0.duplicate(None).map(Event::from_handle) else {
            sound.send(audio::Command::Detach);
            return Ok(());
        };
        sound.send(audio::Command::Attach { ring, ready, wake });
        if let Some(open) = &mut self.open {
            open.ring = Some(vmo);
        }
        Ok(())
    }

    fn set_audio(&mut self, ring: &Vmo, rate: u32, ready: &Event) -> Result<(), Failure> {
        let r = ring.0.duplicate(None).map(Vmo::from_handle);
        let ev = ready.0.duplicate(None).map(Event::from_handle);
        let wake = self.shared.decoder_wake.0.duplicate(None).map(Event::from_handle);
        let (Ok(r), Ok(ev), Ok(wake)) = (r, ev, wake) else { return Err(Failure::Refused(MediaError::NoMemory)) };
        self.call(|c| c.set_audio(AudioSpec { rate }, r, ev, wake))
    }

    /// Looks at the decoder: one that ended, takes too much memory or hangs
    /// is ended, and started again if a video is open.
    fn watch(&mut self) {
        let Some(d) = &self.decoder else { return };
        let now = runtime::time::now_ns();
        let mut why = None;
        let mut memory = false;
        if d.process.wait(signals::TERMINATED, 0).is_ok() {
            why = Some(exit_reason(d.process.info().map_or(0, |i| i.exit_code)));
        } else if d.process.info().is_ok_and(|i| i.memory_bytes > self.memory_limit) {
            why = Some(String::from("it used too much memory"));
            memory = true;
        } else {
            // It beats at least every second, working or not: a heartbeat
            // that stops means it is stuck.
            let beat = d.heartbeat.count();
            if beat != self.beat.0 {
                self.beat = (beat, now);
            } else if now.saturating_sub(self.beat.1) > HANG_NS {
                why = Some(String::from("it stopped working"));
            }
        }
        let Some(why) = why else { return };
        if self.open.is_some() {
            self.restart(&why, None, memory);
        } else {
            println!("decoder stopped ({why})");
            self.decoder = None;
        }
    }

    /// Starts the decoder again: where `at_us` says (a seek it died on), or
    /// where the picture on the screen is. `memory`: it stopped for taking
    /// too much memory.
    ///
    /// One that stops again before it has taken up the video (it can hang
    /// or crash as soon as it decodes, before it has answered the restart's
    /// last calls) stopped where it was going, and is started again as one
    /// that stopped there later would be: whatever the timing, twice at one
    /// place goes on further along, and one that keeps stopping is given up.
    fn restart(&mut self, why: &str, at_us: Option<i64>, memory: bool) {
        let mut again = self.restart_once(why, at_us, memory);
        while let Some((why, at_us)) = again {
            again = self.restart_once(&why, Some(at_us), false);
        }
    }

    /// Starts the decoder again once (see [`Engine::restart`]): if it
    /// stopped again before it had taken up the video, why, and where it
    /// was going.
    fn restart_once(&mut self, why: &str, at_us: Option<i64>, memory: bool) -> Option<(String, i64)> {
        let shown = at_us.unwrap_or_else(|| self.shared.shown_us.load(Ordering::Acquire));
        println!("decoder stopped ({why}) at {}: restarting", time(shown));
        let began = runtime::time::now_ns();
        self.decoder = None;
        self.memory = memory;
        while self.restarts.front().is_some_and(|&t| began - t > 60_000_000_000) {
            self.restarts.pop_front();
        }
        // Nothing open: started again when needed.
        self.open.as_ref()?;
        if self.restarts.len() >= MAX_RESTARTS {
            println!("decoder keeps stopping: given up");
            self.open = None;
            let error = if self.memory {
                "There isn't enough memory to play this video."
            } else {
                "The decoder keeps stopping on this video."
            };
            self.set_state(|s| {
                s.phase = EnginePhase::Failed;
                s.error = Some(error.into());
            });
            return None;
        }
        self.restarts.push_back(began);
        self.set_state(|s| {
            s.phase = EnginePhase::Restarting;
            s.restarts += 1;
        });
        let mut resume = shown;
        if self.stopped_at.is_some_and(|at| shown.abs_diff(at) < SAME_SPOT_US as u64) {
            // Something there breaks it: on from further along.
            let end = self.open.as_ref().and_then(|o| o.info.duration_us).unwrap_or(i64::MAX);
            resume = shown.saturating_add(SKIP_US).min(end);
            println!("decoder stopped twice at the same place: going on from {}", time(resume));
        }
        self.stopped_at = Some(shown);
        match self.resume_after_restart(resume) {
            Ok(()) => {
                println!("decoder restarted in {} ms", (runtime::time::now_ns() - began) / 1_000_000);
                self.set_state(|s| s.phase = EnginePhase::Ready);
                None
            }
            Err(NotResumed::Stopped(why)) => Some((why, resume)),
            Err(NotResumed::Failed(e)) => {
                println!("decoder could not be restarted: {e}");
                self.open = None;
                self.decoder = None;
                self.set_state(|s| {
                    s.phase = EnginePhase::Failed;
                    s.error = Some(e);
                });
                None
            }
        }
    }

    fn resume_after_restart(&mut self, shown_us: i64) -> Result<(), NotResumed> {
        self.spawn().map_err(NotResumed::Failed)?;
        let failed = |f: Failure| match f {
            Failure::Refused(e) => NotResumed::Failed(String::from(sentence(e))),
            Failure::Dead(why) => NotResumed::Stopped(why),
        };
        let open = self.open.as_ref().ok_or_else(|| NotResumed::Failed(String::from("nothing open")))?;
        // What the failed decoder was writing or had published is free
        // again, its pictures' end forgotten; the picture the window holds
        // stays on the screen.
        if let Some((_, _, view)) = &open.pool {
            let held = (0..view.slots()).find(|&s| view.state(s) == media::pool::state::HELD);
            view.reset(held);
        }
        let (playing, pool_size) = (open.playing, open.pool.as_ref().map(|(_, _, v)| (v.width(), v.height())));
        let Ok(Ok(source)) = open.file.duplicate() else {
            return Err(NotResumed::Failed("The file could not be read.".into()));
        };
        let info = self.open_in_decoder(source).map_err(failed)?;
        if let Some(open) = &mut self.open {
            open.info = Arc::new(info);
        }
        if let Some((w, h)) = pool_size {
            let vmos = self.open.as_ref().and_then(|o| o.pool.as_ref()).map(|(s, p, _)| {
                (s.0.duplicate(None).map(Vmo::from_handle), p.0.duplicate(None).map(Vmo::from_handle))
            });
            if let Some((Ok(state), Ok(pixels))) = vmos {
                self.set_video(state, pixels, w, h).map_err(failed)?;
            }
        }
        let had_sound = self.open.as_ref().is_some_and(|o| o.ring.is_some());
        if had_sound {
            // A new ring: what the old one held went with its decoder.
            self.attach_sound().map_err(failed)?;
        }
        if self.skip != SkipLevel::Nothing {
            // What the window asked of the decoder before, asked again.
            let skip = self.skip;
            self.call(|c| c.set_skip(skip)).map_err(failed)?;
        }
        self.seek(shown_us.max(0), SeekMode::Exact).map_err(failed)?;
        if playing {
            self.call(|c| c.play()).map_err(failed)?;
        }
        Ok(())
    }
}

/// Why a decoder that ended did.
fn exit_reason(code: i64) -> String {
    if code == abi::EXIT_CODE_PANICKED { String::from("it panicked") } else { format!("exit {code}") }
}

/// `m:ss` of a time in µs.
fn time(us: i64) -> String {
    files::format::duration(us.max(0) as u64 / 1000)
}

/// A decoder's error as a sentence for people.
pub fn sentence(e: MediaError) -> &'static str {
    match e {
        MediaError::Unsupported => "This video is in a format Videos can't play.",
        MediaError::Corrupt => "This file isn't a video Videos can open, or it is damaged.",
        MediaError::Io => "The file could not be read.",
        MediaError::NoMemory => "There isn't enough memory to play this video.",
        MediaError::TooLarge => "This video is larger than Videos can show.",
        MediaError::Version => "The decoder is out of date.",
        _ => "The video could not be played.",
    }
}
