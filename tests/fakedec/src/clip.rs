//! What a fake video is: a text file whose first line is `FAKEVIDEO`
//! followed by options, `key=value` separated by spaces (the rest of the
//! file is not read):
//!
//! ```text
//! FAKEVIDEO duration=12.5 fps=30000/1001 w=1280 h=720 title=Night_Train crash=5
//! ```
//!
//! | Option | Default | Meaning |
//! |--------|---------|---------|
//! | `duration` | `10` | its length in seconds (decimals allowed) |
//! | `fps` | `30` | pictures a second, `30` or `30000/1001` (1 to 1000) |
//! | `w`, `h` | `640`, `360` | the pictures' size (1 to 8192) |
//! | `audio` | `1` | `0`: no sound |
//! | `rotation` | `0` | clockwise turn to show it upright: 0, 90, 180 or 270 |
//! | `title` | none | its title, underscores as spaces |
//! | `crash` | none | seconds: the decoder crashes decoding that time |
//! | `hang` | none | seconds: the decoder stops for good decoding that time |
//! | `slow_ms` | `0` | milliseconds each picture takes to decode |
//!
//! Anything else (another first word, an option it does not know, a value
//! out of range) is not a fake video.
//!
//! Picture `n` starts at `n / fps` seconds (rounded down to the µs) and
//! lasts until the next; every `fps`th picture (rounded: once a second) is
//! a key frame. Sound is counted in frames at the output's rate, frame `n`
//! heard at `n / rate` seconds.

use alloc::string::String;
use core::fmt;
use core::str::FromStr;

use vproto::media::{MAX_SIDE, MAX_TEXT, bounded};

/// The first word of a fake video.
pub const MAGIC: &str = "FAKEVIDEO";
/// Bytes of the first line at most.
pub const HEAD_BYTES: usize = 4096;
/// The longest time an option takes (100 hours, in seconds).
const MAX_SECONDS: i64 = 360_000;
/// The longest a picture may take to decode (ms).
const MAX_SLOW_MS: u32 = 10_000;

/// A fake video, as its file describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clip {
    /// Its length (µs, at least 1).
    pub duration_us: i64,
    /// Pictures a second: `fps_num / fps_den`, from 1 to 1000.
    pub fps_num: u32,
    pub fps_den: u32,
    /// The pictures' size as coded.
    pub width: u32,
    pub height: u32,
    /// It has sound.
    pub audio: bool,
    /// Clockwise turn to show it upright: 0, 90, 180 or 270.
    pub rotation: u32,
    pub title: String,
    /// Where decoding crashes, or hangs for good (µs).
    pub crash_us: Option<i64>,
    pub hang_us: Option<i64>,
    /// How long each picture takes to decode (ms).
    pub slow_ms: u32,
}

impl Default for Clip {
    fn default() -> Clip {
        Clip {
            duration_us: 10_000_000,
            fps_num: 30,
            fps_den: 1,
            width: 640,
            height: 360,
            audio: true,
            rotation: 0,
            title: String::new(),
            crash_us: None,
            hang_us: None,
            slow_ms: 0,
        }
    }
}

/// Why a file is not a fake video.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipError {
    /// Its first word is not [`MAGIC`] (or it is not text).
    NotFake,
    /// Its first line goes on past [`HEAD_BYTES`].
    TooLong,
    /// An option it does not know, or a value out of range.
    BadOption(String),
}

impl fmt::Display for ClipError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClipError::NotFake => write!(f, "it does not start with {MAGIC}"),
            ClipError::TooLong => f.write_str("its first line is too long"),
            ClipError::BadOption(o) => write!(f, "`{o}`: no such option, or a value out of range"),
        }
    }
}

/// A fault a clip asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// The decoder crashes.
    Crash,
    /// The decoder stops answering and working, for good.
    Hang,
}

/// A picture of a clip: which, when, and whether it is a key frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Picture {
    pub index: u64,
    pub pts_us: i64,
    pub duration_us: i64,
    pub key: bool,
}

impl Picture {
    /// When it ends (µs).
    pub fn end_us(&self) -> i64 {
        self.pts_us.saturating_add(self.duration_us)
    }
}

impl Clip {
    /// The clip the start of a file describes: `head` holds its first line
    /// (or the whole file, if shorter).
    pub fn parse(head: &[u8]) -> Result<Clip, ClipError> {
        let line = match head.iter().position(|&b| b == b'\n') {
            Some(end) => &head[..end],
            None if head.len() >= HEAD_BYTES => return Err(ClipError::TooLong),
            None => head,
        };
        let line = core::str::from_utf8(line).map_err(|_| ClipError::NotFake)?;
        let mut words = line.split_ascii_whitespace();
        if words.next() != Some(MAGIC) {
            return Err(ClipError::NotFake);
        }
        let mut clip = Clip::default();
        for word in words {
            let bad = || ClipError::BadOption(bounded(word, 64));
            let (key, value) = word.split_once('=').ok_or_else(bad)?;
            match key {
                "duration" => clip.duration_us = seconds(value).filter(|&d| d > 0).ok_or_else(bad)?,
                "fps" => (clip.fps_num, clip.fps_den) = frame_rate(value).ok_or_else(bad)?,
                "w" => clip.width = side(value).ok_or_else(bad)?,
                "h" => clip.height = side(value).ok_or_else(bad)?,
                "audio" => {
                    clip.audio = match value {
                        "0" => false,
                        "1" => true,
                        _ => return Err(bad()),
                    }
                }
                "rotation" => {
                    clip.rotation = number(value).filter(|r| matches!(r, 0 | 90 | 180 | 270)).ok_or_else(bad)?
                }
                "title" => clip.title = bounded(&value.replace('_', " "), MAX_TEXT),
                "crash" => clip.crash_us = Some(seconds(value).ok_or_else(bad)?),
                "hang" => clip.hang_us = Some(seconds(value).ok_or_else(bad)?),
                "slow_ms" => clip.slow_ms = number(value).filter(|&ms| ms <= MAX_SLOW_MS).ok_or_else(bad)?,
                _ => return Err(bad()),
            }
        }
        Ok(clip)
    }

