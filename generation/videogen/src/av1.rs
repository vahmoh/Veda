// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! AV1 encoding with rav1e: the settings every sample shares, the packets
//! as Matroska stores them and the fingerprints of the pictures a decoder
//! makes of them.
//!
//! rav1e is built without assembly and without threads, so an encode gives
//! the same bytes on every run. Low latency mode makes every packet one
//! shown frame, in display order, and each packet carries the encoder's
//! reconstruction of its frame (`Packet::rec`, after the loop filters):
//! the very picture a decoder outputs, since AV1 decoding is exact and
//! there is no film grain to add on top.

use std::fmt;

use rav1e::prelude::{
    ChromaSampling, ColorDescription, ColorPrimaries, Config, Context, EncoderConfig, EncoderStatus, Frame, FrameType,
    InvalidConfig, MatrixCoefficients, PixelRange, Rational, SceneDetectionSpeed, TransferCharacteristics,
};

use crate::yuv::Yuv420;
use md5::{Digest, Md5};

/// OBU types (AV1 specification, section 6.2.2).
pub const OBU_SEQUENCE_HEADER: u8 = 1;
pub const OBU_TEMPORAL_DELIMITER: u8 = 2;

/// How to encode a video.
#[derive(Clone, Copy, Debug)]
pub struct Settings {
    pub width: usize,
    pub height: usize,
    /// Frames per second.
    pub fps: u64,
    /// Frames from one key frame to the next (fixed).
    pub key_interval: u64,
    /// The base quantizer index (0-255; rav1e gives key frames a finer one).
    pub quantizer: usize,
    /// rav1e's speed preset (0-10).
    pub speed: u8,
}

impl Settings {
    /// rav1e's configuration for these settings.
    pub fn encoder_config(&self) -> EncoderConfig {
        let mut enc = EncoderConfig::with_speed_preset(self.speed);
        enc.width = self.width;
        enc.height = self.height;
        enc.time_base = Rational::new(1, self.fps);
        enc.bit_depth = 8;
        enc.chroma_sampling = ChromaSampling::Cs420;
        enc.pixel_range = PixelRange::Limited;
        enc.color_description = Some(ColorDescription {
            color_primaries: ColorPrimaries::BT709,
            transfer_characteristics: TransferCharacteristics::BT709,
            matrix_coefficients: MatrixCoefficients::BT709,
        });
        enc.low_latency = true;
        enc.min_key_frame_interval = self.key_interval;
        enc.max_key_frame_interval = self.key_interval;
        enc.quantizer = self.quantizer;
        enc.bitrate = 0;
        enc.film_grain_params = None;
        // Key frames at the fixed interval only.
        enc.speed_settings.scene_detection_mode = SceneDetectionSpeed::None;
        enc
    }
}

/// One frame as Matroska stores it.
#[derive(Clone, Debug)]
pub struct Packet {
    /// The temporal unit without its temporal delimiter.
    pub data: Vec<u8>,
    pub key: bool,
}

/// An encoded video.
#[derive(Clone, Debug)]
pub struct Video {
    /// The AV1CodecConfigurationRecord, with the sequence header OBU as its
    /// `configOBUs` (Matroska's CodecPrivate).
    pub config: Vec<u8>,
    /// Every frame, in display order.
    pub packets: Vec<Packet>,
    /// The MD5 of every decoded picture's visible planes, in display order.
    pub md5: Vec<[u8; 16]>,
    /// The decoded pictures asked for with [`Encoder::keep`].
    pub kept: Vec<(u64, Yuv420)>,
}

/// What can go wrong while encoding.
#[derive(Debug)]
pub enum Error {
    /// rav1e refuses the settings.
    Config(InvalidConfig),
    /// rav1e failed.
    Encoder(EncoderStatus),
    /// A picture of the wrong size.
    Size { expected: (usize, usize), got: (usize, usize) },
    /// The stream is not as these videos need it.
    Stream(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Config(e) => write!(f, "rav1e refuses the settings: {e}"),
            Error::Encoder(e) => write!(f, "rav1e failed: {e}"),
            Error::Size { expected, got } => {
                write!(f, "a {}x{} picture for a {}x{} video", got.0, got.1, expected.0, expected.1)
            }
            Error::Stream(what) => write!(f, "the AV1 stream {what}"),
        }
    }
}

