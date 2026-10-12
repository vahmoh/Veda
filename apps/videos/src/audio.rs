//! The audio thread: plays the decoder's sound and keeps the clock.
//!
//! It takes the sound the decoder writes into the ring (records of a
//! generation, each saying when it is heard), copies it into an output
//! stream of the audio service, and notes where each piece came from. How
//! much of the stream has been played then says which moment of the video
//! is heard: the clock the pictures are shown by
//! (`videos_core::clock`), published for the window's thread.
//!
//! Every call to the audio service is made here (its client is not shared
//! between threads); the window's thread sends [`Command`]s.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use vabi::{WaitItem, signals};
use videos_core::clock::{AudioMap, Clock, ClockModel};
use vproto::audio::{OutputStream, StreamSpec, audio, ring as stream_ring};
use vproto::media::{PcmRing, pcm};
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

/// What the audio thread says.
#[derive(Debug, Clone, Default)]
pub struct State {
    /// The audio service was reached and a stream can be opened.
    pub available: bool,
    /// The output device ("VirtIO SoundCard (Linux)").
    pub device: String,
    /// The rate sound is played at.
    pub rate: u32,
    /// A ring is attached.
    pub attached: bool,
    /// The clock, once sound is heard; standing still otherwise.
    pub clock: Clock,
    /// Sound of the current generation has been heard.
    pub heard: bool,
    /// Sound of the current generation queued and not yet heard (µs).
    pub buffered_us: i64,
    /// The sound of this generation ended, and has all been heard.
    pub ended: Option<u32>,
    /// The ring held something no decoder writes: the decoder is broken.
    pub broken: bool,
}

pub struct Shared {
    commands: Mutex<VecDeque<Command>>,
    wake: Event,
    /// Signalled when something the window shows changed (ended, broken).
    pub notify: Event,
    state: Mutex<State>,
}

impl Shared {
    pub fn send(&self, c: Command) {
        self.commands.lock().push_back(c);
        let _ = self.wake.signal();
    }

    pub fn state(&self) -> State {
        self.state.lock().clone()
    }

    /// A ring is attached (a video with sound is open).
    pub fn attached(&self) -> bool {
        self.state.lock().attached
    }

    /// The clock of the sound of `generation` while it is heard: not before
    /// its first sound, nor after it ended or broke.
    pub fn clock_for(&self, generation: u32) -> Option<Clock> {
        let s = self.state.lock();
        let live = s.heard && s.clock.generation == generation && s.ended != Some(generation) && !s.broken;
        live.then_some(s.clock)
    }

    /// Whether the sound of `generation` may still be heard.
    pub fn live(&self, generation: u32) -> bool {
        let s = self.state.lock();
        !s.broken && s.ended != Some(generation)
    }
}

