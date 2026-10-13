// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! A small Matroska muxer for the sample videos: AV1 video (track 1) and,
//! optionally, FLAC sound (track 2).
//!
//! The file is an EBML header (DocType `matroska`, version 4) and a
//! Segment of known size holding a SeekHead (to Info, Tracks and Cues),
//! Info (millisecond timestamps, the duration, the title), Tracks, one
//! Cluster per video key frame with the SimpleBlocks of both tracks in
//! time order, and Cues with a CuePoint per key frame. Everything is built
//! in memory, so every size is known.

use std::fmt;

/// Element IDs (with their length marker bits, as written).
pub mod id {
    pub const EBML: u32 = 0x1A45_DFA3;
    pub const EBML_VERSION: u32 = 0x4286;
    pub const EBML_READ_VERSION: u32 = 0x42F7;
    pub const EBML_MAX_ID_LENGTH: u32 = 0x42F2;
    pub const EBML_MAX_SIZE_LENGTH: u32 = 0x42F3;
    pub const DOC_TYPE: u32 = 0x4282;
    pub const DOC_TYPE_VERSION: u32 = 0x4287;
    pub const DOC_TYPE_READ_VERSION: u32 = 0x4285;
    pub const SEGMENT: u32 = 0x1853_8067;
    pub const SEEK_HEAD: u32 = 0x114D_9B74;
    pub const SEEK: u32 = 0x4DBB;
    pub const SEEK_ID: u32 = 0x53AB;
    pub const SEEK_POSITION: u32 = 0x53AC;
    pub const INFO: u32 = 0x1549_A966;
    pub const TIMESTAMP_SCALE: u32 = 0x2A_D7B1;
    pub const DURATION: u32 = 0x4489;
    pub const TITLE: u32 = 0x7BA9;
    pub const MUXING_APP: u32 = 0x4D80;
    pub const WRITING_APP: u32 = 0x5741;
    pub const TRACKS: u32 = 0x1654_AE6B;
    pub const TRACK_ENTRY: u32 = 0xAE;
    pub const TRACK_NUMBER: u32 = 0xD7;
    pub const TRACK_UID: u32 = 0x73C5;
    pub const TRACK_TYPE: u32 = 0x83;
    pub const FLAG_LACING: u32 = 0x9C;
    pub const CODEC_ID: u32 = 0x86;
    pub const CODEC_PRIVATE: u32 = 0x63A2;
    pub const DEFAULT_DURATION: u32 = 0x23_E383;
    pub const VIDEO: u32 = 0xE0;
    pub const PIXEL_WIDTH: u32 = 0xB0;
    pub const PIXEL_HEIGHT: u32 = 0xBA;
    pub const COLOUR: u32 = 0x55B0;
    pub const MATRIX_COEFFICIENTS: u32 = 0x55B1;
    pub const RANGE: u32 = 0x55B9;
    pub const TRANSFER_CHARACTERISTICS: u32 = 0x55BA;
    pub const PRIMARIES: u32 = 0x55BB;
    pub const AUDIO: u32 = 0xE1;
    pub const SAMPLING_FREQUENCY: u32 = 0xB5;
    pub const CHANNELS: u32 = 0x9F;
    pub const BIT_DEPTH: u32 = 0x6264;
    pub const CLUSTER: u32 = 0x1F43_B675;
    pub const TIMESTAMP: u32 = 0xE7;
    pub const SIMPLE_BLOCK: u32 = 0xA3;
    pub const CUES: u32 = 0x1C53_BB6B;
    pub const CUE_POINT: u32 = 0xBB;
    pub const CUE_TIME: u32 = 0xB3;
    pub const CUE_TRACK_POSITIONS: u32 = 0xB7;
    pub const CUE_TRACK: u32 = 0xF7;
    pub const CUE_CLUSTER_POSITION: u32 = 0xF1;
}

/// The muxing and writing application.
const APP: &str = "Veda videogen";

/// The track numbers.
pub const VIDEO_TRACK: u8 = 1;
pub const AUDIO_TRACK: u8 = 2;

