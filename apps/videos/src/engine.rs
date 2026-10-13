// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The engine thread: the decoder's keeper.
//!
//! It starts the decoder (`mediadec`, or a stand-in named on the command
//! line) with nothing but its control channel, opens videos with it, makes
//! the frame pool the window shows pictures from and the ring the audio
//! thread plays sound from, and carries out what the window asks (seek,
//! play, pause, a new size). It watches the decoder: one that crashes,
//! stops answering or stops working while it should is started again at
//! the picture on the screen, at most a few times a minute.
//!
//! Calls to the decoder block, so they are made here, never on the
//! window's thread.

use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicI64, AtomicU32, Ordering};

use vabi::{WaitItem, signals};
use videos_core::layout;
use vproto::fs::{FsError, file, open_flags, vfs};
use vproto::media::{
    self, AudioSpec, FramePool, MediaError, MediaInfo, OpenOptions, PcmRing, SeekMode, SkipLevel, VideoSpec,
    media::Client,
};
use vrt::object::{Event, Vmo};
use vrt::println;
use vrt::sync::Mutex;

use crate::audio;
use crate::decoder::{self, Decoder, Image};

/// The decoder, unless the command line names another.
pub const DECODER: &str = "/system/bin/mediadec";
/// How long a call may take: opening reads and probes the file.
const OPEN_TIMEOUT_NS: u64 = 15_000_000_000;
const CALL_TIMEOUT_NS: u64 = 3_000_000_000;
/// How often the decoder is looked at while a video is open.
const WATCH_NS: u64 = 250_000_000;
/// A heartbeat still for this long: hung (the decoder beats every second).
const HANG_NS: u64 = 5_000_000_000;
/// More memory than this: something is wrong with the file.
const MEMORY_LIMIT: u64 = 1536 << 20;
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
    Seek {
        position_us: i64,
        mode: SeekMode,
        generation: u32,
    },
    Play,
    Pause,
    /// Decodes this many more pictures while paused (stepping).
    Step(u32),
    Skip(SkipLevel),
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Phase {
    /// Starting the decoder.
    #[default]
    Starting,
    /// No decoder is installed.
    Missing,
    Idle,
    Opening,
    Ready,
    Failed,
    /// The decoder failed and is being started again.
    Restarting,
}

/// What the engine says.
#[derive(Debug, Clone, Default)]
pub struct State {
    pub phase: Phase,
    /// The open request this is about.
    pub id: u32,
    pub path: Option<String>,
    pub info: Option<MediaInfo>,
    /// Why it failed, as a sentence.
    pub error: Option<String>,
    /// What the decoder is built from.
    pub decoder: Option<String>,
    /// How long opening took (ms).
    pub open_ms: u32,
    pub restarts: u32,
    /// The pictures are decoded at this size.
    pub output: (u32, u32),
}

pub struct Shared {
    commands: Mutex<VecDeque<Command>>,
    wake: Event,
    /// Signalled whenever the state changes or a pool is waiting.
    pub notify: Event,
    state: Mutex<State>,
    /// A new frame pool for the window to take.
    pool: Mutex<Option<FramePool>>,
    /// The seek generation: the window counts it up when it seeks, the
    /// engine when it restarts the decoder.
    pub generation: AtomicU32,
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

    pub fn state(&self) -> State {
        self.state.lock().clone()
    }

    pub fn take_pool(&self) -> Option<FramePool> {
        self.pool.lock().take()
    }

    /// The next seek generation (the window's seeks).
    pub fn next_generation(&self) -> u32 {
        self.generation.fetch_add(1, Ordering::AcqRel).wrapping_add(1)
    }
}

