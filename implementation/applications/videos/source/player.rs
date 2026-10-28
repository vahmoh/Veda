// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The player: a video on a black stage, its pictures shown in time with
//! its sound, and the controls over it.
//!
//! What playback does (opening, seeking, playing, pausing, starting with the
//! sound, the end) is `videos_core::playback`'s; the player feeds it what
//! the engine, the audio thread and the pool say, carries out what it asks,
//! and draws.
//!
//! Every frame it works out when the frame will be seen
//! (`videos_core::pacing`) and what the clock will say then, takes the
//! newest picture due by then from the pool (`videos_core::select`), and
//! copies it to the window. It presents only when the picture or the
//! controls changed, and wakes when the next picture is due: a 24 fps video
//! is presented 24 times a second on a screen that refreshes 75 times.

use core::sync::atomic::Ordering;
use std::sync::Arc;

use files::format::duration as time;
use gfx::{Align, Bitmap, Color, Filter, Rect};
use proto::display::modifiers;
use proto::input::keys;
use proto::media::{FramePool, MediaInfo, SeekMode, pool as pool_layout};
use runtime::println;
use ui::{ButtonKind, Cursor, Font, Icon, Ui, WindowState, media};
use videos_core::layout;
use videos_core::pacing::{Pacer, Timing};
use videos_core::playback::{Ask, Effect, EngineState, Phase, Pictures, Playback, SoundAsk};
use videos_core::select::{self, Picture};
use videos_core::text;

use crate::widgets;
use crate::{audio, engine, thumbs};

/// The controls go this long after the pointer stops, while playing.
const CONTROLS_NS: u64 = 2_500_000_000;
/// The spinner only for waits longer than this.
const SPINNER_AFTER_NS: u64 = 150_000_000;
/// A new size is asked of the decoder once the window kept it this long.
const RESIZE_SETTLE_NS: u64 = 150_000_000;
/// Frames are presented this long after the pointer moved, for `ui`'s
/// widgets to animate.
const INTERACTIVE_NS: u64 = 700_000_000;
/// The "Resuming from" notice stays this long.
const RESUMED_NS: u64 = 6_000_000_000;
/// While playing with nothing due, the decoder is looked at this often
/// (it signals new pictures, but a late one is worth a look).
const POLL_NS: u64 = 100_000_000;
/// Keys move this far (µs).
const SHORT_STEP_US: i64 = 5_000_000;
const LONG_STEP_US: i64 = 10_000_000;
/// The controls' bars (px).
const TOP_BAR: i32 = 96;
const BOTTOM_BAR: i32 = 128;

/// Videos' accent, a cyan that turns cobalt.
pub const ACCENT: Color = Color::hex(0x3CC8F0);
pub const ACCENT2: Color = Color::hex(0x2563D9);

/// What the player asks of the app (things outside the player).
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    /// Back to the library.
    Library,
    /// Full screen on or off.
    FullScreen(bool),
    /// Remember where the video is.
    Remember { position_ms: u64, duration_ms: u64 },
    /// Forget where the video was left (started over).
    Forget,
    /// The volume changed (to save it).
    Volume,
    /// Open the video again (after it failed).
    Retry,
    /// Open the video again from its start (a video that cannot seek
    /// there).
    Replay,
}

pub struct Player {
    pub path: String,
    /// The open request this player shows.
    pub id: u32,
    playback: Playback,
    engine: Arc<engine::Shared>,
    /// The engine's state as last taken, and its version.
    engine_state: EngineState,
    engine_seen: u64,
    sound: Option<Arc<audio::Shared>>,
    thumbs: Option<Arc<thumbs::Shared>>,
    pool: Option<FramePool>,
    /// The picture shown (its slot tagged [`OLD`] if in `old_pool`).
    shown: Option<Picture>,
    /// The pool before a resize: its pictures still show, in their time,
    /// until the new pool has the next ones.
    old_pool: Option<FramePool>,
    pacer: Pacer,
    /// What the window last showed: the picture and the controls.
    look: u64,
    /// The frame timing last taken, and the time of the picture presented
    /// since (to see how far it was from the sound).
    timing_seen: u64,
    presented_pts: Option<i64>,
    last_activity_ns: u64,
    last_pointer: Option<(i32, i32)>,
    seek_drag: Option<f32>,
    preview: Option<(u64, i64, Bitmap)>,
    preview_asked: Option<(u64, i64)>,
    preview_id: u64,
    osd: Option<(String, u64)>,
    /// "Resuming from" this (µs) until then (ns), and the seeks made when
    /// it was put up (any seek takes it down).
    resumed: Option<(i64, u64)>,
    resumed_seeks: u32,
    pub volume: f32,
    pub muted: bool,
    /// The size asked of the decoder, and a new one waiting to settle.
    output: (u32, u32),
    pending_output: Option<((u32, u32), u64)>,
    /// The screen's size (pictures are decoded no larger).
    pub screen: (u32, u32),
    pub requests: Vec<Request>,
}