    /// `1_000_000 * fps_den`: picture `n` starts at
    /// `n * per_picture() / fps_num` µs.
    fn per_picture(&self) -> u128 {
        1_000_000 * self.fps_den as u128
    }

    /// Its pictures: those that start before its end (at least one).
    pub fn frames(&self) -> u64 {
        (self.duration_us.max(1) as u128 * self.fps_num as u128).div_ceil(self.per_picture()) as u64
    }

    /// When picture `n` starts (µs).
    pub fn pts(&self, n: u64) -> i64 {
        (n as u128 * self.per_picture() / self.fps_num.max(1) as u128).min(i64::MAX as u128) as i64
    }

    /// Picture `n`.
    pub fn picture(&self, n: u64) -> Picture {
        let pts_us = self.pts(n);
        Picture { index: n, pts_us, duration_us: self.pts(n + 1) - pts_us, key: self.is_key(n) }
    }

    /// The picture shown at `t_us`: the last that starts at or before it
    /// (the first before the start, the last after the end).
    pub fn frame_at(&self, t_us: i64) -> u64 {
        let t = t_us.max(0) as u128;
        let n = ((t + 1) * self.fps_num as u128 - 1) / self.per_picture();
        (n.min(u64::MAX as u128) as u64).min(self.frames() - 1)
    }

    /// The first picture that starts at or after `t_us`.
    pub fn frame_from(&self, t_us: i64) -> u64 {
        let n = (t_us.max(0) as u128 * self.fps_num as u128).div_ceil(self.per_picture());
        n.min(u64::MAX as u128) as u64
    }

    /// Pictures from one key frame to the next: a second's, rounded.
    pub fn key_interval(&self) -> u64 {
        ((self.fps_num as u64 + self.fps_den as u64 / 2) / self.fps_den.max(1) as u64).max(1)
    }

    pub fn is_key(&self, n: u64) -> bool {
        n.is_multiple_of(self.key_interval())
    }

    /// Whether other pictures refer to picture `n`: key frames and every
    /// other picture after them (the rest are left out first, as B-frames
    /// are).
    pub fn is_reference(&self, n: u64) -> bool {
        (n % self.key_interval()).is_multiple_of(2)
    }

    /// The key frame at or before picture `n`.
    pub fn key_before(&self, n: u64) -> u64 {
        n - n % self.key_interval()
    }

    /// The key frame at or after picture `n`.
    pub fn key_after(&self, n: u64) -> u64 {
        n.div_ceil(self.key_interval()).saturating_mul(self.key_interval())
    }

    /// Frames of its sound at `rate`: up to its end.
    pub fn sound_frames(&self, rate: u32) -> u64 {
        sound_frame(self.duration_us, rate)
    }

    /// The fault met decoding what is seen or heard from `from_us` until
    /// `to_us`, and where it is.
    pub fn fault(&self, from_us: i64, to_us: i64) -> Option<(Fault, i64)> {
        let within = |t: Option<i64>| t.filter(|&t| from_us <= t && t < to_us);
        within(self.crash_us).map(|t| (Fault::Crash, t)).or_else(|| within(self.hang_us).map(|t| (Fault::Hang, t)))
    }
}

/// The first frame of sound at `rate` heard at or after `t_us`.
pub fn sound_frame(t_us: i64, rate: u32) -> u64 {
    (t_us.max(0) as u128 * rate as u128).div_ceil(1_000_000).min(u64::MAX as u128) as u64
}

/// When frame `n` of sound at `rate` is heard (µs).
pub fn sound_time(n: u64, rate: u32) -> i64 {
    (n as u128 * 1_000_000 / rate.max(1) as u128).min(i64::MAX as u128) as i64
}

/// A time as logs show it.
pub struct Secs(pub i64);

impl fmt::Display for Secs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{:03} s", self.0 / 1_000_000, self.0 % 1_000_000 / 1000)
    }
}

/// Digits only (no sign), as a `T`.
fn number<T: FromStr>(s: &str) -> Option<T> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// `12`, `12.5` or `.25` seconds, in µs (digits past the µs dropped).
fn seconds(s: &str) -> Option<i64> {
    let (whole, fraction) = s.split_once('.').unwrap_or((s, ""));
    if whole.is_empty() && fraction.is_empty() {
        return None;
    }
    let whole: i64 = if whole.is_empty() { 0 } else { number(whole)? };
    if whole > MAX_SECONDS || !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut us = whole * 1_000_000;
    let mut unit = 100_000;
    for b in fraction.bytes().take(6) {
        us += (b - b'0') as i64 * unit;
        unit /= 10;
    }
    Some(us)
}

/// `30` or `30000/1001` pictures a second, from 1 to 1000.
fn frame_rate(s: &str) -> Option<(u32, u32)> {
    let (num, den) = s.split_once('/').unwrap_or((s, "1"));
    let (num, den): (u32, u32) = (number(num)?, number(den)?);
    (den >= 1 && num >= den && num as u64 <= 1000 * den as u64).then_some((num, den))
}

/// A picture's width or height.
fn side(s: &str) -> Option<u32> {
    number(s).filter(|v| (1..=MAX_SIDE).contains(v))
}
