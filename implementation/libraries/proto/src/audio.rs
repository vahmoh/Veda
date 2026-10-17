// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The audio protocols.
//!
//! * [`audio`] (service `"audio"`): applications open playback streams,
//!   pause, flush and adjust them, and query or set the master volume.
//!   The `audio` service mixes every stream (resampling as needed) into the
//!   sound device's format. Applications also open capture (microphone)
//!   streams, optionally with the system's own playback removed (echo
//!   cancellation), in the rate and channel count they want.
//! * [`audiodev`] (service `"audiodev"`, also provided by the `audio`
//!   service): a sound driver attaches its output device and, separately,
//!   its input device. The driver connects to the audio service, never the
//!   other way round, so the service keeps working (with a silent "null"
//!   device that consumes audio in real time) on machines without sound
//!   hardware.
//!
//! # Shared rings
//!
//! PCM never travels in channel messages. Every stream, and the link to the
//! device, is a single-producer/single-consumer ring of interleaved `i16`
//! frames in a shared VMO ([`Ring`], layout in [`ring`]). The producer only
//! advances `write_pos`, the consumer only `read_pos`; both are running
//! frame counters, so `write_pos - read_pos` is the fill level. Each side
//! keeps its own counter privately and never trusts the peer's value
//! beyond clamping it to the capacity, so a misbehaving peer can garble
//! its own audio but not the other side's state.
//!
//! The consumer also publishes `played_pos` (frames that have actually
//! been played) with the moment it got there, which lets a player show an
//! accurate position and drive a visualiser from the samples that are
//! audible right now ([`OutputStream::played_now`]). Such a pair (and the
//! capture clock's, below) is published under a sequence count, odd while
//! it is being written, so that a reader never takes the position of one
//! moment with the time of another (a seqlock: [`Ring::played`]).
//!
//! # Flow control
//!
//! A stream has an event that the audio service signals whenever at least
//! `notify_frames` frames are free (and when its state changes). A client
//! clears the event, writes as much as fits, and waits on the event again
//! ([`OutputStream::wait_writable`]). Between the service and the driver,
//! `data_event` says "new data in the ring" and `space_event` says "the
//! device consumed data".
//!
//! # Capture
//!
//! Capture runs the other way: the input driver produces frames into its
//! link ring and signals `data_event`; the service distributes them to every
//! input stream, whose rings it produces into and whose clients consume
//! ([`InputStream`]). The service sets [`ring::flags::CAPTURE`] on the
//! driver's ring while any input stream is open (and signals `wake_event`),
//! so the device records only while someone listens. Producers of captured
//! audio publish when their latest frame was recorded
//! ([`Ring::set_capture_clock`]), which lets the service line the
//! microphone up with what the speakers played for echo cancellation.

use alloc::string::String;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use ipc::{enumeration, message, protocol};
use rt::object::{Event, Vmo};
use rt::vm::Mapping;

enumeration! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum AudioError {
        /// No stream with this id belongs to the caller.
        NoSuchStream = 1,
        /// Unsupported rate, channel count or buffer size.
        BadFormat = 2,
        /// Out of memory creating the ring.
        NoMemory = 3,
        /// The per-client or global stream limit was reached.
        TooManyStreams = 4,
        /// A device is already attached.
        Busy = 5,
        /// The caller may not do that: only devmgr's drivers attach devices
        /// and say what is coming, and only the voice agent ducks the
        /// other sound.
        Denied = 6,
    }
}

impl core::fmt::Display for AudioError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            AudioError::NoSuchStream => "no such stream",
            AudioError::BadFormat => "unsupported audio format",
            AudioError::NoMemory => "out of memory",
            AudioError::TooManyStreams => "too many streams",
            AudioError::Busy => "device already attached",
            AudioError::Denied => "not allowed",
        })
    }
}

message! {
    /// Parameters of a new playback stream.
    #[derive(Debug, Clone, PartialEq)]
    pub struct StreamSpec {
        /// Sample rate of the PCM the client writes (8 000 ..= 192 000 Hz).
        pub rate: u32,
        /// 1 (mono) or 2 (stereo, interleaved). Samples are `i16`.
        pub channels: u32,
        /// Ring capacity in frames (rounded up to a power of two, clamped
        /// to 1 024 ..= 262 144). More buffering survives longer stalls of
        /// the client; less means lower latency.
        pub buffer_frames: u32,
        /// The stream event is signalled whenever at least this many frames
        /// are free (0 = a quarter of the capacity).
        pub notify_frames: u32,
        /// A name for diagnostics ("Music").
        pub name: String,
        /// Start paused.
        pub paused: bool,
        /// Initial volume, a linear factor in 0..=1.
        pub volume: f32,
    }
}

impl StreamSpec {
    /// A stereo stream at `rate` with about `buffer_ms` of buffering.
    pub fn stereo(rate: u32, buffer_ms: u32, name: &str) -> StreamSpec {
        StreamSpec {
            rate,
            channels: 2,
            buffer_frames: (rate as u64 * buffer_ms as u64 / 1000) as u32,
            notify_frames: 0,
            name: name.into(),
            paused: false,
            volume: 1.0,
        }
    }
}

message! {
    /// A newly opened stream.
    #[derive(Debug)]
    pub struct StreamHandle {
        pub id: u32,
        /// The shared ring (the client is the producer).
        pub ring: Vmo,
        /// Signalled when space frees up or the stream's state changes.
        pub event: Event,
        /// Rate of the output device (the service resamples to it).
        pub device_rate: u32,
    }
}

