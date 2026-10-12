//! The media protocol: how an application has videos and sounds decoded by
//! `mediadec`, a sandboxed decoder process (FFmpeg and dav1d, see
//! `services/mediadec`).
//!
//! The application starts the decoder itself, handing it nothing but the
//! control channel ([`CONTROL_ROLE`]): no registry, so no file system,
//! network or other service. Everything else arrives over that channel:
//!
//! * the **source**, a channel speaking the VFS's `file` protocol
//!   (`read_at`, `stat`; [`crate::fs`]): the file the user opened, opened
//!   read-only. The decoder reads nothing else.
//! * the **frame pool** ([`FramePool`], layout in [`pool`]): pictures,
//!   converted and scaled to the size the application asks for, as opaque
//!   `0xAARRGGBB` pixels, in slots of a VMO the application creates (so
//!   they outlive a decoder that crashes, and the last picture stays on
//!   the screen while another decoder starts).
//! * the **sound ring** ([`PcmRing`], layout in [`pcm`]): interleaved
//!   stereo `i16` samples at the application's rate, in records that say
//!   when they are to be heard.
//! * events each way: the decoder signals `ready` when it published a
//!   picture or sound (or ended), the application `wake` when it freed a
//!   slot or read sound.
//!
//! # Generations
//!
//! Every seek (and stream selection) carries a new *generation* number,
//! chosen by the application. Pictures and sound records carry the
//! generation they were decoded for, so the application drops what is left
//! of an earlier position without any handshake: replies come at once, and
//! the pictures of the new position follow.
//!
//! # Trust
//!
//! The decoder parses files from anywhere, so the application trusts
//! nothing it publishes: the layout is checked once when mapped, every slot
//! and record is validated before use, positions are clamped, and pixels
//! are only ever copied. A broken decoder can garble its own pictures and
//! sound, never the application's state. The decoder likewise checks what
//! the application sends.
//!
//! Everything here is integers (times in µs, rationals, H.273 colour
//! codes): the decoder's Rust half is built without floating point.

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering};

use vipc::{enumeration, message, protocol};
use vrt::object::{Channel, Event, Vmo};
use vrt::vm::Mapping;

/// The protocol's version, which [`media::Client::hello`] checks.
pub const VERSION: u32 = 1;

/// The role of the control channel among the decoder's startup handles.
pub const CONTROL_ROLE: u32 = vabi::startup::role::USER;

/// Limits on what [`MediaInfo`] lists, which keep it well inside one
/// message.
pub const MAX_VIDEO_STREAMS: usize = 8;
pub const MAX_AUDIO_STREAMS: usize = 16;
pub const MAX_CHAPTERS: usize = 64;
/// Bytes of a codec's or a container's name.
pub const MAX_NAME: usize = 32;
/// Bytes of a language code.
pub const MAX_LANGUAGE: usize = 16;
/// Bytes of a title, an artist or a chapter's name.
pub const MAX_TEXT: usize = 256;
/// Pictures [`media::Client::checksums`] reports at most.
pub const MAX_CHECKSUMS: u32 = 1024;
/// The largest picture a pool holds, each way.
pub const MAX_SIDE: u32 = 8192;
/// The most bytes a pool's pixels take.
pub const MAX_POOL_BYTES: usize = 256 << 20;

enumeration! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum MediaError {
        /// A request that makes no sense (a size of 0, an unknown stream).
        Invalid = 1,
        /// Not now: no source opened yet, or no output set.
        State = 2,
        /// A format or codec the decoder does not have.
        Unsupported = 3,
        /// The file is damaged or not what it claims to be.
        Corrupt = 4,
        /// The source could not be read.
        Io = 5,
        NoMemory = 6,
        /// Larger than the decoder takes (a picture beyond its limits).
        TooLarge = 7,
        /// Anything else that went wrong in the decoder.
        Internal = 8,
        /// The application and the decoder speak different versions.
        Version = 9,
    }
}

impl MediaError {
    /// The error with code `code` (as a pool's header keeps it).
    pub fn from_code(code: u32) -> MediaError {
        match code {
            1 => MediaError::Invalid,
            2 => MediaError::State,
            3 => MediaError::Unsupported,
            4 => MediaError::Corrupt,
            5 => MediaError::Io,
            6 => MediaError::NoMemory,
            7 => MediaError::TooLarge,
            9 => MediaError::Version,
            _ => MediaError::Internal,
        }
    }
}

impl core::fmt::Display for MediaError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            MediaError::Invalid => "invalid request",
            MediaError::State => "not ready for that",
            MediaError::Unsupported => "unsupported format",
            MediaError::Corrupt => "damaged file",
            MediaError::Io => "the file could not be read",
            MediaError::NoMemory => "out of memory",
            MediaError::TooLarge => "too large",
            MediaError::Internal => "decoder error",
            MediaError::Version => "decoder of another version",
        })
    }
}

enumeration! {
    /// How exactly a seek lands.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum SeekMode {
        /// At the key frame at or before the position: fast, for scrubbing.
        Keyframe = 1,
        /// The picture shown at the position, and sound from it on.
        Exact = 2,
    }
}

enumeration! {
    /// How much of the video the decoder may leave out to keep up.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum SkipLevel {
        /// Everything (the default).
        Nothing = 0,
        /// Skip the loop filter (slightly softer pictures).
        LoopFilter = 1,
        /// Also skip pictures no other picture refers to.
        NonReference = 2,
        /// Only key frames.
        KeyframesOnly = 3,
        /// Decode as needed for what follows, but deliver no pictures (the
        /// window is hidden): sound only.
        Hidden = 4,
    }
}

message! {
    /// A fraction (frame rates, sample aspect ratios); 0/0 when unknown.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct Rational {
        pub num: u32,
        pub den: u32,
    }
}

impl Rational {
    pub const fn new(num: u32, den: u32) -> Rational {
        Rational { num, den }
    }

    /// Whether it is a usable value (neither part 0).
    pub fn is_known(&self) -> bool {
        self.num != 0 && self.den != 0
    }
}

message! {
    /// A picture's colours as H.273 code points (2: unspecified).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct Colour {
        pub primaries: u8,
        pub transfer: u8,
        pub matrix: u8,
        /// Full range (0..=255) rather than limited (16..=235).
        pub full_range: bool,
    }
}

message! {
    /// What the decoder is.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct DecoderInfo {
        /// [`VERSION`].
        pub version: u32,
        /// What it is built from ("FFmpeg 9.0.2, dav1d 1.5.4").
        pub build: String,
        /// The decoding threads it uses at most.
        pub threads: u32,
    }
}

message! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct OpenOptions {
        /// Threads for decoding (0: the decoder's choice).
        pub threads: u32,
        /// Pictures larger than this are refused (0: the decoder's limit).
        pub max_width: u32,
        pub max_height: u32,
        /// [`open_flags`].
        pub flags: u32,
    }
}

/// Bits of [`OpenOptions::flags`].
pub mod open_flags {
    /// Only for pictures ([`super::media::Client::thumbnail`]): no sound,
    /// no decoding ahead.
    pub const THUMBNAILS: u32 = 1;
}

message! {
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct VideoStream {
        /// The stream's number in the file.
        pub index: u32,
        /// The codec ("h264", "av1").
        pub codec: String,
        /// Coded size.
        pub width: u32,
        pub height: u32,
        /// The shape of its pixels (1/1 for square ones).
        pub sample_aspect: Rational,
        /// Clockwise rotation to show it upright: 0, 90, 180 or 270.
        pub rotation: u32,
        /// Mirrored left to right (after rotating).
        pub mirrored: bool,
        /// Frames a second (0/0 when it varies or is not known).
        pub frame_rate: Rational,
        /// Bits per sample (8, 10, 12).
        pub bit_depth: u8,
        pub colour: Colour,
        /// High dynamic range (PQ or HLG transfer).
        pub hdr: bool,
        /// The decoder can decode it.
        pub decodable: bool,
        /// The file marks it as the one to show.
        pub default: bool,
        pub language: String,
        pub title: String,
    }
}

impl VideoStream {
    /// The size it is shown at: its pixels made square (widened, or made
    /// taller, never narrower) and turned upright.
    pub fn display_size(&self) -> (u32, u32) {
        display_size(self.width, self.height, self.sample_aspect, self.rotation)
    }
}

/// The size a `width` x `height` picture is shown at: its pixels of
/// `sample_aspect` made square (widened, or made taller, never narrower),
/// then turned by `rotation` degrees.
pub fn display_size(width: u32, height: u32, sample_aspect: Rational, rotation: u32) -> (u32, u32) {
    let (mut w, mut h) = (width as u64, height as u64);
    if sample_aspect.is_known() && sample_aspect.num != sample_aspect.den {
        let (n, d) = (sample_aspect.num as u64, sample_aspect.den as u64);
        if n > d { w = (w * n).div_ceil(d) } else { h = (h * d).div_ceil(n) }
    }
    let (w, h) = (w.clamp(1, MAX_SIDE as u64 * 4) as u32, h.clamp(1, MAX_SIDE as u64 * 4) as u32);
    if rotation == 90 || rotation == 270 { (h, w) } else { (w, h) }
}

