// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The player: a video on a black stage, its pictures shown in time with
//! its sound, and the controls over it.
//!
//! Every frame it works out when the frame will be seen
//! (`videos_core::pacing`) and what the clock will say then (the sound's,
//! or its own for a silent video), takes the newest picture due by then
//! from the pool (`videos_core::select`), and copies it to the window. It
//! presents only when the picture or the controls changed, and wakes when
//! the next picture is due: a 24 fps video is presented 24 times a second
//! on a screen that refreshes 75 times.
//!
//! After opening or seeking it shows the first picture at once, waits a
//! moment for some sound (the *preroll*), then plays.

use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;

use vgfx::{Align, Bitmap, Color, Filter, Rect};
use videos_core::clock::Clock;
use videos_core::layout;
use videos_core::pacing::{Pacer, Timing};
use videos_core::select::{self, Picture};
use videos_core::skip::SkipControl;
use videos_core::stats::PlaybackStats;
use videos_core::text;
use vproto::display::modifiers;
use vproto::input::keys;
use vproto::media::{FramePool, MediaInfo, SeekMode, SkipLevel, pool as pool_layout};
use vrt::println;
use vui::{ButtonKind, Cursor, Font, Icon, Ui, WindowState, media};

use crate::widgets;
use crate::{audio, engine, thumbs};

/// The controls go this long after the pointer stops, while playing.
const CONTROLS_NS: u64 = 2_500_000_000;
/// The spinner only for waits longer than this.
const SPINNER_AFTER_NS: u64 = 150_000_000;
/// Sound buffered before playing starts, unless that takes longer than
/// [`PREROLL_MAX_NS`].
const PREROLL_SOUND_US: i64 = 120_000;
const PREROLL_MAX_NS: u64 = 750_000_000;
/// A new size is asked of the decoder once the window kept it this long.
const RESIZE_SETTLE_NS: u64 = 150_000_000;
/// How often the playback line is logged, and the skip controller judges.
const STATS_EVERY_NS: u64 = 10_000_000_000;
const SKIP_WINDOW_NS: u64 = 2_000_000_000;
/// While playing, where it is is remembered this often.
const REMEMBER_EVERY_NS: u64 = 60_000_000_000;
/// Frames are presented this long after the pointer moved, for vui's
/// widgets to animate.
const INTERACTIVE_NS: u64 = 700_000_000;
/// After playing starts, the picture waits at most this long for the sound
/// to be heard.
const SOUND_WAIT_NS: u64 = 400_000_000;
/// The sound's clock standing still longer than this (the sound stopped
/// coming): the picture goes on by itself.
const SOUND_STALL_NS: u64 = 1_000_000_000;
/// The "Resuming from" notice stays this long.
const RESUMED_NS: u64 = 6_000_000_000;
/// Keys move this far (µs).
const SHORT_STEP_US: i64 = 5_000_000;
const LONG_STEP_US: i64 = 10_000_000;
/// The controls' bars (px).
const TOP_BAR: i32 = 96;
const BOTTOM_BAR: i32 = 128;

/// Videos' accent, a cyan that turns cobalt.
pub const ACCENT: Color = Color::hex(0x3CC8F0);
pub const ACCENT2: Color = Color::hex(0x2563D9);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    Opening,
    /// Waiting for the first picture (and some sound) after a seek.
    Seeking,
    Playing,
    Paused,
    Ended,
    Failed(String),
    /// The decoder failed and is being started again.
    Restarting,
}

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
}