/// One frame of a track.
#[derive(Clone, Copy, Debug)]
pub struct Frame<'a> {
    /// Presentation time in milliseconds.
    pub time_ms: u64,
    pub key: bool,
    pub data: &'a [u8],
}

/// The video track (AV1).
pub struct Video<'a> {
    pub width: u32,
    pub height: u32,
    /// The duration of a frame in nanoseconds.
    pub frame_ns: u64,
    /// The AV1CodecConfigurationRecord.
    pub codec_private: &'a [u8],
    /// In display order; the first must be a key frame.
    pub frames: Vec<Frame<'a>>,
}

/// The sound track (FLAC).
pub struct Audio<'a> {
    pub rate: u32,
    pub channels: u8,
    pub bit_depth: u8,
    /// `fLaC` and the STREAMINFO block.
    pub codec_private: &'a [u8],
    /// One FLAC frame each, in order.
    pub frames: Vec<Frame<'a>>,
}

/// What goes into a file.
pub struct Movie<'a> {
    pub title: &'a str,
    pub duration_ms: f64,
    pub video: Video<'a>,
    pub audio: Option<Audio<'a>>,
}

/// Why a movie cannot be muxed.
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    /// The video does not start with a key frame (or has no frames).
    NoKeyFrame,
    /// A frame is too far from its cluster's start for a SimpleBlock.
    TooFar(u64),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NoKeyFrame => write!(f, "the video does not start with a key frame"),
            Error::TooFar(t) => write!(f, "the frame at {t} ms is too far from its cluster's start"),
        }
    }
}

/// Writes an element ID (its bytes without leading zeros).
fn put_id(out: &mut Vec<u8>, id: u32) {
    let bytes = id.to_be_bytes();
    let skip = (id.leading_zeros() / 8).min(3) as usize;
    out.extend_from_slice(&bytes[skip..]);
}

/// Writes an element size as the shortest variable-size integer (all ones
/// would mean "unknown").
fn put_size(out: &mut Vec<u8>, n: u64) {
    let mut len = 1;
    while len < 8 && n >= (1u64 << (7 * len)) - 1 {
        len += 1;
    }
    let v = n | 1u64 << (7 * len);
    out.extend_from_slice(&v.to_be_bytes()[8 - len..]);
}

fn element(out: &mut Vec<u8>, id: u32, body: &[u8]) {
    put_id(out, id);
    put_size(out, body.len() as u64);
    out.extend_from_slice(body);
}

/// An unsigned integer in as few bytes as it needs (one at least).
fn uint(out: &mut Vec<u8>, id: u32, v: u64) {
    let len = (8 - v.leading_zeros() as usize / 8).max(1);
    element(out, id, &v.to_be_bytes()[8 - len..]);
}

fn float(out: &mut Vec<u8>, id: u32, v: f64) {
    element(out, id, &v.to_be_bytes());
}

fn string(out: &mut Vec<u8>, id: u32, s: &str) {
    element(out, id, s.as_bytes());
}