/// The largest size of the shape `width` x `height` within `box_width` x
/// `box_height` (at least 1 x 1).
pub fn fit(width: u32, height: u32, box_width: u32, box_height: u32) -> (u32, u32) {
    let (w, h) = (width.max(1) as u64, height.max(1) as u64);
    let (bw, bh) = (box_width.max(1) as u64, box_height.max(1) as u64);
    let (fw, fh) = if w * bh <= h * bw { ((w * bh + h / 2) / h, bh) } else { (bw, (h * bw + w / 2) / w) };
    (fw.clamp(1, bw) as u32, fh.clamp(1, bh) as u32)
}

message! {
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct AudioStream {
        pub index: u32,
        /// The codec ("aac", "opus").
        pub codec: String,
        pub rate: u32,
        pub channels: u32,
        pub decodable: bool,
        pub default: bool,
        pub language: String,
        pub title: String,
    }
}

message! {
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Chapter {
        pub start_us: i64,
        pub title: String,
    }
}

message! {
    /// What a source holds. Times are µs from its start.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct MediaInfo {
        /// The container ("matroska", "mp4").
        pub container: String,
        pub duration_us: Option<i64>,
        /// Positions can be sought.
        pub seekable: bool,
        /// Bits a second, overall (0: not known).
        pub bit_rate: u64,
        pub title: String,
        pub artist: String,
        pub video: Vec<VideoStream>,
        pub audio: Vec<AudioStream>,
        /// Subtitle streams (not shown yet).
        pub subtitles: u32,
        pub chapters: Vec<Chapter>,
        /// The streams decoded (their `index`), until [`media::Client::select`].
        pub selected_video: Option<u32>,
        pub selected_audio: Option<u32>,
    }
}

impl MediaInfo {
    /// Whether it keeps to the protocol's limits: what an application
    /// checks before it believes a decoder.
    pub fn is_valid(&self) -> bool {
        let text = |s: &String, max: usize| s.len() <= max;
        text(&self.container, MAX_NAME)
            && text(&self.title, MAX_TEXT)
            && text(&self.artist, MAX_TEXT)
            && self.video.len() <= MAX_VIDEO_STREAMS
            && self.audio.len() <= MAX_AUDIO_STREAMS
            && self.chapters.len() <= MAX_CHAPTERS
            && self.video.iter().all(|v| {
                text(&v.codec, MAX_NAME)
                    && text(&v.language, MAX_LANGUAGE)
                    && text(&v.title, MAX_TEXT)
                    && v.width <= MAX_SIDE
                    && v.height <= MAX_SIDE
                    && matches!(v.rotation, 0 | 90 | 180 | 270)
            })
            && self
                .audio
                .iter()
                .all(|a| text(&a.codec, MAX_NAME) && text(&a.language, MAX_LANGUAGE) && text(&a.title, MAX_TEXT))
            && self.chapters.iter().all(|c| text(&c.title, MAX_TEXT))
            && self.selected_video.is_none_or(|i| self.video.iter().any(|v| v.index == i))
            && self.selected_audio.is_none_or(|i| self.audio.iter().any(|a| a.index == i))
    }

    /// The selected video stream.
    pub fn video_stream(&self) -> Option<&VideoStream> {
        self.video.iter().find(|v| Some(v.index) == self.selected_video)
    }

    /// The selected audio stream.
    pub fn audio_stream(&self) -> Option<&AudioStream> {
        self.audio.iter().find(|a| Some(a.index) == self.selected_audio)
    }
}

/// `s` cut to at most `max` bytes, at a character's boundary.
pub fn bounded(s: &str, max: usize) -> String {
    let mut end = s.len().min(max);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    String::from(&s[..end])
}

message! {
    /// The pictures wanted: their size (the pool's), and how.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct VideoSpec {
        pub width: u32,
        pub height: u32,
        /// [`video_flags`].
        pub flags: u32,
    }
}

/// Bits of [`VideoSpec::flags`].
pub mod video_flags {
    /// Scale exactly (for tests: the same pictures on every machine).
    pub const BITEXACT: u32 = 1;
}

message! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct AudioSpec {
        /// Samples a second (8 000 ..= 192 000), the output's.
        pub rate: u32,
    }
}

message! {
    /// A picture made on request (`0xAARRGGBB`, opaque).
    #[derive(Debug)]
    pub struct Thumbnail {
        pub pixels: Vmo,
        pub width: u32,
        pub height: u32,
        /// Pixels from one row to the next.
        pub stride: u32,
        /// The time of the picture it shows.
        pub pts_us: i64,
    }
}

message! {
    /// A decoded picture's fingerprint: the MD5 of its visible planes as
    /// decoded (before conversion), for conformance tests.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct PictureChecksum {
        pub pts_us: i64,
        pub md5: [u8; 16],
    }
}

message! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct Stats {
        /// Pictures decoded, and those left out to keep up.
        pub decoded: u64,
        pub skipped: u64,
        /// Pictures decoded too late to be converted.
        pub late: u64,
        /// Mean time to decode and to convert a picture (µs).
        pub decode_us: u32,
        pub convert_us: u32,
        /// Bytes of compressed data waiting to be decoded.
        pub queued_video: u32,
        pub queued_audio: u32,
        /// Errors decoding, which it went past.
        pub errors: u32,
    }
}

protocol! {
    /// A decoder, over its private control channel (never registered).
    pub mod media = "media" {
        /// What the decoder is; `version` is the application's.
        1 => fn hello(version: u32) -> DecoderInfo;
        /// Opens `source` (a `file` protocol channel) and says what it
        /// holds; the streams are those to show. Replaces an open source.
        2 => fn open(source: Channel, options: OpenOptions) -> Result<MediaInfo, MediaError>;
        /// Decodes other streams (`None`: none of that kind) from the
        /// current position, as generation `generation`.
        3 => fn select(video: Option<u32>, audio: Option<u32>, generation: u32) -> Result<(), MediaError>;
        /// Pictures go to `pool` (its `state` and `pixels` VMOs) from now
        /// on; the last one is delivered again at the new size.
        4 => fn set_video(spec: VideoSpec, state: Vmo, pixels: Vmo, ready: Event, wake: Event) -> Result<(), MediaError>;
        /// Sound goes to `ring` from now on.
        5 => fn set_audio(spec: AudioSpec, ring: Vmo, ready: Event, wake: Event) -> Result<(), MediaError>;
        /// Decodes ahead while there is room.
        6 => fn play() -> Result<(), MediaError>;
        /// Stops decoding ahead (what is decoded stays).
        7 => fn stop() -> Result<(), MediaError>;
        /// Goes to `position_us`; what follows is generation `generation`.
        /// Replies at once: the pictures come later.
        8 => fn seek(position_us: i64, mode: SeekMode, generation: u32) -> Result<(), MediaError>;
        /// Decodes `pictures` more pictures, even when stopped.
        9 => fn step(pictures: u32) -> Result<(), MediaError>;
        /// What to leave out to keep up. Leaving [`SkipLevel::Hidden`], the
        /// player seeks: no pictures were decoded meanwhile (and at the end of
        /// the source the pool's end was set).
        10 => fn set_skip(level: SkipLevel) -> Result<(), MediaError>;
        /// The picture shown at `at_us` (decoded from the key frame before
        /// it, a few seconds on at most), at most `max_width` x
        /// `max_height`, in proportion. It moves the decoding
        /// position (and leaves the pool alone): for a decoder opened with
        /// [`open_flags::THUMBNAILS`].
        11 => fn thumbnail(at_us: i64, max_width: u32, max_height: u32) -> Result<Thumbnail, MediaError>;
        /// Decodes the source from its start (at most `max_pictures`,
        /// [`MAX_CHECKSUMS`]), fingerprinting every picture. It moves the
        /// decoding position as [`media::Client::thumbnail`] does.
        12 => fn checksums(max_pictures: u32) -> Result<Vec<PictureChecksum>, MediaError>;
        13 => fn stats() -> Stats;
        /// Closes the source.
        14 => fn close() -> ();
    }
}

// ---- the frame pool --------------------------------------------------------

