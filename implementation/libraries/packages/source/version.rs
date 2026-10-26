// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Versions, and their order.
//!
//! A version is `[EPOCH:]UPSTREAM[-REVISION]`, ordered as Debian orders
//! its packages' versions (Debian Policy 5.6.12), an order that has
//! carried every kind of version upstream projects give for thirty years:
//!
//! * `EPOCH`, a number (0 when left out), counts first: it lets a package
//!   whose upstream numbering went backwards (`2024.1` after `5.0`) go on
//!   upgrading (`1:2024.1`).
//! * `UPSTREAM`, the upstream release (`1.2.3`, `2.0~rc1`, `9.5p1`),
//!   begins with a digit and holds letters, digits and `.`, `+` and `~`.
//! * `REVISION`, the package's own (`-1`, `-2`), holds the same, and is
//!   compared after the upstream part ("0" when left out).
//!
//! Upstream and revision are compared as Debian's `verrevcmp` does: in
//! turns of non-digits, compared character by character (letters before
//! other characters, `~` before everything, even the end), and of digits,
//! compared as numbers. So `1.0~rc1 < 1.0 < 1.0a < 1.0+b1 < 1.0.1 < 1.10`.
//!
//! Unlike Debian's, a version has at most one `:` and one `-`, so that
//! where its parts begin is never in doubt, and at most 64 bytes. Two
//! versions that differ only in leading zeros (`1.01`, `1.1`) are equal:
//! a repository offering both for one package is refused.

use alloc::string::{String, ToString};
use core::cmp::Ordering;
use core::fmt;

use crate::limits;

/// A package's version.
#[derive(Clone)]
pub struct Version {
    /// The version as written.
    text: String,
    epoch: u32,
    /// Where the upstream part begins and ends in `text`, and the revision
    /// (empty when there is none).
    upstream: (usize, usize),
    revision: (usize, usize),
}

/// Why a version is not one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadVersion(pub String);

impl fmt::Display for BadVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "'{}' is not a version: one is [EPOCH:]UPSTREAM[-REVISION] (such as 1.2.3-1), its upstream part \
             beginning with a digit, of letters, digits, '.', '+' and '~' alone, in at most {} bytes",
            self.0.chars().take(80).collect::<String>(),
            limits::VERSION
        )
    }
}

/// Whether `part` is an upstream part or a revision's characters.
fn plain(part: &str) -> bool {
    !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'~'))
}

impl Version {
    /// Checks and reads `text` as a version.
    pub fn parse(text: &str) -> Result<Version, BadVersion> {
        let bad = || BadVersion(text.to_string());
        if text.len() > limits::VERSION || text.matches(':').count() > 1 || text.matches('-').count() > 1 {
            return Err(bad());
        }
        let (epoch, start) = match text.split_once(':') {
            Some((e, _)) => {
                // Digits alone, without leading zeros: `01:` and `1:` would
                // be the same epoch written two ways.
                let valid = !e.is_empty() && e.bytes().all(|b| b.is_ascii_digit()) && (e == "0" || !e.starts_with('0'));
                (if valid { e.parse::<u32>().map_err(|_| bad())? } else { return Err(bad()) }, e.len() + 1)
            }
            None => (0, 0),
        };
        let rest = &text[start..];
        let (upstream, revision) = match rest.split_once('-') {
            Some((u, r)) => (u, Some(r)),
            None => (rest, None),
        };
        if !plain(upstream) || !upstream.as_bytes()[0].is_ascii_digit() || revision.is_some_and(|r| !plain(r)) {
            return Err(bad());
        }
        let upstream_end = start + upstream.len();
        let revision = match revision {
            Some(r) => (upstream_end + 1, upstream_end + 1 + r.len()),
            None => (text.len(), text.len()),
        };
        Ok(Version { text: text.to_string(), epoch, upstream: (start, upstream_end), revision })
    }

    /// The version as written.
    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub fn epoch(&self) -> u32 {
        self.epoch
    }

    pub fn upstream(&self) -> &str {
        &self.text[self.upstream.0..self.upstream.1]
    }

    /// The package's revision ("" when there is none, which orders as
    /// "0").
    pub fn revision(&self) -> &str {
        &self.text[self.revision.0..self.revision.1]
    }
}

/// Where a character stands in the order of non-digit runs: `~` before the
/// end, the end and digits (which end a run) together, letters before every
/// other character.
fn rank(c: Option<u8>) -> i32 {
    match c {
        None => 0,
        Some(c) if c.is_ascii_digit() => 0,
        Some(c) if c.is_ascii_alphabetic() => c as i32,
        Some(b'~') => -1,
        Some(c) => c as i32 + 256,
    }
}