message! {
    /// Parameters of a new capture (microphone) stream.
    #[derive(Debug, Clone, PartialEq)]
    pub struct InputSpec {
        /// Sample rate the client wants (8 000 ..= 48 000 Hz).
        pub rate: u32,
        /// 1 (mono) or 2 (stereo, interleaved); see `echo_cancel` for its
        /// third channel.
        pub channels: u32,
        /// Ring capacity in frames (rounded up to a power of two, clamped
        /// to 1 024 ..= 262 144). Frames the client does not read in time
        /// are dropped and counted as overruns.
        pub buffer_frames: u32,
        /// The stream event is signalled whenever at least this many frames
        /// are waiting (0 = 20 ms worth).
        pub notify_frames: u32,
        /// A name for diagnostics ("Agent").
        pub name: String,
        /// Remove what the system itself plays from the signal (acoustic
        /// echo cancellation), so a voice assistant can listen while it
        /// talks. Needs a stream at 16 000 Hz: its first channel is the
        /// cleaned microphone, a second one what was playing when it was
        /// recorded (the echo's source), a third the microphone as
        /// recorded.
        pub echo_cancel: bool,
    }
}

impl InputSpec {
    /// A mono stream at `rate` with about `buffer_ms` of buffering.
    pub fn mono(rate: u32, buffer_ms: u32, name: &str) -> InputSpec {
        InputSpec {
            rate,
            channels: 1,
            buffer_frames: (rate as u64 * buffer_ms as u64 / 1000) as u32,
            notify_frames: 0,
            name: name.into(),
            echo_cancel: false,
        }
    }
}

message! {
    /// A newly opened capture stream.
    #[derive(Debug)]
    pub struct InputHandle {
        pub id: u32,
        /// The shared ring (the service is the producer).
        pub ring: Vmo,
        /// Signalled when frames are waiting.
        pub event: Event,
        /// Rate of the input device (the service resamples from it), 0
        /// while there is no input device.
        pub device_rate: u32,
    }
}

message! {
    /// State of the audio system.
    #[derive(Debug, Clone, PartialEq)]
    pub struct AudioStatus {
        /// Output device ("VirtIO SoundCard (Linux)", or "none" when there
        /// is no sound hardware and audio is consumed silently in real
        /// time).
        pub device: String,
        /// No output device is attached yet, but one is coming: `devmgr`
        /// gave a sound card to its driver ([`audiodev::expect`]).
        pub coming: bool,
        pub rate: u32,
        pub channels: u32,
        /// Frames per device period.
        pub period_frames: u32,
        /// Output latency (mixer to device).
        pub latency_ns: u64,
        pub master_volume: f32,
        pub muted: bool,
        /// Open streams.
        pub streams: u32,
        /// Device underruns since boot.
        pub underruns: u64,
        /// Input device ("none" without a microphone).
        pub input_device: String,
        pub input_rate: u32,
        pub input_channels: u32,
    }
}

protocol! {
    /// The system audio service.
    pub mod audio = "audio" {
        /// Opens a playback stream (closed with `close` or when the
        /// connection closes).
        1 => fn open_output(spec: StreamSpec) -> Result<StreamHandle, AudioError>;
        /// Closes a playback or capture stream.
        2 => fn close(stream: u32) -> Result<(), AudioError>;
        /// Pauses or resumes a stream (a paused stream keeps its data).
        3 => fn set_paused(stream: u32, paused: bool) -> Result<(), AudioError>;
        /// Sets a stream's volume (linear factor 0..=1; changes are ramped).
        4 => fn set_volume(stream: u32, volume: f32) -> Result<(), AudioError>;
        /// Discards everything queued in the stream (e.g. before a seek).
        /// Returns the new consumed/played position (= frames written).
        5 => fn flush(stream: u32) -> Result<u64, AudioError>;
        7 => fn status() -> AudioStatus;
        /// Sets the master volume (linear 0..=1) and mute state.
        8 => fn set_master(volume: f32, muted: bool) -> ();
        /// Opens a capture stream from the microphone (closed with `close`
        /// or when the connection closes).
        9 => fn open_input(spec: InputSpec) -> Result<InputHandle, AudioError>;
        /// While on, every other client's streams play quieter (the voice
        /// agent in a conversation: music under it, so that it hears the
        /// user); off again when the connection closes. Only the voice
        /// agent may (`Denied`).
        11 => fn set_ducking(on: bool) -> Result<(), AudioError>;
    }
}

message! {
    /// The output format a sound driver configured.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct DeviceFormat {
        pub name: String,
        pub rate: u32,
        pub channels: u32,
        /// Frames per hardware period (one transfer).
        pub period_frames: u32,
        /// Most periods the driver keeps queued in the device.
        pub max_periods: u32,
    }
}

message! {
    /// The shared state between the audio service and a driver.
    #[derive(Debug)]
    pub struct DeviceLink {
        /// Mixed output in the device format (service produces, driver
        /// consumes; the driver publishes `played_pos` and its state).
        pub ring: Vmo,
        /// Service → driver: new data is in the ring.
        pub data_event: Event,
        /// Driver → service: the device consumed data or changed state.
        pub space_event: Event,
    }
}

message! {
    /// The shared state between the audio service and an input driver.
    #[derive(Debug)]
    pub struct InputLink {
        /// Captured audio in the device format (driver produces, service
        /// consumes; the service sets [`ring::flags::CAPTURE`] while it
        /// wants audio).
        pub ring: Vmo,
        /// Driver → service: new frames are in the ring.
        pub data_event: Event,
        /// Service → driver: the CAPTURE flag changed.
        pub wake_event: Event,
    }
}