/// Builds the file.
pub fn mux(movie: &Movie<'_>) -> Result<Vec<u8>, Error> {
    let video = &movie.video;
    if !video.frames.first().is_some_and(|f| f.key) {
        return Err(Error::NoKeyFrame);
    }

    let mut ebml = Vec::new();
    uint(&mut ebml, id::EBML_VERSION, 1);
    uint(&mut ebml, id::EBML_READ_VERSION, 1);
    uint(&mut ebml, id::EBML_MAX_ID_LENGTH, 4);
    uint(&mut ebml, id::EBML_MAX_SIZE_LENGTH, 8);
    string(&mut ebml, id::DOC_TYPE, "matroska");
    uint(&mut ebml, id::DOC_TYPE_VERSION, 4);
    uint(&mut ebml, id::DOC_TYPE_READ_VERSION, 2);

    let mut info = Vec::new();
    uint(&mut info, id::TIMESTAMP_SCALE, 1_000_000);
    float(&mut info, id::DURATION, movie.duration_ms);
    string(&mut info, id::TITLE, movie.title);
    string(&mut info, id::MUXING_APP, APP);
    string(&mut info, id::WRITING_APP, APP);

    let mut tracks = Vec::new();
    {
        let mut entry = Vec::new();
        uint(&mut entry, id::TRACK_NUMBER, VIDEO_TRACK as u64);
        uint(&mut entry, id::TRACK_UID, VIDEO_TRACK as u64);
        uint(&mut entry, id::TRACK_TYPE, 1);
        uint(&mut entry, id::FLAG_LACING, 0);
        string(&mut entry, id::CODEC_ID, "V_AV1");
        element(&mut entry, id::CODEC_PRIVATE, video.codec_private);
        uint(&mut entry, id::DEFAULT_DURATION, video.frame_ns);
        let mut colour = Vec::new();
        // BT.709 matrix, limited ("broadcast") range, BT.709 transfer and primaries.
        uint(&mut colour, id::MATRIX_COEFFICIENTS, 1);
        uint(&mut colour, id::RANGE, 1);
        uint(&mut colour, id::TRANSFER_CHARACTERISTICS, 1);
        uint(&mut colour, id::PRIMARIES, 1);
        let mut v = Vec::new();
        uint(&mut v, id::PIXEL_WIDTH, video.width as u64);
        uint(&mut v, id::PIXEL_HEIGHT, video.height as u64);
        element(&mut v, id::COLOUR, &colour);
        element(&mut entry, id::VIDEO, &v);
        element(&mut tracks, id::TRACK_ENTRY, &entry);
    }
    if let Some(audio) = &movie.audio {
        let mut entry = Vec::new();
        uint(&mut entry, id::TRACK_NUMBER, AUDIO_TRACK as u64);
        uint(&mut entry, id::TRACK_UID, AUDIO_TRACK as u64);
        uint(&mut entry, id::TRACK_TYPE, 2);
        uint(&mut entry, id::FLAG_LACING, 0);
        string(&mut entry, id::CODEC_ID, "A_FLAC");
        element(&mut entry, id::CODEC_PRIVATE, audio.codec_private);
        let mut a = Vec::new();
        float(&mut a, id::SAMPLING_FREQUENCY, audio.rate as f64);
        uint(&mut a, id::CHANNELS, audio.channels as u64);
        uint(&mut a, id::BIT_DEPTH, audio.bit_depth as u64);
        element(&mut entry, id::AUDIO, &a);
        element(&mut tracks, id::TRACK_ENTRY, &entry);
    }

    // A cluster from every key frame to the next; sound goes into the
    // cluster its time falls in.
    let starts: Vec<u64> = video.frames.iter().filter(|f| f.key).map(|f| f.time_ms).collect();
    let cluster_of = |t: u64| starts.partition_point(|&s| s <= t).saturating_sub(1);
    let mut blocks: Vec<Vec<(u64, u8, &Frame<'_>)>> = vec![Vec::new(); starts.len()];
    for f in &video.frames {
        blocks[cluster_of(f.time_ms)].push((f.time_ms, VIDEO_TRACK, f));
    }
    if let Some(audio) = &movie.audio {
        for f in &audio.frames {
            blocks[cluster_of(f.time_ms)].push((f.time_ms, AUDIO_TRACK, f));
        }
    }
    let mut clusters = Vec::with_capacity(starts.len());
    for (start, mut list) in starts.iter().zip(blocks) {
        // Stable: frames of one track keep their order.
        list.sort_by_key(|&(t, track, _)| (t, track));
        let mut body = Vec::new();
        uint(&mut body, id::TIMESTAMP, *start);
        for (t, track, f) in list {
            let rel = i16::try_from(t as i64 - *start as i64).map_err(|_| Error::TooFar(t))?;
            let mut block = Vec::with_capacity(f.data.len() + 4);
            block.push(0x80 | track);
            block.extend_from_slice(&rel.to_be_bytes());
            // Audio frames are all key frames.
            block.push(if f.key || track == AUDIO_TRACK { 0x80 } else { 0 });
            block.extend_from_slice(f.data);
            element(&mut body, id::SIMPLE_BLOCK, &block);
        }
        let mut cluster = Vec::new();
        element(&mut cluster, id::CLUSTER, &body);
        clusters.push(cluster);
    }

    // The SeekHead's positions are written in eight bytes, so that its size
    // does not depend on them.
    let seek_head = |positions: [u64; 3]| {
        let mut body = Vec::new();
        for (target, pos) in [id::INFO, id::TRACKS, id::CUES].into_iter().zip(positions) {
            let mut seek = Vec::new();
            let mut target_id = Vec::new();
            put_id(&mut target_id, target);
            element(&mut seek, id::SEEK_ID, &target_id);
            element(&mut seek, id::SEEK_POSITION, &pos.to_be_bytes());
            element(&mut body, id::SEEK, &seek);
        }
        let mut out = Vec::new();
        element(&mut out, id::SEEK_HEAD, &body);
        out
    };
    let wrapped = |id: u32, body: &[u8]| {
        let mut out = Vec::new();
        element(&mut out, id, body);
        out
    };
    let info = wrapped(id::INFO, &info);
    let tracks = wrapped(id::TRACKS, &tracks);
    // Positions are relative to the start of the Segment's data.
    let info_at = seek_head([0; 3]).len() as u64;
    let tracks_at = info_at + info.len() as u64;
    let mut at = tracks_at + tracks.len() as u64;
    let mut cues = Vec::new();
    for (start, cluster) in starts.iter().zip(&clusters) {
        let mut positions = Vec::new();
        uint(&mut positions, id::CUE_TRACK, VIDEO_TRACK as u64);
        uint(&mut positions, id::CUE_CLUSTER_POSITION, at);
        let mut point = Vec::new();
        uint(&mut point, id::CUE_TIME, *start);
        element(&mut point, id::CUE_TRACK_POSITIONS, &positions);
        element(&mut cues, id::CUE_POINT, &point);
        at += cluster.len() as u64;
    }
    let cues = wrapped(id::CUES, &cues);
    let head = seek_head([info_at, tracks_at, at]);

    let mut segment = Vec::with_capacity(at as usize + cues.len());
    for part in [&head, &info, &tracks] {
        segment.extend_from_slice(part);
    }
    for c in &clusters {
        segment.extend_from_slice(c);
    }
    segment.extend_from_slice(&cues);

    let mut out = Vec::with_capacity(segment.len() + 64);
    element(&mut out, id::EBML, &ebml);
    element(&mut out, id::SEGMENT, &segment);
    Ok(out)
}

/// An EBML reader for the tests.
#[cfg(test)]
pub mod read {
    /// An element of a file.
    #[derive(Clone, Copy, Debug)]
    pub struct Element<'a> {
        pub id: u32,
        /// Where its ID starts in the data given to [`children`].
        pub offset: usize,
        /// Where its data starts.
        pub data_offset: usize,
        pub data: &'a [u8],
    }

    impl<'a> Element<'a> {
        pub fn children(&self) -> Vec<Element<'a>> {
            let mut list = children(self.data).expect("well-formed children");
            for e in &mut list {
                e.offset += self.data_offset;
                e.data_offset += self.data_offset;
            }
            list
        }

        pub fn uint(&self) -> u64 {
            assert!(self.data.len() <= 8);
            self.data.iter().fold(0, |v, &b| v << 8 | b as u64)
        }

        pub fn float(&self) -> f64 {
            match self.data.len() {
                4 => f32::from_be_bytes(self.data.try_into().unwrap()) as f64,
                8 => f64::from_be_bytes(self.data.try_into().unwrap()),
                n => panic!("a float of {n} bytes"),
            }
        }

        pub fn string(&self) -> &'a str {
            std::str::from_utf8(self.data).expect("UTF-8")
        }

        /// The first child with this ID.
        pub fn child(&self, id: u32) -> Element<'a> {
            self.children().into_iter().find(|e| e.id == id).unwrap_or_else(|| panic!("no element {id:x}"))
        }
    }

    /// The elements one after another in `data`.
    pub fn children(data: &[u8]) -> Result<Vec<Element<'_>>, String> {
        let mut out = Vec::new();
        let mut at = 0;
        while at < data.len() {
            let offset = at;
            let first = data[at];
            let len = first.leading_zeros() as usize + 1;
            if len > 4 || at + len > data.len() {
                return Err(format!("a bad ID at {at}"));
            }
            let id = data[at..at + len].iter().fold(0u32, |v, &b| v << 8 | b as u32);
            at += len;
            let first = *data.get(at).ok_or("no size")?;
            let len = first.leading_zeros() as usize + 1;
            if len > 8 || at + len > data.len() {
                return Err(format!("a bad size at {at}"));
            }
            let mut size = (first as u64) & (0xFF >> len);
            for &b in &data[at + 1..at + len] {
                size = size << 8 | b as u64;
            }
            if size == (1u64 << (7 * len)) - 1 {
                return Err(format!("an unknown size at {at}"));
            }
            at += len;
            let end = at + size as usize;
            if end > data.len() {
                return Err(format!("element {id:x} at {offset} runs past the end"));
            }
            out.push(Element { id, offset, data_offset: at, data: &data[at..end] });
            at = end;
        }
        Ok(out)
    }

    /// A SimpleBlock: (track, relative time, flags, frame).
    pub fn simple_block(data: &[u8]) -> (u8, i16, u8, &[u8]) {
        assert!(data[0] & 0x80 != 0, "a one-byte track number");
        (data[0] & 0x7F, i16::from_be_bytes([data[1], data[2]]), data[3], &data[4..])
    }
}

