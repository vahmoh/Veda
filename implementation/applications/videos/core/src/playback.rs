// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Playback: what the player does with what the engine, the audio thread
//! and the frame pool say, and with what the user asks. It draws nothing
//! and calls no one: the window feeds it and carries out what it asks
//! ([`Effect`]s), so that it can be tested on the host.
//!
//! * **Inputs**: the engine's state ([`EngineState`]: the video opened or
//!   failed, the decoder restarted, the seeks it carried out), the audio
//!   thread's ([`SoundState`]: the clock of the sound heard, how much of it
//!   is queued), the pool's ([`Pictures`]: the picture on the screen, the
//!   end of the pictures), the user's requests (play, pause, seek, step,
//!   hide) and the time.
//! * **Outputs**: what to ask of the engine ([`Ask`]) and of the audio
//!   thread ([`SoundAsk`]), lines for the log, where to remember the video
//!   was left; and the clock pictures are shown by ([`Playback::clock`]).
//!
//! **Generations.** The engine gives every seek the decoder carries out a
//! new generation (the player's seeks, opening, a restart), says which it
//! is now and which of the player's seeks it carried out last. While a seek
//! of the player's is under way the player takes no picture; once it is
//! done the pictures of its generation are shown, or, if the decoder could
//! not go there, playback goes on from where it was.
//!
//! **Starting.** After opening or a seek the first picture is shown at
//! once; playing waits for a little sound of the new position (the
//! *preroll*), and the picture for the sound to be heard, so that they
//! start together. A decoder that delivers no picture (the end of the
//! source, or none for [`PATIENCE_NS`]) is not waited for.
//!
//! **The clock** is the sound's while it is heard, and the system's
//! otherwise: a silent video, the sound ended or stalled, no sound output.

use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use vproto::media::{MediaInfo, SeekMode, SkipLevel, SlotMeta};

use crate::clock::Clock;
use crate::skip::SkipControl;
use crate::stats::PlaybackStats;
use crate::text;

/// Sound buffered before playing starts, unless that takes longer than
/// [`PREROLL_MAX_NS`].
pub const PREROLL_SOUND_US: i64 = 120_000;
/// The longest playing waits for that sound (from the seek).
pub const PREROLL_MAX_NS: u64 = 750_000_000;
/// After playing starts, the picture waits at most this long for the sound
/// to be heard.
pub const SOUND_WAIT_NS: u64 = 400_000_000;
/// The sound's clock standing still longer than this (the sound stopped
/// coming): the picture goes on by itself.
pub const SOUND_STALL_NS: u64 = 1_000_000_000;
/// No picture of a seek this long after the decoder went there: playback
/// goes on without it.
pub const PATIENCE_NS: u64 = 10_000_000_000;
/// How often the playback line is logged ...
pub const STATS_EVERY_NS: u64 = 10_000_000_000;
/// ... and the skip controller judges.
pub const SKIP_WINDOW_NS: u64 = 2_000_000_000;
/// While playing, where it is is remembered this often.
pub const REMEMBER_EVERY_NS: u64 = 60_000_000_000;
/// How often to look again while waiting for the decoder (it signals, but
/// not every change it makes), while the sound plays out after pausing,
/// and while hidden (only to notice the end).
const WAITING_NS: u64 = 50_000_000;
const SETTLING_NS: u64 = 10_000_000;
const HIDDEN_NS: u64 = 1_000_000_000;

/// Whether generation `a` came after `b`: generations count up and wrap,
/// so the later of two is the one less than half the numbers ahead.
pub fn is_newer(a: u32, b: u32) -> bool {
    a != b && a.wrapping_sub(b) < 1 << 31
}

/// What the engine is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EnginePhase {
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

/// What the engine (the decoder's keeper) says.
#[derive(Debug, Clone, Default)]
pub struct EngineState {
    pub phase: EnginePhase,
    /// The open request this is about.
    pub id: u32,
    pub path: Option<String>,
    pub info: Option<Arc<MediaInfo>>,
    /// Why it failed, as a sentence.
    pub error: Option<String>,
    /// What the decoder is built from.
    pub decoder: Option<String>,
    /// How long opening took (ms).
    pub open_ms: u32,
    pub restarts: u32,
    /// The pictures are decoded at this size.
    pub output: (u32, u32),
    /// The generation the decoder decodes now.
    pub generation: u32,
    /// The player's seek it carried out last (its id), and whether the
    /// decoder refused to go there.
    pub seeked: u32,
    pub seek_failed: bool,
}

/// What the audio thread says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SoundState {
    /// An output stream of the audio service is open (it goes while the
    /// service restarts).
    pub available: bool,
    /// The rate sound is played at.
    pub rate: u32,
    /// A ring is attached (a video with sound is open).
    pub attached: bool,
    /// The generation whose sound it plays.
    pub generation: u32,
    /// The clock, once sound is heard; standing still otherwise.
    pub clock: Clock,
    /// Sound of `generation` has been heard.
    pub heard: bool,
    /// Sound of `generation` queued and not yet heard (µs).
    pub queued_us: i64,
    /// The sound of this generation ended, and has all been heard.
    pub ended: Option<u32>,
    /// The ring held something no decoder writes: the decoder is broken.
    pub broken: bool,
}

impl SoundState {
    /// The clock of the sound of `generation` while it is heard: not before
    /// its first sound, nor after it ended or broke.
    pub fn clock_for(&self, generation: u32) -> Option<Clock> {
        let live = self.heard && self.clock.generation == generation && self.ended != Some(generation) && !self.broken;
        live.then_some(self.clock)
    }

    /// Whether the sound of `generation` may still be heard.
    pub fn live(&self, generation: u32) -> bool {
        !self.broken && self.ended != Some(generation)
    }

    /// Sound of `generation` queued (µs): none while another plays.
    pub fn queued_for(&self, generation: u32) -> i64 {
        if self.generation == generation { self.queued_us } else { 0 }
    }
}

