// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The audio thread: plays the decoder's sound and keeps the clock.
//!
//! It takes the sound the decoder writes into the ring (records of a
//! generation, each saying when it is heard), copies it into an output
//! stream of the audio service, and notes where each piece came from. How
//! much of the stream has been played then says which moment of the video
//! is heard: the clock the pictures are shown by (`videos_core::clock`),
//! published for the window's thread with how much sound of the position
//! is queued.
//!
//! The stream outlives the audio service (`videos_core::output`): while the
//! service is away, sound is taken from the ring as fast as it would have
//! played and dropped (so that the decoder goes on), the window times the
//! pictures by itself, and the sound comes back with the service.
//!
//! Records of a generation the engine has not told it of yet (it tells it
//! just after the decoder starts one) wait for it; those of earlier ones are
//! dropped. Every call to the audio service is made here (its client is not
//! shared between threads); the window's thread and the engine send
//! [`Command`]s.

use std::collections::VecDeque;
use std::sync::Arc;

use vabi::{WaitItem, signals};
use videos_core::clock::{AudioMap, ClockModel};
use videos_core::output::{Kept, Output};
use videos_core::playback::{SoundState, is_newer};
use vproto::audio::ring as stream_ring;
use vproto::media::{PcmRing, Record, pcm};
use vrt::object::Event;
use vrt::println;
use vrt::sync::Mutex;

/// The output stream's buffer (ms): enough to ride out a busy moment, short
/// enough that pausing and seeking feel immediate.
const BUFFER_MS: u32 = 250;
/// How often the clock is read while playing (ns).
const TICK_NS: u64 = 20_000_000;
/// After pausing, what the output was sent still plays: the clock follows
/// it until it stops, at most this long (ns).
const SETTLE_NS: u64 = 500_000_000;
/// Rate assumed until the audio service says.
const DEFAULT_RATE: u32 = 48_000;

pub enum Command {
    /// The sound of a newly opened video: the ring, the event the decoder
    /// signals, and the decoder's own to signal after reading.
    Attach {
        ring: PcmRing,
        ready: Event,
        wake: Event,
    },
    /// No sound now (closed, or a silent video).
    Detach,
    Play,
    Pause,
    /// Drops what is queued: sound of `generation` follows (a seek).
    Flush {
        generation: u32,
    },
    /// The player's own volume: a linear gain.
    Volume(f32),
}

pub struct Shared {
    commands: Mutex<VecDeque<Command>>,
    wake: Event,
    /// Signalled when something the window waits for changed: sound heard,
    /// ended, broken, the output gone or back.
    pub notify: Event,
    state: Mutex<SoundState>,
}

impl Shared {
    pub fn send(&self, c: Command) {
        self.commands.lock().push_back(c);
        let _ = self.wake.signal();
    }

    /// What the audio thread says now.
    pub fn state(&self) -> SoundState {
        *self.state.lock()
    }

    /// The rate to make sound rings at: the output's.
    pub fn rate(&self) -> u32 {
        self.state.lock().rate
    }
}

/// Starts the audio thread.
pub fn start(volume: f32) -> Option<Arc<Shared>> {
    let shared = Arc::new(Shared {
        commands: Mutex::new(VecDeque::new()),
        wake: Event::create().ok()?,
        notify: Event::create().ok()?,
        state: Mutex::new(SoundState { rate: DEFAULT_RATE, ..SoundState::default() }),
    });
    let ours = shared.clone();
    vrt::thread::Builder::new()
        .name("audio")
        .priority(vabi::priority::HIGH)
        .spawn(move || {
            let mut p = Player::new(ours, volume);
            p.run()
        })
        .ok()?;
    Some(shared)
}

struct Attached {
    ring: PcmRing,
    ready: Event,
    wake: Event,
}

/// What to do with a record of the ring.
enum Use {
    /// It is of the generation playing.
    Play(Record),
    /// The end of the generation playing.
    End(Record),
    /// Of an earlier generation: dropped.
    Drop(Record),
    /// Of a later one: it waits for the engine to say so.
    Wait,
    /// Nothing (more) to read.
    Nothing,
}

struct Player {
    shared: Arc<Shared>,
    output: Output,
    attached: Option<Attached>,
    map: AudioMap,
    model: ClockModel,
    generation: u32,
    playing: bool,
    /// Paused at this time, the sound already sent still playing.
    stopping: Option<u64>,
    /// The end record of the generation was read: once all is heard, it
    /// ended.
    end_read: bool,
    ended: Option<u32>,
    /// The stream frame the generation's sound starts at, once written.
    start: Option<u64>,
    /// When the stream last started playing: the service's reports of
    /// what was played from before then are not run on from.
    resumed_ns: u64,
    heard: bool,
    broken: bool,
    /// Without an output, while playing: since when sound is taken as if it
    /// played, and how many frames were taken since.
    silence: Option<(u64, u64)>,
    /// What was last published (to wake the window when it matters).
    published: SoundState,
    buf: Vec<i16>,
}

