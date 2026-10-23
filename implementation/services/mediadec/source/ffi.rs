// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The shim's interface (`shim/vmd.h`), as Rust sees it. Integers and
//! opaque pointers only: this half is built without floating point, which
//! it would pass otherwise than GCC. The structures' sizes are checked on
//! both sides (here, and `_Static_assert` in `shim/main.c`).

use core::ffi::{c_char, c_void};

pub const OK: i32 = 0;
pub const AGAIN: i32 = 1;
pub const END: i32 = 2;
pub const EEXIT: i32 = -10;
/// A time not known.
pub const NO_TIME: i64 = i64::MIN;

pub const KIND_VIDEO: u32 = 1;
pub const KIND_AUDIO: u32 = 2;
pub const KIND_SUBTITLE: u32 = 3;

pub const DECODE_BITEXACT: u32 = 1;
/// `VMD_DECODE_SKIP(level)`: what a decoder leaves out from the start.
pub const fn decode_skip(level: u32) -> u32 {
    (level & 3) << 8
}
pub const PICTURE_BITEXACT: u32 = 1;
pub const PICTURE_FAST: u32 = 2;

/// How FFmpeg reads (`struct vmd_io`).
#[repr(C)]
pub struct Io {
    pub opaque: *mut c_void,
    pub read: unsafe extern "C" fn(*mut c_void, *mut u8, i32) -> i32,
    pub seek: unsafe extern "C" fn(*mut c_void, i64, i32) -> i64,
    pub interrupted: unsafe extern "C" fn(*mut c_void) -> i32,
}

/// `struct vmd_info`.
#[repr(C)]
pub struct Info {
    pub duration_us: i64,
    pub start_us: i64,
    pub bit_rate: u64,
    pub streams: u32,
    pub chapters: u32,
    pub seekable: u32,
    pub container: [u8; 32],
    pub title: [u8; 256],
    pub artist: [u8; 256],
}

/// `struct vmd_stream`.
#[repr(C)]
pub struct Stream {
    pub index: u32,
    pub kind: u32,
    pub codec: [u8; 32],
    pub language: [u8; 16],
    pub title: [u8; 256],
    pub width: u32,
    pub height: u32,
    pub sar_num: u32,
    pub sar_den: u32,
    pub fps_num: u32,
    pub fps_den: u32,
    pub rotation: u32,
    pub mirrored: u32,
    pub bit_depth: u32,
    pub primaries: u32,
    pub transfer: u32,
    pub matrix: u32,
    pub full_range: u32,
    pub rate: u32,
    pub channels: u32,
    pub decodable: u32,
    pub is_default: u32,
    pub cover: u32,
}

/// `struct vmd_packet_info`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct PacketInfo {
    pub stream: u32,
    pub size: u32,
    pub pts_us: i64,
    pub dts_us: i64,
    pub duration_us: i64,
    pub key: u32,
}

/// `struct vmd_frame_info`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct FrameInfo {
    pub pts_us: i64,
    pub duration_us: i64,
    pub width: u32,
    pub height: u32,
    pub key: u32,
    pub corrupt: u32,
    pub samples: u32,
    pub rate: u32,
    pub channels: u32,
}

const _: () = assert!(size_of::<Io>() == 32);
const _: () = assert!(size_of::<Info>() == 584);
const _: () = assert!(size_of::<Stream>() == 384);
const _: () = assert!(size_of::<PacketInfo>() == 40);
const _: () = assert!(size_of::<FrameInfo>() == 48);

impl Info {
    pub const fn zeroed() -> Info {
        Info {
            duration_us: 0,
            start_us: 0,
            bit_rate: 0,
            streams: 0,
            chapters: 0,
            seekable: 0,
            container: [0; 32],
            title: [0; 256],
            artist: [0; 256],
        }
    }
}

impl Stream {
    pub const fn zeroed() -> Stream {
        Stream {
            index: 0,
            kind: 0,
            codec: [0; 32],
            language: [0; 16],
            title: [0; 256],
            width: 0,
            height: 0,
            sar_num: 0,
            sar_den: 0,
            fps_num: 0,
            fps_den: 0,
            rotation: 0,
            mirrored: 0,
            bit_depth: 0,
            primaries: 0,
            transfer: 0,
            matrix: 0,
            full_range: 0,
            rate: 0,
            channels: 0,
            decodable: 0,
            is_default: 0,
            cover: 0,
        }
    }
}