#[cfg(test)]
mod tests {
    use super::read::{Element, children, simple_block};
    use super::*;

    fn size(n: u64) -> Vec<u8> {
        let mut v = Vec::new();
        put_size(&mut v, n);
        v
    }

    #[test]
    fn sizes_are_short_and_never_unknown() {
        assert_eq!(size(0), [0x80]);
        assert_eq!(size(126), [0xFE]);
        // 0xFF would be "unknown".
        assert_eq!(size(127), [0x40, 0x7F]);
        assert_eq!(size(16_382), [0x7F, 0xFE]);
        assert_eq!(size(16_383), [0x20, 0x3F, 0xFF]);
        // Six bytes: the marker bit, then 2^40.
        assert_eq!(size(1 << 40), [0x05, 0, 0, 0, 0, 0]);
        let mut v = Vec::new();
        put_id(&mut v, id::SEGMENT);
        put_id(&mut v, id::SEEK);
        put_id(&mut v, id::CUE_TIME);
        put_id(&mut v, id::TIMESTAMP_SCALE);
        assert_eq!(v, [0x18, 0x53, 0x80, 0x67, 0x4D, 0xBB, 0xB3, 0x2A, 0xD7, 0xB1]);
        let mut v = Vec::new();
        uint(&mut v, id::TRACK_NUMBER, 0);
        uint(&mut v, id::TRACK_NUMBER, 300);
        assert_eq!(v, [0xD7, 0x81, 0, 0xD7, 0x82, 0x01, 0x2C]);
    }

