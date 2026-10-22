// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `shell-common` — what the shell's programs share.
//!
//! The desktop shell (`implementation/shell/components/desktop`) and the
//! lock screen (`implementation/shell/components/lock`) are separate
//! processes, trusted by the window system for different things, but they
//! show the same picture and the same figure:
//!
//! * [`wallpaper`]: the wallpaper the user chose, at the screen's size, and
//!   its heavily blurred copy; the procedural one when there is none; a
//!   [`wallpaper::Loader`] that decodes on a thread of its own.
//! * [`presence`]: OS1's ring and coil, from "Her": the voice agent's
//!   figure on the desktop, and the ring around the user on the lock
//!   screen.
//!
//! The pure parts (the figures' shapes, the procedural picture, the blur)
//! are tested on the host.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod presence;
pub mod wallpaper;

use alloc::vec;
use alloc::vec::Vec;

use proto::vfs;

/// Reads a whole file of at most `max` bytes (`None` for a larger one,
/// whatever wrote it, or one that cannot be read).
pub fn read_file(vfs: &vfs::Client, path: &str, max: u64) -> Option<Vec<u8>> {
    let (vmo, len) = vfs.read_file(path.into()).ok()?.ok()?;
    if len > max {
        return None;
    }
    let mut buf = vec![0u8; len as usize];
    vmo.read(0, &mut buf).ok()?;
    Some(buf)
}
