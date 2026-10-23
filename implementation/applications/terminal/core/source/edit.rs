// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The line being typed: at the shell's prompt, and for a program that
//! reads the terminal a line at a time. Both are edited the same way, with
//! the keys of Unix shells (Ctrl+A, Ctrl+E, Ctrl+U, Ctrl+K, Ctrl+W, and
//! Ctrl with the arrows, Backspace and Delete for words).

use alloc::string::String;

/// The longest line (bytes): what is typed or pasted beyond it is left
/// out, so that a line cannot grow without bound.
pub const MAX_TEXT: usize = 64 * 1024;

/// An edit of the line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edit<'a> {
    /// Types text at the cursor (control characters are left out, and
    /// what would make the line longer than [`MAX_TEXT`]).
    Insert(&'a str),
    Left,
    Right,
    /// To the start of the word before the cursor (Ctrl+Left).
    WordLeft,
    /// To the end of the word after the cursor (Ctrl+Right).
    WordRight,
    Home,
    End,
    /// Deletes the character before the cursor (Backspace).
    Backspace,
    /// Deletes the word before the cursor (Ctrl+Backspace, Ctrl+W).
    BackspaceWord,
    /// Deletes the character after the cursor (Delete).
    Delete,
    /// Deletes the word after the cursor (Ctrl+Delete).
    DeleteWord,
    /// Deletes everything before the cursor (Ctrl+U).
    KillToStart,
    /// Deletes everything after the cursor (Ctrl+K).
    KillToEnd,
}

/// A line of text and a cursor in it (a byte offset at a character
/// boundary).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineEditor {
    text: String,
    cursor: usize,
}

