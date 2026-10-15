// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Names and durations as people read them.

use alloc::format;
use alloc::string::String;

/// The text of a NUL-terminated byte string the kernel fills in (a
/// version, a processor's brand), without the spaces around it.
pub fn cstr(b: &[u8]) -> String {
    let n = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..n]).trim().into()
}

/// How long the system has been running: `3 d 4 h 05 min`, `2 h 05 min`,
/// `12 min 30 s`, `45 s`.
pub fn uptime(ns: u64) -> String {
    let s = ns / 1_000_000_000;
    let (d, h, m, s) = (s / 86_400, (s / 3600) % 24, (s / 60) % 60, s % 60);
    if d > 0 {
        format!("{d} d {h} h {m:02} min")
    } else if h > 0 {
        format!("{h} h {m:02} min")
    } else if m > 0 {
        format!("{m} min {s:02} s")
    } else {
        format!("{s} s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_uptimes() {
        assert_eq!(cstr(b" Veda 0.1 \0junk"), "Veda 0.1");
        assert_eq!(cstr(b"no end"), "no end");
        assert_eq!(cstr(b"\xffx\0"), "\u{fffd}x");
        let s = 1_000_000_000;
        assert_eq!(uptime(45 * s), "45 s");
        assert_eq!(uptime(750 * s), "12 min 30 s");
        assert_eq!(uptime((2 * 3600 + 5 * 60 + 9) * s), "2 h 05 min");
        assert_eq!(uptime((3 * 86_400 + 4 * 3600 + 5 * 60) * s), "3 d 4 h 05 min");
    }
}