protocol! {
    /// Sound drivers attach their devices to the audio service. Only the
    /// drivers devmgr starts may (any other program's connection is
    /// closed); the test microphone (`testmic`) may attach an input device,
    /// which a driver's then replaces.
    pub mod audiodev = "audiodev" {
        1 => fn attach(format: DeviceFormat) -> Result<DeviceLink, AudioError>;
        /// Attaches an input (capture) device. `format.max_periods` is
        /// unused.
        2 => fn attach_input(format: DeviceFormat) -> Result<InputLink, AudioError>;
        /// (devmgr) Whether a sound card's driver is starting, and the
        /// card's name (for the log): its output device is coming. It holds
        /// while the connection is open.
        3 => fn expect(device: Option<String>) -> Result<(), AudioError>;
    }
}

/// Layout of a shared ring VMO (all fields little-endian, naturally
/// aligned; producer and consumer fields live on separate cache lines).
pub mod ring {
    /// `"VRNG"`.
    pub const MAGIC: u32 = u32::from_le_bytes(*b"VRNG");
    pub const VERSION: u32 = 1;
    /// Header fields written once by the creator.
    pub const OFF_MAGIC: usize = 0;
    pub const OFF_VERSION: usize = 4;
    /// Capacity in frames (a power of two).
    pub const OFF_CAPACITY: usize = 8;
    pub const OFF_CHANNELS: usize = 12;
    pub const OFF_RATE: usize = 16;
    /// Byte offset of the sample data.
    pub const OFF_DATA: usize = 20;
    /// Producer: frames written (u64).
    pub const OFF_WRITE_POS: usize = 64;
    /// Producer: flags (u32, [`flags`]).
    pub const OFF_PRODUCER_FLAGS: usize = 72;
    /// Producer of captured audio: frames dropped because the ring was
    /// full (u32).
    pub const OFF_OVERRUNS: usize = 76;
    /// Producer of captured audio: the frame position (u64) and the
    /// monotonic time in ns (u64) at which that frame was recorded ...
    pub const OFF_CAPTURE_POS: usize = 80;
    pub const OFF_CAPTURE_NS: usize = 88;
    /// ... and their sequence count (u32, odd while they are written).
    pub const OFF_CAPTURE_SEQ: usize = 96;
    /// Consumer: frames consumed (u64).
    pub const OFF_READ_POS: usize = 128;
    /// Consumer: frames played (u64).
    pub const OFF_PLAYED_POS: usize = 136;
    /// Consumer: monotonic time of `played_pos` in ns (u64).
    pub const OFF_PLAYED_NS: usize = 144;
    /// Consumer: state flags (u32, [`flags`]).
    pub const OFF_CONSUMER_FLAGS: usize = 152;
    /// Consumer: underrun count (u32).
    pub const OFF_UNDERRUNS: usize = 156;
    /// Consumer: frames between `read_pos` and the output (u32).
    pub const OFF_LATENCY: usize = 160;
    /// Consumer: the sequence count of `played_pos` and `played_ns` (u32,
    /// odd while they are written).
    pub const OFF_PLAYED_SEQ: usize = 164;
    /// Where the samples start.
    pub const DATA_OFFSET: usize = 4096;
    pub const MIN_CAPACITY: u32 = 256;
    pub const MAX_CAPACITY: u32 = 1 << 18;

    /// Bits of the flag words.
    pub mod flags {
        /// Consumer: the stream is paused.
        pub const PAUSED: u32 = 1;
        /// Consumer: the device is running (driver link).
        pub const RUNNING: u32 = 2;
        /// Producer: no more data will follow (end of stream).
        pub const END: u32 = 4;
        /// Producer (device link): streams are playing, so running out of
        /// data is an underrun rather than the end of playback.
        pub const ACTIVE: u32 = 8;
        /// Consumer (input device link): capture is wanted.
        pub const CAPTURE: u32 = 16;
    }
}

/// Why a ring could not be created or mapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RingError {
    NoMemory,
    BadHeader,
}

/// Which side of a ring this process is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Producer,
    Consumer,
}

/// How often a reader tries to read a pair published under a sequence
/// count before it settles for the last pair it read whole: a writer takes
/// a few instructions, but may be interrupted half way, and a peer that
/// never finishes must not hold the reader.
const SEQ_ATTEMPTS: usize = 16;

/// One side of a shared SPSC ring of interleaved `i16` frames.
pub struct Ring {
    base: *mut u8,
    capacity: u32,
    channels: u32,
    rate: u32,
    role: Role,
    /// Our own position (write for the producer, read for the consumer);
    /// the shared copy is only published, never trusted.
    local: core::cell::Cell<u64>,
    /// Our own sequence counts of the pairs we publish (the consumer's
    /// played position, the producer's capture clock).
    played_seq: core::cell::Cell<u32>,
    clock_seq: core::cell::Cell<u32>,
    /// The last pairs read whole from the peer.
    played_seen: core::cell::Cell<(u64, u64)>,
    clock_seen: core::cell::Cell<(u64, u64)>,
    _map: Option<Mapping>,
}

// SAFETY: the ring is only used from one thread at a time on each side;
// shared fields are accessed atomically.
unsafe impl Send for Ring {}

impl core::fmt::Debug for Ring {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Ring({:?}, {} frames x {} ch @ {} Hz)", self.role, self.capacity, self.channels, self.rate)
    }
}

/// Bytes needed for a ring of `capacity` frames.
pub fn ring_bytes(capacity: u32, channels: u32) -> usize {
    (ring::DATA_OFFSET + capacity as usize * channels as usize * 2).next_multiple_of(4096)
}

