// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! A repository's index (`index.vidx`): what the repository offers, signed
//! by its keys.
//!
//! Layout (integers little-endian):
//!
//! ```text
//! +----------------------------------+ 0
//! | magic "VEDAIDX\0"                |
//! | format (u32, 1)                  |
//! | compression (u32): 0 stored,     |
//! |   1 deflate                      |
//! | signatures (u32, 1 to 8)         |
//! | zero (u32)                       |
//! | contents' length (u64)           |
//! +----------------------------------+ 32
//! | each signature: the key's id     |
//! |   (32 bytes, the SHA-256 of the  |
//! |   key), the signature (64)       |
//! +----------------------------------+
//! | contents (JSON, compressed):     |
//! |   the repository's name, serial, |
//! |   when made and when it expires, |
//! |   and its packages              |
//! +----------------------------------+ end of the file
//! ```
//!
//! Each signature is of [`DOMAIN`], the format, the compression, the
//! contents' length and the contents as stored: no signature made for
//! anything else checks as an index's, and nothing in the file but the
//! signatures themselves goes unsigned.
//!
//! An index is taken ([`verify`]) only once one of its signatures checks
//! against a key the repository is trusted with, before anything of it is
//! decompressed or read; and only if it then names the repository it was
//! fetched for, so that one repository's index, signed by a key it shares
//! with another, cannot pass for the other's. What it offers is bound by
//! its packages' SHA-256s and sizes. Besides:
//!
//! * **Freshness.** An index may say when it expires; past then it is
//!   refused, so that someone who keeps a system from newer indexes cannot
//!   keep it on an old one, with the holes of its packages, for long.
//!   Indexes fetched over the network must say it.
//! * **No going back.** Its serial number never goes down from one index of
//!   a repository to the next, and an index of the same serial is the
//!   same index: no one can replay an older one, signed though it was.
//!
//! These are the protections The Update Framework (TUF) names against
//! rollback, freeze, mix-and-match and endless-data attacks, for
//! repositories with one role of keys.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

use serde::{Deserialize, Serialize};

use crate::keys::{PublicKey, SIGNATURE_LEN, SecretKey};
use crate::manifest::Manifest;
use crate::name::is_repository_name;
use crate::{Digest, date, limits};

/// The first bytes of every index.
pub const MAGIC: [u8; 8] = *b"VEDAIDX\0";
/// The format this code reads and writes.
pub const FORMAT: u32 = 1;
/// What an index file is called in its repository.
pub const FILE: &str = "index.vidx";
/// What every signature of an index signs first.
pub const DOMAIN: &[u8] = b"Veda package repository index\0";
const HEADER: usize = 32;
const SIGNATURE: usize = 32 + SIGNATURE_LEN;
const LEVEL: u8 = 9;

/// A package as a repository offers it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Offer {
    pub manifest: Manifest,
    /// The package file's path in the repository.
    pub file: String,
    /// The package file's size and SHA-256.
    pub size: u64,
    pub sha256: Digest,
    /// Its files' sizes together, installed.
    pub unpacked: u64,
}

/// What an index says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Contents {
    /// The repository's name.
    pub repository: String,
    /// Higher with each index the repository publishes.
    pub serial: u64,
    /// When it was made, and when it expires (Unix time, s), if it does.
    pub generated: u64,
    pub expires: Option<u64>,
    /// Sorted by name, then version.
    pub packages: Vec<Offer>,
}