impl Player {
    fn new(shared: Arc<Shared>, volume: f32) -> Player {
        Player {
            shared,
            output: Output::new("Videos", DEFAULT_RATE, BUFFER_MS, volume, true),
            attached: None,
            map: AudioMap::new(DEFAULT_RATE),
            model: ClockModel::new(),
            generation: 0,
            playing: false,
            stopping: None,
            end_read: false,
            ended: None,
            start: None,
            resumed_ns: 0,
            heard: false,
            broken: false,
            silence: None,
            published: SoundState::default(),
            buf: vec![0; 2 * pcm::MAX_FRAMES as usize],
        }
    }

    fn rate(&self) -> u32 {
        self.output.rate()
    }

    fn run(&mut self) {
        let mut first = true;
        // The output's losses as last seen.
        let mut losses = 0;
        loop {
            let now = vrt::time::now_ns();
            match self.output.keep(now) {
                Kept::Opened => {
                    let back = self.output.losses() != losses;
                    losses = self.output.losses();
                    self.opened(now, core::mem::take(&mut first), back);
                }
                Kept::Same => {}
                Kept::None if self.silence.is_none() && self.playing => self.silence = Some((now, 0)),
                Kept::None => {}
            }
            self.handle_commands();
            self.pump(vrt::time::now_ns());
            self.publish();
            let mut items =
                vec![WaitItem { handle: self.shared.wake.raw(), signals: signals::SIGNALED, ..Default::default() }];
            self.output.wait_items(&mut items);
            if let Some(a) = &self.attached {
                items.push(WaitItem { handle: a.ready.raw(), signals: signals::SIGNALED, ..Default::default() });
            }
            let mut deadline = if self.playing || self.stopping.is_some() {
                vrt::time::now_ns() + TICK_NS
            } else {
                vabi::DEADLINE_INFINITE
            };
            if let Some(at) = self.output.retry_at() {
                deadline = deadline.min(at.max(vrt::time::now_ns() + TICK_NS));
            }
            let _ = vrt::object::wait_many(&mut items, deadline);
            let _ = self.shared.wake.clear();
            if let Some(s) = self.output.stream() {
                let _ = s.event().clear();
            }
            if let Some(a) = &self.attached {
                let _ = a.ready.clear();
            }
            self.output.observe();
        }
    }

    /// A new output stream (the `first`, one at another rate, or one opened
    /// again as the service came `back`): its positions count from 0, so
    /// what is known of the old one's is forgotten; the sound goes on from
    /// where the ring is.
    fn opened(&mut self, now: u64, first: bool, back: bool) {
        let device_rate = self.output.device_rate();
        if first && device_rate >= 8_000 {
            self.shared.state.lock().rate = device_rate;
        }
        if back {
            println!("sound output back ({})", self.output.device());
        }
        self.map.clear();
        self.start = None;
        self.heard = false;
        self.silence = None;
        self.resumed_ns = now;
        let at = self.model.clock().at(now);
        self.model.stand(at, self.generation);
    }

    fn handle_commands(&mut self) {
        loop {
            let Some(c) = self.shared.commands.lock().pop_front() else { return };
            match c {
                Command::Attach { ring, ready, wake } => {
                    // The stream plays at the ring's rate (the service
                    // resamples to the device's).
                    if ring.rate() != self.rate() {
                        self.output.set_rate(ring.rate());
                        self.map = AudioMap::new(ring.rate());
                    }
                    self.attached = Some(Attached { ring, ready, wake });
                    self.flush(0);
                    self.broken = false;
                }
                Command::Detach => {
                    self.attached = None;
                    self.output.set_paused(true);
                    self.flush(0);
                }
                Command::Play => {
                    self.playing = true;
                    self.stopping = None;
                    self.resumed_ns = vrt::time::now_ns();
                    self.output.set_paused(false);
                    if self.output.stream().is_none() {
                        self.silence = Some((vrt::time::now_ns(), 0));
                    }
                }
                Command::Pause => {
                    // The clock goes on with what still plays, and stops
                    // where the sound does.
                    self.playing = false;
                    self.output.set_paused(true);
                    self.stopping = Some(vrt::time::now_ns());
                    self.silence = None;
                }
                Command::Flush { generation } => self.flush(generation),
                Command::Volume(v) => self.output.set_volume(v),
            }
        }
    }

    /// Empties the stream: what follows is `generation`'s sound.
    fn flush(&mut self, generation: u32) {
        self.output.flush();
        self.map.clear();
        self.generation = generation;
        self.end_read = false;
        self.ended = None;
        self.start = None;
        self.heard = false;
        self.stopping = None;
        if self.silence.is_some() {
            self.silence = Some((vrt::time::now_ns(), 0));
        }
        let at = self.model.clock().at(vrt::time::now_ns());
        self.model.stand(at, generation);
    }

