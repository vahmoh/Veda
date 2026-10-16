// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Videos — the Veda video player.
//!
//! * A library of the videos in `~/Videos`: a picture of each, its length,
//!   size and codec, and those left part way under "Continue watching".
//! * A player: the video on a black stage, the controls over it (they go
//!   while it plays), full screen, a seek bar with previews, and where each
//!   video was left remembered between runs (`~/.config/videos/state`).
//! * The decoding is done by `mediadec` (FFmpeg and dav1d) in a process of
//!   its own that reaches nothing but what it is sent ([`engine`]); pictures
//!   come through shared memory and are shown in time with the sound
//!   ([`player`], [`audio`]); a second decoder makes the library's pictures
//!   ([`thumbs`]).
//! * A second `videos FILE` hands the file to the running player
//!   (`vproto::instance`); [`agent`] lets the voice agent play and control
//!   videos.
//! * The decoders are processes of their own: they end with Videos.
//! * What happens is logged concisely (`playing <title> (...)`, `paused at
//!   m:ss`, `seek to m:ss`, `playback: ...` every 10 s) for the GUI tests.
//!
//! Usage: `videos [--decoder PATH] [FILE | FOLDER]`: a file plays at once,
//! and its folder (or the folder given) is the library in place of
//! `~/Videos`; `--decoder` names a stand-in for `mediadec`, such as the
//! tests' `fakedec`.
//!
//! Keyboard in the player: Space or K play/pause, J/L back/forward 10 s,
//! Left/Right 5 s, Up/Down volume, M mute, F or F11 full screen, Esc leaves
//! full screen or the player, 0-9 jump to that tenth, comma/period a picture
//! back/forward, Home/End.

mod agent;
mod audio;
mod decoder;
mod engine;
mod library;
mod player;
mod thumbs;
mod widgets;

use std::sync::Arc;

use vabi::{RawHandle, signals};
use vgfx::ShadowTemplate;
use videos_core::playback::{EnginePhase, Phase};
use videos_core::store;
use vproto::fs::vfs;
use vproto::instance::{self, Instance};
use vrt::object::Vmo;
use vrt::println;
use vui::{App, FileDialog, FileDialogResult, Ui, WindowSpec, WindowState};

use library::{Card, Entry};
use player::{Player, Request};

/// Where what Videos remembers is kept, in the home folder.
const CONFIG_DIR: &str = ".config/videos";
const STATE_FILE: &str = "state";
/// The window's size when it opens.
const WINDOW: (u32, u32) = (1100, 680);
/// Changes are saved this long after the last one (every save rewrites the
/// home folder's snapshot).
const SAVE_AFTER_NS: u64 = 3_000_000_000;

/// Where a video starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenAt {
    /// Where it was left, if it was.
    Resume,
    Start,
}

pub struct Videos {
    vfs: Option<vfs::Client>,
    pub(crate) dir: String,
    pub(crate) entries: Vec<Entry>,
    /// Counted up whenever `entries` are listed again.
    pub(crate) entries_version: u64,
    /// The library's rows as last worked out.
    pub(crate) rows: library::Rows,
    pub(crate) list_error: Option<String>,
    /// The folder was listed at least once.
    pub(crate) listed: bool,
    /// The card selected with the keyboard (its place in keyboard order).
    pub(crate) current: usize,
    pub(crate) keyboard_nav: bool,
    pub(crate) reveal_selection: bool,
    pub(crate) card_shadow: ShadowTemplate,
    pub(crate) dialog: Option<FileDialog>,
    /// Why videos cannot be played here (no decoder).
    pub(crate) unavailable: Option<String>,
    pub(crate) state: store::State,
    save_due: Option<u64>,
    pub(crate) player: Option<Player>,
    /// The size and modification time of the file open (to remember it).
    open_file: (u64, u64),
    next_id: u32,
    engine: Arc<engine::Shared>,
    sound: Option<Arc<audio::Shared>>,
    pub(crate) thumbs: Option<Arc<thumbs::Shared>>,
    pub(crate) volume: f32,
    pub(crate) muted: bool,
    pub(crate) full_screen: bool,
    /// The window's state before full screen.
    before_full_screen: WindowState,
    /// Full screen asked for outside a frame (by the agent).
    pub(crate) want_full_screen: Option<bool>,
    /// The player asked to go back to the library, or to open its video
    /// again.
    want_library: bool,
    want_retry: bool,
    /// The video open is to be opened again from its start.
    want_replay: bool,
    screen: (u32, u32),
    /// The window's size (the video's stage).
    stage: (u32, u32),
    /// The `videos` service, to which later Videos processes hand files.
    instance: Option<Instance>,
    activate: bool,
    was_focused: bool,
    /// The engine's state version last taken.
    engine_seen: u64,
}