/// Layout of a frame pool: a page of state (this) and a VMO of pixels, slot
/// `i` at `i * slot_bytes` (`stride` pixels to a row, as a window's buffers
/// are laid out). All fields little-endian and naturally aligned; the
/// decoder's and the player's fields live on separate cache lines.
pub mod pool {
    /// `"VFRM"`.
    pub const MAGIC: u32 = u32::from_le_bytes(*b"VFRM");
    pub const VERSION: u32 = 1;
    /// Written once by the player, which creates the pool.
    pub const OFF_MAGIC: usize = 0;
    pub const OFF_VERSION: usize = 4;
    pub const OFF_SLOTS: usize = 8;
    pub const OFF_WIDTH: usize = 12;
    pub const OFF_HEIGHT: usize = 16;
    /// Pixels from one row to the next (a multiple of 16).
    pub const OFF_STRIDE: usize = 20;
    /// Bytes from one slot to the next (u64).
    pub const OFF_SLOT_BYTES: usize = 24;
    /// Decoder: the generation it decodes (u32).
    pub const OFF_GENERATION: usize = 64;
    /// Decoder: [`flags`] (u32).
    pub const OFF_FLAGS: usize = 68;
    /// Decoder: the generation `END` is for (u32).
    pub const OFF_END_GENERATION: usize = 72;
    /// Decoder: the error `FAILED` is for (u32, a [`super::MediaError`]).
    pub const OFF_ERROR: usize = 76;
    /// Decoder: pictures published (u64).
    pub const OFF_PRODUCED: usize = 80;
    /// Decoder: bumped every time round its loop, and at least once a
    /// second (u64).
    pub const OFF_HEARTBEAT: usize = 88;
    /// Decoder: pictures decoded but dropped unconverted, too late to be
    /// seen (u64): the player counts them as dropped.
    pub const OFF_LATE: usize = 96;
    /// Player: the media time it shows (i64, µs) ...
    pub const OFF_CLOCK_US: usize = 128;
    /// ... and when (u64, monotonic ns): the decoder skips converting
    /// pictures already late.
    pub const OFF_CLOCK_NS: usize = 136;
    /// Player: slots it took (u64).
    pub const OFF_CONSUMED: usize = 144;
    /// The slots' entries, [`SLOT_SIZE`] bytes each.
    pub const OFF_SLOTS_TABLE: usize = 256;
    pub const SLOT_SIZE: usize = 64;
    /// A slot's entry: its state (u32, [`state`]), generation (u32),
    /// order of publication (u64), time and duration (i64, µs), size
    /// (u32 each) and [`slot_flags`] (u32).
    pub const SLOT_STATE: usize = 0;
    pub const SLOT_GENERATION: usize = 4;
    pub const SLOT_SEQ: usize = 8;
    pub const SLOT_PTS: usize = 16;
    pub const SLOT_DURATION: usize = 24;
    pub const SLOT_WIDTH: usize = 32;
    pub const SLOT_HEIGHT: usize = 36;
    pub const SLOT_FLAGS: usize = 40;
    /// Bytes of the state page.
    pub const STATE_BYTES: usize = 4096;
    pub const MIN_SLOTS: u32 = 2;
    pub const MAX_SLOTS: u32 = 16;

    /// Slot states: the decoder takes a free slot (`WRITING`) and publishes
    /// it (`READY`); the player takes it to show (`HELD`) and frees it.
    pub mod state {
        pub const FREE: u32 = 0;
        pub const WRITING: u32 = 1;
        pub const READY: u32 = 2;
        pub const HELD: u32 = 3;
    }

    /// Bits of a slot's flags.
    pub mod slot_flags {
        /// A key frame.
        pub const KEY: u32 = 1;
        /// The first picture after a seek or a gap.
        pub const DISCONTINUITY: u32 = 2;
        /// The last picture again (at a new size, or while stopped).
        pub const REPEAT: u32 = 4;
        /// Decoded with errors.
        pub const CORRUPT: u32 = 8;
    }

    /// Bits of the decoder's flags.
    pub mod flags {
        /// No more pictures of `OFF_END_GENERATION`.
        pub const END: u32 = 1;
        /// The decoder failed for good (`OFF_ERROR` says why).
        pub const FAILED: u32 = 2;
    }
}

/// Why a pool or a ring could not be made or mapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SharedError {
    NoMemory,
    /// Its header or size does not hold together.
    BadLayout,
}

/// Which side of a pool or a ring this process is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// The decoder: writes pictures and sound.
    Decoder,
    /// The application: shows and plays them.
    Player,
}

/// A slot's entry as published.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SlotMeta {
    pub generation: u32,
    pub seq: u64,
    pub pts_us: i64,
    pub duration_us: i64,
    pub width: u32,
    pub height: u32,
    pub flags: u32,
}

/// One side of a frame pool.
pub struct FramePool {
    state: *mut u8,
    pixels: *mut u8,
    slots: u32,
    width: u32,
    height: u32,
    stride: u32,
    slot_bytes: usize,
    side: Side,
    /// The decoder's count of published pictures.
    seq: core::cell::Cell<u64>,
    _maps: Option<(Mapping, Mapping)>,
}

// SAFETY: each side uses its pool from one thread at a time; the shared
// fields are accessed atomically, pixels only through raw pointers.
unsafe impl Send for FramePool {}

impl core::fmt::Debug for FramePool {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "FramePool({:?}, {} x {}x{})", self.side, self.slots, self.width, self.height)
    }
}

impl FramePool {
    /// The stride and bytes of each slot of `width` x `height` pictures.
    pub fn geometry(width: u32, height: u32) -> (u32, usize) {
        let stride = width.next_multiple_of(16);
        (stride, stride as usize * height as usize * 4)
    }

    /// Bytes of the pixels of `slots` slots of `width` x `height`.
    pub fn pixel_bytes(slots: u32, width: u32, height: u32) -> usize {
        (Self::geometry(width, height).1 * slots as usize).next_multiple_of(4096)
    }

    /// Player: creates a pool of `slots` slots of `width` x `height`
    /// pictures. Returns it and the decoder's handles to its state and its
    /// pixels.
    pub fn create(slots: u32, width: u32, height: u32) -> Result<(FramePool, Vmo, Vmo), SharedError> {
        if !(pool::MIN_SLOTS..=pool::MAX_SLOTS).contains(&slots)
            || !(1..=MAX_SIDE).contains(&width)
            || !(1..=MAX_SIDE).contains(&height)
            || Self::pixel_bytes(slots, width, height) > MAX_POOL_BYTES
        {
            return Err(SharedError::BadLayout);
        }
        let len = Self::pixel_bytes(slots, width, height);
        let state = Vmo::create(pool::STATE_BYTES).map_err(|_| SharedError::NoMemory)?;
        let pixels = Vmo::create(len).map_err(|_| SharedError::NoMemory)?;
        let dup = |v: &Vmo| v.0.duplicate(None).map(Vmo::from_handle).map_err(|_| SharedError::NoMemory);
        let (their_state, their_pixels) = (dup(&state)?, dup(&pixels)?);
        let rw = vabi::map_flags::READ | vabi::map_flags::WRITE;
        let state = Mapping::new(state, pool::STATE_BYTES, rw).map_err(|_| SharedError::NoMemory)?;
        let pixels = Mapping::new(pixels, len, rw).map_err(|_| SharedError::NoMemory)?;
        // SAFETY: fresh mappings of the right sizes, kept with the pool.
        let pool = unsafe { FramePool::init_raw(state.as_ptr(), pixels.as_ptr(), slots, width, height) };
        Ok((FramePool { _maps: Some((state, pixels)), ..pool }, their_state, their_pixels))
    }

    /// Decoder: maps a pool the player created, checking its layout.
    pub fn map(state: Vmo, pixels: Vmo) -> Result<FramePool, SharedError> {
        let state_len = state.size().map_err(|_| SharedError::BadLayout)?;
        let pixels_len = pixels.size().map_err(|_| SharedError::BadLayout)?;
        if state_len < pool::STATE_BYTES || pixels_len > MAX_POOL_BYTES.next_multiple_of(4096) {
            return Err(SharedError::BadLayout);
        }
        let rw = vabi::map_flags::READ | vabi::map_flags::WRITE;
        let state = Mapping::new(state, pool::STATE_BYTES, rw).map_err(|_| SharedError::NoMemory)?;
        let pixels = Mapping::new(pixels, pixels_len, rw).map_err(|_| SharedError::NoMemory)?;
        // SAFETY: the mappings are as long as given, kept with the pool.
        let pool = unsafe {
            FramePool::attach_raw(state.as_ptr(), pool::STATE_BYTES, pixels.as_ptr(), pixels_len, Side::Decoder)?
        };
        Ok(FramePool { _maps: Some((state, pixels)), ..pool })
    }

    /// Player: lays a pool out in raw memory.
    ///
    /// # Safety
    /// `state` is valid for [`pool::STATE_BYTES`] bytes and `pixels` for
    /// [`FramePool::pixel_bytes`], both 8-byte aligned and outliving the
    /// pool; the arguments are within the limits [`FramePool::create`]
    /// checks.
    pub unsafe fn init_raw(state: *mut u8, pixels: *mut u8, slots: u32, width: u32, height: u32) -> FramePool {
        let (stride, slot_bytes) = Self::geometry(width, height);
        // SAFETY: the caller guarantees the memory; the header is ours.
        unsafe {
            core::ptr::write_bytes(state, 0, pool::STATE_BYTES);
            let w = |off: usize, v: u32| (state.add(off) as *mut u32).write_volatile(v);
            w(pool::OFF_VERSION, pool::VERSION);
            w(pool::OFF_SLOTS, slots);
            w(pool::OFF_WIDTH, width);
            w(pool::OFF_HEIGHT, height);
            w(pool::OFF_STRIDE, stride);
            (state.add(pool::OFF_SLOT_BYTES) as *mut u64).write_volatile(slot_bytes as u64);
            core::sync::atomic::fence(Ordering::Release);
            w(pool::OFF_MAGIC, pool::MAGIC);
        }
        FramePool {
            state,
            pixels,
            slots,
            width,
            height,
            stride,
            slot_bytes,
            side: Side::Player,
            seq: core::cell::Cell::new(0),
            _maps: None,
        }
    }