/// Why an index is not taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    NotAnIndex,
    Format(u32),
    Compression(u32),
    Truncated,
    TooLarge(&'static str),
    /// No signature checks against a key the repository is trusted with.
    Untrusted,
    /// Its contents do not read, or are not valid (what is wrong).
    Contents(String),
    /// It is another repository's index.
    WrongRepository {
        expected: String,
        found: String,
    },
    /// It expired then.
    Expired(u64),
    /// It does not say when it expires, as one fetched over the network
    /// must.
    NoExpiry,
    /// Its serial is lower than that of an index taken before.
    Rollback {
        had: u64,
        got: u64,
    },
    /// It has the serial of the index taken before, but other contents.
    Changed(u64),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotAnIndex => f.write_str("not a Veda repository index"),
            Error::Format(v) => {
                write!(f, "an index of format {v}, which this system does not read (it reads {FORMAT})")
            }
            Error::Compression(c) => write!(f, "an index compressed in a way this system does not know ({c})"),
            Error::Truncated => f.write_str("the index is cut short, or its lengths do not add up"),
            Error::TooLarge(what) => write!(f, "the index's {what} is larger than an index may be"),
            Error::Untrusted => f.write_str("the index is not signed by a key the repository is trusted with"),
            Error::Contents(why) => write!(f, "the index's contents are not valid: {why}"),
            Error::WrongRepository { expected, found } => {
                write!(f, "the index is the repository '{found}''s, not '{expected}''s")
            }
            Error::Expired(at) => {
                write!(f, "the index expired on {} (if that is not past, the computer's clock is wrong)", date(*at))
            }
            Error::NoExpiry => f.write_str(
                "the index does not say when it expires, as one fetched over the network must: an old one could be \
                 replayed for ever",
            ),
            Error::Rollback { had, got } => {
                write!(f, "the index is older (serial {got}) than one this system has taken already ({had})")
            }
            Error::Changed(serial) => {
                write!(f, "the index has the serial of one taken already ({serial}), but other contents")
            }
        }
    }
}

/// Checks a package file's path in a repository: relative, of plain
/// components (letters, digits, `.`, `-`, `+`, `_`, `~`) that are neither
/// `.` nor `..`.
pub fn check_file(path: &str) -> bool {
    path.len() <= 255
        && path.split('/').all(|c| {
            !c.is_empty()
                && c != "."
                && c != ".."
                && c.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+' | b'_' | b'~'))
        })
}

impl Contents {
    /// Checks what the types alone do not: the repository's name, the
    /// packages' manifests, files and sizes, and that they are in order, no
    /// package twice at one version.
    pub fn validate(&self) -> Result<(), Error> {
        let bad = |why: String| Err(Error::Contents(why));
        if !is_repository_name(&self.repository) {
            return bad(alloc::format!("'{}' is not a repository's name", self.repository));
        }
        if self.packages.len() > limits::INDEX_PACKAGES {
            return Err(Error::TooLarge("number of packages"));
        }
        if self.expires.is_some_and(|e| e < self.generated) {
            return bad(String::from("it expires before it was made"));
        }
        for (i, offer) in self.packages.iter().enumerate() {
            let label = offer.manifest.label();
            offer.manifest.validate().map_err(|e| Error::Contents(e.to_string()))?;
            if !check_file(&offer.file) {
                return bad(alloc::format!("{label}'s file '{}' is not a path in the repository", offer.file));
            }
            if offer.size > limits::ARCHIVE || offer.unpacked > limits::UNPACKED {
                return bad(alloc::format!("{label} is larger than a package may be"));
            }
            if let Some(before) = i.checked_sub(1).map(|j| &self.packages[j].manifest) {
                let order = before.name.cmp(&offer.manifest.name).then(before.version.cmp(&offer.manifest.version));
                if order.is_ge() {
                    return bad(alloc::format!("{label} is out of order, or there twice"));
                }
            }
        }
        Ok(())
    }
}

/// The bytes each signature of an index signs.
fn signed(format: u32, compression: u32, contents: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(DOMAIN.len() + 16 + contents.len());
    message.extend_from_slice(DOMAIN);
    message.extend_from_slice(&format.to_le_bytes());
    message.extend_from_slice(&compression.to_le_bytes());
    message.extend_from_slice(&(contents.len() as u64).to_le_bytes());
    message.extend_from_slice(contents);
    message
}