pub struct Player {
    pub path: String,
    pub title: String,
    pub info: Option<MediaInfo>,
    pub phase: Phase,
    /// Playing was asked for (it resumes after seeking or opening).
    pub want_playing: bool,
    /// The open request this player shows.
    pub id: u32,
    engine: Arc<engine::Shared>,
    sound: Option<Arc<audio::Shared>>,
    thumbs: Option<Arc<thumbs::Shared>>,
    pool: Option<FramePool>,
    /// The picture shown (its slot tagged [`OLD`] if in `old_pool`).
    shown: Option<Picture>,
    /// The pool before a resize: its pictures still show, in their time,
    /// until the new pool has the next ones.
    old_pool: Option<FramePool>,
    generation: u32,
    /// Where a seek goes (shown while seeking).
    target_us: i64,
    /// The clock of a video without sound (and before the sound is heard,
    /// and after it ended); with the sound's while it is heard.
    clock: Clock,
    /// When playing last started, and since when the sound's clock stands
    /// still.
    play_began: u64,
    sound_still_since: Option<u64>,
    /// Paused: the sound sent before still plays, and the picture follows
    /// it until it stops.
    settling: bool,
    pacer: Pacer,
    stats: PlaybackStats,
    stats_since: u64,
    skip: SkipControl,
    /// Pictures due and late since `skip_since`.
    skip_counts: (u32, u32),
    /// The pool's count of pictures the decoder dropped, as last taken.
    decoder_late: u64,
    skip_since: u64,
    /// When opening or the last seek began, and whether its first picture
    /// was shown.
    began_ns: u64,
    first_shown: bool,
    /// The first picture since opening is still to come.
    opening: bool,
    open_ms: u32,
    /// The decoder's restarts seen (a restart can finish between frames).
    restarts: u32,
    /// The last scrubbing seek's target (µs).
    scrub_us: Option<i64>,
    /// Whether the last seek was exact (said in the log when done).
    exact: bool,
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
    /// "Resuming from" this (µs) until then (ns).
    resumed: Option<(i64, u64)>,
    pub volume: f32,
    pub muted: bool,
    /// The size asked of the decoder, and a new one waiting to settle.
    output: (u32, u32),
    pending_output: Option<((u32, u32), u64)>,
    /// The screen's size (pictures are decoded no larger).
    pub screen: (u32, u32),
    hidden: bool,
    pub requests: Vec<Request>,
    /// Remember the position at this time (while playing).
    remember_at: u64,
}