    /// Attaches to a pool laid out by the player, checking its layout.
    ///
    /// # Safety
    /// `state` is valid for `state_len` bytes and `pixels` for
    /// `pixels_len`, both 8-byte aligned and outliving the pool.
    pub unsafe fn attach_raw(
        state: *mut u8,
        state_len: usize,
        pixels: *mut u8,
        pixels_len: usize,
        side: Side,
    ) -> Result<FramePool, SharedError> {
        if state_len < pool::STATE_BYTES {
            return Err(SharedError::BadLayout);
        }
        // SAFETY: inside the caller's memory.
        let r = |off: usize| unsafe { (state.add(off) as *const u32).read_volatile() };
        if r(pool::OFF_MAGIC) != pool::MAGIC || r(pool::OFF_VERSION) != pool::VERSION {
            return Err(SharedError::BadLayout);
        }
        core::sync::atomic::fence(Ordering::Acquire);
        let (slots, width, height, stride) =
            (r(pool::OFF_SLOTS), r(pool::OFF_WIDTH), r(pool::OFF_HEIGHT), r(pool::OFF_STRIDE));
        // SAFETY: as above.
        let slot_bytes = unsafe { (state.add(pool::OFF_SLOT_BYTES) as *const u64).read_volatile() };
        let (want_stride, want_bytes) = Self::geometry(width.min(MAX_SIDE), height.min(MAX_SIDE));
        if !(pool::MIN_SLOTS..=pool::MAX_SLOTS).contains(&slots)
            || !(1..=MAX_SIDE).contains(&width)
            || !(1..=MAX_SIDE).contains(&height)
            || stride != want_stride
            || slot_bytes != want_bytes as u64
            || pixels_len < want_bytes * slots as usize
        {
            return Err(SharedError::BadLayout);
        }
        let seq = core::cell::Cell::new(0);
        let pool =
            FramePool { state, pixels, slots, width, height, stride, slot_bytes: want_bytes, side, seq, _maps: None };
        pool.seq.set(pool.u64_at(pool::OFF_PRODUCED).load(Ordering::Acquire));
        Ok(pool)
    }

    fn u32_at(&self, off: usize) -> &AtomicU32 {
        // SAFETY: offsets are inside the state page and 4-byte aligned.
        unsafe { &*(self.state.add(off) as *const AtomicU32) }
    }

    fn u64_at(&self, off: usize) -> &AtomicU64 {
        // SAFETY: offsets are inside the state page and 8-byte aligned.
        unsafe { &*(self.state.add(off) as *const AtomicU64) }
    }

    fn i64_at(&self, off: usize) -> &AtomicI64 {
        // SAFETY: as above.
        unsafe { &*(self.state.add(off) as *const AtomicI64) }
    }

    fn entry(&self, slot: u32, field: usize) -> usize {
        pool::OFF_SLOTS_TABLE + slot as usize * pool::SLOT_SIZE + field
    }