/// Makes an index of `contents` (its packages put in order), signed by
/// every one of `keys`.
pub fn sign(mut contents: Contents, keys: &[&SecretKey], compress: bool) -> Result<Vec<u8>, Error> {
    contents
        .packages
        .sort_by(|a, b| a.manifest.name.cmp(&b.manifest.name).then(a.manifest.version.cmp(&b.manifest.version)));
    contents.validate()?;
    if keys.is_empty() || keys.len() > limits::SIGNATURES {
        return Err(Error::Contents(alloc::format!("an index has 1 to {} signatures", limits::SIGNATURES)));
    }
    let json = crate::to_json(&contents);
    if json.len() > limits::INDEX {
        return Err(Error::TooLarge("contents"));
    }
    let (compression, stored) =
        if compress { (1, miniz_oxide::deflate::compress_to_vec(&json, LEVEL)) } else { (0, json) };
    if HEADER + keys.len() * SIGNATURE + stored.len() > limits::INDEX_FILE {
        return Err(Error::TooLarge("file"));
    }
    let message = signed(FORMAT, compression, &stored);
    let mut out = Vec::with_capacity(HEADER + keys.len() * SIGNATURE + stored.len());
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&FORMAT.to_le_bytes());
    out.extend_from_slice(&compression.to_le_bytes());
    out.extend_from_slice(&(keys.len() as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(stored.len() as u64).to_le_bytes());
    for key in keys {
        out.extend_from_slice(&key.public().id().0);
        out.extend_from_slice(&key.sign(&message));
    }
    out.extend_from_slice(&stored);
    Ok(out)
}

/// What an index is checked against.
#[derive(Debug, Clone, Copy)]
pub struct Policy<'a> {
    /// The keys the repository is trusted with.
    pub keys: &'a [PublicKey],
    /// The repository's name, which the index must give.
    pub repository: &'a str,
    /// The time now (Unix time, s), past which an index has expired.
    pub now: u64,
    /// The index must say when it expires (it comes over the network).
    pub must_expire: bool,
    /// The index taken last from the repository, if one was: its serial
    /// and its contents' SHA-256 ([`Verified::contents_sha256`]).
    pub previous: Option<(u64, Digest)>,
}

/// An index that was taken.
#[derive(Debug, Clone)]
pub struct Verified {
    pub contents: Contents,
    /// The SHA-256 of its contents as stored, which an index of the same
    /// serial must have too.
    pub contents_sha256: Digest,
    /// The keys whose signatures checked.
    pub signed_by: Vec<PublicKey>,
}