/// Debian's `verrevcmp`: compares two upstream parts, or two revisions.
pub fn compare_parts(a: &str, b: &str) -> Ordering {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let (mut i, mut j) = (0, 0);
    let digit = |s: &[u8], k: usize| s.get(k).is_some_and(u8::is_ascii_digit);
    while i < a.len() || j < b.len() {
        // A run of non-digits, compared character by character.
        while (i < a.len() && !digit(a, i)) || (j < b.len() && !digit(b, j)) {
            let (x, y) = (rank(a.get(i).copied()), rank(b.get(j).copied()));
            if x != y {
                return x.cmp(&y);
            }
            i += 1;
            j += 1;
        }
        // A run of digits, compared as a number: leading zeros skipped,
        // then the longer number is the larger, else the first digit that
        // differs decides.
        while a.get(i) == Some(&b'0') {
            i += 1;
        }
        while b.get(j) == Some(&b'0') {
            j += 1;
        }
        let mut first_difference = Ordering::Equal;
        while digit(a, i) && digit(b, j) {
            if first_difference == Ordering::Equal {
                first_difference = a[i].cmp(&b[j]);
            }
            i += 1;
            j += 1;
        }
        if digit(a, i) {
            return Ordering::Greater;
        }
        if digit(b, j) {
            return Ordering::Less;
        }
        if first_difference != Ordering::Equal {
            return first_difference;
        }
    }
    Ordering::Equal
}

impl Ord for Version {
    fn cmp(&self, other: &Version) -> Ordering {
        self.epoch
            .cmp(&other.epoch)
            .then_with(|| compare_parts(self.upstream(), other.upstream()))
            .then_with(|| compare_parts(self.revision(), other.revision()))
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Version) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Versions are equal when they are the same in the order, as `1.01` and
/// `1.1` are.
impl PartialEq for Version {
    fn eq(&self, other: &Version) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Version {}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

impl fmt::Debug for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.text)
    }
}

impl serde::Serialize for Version {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.text)
    }
}

impl<'de> serde::Deserialize<'de> for Version {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Version, D::Error> {
        let text: &str = serde::Deserialize::deserialize(d)?;
        Version::parse(text).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use alloc::format;
    use alloc::vec::Vec;

    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn versions_have_three_parts() {
        let full = v("2:1.2.3~rc1-4");
        assert_eq!((full.epoch(), full.upstream(), full.revision()), (2, "1.2.3~rc1", "4"));
        let plain = v("1.0");
        assert_eq!((plain.epoch(), plain.upstream(), plain.revision()), (0, "1.0", ""));
        assert_eq!(v("0:7").upstream(), "7");
        assert_eq!(v("1.0-1").to_string(), "1.0-1");
    }

    #[test]
    fn malformed_versions_are_refused() {
        let long = format!("1.{}", "0".repeat(limits::VERSION));
        for bad in [
            "", "a1", ".1", "1..x-", "1-", "-1", ":1", "1:", "x:1", "01:1", "1:2:3", "1-2-3", "1.0 ", "1,0", "1_0",
            "1/2", "é", &long,
        ] {
            assert!(Version::parse(bad).is_err(), "{bad:?}");
        }
        assert!(BadVersion("x".into()).to_string().contains("'x' is not a version"));
    }

    #[test]
    fn versions_are_ordered_as_debian_orders_them() {
        // Each strictly before the next.
        let ascending = [
            "0.9", "1.0~~", "1.0~~a", "1.0~", "1.0~rc1", "1.0~rc2", "1.0", "1.0-1", "1.0-1.1", "1.0-2", "1.0-10",
            "1.0a", "1.0b", "1.0+b1", "1.0.1", "1.1", "1.2", "1.10", "1.10a", "2", "10", "2024.1", "1:0.1", "1:1.0",
            "2:0",
        ];
        let parsed: Vec<Version> = ascending.iter().map(|t| v(t)).collect();
        for (i, a) in parsed.iter().enumerate() {
            for (j, b) in parsed.iter().enumerate() {
                assert_eq!(a.cmp(b), i.cmp(&j), "{a} against {b}");
            }
        }
    }

    #[test]
    fn leading_zeros_do_not_count() {
        assert_eq!(v("1.01"), v("1.1"));
        assert_eq!(v("1.0-0"), v("1.0"));
        assert_eq!(v("0:1.0"), v("1.0"));
        assert_eq!(v("007"), v("7"));
        assert_ne!(v("1.0"), v("1.0.0"));
        assert!(v("1.0") < v("1.0.0"));
    }

    #[test]
    fn the_parts_compare_as_verrevcmp_does() {
        use Ordering::*;
        for (a, b, order) in [
            ("", "", Equal),
            ("", "0", Equal),
            ("~", "", Less),
            ("a", "", Greater),
            ("a", "b", Less),
            ("a", "+", Less),
            ("+", ".", Less),
            ("9", "10", Less),
            ("1a", "1", Greater),
            ("1~", "1", Less),
            ("123456789012345678901234567890", "123456789012345678901234567889", Greater),
        ] {
            assert_eq!(compare_parts(a, b), order, "{a:?} against {b:?}");
            assert_eq!(compare_parts(b, a), order.reverse(), "{b:?} against {a:?}");
        }
    }

    #[test]
    fn versions_travel_as_strings() {
        let text = serde_json::to_string(&v("1:2.0-3")).unwrap();
        assert_eq!(text, "\"1:2.0-3\"");
        assert_eq!(serde_json::from_str::<Version>(&text).unwrap(), v("1:2.0-3"));
        assert!(serde_json::from_str::<Version>("\"x\"").is_err());
        assert!(serde_json::from_str::<Version>("12").is_err());
    }
}