impl Videos {
    fn new(
        engine: Arc<engine::Shared>,
        sound: Option<Arc<audio::Shared>>,
        thumbs: Option<Arc<thumbs::Shared>>,
        state: store::State,
        instance: Option<Instance>,
        folder: Option<String>,
    ) -> Videos {
        let mut v = Videos {
            vfs: vproto::connect(vfs::NAME).ok().map(vfs::Client::new),
            dir: match folder {
                Some(f) if f.len() > 1 => f.trim_end_matches('/').into(),
                Some(f) => f,
                None => library::videos_dir(),
            },
            entries: Vec::new(),
            entries_version: 0,
            rows: library::Rows::default(),
            list_error: None,
            listed: false,
            current: 0,
            keyboard_nav: false,
            reveal_selection: false,
            card_shadow: ShadowTemplate::new(10, 16),
            dialog: None,
            unavailable: None,
            volume: state.volume.min(100) as f32 / 100.0,
            muted: state.muted,
            state,
            save_due: None,
            player: None,
            open_file: (0, 0),
            next_id: 0,
            engine,
            sound,
            thumbs,
            full_screen: false,
            before_full_screen: WindowState::Normal,
            want_full_screen: None,
            want_library: false,
            want_retry: false,
            want_replay: false,
            screen: (0, 0),
            stage: WINDOW,
            instance,
            activate: false,
            was_focused: true,
            engine_seen: 0,
        };
        v.reload();
        v
    }

    /// Lists the folder again, keeping what is known of the videos still
    /// there.
    pub(crate) fn reload(&mut self) {
        self.listed = true;
        self.entries_version += 1;
        let Some(vfs) = &self.vfs else {
            self.list_error = Some("The file system is not available.".into());
            return;
        };
        match library::list(vfs, &self.dir) {
            Ok(mut fresh) => {
                // Cards not made yet are asked for again when seen.
                if let Some(t) = &self.thumbs {
                    t.clear_cards();
                }
                for e in fresh.iter_mut() {
                    if let Some(old) = self
                        .entries
                        .iter_mut()
                        .find(|o| o.path == e.path && o.size == e.size && o.modified == e.modified)
                    {
                        e.title = core::mem::take(&mut old.title);
                        e.info = old.info.take();
                        e.card = match core::mem::replace(&mut old.card, Card::Missing) {
                            Card::Requested => Card::Missing,
                            c => c,
                        };
                    }
                }
                if fresh.len() != self.entries.len() {
                    println!("library: {} videos in {}", fresh.len(), self.dir);
                }
                self.entries = fresh;
                self.current = self.current.min(self.entries.len().saturating_sub(1));
                self.list_error = None;
            }
            Err(e) => {
                println!("library: {e}");
                self.entries.clear();
                self.list_error = Some(e);
            }
        }
    }

    // ---- opening and closing --------------------------------------------------------

    pub(crate) fn open_entry(&mut self, i: usize, at: OpenAt) {
        let path = self.entries[i].path.clone();
        self.open_path(path, at);
    }

