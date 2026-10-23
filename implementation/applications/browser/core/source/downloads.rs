// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What a downloaded file is called on disk: the name the site suggests
//! (or its address's last part), made safe, and new.

use alloc::format;
use alloc::string::{String, ToString};

/// The longest name given, in bytes.
pub const MAX_NAME: usize = 200;

/// A file name for a download that suggests `suggested`, from `url`: no
/// directories, no line ends or control characters, no name of hidden
/// files, at most [`MAX_NAME`] bytes (its extension kept); `download`
/// when nothing is left.
pub fn file_name(suggested: &str, url: &str) -> String {
    // The last part of a hierarchical address's path, or its host.
    let from_url = url
        .split_once("://")
        .and_then(|(_, rest)| rest.split(['?', '#']).next())
        .and_then(|rest| rest.rsplit('/').find(|s| !s.is_empty()))
        .unwrap_or("");
    let raw = if suggested.trim().is_empty() { from_url } else { suggested };
    let base = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
    let mut name: String = base
        .chars()
        .map(|c| if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*') { '_' } else { c })
        .collect();
    name = name.trim().trim_start_matches('.').trim_end_matches(['.', ' ']).to_string();
    if name.is_empty() {
        return "download".into();
    }
    if name.len() > MAX_NAME {
        let (stem, ext) = split(&name);
        let keep = MAX_NAME.saturating_sub(ext.len());
        let mut cut = keep.min(stem.len());
        while !stem.is_char_boundary(cut) {
            cut -= 1;
        }
        name = format!("{}{}", &stem[..cut], ext);
    }
    name
}

/// `name`'s stem and extension (with its dot; `.tar.gz` as one).
fn split(name: &str) -> (&str, &str) {
    let dot = name
        .strip_suffix(".gz")
        .or_else(|| name.strip_suffix(".xz"))
        .or_else(|| name.strip_suffix(".bz2"))
        .and_then(|n| n.strip_suffix(".tar"))
        .map(|n| n.len())
        .or_else(|| name.rfind('.').filter(|&i| i > 0));
    match dot {
        Some(i) => (&name[..i], &name[i..]),
        None => (name, ""),
    }
}

/// `name`, or the first of `stem (1).ext`, `stem (2).ext`... that does not
/// `exist`.
pub fn unique(name: &str, exists: impl Fn(&str) -> bool) -> String {
    if !exists(name) {
        return name.to_string();
    }
    let (stem, ext) = split(name);
    (1..10_000).map(|n| format!("{stem} ({n}){ext}")).find(|c| !exists(c)).unwrap_or_else(|| name.to_string())
}

/// `bytes` as people read them.
pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1000.0;
    let mut unit = 0;
    while value >= 1000.0 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    if value < 10.0 { format!("{value:.1} {}", UNITS[unit]) } else { format!("{value:.0} {}", UNITS[unit]) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_safe() {
        assert_eq!(file_name("report.pdf", "https://a/x"), "report.pdf");
        assert_eq!(file_name("", "https://example.com/files/data.csv?x=1"), "data.csv");
        assert_eq!(file_name("../../etc/passwd", ""), "passwd");
        assert_eq!(file_name("..hidden", ""), "hidden");
        assert_eq!(file_name("a\nb\u{7}c.txt", ""), "a_b_c.txt");
        assert_eq!(file_name("", "https://example.com/"), "example.com");
        assert_eq!(file_name("  ", "data:text/plain,hi"), "download");
        let long = format!("{}.tar.gz", "x".repeat(500));
        let got = file_name(&long, "");
        assert!(got.len() <= MAX_NAME && got.ends_with(".tar.gz"));
    }

    #[test]
    fn names_are_new() {
        let taken = ["a.txt", "a (1).txt", "b"];
        let exists = |n: &str| taken.contains(&n);
        assert_eq!(unique("a.txt", exists), "a (2).txt");
        assert_eq!(unique("b", exists), "b (1)");
        assert_eq!(unique("c.tar.gz", exists), "c.tar.gz");
        assert_eq!(size(999), "999 B");
        assert_eq!(size(1500), "1.5 KB");
        assert_eq!(size(25_000_000), "25 MB");
    }
}
