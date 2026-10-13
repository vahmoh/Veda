// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The I/O port space, which drivers reach through I/O port objects.

use super::port;

/// Reads `width` bytes (1, 2 or 4) from `port`; `None` for another width.
///
/// # Safety
/// Port I/O can have arbitrary side effects on devices.
pub unsafe fn read(port: u16, width: usize) -> Option<u32> {
    // SAFETY: forwarded to the caller.
    unsafe {
        match width {
            1 => Some(port::inb(port) as u32),
            2 => Some(port::inw(port) as u32),
            4 => Some(port::inl(port)),
            _ => None,
        }
    }
}

/// Writes the low `width` bytes (1, 2 or 4) of `value` to `port`; false for
/// another width.
///
/// # Safety
/// See [`read`].
pub unsafe fn write(port: u16, width: usize, value: u32) -> bool {
    // SAFETY: forwarded to the caller.
    unsafe {
        match width {
            1 => port::outb(port, value as u8),
            2 => port::outw(port, value as u16),
            4 => port::outl(port, value),
            _ => return false,
        }
    }
    true
}