    /// Plays `path` (from where it was left, or the start).
    pub(crate) fn open_path(&mut self, path: String, at: OpenAt) {
        let now = vrt::time::now_ns();
        if let Some(p) = &mut self.player
            && p.path == path
            && !matches!(p.phase(), Phase::Failed(_))
        {
            // Already open: play it.
            if at == OpenAt::Start {
                p.start_over(now);
            } else {
                p.play(now);
            }
            return;
        }
        self.close_player();
        let (size, modified) = match self.entries.iter().find(|e| e.path == path) {
            Some(e) => (e.size, e.modified),
            None => self
                .vfs
                .as_ref()
                .and_then(|v| v.stat(path.clone()).ok())
                .and_then(|r| r.ok())
                .map_or((0, 0), |s| (s.size, s.modified)),
        };
        let resume = match at {
            OpenAt::Resume => self.state.resume_at(&path, size, modified),
            OpenAt::Start => {
                if self.state.forget(&path) {
                    self.save_soon(now);
                }
                None
            }
        };
        self.next_id = self.next_id.wrapping_add(1);
        let mut p = Player::new(
            path.clone(),
            self.next_id,
            self.engine.clone(),
            self.sound.clone(),
            self.thumbs.clone(),
            self.volume,
            self.muted,
            self.screen,
            now,
        );
        let start_us = resume.map_or(0, |ms| ms as i64 * 1000);
        if start_us > 0 {
            p.resuming(start_us, now);
        }
        p.apply_volume();
        self.player = Some(p);
        self.open_file = (size, modified);
        let (stage, screen) = (self.stage, self.screen);
        self.engine.send(engine::Command::Open { path, start_us, play: true, id: self.next_id, stage, screen });
    }

    /// Closes the video open, remembering where it was.
    fn close_player(&mut self) {
        let Some(mut p) = self.player.take() else { return };
        let now = vrt::time::now_ns();
        p.remember(now);
        self.take_requests(&mut p, None, now);
        self.engine.send(engine::Command::Close);
        self.save_now();
    }

    /// Back to the library (in the window).
    pub(crate) fn show_library(&mut self) {
        if self.full_screen {
            self.want_full_screen = Some(false);
        }
        if self.player.is_some() {
            self.close_player();
            println!("showing the library");
        }
    }

    /// Carries out what the player asked.
    fn take_requests(&mut self, p: &mut Player, mut ui: Option<&mut Ui>, now: u64) {
        for r in core::mem::take(&mut p.requests) {
            match r {
                Request::Library => self.want_library = true,
                Request::FullScreen(on) => match ui.as_deref_mut() {
                    Some(ui) => self.set_full_screen(ui, on),
                    None => self.want_full_screen = Some(on),
                },
                Request::Remember { position_ms, duration_ms } => {
                    let (size, modified) = self.open_file;
                    let now_s = vrt::time::unix_time_ns() / 1_000_000_000;
                    if self.state.remember(&p.path, position_ms, duration_ms, size, modified, now_s) {
                        self.save_soon(now);
                    }
                }
                Request::Forget => {
                    if self.state.forget(&p.path) {
                        self.save_soon(now);
                    }
                }
                Request::Volume => {
                    self.volume = p.volume;
                    self.muted = p.muted;
                    self.note_volume(now);
                }
                Request::Retry => self.want_retry = true,
                Request::Replay => self.want_replay = true,
            }
        }
    }

    // ---- volume and full screen --------------------------------------------------------

    pub(crate) fn set_volume(&mut self, v: f32) {
        let v = v.clamp(0.0, 1.0);
        match &mut self.player {
            Some(p) => p.set_volume(v),
            None => {
                self.volume = v;
                self.muted = false;
                println!("volume {}%", (v * 100.0 + 0.5) as u32);
                self.apply_volume();
            }
        }
        self.volume = v;
        self.muted = false;
        self.note_volume(vrt::time::now_ns());
    }

