// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `packages` — Veda's packages, apart from the service that installs them
//! (`implementation/services/package-manager`), so that the build machine's
//! tools, the `pkg` command and the service share one implementation,
//! tested on the host. [Packages](../../../../documentation/PACKAGES.md)
//! describes the design; in short:
//!
//! * [`name`], [`version`], [`requirement`]: what packages are called,
//!   how their versions are ordered (Debian's order), and what they ask of
//!   one another (`libgreet (>= 1.2, < 2)`).
//! * [`manifest`]: what a package says of itself.
//! * [`archive`]: the package file (`.vpkg`): its manifest, its files'
//!   paths, sizes and SHA-256s, and their contents in one deflate stream,
//!   read with every limit checked before anything is unpacked.
//! * [`keys`]: Ed25519 keys, which sign repositories' indexes.
//! * [`index`]: a repository's index (`index.vidx`), signed: what it
//!   offers, with each package file's size and SHA-256, its serial number
//!   (no older index replaces a newer one) and when it expires.
//! * [`config`]: the repositories a system uses: the system image's, and
//!   those the user adds.
//! * [`database`]: what is installed, file by file, and the *world*: the
//!   packages the user asked for by name.
//! * [`resolve`]: which versions of which packages satisfy the world.
//! * [`plan`]: what a change installs, upgrades and removes.
//! * [`transaction`]: carries a change out over a file system
//!   ([`transaction::Files`]), journalled, so that one cut short (power
//!   lost, the service ended) is rolled back or completed at the next
//!   start, never left half done.
//!
//! Nothing here panics on what it reads: archives, indexes, databases and
//! journals that are malformed, too large or inconsistent are errors.

#![no_std]

extern crate alloc;
#[cfg(any(test, feature = "std"))]
extern crate std;

pub mod archive;
pub mod config;
pub mod database;
pub mod index;
pub mod keys;
pub mod manifest;
pub mod name;
pub mod plan;
pub mod requirement;
pub mod resolve;
#[cfg(feature = "std")]
pub mod stdfs;
pub mod transaction;
pub mod version;

#[cfg(test)]
mod memfs;
#[cfg(test)]
mod tests;

use alloc::string::String;
use core::fmt;

use sha2::Digest as _;

/// Limits on what packages, indexes and the database may hold: what any of
/// them can make the system read, unpack or keep. Each is checked before
/// the work it bounds is done.
pub mod limits {
    /// Bytes of a package's name.
    pub const NAME: usize = 64;
    /// Bytes of a version.
    pub const VERSION: usize = 64;
    /// Bytes of a package's summary (one line).
    pub const SUMMARY: usize = 200;
    /// Bytes of a package's description.
    pub const DESCRIPTION: usize = 8192;
    /// Bytes of a package's licence, homepage, and other short texts.
    pub const SHORT_TEXT: usize = 512;
    /// Dependencies, conflicts and provisions of one package, each.
    pub const RELATIONS: usize = 256;
    /// Constraints on one dependency's version.
    pub const CONSTRAINTS: usize = 4;
    /// Files of one package.
    pub const FILES: usize = 65_536;
    /// Bytes of a path in a package.
    pub const PATH: usize = 1024;
    /// Bytes of one component of a path.
    pub const COMPONENT: usize = 255;
    /// Bytes of a package's metadata (its manifest and its files' list).
    pub const METADATA: usize = 16 << 20;
    /// Bytes of a package file.
    pub const ARCHIVE: u64 = 2 << 30;
    /// Bytes of a package's files, unpacked.
    pub const UNPACKED: u64 = 4 << 30;
    /// Bytes of a repository's index file.
    pub const INDEX_FILE: usize = 64 << 20;
    /// Bytes of an index's contents, unpacked.
    pub const INDEX: usize = 256 << 20;
    /// Packages in one index.
    pub const INDEX_PACKAGES: usize = 200_000;
    /// Signatures on one index.
    pub const SIGNATURES: usize = 8;
    /// Keys one repository is trusted with.
    pub const KEYS: usize = 8;
    /// Repositories a system uses.
    pub const REPOSITORIES: usize = 32;
    /// Bytes of a repository's address.
    pub const URL: usize = 512;
    /// Bytes of the database of installed packages.
    pub const DATABASE: usize = 256 << 20;
    /// Bytes of a transaction's journal.
    pub const JOURNAL: usize = 256 << 20;
    /// Packages the world names.
    pub const WORLD: usize = 4096;
}

/// A SHA-256 digest, written as 64 lower-case hexadecimal digits.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Digest(pub [u8; 32]);

impl Digest {
    /// The digest of `data`.
    pub fn of(data: &[u8]) -> Digest {
        Digest(sha2::Sha256::digest(data).into())
    }

    /// Parses 64 hexadecimal digits.
    pub fn parse(text: &str) -> Option<Digest> {
        let mut out = [0u8; 32];
        if text.len() != 64 || !text.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            return None;
        }
        base16ct::lower::decode(text, &mut out).ok()?;
        Some(Digest(out))
    }

    /// The digest as hexadecimal digits.
    pub fn hex(&self) -> String {
        base16ct::lower::encode_string(&self.0)
    }

    /// The first `n` hexadecimal digits, for people (a cache's file name, a
    /// key's fingerprint).
    pub fn short(&self, n: usize) -> String {
        let mut hex = self.hex();
        hex.truncate(n);
        hex
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.hex())
    }
}

impl fmt::Debug for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Digest({})", self.short(16))
    }
}

impl serde::Serialize for Digest {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.hex())
    }
}

impl<'de> serde::Deserialize<'de> for Digest {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Digest, D::Error> {
        let text: &str = serde::Deserialize::deserialize(d)?;
        Digest::parse(text).ok_or_else(|| serde::de::Error::custom("not a SHA-256 digest (64 hexadecimal digits)"))
    }
}

/// A SHA-256 computed piece by piece.
#[derive(Clone, Default)]
pub struct Hasher(sha2::Sha256);

impl Hasher {
    pub fn new() -> Hasher {
        Hasher::default()
    }

    pub fn update(&mut self, data: &[u8]) {
        self.0.update(data);
    }

    pub fn finish(self) -> Digest {
        Digest(self.0.finalize().into())
    }
}

impl fmt::Debug for Hasher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Hasher")
    }
}

/// The date of `unix` (seconds since 1970-01-01 UTC), `2026-10-26`, for
/// messages (Howard Hinnant's civil-from-days).
pub fn date(unix: u64) -> String {
    let days = (unix / 86_400) as i64 + 719_468;
    let era = days.div_euclid(146_097);
    let doe = days - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    alloc::format!("{year:04}-{month:02}-{day:02}")
}

/// `n` things, as people say it: `1 package`, `2 packages`.
pub fn count(n: u64, one: &str, many: &str) -> String {
    if n == 1 { alloc::format!("1 {one}") } else { alloc::format!("{n} {many}") }
}

/// Whether `text` is one line of printable characters (no control
/// characters), as names, summaries and other short texts are.
pub(crate) fn printable(text: &str) -> bool {
    !text.chars().any(char::is_control)
}

/// Writes `value` as JSON, the same every time for the same value (struct
/// fields in their order, no maps of unspecified order), as signatures and
/// reproducible builds need.
pub(crate) fn to_json<T: serde::Serialize>(value: &T) -> alloc::vec::Vec<u8> {
    // Serialising the crate's own types cannot fail: they hold strings,
    // numbers and lists alone, and no map whose keys are not strings.
    serde_json::to_vec(value).expect("the crate's types always serialise")
}