    pub fn slots(&self) -> u32 {
        self.slots
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// Pixels from one row of a slot to the next.
    pub fn stride(&self) -> u32 {
        self.stride
    }

    pub fn side(&self) -> Side {
        self.side
    }

    /// The pixels of `slot` (`stride` x `height`). Only ever copied to or
    /// from through the pointer: the other process may write them.
    pub fn slot_pixels(&self, slot: u32) -> *mut u32 {
        let slot = slot.min(self.slots - 1);
        // SAFETY: inside the pixels mapping, which holds `slots` slots.
        unsafe { self.pixels.add(slot as usize * self.slot_bytes) as *mut u32 }
    }

    /// The state of `slot` ([`pool::state`]).
    pub fn state(&self, slot: u32) -> u32 {
        if slot >= self.slots {
            return pool::state::FREE;
        }
        self.u32_at(self.entry(slot, pool::SLOT_STATE)).load(Ordering::Acquire)
    }

    fn change(&self, slot: u32, from: u32, to: u32) -> bool {
        slot < self.slots
            && self
                .u32_at(self.entry(slot, pool::SLOT_STATE))
                .compare_exchange(from, to, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
    }

    // ---- the decoder's side ------------------------------------------------

    /// Decoder: takes a free slot to write a picture into.
    pub fn claim(&self) -> Option<u32> {
        (0..self.slots).find(|&s| self.change(s, pool::state::FREE, pool::state::WRITING))
    }

    /// Decoder: publishes the picture written into `slot`.
    pub fn publish(&self, slot: u32, meta: &SlotMeta) {
        if slot >= self.slots || self.state(slot) != pool::state::WRITING {
            return;
        }
        let seq = self.seq.get() + 1;
        self.seq.set(seq);
        self.u32_at(self.entry(slot, pool::SLOT_GENERATION)).store(meta.generation, Ordering::Relaxed);
        self.u64_at(self.entry(slot, pool::SLOT_SEQ)).store(seq, Ordering::Relaxed);
        self.i64_at(self.entry(slot, pool::SLOT_PTS)).store(meta.pts_us, Ordering::Relaxed);
        self.i64_at(self.entry(slot, pool::SLOT_DURATION)).store(meta.duration_us, Ordering::Relaxed);
        self.u32_at(self.entry(slot, pool::SLOT_WIDTH)).store(meta.width, Ordering::Relaxed);
        self.u32_at(self.entry(slot, pool::SLOT_HEIGHT)).store(meta.height, Ordering::Relaxed);
        self.u32_at(self.entry(slot, pool::SLOT_FLAGS)).store(meta.flags, Ordering::Relaxed);
        self.change(slot, pool::state::WRITING, pool::state::READY);
        self.u64_at(pool::OFF_PRODUCED).store(seq, Ordering::Release);
    }

    /// Decoder: gives back a slot it claimed without publishing it.
    pub fn abandon(&self, slot: u32) {
        self.change(slot, pool::state::WRITING, pool::state::FREE);
    }

    /// Decoder: frees the pictures of other generations it published and
    /// the player has not taken yet (after a seek). Returns how many.
    pub fn withdraw_other_generations(&self, generation: u32) -> u32 {
        let mut n = 0;
        for s in 0..self.slots {
            if self.state(s) == pool::state::READY
                && self.u32_at(self.entry(s, pool::SLOT_GENERATION)).load(Ordering::Acquire) != generation
                && self.change(s, pool::state::READY, pool::state::FREE)
            {
                n += 1;
            }
        }
        n
    }

    /// Decoder: the generation it decodes now (ends and failures are
    /// cleared).
    pub fn set_generation(&self, generation: u32) {
        self.u32_at(pool::OFF_GENERATION).store(generation, Ordering::Release);
        self.u32_at(pool::OFF_FLAGS).fetch_and(!pool::flags::END, Ordering::AcqRel);
    }

    /// Decoder: no more pictures of `generation`.
    pub fn set_end(&self, generation: u32) {
        self.u32_at(pool::OFF_END_GENERATION).store(generation, Ordering::Release);
        self.u32_at(pool::OFF_FLAGS).fetch_or(pool::flags::END, Ordering::AcqRel);
    }

    /// Decoder: failed for good with `error`.
    pub fn set_failed(&self, error: MediaError) {
        self.u32_at(pool::OFF_ERROR).store(error as u32, Ordering::Release);
        self.u32_at(pool::OFF_FLAGS).fetch_or(pool::flags::FAILED, Ordering::AcqRel);
    }

    /// Decoder: alive (once round its loop).
    pub fn beat(&self) {
        self.u64_at(pool::OFF_HEARTBEAT).fetch_add(1, Ordering::Relaxed);
    }

    /// Decoder: a picture was dropped, too late to be worth converting.
    pub fn note_late(&self) {
        self.u64_at(pool::OFF_LATE).fetch_add(1, Ordering::Relaxed);
    }

    /// Decoder: the media time the player shows now, if it says (µs, and
    /// the monotonic ns it was at).
    pub fn player_clock(&self) -> Option<(i64, u64)> {
        let ns = self.u64_at(pool::OFF_CLOCK_NS).load(Ordering::Acquire);
        (ns != 0).then(|| (self.i64_at(pool::OFF_CLOCK_US).load(Ordering::Acquire), ns))
    }

    // ---- the player's side -------------------------------------------------

    /// Player: the entry of `slot` if it holds a published picture that
    /// fits the pool, as published (`None` otherwise).
    pub fn snapshot(&self, slot: u32) -> Option<SlotMeta> {
        let state = self.state(slot);
        if state != pool::state::READY && state != pool::state::HELD {
            return None;
        }
        let meta = SlotMeta {
            generation: self.u32_at(self.entry(slot, pool::SLOT_GENERATION)).load(Ordering::Acquire),
            seq: self.u64_at(self.entry(slot, pool::SLOT_SEQ)).load(Ordering::Acquire),
            pts_us: self.i64_at(self.entry(slot, pool::SLOT_PTS)).load(Ordering::Acquire),
            duration_us: self.i64_at(self.entry(slot, pool::SLOT_DURATION)).load(Ordering::Acquire).max(0),
            width: self.u32_at(self.entry(slot, pool::SLOT_WIDTH)).load(Ordering::Acquire),
            height: self.u32_at(self.entry(slot, pool::SLOT_HEIGHT)).load(Ordering::Acquire),
            flags: self.u32_at(self.entry(slot, pool::SLOT_FLAGS)).load(Ordering::Acquire),
        };
        let fits = (1..=self.width).contains(&meta.width) && (1..=self.height).contains(&meta.height);
        fits.then_some(meta)
    }

    /// Player: takes a published picture to show (it stays until freed).
    pub fn hold(&self, slot: u32) -> bool {
        let held = self.change(slot, pool::state::READY, pool::state::HELD);
        if held {
            self.u64_at(pool::OFF_CONSUMED).fetch_add(1, Ordering::Relaxed);
        }
        held
    }

    /// Player: frees a slot it holds, or a published picture it will not
    /// show. Signal the decoder's `wake` after freeing.
    pub fn release(&self, slot: u32) -> bool {
        self.change(slot, pool::state::HELD, pool::state::FREE)
            || self.change(slot, pool::state::READY, pool::state::FREE)
    }

    /// Player: frees what a decoder that is gone was writing or had
    /// published (all but `keep`, the picture shown).
    pub fn reset(&self, keep: Option<u32>) {
        for s in 0..self.slots {
            if Some(s) != keep {
                self.u32_at(self.entry(s, pool::SLOT_STATE)).store(pool::state::FREE, Ordering::Release);
            }
        }
        self.u32_at(pool::OFF_FLAGS).store(0, Ordering::Release);
    }

    /// The generation `END` was set for, if it is set.
    pub fn end(&self) -> Option<u32> {
        let flags = self.u32_at(pool::OFF_FLAGS).load(Ordering::Acquire);
        (flags & pool::flags::END != 0).then(|| self.u32_at(pool::OFF_END_GENERATION).load(Ordering::Acquire))
    }

    /// The decoder's error, if it failed for good.
    pub fn failed(&self) -> Option<MediaError> {
        let flags = self.u32_at(pool::OFF_FLAGS).load(Ordering::Acquire);
        (flags & pool::flags::FAILED != 0)
            .then(|| MediaError::from_code(self.u32_at(pool::OFF_ERROR).load(Ordering::Acquire)))
    }

    /// The decoder's heartbeat: it moves at least once a second while the
    /// decoder is alive, working or not.
    pub fn heartbeat(&self) -> u64 {
        self.u64_at(pool::OFF_HEARTBEAT).load(Ordering::Acquire)
    }

    /// Pictures published so far.
    pub fn produced(&self) -> u64 {
        self.u64_at(pool::OFF_PRODUCED).load(Ordering::Acquire)
    }

    /// Pictures the decoder dropped, too late (see [`FramePool::note_late`]).
    pub fn late(&self) -> u64 {
        self.u64_at(pool::OFF_LATE).load(Ordering::Acquire)
    }

    /// Player: the media time it shows (µs) at `at_ns`.
    pub fn set_clock(&self, media_us: i64, at_ns: u64) {
        self.i64_at(pool::OFF_CLOCK_US).store(media_us, Ordering::Release);
        self.u64_at(pool::OFF_CLOCK_NS).store(at_ns, Ordering::Release);
    }
}

// ---- the sound ring --------------------------------------------------------

/// Layout of a sound ring: a header, then records of interleaved stereo
/// `i16` samples, each saying when it is to be heard. Records are whole
/// multiples of [`RECORD_ALIGN`] bytes and never wrap: the decoder pads to
/// the end when one would not fit.
pub mod pcm {
    /// `"VPCM"`.
    pub const MAGIC: u32 = u32::from_le_bytes(*b"VPCM");
    pub const VERSION: u32 = 1;
    pub const OFF_MAGIC: usize = 0;
    pub const OFF_VERSION: usize = 4;
    pub const OFF_RATE: usize = 8;
    pub const OFF_CHANNELS: usize = 12;
    /// Bytes of records (a power of two).
    pub const OFF_CAPACITY: usize = 16;
    /// Decoder: bytes written (u64).
    pub const OFF_WRITE_POS: usize = 64;
    /// Player: bytes read (u64).
    pub const OFF_READ_POS: usize = 128;
    pub const DATA_OFFSET: usize = 4096;
    pub const MIN_CAPACITY: u32 = 64 << 10;
    pub const MAX_CAPACITY: u32 = 8 << 20;
    /// Records start at multiples of this, and their headers fill it.
    pub const RECORD_ALIGN: usize = 32;
    /// A record's header: its kind (u32, [`kind`]), length in bytes with
    /// the header (u32), frames (u32), generation (u32), the time of its
    /// first frame (i64, µs) and [`record_flags`] (u32).
    pub const REC_KIND: usize = 0;
    pub const REC_LENGTH: usize = 4;
    pub const REC_FRAMES: usize = 8;
    pub const REC_GENERATION: usize = 12;
    pub const REC_PTS: usize = 16;
    pub const REC_FLAGS: usize = 24;
    /// The most frames in one record.
    pub const MAX_FRAMES: u32 = 4096;

    pub mod kind {
        /// Samples.
        pub const DATA: u32 = 1;
        /// Nothing up to the end of the ring.
        pub const PAD: u32 = 2;
        /// No more sound of the record's generation.
        pub const END: u32 = 3;
    }

    pub mod record_flags {
        /// The first samples after a seek or a gap.
        pub const DISCONTINUITY: u32 = 1;
    }
}

/// A sound record, as the player found it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Record {
    /// [`pcm::kind::DATA`] or [`pcm::kind::END`].
    pub kind: u32,
    pub generation: u32,
    /// The time of its first frame (µs).
    pub pts_us: i64,
    pub flags: u32,
    /// Its frames, and how many of them were taken already.
    pub frames: u32,
    pub taken: u32,
    offset: usize,
    length: usize,
}

impl Record {
    /// The time of the first frame not taken yet, at `rate` (µs).
    pub fn next_pts(&self, rate: u32) -> i64 {
        self.pts_us.saturating_add((self.taken as i64 * 1_000_000) / rate.max(1) as i64)
    }
}

/// One side of a sound ring.
pub struct PcmRing {
    base: *mut u8,
    capacity: usize,
    rate: u32,
    side: Side,
    /// Our own position (written by the decoder, read by the player); the
    /// peer's is only ever clamped.
    local: core::cell::Cell<u64>,
    /// Player: frames taken from the record at the read position.
    taken: core::cell::Cell<u32>,
    _map: Option<Mapping>,
}

// SAFETY: as for `FramePool`.
unsafe impl Send for PcmRing {}

impl core::fmt::Debug for PcmRing {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "PcmRing({:?}, {} bytes @ {} Hz)", self.side, self.capacity, self.rate)
    }
}

/// The bytes a record of `frames` stereo frames takes.
fn record_len(frames: u32) -> usize {
    (pcm::RECORD_ALIGN + frames as usize * 4).next_multiple_of(pcm::RECORD_ALIGN)
}

impl PcmRing {
    /// Bytes of a ring with `capacity` bytes of records.
    pub fn bytes(capacity: u32) -> usize {
        pcm::DATA_OFFSET + capacity as usize
    }

    /// Player: creates a ring of about `capacity` bytes for sound at
    /// `rate`. Returns it and the decoder's handle to it.
    pub fn create(capacity: u32, rate: u32) -> Result<(PcmRing, Vmo), SharedError> {
        let capacity = capacity.clamp(pcm::MIN_CAPACITY, pcm::MAX_CAPACITY).next_power_of_two();
        if !(8_000..=192_000).contains(&rate) {
            return Err(SharedError::BadLayout);
        }
        let len = Self::bytes(capacity);
        let vmo = Vmo::create(len).map_err(|_| SharedError::NoMemory)?;
        let theirs = vmo.0.duplicate(None).map(Vmo::from_handle).map_err(|_| SharedError::NoMemory)?;
        let map = Mapping::new(vmo, len, vabi::map_flags::READ | vabi::map_flags::WRITE)
            .map_err(|_| SharedError::NoMemory)?;
        // SAFETY: a fresh mapping of `len` bytes, kept with the ring.
        let ring = unsafe { PcmRing::init_raw(map.as_ptr(), capacity, rate) };
        Ok((PcmRing { _map: Some(map), ..ring }, theirs))
    }