    pub(crate) fn set_muted(&mut self, muted: bool) {
        match &mut self.player {
            Some(p) => p.set_muted(muted),
            None => {
                println!("{}", if muted { "muted" } else { "unmuted" });
                self.muted = muted;
                self.apply_volume();
            }
        }
        self.muted = muted;
        self.note_volume(vrt::time::now_ns());
    }

    fn apply_volume(&self) {
        let gain = if self.muted { 0.0 } else { vaudio::mix::slider_to_gain(self.volume) };
        if let Some(s) = &self.sound {
            s.send(audio::Command::Volume(gain));
        }
    }

    fn note_volume(&mut self, now: u64) {
        let (volume, muted) = ((self.volume * 100.0 + 0.5) as u32, self.muted);
        if (self.state.volume, self.state.muted) != (volume, muted) {
            self.state.volume = volume;
            self.state.muted = muted;
            self.save_soon(now);
        }
    }

    fn set_full_screen(&mut self, ui: &mut Ui, on: bool) {
        let current = ui.window_state();
        if on == (current == WindowState::Fullscreen) {
            return;
        }
        if on {
            self.before_full_screen = if current == WindowState::Maximized { current } else { WindowState::Normal };
            ui.set_window_state(WindowState::Fullscreen);
        } else {
            ui.set_window_state(self.before_full_screen);
        }
        self.full_screen = on;
        println!("full screen {}", if on { "on" } else { "off" });
    }

    // ---- what is remembered --------------------------------------------------------------

    fn save_soon(&mut self, now: u64) {
        self.save_due = Some(self.save_due.unwrap_or(now + SAVE_AFTER_NS));
    }

    fn save_now(&mut self) {
        if self.save_due.take().is_some() && !save_state(self.vfs.as_ref(), &self.state) {
            println!("cannot save {}", state_path());
        }
    }

    // ---- news from the threads ---------------------------------------------------------

    fn take_thumbs(&mut self) {
        let Some(t) = self.thumbs.clone() else { return };
        for done in t.take() {
            match done {
                thumbs::Done::Card { path, info, picture } => self.card_ready(&path, info, picture),
                thumbs::Done::Preview { id, at_us, picture } => {
                    if let Some(p) = &mut self.player {
                        p.preview_ready(id, at_us, picture);
                    }
                }
                thumbs::Done::Unavailable(why) => {
                    if self.unavailable.is_none() {
                        self.unavailable = Some(why);
                    }
                }
            }
        }
    }

    /// Whether videos can be played: not until a decoder runs, once one
    /// could not be started.
    fn sync_engine(&mut self) {
        let Some(st) = self.engine.state_since(&mut self.engine_seen) else { return };
        if st.decoder.is_some() {
            self.unavailable = None;
        } else if st.phase == EnginePhase::Missing && self.unavailable.is_none() {
            self.unavailable = Some(st.error.unwrap_or_else(|| "The decoder is not installed.".into()));
        }
    }

    /// The window's title.
    fn title(&self) -> String {
        match &self.player {
            Some(p) => format!("{} \u{2014} Videos", p.title()),
            None => String::from("Videos"),
        }
    }
}

impl App for Videos {
    fn agent_info(&self) -> Option<vui::agent::AppAgentInfo> {
        Some(agent::info())
    }

    fn agent_state(&self) -> vui::agent::Value {
        agent::state(self)
    }

    fn agent_invoke(&mut self, action: &str, args: &vui::agent::Value) -> Result<vui::agent::Value, String> {
        Videos::agent_invoke(self, action, args)
    }

    fn paints_every_pixel(&self) -> bool {
        self.player.is_some()
    }

    fn close_requested(&mut self) -> bool {
        self.close_player();
        self.save_now();
        true
    }