impl Ring {
    /// Creates a ring in a new VMO. Returns our side and a VMO handle for
    /// the peer.
    pub fn create(capacity: u32, channels: u32, rate: u32, role: Role) -> Result<(Ring, Vmo), RingError> {
        let capacity = capacity.clamp(ring::MIN_CAPACITY, ring::MAX_CAPACITY).next_power_of_two();
        let channels = channels.clamp(1, 8);
        let len = ring_bytes(capacity, channels);
        let vmo = Vmo::create(len).map_err(|_| RingError::NoMemory)?;
        let peer = Vmo::from_handle(vmo.0.duplicate(None).map_err(|_| RingError::NoMemory)?);
        let map =
            Mapping::new(vmo, len, abi::map_flags::READ | abi::map_flags::WRITE).map_err(|_| RingError::NoMemory)?;
        // SAFETY: fresh mapping of `len` bytes.
        let ring = unsafe { Ring::init_raw(map.as_ptr(), len, capacity, channels, rate, role) };
        Ok((Ring { _map: Some(map), ..ring }, peer))
    }

    /// Maps a ring created by the peer, validating its header.
    pub fn map(vmo: Vmo, role: Role) -> Result<Ring, RingError> {
        let len = vmo.size().map_err(|_| RingError::BadHeader)?;
        if len < ring::DATA_OFFSET + 4096 {
            return Err(RingError::BadHeader);
        }
        let map =
            Mapping::new(vmo, len, abi::map_flags::READ | abi::map_flags::WRITE).map_err(|_| RingError::NoMemory)?;
        // SAFETY: the mapping is `len` bytes long.
        let ring = unsafe { Ring::attach_raw(map.as_ptr(), len, role)? };
        Ok(Ring { _map: Some(map), ..ring })
    }

    /// Initialises a ring header in raw memory.
    ///
    /// # Safety
    /// `base` must be valid for `len` bytes (at least
    /// [`ring_bytes`]`(capacity, channels)`), 8-byte aligned and outlive
    /// the ring.
    pub unsafe fn init_raw(base: *mut u8, len: usize, capacity: u32, channels: u32, rate: u32, role: Role) -> Ring {
        assert!(len >= ring_bytes(capacity, channels) && capacity.is_power_of_two());
        // SAFETY: the caller guarantees the memory; the header is ours.
        unsafe {
            core::ptr::write_bytes(base, 0, ring::DATA_OFFSET);
            let w = |off: usize, v: u32| (base.add(off) as *mut u32).write_volatile(v);
            w(ring::OFF_VERSION, ring::VERSION);
            w(ring::OFF_CAPACITY, capacity);
            w(ring::OFF_CHANNELS, channels);
            w(ring::OFF_RATE, rate);
            w(ring::OFF_DATA, ring::DATA_OFFSET as u32);
            core::sync::atomic::fence(Ordering::Release);
            w(ring::OFF_MAGIC, ring::MAGIC);
        }
        Ring::new(base, capacity, channels, rate, role)
    }

    fn new(base: *mut u8, capacity: u32, channels: u32, rate: u32, role: Role) -> Ring {
        use core::cell::Cell;
        Ring {
            base,
            capacity,
            channels,
            rate,
            role,
            local: Cell::new(0),
            played_seq: Cell::new(0),
            clock_seq: Cell::new(0),
            played_seen: Cell::new((0, 0)),
            clock_seen: Cell::new((0, 0)),
            _map: None,
        }
    }

    /// Attaches to a ring header in raw memory created by the peer.
    ///
    /// # Safety
    /// `base` must be valid for `len` bytes, 8-byte aligned and outlive the
    /// ring.
    pub unsafe fn attach_raw(base: *mut u8, len: usize, role: Role) -> Result<Ring, RingError> {
        // SAFETY: inside the caller's memory.
        let r = |off: usize| unsafe { (base.add(off) as *const u32).read_volatile() };
        if len < ring::DATA_OFFSET || r(ring::OFF_MAGIC) != ring::MAGIC || r(ring::OFF_VERSION) != ring::VERSION {
            return Err(RingError::BadHeader);
        }
        let (capacity, channels, rate) = (r(ring::OFF_CAPACITY), r(ring::OFF_CHANNELS), r(ring::OFF_RATE));
        if !capacity.is_power_of_two()
            || !(ring::MIN_CAPACITY..=ring::MAX_CAPACITY).contains(&capacity)
            || !(1..=8).contains(&channels)
            || r(ring::OFF_DATA) as usize != ring::DATA_OFFSET
            || len < ring_bytes(capacity, channels)
        {
            return Err(RingError::BadHeader);
        }
        let ring = Ring::new(base, capacity, channels, rate, role);
        // Resume from the published position and counts of our own side
        // (counts even: nothing is being written).
        let start = match role {
            Role::Producer => ring.u64_at(ring::OFF_WRITE_POS).load(Ordering::Acquire),
            Role::Consumer => ring.u64_at(ring::OFF_READ_POS).load(Ordering::Acquire),
        };
        ring.local.set(start);
        let even = |off: usize| ring.u32_at(off).load(Ordering::Acquire).wrapping_add(1) & !1;
        ring.played_seq.set(even(ring::OFF_PLAYED_SEQ));
        ring.clock_seq.set(even(ring::OFF_CAPTURE_SEQ));
        Ok(ring)
    }

    fn u64_at(&self, off: usize) -> &AtomicU64 {
        // SAFETY: header offsets are inside the mapping and 8-byte aligned.
        unsafe { &*(self.base.add(off) as *const AtomicU64) }
    }

    fn u32_at(&self, off: usize) -> &AtomicU32 {
        // SAFETY: header offsets are inside the mapping and 4-byte aligned.
        unsafe { &*(self.base.add(off) as *const AtomicU32) }
    }

