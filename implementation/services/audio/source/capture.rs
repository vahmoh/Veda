// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Capture: the input device and the capture streams.
//!
//! An input driver produces frames into its link ring and signals
//! `data_event`; [`Capture::run`] then takes the waiting frames (a ring's
//! capacity at most each pass, so that a driver producing without end
//! cannot hold the service's loop), converts them to each stream's channel
//! count and rate, and writes them into the stream's ring (the service is
//! the producer there). A client that does not keep up loses the newest
//! frames, which are counted as overruns; it can never block the device or
//! the other streams.
//!
//! **Echo cancellation.** Streams opened with `echo_cancel` (16 kHz mono,
//! for voice assistants) share one path: the microphone is converted to
//! 16 kHz mono, and the echo canceller removes the system's own playback,
//! taken from the mixer's [`Reference`](crate::mixer::Reference) at the
//! moment the frames were recorded (the driver publishes a capture
//! clock). The canceller ([`audio::aec`]) estimates the remaining delay
//! itself; how well it does is logged once a minute while it works. A
//! stream may ask for more channels beside the cleaned microphone: what
//! was playing when it was recorded (the echo's source), and the
//! microphone as recorded — so a voice assistant can tell its own echo
//! from the user (see `agent::gate`).
//!
//! **Power.** The device records only while some stream is open: the
//! service sets the ring's CAPTURE flag and signals `wake_event` when the
//! first stream opens and clears it when the last one closes.

use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use audio::mix;
use audio::resample::Resampler;
use proto::audio::{
    AudioError, DeviceFormat, InputHandle, InputLink, InputSpec, Ring, RingError, Role, recorded_at, ring,
};
use runtime::object::Event;

use crate::mixer::Mixer;
use audio::aec::EchoCanceller;

/// Capture stream ids carry this bit, so `close` can tell them from
/// playback streams.
pub const ID_BIT: u32 = 0x8000_0000;
/// Rate and channel count of echo-cancelled streams.
pub const ECHO_RATE: u32 = 16_000;
/// Echo-cancelled audio is processed in 10 ms frames.
const ECHO_FRAME: usize = (ECHO_RATE / 100) as usize;
/// How often the canceller's performance is logged.
const ECHO_REPORT_NS: u64 = 60_000_000_000;
/// Echo path length the canceller models.
const ECHO_TAIL_MS: u32 = 250;
/// Largest block taken from the device at once.
const MAX_BLOCK: usize = 4096;
/// Capture streams open at most, and per client.
pub const MAX_INPUT_STREAMS: usize = 16;
const MAX_INPUT_STREAMS_PER_CLIENT: usize = 4;

struct Device {
    ring: Ring,
    data_event: Event,
    wake_event: Event,
    name: String,
    rate: u32,
    channels: usize,
}

struct InStream {
    owner: u64,
    name: String,
    ring: Ring,
    event: Event,
    notify_frames: u32,
    echo_cancel: bool,
    /// Device rate to stream rate (plain streams only).
    resampler: Resampler,
    overruns: u32,
}

/// The shared echo-cancelled 16 kHz mono path.
struct EchoPath {
    to_16k: Resampler,
    canceller: EchoCanceller,
    /// 16 kHz microphone samples not yet processed (the canceller works on
    /// whole frames).
    pending: Vec<i16>,
    /// Time the newest pending sample was recorded.
    pending_end_ns: u64,
    /// The reference and the recorded microphone, delayed like the
    /// canceller's output.
    side: VecDeque<(i16, i16)>,
    report_at_ns: u64,
    /// Scratch, kept from delivery to delivery: what was playing, the
    /// cleaned microphone, and the streams' interleaved frames.
    reference: Vec<i16>,
    cleaned: Vec<i16>,
    wide: Vec<i16>,
    two: Vec<i16>,
}

/// The input device and the capture streams.
pub struct Capture {
    device: Option<Device>,
    streams: BTreeMap<u32, InStream>,
    next_id: u32,
    echo: Option<EchoPath>,
    raw: Vec<i16>,
    converted: Vec<i16>,
    resampled: Vec<i16>,
}

fn ring_err(e: RingError) -> AudioError {
    match e {
        RingError::NoMemory => AudioError::NoMemory,
        RingError::BadHeader => AudioError::BadFormat,
    }
}