/// What the frame pool shows this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Pictures {
    /// The picture on the screen (of whichever generation).
    pub shown: Option<SlotMeta>,
    /// A pool exists (pictures are decoded).
    pub pool: bool,
    /// The generation the decoder said it ended the pictures of, if
    /// nothing of the pool waits to be shown.
    pub ended: Option<u32>,
    /// Pictures of the pool before a resize may still be shown.
    pub resizing: bool,
}

/// Where playback is.
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

/// What to ask of the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ask {
    /// A seek, `id` saying which (the engine says when it is done).
    Seek {
        position_us: i64,
        mode: SeekMode,
        id: u32,
    },
    Play,
    Pause,
    /// Decodes this many more pictures while paused.
    Step(u32),
    Skip(SkipLevel),
}

/// What to ask of the audio thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoundAsk {
    Play,
    Pause,
}

/// What playback asks of the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Engine(Ask),
    Sound(SoundAsk),
    /// A line for the log.
    Log(String),
    /// Remember where the video is.
    Remember {
        position_ms: u64,
        duration_ms: u64,
    },
    /// Open the video again (to play a source that cannot seek from its
    /// start).
    Reopen,
}

/// The playback of one video.
#[derive(Debug, Clone)]
pub struct Playback {
    title: String,
    /// The file's name (for the log).
    file: String,
    phase: Phase,
    /// Playing was asked for (it resumes after seeking or opening).
    pub want_playing: bool,
    info: Option<Arc<MediaInfo>>,
    /// The generation whose pictures and sound are shown.
    generation: u32,
    /// A seek asked of the engine and not carried out yet (its id).
    pending: Option<u32>,
    /// Seeks asked so far (the last one's id).
    seeks: u32,
    /// Where a seek goes (shown while seeking), and where playback was
    /// before it (where it goes on should the decoder not go there).
    target_us: i64,
    from_us: i64,
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
    /// When opening or the last seek began, since when the decoder works
    /// on its generation, and whether its first picture was shown.
    began_ns: u64,
    waiting_since: u64,
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
    /// The seek bar is dragged: playing waits until it is let go.
    hold: bool,
    hidden: bool,
    sound: SoundState,
    pictures: Pictures,
    skip: SkipControl,
    /// Pictures due and late since `skip_since`.
    skip_counts: (u32, u32),
    skip_since: u64,
    /// The pool's count of pictures the decoder dropped, as last taken.
    decoder_late: u64,
    stats: PlaybackStats,
    stats_since: u64,
    /// Remember the position at this time (while playing).
    remember_at: u64,
    effects: Vec<Effect>,
}

/// `m:ss` of a time in µs.
fn time(us: i64) -> String {
    vfiles::format::duration(us.max(0) as u64 / 1000)
}

impl Playback {
    /// The playback of the video `title` (its file named `file`), opened
    /// at `now`: playing as soon as it can.
    pub fn new(title: &str, file: &str, now: u64) -> Playback {
        Playback {
            title: title.into(),
            file: file.into(),
            phase: Phase::Opening,
            want_playing: true,
            info: None,
            generation: 0,
            pending: None,
            seeks: 0,
            target_us: 0,
            from_us: 0,
            clock: Clock::still(0, 0),
            play_began: now,
            sound_still_since: None,
            settling: false,
            began_ns: now,
            waiting_since: now,
            first_shown: false,
            opening: true,
            open_ms: 0,
            restarts: 0,
            scrub_us: None,
            exact: true,
            hold: false,
            hidden: false,
            sound: SoundState::default(),
            pictures: Pictures::default(),
            skip: SkipControl::new(),
            skip_counts: (0, 0),
            skip_since: now,
            decoder_late: 0,
            stats: PlaybackStats::new(now),
            stats_since: now,
            remember_at: now.saturating_add(REMEMBER_EVERY_NS),
            effects: Vec::new(),
        }
    }

    /// Opening resumes at `at_us` (shown until the first picture comes).
    pub fn resuming(&mut self, at_us: i64) {
        self.target_us = at_us;
    }

    /// What it asks of the window since last asked.
    pub fn take_effects(&mut self) -> Vec<Effect> {
        core::mem::take(&mut self.effects)
    }

    fn log(&mut self, line: String) {
        self.effects.push(Effect::Log(line));
    }

    fn ask(&mut self, a: Ask) {
        self.effects.push(Effect::Engine(a));
    }

    fn ask_sound(&mut self, a: SoundAsk) {
        self.effects.push(Effect::Sound(a));
    }

    // ---- what it shows ------------------------------------------------------

    /// Where playback is.
    pub fn phase(&self) -> &Phase {
        &self.phase
    }

    /// The video's title (its own, once open, or its file's).
    pub fn title(&self) -> &str {
        &self.title
    }

    /// What the video holds, once open.
    pub fn info(&self) -> Option<&MediaInfo> {
        self.info.as_deref()
    }

    /// How long the video is, if known (µs).
    pub fn duration_us(&self) -> Option<i64> {
        self.info.as_ref().and_then(|i| i.duration_us)
    }

    /// Positions can be sought (the source says so).
    pub fn seekable(&self) -> bool {
        self.info.as_ref().is_some_and(|i| i.seekable)
    }

    /// The generation whose pictures are shown.
    pub fn generation(&self) -> u32 {
        self.generation
    }

    /// Pictures are taken from the pool: not while a seek is under way
    /// (they are of the position left, or of one the player has not heard
    /// of yet).
    pub fn takes_pictures(&self) -> bool {
        self.pending.is_none()
    }

    /// Seeks asked so far.
    pub fn seeks(&self) -> u32 {
        self.seeks
    }

    /// Waiting for the decoder (a spinner after a moment).
    pub fn waiting(&self) -> bool {
        matches!(self.phase, Phase::Opening | Phase::Seeking | Phase::Restarting)
    }

    /// When the wait began (opening, the last seek, a restart).
    pub fn wait_began(&self) -> u64 {
        self.began_ns
    }