    pub fn capacity(&self) -> u32 {
        self.capacity
    }

    pub fn channels(&self) -> u32 {
        self.channels
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    pub fn role(&self) -> Role {
        self.role
    }

    /// Frames written by the producer (our own count on the producer side,
    /// the peer's clamped count on the consumer side).
    pub fn write_pos(&self) -> u64 {
        match self.role {
            Role::Producer => self.local.get(),
            Role::Consumer => {
                let r = self.local.get();
                let w = self.u64_at(ring::OFF_WRITE_POS).load(Ordering::Acquire);
                r + w.wrapping_sub(r).min(self.capacity as u64)
            }
        }
    }

    /// Frames consumed (our own count on the consumer side, the peer's
    /// clamped count on the producer side).
    pub fn read_pos(&self) -> u64 {
        match self.role {
            Role::Consumer => self.local.get(),
            Role::Producer => {
                let w = self.local.get();
                let r = self.u64_at(ring::OFF_READ_POS).load(Ordering::Acquire);
                // A bogus consumer position makes the ring look full.
                w - w.wrapping_sub(r).min(self.capacity as u64).min(w)
            }
        }
    }

    /// Frames queued in the ring.
    pub fn filled(&self) -> u32 {
        (self.write_pos() - self.read_pos()) as u32
    }

    /// Frames that can be written.
    pub fn free(&self) -> u32 {
        self.capacity - self.filled()
    }

    fn data(&self) -> *mut i16 {
        // SAFETY: the data area starts at DATA_OFFSET inside the mapping.
        unsafe { self.base.add(ring::DATA_OFFSET) as *mut i16 }
    }

    /// Producer: appends interleaved frames; returns how many fit.
    pub fn write(&self, samples: &[i16]) -> usize {
        debug_assert_eq!(self.role, Role::Producer);
        let ch = self.channels as usize;
        let frames = (samples.len() / ch).min(self.free() as usize);
        let w = self.local.get();
        let start = (w & (self.capacity as u64 - 1)) as usize;
        let first = frames.min(self.capacity as usize - start);
        // SAFETY: both copies stay inside the data area; the consumer does
        // not read frames beyond the published write position.
        unsafe {
            core::ptr::copy_nonoverlapping(samples.as_ptr(), self.data().add(start * ch), first * ch);
            core::ptr::copy_nonoverlapping(samples.as_ptr().add(first * ch), self.data(), (frames - first) * ch);
        }
        self.local.set(w + frames as u64);
        self.u64_at(ring::OFF_WRITE_POS).store(w + frames as u64, Ordering::Release);
        frames
    }

    /// Consumer: copies up to `out.len() / channels` queued frames out
    /// without consuming them.
    pub fn peek(&self, out: &mut [i16]) -> usize {
        let ch = self.channels as usize;
        let frames = (out.len() / ch).min(self.filled() as usize);
        let r = self.read_pos();
        let start = (r & (self.capacity as u64 - 1)) as usize;
        let first = frames.min(self.capacity as usize - start);
        // SAFETY: inside the data area; these frames were published.
        unsafe {
            core::ptr::copy_nonoverlapping(self.data().add(start * ch), out.as_mut_ptr(), first * ch);
            core::ptr::copy_nonoverlapping(self.data(), out.as_mut_ptr().add(first * ch), (frames - first) * ch);
        }
        frames
    }

    /// Consumer: marks `frames` frames as consumed.
    pub fn consume(&self, frames: usize) {
        debug_assert_eq!(self.role, Role::Consumer);
        let n = frames.min(self.filled() as usize) as u64;
        let r = self.local.get() + n;
        self.local.set(r);
        self.u64_at(ring::OFF_READ_POS).store(r, Ordering::Release);
    }

    /// Consumer: reads and consumes up to `out.len() / channels` frames.
    pub fn read(&self, out: &mut [i16]) -> usize {
        let n = self.peek(out);
        self.consume(n);
        n
    }

    /// Consumer: discards everything queued; returns the new read position.
    pub fn discard_all(&self) -> u64 {
        let n = self.filled() as usize;
        self.consume(n);
        self.local.get()
    }

    /// Publishes the pair (`a` at `a_off`, `b` at `b_off`) under the
    /// sequence count at `seq_off`, whose value this side keeps in `seq`:
    /// odd while the pair is written, even again once it is whole.
    fn publish_pair(&self, seq: &core::cell::Cell<u32>, seq_off: usize, pair: [(usize, u64); 2]) {
        let count = self.u32_at(seq_off);
        let s = seq.get().wrapping_add(1);
        count.store(s, Ordering::Relaxed);
        core::sync::atomic::fence(Ordering::Release);
        for (off, v) in pair {
            self.u64_at(off).store(v, Ordering::Relaxed);
        }
        let s = s.wrapping_add(1);
        count.store(s, Ordering::Release);
        seq.set(s);
    }

    /// Reads a pair the peer published with [`Ring::publish_pair`]: again
    /// while it changes under the reader, a few times at most, and else
    /// the last pair read whole (`seen`).
    fn read_pair(&self, seen: &core::cell::Cell<(u64, u64)>, seq_off: usize, a_off: usize, b_off: usize) -> (u64, u64) {
        let count = self.u32_at(seq_off);
        for _ in 0..SEQ_ATTEMPTS {
            let s1 = count.load(Ordering::Acquire);
            let pair = (self.u64_at(a_off).load(Ordering::Relaxed), self.u64_at(b_off).load(Ordering::Relaxed));
            core::sync::atomic::fence(Ordering::Acquire);
            if s1 & 1 == 0 && count.load(Ordering::Relaxed) == s1 {
                seen.set(pair);
                return pair;
            }
            core::hint::spin_loop();
        }
        seen.get()
    }

    /// Consumer: publishes how much has been played, and when it got
    /// there. That moment, not the time of publishing: whoever extrapolates
    /// the position must see time pass after the last frame, or the device
    /// running dry looks like the last frames playing on and on.
    pub fn set_played(&self, played: u64, at_ns: u64) {
        self.publish_pair(
            &self.played_seq,
            ring::OFF_PLAYED_SEQ,
            [(ring::OFF_PLAYED_POS, played), (ring::OFF_PLAYED_NS, at_ns)],
        );
    }

    /// `(played frames, timestamp ns)` as published by the consumer, the
    /// two of one moment.
    pub fn played(&self) -> (u64, u64) {
        self.read_pair(&self.played_seen, ring::OFF_PLAYED_SEQ, ring::OFF_PLAYED_POS, ring::OFF_PLAYED_NS)
    }

    pub fn consumer_flags(&self) -> u32 {
        self.u32_at(ring::OFF_CONSUMER_FLAGS).load(Ordering::Acquire)
    }

    pub fn set_consumer_flags(&self, flags: u32) {
        self.u32_at(ring::OFF_CONSUMER_FLAGS).store(flags, Ordering::Release);
    }

    pub fn producer_flags(&self) -> u32 {
        self.u32_at(ring::OFF_PRODUCER_FLAGS).load(Ordering::Acquire)
    }

    pub fn set_producer_flags(&self, flags: u32) {
        self.u32_at(ring::OFF_PRODUCER_FLAGS).store(flags, Ordering::Release);
    }

    pub fn underruns(&self) -> u32 {
        self.u32_at(ring::OFF_UNDERRUNS).load(Ordering::Acquire)
    }

    pub fn set_underruns(&self, n: u32) {
        self.u32_at(ring::OFF_UNDERRUNS).store(n, Ordering::Release);
    }

    /// Consumer-side latency in frames (published by the consumer).
    pub fn latency(&self) -> u32 {
        self.u32_at(ring::OFF_LATENCY).load(Ordering::Acquire)
    }

    pub fn set_latency(&self, frames: u32) {
        self.u32_at(ring::OFF_LATENCY).store(frames, Ordering::Release);
    }

    /// Producer of captured audio: frames dropped because the ring was full.
    pub fn overruns(&self) -> u32 {
        self.u32_at(ring::OFF_OVERRUNS).load(Ordering::Acquire)
    }

    pub fn set_overruns(&self, n: u32) {
        self.u32_at(ring::OFF_OVERRUNS).store(n, Ordering::Release);
    }

    /// Producer of captured audio: publishes that frame `pos` (counted like
    /// the write position) was recorded at monotonic time `ns`.
    pub fn set_capture_clock(&self, pos: u64, ns: u64) {
        self.publish_pair(
            &self.clock_seq,
            ring::OFF_CAPTURE_SEQ,
            [(ring::OFF_CAPTURE_POS, pos), (ring::OFF_CAPTURE_NS, ns)],
        );
    }

    /// `(frame position, monotonic ns)` as published by the producer, the
    /// two of one moment (`(0, 0)` if it never did).
    pub fn capture_clock(&self) -> (u64, u64) {
        self.read_pair(&self.clock_seen, ring::OFF_CAPTURE_SEQ, ring::OFF_CAPTURE_POS, ring::OFF_CAPTURE_NS)
    }
}

/// When frame `pos` of a capture ring at `rate` Hz was recorded, by its
/// capture clock `clock` (frame `clock.0` recorded at `clock.1` ns, as
/// [`Ring::capture_clock`] gives it); `None` if the producer never
/// published one. A clock far ahead of `pos` (a producer's mistake, or
/// worse) cannot overflow the arithmetic: it only makes the moment earlier,
/// down to 0.
pub fn recorded_at(clock: (u64, u64), pos: u64, rate: u32) -> Option<u64> {
    let (clock_pos, clock_ns) = clock;
    if clock_ns == 0 {
        return None;
    }
    let after = clock_pos.saturating_sub(pos);
    let back = after as u128 * 1_000_000_000 / rate.max(1) as u128;
    Some(clock_ns.saturating_sub(back.min(u64::MAX as u128) as u64))
}

/// The client side of a playback stream: a mapped ring plus its event.
///
/// ```ignore
/// let audio = proto::audio::audio::Client::new(proto::connect(proto::audio::audio::NAME)?);
/// let stream = OutputStream::open(&audio, StreamSpec::stereo(44_100, 300, "Game"))?;
/// loop {
///     stream.wait_writable(1024, abi::DEADLINE_INFINITE);
///     let n = stream.write(&pcm[pos..]);
///     pos += n * 2;
/// }
/// ```
pub struct OutputStream {
    pub id: u32,
    ring: Ring,
    event: Event,
    /// Rate of the output device.
    pub device_rate: u32,
}

/// Why a stream could not be opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenError {
    Ipc(ipc::IpcError),
    Audio(AudioError),
    Ring(RingError),
}

