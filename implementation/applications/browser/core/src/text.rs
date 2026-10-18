// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The browser's files: lines, a record each, its fields apart by tabs. A
//! field's backslashes, tabs and line ends are written `\\`, `\t`, `\n`
//! and `\r`, so that any text fits in one.

use alloc::string::String;
use alloc::vec::Vec;

/// `field` as a file has it.
pub fn escape(field: &str) -> String {
    let mut out = String::with_capacity(field.len());
    for c in field.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out
}

/// What a file's `field` says; an escape it does not know stands for
/// itself.
pub fn unescape(field: &str) -> String {
    let mut out = String::with_capacity(field.len());
    let mut chars = field.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// A record's line (without its line end), from its fields.
pub fn line(fields: &[&str]) -> String {
    let fields: Vec<String> = fields.iter().map(|f| escape(f)).collect();
    fields.join("\t")
}

/// A line's fields.
pub fn fields(line: &str) -> Vec<String> {
    line.split('\t').map(unescape).collect()
}

/// The records of a file's text: its lines' fields, without empty lines.
pub fn records(text: &str) -> impl Iterator<Item = Vec<String>> + '_ {
    text.lines().filter(|l| !l.trim().is_empty()).map(fields)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn any_text_is_one_field() {
        for s in ["", "plain", "a\tb", "two\nlines\r\n", "back\\slash", "\\t", "end\\"] {
            assert_eq!(unescape(&escape(s)), s);
            assert!(!escape(s).contains(['\t', '\n', '\r']));
        }
        let l = line(&["a\tb", "c", ""]);
        assert_eq!(fields(&l), ["a\tb", "c", ""]);
        // A stray backslash stands for itself.
        assert_eq!(unescape("a\\qb\\"), "aqb\\");
        let text = "x\ty\n\n  \nz\n";
        assert_eq!(records(text).count(), 2);
    }
}