    /// Playing, or about to (the play button shows pause).
    pub fn playing(&self) -> bool {
        self.phase == Phase::Playing || (self.want_playing && self.waiting())
    }

    /// The window is hidden (no pictures are decoded).
    pub fn hidden(&self) -> bool {
        self.hidden
    }

    /// How much the decoder leaves out to keep up.
    pub fn skip_level(&self) -> SkipLevel {
        self.skip.level()
    }

    fn has_sound(&self) -> bool {
        self.info.as_ref().is_some_and(|i| i.selected_audio.is_some()) && self.sound.attached && self.sound.available
    }

    /// The picture shown, if it is of the generation shown.
    fn shown_here(&self) -> Option<SlotMeta> {
        self.pictures.shown.filter(|p| p.generation == self.generation)
    }

    /// The sound's clock, while playing and the sound is heard (and moves,
    /// or stood still only a moment: sound that stops coming is not waited
    /// for).
    fn sound_clock(&self, now: u64) -> Option<Clock> {
        if self.phase != Phase::Playing || !self.has_sound() {
            return None;
        }
        let c = self.sound.clock_for(self.generation)?;
        let stalled = self.sound_still_since.is_some_and(|t| now.saturating_sub(t) > SOUND_STALL_NS);
        (c.running || !stalled).then_some(c)
    }

    /// The clock now: the sound's while it is heard (and while it plays out
    /// after pausing), the player's own otherwise.
    pub fn clock(&self, now: u64) -> Clock {
        if self.settling
            && let Some(c) = self.sound.clock_for(self.generation)
        {
            return c;
        }
        self.sound_clock(now).unwrap_or(self.clock)
    }

    /// The media time pictures are chosen for: when the frame is seen
    /// (`seen_ns`) while playing, now otherwise.
    pub fn picture_time(&self, seen_ns: u64, now: u64) -> i64 {
        let clock = self.clock(now);
        if self.phase == Phase::Playing { clock.at(seen_ns) } else { clock.at(now) }
    }

    /// The position shown (µs).
    pub fn position_us(&self, now: u64) -> i64 {
        let at = match self.phase {
            Phase::Opening | Phase::Seeking | Phase::Restarting => self.target_us,
            Phase::Paused => self.shown_here().map_or(self.clock.base_us, |p| p.pts_us),
            _ => self.clock(now).at(now),
        };
        self.duration_us().map_or(at, |d| at.min(d)).max(0)
    }

    // ---- what it is told -----------------------------------------------------

    /// The audio thread's state this frame.
    pub fn see_sound(&mut self, sound: SoundState) {
        self.sound = sound;
    }

    /// The pool's this frame.
    pub fn see_pictures(&mut self, pictures: Pictures) {
        self.pictures = pictures;
    }

    /// Takes the engine's news: the video opened (or failed), a restart, a
    /// seek carried out. The same state may come again: news counts once.
    pub fn sync(&mut self, st: &EngineState, now: u64) {
        if st.restarts != self.restarts {
            // The decoder was started again, from the picture shown (or
            // further along): as after a seek, whether or not the restart
            // was seen under way.
            self.restarts = st.restarts;
            self.ask_sound(SoundAsk::Pause);
            // Where it goes on: the picture shown, unless a seek is under way.
            if self.pending.is_none() {
                self.target_us = self.pictures.shown.map_or(self.target_us, |p| p.pts_us);
            }
            self.clock = Clock::still(self.target_us, self.generation);
            self.began_ns = now;
            self.waiting_since = now;
            self.first_shown = false;
            self.exact = true;
            if !matches!(self.phase, Phase::Failed(_)) {
                self.phase = Phase::Restarting;
            }
        }
        if let Some(id) = self.pending
            && st.seeked == id
        {
            self.pending = None;
            if st.seek_failed {
                self.seek_refused(now);
            } else {
                self.adopt(st.generation, now);
            }
        }
        match st.phase {
            EnginePhase::Ready => {
                if self.info.is_none()
                    && let Some(info) = &st.info
                {
                    self.open_ms = st.open_ms;
                    self.opened(info.clone(), now);
                }
                // Opening's generation, or a restart's (the player's own seeks
                // are taken when done).
                if self.pending.is_none() && st.generation != self.generation {
                    self.adopt(st.generation, now);
                }
                if self.phase == Phase::Restarting {
                    // The restarted decoder seeks to the picture shown.
                    self.phase = Phase::Seeking;
                }
            }
            EnginePhase::Failed | EnginePhase::Missing => {
                let reason = st.error.clone().unwrap_or_else(|| "The video could not be played.".into());
                if !matches!(self.phase, Phase::Failed(_)) {
                    self.log(format!("cannot play {}: {reason}", self.file));
                    self.ask_sound(SoundAsk::Pause);
                }
                self.phase = Phase::Failed(reason);
            }
            _ => {}
        }
    }

    /// The decoder decodes `generation` now.
    fn adopt(&mut self, generation: u32, now: u64) {
        self.generation = generation;
        self.clock.generation = generation;
        self.waiting_since = now;
    }

    fn opened(&mut self, info: Arc<MediaInfo>, now: u64) {
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
        let duration = info.duration_us.map_or(String::from("?"), time);
        self.log(format!("playing {} ({picture}, {sound}, {duration})", self.title));
        self.info = Some(info);
        self.waiting_since = now;
        if self.phase == Phase::Opening {
            self.phase = Phase::Seeking;
        }
    }

    /// The decoder could not go where the seek asked: playback goes on from
    /// the picture shown, or where it was.
    fn seek_refused(&mut self, now: u64) {
        let target = self.target_us;
        self.log(format!("cannot go to {}", time(target)));
        let at = self.shown_here().map_or(self.from_us, |p| p.pts_us);
        self.target_us = at;
        self.first_shown = true;
        if self.want_playing && !self.hold {
            self.start(at, now);
        } else {
            self.ask(Ask::Pause);
            self.clock = Clock::still(at, self.generation);
            self.phase = Phase::Paused;
        }
    }