    fn wait_handles(&self) -> Vec<(RawHandle, u32)> {
        let mut v =
            vec![(self.engine.notify.raw(), signals::SIGNALED), (self.engine.video_ready.raw(), signals::SIGNALED)];
        if let Some(s) = &self.sound {
            v.push((s.notify.raw(), signals::SIGNALED));
        }
        if let Some(t) = &self.thumbs {
            v.push((t.ready.raw(), signals::SIGNALED));
        }
        if let Some(i) = &self.instance {
            i.wait_handles(&mut v);
        }
        v
    }

    fn handle_signaled(&mut self, index: usize, _observed: u32) {
        let sound = self.sound.is_some() as usize;
        let thumbs = self.thumbs.is_some() as usize;
        match index {
            0 => {
                let _ = self.engine.notify.clear();
            }
            1 => {
                let _ = self.engine.video_ready.clear();
            }
            i if i < 2 + sound => {
                if let Some(s) = &self.sound {
                    let _ = s.notify.clear();
                }
            }
            i if i < 2 + sound + thumbs => {
                if let Some(t) = &self.thumbs {
                    let _ = t.ready.clear();
                }
            }
            _ => {
                // Another Videos process handed us files to play.
                let files = self.instance.as_mut().map(|i| i.poll(|_| true)).unwrap_or_default();
                if let Some(path) = files.into_iter().last() {
                    println!("opening {path}");
                    self.open_path(path, OpenAt::Resume);
                    self.activate = true;
                }
            }
        }
    }

    fn update(&mut self, ui: &mut Ui) {
        let now = ui.now();
        if self.screen == (0, 0) {
            self.screen = ui.ctx.display.screen_info().map_or((1920, 1080), |s| (s.width, s.height));
        }
        self.stage = (ui.width.max(1) as u32, ui.height.max(1) as u32);
        if core::mem::take(&mut self.activate) {
            ui.activate();
        }
        self.sync_engine();
        self.take_thumbs();
        let state = ui.window_state();
        self.full_screen = state == WindowState::Fullscreen;
        if let Some(on) = self.want_full_screen.take() {
            self.set_full_screen(ui, on);
        }
        // Coming back to the window picks up videos added meanwhile.
        if ui.input.focused && !self.was_focused && self.player.is_none() {
            self.reload();
        }
        self.was_focused = ui.input.focused;
        if let Some(mut p) = self.player.take() {
            p.screen = self.screen;
            p.set_hidden(state == WindowState::Minimized, now);
            p.update(ui);
            self.take_requests(&mut p, Some(ui), now);
            self.player = Some(p);
            if core::mem::take(&mut self.want_retry)
                && let Some(path) = self.player.as_ref().map(|p| p.path.clone())
            {
                // The failed player is replaced by a new one.
                self.open_path(path, OpenAt::Resume);
                ui.repaint();
            }
            if core::mem::take(&mut self.want_replay)
                && let Some(path) = self.player.as_ref().map(|p| p.path.clone())
            {
                // A video that cannot seek back to its start is opened again.
                self.close_player();
                self.open_path(path, OpenAt::Start);
                ui.repaint();
            }
            if core::mem::take(&mut self.want_library) {
                if self.full_screen {
                    self.set_full_screen(ui, false);
                }
                self.show_library();
                ui.repaint();
            }
        } else {
            self.library(ui);
            if let Some(mut d) = self.dialog.take() {
                match d.show(ui) {
                    Some(FileDialogResult::Chosen(path)) => {
                        self.open_path(path, OpenAt::Resume);
                        ui.repaint();
                    }
                    Some(FileDialogResult::Cancelled) => {}
                    None => self.dialog = Some(d),
                }
            }
        }
        let title = self.title();
        ui.set_title(&title);
        if let Some(due) = self.save_due {
            if now >= due {
                self.save_now();
            } else {
                ui.repaint_at(due);
            }
        }
    }
}

