// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The kernel log as a descriptor ([`crate::fd::Object::Log`]): where the
//! output of a program without a terminal goes, such as a service's or an
//! application's that `init` started.
//!
//! The kernel makes a line of the log of every line a write holds, and of
//! what is left at its end, so the descriptor gathers whole lines first: a
//! line written in pieces (Rust's standard error, which buffers nothing,
//! writes a panic's message so) stays one line of the log. What is left of
//! a line goes when the descriptor is closed and when the program ends
//! ([`crate::fd::flush_logs`]).

use alloc::vec::Vec;

use rt::sync::Mutex;

/// The most the kernel takes in one write, and so the longest line kept
/// whole; a longer one goes in pieces.
const CHUNK: usize = 4096;

/// A log descriptor's line not yet ended.
pub struct Log {
    pending: Mutex<Vec<u8>>,
}

impl Log {
    pub fn new() -> Log {
        Log { pending: Mutex::new(Vec::new()) }
    }

    /// Writes `data`: the lines it ends now, the rest with the next write.
    pub fn write(&self, data: &[u8]) {
        let mut pending = self.pending.lock();
        pending.extend_from_slice(data);
        let ended = pending.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
        // A line too long to keep whole goes as it is.
        let ended = if pending.len() - ended >= CHUNK { pending.len() } else { ended };
        write_lines(&pending[..ended], rt::sys::debug_write);
        pending.drain(..ended);
    }

    /// Writes what is left of a line, if anything is.
    pub fn flush(&self) {
        // At the program's end, another thread may hold the lock for ever.
        if let Some(mut pending) = self.pending.try_lock() {
            write_lines(&pending, rt::sys::debug_write);
            pending.clear();
        }
    }
}

impl Drop for Log {
    fn drop(&mut self) {
        self.flush();
    }
}

/// Gives `text`, whole lines but perhaps its last, to `write` in as few
/// pieces as the kernel's limit allows, each ending where a line does
/// (but a line longer than [`CHUNK`], which goes in pieces of that size).
fn write_lines(mut text: &[u8], mut write: impl FnMut(&[u8])) {
    while !text.is_empty() {
        let len = if text.len() <= CHUNK {
            text.len()
        } else {
            text[..CHUNK].iter().rposition(|&b| b == b'\n').map_or(CHUNK, |i| i + 1)
        };
        write(&text[..len]);
        text = &text[len..];
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;
    use alloc::vec::Vec;

    use super::*;

    fn pieces(text: &[u8]) -> Vec<usize> {
        let mut lens = Vec::new();
        write_lines(text, |p| lens.push(p.len()));
        lens
    }

    #[test]
    fn lines_go_together_up_to_the_kernels_limit() {
        assert_eq!(pieces(b"one\ntwo\n"), [8]);
        assert!(pieces(b"").is_empty());
        // 3000-byte lines: one a piece, never cut.
        let mut text = vec![b'a'; 2999];
        text.push(b'\n');
        let line = text.clone();
        text.extend_from_slice(&line);
        text.extend_from_slice(&line);
        assert_eq!(pieces(&text), [3000, 3000, 3000]);
        // A line longer than the limit goes in pieces of it.
        assert_eq!(pieces(&[b'b'; CHUNK + 10]), [CHUNK, 10]);
    }
}