impl LineEditor {
    pub fn new() -> LineEditor {
        LineEditor::default()
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// The cursor (a byte offset).
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// The characters before the cursor (its column).
    pub fn cursor_chars(&self) -> usize {
        self.text[..self.cursor].chars().count()
    }

    /// Puts the cursor before character `n` (at the end past it).
    pub fn set_cursor_chars(&mut self, n: usize) {
        self.cursor = self.text.char_indices().nth(n).map_or(self.text.len(), |(i, _)| i);
    }

    /// Replaces the line, the cursor at its end. What could not be typed
    /// is left out, as [`Edit::Insert`] does.
    pub fn set(&mut self, text: &str) {
        self.clear();
        self.apply(Edit::Insert(text));
    }

    /// Takes the line, leaving it empty.
    pub fn take(&mut self) -> String {
        self.cursor = 0;
        core::mem::take(&mut self.text)
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    /// Replaces the bytes from `start` to the cursor with `with` (Tab
    /// completion), the cursor after it, unless the line would be longer
    /// than [`MAX_TEXT`]; returns whether it did. `start` is clamped to a
    /// character boundary at or before the cursor.
    pub fn replace_before_cursor(&mut self, start: usize, with: &str) -> bool {
        let mut start = start.min(self.cursor);
        while !self.text.is_char_boundary(start) {
            start -= 1;
        }
        if self.text.len() - (self.cursor - start) + with.len() > MAX_TEXT {
            return false;
        }
        self.text.replace_range(start..self.cursor, with);
        self.cursor = start + with.len();
        true
    }

    fn prev_char(&self, i: usize) -> usize {
        self.text[..i].char_indices().next_back().map_or(0, |(p, _)| p)
    }

    fn next_char(&self, i: usize) -> usize {
        self.text[i..].chars().next().map_or(i, |c| i + c.len_utf8())
    }

    /// The start of the word before `i`: back over spaces and `/`, then
    /// over a word, which a space or a `/` ends (so that a path is taken a
    /// folder at a time).
    fn word_left(&self, mut i: usize) -> usize {
        let sep = |c: char| c == ' ' || c == '/';
        while i > 0 && self.text[..i].ends_with(sep) {
            i = self.prev_char(i);
        }
        while i > 0 && !self.text[..i].ends_with(sep) {
            i = self.prev_char(i);
        }
        i
    }

    /// The end of the word after `i`: on over spaces and `/`, then over a
    /// word, and past a `/` that ends it.
    fn word_right(&self, mut i: usize) -> usize {
        let sep = |c: char| c == ' ' || c == '/';
        while i < self.text.len() && self.text[i..].starts_with(sep) {
            i = self.next_char(i);
        }
        while i < self.text.len() && !self.text[i..].starts_with(sep) {
            i = self.next_char(i);
        }
        if self.text[i..].starts_with('/') {
            i += 1;
        }
        i
    }

    /// Applies an edit; returns whether the text changed.
    pub fn apply(&mut self, e: Edit) -> bool {
        let c = self.cursor;
        match e {
            Edit::Insert(s) => {
                let room = MAX_TEXT.saturating_sub(self.text.len());
                let mut clean = String::new();
                for ch in s.chars().filter(|c| !c.is_control()) {
                    if clean.len() + ch.len_utf8() > room {
                        break;
                    }
                    clean.push(ch);
                }
                self.text.insert_str(c, &clean);
                self.cursor += clean.len();
                return !clean.is_empty();
            }
            Edit::Left => self.cursor = self.prev_char(c),
            Edit::Right => self.cursor = self.next_char(c),
            Edit::WordLeft => self.cursor = self.word_left(c),
            Edit::WordRight => self.cursor = self.word_right(c),
            Edit::Home => self.cursor = 0,
            Edit::End => self.cursor = self.text.len(),
            Edit::Backspace => return self.delete(self.prev_char(c)..c),
            Edit::BackspaceWord => return self.delete(self.word_left(c)..c),
            Edit::Delete => return self.delete(c..self.next_char(c)),
            Edit::DeleteWord => return self.delete(c..self.word_right(c)),
            Edit::KillToStart => return self.delete(0..c),
            Edit::KillToEnd => return self.delete(c..self.text.len()),
        }
        false
    }

    /// Deletes `range` (around the cursor), the cursor at its start.
    fn delete(&mut self, range: core::ops::Range<usize>) -> bool {
        if range.is_empty() {
            return false;
        }
        self.cursor = range.start;
        self.text.replace_range(range, "");
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    fn at(text: &str, cursor: usize) -> LineEditor {
        LineEditor { text: text.to_string(), cursor }
    }

    #[test]
    fn moving() {
        let mut e = at("cd /home/user/Documents", 23);
        e.apply(Edit::WordLeft);
        assert_eq!(e.cursor(), 14);
        e.apply(Edit::WordLeft);
        assert_eq!(e.cursor(), 9);
        e.apply(Edit::WordRight);
        assert_eq!(e.cursor(), 14);
        e.apply(Edit::Home);
        e.apply(Edit::WordRight);
        assert_eq!(e.cursor(), 2);
        let mut e = at("héllo", 0);
        e.apply(Edit::Right);
        e.apply(Edit::Right);
        assert_eq!((e.cursor(), e.cursor_chars()), (3, 2));
        e.apply(Edit::End);
        e.apply(Edit::Left);
        assert_eq!(e.cursor(), 5);
        e.set_cursor_chars(2);
        assert_eq!(e.cursor(), 3);
        e.set_cursor_chars(99);
        assert_eq!(e.cursor(), 6);
    }

    #[test]
    fn deleting() {
        let mut e = at("ls /tmp/dir", 11);
        assert!(e.apply(Edit::BackspaceWord));
        assert_eq!(e.text(), "ls /tmp/");
        let mut e = at("ls /tmp/dir", 3);
        assert!(e.apply(Edit::DeleteWord));
        assert_eq!(e.text(), "ls dir");
        let mut e = at("abc", 1);
        assert!(e.apply(Edit::KillToEnd));
        assert_eq!((e.text(), e.cursor()), ("a", 1));
        let mut e = at("abc", 2);
        assert!(e.apply(Edit::KillToStart));
        assert_eq!((e.text(), e.cursor()), ("c", 0));
        assert!(!e.apply(Edit::Backspace));
        assert!(e.apply(Edit::Delete));
        assert!(e.is_empty());
    }

    #[test]
    fn typing() {
        let mut e = LineEditor::new();
        e.apply(Edit::Insert("ls\t-l\x1b\n"));
        assert_eq!(e.text(), "ls-l");
        assert!(e.replace_before_cursor(2, " -a"));
        assert_eq!((e.text(), e.cursor()), ("ls -a", 5));
        e.set("cat\tnotes.txt\x1b");
        assert_eq!((e.text(), e.cursor()), ("catnotes.txt", 12));
        // A line cannot grow without bound, however it is set.
        let mut e = LineEditor::new();
        e.apply(Edit::Insert(&"é".repeat(MAX_TEXT)));
        assert_eq!(e.text().len(), MAX_TEXT);
        assert!(!e.apply(Edit::Insert("x")));
        assert!(!e.replace_before_cursor(MAX_TEXT, "x"));
        assert!(e.replace_before_cursor(MAX_TEXT - 2, "e"));
        assert_eq!(e.text().len(), MAX_TEXT - 1);
        assert_eq!(e.take().len(), MAX_TEXT - 1);
        assert_eq!((e.text(), e.cursor()), ("", 0));
        e.set(&"x".repeat(MAX_TEXT + 10));
        assert_eq!((e.text().len(), e.cursor()), (MAX_TEXT, MAX_TEXT));
    }
}
