// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Text output to the system log (`print!`, `println!`, `log!`).

use alloc::string::String;
use core::fmt::{self, Write};

use crate::sys;

/// Writes raw text to the kernel log (tagged with the process name).
pub fn write_str(s: &str) {
    sys::debug_write(s.as_bytes());
}

struct LineBuffer(String);

impl Write for LineBuffer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.0.push_str(s);
        Ok(())
    }
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    let mut buf = LineBuffer(String::new());
    let _ = buf.write_fmt(args);
    write_str(&buf.0);
}

/// The most bytes [`log_without_allocating`] writes of a message.
pub const UNALLOCATED_MESSAGE_MAX: usize = 512;

/// Text formatted into a buffer of its own, cut short where it is full.
struct FixedBuffer {
    data: [u8; UNALLOCATED_MESSAGE_MAX],
    len: usize,
}

impl FixedBuffer {
    const fn new() -> FixedBuffer {
        FixedBuffer { data: [0; UNALLOCATED_MESSAGE_MAX], len: 0 }
    }

    fn bytes(&self) -> &[u8] {
        &self.data[..self.len]
    }
}

impl Write for FixedBuffer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let n = s.len().min(self.data.len() - self.len);
        self.data[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
        self.len += n;
        Ok(())
    }
}

/// Formats `args` without allocating and writes the result (its first
/// [`UNALLOCATED_MESSAGE_MAX`] bytes) to the kernel log: for panic handlers
/// and the allocator's own errors, when the heap may be what failed.
pub fn log_without_allocating(args: fmt::Arguments) {
    let mut b = FixedBuffer::new();
    let _ = b.write_fmt(args);
    sys::debug_write(b.bytes());
}

/// Prints to the system log.
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => { $crate::io::_print(format_args!($($arg)*)) };
}

/// Prints a line to the system log.
#[macro_export]
macro_rules! println {
    () => { $crate::io::_print(format_args!("\n")) };
    ($($arg:tt)*) => { $crate::io::_print(format_args!("{}\n", format_args!($($arg)*))) };
}

/// Alias of `println!` for diagnostics.
#[macro_export]
macro_rules! eprintln {
    ($($arg:tt)*) => { $crate::println!($($arg)*) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_buffers_cut_long_text() {
        let mut b = FixedBuffer::new();
        let what = "at the disco";
        write!(b, "panic: {what}").unwrap();
        assert_eq!(b.bytes(), b"panic: at the disco");
        let long = "x".repeat(UNALLOCATED_MESSAGE_MAX);
        write!(b, "{long}{long}").unwrap();
        assert_eq!(b.bytes().len(), UNALLOCATED_MESSAGE_MAX);
        assert!(b.bytes().starts_with(b"panic: at the disco") && b.bytes().ends_with(b"x"));
        // Full: more is dropped, not an error.
        assert!(write!(b, "more").is_ok());
        assert_eq!(b.bytes().len(), UNALLOCATED_MESSAGE_MAX);
    }
}