/// Starts the audio thread.
pub fn start(volume: f32) -> Option<Arc<Shared>> {
    let shared = Arc::new(Shared {
        commands: Mutex::new(VecDeque::new()),
        wake: Event::create().ok()?,
        notify: Event::create().ok()?,
        state: Mutex::new(State { rate: DEFAULT_RATE, ..State::default() }),
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

struct Player {
    shared: Arc<Shared>,
    client: Option<audio::Client>,
    stream: Option<OutputStream>,
    rate: u32,
    volume: f32,
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
    buf: Vec<i16>,
}

impl Player {
    fn new(shared: Arc<Shared>, volume: f32) -> Player {
        Player {
            shared,
            client: None,
            stream: None,
            rate: DEFAULT_RATE,
            volume,
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
            buf: vec![0; 2 * pcm::MAX_FRAMES as usize],
        }
    }

    fn connect(&mut self) {
        if !crate::service_registered(audio::NAME) {
            println!("no sound: the audio service is not running");
            return;
        }
        let Some(client) = vproto::connect(audio::NAME).ok().map(audio::Client::new) else { return };
        let mut device = String::new();
        if let Ok(st) = client.status() {
            if st.rate >= 8_000 {
                self.rate = st.rate;
            }
            device = st.device.clone();
        }
        self.map = AudioMap::new(self.rate);
        let mut spec = StreamSpec::stereo(self.rate, BUFFER_MS, "Videos");
        spec.paused = true;
        spec.volume = self.volume;
        match OutputStream::open(&client, spec) {
            Ok(s) => self.stream = Some(s),
            Err(e) => {
                println!("no sound: cannot open a stream ({e:?})");
                return;
            }
        }
        self.client = Some(client);
        let mut st = self.shared.state.lock();
        st.available = true;
        st.device = device;
        st.rate = self.rate;
    }

    fn run(&mut self) {
        self.connect();
        loop {
            self.handle_commands();
            self.pump();
            self.publish();
            let mut items =
                vec![WaitItem { handle: self.shared.wake.raw(), signals: signals::SIGNALED, ..Default::default() }];
            if let Some(s) = &self.stream {
                items.push(WaitItem { handle: s.event().raw(), signals: signals::SIGNALED, ..Default::default() });
            }
            if let Some(a) = &self.attached {
                items.push(WaitItem { handle: a.ready.raw(), signals: signals::SIGNALED, ..Default::default() });
            }
            let deadline = if self.playing || self.stopping.is_some() {
                vrt::time::now_ns() + TICK_NS
            } else {
                vabi::DEADLINE_INFINITE
            };
            let _ = vrt::object::wait_many(&mut items, deadline);
            let _ = self.shared.wake.clear();
            if let Some(s) = &self.stream {
                let _ = s.event().clear();
            }
            if let Some(a) = &self.attached {
                let _ = a.ready.clear();
            }
        }
    }

    fn handle_commands(&mut self) {
        loop {
            let Some(c) = self.shared.commands.lock().pop_front() else { return };
            match c {
                Command::Attach { ring, ready, wake } => {
                    if ring.rate() != self.rate {
                        println!("sound at {} Hz for a stream at {} Hz: not played", ring.rate(), self.rate);
                        continue;
                    }
                    self.attached = Some(Attached { ring, ready, wake });
                    self.flush(0);
                    self.broken = false;
                }
                Command::Detach => {
                    self.attached = None;
                    self.set_paused(true);
                    self.flush(0);
                }
                Command::Play => {
                    self.playing = true;
                    self.stopping = None;
                    self.resumed_ns = vrt::time::now_ns();
                    self.set_paused(false);
                }
                Command::Pause => {
                    // The clock goes on with what still plays, and stops
                    // where the sound does.
                    self.playing = false;
                    self.set_paused(true);
                    self.stopping = Some(vrt::time::now_ns());
                }
                Command::Flush { generation } => self.flush(generation),
                Command::Volume(v) => {
                    self.volume = v;
                    if let (Some(c), Some(s)) = (&self.client, &self.stream) {
                        let _ = c.set_volume(s.id, v);
                    }
                }
            }
        }
    }

    fn set_paused(&mut self, paused: bool) {
        if let (Some(c), Some(s)) = (&self.client, &self.stream) {
            let _ = c.set_paused(s.id, paused);
        }
    }

    /// Empties the stream: what follows is `generation`'s sound.
    fn flush(&mut self, generation: u32) {
        if let (Some(c), Some(s)) = (&self.client, &self.stream) {
            let _ = c.flush(s.id);
            s.ring().set_producer_flags(0);
        }
        self.map.clear();
        self.generation = generation;
        self.end_read = false;
        self.ended = None;
        self.start = None;
        self.heard = false;
        self.stopping = None;
        let at = self.model.clock().at(vrt::time::now_ns());
        self.model.stand(at, generation);
    }

    /// Moves sound from the decoder's ring into the stream, as it fits.
    fn pump(&mut self) {
        let (Some(a), Some(s)) = (&self.attached, &self.stream) else { return };
        let mut free = s.free() as usize;
        let mut read = false;
        loop {
            let r = match a.ring.peek() {
                Ok(Some(r)) => r,
                Ok(None) => break,
                Err(_) => {
                    if !self.broken {
                        println!("the decoder's sound does not hold together: not played");
                        self.broken = true;
                        let _ = self.shared.notify.signal();
                    }
                    break;
                }
            };
            if r.kind == pcm::kind::END {
                if r.generation == self.generation {
                    self.end_read = true;
                    s.ring().set_producer_flags(stream_ring::flags::END);
                }
                a.ring.take(&r, 0);
                read = true;
                continue;
            }
            if r.generation != self.generation {
                a.ring.take(&r, r.frames);
                read = true;
                continue;
            }
            if free == 0 {
                break;
            }
            let n = a.ring.copy(&r, &mut self.buf[..free.min(pcm::MAX_FRAMES as usize) * 2]);
            let at = s.written();
            self.start.get_or_insert(at);
            let w = s.write(&self.buf[..n * 2]);
            self.map.push(at, r.next_pts(self.rate), w as u64);
            a.ring.take(&r, w as u32);
            free -= w;
            read = true;
        }
        if read {
            let _ = a.wake.signal();
        }
    }

    /// Publishes the clock, what is buffered, and whether the sound ended.
    fn publish(&mut self) {
        let now = vrt::time::now_ns();
        let mut ended_now = false;
        if let Some(s) = &self.stream {
            // Run on from the service's last report of what was played, but
            // not from one made before the stream started again (paused, or
            // emptied by a seek): that would run ahead of what is heard.
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
            if self.end_read
                && self.ended != Some(self.generation)
                && s.queued() == 0
                && played + (self.rate / 100) as u64 >= s.written()
            {
                self.ended = Some(self.generation);
                ended_now = true;
            }
        }
        let buffered = match (&self.stream, self.map.end_us()) {
            (Some(_), Some(end)) => end.saturating_sub(self.model.clock().at(now)).max(0),
            _ => 0,
        };
        {
            let mut st = self.shared.state.lock();
            st.attached = self.attached.is_some();
            st.clock = self.model.clock();
            st.heard = self.heard;
            st.buffered_us = buffered;
            st.ended = self.ended;
            st.broken = self.broken;
        }
        if ended_now {
            let _ = self.shared.notify.signal();
        }
    }
}