impl Player {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        path: String,
        id: u32,
        engine: Arc<engine::Shared>,
        sound: Option<Arc<audio::Shared>>,
        thumbs: Option<Arc<thumbs::Shared>>,
        volume: f32,
        muted: bool,
        screen: (u32, u32),
        now: u64,
    ) -> Player {
        let playback = Playback::new(files::path::file_stem(&path), files::path::file_name(&path), now);
        Player {
            path,
            id,
            playback,
            engine,
            engine_state: EngineState::default(),
            engine_seen: 0,
            sound,
            thumbs,
            pool: None,
            shown: None,
            old_pool: None,
            pacer: Pacer::new(),
            look: 0,
            timing_seen: 0,
            presented_pts: None,
            last_activity_ns: now,
            last_pointer: None,
            seek_drag: None,
            preview: None,
            preview_asked: None,
            preview_id: 0,
            osd: None,
            resumed: None,
            resumed_seeks: 0,
            volume,
            muted,
            output: (0, 0),
            pending_output: None,
            screen,
            requests: Vec::new(),
        }
    }

    /// Opening resumes at `at_us`: says so, with a way to start over.
    pub fn resuming(&mut self, at_us: i64, now: u64) {
        self.playback.resuming(at_us);
        self.resumed = Some((at_us, now + RESUMED_NS));
        self.resumed_seeks = self.playback.seeks();
    }

    // ---- what playback says ------------------------------------------------------

    pub fn phase(&self) -> &Phase {
        self.playback.phase()
    }

    pub fn title(&self) -> &str {
        self.playback.title()
    }

    pub fn info(&self) -> Option<&MediaInfo> {
        self.playback.info()
    }

    pub fn duration_us(&self) -> Option<i64> {
        self.playback.duration_us()
    }

    /// The position shown (µs).
    pub fn position_us(&self, now: u64) -> i64 {
        self.playback.position_us(now)
    }

    /// Playing, or about to.
    pub fn playing(&self) -> bool {
        self.playback.playing()
    }

    /// Positions can be sought.
    pub fn seekable(&self) -> bool {
        self.playback.seekable()
    }

    // ---- what the user asks ------------------------------------------------------

    pub fn play(&mut self, now: u64) {
        self.playback.play(now);
        self.carry_out();
    }

    pub fn pause(&mut self, now: u64) {
        self.playback.pause(now);
        self.carry_out();
    }

    pub fn toggle(&mut self, now: u64) {
        self.playback.toggle(now);
        self.carry_out();
    }

    /// Goes to `position_us`.
    pub fn seek_to(&mut self, position_us: i64, mode: SeekMode, now: u64) {
        self.playback.seek_to(position_us, mode, now);
        self.carry_out();
    }

    /// Plays from the start (a video that cannot seek is opened again).
    pub fn start_over(&mut self, now: u64) {
        if self.seekable() {
            self.playback.want_playing = true;
            self.seek_to(0, SeekMode::Exact, now);
        } else {
            self.requests.push(Request::Replay);
        }
    }

    fn seek_by(&mut self, delta_us: i64, now: u64) {
        self.playback.seek_by(delta_us, now);
        self.carry_out();
    }

    fn step(&mut self, forward: bool, now: u64) {
        self.playback.step(forward, now);
        self.carry_out();
    }

    pub fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, 1.0);
        self.muted = false;
        self.apply_volume();
        println!("volume {}%", (self.volume * 100.0 + 0.5) as u32);
        self.requests.push(Request::Volume);
    }

    pub fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
        self.apply_volume();
        println!("{}", if muted { "muted" } else { "unmuted" });
        self.requests.push(Request::Volume);
    }

    pub fn apply_volume(&self) {
        let gain = if self.muted { 0.0 } else { ::audio::mix::slider_to_gain(self.volume) };
        self.send_audio(audio::Command::Volume(gain));
    }

    /// Asks the app to remember where playback is.
    pub fn remember(&mut self, now: u64) {
        self.playback.remember(now);
        self.carry_out();
    }

    /// The window was minimised or shown again: no pictures meanwhile.
    pub fn set_hidden(&mut self, hidden: bool, now: u64) {
        if hidden != self.playback.hidden() {
            if !hidden {
                self.look = 0;
            }
            self.playback.set_hidden(hidden, now);
            self.carry_out();
        }
    }

    fn osd(&mut self, text: String, now: u64) {
        self.osd = Some((text, now + 800_000_000));
    }

    fn send_audio(&self, c: audio::Command) {
        if let Some(s) = &self.sound {
            s.send(c);
        }
    }

    /// Carries out what playback asks: of the engine, the audio thread, the
    /// log and the app.
    fn carry_out(&mut self) {
        for e in self.playback.take_effects() {
            match e {
                Effect::Engine(a) => self.engine.send(match a {
                    Ask::Seek { position_us, mode, id } => engine::Command::Seek { position_us, mode, id },
                    Ask::Play => engine::Command::Play,
                    Ask::Pause => engine::Command::Pause,
                    Ask::Step(n) => engine::Command::Step(n),
                    Ask::Skip(level) => engine::Command::Skip(level),
                }),
                Effect::Sound(SoundAsk::Play) => self.send_audio(audio::Command::Play),
                Effect::Sound(SoundAsk::Pause) => self.send_audio(audio::Command::Pause),
                Effect::Log(line) => println!("{line}"),
                Effect::Remember { position_ms, duration_ms } => {
                    self.requests.push(Request::Remember { position_ms, duration_ms })
                }
                Effect::Reopen => self.requests.push(Request::Replay),
            }
        }
    }

    // ---- what the engine and the pool say -------------------------------------

    /// Takes the engine's news (the video opened or failed, a new pool, a
    /// restart, a seek done), the audio thread's and the pool's.
    pub fn sync(&mut self, now: u64) {
        if let Some(st) = self.engine.state_since(&mut self.engine_seen) {
            self.engine_state = st;
        }
        if self.engine_state.id != self.id {
            return;
        }
        if let Some(pool) = self.engine.take_pool() {
            // A new pool counts its own late pictures.
            self.playback.new_pool();
            // A resize: the pictures decoded at the old size still show, so
            // that it is seamless.
            if let Some(previous) = self.pool.replace(pool) {
                if self.shown.is_some_and(|p| p.slot & OLD != 0) {
                    // Still on a picture of the pool before that one: it
                    // stays up, and the pool in between goes.
                    drop(previous);
                } else {
                    if let Some(p) = &mut self.shown {
                        p.slot |= OLD;
                        p.meta.seq = p.meta.seq.saturating_sub(CURRENT_SEQ);
                    }
                    self.old_pool = Some(previous);
                }
            }
        }
        if let Some(s) = &self.sound {
            self.playback.see_sound(s.state());
        }
        self.playback.see_pictures(self.pictures());
        self.playback.sync(&self.engine_state, now);
        self.carry_out();
    }

    /// What the pool shows.
    fn pictures(&self) -> Pictures {
        let ended = self
            .pool
            .as_ref()
            .and_then(|p| p.end().filter(|_| (0..p.slots()).all(|s| p.state(s) != pool_layout::state::READY)));
        Pictures {
            shown: self.shown.map(|p| p.meta),
            pool: self.pool.is_some(),
            ended,
            resizing: self.old_pool.is_some(),
        }
    }

    // ---- the frame ------------------------------------------------------------------

    /// Shows the newest picture due when the frame is seen (`seen_ns`); gives
    /// back the ones passed over. True if the picture shown changed, and when
    /// the next one is due (µs).
    fn pick(&mut self, seen_ns: u64, now: u64) -> (bool, Option<i64>) {
        if self.pool.is_none() {
            return (false, None);
        }
        let mut changed = false;
        let mut released = false;
        let mut next_due = None;
        // While a seek is under way, the pool is left to the decoder.
        if self.playback.takes_pictures() {
            // Both pools' pictures, the old pool's first in the decoder's
            // order.
            let mut ready = Vec::new();
            for (tag, pool) in [(OLD, &self.old_pool), (0, &self.pool)] {
                let Some(pool) = pool else { continue };
                for slot in 0..pool.slots() {
                    if pool.state(slot) == pool_layout::state::READY
                        && let Some(mut meta) = pool.snapshot(slot)
                    {
                        if tag == 0 {
                            meta.seq = meta.seq.saturating_add(CURRENT_SEQ);
                        }
                        ready.push(Picture { slot: slot | tag, meta });
                    }
                }
            }
            let generation = self.playback.generation();
            let target = self.playback.picture_time(seen_ns, now);
            let tolerance = (self.pacer.period() / 2 / 1000) as i64;
            let choice = select::choose(&ready, self.shown.as_ref(), generation, target, tolerance);
            for slot in &choice.drop {
                if ready.iter().any(|p| p.slot == *slot && p.meta.generation == generation) {
                    self.playback.count_dropped();
                }
                if let Some((pool, s)) = pool_of(&self.pool, &self.old_pool, *slot) {
                    pool.release(s);
                }
                released = true;
            }
            if let Some(p) = choice.show
                && pool_of(&self.pool, &self.old_pool, p.slot).is_some_and(|(pool, s)| pool.hold(s))
            {
                if let Some(old) = self.shown.replace(p)
                    && let Some((pool, s)) = pool_of(&self.pool, &self.old_pool, old.slot)
                {
                    pool.release(s);
                }
                released = true;
                changed = true;
                let late = p.meta.pts_us.saturating_add(p.meta.duration_us.max(1)) < target.saturating_sub(tolerance);
                self.playback.count_shown(late);
                self.engine.shown_us.store(p.meta.pts_us, Ordering::Release);
            }
            next_due = choice.next_due_us;
        }
        if released {
            let _ = self.engine.decoder_wake.signal();
        }
        // The old pool, once nothing of it is left to show, goes.
        if let Some(old) = &self.old_pool
            && self.shown.is_none_or(|p| p.slot & OLD == 0)
            && (0..old.slots()).all(|s| old.state(s) != pool_layout::state::READY)
        {
            self.old_pool = None;
        }
        if let Some(pool) = &self.pool {
            pool.set_clock(self.playback.clock(now).at(now), now);
        }
        (changed, next_due)
    }

    /// The size pictures should be decoded at for `frame`, asked of the
    /// decoder once it settles.
    fn want_output(&mut self, frame: layout::Frame, now: u64) {
        let want = layout::output_size(frame, self.screen.0, self.screen.1);
        if want == self.output || want.0 == 0 {
            self.pending_output = None;
            return;
        }
        let since = match self.pending_output {
            Some((w, t)) if w == want => t,
            _ => {
                self.pending_output = Some((want, now));
                now
            }
        };
        // The first size at once; later ones once the window stops changing.
        if self.output == (0, 0) || now - since >= RESIZE_SETTLE_NS {
            self.output = want;
            self.pending_output = None;
            self.engine.send(engine::Command::Output { width: want.0, height: want.1 });
        }
    }

    /// Notes timing and judges playback now and then.
    fn bookkeeping(&mut self, ui: &Ui, now: u64) {
        let t = ui.frame_timing();
        self.pacer.observe(Timing { presented_ns: t.presented_ns, seen_ns: t.seen_ns, period_ns: t.period_ns });
        // The frame presented last is acknowledged: how far its picture was
        // from the sound when it was seen.
        if t.seen_ns > 0 && t.presented_ns != self.timing_seen {
            self.timing_seen = t.presented_ns;
            if let Some(pts) = self.presented_pts.take() {
                self.playback.count_presented(pts, t.seen_ns, now);
            }
        }
        // Pictures the decoder dropped itself, too late to be converted:
        // dropped all the same.
        if let Some(late) = self.pool.as_ref().map(|p| p.late()) {
            self.playback.count_decoder_late(late);
        }
        self.playback.tick(now);
    }

    // ---- drawing --------------------------------------------------------------------

    /// One frame of the player; the window is all stage.
    pub fn update(&mut self, ui: &mut Ui) {
        let now = ui.now();
        self.sync(now);
        self.playback.follow_sound(now);
        self.bookkeeping(ui, now);
        let stage = ui.rect();
        let frame = self
            .info()
            .and_then(|i| i.video_stream())
            .map(|v| {
                let (w, h) = v.display_size();
                layout::fit_frame(w, h, stage.w as u32, stage.h as u32)
            })
            .unwrap_or_default();
        if frame.w > 0 {
            self.want_output(frame, now);
        }
        let seen = self.pacer.seen_if_presented(now);
        let (picture_changed, next_due) = self.pick(seen, now);
        self.playback.see_pictures(self.pictures());
        self.playback.set_hold(self.seek_drag.is_some());
        self.playback.advance(now);
        self.carry_out();
        let input = self.handle_input(ui, stage, now);
        if self.playback.seeks() != self.resumed_seeks {
            // Gone elsewhere: no longer resuming.
            self.resumed = None;
        }

        // The controls: up while not playing, for a while after the pointer
        // moved, while the pointer is on them or the seek bar is dragged.
        let playing = *self.phase() == Phase::Playing;
        let pointer_on_controls =
            ui.pointer().is_some_and(|(_, y)| y < stage.y + TOP_BAR || y > stage.bottom() - BOTTOM_BAR);
        let recent = now - self.last_activity_ns < CONTROLS_NS;
        let up = !playing || recent || pointer_on_controls || self.seek_drag.is_some();
        let alpha = ui.animate(ui.id("controls"), if up { 1.0 } else { 0.0 }, 6.0);
        let busy = self.playback.waiting()
            && now.saturating_sub(self.playback.wait_began()) > SPINNER_AFTER_NS
            && self.seek_drag.is_none();
        let osd = self.osd.as_ref().filter(|(_, until)| now < *until).map(|(t, _)| t.clone());
        let resumed = self.resumed.filter(|(_, until)| now < *until).map(|(at, _)| at);
        let position = self.position_us(now);
        let full_screen = ui.window_state() == WindowState::Fullscreen;
        let look = hash(&[
            self.shown.map_or(0, |p| p.meta.seq ^ (p.slot as u64) << 56),
            (alpha * 32.0) as u64,
            (position / 1_000_000) as u64,
            phase_code(self.phase()),
            busy as u64,
            osd.as_ref().map_or(0, |t| t.len() as u64 + 1),
            resumed.map_or(0, |at| at as u64 + 1),
            self.seek_drag.map_or(0, |f| (f * 4096.0) as u64 + 1),
            self.preview.as_ref().map_or(0, |p| p.0 + 1),
            frame.w as u64 | (frame.h as u64) << 16 | (stage.w as u64) << 32 | (stage.h as u64) << 48,
            full_screen as u64,
        ]);
        // `ui`'s widgets animate a moment after the pointer moves (hover,
        // tooltips): those frames are presented too.
        let interactive = alpha > 0.01 && now - self.last_activity_ns < INTERACTIVE_NS;
        if !(picture_changed || input || interactive || look != self.look) {
            ui.skip_present();
        } else {
            self.look = look;
            let began = runtime::time::now_ns();
            let blit = self.draw_picture(ui, stage, frame);
            let phase = self.phase().clone();
            if phase == Phase::Ended {
                // The picture dimmed under the buttons; the controls stay.
                ui.canvas.fill_rect(stage, Color::rgba(0, 0, 0, 120));
            }
            if alpha > 0.01 {
                self.draw_controls(ui, stage, alpha, position, now, full_screen);
            }
            if busy {
                let (cx, cy) = stage.center();
                ui.spinner(cx, cy, 18.0);
            }
            if let Phase::Failed(reason) = &phase {
                self.draw_failure(ui, stage, reason);
            }
            if phase == Phase::Ended {
                self.draw_ended(ui, stage);
            }
            if let Some(at) = resumed {
                let text = format!("Resuming from {}", time(at.max(0) as u64 / 1000));
                if widgets::toast(ui, stage, &text, "Start over", ACCENT) {
                    self.resumed = None;
                    self.requests.push(Request::Forget);
                    self.seek_to(0, SeekMode::Exact, now);
                }
            }
            if let Some(t) = &osd {
                widgets::osd(ui, stage, t);
            }
            let drawn = runtime::time::now_ns() - began;
            self.pacer.note_draw(drawn);
            self.playback.count_drawn(drawn, blit);
            let generation = self.playback.generation();
            self.presented_pts = self.shown.filter(|p| p.meta.generation == generation).map(|p| p.meta.pts_us);
        }
        // The pointer goes with the controls.
        if alpha < 0.01 && *self.phase() == Phase::Playing && ui.pointer().is_some() {
            ui.set_cursor(Cursor::Hidden);
        }
        self.schedule(ui, next_due, up, recent, now);
        if let Some((_, until)) = self.osd.as_ref().filter(|(_, until)| now < *until) {
            ui.repaint_at(*until);
        }
        if let Some((_, until)) = self.resumed.filter(|(_, until)| now < *until) {
            ui.repaint_at(until);
        }
        self.carry_out();
    }

    /// When to draw again: when the next picture is due, when playback
    /// waits for something, and when the controls should go.
    fn schedule(&mut self, ui: &mut Ui, next_due: Option<i64>, up: bool, recent: bool, now: u64) {
        if let Some(at) = self.playback.wake_at(now) {
            ui.repaint_at(at);
        }
        if self.playback.hidden() || *self.phase() != Phase::Playing {
            return;
        }
        match self.playback.due_ns(next_due, now) {
            Some(due) => ui.repaint_at(self.pacer.wake_for(due, now)),
            // Waiting for the decoder (it signals): look again anyway.
            None if self.playback.clock(now).running => ui.repaint_at(now + POLL_NS),
            None => {}
        }
        if up && recent && self.seek_drag.is_none() {
            ui.repaint_at(self.last_activity_ns + CONTROLS_NS);
        }
        // The time shown moves on, whatever the pictures do.
        if up {
            ui.repaint_at(now + 250_000_000);
        }
    }

    /// Copies the picture shown to the window, black around it; returns how
    /// long the copy took (ns).
    fn draw_picture(&mut self, ui: &mut Ui, stage: Rect, frame: layout::Frame) -> u64 {
        let black = Color::rgb(0, 0, 0);
        let r = Rect::new(stage.x + frame.x, stage.y + frame.y, frame.w as i32, frame.h as i32);
        let picture =
            self.shown.and_then(|p| pool_of(&self.pool, &self.old_pool, p.slot).map(|(pool, s)| (pool, s, p)));
        let Some((pool, slot, p)) = picture.filter(|_| frame.w > 0) else {
            ui.canvas.fill_rect(stage, black);
            if self.info().is_some_and(|i| i.selected_video.is_none()) {
                // Sound only.
                ui.icon(stage.centered(160, 160), Icon::Music, 96.0, Color::rgba(255, 255, 255, 60));
            }
            return 0;
        };
        for bar in bars(stage, r) {
            ui.canvas.fill_rect(bar, black);
        }
        let began = runtime::time::now_ns();
        let (w, h) = (p.meta.width.min(pool.width()) as i32, p.meta.height.min(pool.height()) as i32);
        let src = pool.slot_pixels(slot) as *const u32;
        // SAFETY: a slot the window holds: `h` rows of `stride` pixels in the
        // pool's mapping (sizes clamped to the pool's); copied, not borrowed.
        unsafe {
            if (w, h) == (r.w, r.h) {
                ui.canvas.blit_raw(src, pool.stride() as usize, w, h, r.x, r.y);
            } else {
                ui.canvas.draw_raw_scaled(src, pool.stride() as usize, w, h, r, Filter::Bilinear);
            }
        }
        runtime::time::now_ns() - began
    }

    fn draw_controls(&mut self, ui: &mut Ui, stage: Rect, alpha: f32, position: i64, now: u64, full_screen: bool) {
        let a = |v: u8| (v as f32 * alpha) as u8;
        // Shades the controls read on, whatever the picture.
        let top = Rect::new(stage.x, stage.y, stage.w, TOP_BAR);
        ui.canvas.fill_vertical_gradient(top, Color::rgba(0, 0, 0, a(170)), Color::rgba(0, 0, 0, 0));
        let bottom = Rect::new(stage.x, stage.bottom() - BOTTOM_BAR, stage.w, BOTTOM_BAR);
        ui.canvas.fill_vertical_gradient(bottom, Color::rgba(0, 0, 0, 0), Color::rgba(0, 0, 0, a(190)));
        let white = Color::rgba(255, 255, 255, a(255));
        let dim = Color::rgba(255, 255, 255, a(170));

        // The top: back to the library, the title and what the video is.
        let back = Rect::new(stage.x + 14, stage.y + 14, 104, 34);
        if widgets::ghost_button(ui, back, Icon::ChevronLeft, "Library", alpha) {
            self.requests.push(Request::Library);
        }
        let tx = stage.x + 132;
        let title = String::from(self.title());
        ui.label(Rect::new(tx, stage.y + 13, stage.w - 160, 22), &title, Font::Bold, 16.0, white, Align::Left);
        if let Some(info) = self.info() {
            let mut line = info.video_stream().map(text::picture).unwrap_or_default();
            if let Some(s) = info.audio_stream() {
                if !line.is_empty() {
                    line += "  ·  ";
                }
                line += &text::sound(s);
            }
            ui.label(Rect::new(tx, stage.y + 36, stage.w - 160, 16), &line, Font::Regular, 12.0, dim, Align::Left);
        }

        // The seek bar: key frames while it is dragged, exact on letting go.
        // A video that cannot be sought shows how far it is, untouchable.
        let duration = self.duration_us().unwrap_or(0);
        let bar = Rect::new(stage.x + 24, stage.bottom() - 88, stage.w - 48, 18);
        let fraction = if duration > 0 { position as f32 / duration as f32 } else { 0.0 };
        let seekable = self.seekable();
        if seekable {
            let r = media::seek_bar(ui, bar, fraction, ACCENT, &mut self.seek_drag);
            if let Some(f) = r.dragging
                && duration > 0
            {
                // Where the pointer moves to, at most one seek waiting.
                self.playback.scrub((f as f64 * duration as f64) as i64, now);
                self.carry_out();
            }
            if let Some(f) = r.commit
                && duration > 0
            {
                self.seek_to((f as f64 * duration as f64) as i64, SeekMode::Exact, now);
            }
            match r.hover.filter(|_| duration > 0) {
                Some(f) => self.draw_preview(ui, bar, f, duration, alpha),
                None => self.preview_asked = None,
            }
        } else {
            self.seek_drag = None;
            widgets::progress(ui, bar, fraction, ACCENT, alpha);
        }

        // The row of buttons.
        let row_y = stage.bottom() - 60;
        let cx = stage.x + 44;
        if media::play_button(ui, (cx, row_y + 18), 20, self.playing(), ACCENT, ACCENT2) {
            self.toggle(now);
        }
        let back = Rect::new(cx + 34, row_y, 36, 36);
        let forward = back.translate(42, 0);
        if seekable {
            if ui.icon_button(back, Icon::Rotate, "Back 10 seconds (J)") {
                self.seek_by(-LONG_STEP_US, now);
            }
            if ui.icon_button(forward, Icon::RotateRight, "Forward 10 seconds (L)") {
                self.seek_by(LONG_STEP_US, now);
            }
        }
        let mute = forward.translate(52, 0);
        let icon = if self.muted || self.volume <= 0.0 { Icon::Mute } else { Icon::Volume };
        if ui.icon_button(mute, icon, if self.muted { "Unmute (M)" } else { "Mute (M)" }) {
            let m = !self.muted;
            self.set_muted(m);
        }
        let slider = Rect::new(mute.right() + 6, row_y + 8, 96, 20);
        let mut v = if self.muted { 0.0 } else { self.volume };
        if ui.slider(slider, "volume", &mut v, 0.0, 1.0) {
            self.volume = v;
            self.muted = false;
            self.apply_volume();
            self.requests.push(Request::Volume);
        }
        let time = format!("{} / {}", time(position.max(0) as u64 / 1000), time(duration.max(0) as u64 / 1000));
        ui.label(Rect::new(slider.right() + 18, row_y, 160, 36), &time, Font::Regular, 13.0, white, Align::Left);
        let fs = Rect::new(stage.right() - 60, row_y, 36, 36);
        let (icon, tip) = if full_screen {
            (Icon::FullscreenExit, "Exit full screen (F)")
        } else {
            (Icon::Fullscreen, "Full screen (F)")
        };
        if ui.icon_button(fs, icon, tip) {
            self.requests.push(Request::FullScreen(!full_screen));
        }

        // Paused: a big play button in the middle.
        if *self.phase() == Phase::Paused && self.seek_drag.is_none() {
            let (cx, cy) = stage.center();
            ui.canvas.fill_circle(cx as f32, cy as f32, 40.0, Color::rgba(0, 0, 0, a(110)));
            ui.icon(Rect::new(cx - 36, cy - 40, 80, 80), Icon::Play, 34.0, white);
        }
    }

    /// The time and a picture above the seek bar where the pointer is.
    fn draw_preview(&mut self, ui: &mut Ui, bar: Rect, f: f32, duration: i64, alpha: f32) {
        let at = (f as f64 * duration as f64) as i64;
        // One preview a second at most.
        let bucket = at / 1_000_000 * 1_000_000;
        if self.preview_asked.is_none_or(|(_, b)| b != bucket)
            && let Some(t) = &self.thumbs
        {
            self.preview_id += 1;
            self.preview_asked = Some((self.preview_id, bucket));
            t.preview(self.path.clone(), bucket, self.preview_id);
        }
        let x = bar.x + (bar.w as f32 * f) as i32;
        let (pw, ph) = (thumbs::PREVIEW.0 as i32, thumbs::PREVIEW.1 as i32);
        let card = Rect::new((x - pw / 2).clamp(bar.x, (bar.right() - pw).max(bar.x)), bar.y - ph - 40, pw, ph);
        if let Some((_, _, bitmap)) = &self.preview {
            let r = Rect::new(
                card.x + (card.w - bitmap.width) / 2,
                card.bottom() - bitmap.height,
                bitmap.width,
                bitmap.height,
            );
            ui.canvas.fill_rounded_rect(r.inflate(2), 6.0, Color::rgba(255, 255, 255, (200.0 * alpha) as u8));
            ui.canvas.draw_bitmap(bitmap, r.x, r.y, (255.0 * alpha) as u8);
        }
        widgets::time_pill(ui, x, card.bottom() + 8, &time(at.max(0) as u64 / 1000), alpha);
    }

    /// A preview the worker made.
    pub fn preview_ready(&mut self, id: u64, at_us: i64, picture: thumbs::Picture) {
        if self.preview_asked.is_some_and(|(asked, _)| asked == id) || self.preview.is_none() {
            let bitmap = Bitmap::from_straight(picture.width as i32, picture.height as i32, picture.pixels);
            self.preview = Some((id, at_us, bitmap));
        }
    }

    fn draw_failure(&mut self, ui: &mut Ui, stage: Rect, reason: &str) {
        let card = stage.centered(420.min(stage.w - 32), 200);
        ui.canvas.fill_rounded_rect(card, 14.0, Color::hex(0x232329));
        ui.icon(Rect::new(card.x, card.y + 22, card.w, 40), Icon::Warning, 30.0, Color::hex(0xF5A524));
        ui.label(
            Rect::new(card.x, card.y + 70, card.w, 24),
            "Can't play this video",
            Font::Bold,
            17.0,
            Color::WHITE,
            Align::Center,
        );
        ui.paragraph(card.inset(24, 100, 24, 52), reason, 13.0, Color::hex(0xA6A6B2));
        let w = 150;
        let again = Rect::new(card.x + card.w / 2 - w - 6, card.bottom() - 46, w, 34);
        if ui.button_full(again, Some(Icon::Refresh), "Try again", ButtonKind::Secondary) {
            self.requests.push(Request::Retry);
        }
        if ui.button_full(again.translate(w + 12, 0), None, "Back to library", ButtonKind::Primary) {
            self.requests.push(Request::Library);
        }
    }

    fn draw_ended(&mut self, ui: &mut Ui, stage: Rect) {
        let (cx, cy) = stage.center();
        let replay = Rect::new(cx - 130, cy - 20, 120, 40);
        if ui.button_full(replay, Some(Icon::Repeat), "Replay", ButtonKind::Primary) {
            let now = ui.now();
            self.play(now);
        }
        if ui.button_full(replay.translate(140, 0), Some(Icon::Grid), "Library", ButtonKind::Secondary) {
            self.requests.push(Request::Library);
        }
    }

    // ---- input ------------------------------------------------------------------------

    /// Keys and the pointer on the stage; true if anything happened.
    fn handle_input(&mut self, ui: &mut Ui, stage: Rect, now: u64) -> bool {
        let mut any = !ui.input.keys.is_empty() || ui.input.scroll != (0, 0);
        if ui.input.pointer != self.last_pointer {
            self.last_pointer = ui.input.pointer;
            if ui.input.pointer.is_some() {
                self.last_activity_ns = now;
                any = true;
            }
        }
        if ui.input.pressed.iter().chain(ui.input.released.iter()).any(|p| *p) {
            self.last_activity_ns = now;
            any = true;
        }
        let seekable = self.seekable();
        for k in ui.input.keys.clone() {
            if k.modifiers & (modifiers::CTRL | modifiers::ALT | modifiers::SUPER) != 0 {
                continue;
            }
            self.last_activity_ns = now;
            match k.code {
                keys::SPACE | keys::K => self.toggle(now),
                keys::J | keys::L | keys::LEFT | keys::RIGHT if seekable => {
                    let (delta, says) = match k.code {
                        keys::J => (-LONG_STEP_US, "−10 s"),
                        keys::L => (LONG_STEP_US, "+10 s"),
                        keys::LEFT => (-SHORT_STEP_US, "−5 s"),
                        _ => (SHORT_STEP_US, "+5 s"),
                    };
                    self.seek_by(delta, now);
                    self.osd(String::from(says), now);
                }
                keys::UP | keys::DOWN => {
                    let base = if self.muted { 0.0 } else { self.volume };
                    let v = (base + if k.code == keys::UP { 0.05 } else { -0.05 }).clamp(0.0, 1.0);
                    let v = (v * 20.0 + 0.5) as u32 as f32 / 20.0;
                    self.set_volume(v);
                    self.osd(format!("Volume {}%", (v * 100.0 + 0.5) as u32), now);
                }
                keys::M => {
                    let m = !self.muted;
                    self.set_muted(m);
                    self.osd(String::from(if m { "Muted" } else { "Sound on" }), now);
                }
                keys::F | keys::F11 => {
                    let on = ui.window_state() != WindowState::Fullscreen;
                    self.requests.push(Request::FullScreen(on));
                }
                keys::ESC => {
                    if ui.window_state() == WindowState::Fullscreen {
                        self.requests.push(Request::FullScreen(false));
                    } else {
                        self.requests.push(Request::Library);
                    }
                }
                keys::HOME => self.seek_to(0, SeekMode::Exact, now),
                keys::END => {
                    if let Some(d) = self.duration_us() {
                        self.seek_to(d.saturating_sub(1), SeekMode::Exact, now);
                    }
                }
                keys::COMMA => self.step(false, now),
                keys::DOT => self.step(true, now),
                c if (keys::KEY_1..=keys::KEY_0).contains(&c) => {
                    let tenths = if c == keys::KEY_0 { 0 } else { (c - keys::KEY_1 + 1) as i64 };
                    if let Some(d) = self.duration_us() {
                        self.seek_to(d.saturating_mul(tenths) / 10, SeekMode::Exact, now);
                    }
                }
                _ => {}
            }
        }
        // The stage between the bars: a press plays or pauses, a double
        // press goes full screen (and undoes the first press's toggle); the
        // wheel is the volume.
        let video = Rect::new(stage.x, stage.y + TOP_BAR - 32, stage.w, (stage.h - TOP_BAR + 32 - BOTTOM_BAR).max(0));
        let id = ui.id("stage");
        let resp = ui.interact(id, video);
        let usable = !matches!(self.phase(), Phase::Failed(_) | Phase::Ended);
        if resp.double_clicked {
            let on = ui.window_state() != WindowState::Fullscreen;
            self.requests.push(Request::FullScreen(on));
            if usable {
                self.toggle(now);
            }
        } else if resp.pressed && usable {
            self.toggle(now);
        }
        if ui.hovered(video) && ui.input.scroll.1 != 0 {
            let base = if self.muted { 0.0 } else { self.volume };
            let v = (base + 0.05 * ui.input.scroll.1.signum() as f32).clamp(0.0, 1.0);
            self.set_volume(v);
            self.osd(format!("Volume {}%", (v * 100.0 + 0.5) as u32), now);
        }
        any
    }
}