/// An encode in progress: pictures go in one by one.
pub struct Encoder {
    ctx: Context<u8>,
    settings: Settings,
    sent: u64,
    sequence_header: Option<Vec<u8>>,
    packets: Vec<Packet>,
    md5: Vec<[u8; 16]>,
    keep: Vec<u64>,
    kept: Vec<(u64, Yuv420)>,
}

impl Encoder {
    pub fn new(settings: Settings) -> Result<Encoder, Error> {
        if settings.key_interval == 0 {
            return Err(Error::Stream("needs key frames".into()));
        }
        let config = Config::new().with_encoder_config(settings.encoder_config());
        let ctx = config.new_context().map_err(Error::Config)?;
        Ok(Encoder {
            ctx,
            settings,
            sent: 0,
            sequence_header: None,
            packets: Vec::new(),
            md5: Vec::new(),
            keep: Vec::new(),
            kept: Vec::new(),
        })
    }

    /// Keeps the decoded pictures of these frames (for previews).
    pub fn keep(&mut self, frames: Vec<u64>) {
        self.keep = frames;
    }

    /// Encodes the next picture (packets come out as rav1e's look-ahead allows).
    pub fn push(&mut self, picture: &Yuv420) -> Result<(), Error> {
        let expected = (self.settings.width, self.settings.height);
        if (picture.width, picture.height) != expected {
            return Err(Error::Size { expected, got: (picture.width, picture.height) });
        }
        let mut frame = self.ctx.new_frame();
        let (cw, _) = picture.chroma_size();
        frame.planes[0].copy_from_raw_u8(&picture.y, picture.width, 1);
        frame.planes[1].copy_from_raw_u8(&picture.u, cw, 1);
        frame.planes[2].copy_from_raw_u8(&picture.v, cw, 1);
        self.ctx.send_frame(frame).map_err(Error::Encoder)?;
        self.sent += 1;
        self.receive()?;
        Ok(())
    }

    /// Takes the packets rav1e has ready; true once it has made them all.
    fn receive(&mut self) -> Result<bool, Error> {
        loop {
            match self.ctx.receive_packet() {
                Ok(p) => self.take(p)?,
                Err(EncoderStatus::Encoded) => {}
                Err(EncoderStatus::NeedMoreData) => return Ok(false),
                Err(EncoderStatus::LimitReached) => return Ok(true),
                Err(e) => return Err(Error::Encoder(e)),
            }
        }
    }

    fn take(&mut self, p: rav1e::Packet<u8>) -> Result<(), Error> {
        let n = self.packets.len() as u64;
        if p.input_frameno != n {
            return Err(Error::Stream(format!("has frame {} where frame {n} belongs", p.input_frameno)));
        }
        let key = p.frame_type == FrameType::KEY;
        if key != n.is_multiple_of(self.settings.key_interval) {
            let what = if key { "an unexpected key frame" } else { "no key frame" };
            return Err(Error::Stream(format!("has {what} at frame {n}")));
        }
        let rec = p.rec.as_ref().ok_or_else(|| Error::Stream(format!("frame {n} has no reconstruction")))?;
        self.md5.push(picture_md5(rec, self.settings.width, self.settings.height));
        if self.keep.contains(&n) {
            self.kept.push((n, picture(rec, self.settings.width, self.settings.height)));
        }
        let obus = split_obus(&p.data).map_err(|e| Error::Stream(format!("frame {n}: {e}")))?;
        if key && self.sequence_header.is_none() {
            let sh = obus.iter().find(|o| o.kind == OBU_SEQUENCE_HEADER);
            let sh = sh.ok_or_else(|| Error::Stream("starts without a sequence header".into()))?;
            self.sequence_header = Some(p.data[sh.bytes.clone()].to_vec());
        }
        let mut data = Vec::with_capacity(p.data.len());
        for o in obus.iter().filter(|o| o.kind != OBU_TEMPORAL_DELIMITER) {
            data.extend_from_slice(&p.data[o.bytes.clone()]);
        }
        self.packets.push(Packet { data, key });
        Ok(())
    }

    /// Encodes what is left and returns the video.
    pub fn finish(mut self) -> Result<Video, Error> {
        self.ctx.flush();
        if !self.receive()? {
            return Err(Error::Stream("ended early".into()));
        }
        if self.packets.len() as u64 != self.sent {
            return Err(Error::Stream(format!("has {} frames of {}", self.packets.len(), self.sent)));
        }
        let sequence_header = self.sequence_header.ok_or_else(|| Error::Stream("is empty".into()))?;
        let mut config = self.ctx.container_sequence_header();
        config.extend_from_slice(&sequence_header);
        Ok(Video { config, packets: self.packets, md5: self.md5, kept: self.kept })
    }
}