/// The shim's objects, only ever behind pointers.
#[repr(C)]
pub struct Source {
    _private: [u8; 0],
}
#[repr(C)]
pub struct Packet {
    _private: [u8; 0],
}
#[repr(C)]
pub struct Decoder {
    _private: [u8; 0],
}
#[repr(C)]
pub struct Frame {
    _private: [u8; 0],
}
#[repr(C)]
pub struct Scaler {
    _private: [u8; 0],
}
#[repr(C)]
pub struct Resampler {
    _private: [u8; 0],
}

unsafe extern "C" {
    pub fn vmd_setup(max_alloc: u64);
    pub fn vmd_build() -> *const c_char;

    pub fn vmd_open(io: *const Io, name: *const c_char, max_width: u32, max_height: u32, out: *mut *mut Source) -> i32;
    pub fn vmd_close(src: *mut Source);
    pub fn vmd_info(src: *mut Source, out: *mut Info) -> i32;
    pub fn vmd_stream(src: *mut Source, index: u32, out: *mut Stream) -> i32;
    pub fn vmd_chapter(src: *mut Source, index: u32, start_us: *mut i64, title: *mut u8) -> i32;
    pub fn vmd_best(src: *mut Source, kind: u32) -> i32;
    pub fn vmd_use(src: *mut Source, video: i32, audio: i32) -> i32;
    pub fn vmd_read(src: *mut Source, out: *mut *mut Packet, info: *mut PacketInfo) -> i32;
    pub fn vmd_packet_free(packet: *mut Packet);
    pub fn vmd_seek(src: *mut Source, target_us: i64) -> i32;

    pub fn vmd_decoder_open(src: *mut Source, stream: u32, threads: u32, flags: u32, out: *mut *mut Decoder) -> i32;
    pub fn vmd_decoder_close(dec: *mut Decoder);
    pub fn vmd_send(dec: *mut Decoder, packet: *const Packet) -> i32;
    pub fn vmd_receive(dec: *mut Decoder, out: *mut *mut Frame, info: *mut FrameInfo) -> i32;
    pub fn vmd_frame_free(frame: *mut Frame);
    pub fn vmd_flush(dec: *mut Decoder);
    pub fn vmd_skip(dec: *mut Decoder, level: u32) -> i32;

    pub fn vmd_picture(
        scaler: *mut *mut Scaler,
        frame: *const Frame,
        dst: *mut u32,
        stride: u32,
        width: u32,
        height: u32,
        flags: u32,
    ) -> i32;
    pub fn vmd_scaler_free(scaler: *mut Scaler);
    #[allow(clippy::too_many_arguments)]
    pub fn vmd_sound(
        resampler: *mut *mut Resampler,
        frame: *const Frame,
        frame_pts_us: i64,
        rate: u32,
        dst: *mut i16,
        capacity: u32,
        frames: *mut u32,
        pts_us: *mut i64,
    ) -> i32;
    pub fn vmd_resampler_free(resampler: *mut Resampler);
    pub fn vmd_frame_md5(frame: *const Frame, md5: *mut u8) -> i32;
}

/// A shim error as the protocol's (`VMD_E*` are its codes, negated).
pub fn error(code: i32) -> proto::media::MediaError {
    proto::media::MediaError::from_code(code.unsigned_abs())
}

/// The text of a C string array (up to its first 0), made valid UTF-8.
pub fn text(bytes: &[u8]) -> alloc::string::String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    alloc::string::String::from_utf8_lossy(&bytes[..end]).into_owned()
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    #[test]
    fn texts_and_errors() {
        assert_eq!(text(b"av1\0junk"), "av1");
        assert_eq!(text(b"no terminator"), "no terminator");
        assert_eq!(text(b"bad \xff byte\0"), "bad \u{fffd} byte");
        assert_eq!(error(-4), proto::media::MediaError::Corrupt);
        assert_eq!(error(-10), proto::media::MediaError::Internal);
    }
}