impl OutputStream {
    /// Opens a stream on `client`'s connection.
    pub fn open(client: &audio::Client, spec: StreamSpec) -> Result<OutputStream, OpenError> {
        let h = client.open_output(spec).map_err(OpenError::Ipc)?.map_err(OpenError::Audio)?;
        let ring = Ring::map(h.ring, Role::Producer).map_err(OpenError::Ring)?;
        Ok(OutputStream { id: h.id, ring, event: h.event, device_rate: h.device_rate })
    }

    /// The ring (producer side).
    pub fn ring(&self) -> &Ring {
        &self.ring
    }

    /// Sample rate of the stream.
    pub fn rate(&self) -> u32 {
        self.ring.rate()
    }

    pub fn channels(&self) -> u32 {
        self.ring.channels()
    }

    /// The stream event (for wait sets).
    pub fn event(&self) -> &Event {
        &self.event
    }

    /// Appends interleaved frames; returns how many fit.
    pub fn write(&self, samples: &[i16]) -> usize {
        self.ring.write(samples)
    }

    /// Frames that can be written now.
    pub fn free(&self) -> u32 {
        self.ring.free()
    }

    /// Frames written but not yet taken by the mixer.
    pub fn queued(&self) -> u32 {
        self.ring.filled()
    }

    /// Frames written since the stream was opened.
    pub fn written(&self) -> u64 {
        self.ring.write_pos()
    }