/// The file what Videos remembers is kept in.
fn state_path() -> String {
    vfiles::path::join(&vfiles::path::join(vfiles::path::HOME, CONFIG_DIR), STATE_FILE)
}

/// The state saved last time, or a new one.
fn load_state(vfs: Option<&vfs::Client>) -> store::State {
    let Some(vfs) = vfs else { return store::State::default() };
    let Some((vmo, len)) = vfs.read_file(state_path()).ok().and_then(|r| r.ok()) else {
        return store::State::default();
    };
    let mut buf = vec![0u8; len.min(1 << 20) as usize];
    if vmo.read(0, &mut buf).is_err() {
        return store::State::default();
    }
    store::State::parse(&String::from_utf8_lossy(&buf))
}

/// Writes `state`; false if it could not be.
fn save_state(vfs: Option<&vfs::Client>, state: &store::State) -> bool {
    let Some(vfs) = vfs else { return false };
    let text = state.to_text();
    let data = text.as_bytes();
    let mut dir = String::from(vfiles::path::HOME);
    for part in CONFIG_DIR.split('/').filter(|p| !p.is_empty()) {
        dir = vfiles::path::join(&dir, part);
        let _ = vfs.mkdir(dir.clone());
    }
    let Ok(vmo) = Vmo::create(data.len().max(1)) else { return false };
    if vmo.write(0, data).is_err() {
        return false;
    }
    matches!(vfs.write_file(state_path(), vmo, data.len() as u64), Ok(Ok(())))
}

const USAGE: &str = "usage: videos [--decoder PATH] [FILE | FOLDER]";

fn main() {
    std::process::exit(run());
}

/// Runs the program; its exit code.
fn run() -> i32 {
    let args = vrt::env::args();
    let mut decoder = String::from(engine::DECODER);
    let mut file = None;
    let mut it = args.iter().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--decoder" => match it.next() {
                Some(p) => decoder = p.clone(),
                None => {
                    println!("{USAGE}");
                    return 2;
                }
            },
            a if a.starts_with('-') => {
                println!("{USAGE}");
                return 2;
            }
            _ => file = Some(a.clone()),
        }
    }
    let vfs = vproto::connect(vfs::NAME).ok().map(vfs::Client::new);
    let is_dir =
        |p: &str| vfs.as_ref().and_then(|v| v.stat(p.into()).ok()).and_then(|r| r.ok()).is_some_and(|s| s.is_dir);
    // The library: the folder given, or the file's (as Photos does).
    let folder = file.take_if(|p| is_dir(p)).or_else(|| file.as_deref().map(|f| vfiles::path::parent(f).into()));
    // One player at a time: a file goes to the player that is running.
    let server = match instance::start("videos", file.as_deref()) {
        instance::Start::Serve(server) => server,
        instance::Start::HandedOver => {
            println!("handed {} to the running player", file.as_deref().unwrap_or_default());
            return 0;
        }
    };
    let state = load_state(vfs.as_ref());
    let gain = if state.muted { 0.0 } else { vaudio::mix::slider_to_gain(state.volume.min(100) as f32 / 100.0) };
    let sound = audio::start(gain);
    if sound.is_none() {
        println!("no sound: cannot start the audio thread");
    }
    let Some(engine) = engine::start(decoder.clone(), sound.clone()) else {
        println!("cannot start the engine thread");
        return 1;
    };
    let thumbs = thumbs::start(decoder);
    let mut app = Videos::new(engine, sound, thumbs, state, server, folder);
    if let Some(path) = file {
        app.open_path(path, OpenAt::Resume);
    }
    let mut spec = WindowSpec::new("Videos", WINDOW.0, WINDOW.1);
    spec.app_id = "videos".into();
    spec.min_width = 640;
    spec.min_height = 420;
    let code = vui::run(spec, app);
    // The decoders are processes of their own, which would outlive Videos.
    decoder::end_all();
    code
}
