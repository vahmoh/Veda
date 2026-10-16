// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What a POSIX program inherits from the POSIX program that starts it,
//! beyond the handles behind its descriptors: what `exec` keeps on Linux of
//! the signals (those ignored, and the mask), the descriptors that are no
//! kernel object (a directory, the empty input, the kernel log), and what
//! makes a handle a socket (a Unix domain socket's names, peer and options,
//! a listening Internet socket's address: `crate::carry`).
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
//! | `posix.socket.N` | descriptor `N` is a socket, carried by the handles of roles `FD + N` and (a network connection's channel) `FD_EXTRA + N`, as `crate::carry` says, in hexadecimal |
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
    /// Its descriptors that are sockets: what makes their handles sockets.
    pub sockets: Vec<(i32, Vec<u8>)>,
}

const IGNORED: &str = "posix.sigign";
const MASK: &str = "posix.sigmask";
const FD: &str = "posix.fd.";
const SOCKET: &str = "posix.socket.";

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

/// A descriptor number of an entry's key, after its prefix.
fn fd_number(key: &str, prefix: &str) -> Option<i32> {
    key.strip_prefix(prefix).and_then(|n| n.parse::<i32>().ok()).filter(|&n| n >= 0)
}

impl Inherited {
    /// The startup message's entries that say it.
    pub fn entries(&self) -> Vec<String> {
        let mut out = Vec::with_capacity(self.fds.len() + self.sockets.len() + 2);
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
        for (n, socket) in &self.sockets {
            out.push(format!("{SOCKET}{n}={}", hex(socket)));
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
                _ if key.starts_with(SOCKET) => {
                    let (Some(n), Some(socket)) = (fd_number(key, SOCKET), unhex(value)) else { continue };
                    out.sockets.push((n, socket));
                }
                _ => {
                    let Some(n) = fd_number(key, FD) else {
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
            sockets: alloc::vec![(4, b"/tmp/s:1=".to_vec()), (5, alloc::vec![]), (6, alloc::vec![0xff, 0])],
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
            ("posix.socket.7", "zz"),
            ("posix.socket.8", "abc"),
            ("posix.socket.-9", "00"),
            ("posix.socket.x", "00"),
        ]);
        assert_eq!(parsed, Inherited { fds: alloc::vec![(6, Pseudo::Log)], ..Default::default() });
    }
}