    /// The next record and what to do with it.
    fn next(&mut self) -> Use {
        let Some(a) = &self.attached else { return Use::Nothing };
        let r = match a.ring.peek() {
            Ok(Some(r)) => r,
            Ok(None) => return Use::Nothing,
            Err(_) => {
                if !self.broken {
                    println!("the decoder's sound does not hold together: not played");
                    self.broken = true;
                }
                return Use::Nothing;
            }
        };
        if r.generation != self.generation && is_newer(r.generation, self.generation) {
            Use::Wait
        } else if r.generation != self.generation {
            Use::Drop(r)
        } else if r.kind == pcm::kind::END {
            Use::End(r)
        } else {
            Use::Play(r)
        }
    }

    /// Moves sound from the decoder's ring into the stream, as it fits (or,
    /// without a stream, as fast as it would have played).
    fn pump(&mut self, now: u64) {
        let rate = self.rate();
        let mut read = false;
        // Without a stream: the frames real time allows since the silence
        // began.
        let mut allowed = match (self.output.stream(), self.silence) {
            (Some(s), _) => s.free() as u64,
            (None, Some((since, taken))) => {
                let due = (now.saturating_sub(since) as u128 * rate as u128 / 1_000_000_000) as u64;
                due.saturating_sub(taken)
            }
            (None, None) => 0,
        };
        loop {
            match self.next() {
                Use::Nothing | Use::Wait => break,
                Use::Drop(r) => {
                    self.take(&r, r.frames);
                    read = true;
                }
                Use::End(r) => {
                    self.end_read = true;
                    if let Some(s) = self.output.stream() {
                        s.ring().set_producer_flags(stream_ring::flags::END);
                    }
                    self.take(&r, 0);
                    read = true;
                }
                Use::Play(r) => {
                    if allowed == 0 {
                        break;
                    }
                    let want = (allowed.min(pcm::MAX_FRAMES as u64) as usize) * 2;
                    let Some(a) = &self.attached else { break };
                    let n = a.ring.copy(&r, &mut self.buf[..want]);
                    let w = match self.output.stream() {
                        Some(s) => {
                            let at = s.written();
                            self.start.get_or_insert(at);
                            let w = s.write(&self.buf[..n * 2]);
                            self.map.push(at, r.next_pts(rate), w as u64);
                            w
                        }
                        // Dropped, as if it played.
                        None => {
                            if let Some((_, taken)) = &mut self.silence {
                                *taken += n as u64;
                            }
                            n
                        }
                    };
                    self.take(&r, w as u32);
                    allowed = allowed.saturating_sub(w as u64);
                    read = true;
                    if w == 0 {
                        break;
                    }
                }
            }
        }
        if read && let Some(a) = &self.attached {
            let _ = a.wake.signal();
        }
    }

    fn take(&self, r: &Record, frames: u32) {
        if let Some(a) = &self.attached {
            a.ring.take(r, frames);
        }
    }

    /// Publishes the clock, what is queued, and whether the sound ended;
    /// wakes the window when the sound is first heard, ends, breaks, or the
    /// output comes or goes.
    fn publish(&mut self) {
        let now = vrt::time::now_ns();
        let rate = self.rate();
        let mut queued = 0;
        match self.output.stream() {
            Some(s) => {
                // Run on from the service's last report of what was played,
                // but not from one made before the stream started again
                // (paused, or emptied by a seek): that would run ahead of
                // what is heard.
                let (reported, at) = s.ring().played();
                let played = if at < self.resumed_ns { reported.min(s.written()) } else { s.played_at(now) };
                let raw = self.map.media_at(played);
                if self.playing {
                    self.model.update(now, raw, self.generation);
                    self.heard |= raw.is_some() && self.start.is_some_and(|start| played > start);
                } else if let Some(paused) = self.stopping {
                    self.model.update(now, raw, self.generation);
                    // Stopped where the sound did (or it took too long).
                    if !self.model.clock().running || now.saturating_sub(paused) > SETTLE_NS {
                        self.model.freeze(now);
                        self.stopping = None;
                    }
                }
                self.map.forget_before(played);
                queued = self.map.queued_us(played);
                if self.end_read
                    && self.ended != Some(self.generation)
                    && s.queued() == 0
                    && played + (rate / 100) as u64 >= s.written()
                {
                    self.ended = Some(self.generation);
                }
            }
            None => {
                // Nothing is heard; the sound ends when its end is read.
                self.heard = false;
                self.stopping = None;
                if self.end_read {
                    self.ended = Some(self.generation);
                }
            }
        }
        let state = {
            let mut st = self.shared.state.lock();
            st.available = self.output.stream().is_some();
            st.attached = self.attached.is_some();
            st.generation = self.generation;
            st.clock = self.model.clock();
            st.heard = self.heard;
            st.queued_us = queued;
            st.ended = self.ended;
            st.broken = self.broken;
            *st
        };
        let before = self.published;
        self.published = state;
        let news = (state.heard && !before.heard)
            || (state.ended.is_some() && state.ended != before.ended)
            || (state.broken && !before.broken)
            || state.available != before.available;
        if news {
            let _ = self.shared.notify.signal();
        }
    }
}