/// Reads an index, taking it only if it is signed, current and the
/// repository's, as the module's documentation says.
pub fn verify(bytes: &[u8], policy: &Policy) -> Result<Verified, Error> {
    if bytes.len() < HEADER || bytes[..8] != MAGIC {
        return Err(if bytes.len() >= 8 && bytes[..8] != MAGIC { Error::NotAnIndex } else { Error::Truncated });
    }
    if bytes.len() > limits::INDEX_FILE {
        return Err(Error::TooLarge("file"));
    }
    let word = |at: usize| u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    let (format, compression, count) = (word(8), word(12), word(16) as usize);
    if format != FORMAT {
        return Err(Error::Format(format));
    }
    if compression > 1 {
        return Err(Error::Compression(compression));
    }
    if word(20) != 0 {
        return Err(Error::Contents(String::from("its header has bytes this format leaves zero")));
    }
    if count == 0 || count > limits::SIGNATURES {
        return Err(Error::Untrusted);
    }
    let mut length = [0u8; 8];
    length.copy_from_slice(&bytes[24..32]);
    let length = u64::from_le_bytes(length);
    let start = HEADER + count * SIGNATURE;
    if start as u64 + length != bytes.len() as u64 {
        return Err(Error::Truncated);
    }
    let stored = &bytes[start..];
    // The signatures first: nothing else is read of an index no key the
    // repository is trusted with signed.
    let message = signed(format, compression, stored);
    let mut signed_by: Vec<PublicKey> = Vec::new();
    for entry in bytes[HEADER..start].as_chunks::<SIGNATURE>().0 {
        let Some(key) = policy.keys.iter().find(|k| k.id().0[..] == entry[..32]) else { continue };
        let mut signature = [0u8; SIGNATURE_LEN];
        signature.copy_from_slice(&entry[32..]);
        if key.verify(&message, &signature) && !signed_by.contains(key) {
            signed_by.push(key.clone());
        }
    }
    if signed_by.is_empty() {
        return Err(Error::Untrusted);
    }
    let json = if compression == 1 {
        miniz_oxide::inflate::decompress_to_vec_with_limit(stored, limits::INDEX).map_err(|e| match e.status {
            miniz_oxide::inflate::TINFLStatus::HasMoreOutput => Error::TooLarge("contents"),
            _ => Error::Contents(String::from("they do not decompress")),
        })?
    } else {
        if stored.len() > limits::INDEX {
            return Err(Error::TooLarge("contents"));
        }
        stored.to_vec()
    };
    let contents: Contents = serde_json::from_slice(&json).map_err(|e| Error::Contents(alloc::format!("{e}")))?;
    contents.validate()?;
    if contents.repository != policy.repository {
        return Err(Error::WrongRepository { expected: policy.repository.to_string(), found: contents.repository });
    }
    match contents.expires {
        Some(at) if policy.now >= at => return Err(Error::Expired(at)),
        None if policy.must_expire => return Err(Error::NoExpiry),
        _ => {}
    }
    let contents_sha256 = Digest::of(stored);
    if let Some((had, digest)) = policy.previous {
        if contents.serial < had {
            return Err(Error::Rollback { had, got: contents.serial });
        }
        if contents.serial == had && contents_sha256 != digest {
            return Err(Error::Changed(had));
        }
    }
    Ok(Verified { contents, contents_sha256, signed_by })
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;
    use crate::tests::{With, manifest};

    fn key(n: u8) -> SecretKey {
        SecretKey::from_seed(&[n; 32])
    }

    fn offer(label: &str) -> Offer {
        let m = manifest(label);
        let file = alloc::format!("{}-{}.amd64.vpkg", m.name, m.version);
        Offer { manifest: m, file, size: 100, sha256: Digest::of(label.as_bytes()), unpacked: 300 }
    }

    fn contents(serial: u64, packages: Vec<Offer>) -> Contents {
        Contents { repository: String::from("main"), serial, generated: 1_000, expires: Some(10_000), packages }
    }

    fn policy(keys: &[PublicKey]) -> Policy<'_> {
        Policy { keys, repository: "main", now: 5_000, must_expire: true, previous: None }
    }

    #[test]
    fn signed_indexes_are_taken() {
        let k = key(1);
        let packages = vec![offer("zed 1.0"), offer("hello 2.0"), offer("hello 1.0").depends("zed")];
        for compress in [false, true] {
            let bytes = sign(contents(3, packages.clone()), &[&k], compress).unwrap();
            let v = verify(&bytes, &policy(&[k.public()])).unwrap();
            let labels: Vec<String> = v.contents.packages.iter().map(|o| o.manifest.label()).collect();
            assert_eq!(labels, ["hello 1.0", "hello 2.0", "zed 1.0"]);
            assert_eq!(v.signed_by, [k.public()]);
            assert_eq!(v.contents.serial, 3);
        }
        // Signing is deterministic: the same index, the same bytes.
        assert_eq!(sign(contents(3, packages.clone()), &[&k], true), sign(contents(3, packages), &[&k], true));
    }

    #[test]
    fn indexes_need_a_trusted_signature() {
        let (k, other) = (key(1), key(2));
        let bytes = sign(contents(1, vec![offer("hello 1.0")]), &[&other], true).unwrap();
        assert_eq!(verify(&bytes, &policy(&[k.public()])).unwrap_err(), Error::Untrusted);
        // Two signatures, one of them trusted: taken.
        let both = sign(contents(1, vec![offer("hello 1.0")]), &[&other, &k], true).unwrap();
        assert_eq!(verify(&both, &policy(&[k.public()])).unwrap().signed_by, [k.public()]);
        // A byte changed anywhere signed: refused.
        for at in [9, 13, 27, both.len() - 1] {
            let mut bent = both.clone();
            bent[at] ^= 1;
            assert!(verify(&bent, &policy(&[k.public()])).is_err(), "byte {at}");
        }
        // A signature moved onto another key's id: refused.
        let mut swapped = bytes.clone();
        swapped[HEADER..HEADER + 32].copy_from_slice(&k.public().id().0);
        assert_eq!(verify(&swapped, &policy(&[k.public()])).unwrap_err(), Error::Untrusted);
    }

    #[test]
    fn indexes_are_the_repositorys_and_current() {
        let k = key(1);
        let keys = [k.public()];
        let mut other = contents(1, Vec::new());
        other.repository = String::from("testing");
        let bytes = sign(other, &[&k], true).unwrap();
        assert!(matches!(verify(&bytes, &policy(&keys)), Err(Error::WrongRepository { .. })));
        let bytes = sign(contents(1, Vec::new()), &[&k], true).unwrap();
        let late = Policy { now: 10_000, ..policy(&keys) };
        assert_eq!(verify(&bytes, &late).unwrap_err(), Error::Expired(10_000));
        assert!(Error::Expired(10_000).to_string().contains("1970-01-01"));
        let mut forever = contents(1, Vec::new());
        forever.expires = None;
        let bytes = sign(forever, &[&k], true).unwrap();
        assert_eq!(verify(&bytes, &policy(&keys)).unwrap_err(), Error::NoExpiry);
        // A local repository's index (the system image's) need not expire.
        assert!(verify(&bytes, &Policy { must_expire: false, ..policy(&keys) }).is_ok());
    }

    #[test]
    fn indexes_never_go_back() {
        let k = key(1);
        let keys = [k.public()];
        let five = verify(&sign(contents(5, Vec::new()), &[&k], true).unwrap(), &policy(&keys)).unwrap();
        let previous = Some((5, five.contents_sha256));
        let check = |serial: u64, packages: Vec<Offer>| {
            let bytes = sign(contents(serial, packages), &[&k], true).unwrap();
            verify(&bytes, &Policy { previous, ..policy(&keys) }).map(|v| v.contents.serial)
        };
        assert_eq!(check(4, Vec::new()), Err(Error::Rollback { had: 5, got: 4 }));
        assert_eq!(check(5, Vec::new()), Ok(5));
        assert_eq!(check(5, vec![offer("hello 1.0")]), Err(Error::Changed(5)));
        assert_eq!(check(6, vec![offer("hello 1.0")]), Ok(6));
    }

    #[test]
    fn invalid_contents_are_refused_however_they_are_signed() {
        let k = key(1);
        for (packages, why) in [
            (vec![offer("hello 1.0"), offer("hello 1.00")], "there twice"),
            (vec![Offer { file: String::from("../x.vpkg"), ..offer("hello 1.0") }], "not a path"),
            (vec![Offer { size: limits::ARCHIVE + 1, ..offer("hello 1.0") }], "larger than"),
        ] {
            let e = sign(contents(1, packages), &[&k], true).unwrap_err();
            assert!(e.to_string().contains(why), "{why}: {e}");
        }
        let mut bad_name = contents(1, Vec::new());
        bad_name.repository = String::from("Main");
        assert!(sign(bad_name, &[&k], true).is_err());
        assert!(sign(contents(1, Vec::new()), &[], true).is_err());
        assert_eq!(verify(b"VEDAIDX", &policy(&[k.public()])).unwrap_err(), Error::Truncated);
        assert_eq!(verify(&[0u8; 64], &policy(&[k.public()])).unwrap_err(), Error::NotAnIndex);
    }

    #[test]
    fn package_paths_in_a_repository_are_plain() {
        for good in ["hello-1.0-1.amd64.vpkg", "pool/h/hello_1.0~rc1+b2.vpkg"] {
            assert!(check_file(good), "{good}");
        }
        for bad in ["", "/x.vpkg", "a//b", "./x", "../x", "a/../b", "x?y", "x y", "x%2e"] {
            assert!(!check_file(bad), "{bad:?}");
        }
    }
}