/// The MD5 of a picture's visible planes as a decoder hashes them: Y
/// (`width` bytes by `height` rows), then U and V (half of each, rounded up).
fn picture_md5(frame: &Frame<u8>, width: usize, height: usize) -> [u8; 16] {
    let mut md5 = Md5::new();
    for (i, plane) in frame.planes.iter().enumerate() {
        let (w, h) = if i == 0 { (width, height) } else { (width.div_ceil(2), height.div_ceil(2)) };
        let stride = plane.cfg.stride;
        let data = plane.data_origin();
        for y in 0..h {
            md5.update(&data[y * stride..y * stride + w]);
        }
    }
    md5.finalize().into()
}

/// The visible part of a frame.
fn picture(frame: &Frame<u8>, width: usize, height: usize) -> Yuv420 {
    let plane = |i: usize, w: usize, h: usize| {
        let p = &frame.planes[i];
        let data = p.data_origin();
        (0..h).flat_map(|y| data[y * p.cfg.stride..y * p.cfg.stride + w].iter().copied()).collect()
    };
    let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
    Yuv420 { width, height, y: plane(0, width, height), u: plane(1, cw, ch), v: plane(2, cw, ch) }
}

/// One OBU of a temporal unit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Obu {
    pub kind: u8,
    /// The whole OBU (header, size and payload) within the temporal unit.
    pub bytes: std::ops::Range<usize>,
}

/// Splits a temporal unit in the low overhead format (section 5.2) into
/// OBUs. Only the last OBU may lack a size field.
pub fn split_obus(data: &[u8]) -> Result<Vec<Obu>, String> {
    let mut obus = Vec::new();
    let mut at = 0;
    while at < data.len() {
        let start = at;
        let header = data[at];
        if header & 0x80 != 0 {
            return Err(format!("an OBU at {start} has its forbidden bit set"));
        }
        let kind = (header >> 3) & 0xF;
        let extension = header & 0x04 != 0;
        let has_size = header & 0x02 != 0;
        at += 1 + extension as usize;
        if at > data.len() {
            return Err(format!("an OBU header at {start} is cut short"));
        }
        let end = if has_size {
            let (size, len) = leb128(data.get(at..).unwrap_or(&[]))?;
            at += len;
            usize::try_from(size).ok().and_then(|s| at.checked_add(s)).ok_or("an OBU is too large")?
        } else {
            data.len()
        };
        if end > data.len() {
            return Err(format!("an OBU at {start} runs past the end"));
        }
        obus.push(Obu { kind, bytes: start..end });
        at = end;
    }
    Ok(obus)
}

