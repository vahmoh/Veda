// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Data as the shell's commands pass it on: bytes, exactly as they are.
//!
//! Commands read files a chunk at a time and write bytes: what goes into a
//! file or a pipe is what came out of the file (`cat a > b` copies `a`
//! whatever it holds). Only what is shown on the terminal is text: invalid
//! UTF-8 shown as replacement characters, a character split between two
//! chunks put back together ([`Utf8Stream`]), binary data recognised by
//! the NUL bytes near its start ([`is_binary`]) rather than shown. Lines
//! ([`LineSplitter`]) and counts ([`Counts`]) are taken from the bytes as
//! they arrive, so that a command never needs the whole of a file at once,
//! and what a pipe holds is bounded ([`Capture`]).

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;

/// How much of a file's start [`is_binary`] looks at.
pub const BINARY_PROBE: usize = 8192;

/// Whether data whose start is `head` is binary rather than text: a NUL
/// byte in its first [`BINARY_PROBE`] bytes.
pub fn is_binary(head: &[u8]) -> bool {
    head[..head.len().min(BINARY_PROBE)].contains(&0)
}

/// Turns bytes arriving in chunks into text for the screen: invalid UTF-8
/// becomes U+FFFD, and a character cut at the end of a chunk is kept for
/// the next.
#[derive(Debug, Default)]
pub struct Utf8Stream {
    carry: Vec<u8>,
}

impl Utf8Stream {
    pub fn new() -> Utf8Stream {
        Utf8Stream::default()
    }

    /// The text of `bytes` (after what was kept of the chunk before), up to
    /// a character cut at its end.
    pub fn push(&mut self, bytes: &[u8]) -> String {
        self.carry.extend_from_slice(bytes);
        let mut out = String::new();
        let mut rest: &[u8] = &self.carry;
        loop {
            match core::str::from_utf8(rest) {
                Ok(s) => {
                    out.push_str(s);
                    rest = &[];
                    break;
                }
                Err(e) => {
                    let (valid, after) = rest.split_at(e.valid_up_to());
                    out.push_str(core::str::from_utf8(valid).unwrap_or_default());
                    match e.error_len() {
                        // Cut at the end: wait for the rest of it.
                        None => {
                            rest = after;
                            break;
                        }
                        Some(n) => {
                            out.push(char::REPLACEMENT_CHARACTER);
                            rest = &after[n..];
                        }
                    }
                }
            }
        }
        let keep = rest.len();
        let start = self.carry.len() - keep;
        self.carry.drain(..start);
        out
    }

    /// What is left at the end of the stream (a character never finished).
    pub fn finish(&mut self) -> String {
        let s = String::from_utf8_lossy(&self.carry).into_owned();
        self.carry.clear();
        s
    }
}

/// The longest line [`LineSplitter`] keeps whole: a longer one (a binary
/// file, minified data) is passed on in pieces of this size.
pub const MAX_LINE: usize = 1 << 20;

/// Splits bytes arriving in chunks into lines.
#[derive(Debug, Default)]
pub struct LineSplitter {
    carry: Vec<u8>,
}

impl LineSplitter {
    pub fn new() -> LineSplitter {
        LineSplitter::default()
    }

    /// Passes each line completed by `chunk` to `f`, with its `\n` if it
    /// has one, until `f` returns `false`; returns whether it never did.
    pub fn feed(&mut self, chunk: &[u8], f: &mut impl FnMut(&[u8]) -> bool) -> bool {
        let mut rest = chunk;
        while let Some(nl) = rest.iter().position(|&b| b == b'\n') {
            let (line, after) = rest.split_at(nl + 1);
            rest = after;
            let go = if self.carry.is_empty() {
                f(line)
            } else {
                self.carry.extend_from_slice(line);
                let whole = core::mem::take(&mut self.carry);
                f(&whole)
            };
            if !go {
                return false;
            }
        }
        self.carry.extend_from_slice(rest);
        while self.carry.len() >= MAX_LINE {
            let piece: Vec<u8> = self.carry.drain(..MAX_LINE).collect();
            if !f(&piece) {
                return false;
            }
        }
        true
    }

    /// Passes the last line, if it has no `\n`, to `f`.
    pub fn finish(&mut self, f: &mut impl FnMut(&[u8]) -> bool) -> bool {
        if self.carry.is_empty() {
            return true;
        }
        let last = core::mem::take(&mut self.carry);
        f(&last)
    }
}

/// A line without its line break (`\n` or `\r\n`).
pub fn trim_line_end(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

/// The last lines of a stream, as they arrive.
#[derive(Debug)]
pub struct LastLines {
    lines: VecDeque<Vec<u8>>,
    n: usize,
}

impl LastLines {
    /// Keeps the last `n` lines.
    pub fn new(n: usize) -> LastLines {
        LastLines { lines: VecDeque::new(), n }
    }

    pub fn push(&mut self, line: &[u8]) {
        if self.n == 0 {
            return;
        }
        if self.lines.len() == self.n {
            self.lines.pop_front();
        }
        self.lines.push_back(line.into());
    }

    pub fn lines(&self) -> impl Iterator<Item = &[u8]> {
        self.lines.iter().map(|l| l.as_slice())
    }
}

/// Lines, words and bytes, counted as the bytes arrive (as `wc` shows
/// them: a last line without a line break counts as a line).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub lines: u64,
    pub words: u64,
    pub bytes: u64,
    in_word: bool,
    /// The stream so far ends inside a line.
    open_line: bool,
}