/// Starts the engine thread, with `decoder` the program to start.
pub fn start(decoder: String, sound: Option<Arc<audio::Shared>>) -> Option<Arc<Shared>> {
    let shared = Arc::new(Shared {
        commands: Mutex::new(VecDeque::new()),
        wake: Event::create().ok()?,
        notify: Event::create().ok()?,
        state: Mutex::new(State::default()),
        pool: Mutex::new(None),
        generation: AtomicU32::new(0),
        shown_us: AtomicI64::new(0),
        video_ready: Event::create().ok()?,
        decoder_wake: Event::create().ok()?,
    });
    let ours = shared.clone();
    vrt::thread::Builder::new()
        .name("engine")
        .priority(vabi::priority::NORMAL + 1)
        .spawn(move || Engine::new(ours, decoder, sound).run())
        .ok()?;
    Some(shared)
}

/// What is open: kept to open it again in a restarted decoder.
struct Open {
    file: file::Client,
    info: MediaInfo,
    /// The pool's VMOs and the engine's own view of it (to watch the
    /// decoder's heartbeat and tidy up after one that failed).
    pool: Option<(Vmo, Vmo, FramePool)>,
    ring: Option<Vmo>,
    playing: bool,
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
    /// The decoder's heartbeat when last seen moving, and when.
    beat: (u64, u64),
}