    // ---- what the user asks ----------------------------------------------------

    /// Plays (or resumes).
    pub fn play(&mut self, now: u64) {
        match self.phase {
            Phase::Ended => {
                self.want_playing = true;
                if self.seekable() {
                    self.seek_to(0, SeekMode::Exact, now);
                } else {
                    self.effects.push(Effect::Reopen);
                }
            }
            Phase::Paused => {
                self.want_playing = true;
                self.settling = false;
                let at = self.position_us(now);
                self.log(format!("resumed at {}", time(at)));
                self.ask(Ask::Play);
                self.ask_sound(SoundAsk::Play);
                self.clock = Clock::running(at, now, self.generation);
                self.phase = Phase::Playing;
                self.stats_restart(now);
            }
            Phase::Opening | Phase::Seeking | Phase::Restarting => self.want_playing = true,
            _ => {}
        }
    }

    /// Pauses (the picture follows the sound already sent until it stops).
    pub fn pause(&mut self, now: u64) {
        self.want_playing = false;
        if self.phase == Phase::Playing {
            let at = self.position_us(now);
            self.log(format!("paused at {}", time(at)));
            self.ask(Ask::Pause);
            self.ask_sound(SoundAsk::Pause);
            let still = self.shown_here().map_or(at, |p| p.pts_us);
            self.clock = Clock::still(still, self.generation);
            self.phase = Phase::Paused;
            self.settling = self.has_sound();
            self.remember(now);
        }
    }

    /// Pauses if playing (or about to), plays otherwise.
    pub fn toggle(&mut self, now: u64) {
        if self.playing() { self.pause(now) } else { self.play(now) }
    }

    /// Goes to `position_us` (said in the log when exact).
    pub fn seek_to(&mut self, position_us: i64, mode: SeekMode, now: u64) {
        if let Some(at) = self.go(position_us, mode, now)
            && mode == SeekMode::Exact
        {
            self.log(format!("seek to {}", time(at)));
        }
    }

    /// Moves by `delta_us`.
    pub fn seek_by(&mut self, delta_us: i64, now: u64) {
        let at = self.position_us(now);
        self.seek_to(at.saturating_add(delta_us), SeekMode::Exact, now);
    }

    /// The seek bar is dragged to `to_us`: key frame seeks, at most one
    /// under way unless the pointer moved far.
    pub fn scrub(&mut self, to_us: i64, now: u64) {
        let moved = self.scrub_us != Some(to_us);
        if moved && (self.phase != Phase::Seeking || to_us.abs_diff(self.target_us) > 2_000_000) {
            self.scrub_us = Some(to_us);
            self.go(to_us, SeekMode::Keyframe, now);
        } else if moved {
            self.target_us = to_us;
        }
    }

    /// Whether the seek bar is held: playing waits until it is let go.
    pub fn set_hold(&mut self, hold: bool) {
        self.hold = hold;
    }

    /// Seeks without saying so; where it goes, unless it cannot.
    fn go(&mut self, position_us: i64, mode: SeekMode, now: u64) -> Option<i64> {
        if !self.seekable() || matches!(self.phase, Phase::Failed(_) | Phase::Restarting) {
            return None;
        }
        let end = self.duration_us().unwrap_or(i64::MAX).saturating_sub(1).max(0);
        let target = position_us.clamp(0, end);
        if self.pending.is_none() {
            self.from_us = self.position_us(now);
        }
        self.seeks = self.seeks.wrapping_add(1);
        let id = self.seeks;
        self.pending = Some(id);
        self.target_us = target;
        self.settling = false;
        self.exact = mode == SeekMode::Exact;
        if self.exact {
            self.scrub_us = None;
        }
        self.ask(Ask::Seek { position_us: target, mode, id });
        self.ask_sound(SoundAsk::Pause);
        self.clock = Clock::still(target, self.generation);
        self.phase = Phase::Seeking;
        self.began_ns = now;
        self.waiting_since = now;
        self.first_shown = false;
        self.skip.reset();
        self.stats_restart(now);
        Some(target)
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
        let Some(shown) = self.shown_here() else { return };
        if forward {
            // The next picture is due at once; the decoder makes it if it
            // has not yet.
            let to = shown.pts_us.saturating_add(shown.duration_us.max(1));
            if self.duration_us().is_some_and(|d| to >= d) {
                return;
            }
            self.clock = Clock::still(to, self.generation);
            self.ask(Ask::Step(1));
            self.log(format!("step to {}", text::precise(to)));
        } else if shown.pts_us > 0 {
            // The picture shown just before this one starts.
            self.want_playing = false;
            if self.go(shown.pts_us - 1, SeekMode::Exact, now).is_some() {
                let to = shown.pts_us.saturating_sub(shown.duration_us.max(1)).max(0);
                self.log(format!("step to {}", text::precise(to)));
            }
        }
    }

    /// The window was minimised or shown again: no pictures meanwhile.
    pub fn set_hidden(&mut self, hidden: bool, now: u64) {
        if hidden == self.hidden {
            return;
        }
        self.hidden = hidden;
        if hidden {
            self.ask(Ask::Skip(SkipLevel::Hidden));
        } else {
            self.ask(Ask::Skip(self.skip.level()));
            // The decoder dropped pictures while hidden: from here again.
            if matches!(self.phase, Phase::Playing | Phase::Paused) {
                let at = self.position_us(now);
                self.go(at, SeekMode::Exact, now);
            }
        }
    }

    /// Asks the window to remember where playback is.
    pub fn remember(&mut self, now: u64) {
        if let Some(d) = self.duration_us()
            && !matches!(self.phase, Phase::Failed(_) | Phase::Opening)
        {
            let at = if self.phase == Phase::Ended { d } else { self.position_us(now) };
            self.effects
                .push(Effect::Remember { position_ms: at.max(0) as u64 / 1000, duration_ms: d.max(0) as u64 / 1000 });
        }
    }

    // ---- time going by ---------------------------------------------------------