impl Player {
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
        let title = vfiles::path::file_stem(&path).into();
        Player {
            path,
            title,
            info: None,
            phase: Phase::Opening,
            want_playing: true,
            id,
            engine,
            sound,
            thumbs,
            pool: None,
            shown: None,
            old_pool: None,
            generation: 0,
            target_us: 0,
            clock: Clock::still(0, 0),
            play_began: now,
            sound_still_since: None,
            settling: false,
            pacer: Pacer::new(),
            stats: PlaybackStats::new(now),
            stats_since: now,
            skip: SkipControl::new(),
            skip_counts: (0, 0),
            decoder_late: 0,
            skip_since: now,
            began_ns: now,
            first_shown: false,
            opening: true,
            open_ms: 0,
            restarts: 0,
            scrub_us: None,
            exact: true,
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
            volume,
            muted,
            output: (0, 0),
            pending_output: None,
            screen,
            hidden: false,
            requests: Vec::new(),
            remember_at: now + REMEMBER_EVERY_NS,
        }
    }

    /// Opening resumes at `at_us`: says so, with a way to start over.
    pub fn resuming(&mut self, at_us: i64, now: u64) {
        self.target_us = at_us;
        self.resumed = Some((at_us, now + RESUMED_NS));
    }

    // ---- what the engine says ---------------------------------------------------

    /// Takes the engine's news: the video opened (or failed), a new pool, a
    /// restart.
    pub fn sync(&mut self, now: u64) {
        let st = self.engine.state();
        if st.id != self.id {
            return;
        }
        if let Some(pool) = self.engine.take_pool() {
            // A new pool counts its own late pictures.
            self.decoder_late = 0;
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
        if st.restarts != self.restarts {
            // The decoder was started again, from the picture shown (or
            // further along): as after a seek, whether or not the restart
            // was seen under way.
            self.restarts = st.restarts;
            self.send_audio(audio::Command::Pause);
            self.target_us = self.shown.map_or(self.target_us, |p| p.meta.pts_us);
            self.clock = Clock::still(self.target_us, self.generation);
            self.began_ns = now;
            self.first_shown = false;
            self.exact = true;
            if !matches!(self.phase, Phase::Failed(_)) {
                self.phase = Phase::Restarting;
            }
        }
        match st.phase {
            engine::Phase::Ready => {
                if self.info.is_none()
                    && let Some(info) = st.info
                {
                    self.open_ms = st.open_ms;
                    self.opened(info);
                }
                if self.phase == Phase::Restarting {
                    // The restarted decoder seeks to the picture shown.
                    self.phase = Phase::Seeking;
                }
            }
            engine::Phase::Failed | engine::Phase::Missing => {
                let reason = st.error.unwrap_or_else(|| "The video could not be played.".into());
                if !matches!(self.phase, Phase::Failed(_)) {
                    println!("cannot play {}: {reason}", vfiles::path::file_name(&self.path));
                    self.send_audio(audio::Command::Pause);
                }
                self.phase = Phase::Failed(reason);
            }
            _ => {}
        }
        self.generation = self.engine.generation.load(Ordering::Acquire);
    }

    fn opened(&mut self, info: MediaInfo) {
        if !info.title.is_empty() {
            self.title = info.title.clone();
        }
        let picture = info.video_stream().map_or(String::from("no picture"), |v| {
            let (w, h) = v.display_size();
            let fps = text::frame_rate(v.frame_rate).unwrap_or_else(|| "?".into());
            format!("{w}x{h} {} {fps} fps", text::codec(&v.codec))
        });
        let sound = info
            .audio_stream()
            .map_or(String::from("no sound"), |a| format!("{} {} Hz", text::codec(&a.codec), a.rate));
        let duration = info.duration_us.map_or(String::from("?"), |d| text::time(d.max(0) as u64 / 1000));
        println!("playing {} ({picture}, {sound}, {duration})", self.title);
        self.info = Some(info);
        if self.phase == Phase::Opening {
            self.phase = Phase::Seeking;
        }
    }

    // ---- playback -----------------------------------------------------------------

    pub fn duration_us(&self) -> Option<i64> {
        self.info.as_ref().and_then(|i| i.duration_us)
    }

    fn has_sound(&self) -> bool {
        self.info.as_ref().is_some_and(|i| i.selected_audio.is_some())
            && self.sound.as_ref().is_some_and(|s| s.attached())
    }

    /// The sound's clock, while playing and the sound is heard (and moves,
    /// or stood still only a moment: sound that stops coming is not waited
    /// for).
    fn sound_clock(&self) -> Option<Clock> {
        if self.phase != Phase::Playing || !self.has_sound() {
            return None;
        }
        let c = self.sound.as_ref()?.clock_for(self.generation)?;
        let stalled = self.sound_still_since.is_some_and(|t| vrt::time::now_ns().saturating_sub(t) > SOUND_STALL_NS);
        (c.running || !stalled).then_some(c)
    }

    /// The clock now: the sound's while it is heard (and while it plays out
    /// after pausing), the player's own otherwise.
    fn current_clock(&self) -> Clock {
        if self.settling
            && let Some(c) = self.sound.as_ref().and_then(|s| s.clock_for(self.generation))
        {
            return c;
        }
        self.sound_clock().unwrap_or(self.clock)
    }

    /// Keeps the player's clock with the sound's, to go on from there should
    /// the sound stop; starts it if the sound is not heard in time.
    fn follow_sound(&mut self, now: u64) {
        if self.settling {
            // Paused: the picture stops where the sound does.
            match self.sound.as_ref().and_then(|s| s.clock_for(self.generation)) {
                Some(c) => {
                    self.clock = Clock::still(c.at(now), self.generation);
                    self.settling = c.running && self.phase == Phase::Paused;
                }
                None => self.settling = false,
            }
        }
        if self.phase != Phase::Playing {
            self.sound_still_since = None;
            return;
        }
        let heard = self.sound.as_ref().and_then(|s| s.clock_for(self.generation)).filter(|_| self.has_sound());
        match heard {
            Some(c) if !c.running => {
                self.sound_still_since.get_or_insert(now);
            }
            _ => self.sound_still_since = None,
        }
        match self.sound_clock() {
            Some(c) => self.clock = Clock { generation: self.generation, ..c },
            None if !self.clock.running => {
                // Not heard in time, ended or broken: the picture goes on by
                // itself.
                let waiting =
                    self.has_sound() && self.sound.as_ref().is_some_and(|s| s.live(self.generation)) && heard.is_none();
                let start = if waiting { self.play_began + SOUND_WAIT_NS } else { now };
                if now >= start {
                    self.clock = Clock::running(self.clock.base_us, start, self.generation);
                }
            }
            None => {}
        }
    }

    /// The position shown (µs).
    pub fn position_us(&self, now: u64) -> i64 {
        let at = match self.phase {
            Phase::Opening | Phase::Seeking | Phase::Restarting => self.target_us,
            Phase::Paused => self
                .shown
                .filter(|p| p.meta.generation == self.generation)
                .map_or(self.clock.base_us, |p| p.meta.pts_us),
            _ => self.current_clock().at(now),
        };
        self.duration_us().map_or(at, |d| at.min(d)).max(0)
    }

    fn send_audio(&self, c: audio::Command) {
        if let Some(s) = &self.sound {
            s.send(c);
        }
    }

    /// Plays (or resumes).
    pub fn play(&mut self, now: u64) {
        match self.phase {
            Phase::Ended => {
                self.want_playing = true;
                self.seek_to(0, SeekMode::Exact, now);
            }
            Phase::Paused => {
                self.want_playing = true;
                self.settling = false;
                let at = self.position_us(now);
                println!("resumed at {}", text::time(at as u64 / 1000));
                self.engine.send(engine::Command::Play);
                self.send_audio(audio::Command::Play);
                self.clock = Clock::running(at, now, self.generation);
                self.phase = Phase::Playing;
                self.stats_restart(now);
            }
            Phase::Opening | Phase::Seeking | Phase::Restarting => self.want_playing = true,
            _ => {}
        }
    }

    pub fn pause(&mut self, now: u64) {
        self.want_playing = false;
        if self.phase == Phase::Playing {
            let at = self.position_us(now);
            println!("paused at {}", text::time(at as u64 / 1000));
            self.engine.send(engine::Command::Pause);
            self.send_audio(audio::Command::Pause);
            let still = self.shown.filter(|p| p.meta.generation == self.generation).map_or(at, |p| p.meta.pts_us);
            self.clock = Clock::still(still, self.generation);
            self.phase = Phase::Paused;
            self.settling = self.has_sound();
            self.remember(now);
        }
    }

    pub fn toggle(&mut self, now: u64) {
        let playing = self.phase == Phase::Playing
            || (self.want_playing && matches!(self.phase, Phase::Seeking | Phase::Opening | Phase::Restarting));
        if playing { self.pause(now) } else { self.play(now) }
    }

    /// Goes to `position_us`.
    pub fn seek_to(&mut self, position_us: i64, mode: SeekMode, now: u64) {
        if let Some(at) = self.go(position_us, mode, now)
            && mode == SeekMode::Exact
        {
            println!("seek to {}", text::time(at as u64 / 1000));
        }
    }

    /// Seeks without saying so; where it goes, unless it cannot.
    fn go(&mut self, position_us: i64, mode: SeekMode, now: u64) -> Option<i64> {
        if self.info.is_none() || matches!(self.phase, Phase::Failed(_) | Phase::Restarting) {
            return None;
        }
        let end = self.duration_us().unwrap_or(i64::MAX).saturating_sub(1).max(0);
        let target = position_us.clamp(0, end);
        let generation = self.engine.next_generation();
        self.generation = generation;
        self.target_us = target;
        self.settling = false;
        self.exact = mode == SeekMode::Exact;
        if self.exact {
            self.scrub_us = None;
        }
        self.engine.send(engine::Command::Seek { position_us: target, mode, generation });
        self.send_audio(audio::Command::Pause);
        self.send_audio(audio::Command::Flush { generation });
        self.clock = Clock::still(target, generation);
        self.phase = Phase::Seeking;
        self.began_ns = now;
        self.first_shown = false;
        self.skip.reset();
        self.stats_restart(now);
        self.resumed = None;
        Some(target)
    }

    /// Moves by `delta_us`.
    pub fn seek_by(&mut self, delta_us: i64, now: u64) {
        let at = self.position_us(now);
        self.seek_to(at.saturating_add(delta_us), SeekMode::Exact, now);
    }

    /// One picture forward or back (pausing first).
    pub fn step(&mut self, forward: bool, now: u64) {
        if self.phase == Phase::Playing {
            self.pause(now);
        }
        // From the picture shown, whatever the sound still plays.
        self.settling = false;
        if self.phase != Phase::Paused {
            return;
        }
        let Some(shown) = self.shown.filter(|p| p.meta.generation == self.generation) else { return };
        if forward {
            // The next picture is due at once; the decoder makes it if it
            // has not yet.
            let to = shown.meta.pts_us.saturating_add(shown.meta.duration_us.max(1));
            if self.duration_us().is_some_and(|d| to >= d) {
                return;
            }
            self.clock = Clock::still(to, self.generation);
            self.engine.send(engine::Command::Step(1));
            println!("step to {}", text::precise(to));
        } else if shown.meta.pts_us > 0 {
            // The picture shown just before this one starts.
            self.want_playing = false;
            self.go(shown.meta.pts_us - 1, SeekMode::Exact, now);
            println!("step to {}", text::precise((shown.meta.pts_us - shown.meta.duration_us.max(1)).max(0)));
        }
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
        let gain = if self.muted { 0.0 } else { vaudio::mix::slider_to_gain(self.volume) };
        self.send_audio(audio::Command::Volume(gain));
    }

    fn osd(&mut self, text: String, now: u64) {
        self.osd = Some((text, now + 800_000_000));
    }

    /// Asks the app to remember where playback is.
    pub fn remember(&mut self, now: u64) {
        if let Some(d) = self.duration_us()
            && !matches!(self.phase, Phase::Failed(_) | Phase::Opening)
        {
            let at = if self.phase == Phase::Ended { d } else { self.position_us(now) };
            self.requests
                .push(Request::Remember { position_ms: at.max(0) as u64 / 1000, duration_ms: d.max(0) as u64 / 1000 });
        }
    }

    fn stats_restart(&mut self, now: u64) {
        self.stats = PlaybackStats::new(now);
        self.stats_since = now;
        self.skip_counts = (0, 0);
        self.skip_since = now;
    }

    // ---- the frame ------------------------------------------------------------------

    /// Shows the newest picture due when the frame is seen (`seen_ns`); gives
    /// back the ones passed over. True if the picture shown changed, and when
    /// the next one is due (µs).
    fn pick(&mut self, seen_ns: u64, now: u64) -> (bool, Option<i64>) {
        if self.pool.is_none() {
            return (false, None);
        }
        // Both pools' pictures, the old pool's first in the decoder's order.
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
        let clock = self.current_clock();
        let target = if self.phase == Phase::Playing { clock.at(seen_ns) } else { clock.at(now) };
        let tolerance = (self.pacer.period() / 2 / 1000) as i64;
        let choice = select::choose(&ready, self.shown.as_ref(), self.generation, target, tolerance);
        let mut changed = false;
        let mut released = false;
        for slot in &choice.drop {
            if self.phase == Phase::Playing
                && ready.iter().any(|p| p.slot == *slot && p.meta.generation == self.generation)
            {
                self.stats.dropped += 1;
                self.skip_counts.1 += 1;
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
            if self.phase == Phase::Playing {
                self.stats.shown += 1;
                self.skip_counts.0 += 1;
                if p.meta.pts_us.saturating_add(p.meta.duration_us.max(1)) < target.saturating_sub(tolerance) {
                    self.stats.late += 1;
                    self.skip_counts.1 += 1;
                }
            }
            self.engine.shown_us.store(p.meta.pts_us, Ordering::Release);
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
            pool.set_clock(clock.at(now), now);
        }
        (changed, choice.next_due_us)
    }

    /// Starts playing once the first picture of a seek is up and some sound
    /// is buffered; notices the end.
    fn advance(&mut self, now: u64) {
        let shown_here = self.shown.filter(|p| p.meta.generation == self.generation);
        if matches!(self.phase, Phase::Seeking | Phase::Opening) && self.info.is_some() {
            let no_picture = self.info.as_ref().is_some_and(|i| i.selected_video.is_none());
            let first = shown_here.map(|p| p.meta.pts_us);
            if first.is_some() || no_picture {
                if !self.first_shown {
                    self.first_shown = true;
                    let ms = (now - self.began_ns) / 1_000_000;
                    if core::mem::take(&mut self.opening) {
                        println!("first frame in {ms} ms (open {} ms)", self.open_ms);
                    } else if self.exact {
                        println!("seek done in {ms} ms");
                    }
                }
                let audio = self.sound.as_ref().map(|s| s.state());
                let sound_ready = !self.has_sound()
                    || audio
                        .as_ref()
                        .is_some_and(|a| a.buffered_us >= PREROLL_SOUND_US || a.ended == Some(self.generation))
                    || now - self.began_ns > PREROLL_MAX_NS;
                // Scrubbing holds playback until the seek bar is let go.
                let hold = self.seek_drag.is_some();
                if sound_ready || !self.want_playing || hold {
                    let at = first.unwrap_or(self.target_us);
                    if self.want_playing && !hold {
                        self.engine.send(engine::Command::Play);
                        self.send_audio(audio::Command::Play);
                        // With sound, the picture waits until the sound is
                        // heard (the output's latency): they start together.
                        self.clock = if self.has_sound() {
                            Clock::still(at, self.generation)
                        } else {
                            Clock::running(at, now, self.generation)
                        };
                        self.play_began = now;
                        self.phase = Phase::Playing;
                        self.stats_restart(now);
                    } else {
                        self.engine.send(engine::Command::Pause);
                        self.clock = Clock::still(at, self.generation);
                        self.phase = Phase::Paused;
                    }
                }
            }
        }
        if self.phase == Phase::Playing && self.is_at_end(now) {
            self.phase = Phase::Ended;
            self.want_playing = false;
            println!("finished {}", self.title);
            if self.stats.shown > 0 {
                println!("playback: {}", self.stats.line(now));
            }
            self.engine.send(engine::Command::Pause);
            self.send_audio(audio::Command::Pause);
            self.remember(now);
        }
    }

    /// The decoder published the last picture of this generation, and it is
    /// the one shown.
    fn pictures_ended(&self) -> bool {
        self.pool.as_ref().is_some_and(|pool| {
            pool.end() == Some(self.generation) && (0..pool.slots()).all(|s| pool.state(s) != pool_layout::state::READY)
        })
    }

    fn is_at_end(&self, now: u64) -> bool {
        let clock = self.current_clock().at(now);
        let video_done = match (&self.pool, self.info.as_ref().and_then(|i| i.selected_video)) {
            (Some(pool), Some(_)) if self.old_pool.is_none() => {
                let shown_out = if self.hidden {
                    // Hidden, the decoder ends the pictures at once: the
                    // video ends when its time is up.
                    self.duration_us().is_none_or(|d| clock >= d)
                } else {
                    self.shown.is_none_or(|p| clock >= p.meta.pts_us.saturating_add(p.meta.duration_us))
                };
                pool.end() == Some(self.generation)
                    && (0..pool.slots()).all(|s| pool.state(s) != pool_layout::state::READY)
                    && shown_out
            }
            (_, Some(_)) => false,
            _ => true,
        };
        let sound_done =
            !self.has_sound() || self.sound.as_ref().is_some_and(|s| s.state().ended == Some(self.generation));
        video_done && sound_done
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

    /// The window was minimised or shown again: no pictures meanwhile.
    pub fn set_hidden(&mut self, hidden: bool, now: u64) {
        if hidden == self.hidden {
            return;
        }
        self.hidden = hidden;
        if hidden {
            self.engine.send(engine::Command::Skip(SkipLevel::Hidden));
        } else {
            self.engine.send(engine::Command::Skip(self.skip.level()));
            self.look = 0;
            // The decoder dropped pictures while hidden: from here again.
            if matches!(self.phase, Phase::Playing | Phase::Paused) {
                let at = self.position_us(now);
                self.go(at, SeekMode::Exact, now);
            }
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
            // (The last picture stays up past its time: not counted.)
            if let Some(pts) = self.presented_pts.take()
                && self.phase == Phase::Playing
                && !self.pictures_ended()
            {
                self.stats.offset(pts, self.current_clock().at(t.seen_ns));
                self.stats.presents += 1;
            }
        }
        // Pictures the decoder dropped itself, too late to be converted:
        // dropped all the same.
        let late = self.pool.as_ref().map_or(self.decoder_late, |p| p.late());
        let new = late.saturating_sub(self.decoder_late).min(u32::MAX as u64) as u32;
        self.decoder_late = late;
        if self.phase != Phase::Playing {
            return;
        }
        self.stats.dropped += new;
        self.skip_counts.0 += new;
        self.skip_counts.1 += new;
        if now - self.skip_since >= SKIP_WINDOW_NS {
            let (due, late) = self.skip_counts;
            self.skip_counts = (0, 0);
            self.skip_since = now;
            if let Some(level) = self.skip.window(due, late) {
                match level {
                    SkipLevel::Nothing => println!("decoder caught up"),
                    l => println!("decoder too slow: skipping {}", skip_name(l)),
                }
                if !self.hidden {
                    self.engine.send(engine::Command::Skip(level));
                }
            }
        }
        if now - self.stats_since >= STATS_EVERY_NS {
            println!("playback: {}", self.stats.line(now));
            if self.stats.out_of_sync()
                && let Some((mean, p95)) = self.stats.sync()
            {
                println!(
                    "out of sync: pictures {} ms from the sound on average, {} ms at worst (p95)",
                    mean / 1000,
                    p95 / 1000
                );
            }
            self.stats_restart(now);
        }
        if now >= self.remember_at {
            self.remember_at = now + REMEMBER_EVERY_NS;
            self.remember(now);
        }
    }

    // ---- drawing --------------------------------------------------------------------

    /// One frame of the player; the window is all stage.
    pub fn update(&mut self, ui: &mut Ui) {
        let now = ui.now();
        self.sync(now);
        self.follow_sound(now);
        self.bookkeeping(ui, now);
        let stage = ui.rect();
        let frame = self
            .info
            .as_ref()
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
        self.advance(now);
        let input = self.handle_input(ui, stage, now);

        // The controls: up while not playing, for a while after the pointer
        // moved, while the pointer is on them or the seek bar is dragged.
        let pointer_on_controls =
            ui.pointer().is_some_and(|(_, y)| y < stage.y + TOP_BAR || y > stage.bottom() - BOTTOM_BAR);
        let recent = now - self.last_activity_ns < CONTROLS_NS;
        let up = self.phase != Phase::Playing || recent || pointer_on_controls || self.seek_drag.is_some();
        let alpha = ui.animate(ui.id("controls"), if up { 1.0 } else { 0.0 }, 6.0);
        let busy = matches!(self.phase, Phase::Opening | Phase::Seeking | Phase::Restarting)
            && now - self.began_ns > SPINNER_AFTER_NS
            && self.seek_drag.is_none();
        let osd = self.osd.as_ref().filter(|(_, until)| now < *until).map(|(t, _)| t.clone());
        let resumed = self.resumed.filter(|(_, until)| now < *until).map(|(at, _)| at);
        let position = self.position_us(now);
        let full_screen = ui.window_state() == WindowState::Fullscreen;
        let look = hash(&[
            self.shown.map_or(0, |p| p.meta.seq ^ (p.slot as u64) << 56),
            (alpha * 32.0) as u64,
            (position / 1_000_000) as u64,
            phase_code(&self.phase),
            busy as u64,
            osd.as_ref().map_or(0, |t| t.len() as u64 + 1),
            resumed.map_or(0, |at| at as u64 + 1),
            self.seek_drag.map_or(0, |f| (f * 4096.0) as u64 + 1),
            self.preview.as_ref().map_or(0, |p| p.0 + 1),
            frame.w as u64 | (frame.h as u64) << 16 | (stage.w as u64) << 32 | (stage.h as u64) << 48,
            full_screen as u64,
        ]);
        // vui's widgets animate a moment after the pointer moves (hover,
        // tooltips): those frames are presented too.
        let interactive = alpha > 0.01 && now - self.last_activity_ns < INTERACTIVE_NS;
        if !(picture_changed || input || interactive || look != self.look) {
            ui.skip_present();
        } else {
            self.look = look;
            let began = vrt::time::now_ns();
            let blit = self.draw_picture(ui, stage, frame);
            if self.phase == Phase::Ended {
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
            if let Phase::Failed(reason) = self.phase.clone() {
                self.draw_failure(ui, stage, &reason);
            }
            if self.phase == Phase::Ended {
                self.draw_ended(ui, stage);
            }
            if let Some(at) = resumed {
                let text = format!("Resuming from {}", text::time(at.max(0) as u64 / 1000));
                if widgets::toast(ui, stage, &text, "Start over", ACCENT) {
                    self.resumed = None;
                    self.requests.push(Request::Forget);
                    self.seek_to(0, SeekMode::Exact, now);
                }
            }
            if let Some(t) = &osd {
                widgets::osd(ui, stage, t);
            }
            let drawn = vrt::time::now_ns() - began;
            self.pacer.note_draw(drawn);
            if self.phase == Phase::Playing {
                self.stats.drawn(drawn, blit);
            }
            self.presented_pts = self.shown.filter(|p| p.meta.generation == self.generation).map(|p| p.meta.pts_us);
        }
        // The pointer goes with the controls.
        if alpha < 0.01 && self.phase == Phase::Playing && ui.pointer().is_some() {
            ui.set_cursor(Cursor::Hidden);
        }
        self.schedule(ui, next_due, up, recent, now);
        if let Some((_, until)) = self.osd.as_ref().filter(|(_, until)| now < *until) {
            ui.repaint_at(*until);
        }
        if let Some((_, until)) = self.resumed.filter(|(_, until)| now < *until) {
            ui.repaint_at(until);
        }
    }

    /// When to draw again: when the next picture is due, and when the
    /// controls should go.
    fn schedule(&mut self, ui: &mut Ui, next_due: Option<i64>, up: bool, recent: bool, now: u64) {
        if self.hidden {
            // Only the sound plays: enough to notice the end.
            if self.phase == Phase::Playing {
                ui.repaint_at(now + 1_000_000_000);
            }
            return;
        }
        match self.phase {
            Phase::Playing => {
                let clock = self.current_clock();
                match next_due.and_then(|d| clock.when(d)) {
                    Some(due) => ui.repaint_at(self.pacer.wake_for(due, now)),
                    // Waiting for the decoder (it signals): look again anyway.
                    None => ui.repaint_at(now + 100_000_000),
                }
                if up && recent && self.seek_drag.is_none() {
                    ui.repaint_at(self.last_activity_ns + CONTROLS_NS);
                }
                // The time shown moves on, whatever the pictures do.
                if up {
                    ui.repaint_at(now + 250_000_000);
                }
            }
            Phase::Seeking | Phase::Opening | Phase::Restarting => ui.repaint_at(now + 50_000_000),
            // The picture follows the sound playing out.
            Phase::Paused if self.settling => ui.repaint_at(now + 10_000_000),
            _ => {}
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
            if self.info.as_ref().is_some_and(|i| i.selected_video.is_none()) {
                // Sound only.
                ui.icon(stage.centered(160, 160), Icon::Music, 96.0, Color::rgba(255, 255, 255, 60));
            }
            return 0;
        };
        for bar in bars(stage, r) {
            ui.canvas.fill_rect(bar, black);
        }
        let began = vrt::time::now_ns();
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
        vrt::time::now_ns() - began
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
        ui.label(Rect::new(tx, stage.y + 13, stage.w - 160, 22), &self.title, Font::Bold, 16.0, white, Align::Left);
        if let Some(info) = &self.info {
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
        let duration = self.duration_us().unwrap_or(0);
        let bar = Rect::new(stage.x + 24, stage.bottom() - 88, stage.w - 48, 18);
        let fraction = if duration > 0 { position as f32 / duration as f32 } else { 0.0 };
        let r = media::seek_bar(ui, bar, fraction, ACCENT, &mut self.seek_drag);
        if let Some(f) = r.dragging
            && duration > 0
        {
            // Where the pointer moves to, at most one seek waiting.
            let to = (f as f64 * duration as f64) as i64;
            let moved = self.scrub_us != Some(to);
            if moved && (self.phase != Phase::Seeking || (to - self.target_us).abs() > 2_000_000) {
                self.scrub_us = Some(to);
                self.go(to, SeekMode::Keyframe, now);
            } else if moved {
                self.target_us = to;
            }
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

        // The row of buttons.
        let row_y = stage.bottom() - 60;
        let cx = stage.x + 44;
        let playing = self.phase == Phase::Playing || (self.want_playing && self.phase == Phase::Seeking);
        if media::play_button(ui, (cx, row_y + 18), 20, playing, ACCENT, ACCENT2) {
            self.toggle(now);
        }
        let back = Rect::new(cx + 34, row_y, 36, 36);
        if ui.icon_button(back, Icon::Rotate, "Back 10 seconds (J)") {
            self.seek_by(-LONG_STEP_US, now);
        }
        let forward = back.translate(42, 0);
        if ui.icon_button(forward, Icon::RotateRight, "Forward 10 seconds (L)") {
            self.seek_by(LONG_STEP_US, now);
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
        let time = format!("{} / {}", text::time(position as u64 / 1000), text::time(duration.max(0) as u64 / 1000));
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
        if self.phase == Phase::Paused && self.seek_drag.is_none() {
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
        widgets::time_pill(ui, x, card.bottom() + 8, &text::time(at as u64 / 1000), alpha);
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
        for k in ui.input.keys.clone() {
            if k.modifiers & (modifiers::CTRL | modifiers::ALT | modifiers::SUPER) != 0 {
                continue;
            }
            self.last_activity_ns = now;
            match k.code {
                keys::SPACE | keys::K => self.toggle(now),
                keys::J => {
                    self.seek_by(-LONG_STEP_US, now);
                    self.osd(String::from("−10 s"), now);
                }
                keys::L => {
                    self.seek_by(LONG_STEP_US, now);
                    self.osd(String::from("+10 s"), now);
                }
                keys::LEFT => {
                    self.seek_by(-SHORT_STEP_US, now);
                    self.osd(String::from("−5 s"), now);
                }
                keys::RIGHT => {
                    self.seek_by(SHORT_STEP_US, now);
                    self.osd(String::from("+5 s"), now);
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
                        self.seek_to(d - 1, SeekMode::Exact, now);
                    }
                }
                keys::COMMA => self.step(false, now),
                keys::DOT => self.step(true, now),
                c if (keys::KEY_1..=keys::KEY_0).contains(&c) => {
                    let tenths = if c == keys::KEY_0 { 0 } else { (c - keys::KEY_1 + 1) as i64 };
                    if let Some(d) = self.duration_us() {
                        self.seek_to(d * tenths / 10, SeekMode::Exact, now);
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
        let usable = !matches!(self.phase, Phase::Failed(_) | Phase::Ended);
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

fn skip_name(l: SkipLevel) -> &'static str {
    match l {
        SkipLevel::Nothing => "nothing",
        SkipLevel::LoopFilter => "the loop filter",
        SkipLevel::NonReference => "pictures nothing refers to",
        SkipLevel::KeyframesOnly => "all but key frames",
        SkipLevel::Hidden => "every picture",
    }
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