impl Engine {
    fn new(shared: Arc<Shared>, decoder_path: String, sound: Option<Arc<audio::Shared>>) -> Engine {
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
        }
    }

    fn set_state(&self, f: impl FnOnce(&mut State)) {
        f(&mut self.shared.state.lock());
        let _ = self.shared.notify.signal();
    }

    fn vfs(&mut self) -> Option<&vfs::Client> {
        if self.vfs.is_none() {
            self.vfs = vproto::connect(vfs::NAME).ok().map(vfs::Client::new);
        }
        self.vfs.as_ref()
    }

    fn run(&mut self) {
        match self.spawn() {
            Ok(build) => {
                println!("decoder: {build}");
                self.set_state(|s| s.phase = Phase::Idle);
            }
            Err(e) => {
                println!("decoder unavailable: {e}");
                self.set_state(|s| {
                    s.phase = Phase::Missing;
                    s.error = Some(e);
                });
            }
        }
        loop {
            let mut items = Vec::with_capacity(3);
            items.push(WaitItem { handle: self.shared.wake.raw(), signals: signals::SIGNALED, ..Default::default() });
            if let Some(d) = &self.decoder {
                items.push(WaitItem { handle: d.process.raw(), signals: signals::TERMINATED, ..Default::default() });
            }
            let deadline = if self.open.is_some() { vrt::time::now_ns() + WATCH_NS } else { vabi::DEADLINE_INFINITE };
            let _ = vrt::object::wait_many(&mut items, deadline);
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
        if self.image.is_none() {
            let path = self.decoder_path.clone();
            let vfs = self.vfs().ok_or("the file system is not running")?;
            self.image = Some(Image::read(vfs, &path)?);
        }
        let d = decoder::start(self.image.as_ref().unwrap(), "mediadec", CALL_TIMEOUT_NS)?;
        let build = d.build.clone();
        self.decoder = Some(d);
        let b = build.clone();
        self.set_state(|s| s.decoder = Some(b));
        Ok(build)
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
                }) {
                    self.make_pool();
                }
            }
            Command::Seek { position_us, mode, generation } => {
                if let Some(d) = &self.decoder {
                    let _ = d.client.seek(position_us, mode, generation);
                }
                self.beat = (0, 0);
            }
            Command::Play => {
                if let Some(o) = &mut self.open {
                    o.playing = true;
                }
                if let Some(d) = &self.decoder {
                    let _ = d.client.play();
                }
                self.beat = (0, 0);
            }
            Command::Pause => {
                if let Some(o) = &mut self.open {
                    o.playing = false;
                }
                if let Some(d) = &self.decoder {
                    let _ = d.client.stop();
                }
            }
            Command::Step(n) => {
                if let Some(d) = &self.decoder {
                    let _ = d.client.step(n);
                }
            }
            Command::Skip(level) => {
                if let Some(d) = &self.decoder {
                    let _ = d.client.set_skip(level);
                }
            }
            Command::Close => {
                if let Some(d) = &self.decoder {
                    let _ = d.client.close();
                }
                self.open = None;
                if let Some(s) = &self.sound {
                    s.send(audio::Command::Detach);
                }
                self.set_state(|s| {
                    s.phase = Phase::Idle;
                    s.path = None;
                    s.info = None;
                    s.error = None;
                });
            }
        }
    }

    fn open(&mut self, path: String, start_us: i64, play: bool, id: u32) {
        self.open = None;
        self.output = (0, 0);
        // Restarts are counted for each video.
        self.stopped_at = None;
        self.restarts.clear();
        self.set_state(|s| {
            s.phase = Phase::Opening;
            s.id = id;
            s.path = Some(path.clone());
            s.info = None;
            s.error = None;
        });
        let began = vrt::time::now_ns();
        match self.try_open(&path, start_us, play) {
            Ok(info) => {
                let ms = ((vrt::time::now_ns() - began) / 1_000_000) as u32;
                self.set_state(|s| {
                    s.phase = Phase::Ready;
                    s.info = Some(info);
                    s.open_ms = ms;
                });
            }
            Err(e) => {
                self.open = None;
                self.set_state(|s| {
                    s.phase = Phase::Failed;
                    s.error = Some(e);
                });
            }
        }
    }

    fn try_open(&mut self, path: &str, start_us: i64, play: bool) -> Result<MediaInfo, String> {
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
        let info = self.open_in_decoder(&file)?;
        self.open = Some(Open { file, info: info.clone(), pool: None, ring: None, playing: play });
        self.make_pool();
        self.attach_sound()?;
        let generation = self.shared.next_generation();
        let mode = if start_us > 0 { SeekMode::Keyframe } else { SeekMode::Exact };
        self.call(|c| c.seek(start_us.max(0), mode, generation))?;
        if let Some(s) = &self.sound {
            s.send(audio::Command::Flush { generation });
        }
        if play {
            self.call(|c| c.play())?;
        }
        self.beat = (0, 0);
        Ok(info)
    }

    /// Opens the kept file in the decoder.
    fn open_in_decoder(&self, file: &file::Client) -> Result<MediaInfo, String> {
        let d = self.decoder.as_ref().ok_or("The decoder is not running.")?;
        let dup = match file.duplicate() {
            Ok(Ok(ch)) => ch,
            _ => return Err("The file could not be read.".into()),
        };
        d.client.set_timeout(OPEN_TIMEOUT_NS);
        let r = d.client.open(dup, OpenOptions { threads: 0, max_width: 0, max_height: 0, flags: 0 });
        d.client.set_timeout(CALL_TIMEOUT_NS);
        match r {
            Ok(Ok(info)) if info.is_valid() => Ok(info),
            Ok(Ok(_)) => Err("The decoder described the video wrongly.".into()),
            Ok(Err(e)) => Err(sentence(e).into()),
            Err(e) => Err(format!("The decoder stopped answering ({e:?}).")),
        }
    }

    /// Calls the decoder; a failure is a sentence.
    fn call<T>(&self, f: impl FnOnce(&Client) -> Result<Result<T, MediaError>, vipc::IpcError>) -> Result<T, String> {
        let d = self.decoder.as_ref().ok_or("The decoder is not running.")?;
        match f(&d.client) {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(sentence(e).into()),
            Err(e) => Err(format!("The decoder stopped answering ({e:?}).")),
        }
    }

    /// Makes a frame pool at the output size and gives it to the decoder
    /// and the window.
    fn make_pool(&mut self) {
        let Some(open) = &mut self.open else { return };
        let Some(v) = open.info.video_stream() else { return };
        let (w, h) = if self.output.0 > 0 {
            self.output
        } else {
            // As the window will ask: no new pool once it does.
            let (dw, dh) = v.display_size();
            let frame = layout::fit_frame(dw, dh, self.stage.0, self.stage.1);
            if frame.w > 0 {
                layout::output_size(frame, self.screen.0, self.screen.1)
            } else {
                vproto::media::fit(dw, dh, 1280, 720)
            }
        };
        let slots = layout::slots(w, h, v.frame_rate.num, v.frame_rate.den);
        let Ok((pool, state, pixels)) = FramePool::create(slots, w, h) else {
            println!("no memory for {slots} pictures of {w}x{h}");
            return;
        };
        let dup = |v: &Vmo| v.0.duplicate(None).ok().map(Vmo::from_handle);
        let (Some(state2), Some(pixels2)) = (dup(&state), dup(&pixels)) else { return };
        let Ok(view) = FramePool::map(state2, pixels2) else { return };
        if let Err(e) = self.set_video(&state, &pixels, w, h) {
            println!("the decoder refused a pool of {w}x{h}: {e}");
            return;
        }
        if let Some(open) = &mut self.open {
            open.pool = Some((state, pixels, view));
        }
        println!("frame pool {slots} x {w}x{h} = {} KiB", FramePool::pixel_bytes(slots, w, h) / 1024);
        *self.shared.pool.lock() = Some(pool);
        self.set_state(|s| s.output = (w, h));
    }

    fn set_video(&self, state: &Vmo, pixels: &Vmo, w: u32, h: u32) -> Result<(), String> {
        let dup = |v: &Vmo| v.0.duplicate(None).map(Vmo::from_handle).map_err(|e| format!("{e:?}"));
        let ev = |e: &Event| e.0.duplicate(None).map(Event::from_handle).map_err(|e| format!("{e:?}"));
        let spec = VideoSpec { width: w, height: h, flags: 0 };
        let (s, p, r, k) = (dup(state)?, dup(pixels)?, ev(&self.shared.video_ready)?, ev(&self.shared.decoder_wake)?);
        self.call(|c| c.set_video(spec, s, p, r, k))
    }

    /// Gives the decoder a sound ring and the audio thread its other end.
    fn attach_sound(&mut self) -> Result<(), String> {
        let Some(sound) = self.sound.clone() else { return Ok(()) };
        let Some(open) = &self.open else { return Ok(()) };
        if open.info.selected_audio.is_none() || !sound.state().available {
            sound.send(audio::Command::Detach);
            return Ok(());
        }
        let rate = sound.state().rate;
        let (ring, vmo) = PcmRing::create(RING_BYTES, rate).map_err(|e| format!("{e:?}"))?;
        let ready = Event::create().map_err(|e| format!("{e:?}"))?;
        self.set_audio(&vmo, rate, &ready)?;
        let wake = self.shared.decoder_wake.0.duplicate(None).map(Event::from_handle).map_err(|e| format!("{e:?}"))?;
        sound.send(audio::Command::Attach { ring, ready, wake });
        if let Some(open) = &mut self.open {
            open.ring = Some(vmo);
        }
        Ok(())
    }

    fn set_audio(&self, ring: &Vmo, rate: u32, ready: &Event) -> Result<(), String> {
        let r = ring.0.duplicate(None).map(Vmo::from_handle).map_err(|e| format!("{e:?}"))?;
        let ev = ready.0.duplicate(None).map(Event::from_handle).map_err(|e| format!("{e:?}"))?;
        let wake = self.shared.decoder_wake.0.duplicate(None).map(Event::from_handle).map_err(|e| format!("{e:?}"))?;
        self.call(|c| c.set_audio(AudioSpec { rate }, r, ev, wake))
    }

    /// Looks at the decoder; starts it again if it failed.
    fn watch(&mut self) {
        let Some(d) = &self.decoder else { return };
        let now = vrt::time::now_ns();
        let mut why = None;
        if d.process.wait(signals::TERMINATED, 0).is_ok() {
            let code = d.process.info().map_or(0, |i| i.exit_code);
            why = Some(if code == vabi::EXIT_CODE_PANICKED {
                String::from("it panicked")
            } else {
                format!("exit {code}")
            });
        } else if d.process.info().is_ok_and(|i| i.memory_bytes > MEMORY_LIMIT) {
            why = Some(String::from("it used too much memory"));
        } else if let Some((_, _, view)) = self.open.as_ref().and_then(|o| o.pool.as_ref()) {
            // It beats at least every second, working or not: a heartbeat
            // that stops means it is stuck.
            let beat = view.heartbeat();
            if beat != self.beat.0 || self.beat.1 == 0 {
                self.beat = (beat, now);
            } else if now - self.beat.1 > HANG_NS {
                why = Some(String::from("it stopped working"));
            }
        }
        if let Some(why) = why {
            self.restart(&why);
        }
    }

    /// Starts the decoder again where the picture on the screen is.
    fn restart(&mut self, why: &str) {
        let shown = self.shared.shown_us.load(Ordering::Acquire);
        println!("decoder stopped ({why}) at {}: restarting", time(shown));
        let began = vrt::time::now_ns();
        self.decoder = None;
        while self.restarts.front().is_some_and(|&t| began - t > 60_000_000_000) {
            self.restarts.pop_front();
        }
        if self.open.is_none() {
            // Nothing open: started again when needed.
            return;
        }
        if self.restarts.len() >= MAX_RESTARTS {
            println!("decoder keeps stopping: given up");
            self.open = None;
            self.set_state(|s| {
                s.phase = Phase::Failed;
                s.error = Some("The decoder keeps stopping on this video.".into());
            });
            return;
        }
        self.restarts.push_back(began);
        self.set_state(|s| {
            s.phase = Phase::Restarting;
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
                println!("decoder restarted in {} ms", (vrt::time::now_ns() - began) / 1_000_000);
                self.set_state(|s| s.phase = Phase::Ready);
            }
            Err(e) => {
                println!("decoder could not be restarted: {e}");
                self.open = None;
                self.set_state(|s| {
                    s.phase = Phase::Failed;
                    s.error = Some(e);
                });
            }
        }
    }

    fn resume_after_restart(&mut self, shown_us: i64) -> Result<(), String> {
        self.spawn()?;
        let open = self.open.as_ref().ok_or("nothing open")?;
        // What the failed decoder was writing or had published is free
        // again; the picture the window holds stays on the screen.
        if let Some((_, _, view)) = &open.pool {
            for s in 0..view.slots() {
                if view.state(s) != media::pool::state::HELD {
                    view.release(s);
                    view.abandon(s);
                }
            }
        }
        let info = self.open_in_decoder(&open.file)?;
        let (playing, pool_size) = (open.playing, open.pool.as_ref().map(|(_, _, v)| (v.width(), v.height())));
        if let Some(open) = &mut self.open {
            open.info = info;
        }
        if let (Some((w, h)), Some(open)) = (pool_size, &self.open)
            && let Some((state, pixels, _)) = &open.pool
        {
            self.set_video(state, pixels, w, h)?;
        }
        let had_sound = self.open.as_ref().is_some_and(|o| o.ring.is_some());
        if had_sound && let Some(sound) = self.sound.clone() {
            let ready = Event::create().map_err(|e| format!("{e:?}"))?;
            let rate = sound.state().rate;
            // A new ring: what the old one held went with its decoder.
            let (ring, vmo) = PcmRing::create(RING_BYTES, rate).map_err(|e| format!("{e:?}"))?;
            self.set_audio(&vmo, rate, &ready)?;
            let wake =
                self.shared.decoder_wake.0.duplicate(None).map(Event::from_handle).map_err(|e| format!("{e:?}"))?;
            sound.send(audio::Command::Attach { ring, ready, wake });
            if let Some(open) = &mut self.open {
                open.ring = Some(vmo);
            }
        }
        let generation = self.shared.next_generation();
        self.call(|c| c.seek(shown_us.max(0), SeekMode::Exact, generation))?;
        if let Some(s) = &self.sound {
            s.send(audio::Command::Flush { generation });
        }
        if playing {
            self.call(|c| c.play())?;
        }
        self.beat = (0, 0);
        Ok(())
    }
}

/// `m:ss` of a time in µs.
fn time(us: i64) -> String {
    videos_core::text::time(us.max(0) as u64 / 1000)
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