    struct Parsed<'a> {
        segment: Element<'a>,
        top: Vec<Element<'a>>,
    }

    fn parse(file: &[u8]) -> Parsed<'_> {
        let top = children(file).unwrap();
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].id, id::EBML);
        let ebml = top[0];
        assert_eq!(ebml.child(id::DOC_TYPE).string(), "matroska");
        assert_eq!(ebml.child(id::DOC_TYPE_VERSION).uint(), 4);
        assert_eq!(ebml.child(id::DOC_TYPE_READ_VERSION).uint(), 2);
        assert_eq!(ebml.child(id::EBML_MAX_SIZE_LENGTH).uint(), 8);
        let segment = top[1];
        assert_eq!(segment.id, id::SEGMENT);
        assert_eq!(segment.data_offset + segment.data.len(), file.len(), "the Segment's size is the rest");
        Parsed { segment, top: segment.children() }
    }

    /// A movie at 30 fps with a key frame every 5 frames and FLAC-sized
    /// sound blocks.
    fn movie<'a>(frames: &'a [Vec<u8>], sound: &'a [Vec<u8>]) -> Movie<'a> {
        Movie {
            title: "Test",
            duration_ms: 1000.0,
            video: Video {
                width: 64,
                height: 48,
                frame_ns: 33_333_333,
                codec_private: &[0x81, 0x1F, 0x0C, 0x00],
                frames: frames
                    .iter()
                    .enumerate()
                    .map(|(i, d)| Frame { time_ms: (i as u64 * 1000 + 15) / 30, key: i % 5 == 0, data: d })
                    .collect(),
            },
            audio: Some(Audio {
                rate: 48_000,
                channels: 2,
                bit_depth: 16,
                codec_private: b"fLaC\x80\0\0\x22",
                frames: sound
                    .iter()
                    .enumerate()
                    .map(|(i, d)| Frame { time_ms: (i as u64 * 4096 * 1000 + 24_000) / 48_000, key: true, data: d })
                    .collect(),
            }),
        }
    }

    #[test]
    fn a_movie_parses_back() {
        let frames: Vec<Vec<u8>> = (0..30u8).map(|i| vec![i; 10 + i as usize]).collect();
        let sound: Vec<Vec<u8>> = (0..12u8).map(|i| vec![0xA0 | i; 50]).collect();
        let m = movie(&frames, &sound);
        let file = mux(&m).unwrap();
        let p = parse(&file);
        let ids: Vec<u32> = p.top.iter().map(|e| e.id).collect();
        let mut expected = vec![id::SEEK_HEAD, id::INFO, id::TRACKS];
        expected.extend([id::CLUSTER; 6]);
        expected.push(id::CUES);
        assert_eq!(ids, expected);

        // The SeekHead points at Info, Tracks and Cues.
        let base = p.segment.data_offset;
        for seek in p.top[0].children() {
            let target = seek.child(id::SEEK_ID).data.iter().fold(0u32, |v, &b| v << 8 | b as u32);
            let pos = seek.child(id::SEEK_POSITION).uint() as usize;
            let e = p.top.iter().find(|e| e.offset == base + pos).expect("an element at the position");
            assert_eq!(e.id, target);
        }

        let info = p.top[1];
        assert_eq!(info.child(id::TIMESTAMP_SCALE).uint(), 1_000_000);
        assert_eq!(info.child(id::DURATION).float(), 1000.0);
        assert_eq!(info.child(id::TITLE).string(), "Test");
        assert_eq!(info.child(id::MUXING_APP).string(), "Veda videogen");
        assert_eq!(info.child(id::WRITING_APP).string(), "Veda videogen");

        let entries = p.top[2].children();
        assert_eq!(entries.len(), 2);
        let v = entries[0];
        assert_eq!(v.child(id::TRACK_NUMBER).uint(), 1);
        assert_eq!(v.child(id::TRACK_TYPE).uint(), 1);
        assert_eq!(v.child(id::CODEC_ID).string(), "V_AV1");
        assert_eq!(v.child(id::CODEC_PRIVATE).data, [0x81, 0x1F, 0x0C, 0x00]);
        assert_eq!(v.child(id::DEFAULT_DURATION).uint(), 33_333_333);
        let video = v.child(id::VIDEO);
        assert_eq!((video.child(id::PIXEL_WIDTH).uint(), video.child(id::PIXEL_HEIGHT).uint()), (64, 48));
        let colour = video.child(id::COLOUR);
        for e in [id::MATRIX_COEFFICIENTS, id::RANGE, id::TRANSFER_CHARACTERISTICS, id::PRIMARIES] {
            assert_eq!(colour.child(e).uint(), 1);
        }
        let a = entries[1];
        assert_eq!(a.child(id::TRACK_NUMBER).uint(), 2);
        assert_eq!(a.child(id::TRACK_TYPE).uint(), 2);
        assert_eq!(a.child(id::CODEC_ID).string(), "A_FLAC");
        assert_eq!(a.child(id::CODEC_PRIVATE).data, b"fLaC\x80\0\0\x22");
        let audio = a.child(id::AUDIO);
        assert_eq!(audio.child(id::SAMPLING_FREQUENCY).float(), 48_000.0);
        assert_eq!(audio.child(id::CHANNELS).uint(), 2);
        assert_eq!(audio.child(id::BIT_DEPTH).uint(), 16);

        // Clusters: one per key frame, blocks in time order, every frame once.
        let mut video_frames = Vec::new();
        let mut sound_frames = Vec::new();
        let mut last = 0;
        let clusters: Vec<&Element<'_>> = p.top.iter().filter(|e| e.id == id::CLUSTER).collect();
        for (i, c) in clusters.iter().enumerate() {
            let kids = c.children();
            assert_eq!(kids[0].id, id::TIMESTAMP);
            let start = kids[0].uint();
            assert_eq!(start, m.video.frames[i * 5].time_ms);
            for b in &kids[1..] {
                assert_eq!(b.id, id::SIMPLE_BLOCK);
                let (track, rel, flags, data) = simple_block(b.data);
                let t = (start as i64 + rel as i64) as u64;
                assert!(t >= last, "blocks in time order");
                last = t;
                match track {
                    1 => {
                        let n = video_frames.len();
                        assert_eq!(t, m.video.frames[n].time_ms);
                        assert_eq!(flags & 0x80 != 0, n % 5 == 0, "frame {n}'s key flag");
                        assert_eq!(rel == 0, n % 5 == 0, "clusters start with their key frame");
                        video_frames.push(data);
                    }
                    2 => {
                        assert_eq!(flags, 0x80);
                        sound_frames.push(data);
                    }
                    t => panic!("track {t}"),
                }
            }
        }
        assert_eq!(video_frames, frames.iter().map(|f| f.as_slice()).collect::<Vec<_>>());
        assert_eq!(sound_frames, sound.iter().map(|f| f.as_slice()).collect::<Vec<_>>());

        // Cues: every key frame, pointing at its cluster.
        let points = p.top.last().unwrap().children();
        assert_eq!(points.len(), clusters.len());
        for (point, cluster) in points.iter().zip(&clusters) {
            assert_eq!(point.id, id::CUE_POINT);
            let time = point.child(id::CUE_TIME).uint();
            let pos = point.child(id::CUE_TRACK_POSITIONS);
            assert_eq!(pos.child(id::CUE_TRACK).uint(), 1);
            assert_eq!(base + pos.child(id::CUE_CLUSTER_POSITION).uint() as usize, cluster.offset);
            assert_eq!(time, cluster.children()[0].uint());
        }
    }

    #[test]
    fn a_silent_movie_has_one_track() {
        let frames: Vec<Vec<u8>> = (0..7u8).map(|i| vec![i; 3]).collect();
        let mut m = movie(&frames, &[]);
        m.audio = None;
        let file = mux(&m).unwrap();
        let p = parse(&file);
        let tracks = p.top.iter().find(|e| e.id == id::TRACKS).unwrap();
        assert_eq!(tracks.children().len(), 1);
        let blocks: usize = p.top.iter().filter(|e| e.id == id::CLUSTER).map(|c| c.children().len() - 1).sum();
        assert_eq!(blocks, 7);
    }

    #[test]
    fn bad_movies_are_refused() {
        let frames: Vec<Vec<u8>> = (0..3u8).map(|i| vec![i; 3]).collect();
        let mut m = movie(&frames, &[]);
        m.video.frames[0].key = false;
        assert_eq!(mux(&m).err(), Some(Error::NoKeyFrame));
        m.video.frames.clear();
        assert_eq!(mux(&m).err(), Some(Error::NoKeyFrame));
        let mut m = movie(&frames, &[]);
        m.video.frames[2].time_ms = 40_000;
        assert_eq!(mux(&m).err(), Some(Error::TooFar(40_000)));
    }
}
