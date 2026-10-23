// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The byte source FFmpeg reads: the file the player opened and sent, a
//! channel speaking the VFS's `file` protocol (`read_at`, `stat`). The
//! decoder has no other way to any file.
//!
//! FFmpeg calls [`read`], [`seek`] and [`interrupted`] (through the shim's
//! `struct vmd_io`) while it opens and demuxes, always on the decoder's
//! one thread.

use core::ffi::c_void;

use abi::{RawHandle, WaitItem, signals};
use proto::fs::{MAX_IO, file};
use rt::object::Channel;

/// How long one read may take before the source counts as failed.
const READ_TIMEOUT_NS: u64 = 10_000_000_000;

/// `whence` asking for the size (`VMD_SEEK_SIZE`).
pub const SEEK_SIZE: i32 = 0x10000;

/// A source being read.
pub struct Source {
    file: file::Client,
    position: u64,
    size: Option<u64>,
    /// The control channel, to notice the player going away.
    control: RawHandle,
}

impl Source {
    pub fn new(file: Channel, control: RawHandle) -> Source {
        let file = file::Client::new(file);
        file.set_timeout(READ_TIMEOUT_NS);
        let size = file.stat().ok().and_then(|r| r.ok()).map(|s| s.size);
        Source { file, position: 0, size, control }
    }

    /// The source's size, if known.
    pub fn size(&self) -> Option<u64> {
        self.size
    }

    /// Reads at the position into `buf`: how many bytes (0 at the end), or
    /// `None` if the source failed.
    pub fn read(&mut self, buf: &mut [u8]) -> Option<usize> {
        let len = buf.len().min(MAX_IO as usize) as u32;
        if len == 0 {
            return Some(0);
        }
        let bytes = self.file.read_at(self.position, len).ok()?.ok()?;
        let n = bytes.0.len().min(buf.len());
        buf[..n].copy_from_slice(&bytes.0[..n]);
        self.position += n as u64;
        Some(n)
    }

    /// Moves as `lseek` does (`whence` 0, 1 or 2), or answers the size
    /// ([`SEEK_SIZE`]).
    pub fn seek(&mut self, offset: i64, whence: i32) -> Option<u64> {
        if whence == SEEK_SIZE {
            return self.size;
        }
        let to = resolve(self.position, self.size, offset, whence)?;
        self.position = to;
        Some(to)
    }

    /// Whether the player went away (its control channel closed).
    pub fn interrupted(&self) -> bool {
        let mut item = [WaitItem { handle: self.control, signals: signals::PEER_CLOSED, ..Default::default() }];
        rt::object::wait_many(&mut item, 0).is_ok() && item[0].observed & signals::PEER_CLOSED != 0
    }
}

/// Where a seek lands: `offset` from the start (`whence` 0), the
/// `position` (1) or the end of a file of `size` (2). `None` if that is
/// before the start, or the size is needed and not known.
pub fn resolve(position: u64, size: Option<u64>, offset: i64, whence: i32) -> Option<u64> {
    let from = match whence {
        0 => 0,
        1 => position,
        2 => size?,
        _ => return None,
    };
    from.checked_add_signed(offset)
}

/// FFmpeg's `read` callback: bytes read, 0 at the end, -1 failed.
///
/// # Safety
/// `opaque` is a live `Source`; `buf` holds `size` bytes.
pub unsafe extern "C" fn read(opaque: *mut c_void, buf: *mut u8, size: i32) -> i32 {
    if size <= 0 {
        return 0;
    }
    // SAFETY: as the caller promises; FFmpeg calls back on this thread only,
    // while no other reference to the source is in use.
    let (source, buf) = unsafe { (&mut *(opaque as *mut Source), core::slice::from_raw_parts_mut(buf, size as usize)) };
    source.read(buf).map_or(-1, |n| n as i32)
}

/// FFmpeg's `seek` callback: the new position (or the size), -1 failed.
///
/// # Safety
/// `opaque` is a live `Source`.
pub unsafe extern "C" fn seek(opaque: *mut c_void, offset: i64, whence: i32) -> i64 {
    // SAFETY: as for `read`.
    let source = unsafe { &mut *(opaque as *mut Source) };
    source.seek(offset, whence).map_or(-1, |p| p.min(i64::MAX as u64) as i64)
}

/// FFmpeg's interrupt callback: 1 if the player went away.
///
/// # Safety
/// `opaque` is a live `Source`.
pub unsafe extern "C" fn interrupted(opaque: *mut c_void) -> i32 {
    // SAFETY: as for `read`.
    let source = unsafe { &*(opaque as *const Source) };
    source.interrupted() as i32
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    #[test]
    fn seeks_land_where_lseek_would() {
        assert_eq!(resolve(10, Some(100), 5, 0), Some(5));
        assert_eq!(resolve(10, Some(100), 5, 1), Some(15));
        assert_eq!(resolve(10, Some(100), -5, 1), Some(5));
        assert_eq!(resolve(10, Some(100), -1, 2), Some(99));
        assert_eq!(resolve(10, None, -1, 2), None, "the end of a source of no known size");
        assert_eq!(resolve(10, Some(100), -11, 1), None, "before the start");
        assert_eq!(resolve(u64::MAX, Some(100), 1, 1), None, "past the end of numbers");
        assert_eq!(resolve(0, Some(100), 0, 7), None);
    }
}
