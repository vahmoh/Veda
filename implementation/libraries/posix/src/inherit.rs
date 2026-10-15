// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What a POSIX program inherits from the POSIX program that starts it,
//! beyond the handles behind its descriptors: what `exec` keeps on Linux of
//! the signals (those ignored, and the mask), and the descriptors that are
//! no kernel object (a directory, the empty input, the kernel log).
//!
//! They travel as `KEY=VALUE` entries of the new program's startup message,
//! where a Veda program's environment goes (a POSIX program's environment
//! is on its stack, `vrt::process`):
//!
//! | Key | Value |
//! |-----|-------|
//! | `posix.sigign` | the signals it ignores, in hexadecimal: bit `n - 1` for signal `n` |
//! | `posix.sigmask` | its main thread's signal mask, likewise |
//! | `posix.fd.N` | descriptor `N`: `dir:PATH`, `null` or `log` |
//!
//! A program started by anything else inherits nothing of these: no signal
//! ignored or blocked, and descriptors only from handles.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// A descriptor that is no kernel object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pseudo {
    /// A directory, by its path.
    Dir(String),
    /// The empty input.
    Null,
    /// The kernel log.
    Log,
}

/// What a program inherits that its handles do not carry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Inherited {
    /// The signals it ignores.
    pub ignored: u64,
    /// Its main thread's signal mask.
    pub mask: u64,
    /// Its descriptors that are no kernel object.
    pub fds: Vec<(i32, Pseudo)>,
}

const IGNORED: &str = "posix.sigign";
const MASK: &str = "posix.sigmask";
const FD: &str = "posix.fd.";

impl Inherited {
    /// The startup message's entries that say it.
    pub fn entries(&self) -> Vec<String> {
        let mut out = Vec::with_capacity(self.fds.len() + 2);
        if self.ignored != 0 {
            out.push(format!("{IGNORED}={:x}", self.ignored));
        }
        if self.mask != 0 {
            out.push(format!("{MASK}={:x}", self.mask));
        }
        for (n, fd) in &self.fds {
            out.push(match fd {
                Pseudo::Dir(path) => format!("{FD}{n}=dir:{path}"),
                Pseudo::Null => format!("{FD}{n}=null"),
                Pseudo::Log => format!("{FD}{n}=log"),
            });
        }
        out
    }

    /// What the startup message's entries `(key, value)` say; any entry not
    /// of these, or malformed, is left out.
    pub fn parse<'a>(entries: impl IntoIterator<Item = (&'a str, &'a str)>) -> Inherited {
        let mut out = Inherited::default();
        for (key, value) in entries {
            match key {
                IGNORED => out.ignored = u64::from_str_radix(value, 16).unwrap_or(0),
                MASK => out.mask = u64::from_str_radix(value, 16).unwrap_or(0),
                _ => {
                    let Some(n) = key.strip_prefix(FD).and_then(|n| n.parse::<i32>().ok()).filter(|&n| n >= 0) else {
                        continue;
                    };
                    let fd = match value {
                        "null" => Pseudo::Null,
                        "log" => Pseudo::Log,
                        v => match v.strip_prefix("dir:") {
                            Some(path) if path.starts_with('/') => Pseudo::Dir(path.to_string()),
                            _ => continue,
                        },
                    };
                    out.fds.push((n, fd));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inheritance_round_trips() {
        let i = Inherited {
            ignored: 1 << 12,
            mask: (1 << 9) | (1 << 63),
            fds: alloc::vec![(3, Pseudo::Dir("/home/user/a=b".into())), (0, Pseudo::Null), (2, Pseudo::Log),],
        };
        let entries = i.entries();
        assert_eq!(entries[0], "posix.sigign=1000");
        let pairs: Vec<(&str, &str)> = entries.iter().map(|e| e.split_once('=').unwrap()).collect();
        assert_eq!(Inherited::parse(pairs), i);
        // Nothing to say, nothing said.
        assert!(Inherited::default().entries().is_empty());
    }

    #[test]
    fn malformed_entries_are_left_out() {
        let parsed = Inherited::parse([
            ("posix.sigign", "zz"),
            ("posix.sigmask", "10000000000000000"),
            ("posix.fd.-1", "null"),
            ("posix.fd.x", "null"),
            ("posix.fd.4", "pipe"),
            ("posix.fd.5", "dir:relative"),
            ("posix.fd.6", "log"),
            ("PATH", "/system/bin"),
        ]);
        assert_eq!(parsed, Inherited { ignored: 0, mask: 0, fds: alloc::vec![(6, Pseudo::Log)] });
    }
}