    /// Waits until at least `frames` frames are free (or the deadline
    /// passes); returns the free space.
    pub fn wait_writable(&self, frames: u32, deadline: u64) -> u32 {
        loop {
            let _ = self.event.clear();
            let free = self.free();
            if free >= frames.min(self.ring.capacity()) {
                return free;
            }
            if self.event.wait(abi::signals::SIGNALED, deadline).is_err() || rt::time::now_ns() >= deadline {
                return self.free();
            }
        }
    }

    /// Frames played so far, extrapolated to `now_ns` from the service's
    /// last report (never beyond what was consumed).
    pub fn played_at(&self, now_ns: u64) -> u64 {
        let (played, at) = self.ring.played();
        let consumed = self.ring.read_pos();
        let paused = self.ring.consumer_flags() & ring::flags::PAUSED != 0;
        if at == 0 || paused || played >= consumed {
            return played.min(consumed);
        }
        let extra = (now_ns.saturating_sub(at) as u128 * self.rate() as u128 / 1_000_000_000) as u64;
        // Never extrapolate more than 100 ms past a report.
        (played + extra.min(self.rate() as u64 / 10)).min(consumed)
    }

    /// Frames played right now (see [`OutputStream::played_at`]).
    pub fn played_now(&self) -> u64 {
        self.played_at(rt::time::now_ns())
    }
}

/// The client side of a capture stream: a mapped ring plus its event.
///
/// ```ignore
/// let audio = proto::audio::audio::Client::new(proto::connect(proto::audio::audio::NAME)?);
/// let mic = InputStream::open(&audio, InputSpec::mono(16_000, 500, "Agent"))?;
/// let mut frame = [0i16; 320];
/// loop {
///     mic.wait_readable(320, abi::DEADLINE_INFINITE);
///     let n = mic.read(&mut frame);
///     // ... use frame[..n]
/// }
/// ```
pub struct InputStream {
    pub id: u32,
    ring: Ring,
    event: Event,
    /// Rate of the input device (0 while there is none).
    pub device_rate: u32,
}

impl InputStream {
    /// Opens a capture stream on `client`'s connection.
    pub fn open(client: &audio::Client, spec: InputSpec) -> Result<InputStream, OpenError> {
        let h = client.open_input(spec).map_err(OpenError::Ipc)?.map_err(OpenError::Audio)?;
        let ring = Ring::map(h.ring, Role::Consumer).map_err(OpenError::Ring)?;
        Ok(InputStream { id: h.id, ring, event: h.event, device_rate: h.device_rate })
    }

    pub fn rate(&self) -> u32 {
        self.ring.rate()
    }

    pub fn channels(&self) -> u32 {
        self.ring.channels()
    }

    /// The stream event (for wait sets): signalled when frames are waiting.
    pub fn event(&self) -> &Event {
        &self.event
    }

    /// Frames waiting to be read.
    pub fn available(&self) -> u32 {
        self.ring.filled()
    }

    /// Reads up to `out.len() / channels` frames; returns how many.
    pub fn read(&self, out: &mut [i16]) -> usize {
        let _ = self.event.clear();
        self.ring.read(out)
    }

    /// Discards everything waiting (e.g. audio recorded while the client
    /// was not interested).
    pub fn discard(&self) {
        let _ = self.event.clear();
        self.ring.discard_all();
    }

    /// Frames lost because the client did not read in time.
    pub fn overruns(&self) -> u32 {
        self.ring.overruns()
    }

    /// When the newest frame was recorded: `(frame position, monotonic ns)`.
    pub fn capture_clock(&self) -> (u64, u64) {
        self.ring.capture_clock()
    }

    /// Waits until at least `frames` frames are waiting (or the deadline
    /// passes); returns how many are.
    pub fn wait_readable(&self, frames: u32, deadline: u64) -> u32 {
        loop {
            let _ = self.event.clear();
            let n = self.available();
            if n >= frames.min(self.ring.capacity()) {
                return n;
            }
            if self.event.wait(abi::signals::SIGNALED, deadline).is_err() || rt::time::now_ns() >= deadline {
                return self.available();
            }
        }
    }
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    struct Mem(Vec<u64>);

    fn pair(capacity: u32, channels: u32) -> (Mem, Ring, Ring) {
        let len = ring_bytes(capacity, channels);
        let mut mem = Mem(vec![0u64; len / 8]);
        let base = mem.0.as_mut_ptr() as *mut u8;
        // SAFETY: `mem` outlives both rings in each test.
        let producer = unsafe { Ring::init_raw(base, len, capacity, channels, 48_000, Role::Producer) };
        let consumer = unsafe { Ring::attach_raw(base, len, Role::Consumer) }.unwrap();
        (mem, producer, consumer)
    }