    /// Decoder: maps a ring the player created, checking its layout.
    pub fn map(vmo: Vmo) -> Result<PcmRing, SharedError> {
        let len = vmo.size().map_err(|_| SharedError::BadLayout)?;
        if len > Self::bytes(pcm::MAX_CAPACITY) {
            return Err(SharedError::BadLayout);
        }
        let map = Mapping::new(vmo, len, vabi::map_flags::READ | vabi::map_flags::WRITE)
            .map_err(|_| SharedError::NoMemory)?;
        // SAFETY: the mapping is `len` bytes, kept with the ring.
        let ring = unsafe { PcmRing::attach_raw(map.as_ptr(), len, Side::Decoder)? };
        Ok(PcmRing { _map: Some(map), ..ring })
    }

    /// Player: lays a ring out in raw memory.
    ///
    /// # Safety
    /// `base` is valid for [`PcmRing::bytes`]`(capacity)` bytes, 8-byte
    /// aligned and outlives the ring; `capacity` is a power of two within
    /// [`pcm::MIN_CAPACITY`] ..= [`pcm::MAX_CAPACITY`].
    pub unsafe fn init_raw(base: *mut u8, capacity: u32, rate: u32) -> PcmRing {
        // SAFETY: the caller guarantees the memory; the header is ours.
        unsafe {
            core::ptr::write_bytes(base, 0, pcm::DATA_OFFSET);
            let w = |off: usize, v: u32| (base.add(off) as *mut u32).write_volatile(v);
            w(pcm::OFF_VERSION, pcm::VERSION);
            w(pcm::OFF_RATE, rate);
            w(pcm::OFF_CHANNELS, 2);
            w(pcm::OFF_CAPACITY, capacity);
            core::sync::atomic::fence(Ordering::Release);
            w(pcm::OFF_MAGIC, pcm::MAGIC);
        }
        PcmRing {
            base,
            capacity: capacity as usize,
            rate,
            side: Side::Player,
            local: core::cell::Cell::new(0),
            taken: core::cell::Cell::new(0),
            _map: None,
        }
    }

    /// Attaches to a ring laid out by the player, checking its layout.
    ///
    /// # Safety
    /// `base` is valid for `len` bytes, 8-byte aligned and outlives the ring.
    pub unsafe fn attach_raw(base: *mut u8, len: usize, side: Side) -> Result<PcmRing, SharedError> {
        if len < pcm::DATA_OFFSET {
            return Err(SharedError::BadLayout);
        }
        // SAFETY: inside the caller's memory.
        let r = |off: usize| unsafe { (base.add(off) as *const u32).read_volatile() };
        if r(pcm::OFF_MAGIC) != pcm::MAGIC || r(pcm::OFF_VERSION) != pcm::VERSION {
            return Err(SharedError::BadLayout);
        }
        core::sync::atomic::fence(Ordering::Acquire);
        let (capacity, rate) = (r(pcm::OFF_CAPACITY), r(pcm::OFF_RATE));
        if !capacity.is_power_of_two()
            || !(pcm::MIN_CAPACITY..=pcm::MAX_CAPACITY).contains(&capacity)
            || r(pcm::OFF_CHANNELS) != 2
            || !(8_000..=192_000).contains(&rate)
            || len < Self::bytes(capacity)
        {
            return Err(SharedError::BadLayout);
        }
        let ring = PcmRing {
            base,
            capacity: capacity as usize,
            rate,
            side,
            local: core::cell::Cell::new(0),
            taken: core::cell::Cell::new(0),
            _map: None,
        };
        let own = match side {
            Side::Decoder => pcm::OFF_WRITE_POS,
            Side::Player => pcm::OFF_READ_POS,
        };
        ring.local.set(ring.u64_at(own).load(Ordering::Acquire));
        Ok(ring)
    }

    fn u64_at(&self, off: usize) -> &AtomicU64 {
        // SAFETY: header offsets are inside the mapping and 8-byte aligned.
        unsafe { &*(self.base.add(off) as *const AtomicU64) }
    }