impl Counts {
    pub fn add(&mut self, chunk: &[u8]) {
        for &b in chunk {
            if b == b'\n' {
                self.lines += 1;
            }
            let space = b.is_ascii_whitespace() || b == 0x0B;
            if !space && !self.in_word {
                self.words += 1;
            }
            self.in_word = !space;
        }
        if let Some(&last) = chunk.last() {
            self.open_line = last != b'\n';
        }
        self.bytes += chunk.len() as u64;
    }

    /// The counts of the whole stream.
    pub fn total(&self) -> (u64, u64, u64) {
        (self.lines + u64::from(self.open_line), self.words, self.bytes)
    }
}

/// What a pipe holds between two commands: at most `limit` bytes. What
/// comes after is left out, and [`Capture::cut`] says so.
#[derive(Debug)]
pub struct Capture {
    bytes: Vec<u8>,
    limit: usize,
    cut: bool,
}

impl Capture {
    pub fn new(limit: usize) -> Capture {
        Capture { bytes: Vec::new(), limit, cut: false }
    }

    /// Adds `b`; `false` once the capture is full (and something was left
    /// out).
    pub fn push(&mut self, b: &[u8]) -> bool {
        let room = self.limit - self.bytes.len();
        if b.len() > room {
            self.bytes.extend_from_slice(&b[..room]);
            self.cut = true;
            return false;
        }
        self.bytes.extend_from_slice(b);
        true
    }

    /// Something was left out.
    pub fn cut(&self) -> bool {
        self.cut
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn lines_of(chunks: &[&[u8]]) -> Vec<Vec<u8>> {
        let mut s = LineSplitter::new();
        let mut out = Vec::new();
        let mut take = |l: &[u8]| {
            out.push(l.to_vec());
            true
        };
        for c in chunks {
            s.feed(c, &mut take);
        }
        s.finish(&mut take);
        out
    }

    #[test]
    fn text_from_chunks() {
        let mut u = Utf8Stream::new();
        let bytes = "h\u{e9}\u{4e2d}".as_bytes();
        // Characters cut between chunks come out whole.
        assert_eq!(u.push(&bytes[..2]), "h");
        assert_eq!(u.push(&bytes[2..4]), "\u{e9}");
        assert_eq!(u.push(&bytes[4..]), "\u{4e2d}");
        // Invalid bytes are replaced, not dropped.
        assert_eq!(u.push(b"a\xffb"), "a\u{fffd}b");
        assert_eq!(u.push(b"\xe4\xb8"), "");
        assert_eq!(u.finish(), "\u{fffd}");
    }

    #[test]
    fn lines() {
        assert_eq!(
            lines_of(&[b"ab", b"c\nd", b"\n\ne"]),
            vec![b"abc\n".to_vec(), b"d\n".to_vec(), b"\n".to_vec(), b"e".to_vec()]
        );
        assert_eq!(lines_of(&[b"x\r\n"]), vec![b"x\r\n".to_vec()]);
        assert_eq!(trim_line_end(b"x\r\n"), b"x");
        // A line without end is passed on in pieces.
        let long = vec![b'a'; MAX_LINE + 10];
        let got = lines_of(&[&long]);
        assert_eq!(got.iter().map(Vec::len).collect::<Vec<_>>(), [MAX_LINE, 10]);
        // Stopping early.
        let mut s = LineSplitter::new();
        let mut n = 0;
        assert!(!s.feed(b"1\n2\n3\n", &mut |_| {
            n += 1;
            n < 2
        }));
        assert_eq!(n, 2);
    }

    #[test]
    fn counting() {
        let mut c = Counts::default();
        c.add(b"one two\nthr");
        c.add(b"ee\n four");
        assert_eq!(c.total(), (3, 4, 19));
        let mut c = Counts::default();
        c.add(b"\xff\xfe x\n");
        assert_eq!(c.total(), (1, 2, 5));
        assert_eq!(Counts::default().total(), (0, 0, 0));
    }

    #[test]
    fn binary_and_bounds() {
        assert!(!is_binary(b"plain text\n"));
        assert!(is_binary(b"\x7fELF\x02\x01\x01\x00"));
        let mut late = vec![b'a'; BINARY_PROBE];
        late.push(0);
        assert!(!is_binary(&late));
        let mut cap = Capture::new(5);
        assert!(cap.push(b"abc"));
        assert!(!cap.cut());
        assert!(!cap.push(b"defg"));
        assert!(cap.cut());
        assert!(!cap.push(b"h"));
        assert_eq!(cap.into_bytes(), b"abcde");
        let mut last = LastLines::new(2);
        for l in [&b"1\n"[..], b"2\n", b"3"] {
            last.push(l);
        }
        assert_eq!(last.lines().collect::<Vec<_>>(), [&b"2\n"[..], b"3"]);
    }
}