    /// Keeps the player's clock with the sound's, to go on from there should
    /// the sound stop; starts it if the sound is not heard in time.
    pub fn follow_sound(&mut self, now: u64) {
        if self.settling {
            // Paused: the picture stops where the sound does.
            match self.sound.clock_for(self.generation) {
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
        let heard = self.sound.clock_for(self.generation).filter(|_| self.has_sound());
        match heard {
            Some(c) if !c.running => {
                self.sound_still_since.get_or_insert(now);
            }
            _ => self.sound_still_since = None,
        }
        match self.sound_clock(now) {
            Some(c) => self.clock = Clock { generation: self.generation, ..c },
            None if !self.clock.running => {
                // Not heard in time, ended or broken: the picture goes on by
                // itself.
                let waiting = self.has_sound() && self.sound.live(self.generation) && heard.is_none();
                let start = if waiting { self.play_began.saturating_add(SOUND_WAIT_NS) } else { now };
                if now >= start {
                    self.clock = Clock::running(self.clock.base_us, start, self.generation);
                }
            }
            None => {}
        }
    }

    /// Starts playing once the first picture of a seek is up and some sound
    /// is buffered; notices the end. Call it once the frame's picture is
    /// chosen.
    pub fn advance(&mut self, now: u64) {
        if matches!(self.phase, Phase::Seeking | Phase::Opening) && self.info.is_some() && self.pending.is_none() {
            let no_picture = self.info.as_ref().is_some_and(|i| i.selected_video.is_none());
            let first = self.shown_here().map(|p| p.pts_us);
            // No picture is coming: the decoder ended the pictures (past the
            // end of the source, or a damaged one), or made none for long.
            let none_coming = first.is_none()
                && !no_picture
                && (self.pictures.ended == Some(self.generation)
                    || now.saturating_sub(self.waiting_since) > PATIENCE_NS);
            if first.is_some() || no_picture || none_coming {
                if !self.first_shown {
                    self.first_shown = true;
                    let ms = now.saturating_sub(self.began_ns) / 1_000_000;
                    if none_coming {
                        self.opening = false;
                        self.log(format!("no picture at {}: going on without it", time(self.target_us)));
                    } else if core::mem::take(&mut self.opening) {
                        self.log(format!("first frame in {ms} ms (open {} ms)", self.open_ms));
                    } else if self.exact {
                        self.log(format!("seek done in {ms} ms"));
                    }
                }
                let sound_ready = !self.has_sound()
                    || self.sound.queued_for(self.generation) >= PREROLL_SOUND_US
                    || self.sound.ended == Some(self.generation)
                    || now.saturating_sub(self.began_ns) > PREROLL_MAX_NS;
                // Scrubbing holds playback until the seek bar is let go.
                if sound_ready || !self.want_playing || self.hold {
                    let at = first.unwrap_or(self.target_us);
                    if self.want_playing && !self.hold {
                        self.start(at, now);
                    } else {
                        self.ask(Ask::Pause);
                        self.clock = Clock::still(at, self.generation);
                        self.phase = Phase::Paused;
                    }
                }
            }
        }
        if self.phase == Phase::Playing && self.is_at_end(now) {
            self.phase = Phase::Ended;
            self.want_playing = false;
            self.log(format!("finished {}", self.title));
            if self.stats.shown > 0 {
                let line = self.stats.line(now);
                self.log(format!("playback: {line}"));
            }
            self.ask(Ask::Pause);
            self.ask_sound(SoundAsk::Pause);
            self.remember(now);
        }
    }

    /// Plays from `at_us`: with sound, the picture waits until the sound is
    /// heard (the output's latency), so that they start together.
    fn start(&mut self, at_us: i64, now: u64) {
        self.ask(Ask::Play);
        self.ask_sound(SoundAsk::Play);
        self.clock = if self.has_sound() {
            Clock::still(at_us, self.generation)
        } else {
            Clock::running(at_us, now, self.generation)
        };
        self.play_began = now;
        self.phase = Phase::Playing;
        self.stats_restart(now);
    }

    /// The decoder published the last picture of this generation, and it is
    /// the one shown.
    fn pictures_ended(&self) -> bool {
        self.pictures.ended == Some(self.generation)
    }

    fn is_at_end(&self, now: u64) -> bool {
        let clock = self.clock(now).at(now);
        let video_done = match self.info.as_ref().and_then(|i| i.selected_video) {
            Some(_) if self.pictures.pool && !self.pictures.resizing => {
                let shown_out = if self.hidden {
                    // Hidden, the decoder ends the pictures at once: the
                    // video ends when its time is up.
                    self.duration_us().is_none_or(|d| clock >= d)
                } else {
                    self.pictures.shown.is_none_or(|p| clock >= p.pts_us.saturating_add(p.duration_us))
                };
                self.pictures_ended() && shown_out
            }
            Some(_) => false,
            None => true,
        };
        let sound_done = !self.has_sound() || self.sound.ended == Some(self.generation);
        video_done && sound_done
    }

    // ---- how it goes -----------------------------------------------------------

    fn stats_restart(&mut self, now: u64) {
        self.stats = PlaybackStats::new(now);
        self.stats_since = now;
        self.skip_counts = (0, 0);
        self.skip_since = now;
    }

    /// A picture of the generation shown was passed over while due.
    pub fn count_dropped(&mut self) {
        if self.phase == Phase::Playing {
            self.stats.add_dropped(1);
            self.skip_counts.1 = self.skip_counts.1.saturating_add(1);
        }
    }

    /// A picture was shown; `late` if after its time.
    pub fn count_shown(&mut self, late: bool) {
        if self.phase == Phase::Playing {
            self.stats.shown = self.stats.shown.saturating_add(1);
            self.skip_counts.0 = self.skip_counts.0.saturating_add(1);
            if late {
                self.stats.late = self.stats.late.saturating_add(1);
                self.skip_counts.1 = self.skip_counts.1.saturating_add(1);
            }
        }
    }

    /// The picture of time `pts_us` was seen at `seen_ns` (the last picture,
    /// which stays up past its time, does not count).
    pub fn count_presented(&mut self, pts_us: i64, seen_ns: u64, now: u64) {
        if self.phase == Phase::Playing && !self.pictures_ended() {
            let at = self.clock(now).at(seen_ns);
            self.stats.offset(pts_us, at);
            self.stats.presents = self.stats.presents.saturating_add(1);
        }
    }

    /// A frame drawn in `ns`, the picture's copy taking `blit_ns`.
    pub fn count_drawn(&mut self, ns: u64, blit_ns: u64) {
        if self.phase == Phase::Playing {
            self.stats.drawn(ns, blit_ns);
        }
    }

    /// A new pool: it counts its own late pictures.
    pub fn new_pool(&mut self) {
        self.decoder_late = 0;
    }

    /// The pool's count of pictures the decoder dropped itself, too late to
    /// be converted: dropped all the same.
    pub fn count_decoder_late(&mut self, total: u64) {
        let new = total.saturating_sub(self.decoder_late).min(u32::MAX as u64) as u32;
        self.decoder_late = total;
        if self.phase == Phase::Playing {
            self.stats.add_dropped(new);
            self.skip_counts.0 = self.skip_counts.0.saturating_add(new);
            self.skip_counts.1 = self.skip_counts.1.saturating_add(new);
        }
    }

    /// Judges playback now and then: what the decoder should leave out, the
    /// log's playback line, where the video is remembered.
    pub fn tick(&mut self, now: u64) {
        if self.phase != Phase::Playing {
            return;
        }
        if now.saturating_sub(self.skip_since) >= SKIP_WINDOW_NS {
            let (due, late) = self.skip_counts;
            self.skip_counts = (0, 0);
            self.skip_since = now;
            if let Some(level) = self.skip.window(due, late) {
                let line = match level {
                    SkipLevel::Nothing => String::from("decoder caught up"),
                    l => format!("decoder too slow: skipping {}", skip_name(l)),
                };
                self.log(line);
                if !self.hidden {
                    self.ask(Ask::Skip(level));
                }
            }
        }
        if now.saturating_sub(self.stats_since) >= STATS_EVERY_NS {
            let line = self.stats.line(now);
            self.log(format!("playback: {line}"));
            if self.stats.out_of_sync()
                && let Some((mean, p95)) = self.stats.sync()
            {
                self.log(format!(
                    "out of sync: pictures {} ms from the sound on average, {} ms at worst (p95)",
                    mean / 1000,
                    p95 / 1000
                ));
            }
            self.stats_restart(now);
        }
        if now >= self.remember_at {
            self.remember_at = now.saturating_add(REMEMBER_EVERY_NS);
            self.remember(now);
        }
    }

    /// When the next picture (due at `due_us`, media time) is due on the
    /// screen (monotonic ns), if the clock runs.
    pub fn due_ns(&self, due_us: Option<i64>, now: u64) -> Option<u64> {
        due_us.and_then(|d| self.clock(now).when(d))
    }

    /// When playback wants to be looked at again, besides a picture due:
    /// while waiting for the decoder or for the sound, while the sound plays
    /// out after pausing, and now and then while hidden.
    pub fn wake_at(&self, now: u64) -> Option<u64> {
        if self.hidden {
            return (self.phase == Phase::Playing).then_some(now.saturating_add(HIDDEN_NS));
        }
        match self.phase {
            Phase::Playing if !self.clock(now).running => {
                // Waiting for the sound to be heard: the audio thread says
                // when it is, and the picture goes by itself at the end of
                // the wait.
                let give_up = self.play_began.saturating_add(SOUND_WAIT_NS);
                Some(if now < give_up { give_up } else { now.saturating_add(WAITING_NS) })
            }
            Phase::Seeking | Phase::Opening | Phase::Restarting => Some(now.saturating_add(WAITING_NS)),
            Phase::Paused if self.settling => Some(now.saturating_add(SETTLING_NS)),
            _ => None,
        }
    }
}

/// What a skip level leaves out, for the log.
pub fn skip_name(l: SkipLevel) -> &'static str {
    match l {
        SkipLevel::Nothing => "nothing",
        SkipLevel::LoopFilter => "the loop filter",
        SkipLevel::NonReference => "pictures nothing refers to",
        SkipLevel::KeyframesOnly => "all but key frames",
        SkipLevel::Hidden => "every picture",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use vproto::media::{AudioStream, Colour, Rational, VideoStream};

    const MS: u64 = 1_000_000;

    fn info(video: bool, sound: bool, seekable: bool) -> Arc<MediaInfo> {
        let v = VideoStream {
            index: 0,
            codec: "av1".into(),
            width: 640,
            height: 360,
            sample_aspect: Rational::new(1, 1),
            rotation: 0,
            mirrored: false,
            frame_rate: Rational::new(25, 1),
            bit_depth: 8,
            colour: Colour::default(),
            hdr: false,
            decodable: true,
            default: true,
            language: String::new(),
            title: String::new(),
        };
        let a = AudioStream {
            index: 1,
            codec: "flac".into(),
            rate: 48_000,
            channels: 2,
            decodable: true,
            default: true,
            language: String::new(),
            title: String::new(),
        };
        Arc::new(MediaInfo {
            container: "matroska".into(),
            duration_us: Some(20_000_000),
            seekable,
            bit_rate: 0,
            title: "Test".into(),
            artist: String::new(),
            video: vec![v],
            audio: vec![a],
            subtitles: 0,
            chapters: Vec::new(),
            selected_video: video.then_some(0),
            selected_audio: sound.then_some(1),
        })
    }

    fn picture(generation: u32, pts_us: i64) -> SlotMeta {
        SlotMeta { generation, seq: 1, pts_us, duration_us: 40_000, width: 64, height: 36, flags: 0 }
    }

    fn sound(generation: u32, queued_us: i64) -> SoundState {
        SoundState {
            available: true,
            rate: 48_000,
            attached: true,
            generation,
            clock: Clock::still(0, generation),
            heard: false,
            queued_us,
            ended: None,
            broken: false,
        }
    }

    /// A playback opened (generation 7) whose first picture is up.
    fn opened(video: bool, sound_too: bool) -> (Playback, EngineState) {
        let mut p = Playback::new("clip", "clip.mkv", 0);
        let st = EngineState {
            phase: EnginePhase::Ready,
            info: Some(info(video, sound_too, true)),
            generation: 7,
            open_ms: 30,
            ..EngineState::default()
        };
        p.see_sound(sound(7, 0));
        p.sync(&st, MS);
        (p, st)
    }

    fn engine_asks(p: &mut Playback) -> Vec<Ask> {
        p.take_effects()
            .into_iter()
            .filter_map(|e| match e {
                Effect::Engine(a) => Some(a),
                _ => None,
            })
            .collect()
    }

    fn logs(effects: &[Effect]) -> Vec<String> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::Log(l) => Some(l.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn it_opens_and_plays_once_some_sound_is_queued() {
        let (mut p, _) = opened(true, true);
        assert_eq!(p.phase(), &Phase::Seeking);
        assert_eq!(p.generation(), 7);
        p.see_pictures(Pictures { shown: Some(picture(7, 0)), pool: true, ..Pictures::default() });
        p.advance(2 * MS);
        assert_eq!(p.phase(), &Phase::Seeking, "no sound queued yet");
        p.see_sound(sound(7, 150_000));
        p.advance(3 * MS);
        assert_eq!(p.phase(), &Phase::Playing);
        let effects = p.take_effects();
        assert!(logs(&effects)[0].starts_with("playing Test (640x360 AV1 25 fps, FLAC 48000 Hz, 0:20)"));
        assert!(logs(&effects).contains(&String::from("first frame in 2 ms (open 30 ms)")));
        assert!(effects.contains(&Effect::Engine(Ask::Play)) && effects.contains(&Effect::Sound(SoundAsk::Play)));
        // The picture waits for the sound to be heard, at most a moment.
        assert!(!p.clock(4 * MS).running);
        assert_eq!(p.wake_at(4 * MS), Some(3 * MS + SOUND_WAIT_NS));
        p.follow_sound(3 * MS + SOUND_WAIT_NS);
        assert!(p.clock(3 * MS + SOUND_WAIT_NS).running, "not heard in time: it goes by itself");
    }

    #[test]
    fn the_preroll_counts_only_the_sound_of_the_new_position() {
        let (mut p, mut st) = opened(true, true);
        p.see_pictures(Pictures { shown: Some(picture(7, 0)), pool: true, ..Pictures::default() });
        p.see_sound(sound(7, 200_000));
        p.advance(2 * MS);
        assert_eq!(p.phase(), &Phase::Playing);
        // A seek: the engine carries it out as generation 8.
        p.seek_to(15_000_000, SeekMode::Exact, 10 * MS);
        let Some(Ask::Seek { id, .. }) = engine_asks(&mut p).into_iter().find(|a| matches!(a, Ask::Seek { .. })) else {
            panic!("no seek asked")
        };
        assert!(!p.takes_pictures(), "no pictures while the seek is under way");
        st.generation = 8;
        st.seeked = id;
        p.sync(&st, 11 * MS);
        assert!(p.takes_pictures() && p.generation() == 8);
        p.see_pictures(Pictures { shown: Some(picture(8, 15_000_000)), pool: true, ..Pictures::default() });
        // The audio thread still has the old position's sound queued (it
        // has not taken the new generation yet): that is not the new one's.
        p.see_sound(sound(7, 5_000_000));
        p.advance(12 * MS);
        assert_eq!(p.phase(), &Phase::Seeking);
        p.see_sound(sound(8, 30_000));
        p.advance(13 * MS);
        assert_eq!(p.phase(), &Phase::Seeking, "too little of it");
        p.see_sound(sound(8, 130_000));
        p.advance(14 * MS);
        assert_eq!(p.phase(), &Phase::Playing);
    }

    #[test]
    fn a_seek_the_decoder_refuses_goes_on_from_where_it_was() {
        let (mut p, mut st) = opened(true, false);
        p.see_pictures(Pictures { shown: Some(picture(7, 2_000_000)), pool: true, ..Pictures::default() });
        p.advance(2 * MS);
        assert_eq!(p.phase(), &Phase::Playing);
        p.take_effects();
        p.seek_to(15_000_000, SeekMode::Exact, 10 * MS);
        assert_eq!(p.phase(), &Phase::Seeking);
        st.seeked = p.seeks();
        st.seek_failed = true;
        p.sync(&st, 11 * MS);
        assert_eq!(p.phase(), &Phase::Playing, "not left waiting");
        assert_eq!(p.generation(), 7);
        assert_eq!(p.position_us(11 * MS), 2_000_000);
        let effects = p.take_effects();
        assert!(logs(&effects).contains(&String::from("cannot go to 0:15")));
        assert!(effects.contains(&Effect::Engine(Ask::Play)));
        // Paused, it stays paused.
        p.pause(12 * MS);
        p.seek_to(5_000_000, SeekMode::Exact, 13 * MS);
        st.seeked = p.seeks();
        p.sync(&st, 14 * MS);
        assert_eq!(p.phase(), &Phase::Paused);
    }

    #[test]
    fn a_seek_racing_a_restart_waits_for_its_own_generation() {
        let (mut p, mut st) = opened(true, false);
        p.see_pictures(Pictures { shown: Some(picture(7, 1_000_000)), pool: true, ..Pictures::default() });
        p.advance(2 * MS);
        p.seek_to(9_000_000, SeekMode::Exact, 10 * MS);
        // The decoder died meanwhile: the engine restarted it at the picture
        // shown (generation 8), then carried out the seek (generation 9).
        st.restarts = 1;
        st.generation = 8;
        p.sync(&st, 11 * MS);
        assert_eq!(p.phase(), &Phase::Seeking);
        assert_eq!(p.generation(), 7, "the restart's generation is not the seek's");
        assert!(!p.takes_pictures());
        assert_eq!(p.position_us(11 * MS), 9_000_000, "it still goes where it was asked");
        st.generation = 9;
        st.seeked = p.seeks();
        p.sync(&st, 12 * MS);
        assert_eq!(p.generation(), 9);
        p.see_pictures(Pictures { shown: Some(picture(9, 9_000_000)), pool: true, ..Pictures::default() });
        p.advance(13 * MS);
        assert_eq!(p.phase(), &Phase::Playing);
        assert_eq!(p.position_us(13 * MS), 9_000_000);
    }

    #[test]
    fn a_restart_takes_the_new_generation() {
        let (mut p, mut st) = opened(true, false);
        p.see_pictures(Pictures { shown: Some(picture(7, 3_000_000)), pool: true, ..Pictures::default() });
        p.advance(2 * MS);
        st.phase = EnginePhase::Restarting;
        st.restarts = 1;
        p.sync(&st, 5 * MS);
        assert_eq!(p.phase(), &Phase::Restarting);
        assert_eq!(p.position_us(5 * MS), 3_000_000);
        st.phase = EnginePhase::Ready;
        st.generation = 8;
        p.sync(&st, 6 * MS);
        assert_eq!((p.phase(), p.generation()), (&Phase::Seeking, 8));
        // The same state again changes nothing.
        p.take_effects();
        p.sync(&st, 7 * MS);
        assert_eq!(p.take_effects(), []);
    }

    #[test]
    fn no_picture_coming_is_not_waited_for() {
        // The decoder ended the generation without a picture.
        let (mut p, _) = opened(true, false);
        p.see_pictures(Pictures { shown: None, pool: true, ended: Some(7), resizing: false });
        p.advance(2 * MS);
        assert_eq!(p.phase(), &Phase::Ended, "playing, then at the end");
        // Or made none for long.
        let (mut p, _) = opened(true, false);
        p.see_pictures(Pictures { shown: None, pool: true, ..Pictures::default() });
        p.advance(PATIENCE_NS);
        assert_eq!(p.phase(), &Phase::Seeking);
        p.advance(2 * MS + PATIENCE_NS);
        assert_eq!(p.phase(), &Phase::Playing);
        assert!(logs(&p.take_effects()).contains(&String::from("no picture at 0:00: going on without it")));
    }

    #[test]
    fn a_source_that_cannot_seek_is_not_sought() {
        let mut p = Playback::new("clip", "clip.mkv", 0);
        let st = EngineState {
            phase: EnginePhase::Ready,
            info: Some(info(true, false, false)),
            generation: 3,
            ..EngineState::default()
        };
        p.sync(&st, MS);
        p.see_pictures(Pictures { shown: Some(picture(3, 0)), pool: true, ..Pictures::default() });
        p.advance(2 * MS);
        p.take_effects();
        p.seek_to(5_000_000, SeekMode::Exact, 3 * MS);
        p.seek_by(-1_000_000, 3 * MS);
        p.scrub(9_000_000, 3 * MS);
        assert_eq!(p.take_effects(), []);
        assert_eq!(p.phase(), &Phase::Playing);
        // At its end, playing again opens it again.
        p.see_pictures(Pictures { shown: Some(picture(3, 0)), pool: true, ended: Some(3), resizing: false });
        p.advance(100 * MS);
        assert_eq!(p.phase(), &Phase::Ended);
        p.take_effects();
        p.play(101 * MS);
        assert_eq!(p.take_effects(), [Effect::Reopen]);
    }

    #[test]
    fn sound_ends_the_video_unless_it_is_lost() {
        let (mut p, _) = opened(false, true);
        p.see_sound(sound(7, 200_000));
        p.advance(2 * MS);
        assert_eq!(p.phase(), &Phase::Playing);
        p.advance(3 * MS);
        assert_eq!(p.phase(), &Phase::Playing, "the sound has not ended");
        // The audio service went away: the video does not wait for its sound.
        p.see_sound(SoundState { available: false, ..sound(7, 0) });
        p.advance(4 * MS);
        assert_eq!(p.phase(), &Phase::Ended);
    }

    #[test]
    fn hiding_skips_pictures_and_showing_seeks() {
        let (mut p, _) = opened(true, false);
        p.see_pictures(Pictures { shown: Some(picture(7, 0)), pool: true, ..Pictures::default() });
        p.advance(2 * MS);
        p.take_effects();
        p.set_hidden(true, 3 * MS);
        assert_eq!(engine_asks(&mut p), [Ask::Skip(SkipLevel::Hidden)]);
        assert_eq!(p.wake_at(3 * MS), Some(3 * MS + HIDDEN_NS));
        p.set_hidden(false, 4 * MS);
        let asks = engine_asks(&mut p);
        assert_eq!(asks[0], Ask::Skip(SkipLevel::Nothing));
        assert!(matches!(asks[1], Ask::Seek { mode: SeekMode::Exact, .. }));
    }

    #[test]
    fn hostile_times_do_not_overflow() {
        let (mut p, _) = opened(true, false);
        p.see_pictures(Pictures {
            shown: Some(SlotMeta { pts_us: i64::MAX, duration_us: i64::MAX, ..picture(7, 0) }),
            pool: true,
            ended: Some(7),
            resizing: false,
        });
        p.advance(2 * MS);
        p.seek_by(i64::MAX, 3 * MS);
        p.seek_by(i64::MIN, 3 * MS);
        p.count_presented(i64::MIN, u64::MAX, 4 * MS);
        p.count_decoder_late(u64::MAX);
        p.count_decoder_late(0);
        assert!(p.position_us(u64::MAX) <= 20_000_000);
    }

    #[test]
    fn generations_wrap() {
        assert!(is_newer(8, 7) && !is_newer(7, 8) && !is_newer(7, 7));
        assert!(is_newer(0, u32::MAX) && is_newer(5, u32::MAX - 5));
    }
}