/// Tags the slots of the old pool (during a resize) in [`Picture::slot`].
const OLD: u32 = 1 << 16;
/// Added to the current pool's sequence numbers: its pictures come after
/// the old pool's.
const CURRENT_SEQ: u64 = 1 << 48;

/// The pool a tagged slot is in, and its slot there.
fn pool_of<'a>(pool: &'a Option<FramePool>, old: &'a Option<FramePool>, slot: u32) -> Option<(&'a FramePool, u32)> {
    let p = if slot & OLD != 0 { old.as_ref() } else { pool.as_ref() };
    p.map(|p| (p, slot & !OLD))
}

/// The parts of `stage` around `r`.
fn bars(stage: Rect, r: Rect) -> Vec<Rect> {
    let mut v = Vec::with_capacity(4);
    if r.y > stage.y {
        v.push(Rect::new(stage.x, stage.y, stage.w, r.y - stage.y));
    }
    if r.bottom() < stage.bottom() {
        v.push(Rect::new(stage.x, r.bottom(), stage.w, stage.bottom() - r.bottom()));
    }
    if r.x > stage.x {
        v.push(Rect::new(stage.x, r.y, r.x - stage.x, r.h));
    }
    if r.right() < stage.right() {
        v.push(Rect::new(r.right(), r.y, stage.right() - r.right(), r.h));
    }
    v
}

fn phase_code(p: &Phase) -> u64 {
    match p {
        Phase::Opening => 1,
        Phase::Seeking => 2,
        Phase::Playing => 3,
        Phase::Paused => 4,
        Phase::Ended => 5,
        Phase::Failed(_) => 6,
        Phase::Restarting => 7,
    }
}

/// FNV-1a over the words of what the window shows.
fn hash(words: &[u64]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for w in words {
        for b in w.to_le_bytes() {
            h = (h ^ b as u64).wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}