    #[test]
    fn spsc_wraps_and_preserves_order() {
        let (_mem, p, c) = pair(256, 2);
        assert_eq!(p.free(), 256);
        let mut next = 0i16;
        let mut expect = 0i16;
        for round in 0..50usize {
            let n = 37 + round * 3;
            let data: Vec<i16> = (1..=(n * 2) as i16).map(|k| next.wrapping_add(k)).collect();
            let written = p.write(&data);
            assert!(written <= n);
            next = next.wrapping_add((written * 2) as i16);
            let mut out = vec![0i16; 300 * 2];
            let got = c.read(&mut out[..(round % 7 + 1) * 64]);
            for s in &out[..got * 2] {
                expect = expect.wrapping_add(1);
                assert_eq!(*s, expect);
            }
            assert_eq!(p.filled(), c.filled());
        }
    }

    #[test]
    fn consumer_never_trusts_the_producer() {
        let (_mem, p, c) = pair(256, 1);
        p.write(&[1; 100]);
        // A hostile producer publishes a write position far ahead.
        p.u64_at(ring::OFF_WRITE_POS).store(1 << 40, Ordering::Release);
        assert_eq!(c.filled(), 256);
        let mut out = vec![0i16; 1000];
        assert_eq!(c.read(&mut out), 256);
        // Read position stays consistent with what was consumed.
        assert_eq!(c.read_pos(), 256);
        // And a hostile consumer cannot make the producer overwrite unread data.
        let (_mem2, p2, c2) = pair(256, 1);
        p2.write(&[5; 200]);
        c2.u64_at(ring::OFF_READ_POS).store(u64::MAX / 2, Ordering::Release);
        assert!(p2.free() <= 256);
        assert_eq!(p2.filled() + p2.free(), 256);
    }

    #[test]
    fn discard_and_positions() {
        let (_mem, p, c) = pair(1024, 2);
        p.write(&[3; 600]);
        assert_eq!(c.filled(), 300);
        assert_eq!(c.discard_all(), 300);
        assert_eq!(p.free(), 1024);
        c.set_played(250, 77);
        assert_eq!(p.played(), (250, 77));
        assert!(unsafe { Ring::attach_raw(_mem.0.as_ptr() as *mut u8, 10, Role::Consumer) }.is_err());
    }

    #[test]
    fn the_capture_clock_dates_frames() {
        // Frame 16 000 recorded at 5 s: frame 8 000 half a second before.
        assert_eq!(recorded_at((16_000, 5_000_000_000), 8_000, 16_000), Some(4_500_000_000));
        assert_eq!(recorded_at((16_000, 5_000_000_000), 20_000, 16_000), Some(5_000_000_000));
        assert_eq!(recorded_at((0, 0), 8_000, 16_000), None);
        // A clock position far ahead does not overflow.
        assert_eq!(recorded_at((u64::MAX, 5_000_000_000), 0, 8_000), Some(0));
        assert_eq!(recorded_at((1 << 40, 5_000_000_000), 0, 0), Some(0));
    }

    #[test]
    fn pairs_are_read_whole() {
        let (_mem, p, c) = pair(1024, 1);
        c.set_played(100, 1_000);
        p.set_capture_clock(7, 2_000);
        assert_eq!((p.played(), c.capture_clock()), ((100, 1_000), (7, 2_000)));
        // A writer interrupted half way (the count odd, one value new):
        // the reader keeps the last pair it read whole, never the torn one.
        c.u32_at(ring::OFF_PLAYED_SEQ).fetch_add(1, Ordering::Relaxed);
        c.u64_at(ring::OFF_PLAYED_POS).store(200, Ordering::Relaxed);
        assert_eq!(p.played(), (100, 1_000));
        // A writer that finished in the meantime is read whole.
        c.u64_at(ring::OFF_PLAYED_NS).store(2_000, Ordering::Relaxed);
        c.u32_at(ring::OFF_PLAYED_SEQ).fetch_add(1, Ordering::Release);
        assert_eq!(p.played(), (200, 2_000));
        // The writer's own count is not the peer's to set: one that the peer
        // left odd does not make the writer publish odd.
        p.u32_at(ring::OFF_CAPTURE_SEQ).store(5, Ordering::Relaxed);
        p.set_capture_clock(8, 3_000);
        assert_eq!(c.capture_clock(), (8, 3_000));
        // Another side attaching later resumes the counts even.
        // SAFETY: `_mem` (the ring's memory) outlives `again`.
        let again =
            unsafe { Ring::attach_raw(_mem.0.as_ptr() as *mut u8, ring_bytes(1024, 1), Role::Consumer) }.unwrap();
        again.set_played(300, 4_000);
        assert_eq!(p.played(), (300, 4_000));
    }

    #[test]
    fn a_reader_never_sees_a_torn_pair() {
        // A writer thread publishes pairs (n, 2n) as fast as it can while
        // this one reads them.
        let (mem, p, c) = pair(1024, 1);
        let base = mem.0.as_ptr() as usize;
        let len = ring_bytes(1024, 1);
        std::thread::scope(|scope| {
            scope.spawn(move || {
                // SAFETY: `mem` outlives the scope; this thread is the only
                // writer of the consumer's fields.
                let w = unsafe { Ring::attach_raw(base as *mut u8, len, Role::Consumer) }.unwrap();
                for n in 1..200_000u64 {
                    w.set_played(n, 2 * n);
                }
            });
            for _ in 0..200_000 {
                let (pos, ns) = p.played();
                assert_eq!(ns, 2 * pos, "a torn pair");
            }
        });
        let _ = c;
    }
}