    fn data(&self, offset: usize) -> *mut u8 {
        // SAFETY: callers keep `offset` inside the records.
        unsafe { self.base.add(pcm::DATA_OFFSET + offset) }
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Bytes written (the decoder's own count; the player clamps the
    /// published one).
    fn write_pos(&self) -> u64 {
        match self.side {
            Side::Decoder => self.local.get(),
            Side::Player => {
                let r = self.local.get();
                let w = self.u64_at(pcm::OFF_WRITE_POS).load(Ordering::Acquire);
                r + w.wrapping_sub(r).min(self.capacity as u64)
            }
        }
    }

    /// Bytes read (the player's own count; the decoder clamps the published
    /// one, so a bogus one makes the ring look full).
    fn read_pos(&self) -> u64 {
        match self.side {
            Side::Player => self.local.get(),
            Side::Decoder => {
                let w = self.local.get();
                let r = self.u64_at(pcm::OFF_READ_POS).load(Ordering::Acquire);
                w - w.wrapping_sub(r).min(self.capacity as u64).min(w)
            }
        }
    }

    /// Bytes holding records not read yet.
    pub fn filled(&self) -> usize {
        (self.write_pos() - self.read_pos()) as usize
    }

    /// Decoder: whether a record of `frames` frames fits now.
    pub fn fits(&self, frames: u32) -> bool {
        let need = record_len(frames.min(pcm::MAX_FRAMES));
        let free = self.capacity - self.filled();
        let tail = self.capacity - (self.write_pos() as usize & (self.capacity - 1));
        if need <= tail { need <= free } else { tail + need <= free }
    }

    fn put_header(
        &self,
        offset: usize,
        kind: u32,
        length: usize,
        frames: u32,
        generation: u32,
        pts_us: i64,
        flags: u32,
    ) {
        let h = self.data(offset);
        // SAFETY: the header lies inside the records (the caller checked).
        unsafe {
            core::ptr::write_bytes(h, 0, pcm::RECORD_ALIGN);
            (h.add(pcm::REC_KIND) as *mut u32).write_volatile(kind);
            (h.add(pcm::REC_LENGTH) as *mut u32).write_volatile(length as u32);
            (h.add(pcm::REC_FRAMES) as *mut u32).write_volatile(frames);
            (h.add(pcm::REC_GENERATION) as *mut u32).write_volatile(generation);
            (h.add(pcm::REC_PTS) as *mut i64).write_volatile(pts_us);
            (h.add(pcm::REC_FLAGS) as *mut u32).write_volatile(flags);
        }
    }

    /// Decoder: appends a record (after padding to the end of the ring if it
    /// would not fit before it), then publishes it.
    fn append(&self, kind: u32, generation: u32, pts_us: i64, flags: u32, samples: &[i16]) -> bool {
        let frames = (samples.len() / 2) as u32;
        if frames > pcm::MAX_FRAMES || !self.fits(frames) {
            return false;
        }
        let need = record_len(frames);
        let mut w = self.local.get();
        let offset = w as usize & (self.capacity - 1);
        let tail = self.capacity - offset;
        if need > tail {
            self.put_header(offset, pcm::kind::PAD, tail, 0, generation, 0, 0);
            w += tail as u64;
        }
        let offset = w as usize & (self.capacity - 1);
        self.put_header(offset, kind, need, frames, generation, pts_us, flags);
        // SAFETY: the record fits before the end (padded above) and in the
        // free space (checked by `fits`).
        unsafe {
            core::ptr::copy_nonoverlapping(
                samples.as_ptr(),
                self.data(offset + pcm::RECORD_ALIGN) as *mut i16,
                frames as usize * 2,
            )
        };
        w += need as u64;
        self.local.set(w);
        self.u64_at(pcm::OFF_WRITE_POS).store(w, Ordering::Release);
        true
    }

    /// Decoder: appends `samples` (interleaved stereo, at most
    /// [`pcm::MAX_FRAMES`] frames), the first heard at `pts_us`. False if
    /// there is no room (try again once the player read some).
    pub fn write(&self, generation: u32, pts_us: i64, flags: u32, samples: &[i16]) -> bool {
        self.append(pcm::kind::DATA, generation, pts_us, flags, samples)
    }

    /// Decoder: says there is no more sound of `generation`.
    pub fn write_end(&self, generation: u32) -> bool {
        self.append(pcm::kind::END, generation, 0, 0, &[])
    }

    /// Player: the next record (padding skipped), or `None` if none is
    /// complete yet. `Err` if what the decoder wrote does not hold together:
    /// stop trusting it.
    pub fn peek(&self) -> Result<Option<Record>, SharedError> {
        loop {
            let filled = self.filled();
            if filled < pcm::RECORD_ALIGN {
                return Ok(None);
            }
            let offset = self.local.get() as usize & (self.capacity - 1);
            let h = self.data(offset);
            // SAFETY: the header is inside the records: offsets are aligned
            // and the capacity is a multiple of the alignment.
            let (kind, length, frames, generation, pts_us, flags) = unsafe {
                (
                    (h.add(pcm::REC_KIND) as *const u32).read_volatile(),
                    (h.add(pcm::REC_LENGTH) as *const u32).read_volatile() as usize,
                    (h.add(pcm::REC_FRAMES) as *const u32).read_volatile(),
                    (h.add(pcm::REC_GENERATION) as *const u32).read_volatile(),
                    (h.add(pcm::REC_PTS) as *const i64).read_volatile(),
                    (h.add(pcm::REC_FLAGS) as *const u32).read_volatile(),
                )
            };
            let sound = kind == pcm::kind::DATA;
            if !matches!(kind, pcm::kind::DATA | pcm::kind::PAD | pcm::kind::END)
                || length < pcm::RECORD_ALIGN
                || length % pcm::RECORD_ALIGN != 0
                || length > filled
                || offset + length > self.capacity
                || (sound && (frames > pcm::MAX_FRAMES || pcm::RECORD_ALIGN + frames as usize * 4 > length))
            {
                return Err(SharedError::BadLayout);
            }
            if kind == pcm::kind::PAD {
                self.skip(length);
                continue;
            }
            let frames = if sound { frames } else { 0 };
            let taken = self.taken.get().min(frames);
            return Ok(Some(Record { kind, generation, pts_us, flags, frames, taken, offset, length }));
        }
    }

    fn skip(&self, length: usize) {
        let r = self.local.get() + length as u64;
        self.local.set(r);
        self.taken.set(0);
        self.u64_at(pcm::OFF_READ_POS).store(r, Ordering::Release);
    }

    /// Player: copies up to `out.len() / 2` of `record`'s frames not taken
    /// yet into `out`; returns how many. Take them with [`PcmRing::take`].
    pub fn copy(&self, record: &Record, out: &mut [i16]) -> usize {
        let n = ((record.frames - record.taken) as usize).min(out.len() / 2);
        let from = record.offset + pcm::RECORD_ALIGN + record.taken as usize * 4;
        // SAFETY: inside the record, which `peek` checked lies in the ring.
        unsafe { core::ptr::copy_nonoverlapping(self.data(from) as *const i16, out.as_mut_ptr(), n * 2) };
        n
    }

    /// Player: takes `frames` frames of `record` (the whole record once all
    /// are taken; an end record at once).
    pub fn take(&self, record: &Record, frames: u32) {
        let taken = (record.taken + frames).min(record.frames);
        if taken >= record.frames {
            self.skip(record.length);
        } else {
            self.taken.set(taken);
        }
    }

    /// Player: drops every record queued.
    pub fn discard_all(&self) {
        let w = self.write_pos();
        self.local.set(w);
        self.taken.set(0);
        self.u64_at(pcm::OFF_READ_POS).store(w, Ordering::Release);
    }
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    fn pool(slots: u32, w: u32, h: u32) -> (Vec<u64>, Vec<u64>, FramePool, FramePool) {
        let mut state = vec![0u64; pool::STATE_BYTES / 8];
        let mut pixels = vec![0u64; FramePool::pixel_bytes(slots, w, h) / 8];
        let (s, p) = (state.as_mut_ptr() as *mut u8, pixels.as_mut_ptr() as *mut u8);
        // SAFETY: the vectors outlive both sides in each test.
        let player = unsafe { FramePool::init_raw(s, p, slots, w, h) };
        let decoder =
            unsafe { FramePool::attach_raw(s, pool::STATE_BYTES, p, pixels.len() * 8, Side::Decoder) }.unwrap();
        (state, pixels, player, decoder)
    }

    fn meta(generation: u32, pts_us: i64, w: u32, h: u32) -> SlotMeta {
        SlotMeta { generation, seq: 0, pts_us, duration_us: 40_000, width: w, height: h, flags: 0 }
    }

    #[test]
    fn pictures_go_round_the_pool() {
        let (_s, _p, player, dec) = pool(3, 100, 50);
        assert_eq!(player.stride(), 112);
        let a = dec.claim().unwrap();
        let b = dec.claim().unwrap();
        let c = dec.claim().unwrap();
        assert_eq!(dec.claim(), None, "every slot is taken");
        // Pixels are where both sides think they are.
        // SAFETY: inside the slot.
        unsafe { dec.slot_pixels(b).add(112 * 49 + 99).write(0xFF12_3456) };
        // SAFETY: as above.
        assert_eq!(unsafe { player.slot_pixels(b).add(112 * 49 + 99).read() }, 0xFF12_3456);
        assert_eq!(player.snapshot(a), None, "not published yet");
        dec.publish(a, &meta(1, 0, 100, 50));
        dec.publish(b, &meta(1, 40_000, 100, 50));
        dec.abandon(c);
        let ma = player.snapshot(a).unwrap();
        let mb = player.snapshot(b).unwrap();
        assert!(ma.seq < mb.seq && mb.pts_us == 40_000 && dec.produced() == 2);
        assert!(player.hold(a) && !player.hold(a));
        assert_eq!(player.snapshot(a), Some(ma), "a held picture is still readable");
        assert!(player.release(a) && player.release(b));
        assert_eq!(dec.claim(), Some(a));
    }

    #[test]
    fn seeks_and_ends_by_generation() {
        let (_s, _p, player, dec) = pool(4, 64, 64);
        for pts in 0..3 {
            let s = dec.claim().unwrap();
            dec.publish(s, &meta(1, pts * 40_000, 64, 64));
        }
        dec.set_generation(2);
        assert_eq!(dec.withdraw_other_generations(2), 3);
        let s = dec.claim().unwrap();
        dec.publish(s, &meta(2, 5_000_000, 64, 64));
        dec.set_end(2);
        assert_eq!(player.end(), Some(2));
        dec.set_generation(3);
        assert_eq!(player.end(), None, "a new generation has not ended");
        dec.set_failed(MediaError::Corrupt);
        assert_eq!(player.failed(), Some(MediaError::Corrupt));
        player.set_clock(1234, 99);
        assert_eq!(dec.player_clock(), Some((1234, 99)));
        dec.beat();
        assert_eq!(player.heartbeat(), 1);
    }

    #[test]
    fn a_hostile_decoder_cannot_mislead_the_player() {
        let (_s, _p, player, dec) = pool(2, 64, 32);
        let s = dec.claim().unwrap();
        // Larger than the pool: refused.
        dec.publish(s, &meta(1, 0, 65, 32));
        assert_eq!(player.snapshot(s), None);
        let s2 = dec.claim().unwrap();
        dec.publish(s2, &SlotMeta { duration_us: -5, ..meta(1, 0, 64, 32) });
        assert_eq!(player.snapshot(s2).unwrap().duration_us, 0);
        // States out of range read as free; slots out of range do nothing.
        assert!(!player.hold(7) && !player.release(7) && player.snapshot(7).is_none());
        player.reset(None);
        assert_eq!(player.state(0), pool::state::FREE);
        assert_eq!(player.state(1), pool::state::FREE);
    }

    #[test]
    fn bad_pool_layouts_are_refused() {
        let mut state = vec![0u64; pool::STATE_BYTES / 8];
        let mut pixels = vec![0u64; FramePool::pixel_bytes(2, 64, 64) / 8];
        let (s, p) = (state.as_mut_ptr() as *mut u8, pixels.as_mut_ptr() as *mut u8);
        let attach = |len: usize| unsafe { FramePool::attach_raw(s, pool::STATE_BYTES, p, len, Side::Decoder) };
        assert!(attach(pixels.len() * 8).is_err(), "no magic");
        // SAFETY: the vectors are large enough.
        unsafe { FramePool::init_raw(s, p, 2, 64, 64) };
        assert!(attach(pixels.len() * 8).is_ok());
        assert!(attach(1000).is_err(), "pixels too short");
        // SAFETY: inside the state page.
        unsafe { (s.add(pool::OFF_STRIDE) as *mut u32).write(48) };
        assert!(attach(pixels.len() * 8).is_err(), "stride narrower than a row");
        unsafe { FramePool::init_raw(s, p, 2, 64, 64) };
        unsafe { (s.add(pool::OFF_SLOTS) as *mut u32).write(17) };
        assert!(attach(pixels.len() * 8).is_err(), "too many slots");
        drop(state);
    }

    fn ring(capacity: u32) -> (Vec<u64>, PcmRing, PcmRing) {
        let mut mem = vec![0u64; PcmRing::bytes(capacity) / 8];
        let base = mem.as_mut_ptr() as *mut u8;
        // SAFETY: `mem` outlives both sides in each test.
        let player = unsafe { PcmRing::init_raw(base, capacity, 48_000) };
        let dec = unsafe { PcmRing::attach_raw(base, mem.len() * 8, Side::Decoder) }.unwrap();
        (mem, player, dec)
    }

    #[test]
    fn sound_records_wrap_in_order() {
        let (_m, player, dec) = ring(pcm::MIN_CAPACITY);
        let mut next = 0i16;
        let mut expect = 0i16;
        let mut pts = 0i64;
        let mut out = vec![0i16; 2 * 512];
        for round in 0..400u32 {
            let frames = 100 + (round * 37) % 900;
            let samples: Vec<i16> = (0..frames * 2)
                .map(|_| {
                    next = next.wrapping_add(1);
                    next
                })
                .collect();
            while !dec.write(7, pts, 0, &samples) {
                // Read some until it fits.
                let r = player.peek().unwrap().unwrap();
                assert_eq!(r.kind, pcm::kind::DATA);
                let n = player.copy(&r, &mut out);
                for s in &out[..n * 2] {
                    expect = expect.wrapping_add(1);
                    assert_eq!(*s, expect);
                }
                player.take(&r, n as u32);
            }
            pts += frames as i64 * 1_000_000 / 48_000;
        }
        dec.write_end(7);
        while let Some(r) = player.peek().unwrap() {
            if r.kind == pcm::kind::END {
                assert_eq!(r.generation, 7);
                player.take(&r, 0);
                break;
            }
            let n = player.copy(&r, &mut out);
            for s in &out[..n * 2] {
                expect = expect.wrapping_add(1);
                assert_eq!(*s, expect);
            }
            player.take(&r, n as u32);
        }
        assert_eq!(expect, next, "every sample arrived once");
        assert_eq!(player.peek(), Ok(None));
    }

    #[test]
    fn records_say_when_they_are_heard() {
        let (_m, player, dec) = ring(pcm::MIN_CAPACITY);
        assert!(dec.write(3, 1_000_000, pcm::record_flags::DISCONTINUITY, &[1; 960 * 2]));
        let r = player.peek().unwrap().unwrap();
        assert_eq!((r.generation, r.frames, r.taken, r.flags), (3, 960, 0, pcm::record_flags::DISCONTINUITY));
        player.take(&r, 480);
        let r = player.peek().unwrap().unwrap();
        assert_eq!(r.taken, 480);
        assert_eq!(r.next_pts(48_000), 1_010_000);
        player.discard_all();
        assert_eq!(player.peek(), Ok(None));
        assert!(dec.fits(pcm::MAX_FRAMES));
    }

    #[test]
    fn a_hostile_decoder_cannot_break_the_ring() {
        let (mem, player, dec) = ring(pcm::MIN_CAPACITY);
        dec.write(1, 0, 0, &[5; 64]);
        let base = mem.as_ptr() as *mut u8;
        // A record longer than what was written.
        // SAFETY: inside `mem`.
        unsafe { (base.add(pcm::DATA_OFFSET + pcm::REC_LENGTH) as *mut u32).write(4096) };
        assert_eq!(player.peek(), Err(SharedError::BadLayout));
        // A length that is not a whole number of records.
        unsafe { (base.add(pcm::DATA_OFFSET + pcm::REC_LENGTH) as *mut u32).write(40) };
        assert_eq!(player.peek(), Err(SharedError::BadLayout));
        // More frames than the record holds.
        unsafe { (base.add(pcm::DATA_OFFSET + pcm::REC_LENGTH) as *mut u32).write(160) };
        unsafe { (base.add(pcm::DATA_OFFSET + pcm::REC_FRAMES) as *mut u32).write(100) };
        assert_eq!(player.peek(), Err(SharedError::BadLayout));
        // An unknown kind.
        unsafe { (base.add(pcm::DATA_OFFSET + pcm::REC_KIND) as *mut u32).write(9) };
        assert_eq!(player.peek(), Err(SharedError::BadLayout));
        // A write position far ahead is clamped to the ring.
        let (_m2, player2, dec2) = ring(pcm::MIN_CAPACITY);
        dec2.u64_at(pcm::OFF_WRITE_POS).store(1 << 50, Ordering::Release);
        assert!(player2.filled() <= pcm::MIN_CAPACITY as usize);
        // And a hostile player cannot make the decoder overwrite records.
        let (_m3, player3, dec3) = ring(pcm::MIN_CAPACITY);
        dec3.write(1, 0, 0, &[1; 2000]);
        player3.u64_at(pcm::OFF_READ_POS).store(u64::MAX / 3, Ordering::Release);
        assert!(dec3.filled() <= pcm::MIN_CAPACITY as usize);
        assert!(
            !dec3.fits(pcm::MAX_FRAMES) || dec3.filled() + record_len(pcm::MAX_FRAMES) <= pcm::MIN_CAPACITY as usize
        );
    }

    #[test]
    fn the_largest_media_info_fits_in_a_message() {
        let text = |n: usize| "é".repeat(n / 2);
        let video = VideoStream {
            index: u32::MAX,
            codec: text(MAX_NAME),
            width: MAX_SIDE,
            height: MAX_SIDE,
            sample_aspect: Rational::new(u32::MAX, u32::MAX),
            rotation: 270,
            mirrored: true,
            frame_rate: Rational::new(u32::MAX, 1),
            bit_depth: 16,
            colour: Colour { primaries: 9, transfer: 16, matrix: 9, full_range: true },
            hdr: true,
            decodable: true,
            default: true,
            language: text(MAX_LANGUAGE),
            title: text(MAX_TEXT),
        };
        let audio = AudioStream {
            index: u32::MAX,
            codec: text(MAX_NAME),
            rate: 192_000,
            channels: 8,
            decodable: true,
            default: true,
            language: text(MAX_LANGUAGE),
            title: text(MAX_TEXT),
        };
        let info = MediaInfo {
            container: text(MAX_NAME),
            duration_us: Some(i64::MAX),
            seekable: true,
            bit_rate: u64::MAX,
            title: text(MAX_TEXT),
            artist: text(MAX_TEXT),
            video: vec![video; MAX_VIDEO_STREAMS],
            audio: vec![audio; MAX_AUDIO_STREAMS],
            subtitles: u32::MAX,
            chapters: vec![Chapter { start_us: i64::MIN, title: text(MAX_TEXT) }; MAX_CHAPTERS],
            selected_video: Some(u32::MAX),
            selected_audio: Some(u32::MAX),
        };
        assert!(info.is_valid());
        let mut e = vipc::Encoder::with_header(2, 1, vipc::FLAG_RESPONSE);
        vipc::Encode::encode(Ok::<MediaInfo, MediaError>(info), &mut e);
        assert!(e.bytes.len() < vabi::CHANNEL_MAX_BYTES, "{} bytes", e.bytes.len());
        // And the most checksums.
        let mut e = vipc::Encoder::with_header(12, 1, vipc::FLAG_RESPONSE);
        let sums = vec![PictureChecksum { pts_us: i64::MAX, md5: [0xFF; 16] }; MAX_CHECKSUMS as usize];
        vipc::Encode::encode(Ok::<Vec<PictureChecksum>, MediaError>(sums), &mut e);
        assert!(e.bytes.len() < vabi::CHANNEL_MAX_BYTES, "{} bytes", e.bytes.len());
    }

    #[test]
    fn pictures_are_shown_upright_and_square() {
        let square = Rational::new(1, 1);
        assert_eq!(display_size(1920, 1080, square, 0), (1920, 1080));
        assert_eq!(display_size(1920, 1080, Rational::default(), 90), (1080, 1920));
        // Anamorphic DVD: 720x480 shown 16:9 (wider) and 4:3 (taller).
        assert_eq!(display_size(720, 480, Rational::new(32, 27), 0), (854, 480));
        assert_eq!(display_size(720, 480, Rational::new(8, 9), 0), (720, 540));
        assert_eq!(display_size(720, 480, Rational::new(32, 27), 270), (480, 854));
        // Hostile values stay within bounds.
        let (w, h) = display_size(8192, 1, Rational::new(u32::MAX, 1), 0);
        assert!(w <= MAX_SIDE * 4 && h == 1);
    }

    #[test]
    fn sizes_fit_in_boxes() {
        assert_eq!(fit(1920, 1080, 1280, 800), (1280, 720));
        assert_eq!(fit(1080, 1920, 1280, 800), (450, 800));
        assert_eq!(fit(640, 360, 240, 135), (240, 135));
        assert_eq!(fit(4000, 1, 100, 100), (100, 1));
        assert_eq!(fit(0, 0, 0, 0), (1, 1));
    }

    #[test]
    fn media_info_is_checked() {
        let mut info = MediaInfo {
            container: "matroska".into(),
            duration_us: Some(1),
            seekable: true,
            bit_rate: 0,
            title: String::new(),
            artist: String::new(),
            video: Vec::new(),
            audio: Vec::new(),
            subtitles: 0,
            chapters: Vec::new(),
            selected_video: None,
            selected_audio: None,
        };
        assert!(info.is_valid());
        info.selected_audio = Some(1);
        assert!(!info.is_valid(), "a selected stream that is not listed");
        info.selected_audio = None;
        info.container = "x".repeat(MAX_NAME + 1);
        assert!(!info.is_valid());
        assert_eq!(bounded("aé", 2), "a");
        assert_eq!(bounded("abc", 8), "abc");
    }
}