impl Capture {
    /// Capture without an input device or streams.
    pub fn new() -> Capture {
        Capture {
            device: None,
            streams: BTreeMap::new(),
            next_id: 1,
            echo: None,
            raw: vec![0; MAX_BLOCK * 2],
            converted: vec![0; MAX_BLOCK * 2],
            resampled: vec![0; MAX_BLOCK * 8],
        }
    }

    /// Attaches a driver's input device.
    pub fn attach(&mut self, f: &DeviceFormat) -> Result<InputLink, AudioError> {
        if self.device.is_some() {
            return Err(AudioError::Busy);
        }
        if !(8_000..=192_000).contains(&f.rate) || !(1..=2).contains(&f.channels) {
            return Err(AudioError::BadFormat);
        }
        // About half a second of slack for a busy service.
        let capacity = (f.rate / 2).max(4096);
        let (ring, vmo) = Ring::create(capacity, f.channels, f.rate, Role::Consumer).map_err(ring_err)?;
        let data_event = Event::create().map_err(|_| AudioError::NoMemory)?;
        let wake_event = Event::create().map_err(|_| AudioError::NoMemory)?;
        let dup = |e: &Event| e.0.duplicate(None).map(Event::from_handle).map_err(|_| AudioError::NoMemory);
        let link = InputLink { ring: vmo, data_event: dup(&data_event)?, wake_event: dup(&wake_event)? };
        self.device = Some(Device {
            ring,
            data_event,
            wake_event,
            name: f.name.clone(),
            rate: f.rate,
            channels: f.channels as usize,
        });
        for s in self.streams.values_mut() {
            if !s.echo_cancel {
                s.resampler = Resampler::new(f.rate, s.ring.rate(), s.ring.channels() as usize);
            }
        }
        if let Some(e) = &mut self.echo {
            e.to_16k = Resampler::new(f.rate, ECHO_RATE, 1);
            e.pending.clear();
        }
        self.update_wanted();
        Ok(link)
    }

    /// The input driver went away.
    pub fn detach(&mut self) {
        self.device = None;
    }

    /// The input device's name (`"none"` without one), rate and channels
    /// (0 without one).
    pub fn device_name(&self) -> &str {
        self.device.as_ref().map_or("none", |d| d.name.as_str())
    }

    pub fn device_rate(&self) -> u32 {
        self.device.as_ref().map_or(0, |d| d.rate)
    }

    pub fn device_channels(&self) -> u32 {
        self.device.as_ref().map_or(0, |d| d.channels as u32)
    }

    /// True if an echo-cancelled stream needs the playback reference.
    pub fn wants_reference(&self) -> bool {
        self.echo.is_some()
    }

    /// The device's data event (to wait on).
    pub fn data_event(&self) -> Option<&Event> {
        self.device.as_ref().map(|d| &d.data_event)
    }

    /// Tells the driver whether to record (some stream is open).
    fn update_wanted(&mut self) {
        let want = !self.streams.is_empty();
        if let Some(d) = &self.device {
            let flags = if want { ring::flags::CAPTURE } else { 0 };
            if d.ring.consumer_flags() != flags {
                d.ring.set_consumer_flags(flags);
                let _ = d.wake_event.signal();
            }
            if !want {
                d.ring.discard_all();
            }
        }
    }