/// Reads a `leb128()` number: (value, bytes read).
fn leb128(data: &[u8]) -> Result<(u64, usize), String> {
    let mut value = 0u64;
    for (i, &b) in data.iter().take(8).enumerate() {
        value |= ((b & 0x7F) as u64) << (7 * i);
        if b & 0x80 == 0 {
            return Ok((value, i + 1));
        }
    }
    Err("an OBU size is cut short".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obu(kind: u8, payload: &[u8]) -> Vec<u8> {
        let mut v = vec![kind << 3 | 0x02];
        let mut n = payload.len();
        loop {
            let b = (n & 0x7F) as u8;
            n >>= 7;
            v.push(if n > 0 { b | 0x80 } else { b });
            if n == 0 {
                break;
            }
        }
        v.extend_from_slice(payload);
        v
    }

    #[test]
    fn obus_split_and_lose_their_temporal_delimiter() {
        let mut tu = obu(OBU_TEMPORAL_DELIMITER, &[]);
        tu.extend(obu(OBU_SEQUENCE_HEADER, &[1, 2, 3]));
        tu.extend(obu(6, &[7; 300]));
        let obus = split_obus(&tu).unwrap();
        let kinds: Vec<u8> = obus.iter().map(|o| o.kind).collect();
        assert_eq!(kinds, [OBU_TEMPORAL_DELIMITER, OBU_SEQUENCE_HEADER, 6]);
        assert_eq!(obus[0].bytes, 0..2);
        assert_eq!(obus[1].bytes, 2..7);
        // 300 bytes need a two-byte size.
        assert_eq!(obus[2].bytes, 7..7 + 3 + 300);
        // An extension byte and a last OBU without a size.
        let tail = [6 << 3 | 0x04, 0x08, 9, 9, 9];
        let obus = split_obus(&tail).unwrap();
        assert_eq!(obus, [Obu { kind: 6, bytes: 0..5 }]);
    }

    #[test]
    fn bad_temporal_units_are_errors() {
        assert!(split_obus(&[0x80]).is_err(), "forbidden bit");
        assert!(split_obus(&[6 << 3 | 0x02, 5, 1, 2]).is_err(), "too short");
        assert!(split_obus(&[6 << 3 | 0x02, 0x80]).is_err(), "size cut short");
        assert!(split_obus(&[6 << 3 | 0x04]).is_err(), "extension cut short");
        assert!(split_obus(&[]).unwrap().is_empty());
    }

    /// A small encode: every packet is one frame with the reconstruction,
    /// key frames at the interval, no temporal delimiters, and the codec
    /// configuration carries the sequence header.
    #[test]
    fn a_small_encode() {
        let settings = Settings { width: 64, height: 48, fps: 30, key_interval: 4, quantizer: 120, speed: 10 };
        let mut enc = Encoder::new(settings).unwrap();
        for i in 0..10u32 {
            let pixels: Vec<u32> = (0..64 * 48u32)
                .map(|p| {
                    let (x, y) = (p % 64, p / 64);
                    0xFF00_0000 | ((x * 4 + i * 8) & 0xFF) << 16 | ((y * 5) & 0xFF) << 8 | 0x60
                })
                .collect();
            enc.push(&crate::yuv::from_rgb(&pixels, 64, 48)).unwrap();
        }
        assert!(matches!(enc.push(&crate::yuv::from_rgb(&[0; 4], 2, 2)), Err(Error::Size { .. })));
        let video = enc.finish().unwrap();
        assert_eq!(video.packets.len(), 10);
        assert_eq!(video.md5.len(), 10);
        let keys: Vec<bool> = video.packets.iter().map(|p| p.key).collect();
        assert_eq!(keys, [true, false, false, false, true, false, false, false, true, false]);
        for p in &video.packets {
            let obus = split_obus(&p.data).unwrap();
            assert!(obus.iter().all(|o| o.kind != OBU_TEMPORAL_DELIMITER));
            assert_eq!(obus.iter().any(|o| o.kind == OBU_SEQUENCE_HEADER), p.key);
        }
        // marker 1, version 1; profile 0, level 31; 4:2:0 8-bit.
        assert_eq!(video.config[..3], [0x81, 0x1F, 0x0C]);
        let obus = split_obus(&video.config[4..]).unwrap();
        assert_eq!(obus.len(), 1);
        assert_eq!(obus[0].kind, OBU_SEQUENCE_HEADER);
        // The same every time.
        let again = {
            let mut enc = Encoder::new(settings).unwrap();
            let pixels = vec![0xFF20_4060; 64 * 48];
            for _ in 0..3 {
                enc.push(&crate::yuv::from_rgb(&pixels, 64, 48)).unwrap();
            }
            enc.finish().unwrap()
        };
        let twice = {
            let mut enc = Encoder::new(settings).unwrap();
            let pixels = vec![0xFF20_4060; 64 * 48];
            for _ in 0..3 {
                enc.push(&crate::yuv::from_rgb(&pixels, 64, 48)).unwrap();
            }
            enc.finish().unwrap()
        };
        assert_eq!(again.md5, twice.md5);
        assert_eq!(
            again.packets.iter().map(|p| &p.data).collect::<Vec<_>>(),
            twice.packets.iter().map(|p| &p.data).collect::<Vec<_>>()
        );
    }

    /// A flat picture decodes to its own value everywhere (the encoder's
    /// reconstruction is checked against the MD5 of the planes it was given).
    #[test]
    fn a_flat_picture_reconstructs_exactly() {
        let settings = Settings { width: 32, height: 32, fps: 30, key_interval: 30, quantizer: 60, speed: 10 };
        let mut enc = Encoder::new(settings).unwrap();
        enc.keep(vec![0]);
        let picture = crate::yuv::from_rgb(&vec![0xFF80_8080; 32 * 32], 32, 32);
        enc.push(&picture).unwrap();
        let video = enc.finish().unwrap();
        assert_eq!(video.kept, [(0, picture.clone())]);
        let mut md5 = Md5::new();
        md5.update(&picture.y);
        md5.update(&picture.u);
        md5.update(&picture.v);
        assert_eq!(video.md5[0], <[u8; 16]>::from(md5.finalize()));
    }
}