    /// Opens a capture stream for client `owner`.
    pub fn open(&mut self, owner: u64, spec: &InputSpec) -> Result<InputHandle, AudioError> {
        // Echo-cancelled streams have up to three channels: the cleaned
        // microphone, what was playing, the microphone as recorded.
        let max_channels = if spec.echo_cancel { 3 } else { 2 };
        if !(8_000..=48_000).contains(&spec.rate) || !(1..=max_channels).contains(&spec.channels) {
            return Err(AudioError::BadFormat);
        }
        if spec.echo_cancel && spec.rate != ECHO_RATE {
            return Err(AudioError::BadFormat);
        }
        if self.streams.len() >= MAX_INPUT_STREAMS
            || self.streams.values().filter(|s| s.owner == owner).count() >= MAX_INPUT_STREAMS_PER_CLIENT
        {
            return Err(AudioError::TooManyStreams);
        }
        let capacity = spec.buffer_frames.clamp(1024, ring::MAX_CAPACITY).next_power_of_two();
        let (ring, vmo) = Ring::create(capacity, spec.channels, spec.rate, Role::Producer).map_err(ring_err)?;
        let event = Event::create().map_err(|_| AudioError::NoMemory)?;
        let client_event = Event::from_handle(event.0.duplicate(None).map_err(|_| AudioError::NoMemory)?);
        let notify = if spec.notify_frames == 0 { (spec.rate / 50).max(1) } else { spec.notify_frames.min(capacity) };
        let id = ID_BIT | self.next_id;
        self.next_id = (self.next_id + 1) & !ID_BIT;
        if self.next_id == 0 {
            self.next_id = 1;
        }
        let device_rate = self.device_rate();
        let in_rate = if device_rate == 0 { spec.rate } else { device_rate };
        if spec.echo_cancel && self.echo.is_none() {
            let canceller = EchoCanceller::new(ECHO_RATE, ECHO_TAIL_MS);
            let side = core::iter::repeat_n((0, 0), canceller.latency_samples()).collect();
            self.echo = Some(EchoPath {
                to_16k: Resampler::new(in_rate, ECHO_RATE, 1),
                canceller,
                pending: Vec::new(),
                pending_end_ns: 0,
                side,
                report_at_ns: 0,
                reference: Vec::new(),
                cleaned: Vec::new(),
                wide: Vec::new(),
                two: Vec::new(),
            });
        }
        self.streams.insert(
            id,
            InStream {
                owner,
                name: spec.name.chars().take(32).collect(),
                ring,
                event,
                notify_frames: notify,
                echo_cancel: spec.echo_cancel,
                resampler: Resampler::new(in_rate, spec.rate, spec.channels as usize),
                overruns: 0,
            },
        );
        self.update_wanted();
        Ok(InputHandle { id, ring: vmo, event: client_event, device_rate })
    }

    /// Closes client `owner`'s capture stream `id`.
    pub fn close(&mut self, owner: u64, id: u32) -> Result<(), AudioError> {
        if !self.streams.get(&id).is_some_and(|s| s.owner == owner) {
            return Err(AudioError::NoSuchStream);
        }
        if let Some(s) = self.streams.remove(&id) {
            runtime::println!("capture stream {} \"{}\" closed ({} overruns)", id & !ID_BIT, s.name, s.overruns);
        }
        self.after_close();
        Ok(())
    }

    /// Closes every capture stream of a client that disconnected.
    pub fn close_owner(&mut self, owner: u64) {
        let before = self.streams.len();
        self.streams.retain(|_, s| s.owner != owner);
        if self.streams.len() != before {
            self.after_close();
        }
    }

    fn after_close(&mut self) {
        if !self.streams.values().any(|s| s.echo_cancel) {
            self.echo = None;
        }
        self.update_wanted();
    }

    /// Delivers what the device recorded to the streams: at most a ring's
    /// capacity each pass; if more is waiting, the device's event is left
    /// signalled, so that the loop comes back for it after its other work.
    pub fn run(&mut self, mixer: &Mixer, now: u64) {
        let Some(dev) = &self.device else { return };
        let _ = dev.data_event.clear();
        let dch = dev.channels;
        let rate = dev.rate;
        let mut budget = dev.ring.capacity() as usize;
        while budget > 0 {
            let Some(dev) = &self.device else { return };
            let waiting = dev.ring.filled() as usize;
            if waiting == 0 {
                return;
            }
            let n = waiting.min(MAX_BLOCK).min(budget);
            // When the newest frame in the ring was recorded, and so the
            // last of those taken now.
            let clock = dev.ring.capture_clock();
            let got = dev.ring.read(&mut self.raw[..n * dch]);
            if got == 0 {
                return;
            }
            budget -= got;
            let end_ns = recorded_at(clock, dev.ring.read_pos(), rate).unwrap_or(now);
            self.deliver(got, dch, end_ns, mixer);
        }
        if let Some(dev) = &self.device
            && dev.ring.filled() > 0
        {
            let _ = dev.data_event.signal();
        }
    }

    /// Converts `n` device frames in `self.raw` for every stream.
    fn deliver(&mut self, n: usize, dch: usize, end_ns: u64, mixer: &Mixer) {
        let raw = &self.raw[..n * dch];
        for s in self.streams.values_mut().filter(|s| !s.echo_cancel) {
            let sch = s.ring.channels() as usize;
            let src: &[i16] = if sch == dch {
                raw
            } else {
                mix::convert_channels(raw, dch, &mut self.converted[..n * sch], sch);
                &self.converted[..n * sch]
            };
            s.resampler.push(src);
            loop {
                let got = s.resampler.pull(&mut self.resampled[..MAX_BLOCK * sch]);
                if got == 0 {
                    break;
                }
                write_stream(s, &self.resampled[..got * sch], end_ns);
            }
        }
        if let Some(echo) = &mut self.echo {
            let mono: &[i16] = if dch == 1 {
                raw
            } else {
                mix::convert_channels(raw, dch, &mut self.converted[..n], 1);
                &self.converted[..n]
            };
            echo.to_16k.push(mono);
            loop {
                let got = echo.to_16k.pull(&mut self.resampled[..MAX_BLOCK]);
                if got == 0 {
                    break;
                }
                echo.pending.extend_from_slice(&self.resampled[..got]);
            }
            echo.pending_end_ns = end_ns;
            let frame = ECHO_FRAME;
            let whole = echo.pending.len() / frame * frame;
            if whole == 0 {
                return;
            }
            // The reference that was playing while these samples were
            // recorded (the newest pending sample was recorded at
            // `pending_end_ns`).
            echo.reference.resize(whole, 0);
            let backlog_ns = (echo.pending.len() - whole) as u64 * 1_000_000_000 / ECHO_RATE as u64;
            mixer.reference(echo.pending_end_ns.saturating_sub(backlog_ns), &mut echo.reference);
            echo.cleaned.resize(whole, 0);
            for (i, chunk) in echo.pending[..whole].chunks_exact(frame).enumerate() {
                let r = &echo.reference[i * frame..(i + 1) * frame];
                echo.canceller.process(chunk, r, &mut echo.cleaned[i * frame..(i + 1) * frame]);
            }
            // Beside the cleaned microphone (for the streams that want
            // them): what was playing and the microphone as recorded.
            let widest = self.streams.values().filter(|s| s.echo_cancel).map(|s| s.ring.channels()).max();
            echo.wide.clear();
            echo.two.clear();
            if widest.is_some_and(|c| c > 1) {
                for ((&c, &r), &m) in echo.cleaned.iter().zip(&echo.reference).zip(&echo.pending[..whole]) {
                    echo.side.push_back((r, m));
                    let (r, m) = echo.side.pop_front().unwrap_or((0, 0));
                    echo.wide.extend_from_slice(&[c, r, m]);
                    echo.two.extend_from_slice(&[c, r]);
                }
            }
            echo.pending.drain(..whole);
            for s in self.streams.values_mut().filter(|s| s.echo_cancel) {
                match s.ring.channels() {
                    1 => write_stream(s, &echo.cleaned, end_ns),
                    2 => write_stream(s, &echo.two, end_ns),
                    _ => write_stream(s, &echo.wide, end_ns),
                }
            }
            if end_ns >= echo.report_at_ns {
                if echo.report_at_ns != 0 && echo.canceller.converged() {
                    let c = &echo.canceller;
                    match c.echo_delay_ms() {
                        Some(d) => {
                            runtime::println!("echo cancellation: {:.0} dB, echo delay {:.0} ms", c.erle_db(), d)
                        }
                        None => runtime::println!("echo cancellation: {:.0} dB", c.erle_db()),
                    }
                }
                echo.report_at_ns = end_ns + ECHO_REPORT_NS;
            }
        }
    }
}

/// Appends frames to a stream (dropping what does not fit), publishes when
/// the newest was recorded and wakes the reader.
fn write_stream(s: &mut InStream, frames: &[i16], recorded_ns: u64) {
    let ch = s.ring.channels() as usize;
    let written = s.ring.write(frames);
    let lost = frames.len() / ch - written;
    if lost > 0 {
        s.overruns = s.overruns.saturating_add(lost as u32);
        s.ring.set_overruns(s.overruns);
    }
    s.ring.set_capture_clock(s.ring.write_pos(), recorded_ns);
    if s.ring.filled() >= s.notify_frames {
        let _ = s.event.signal();
    }
}
