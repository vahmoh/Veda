// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The package file (`.vpkg`): a package's manifest, its files' paths,
//! sizes and SHA-256s, and their contents.
//!
//! Layout (integers little-endian):
//!
//! ```text
//! +----------------------------------+ 0
//! | magic "VEDAPKG\0"                |
//! | format (u32, 1)                  |
//! | compression (u32): 0 stored,     |
//! |   1 deflate (RFC 1951)           |
//! | metadata's length (u64)          |
//! | payload's length (u64)           |
//! | files' sizes together (u64)      |
//! | zeros (24 bytes)                 |
//! +----------------------------------+ 64
//! | metadata: JSON, {"manifest": {}, |
//! |   "files": [{"path", "size",     |
//! |   "sha256"}]}, sorted by path    |
//! +----------------------------------+
//! | payload: the files' contents in  |
//! |   the order of the list, one     |
//! |   after the other, as one stream |
//! +----------------------------------+ end of the file
//! ```
//!
//! A package holds regular files alone, at relative paths under the
//! installed packages' directory, each beginning with one of
//! [`TOP_LEVEL`]: no links, devices, owners or permissions, which Veda has
//! none of, and which make archive formats such as tar a way to write
//! outside where an archive is unpacked. Directories are where files are.
//! A file is a program by what it holds (an ELF executable, a `#!`
//! script), as everywhere on Veda.
//!
//! Reading checks everything before anything is unpacked: the header, the
//! lengths (which must add up to the file's own, exactly), the metadata,
//! every path, that no file stands where another's directory is, and that
//! the sizes add up. Unpacking ([`Archive::unpack`]) then streams each file
//! out in pieces, never holding more than a piece of it, and checks each
//! one's size and SHA-256 as it ends; a stream that would make more than
//! the sizes say is cut off where it does.
//!
//! The same files and manifest make the same bytes ([`Builder::build`]),
//! so that packages build reproducibly.

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

use miniz_oxide::inflate::stream::InflateState;
use miniz_oxide::{DataFormat, MZError, MZFlush, MZStatus};
use serde::{Deserialize, Serialize};

use crate::manifest::Manifest;
use crate::{Digest, Hasher, limits};

/// The first bytes of every package file.
pub const MAGIC: [u8; 8] = *b"VEDAPKG\0";
/// The format this code reads and writes.
pub const FORMAT: u32 = 1;
/// The header's bytes.
const HEADER: usize = 64;
/// The pieces files are unpacked in.
const PIECE: usize = 64 * 1024;
/// How hard the builder compresses: miniz's best but its slowest.
const LEVEL: u8 = 9;

/// The directories a package's paths may begin with, as `/system` has them.
pub const TOP_LEVEL: [&str; 9] = ["apps", "bin", "etc", "fonts", "include", "lib", "libexec", "licences", "share"];

/// How a payload is stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    Stored,
    Deflate,
}

impl Compression {
    fn code(self) -> u32 {
        match self {
            Compression::Stored => 0,
            Compression::Deflate => 1,
        }
    }

    fn from_code(code: u32) -> Option<Compression> {
        match code {
            0 => Some(Compression::Stored),
            1 => Some(Compression::Deflate),
            _ => None,
        }
    }
}

/// A file of a package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    /// Its path under the installed packages' directory (`bin/hello`).
    pub path: String,
    pub size: u64,
    pub sha256: Digest,
}

/// What follows the header.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Metadata {
    manifest: Manifest,
    files: Vec<Entry>,
}

/// What is wrong with a package file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// It does not begin as a package does.
    NotAPackage,
    /// It is of a format this system does not read (a newer one).
    Format(u32),
    /// Its payload is compressed in a way this system does not know.
    Compression(u32),
    /// Its lengths do not add up to its own, or it is cut short.
    Truncated,
    /// It, its metadata, or its files are larger than a package may be
    /// (the limit's name).
    TooLarge(&'static str),
    /// Its metadata does not read, or its manifest is not a valid one.
    Metadata(String),
    /// One of its paths is not one a package may install (the path, why).
    Path(String, &'static str),
    /// Its payload does not hold what its list says (the path where it
    /// went wrong, or what).
    Corrupt(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotAPackage => f.write_str("not a Veda package file"),
            Error::Format(v) => {
                write!(f, "a package of format {v}, which this system does not read (it reads {FORMAT})")
            }
            Error::Compression(c) => write!(f, "a package compressed in a way this system does not know ({c})"),
            Error::Truncated => f.write_str("the package file is cut short, or its lengths do not add up"),
            Error::TooLarge(what) => write!(f, "the package's {what} is larger than a package may be"),
            Error::Metadata(why) => write!(f, "the package's description of itself is not valid: {why}"),
            Error::Path(path, why) => write!(f, "the package has a file at '{path}', which {why}"),
            Error::Corrupt(what) => write!(f, "the package's contents are damaged ({what})"),
        }
    }
}

/// Checks a path a package installs a file at: relative, of components
/// separated by single `/`, none of them `.`, `..` or empty, of printable
/// characters but `\`, and under one of [`TOP_LEVEL`].
pub fn check_path(path: &str) -> Result<(), &'static str> {
    if path.is_empty() || path.len() > limits::PATH {
        return Err("is empty, or longer than a path may be");
    }
    if path.chars().any(|c| c.is_control() || c == '\\') {
        return Err("has a control character or a backslash");
    }
    let mut components = path.split('/');
    let top = components.next().unwrap_or_default();
    if !TOP_LEVEL.contains(&top) {
        return Err("is not in one of the directories packages install into (bin, lib, share, ...)");
    }
    let mut depth = 0;
    for c in components {
        if c.is_empty() || c == "." || c == ".." || c.len() > limits::COMPONENT {
            return Err("has an empty, '.' or '..' component, or one longer than a name may be");
        }
        depth += 1;
    }
    if depth == 0 {
        return Err("is one of the directories packages install into, not a file in it");
    }
    Ok(())
}

/// Checks a list of entries: paths valid, in order and each once, no file
/// where another's directory is, at most [`limits::FILES`] of them; returns
/// their sizes together.
fn check_entries(files: &[Entry]) -> Result<u64, Error> {
    if files.len() > limits::FILES {
        return Err(Error::TooLarge("number of files"));
    }
    let mut total: u64 = 0;
    let mut seen = BTreeSet::new();
    for (i, e) in files.iter().enumerate() {
        check_path(&e.path).map_err(|why| Error::Path(e.path.clone(), why))?;
        if i > 0 && files[i - 1].path >= e.path {
            return Err(Error::Path(e.path.clone(), "is out of order, or there twice"));
        }
        total = total.checked_add(e.size).filter(|&t| t <= limits::UNPACKED).ok_or(Error::TooLarge("files"))?;
        seen.insert(e.path.as_str());
    }
    // A file at a path another one's directory is on (`bin/x` and
    // `bin/x/y`) could not be unpacked.
    for e in files {
        let mut at = e.path.as_str();
        while let Some((parent, _)) = at.rsplit_once('/') {
            if seen.contains(parent) {
                return Err(Error::Path(e.path.clone(), "is inside another file of the package"));
            }
            at = parent;
        }
    }
    Ok(total)
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    let mut word = [0u8; 8];
    word.copy_from_slice(&b[at..at + 8]);
    u64::from_le_bytes(word)
}

/// A package file read and checked, its contents not yet unpacked.
pub struct Archive<'a> {
    pub manifest: Manifest,
    pub files: Vec<Entry>,
    compression: Compression,
    payload: &'a [u8],
    unpacked: u64,
}

impl fmt::Debug for Archive<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Archive").field("package", &self.manifest.label()).field("files", &self.files.len()).finish()
    }
}

/// A piece of what [`Archive::unpack`] gives out, in order: each file
/// begins, has its contents in any number of pieces (none for an empty
/// file), and ends once they are whole and checked.
#[derive(Debug)]
pub enum Piece<'p> {
    Begin(&'p Entry),
    Data(&'p [u8]),
    End(&'p Entry),
}

/// Why unpacking stopped: the package is damaged, or what it was unpacked
/// into failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnpackError<E> {
    Archive(Error),
    Sink(E),
}

impl<'a> Archive<'a> {
    /// Reads and checks a package file's header and metadata.
    pub fn open(bytes: &'a [u8]) -> Result<Archive<'a>, Error> {
        if bytes.len() < HEADER || bytes[..8] != MAGIC {
            return Err(if bytes.len() >= 8 && bytes[..8] != MAGIC { Error::NotAPackage } else { Error::Truncated });
        }
        let format = u32_at(bytes, 8);
        if format != FORMAT {
            return Err(Error::Format(format));
        }
        let compression = u32_at(bytes, 12);
        let compression = Compression::from_code(compression).ok_or(Error::Compression(compression))?;
        let (metadata_len, payload_len, unpacked) = (u64_at(bytes, 16), u64_at(bytes, 24), u64_at(bytes, 32));
        if bytes[40..HEADER].iter().any(|&b| b != 0) {
            return Err(Error::Metadata(String::from("its header has bytes this format leaves zero")));
        }
        if bytes.len() as u64 > limits::ARCHIVE {
            return Err(Error::TooLarge("file"));
        }
        if metadata_len > limits::METADATA as u64 {
            return Err(Error::TooLarge("metadata"));
        }
        if unpacked > limits::UNPACKED {
            return Err(Error::TooLarge("files"));
        }
        let end = (HEADER as u64).checked_add(metadata_len).and_then(|m| m.checked_add(payload_len));
        if end != Some(bytes.len() as u64) {
            return Err(Error::Truncated);
        }
        let metadata_end = HEADER + metadata_len as usize;
        let metadata: Metadata =
            serde_json::from_slice(&bytes[HEADER..metadata_end]).map_err(|e| Error::Metadata(alloc::format!("{e}")))?;
        metadata.manifest.validate().map_err(|e| Error::Metadata(e.to_string()))?;
        let total = check_entries(&metadata.files)?;
        if total != unpacked || (compression == Compression::Stored && payload_len != unpacked) {
            return Err(Error::Corrupt(String::from("its files' sizes do not add up to what its header says")));
        }
        Ok(Archive {
            manifest: metadata.manifest,
            files: metadata.files,
            compression,
            payload: &bytes[metadata_end..],
            unpacked,
        })
    }

    /// The files' sizes together.
    pub fn unpacked(&self) -> u64 {
        self.unpacked
    }

    /// Gives `sink` every file's contents, in pieces, checking each file's
    /// size and SHA-256 before its end; stops at the first thing wrong,
    /// with the package or with what `sink` returns.
    pub fn unpack<E>(&self, mut sink: impl FnMut(Piece<'_>) -> Result<(), E>) -> Result<(), UnpackError<E>> {
        let mut out = Output { files: &self.files, at: 0, left: 0, hasher: Hasher::new(), begun: false };
        out.next(&mut sink)?;
        match self.compression {
            Compression::Stored => out.feed(self.payload, &mut sink)?,
            Compression::Deflate => {
                let mut state: Box<InflateState> = InflateState::new_boxed(DataFormat::Raw);
                let mut buffer = vec![0u8; PIECE];
                let mut input = self.payload;
                loop {
                    let r = miniz_oxide::inflate::stream::inflate(&mut state, input, &mut buffer, MZFlush::None);
                    input = &input[r.bytes_consumed..];
                    out.feed(&buffer[..r.bytes_written], &mut sink)?;
                    match r.status {
                        Ok(MZStatus::StreamEnd) => break,
                        Ok(MZStatus::Ok) if r.bytes_consumed > 0 || r.bytes_written > 0 => {}
                        Err(MZError::Buf) | Ok(MZStatus::Ok) if input.is_empty() => {
                            return Err(UnpackError::Archive(Error::Corrupt(String::from("the stream ends early"))));
                        }
                        _ => {
                            return Err(UnpackError::Archive(Error::Corrupt(String::from(
                                "the stream does not decompress",
                            ))));
                        }
                    }
                }
                if !input.is_empty() {
                    return Err(UnpackError::Archive(Error::Corrupt(String::from("data follows the stream's end"))));
                }
            }
        }
        if out.at < self.files.len() {
            let path = self.files[out.at].path.clone();
            return Err(UnpackError::Archive(Error::Corrupt(alloc::format!("{path} is cut short"))));
        }
        Ok(())
    }
}

/// Where unpacking is: which file, and how much of it is still to come.
struct Output<'f> {
    files: &'f [Entry],
    at: usize,
    left: u64,
    hasher: Hasher,
    /// The file at `at` has begun.
    begun: bool,
}

impl Output<'_> {
    /// Ends the files that are whole (an empty one is as soon as it
    /// begins) and begins the next.
    fn next<E>(&mut self, sink: &mut impl FnMut(Piece<'_>) -> Result<(), E>) -> Result<(), UnpackError<E>> {
        loop {
            let Some(entry) = self.files.get(self.at) else { return Ok(()) };
            if !self.begun {
                sink(Piece::Begin(entry)).map_err(UnpackError::Sink)?;
                self.begun = true;
                self.left = entry.size;
                self.hasher = Hasher::new();
            }
            if self.left > 0 {
                return Ok(());
            }
            if core::mem::take(&mut self.hasher).finish() != entry.sha256 {
                return Err(UnpackError::Archive(Error::Corrupt(alloc::format!(
                    "{} is not what was packed",
                    entry.path
                ))));
            }
            sink(Piece::End(entry)).map_err(UnpackError::Sink)?;
            self.at += 1;
            self.begun = false;
        }
    }

    /// Hands `data` to the files it belongs to.
    fn feed<E>(
        &mut self,
        mut data: &[u8],
        sink: &mut impl FnMut(Piece<'_>) -> Result<(), E>,
    ) -> Result<(), UnpackError<E>> {
        while !data.is_empty() {
            if self.at >= self.files.len() {
                return Err(UnpackError::Archive(Error::Corrupt(String::from("more data than its files' sizes"))));
            }
            let n = (self.left.min(data.len() as u64)) as usize;
            let (piece, rest) = data.split_at(n);
            self.hasher.update(piece);
            sink(Piece::Data(piece)).map_err(UnpackError::Sink)?;
            self.left -= n as u64;
            data = rest;
            self.next(sink)?;
        }
        Ok(())
    }
}

/// Makes a package file.
#[derive(Debug)]
pub struct Builder {
    manifest: Manifest,
    files: BTreeMap<String, Vec<u8>>,
}

impl Builder {
    /// A package of `manifest`, without files yet.
    pub fn new(manifest: Manifest) -> Builder {
        Builder { manifest, files: BTreeMap::new() }
    }

    /// Adds the file `path` (`bin/hello`), holding `data`.
    pub fn add(&mut self, path: &str, data: Vec<u8>) -> Result<(), Error> {
        check_path(path).map_err(|why| Error::Path(path.to_string(), why))?;
        if self.files.contains_key(path) {
            return Err(Error::Path(path.to_string(), "is added twice"));
        }
        self.files.insert(path.to_string(), data);
        Ok(())
    }

    /// The package file.
    pub fn build(self, compression: Compression) -> Result<Vec<u8>, Error> {
        self.manifest.validate().map_err(|e| Error::Metadata(e.to_string()))?;
        let files: Vec<Entry> = self
            .files
            .iter()
            .map(|(path, data)| Entry { path: path.clone(), size: data.len() as u64, sha256: Digest::of(data) })
            .collect();
        let unpacked = check_entries(&files)?;
        let metadata = crate::to_json(&Metadata { manifest: self.manifest, files });
        if metadata.len() > limits::METADATA {
            return Err(Error::TooLarge("metadata"));
        }
        let mut contents = Vec::with_capacity(unpacked as usize);
        for data in self.files.values() {
            contents.extend_from_slice(data);
        }
        let payload = match compression {
            Compression::Stored => contents,
            Compression::Deflate => miniz_oxide::deflate::compress_to_vec(&contents, LEVEL),
        };
        let mut out = Vec::with_capacity(HEADER + metadata.len() + payload.len());
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&FORMAT.to_le_bytes());
        out.extend_from_slice(&compression.code().to_le_bytes());
        out.extend_from_slice(&(metadata.len() as u64).to_le_bytes());
        out.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        out.extend_from_slice(&unpacked.to_le_bytes());
        out.resize(HEADER, 0);
        out.extend_from_slice(&metadata);
        out.extend_from_slice(&payload);
        if out.len() as u64 > limits::ARCHIVE {
            return Err(Error::TooLarge("file"));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::manifest;

    /// A package of `files`, compressed as asked.
    fn package(files: &[(&str, &[u8])], compression: Compression) -> Vec<u8> {
        let mut b = Builder::new(manifest("hello 1.0"));
        for (path, data) in files {
            b.add(path, data.to_vec()).unwrap();
        }
        b.build(compression).unwrap()
    }

    /// Everything `archive` unpacks: each file's path and contents.
    fn unpacked(archive: &Archive) -> Result<Vec<(String, Vec<u8>)>, Error> {
        let mut files: Vec<(String, Vec<u8>)> = Vec::new();
        let mut ended = 0;
        archive
            .unpack(|piece| -> Result<(), ()> {
                match piece {
                    Piece::Begin(e) => files.push((e.path.clone(), Vec::new())),
                    Piece::Data(d) => files.last_mut().unwrap().1.extend_from_slice(d),
                    Piece::End(e) => {
                        assert_eq!(files.last().unwrap().0, e.path);
                        ended += 1;
                    }
                }
                Ok(())
            })
            .map_err(|e| match e {
                UnpackError::Archive(e) => e,
                UnpackError::Sink(()) => unreachable!(),
            })?;
        assert_eq!(ended, files.len());
        Ok(files)
    }

    #[test]
    fn packages_hold_their_files() {
        let big: Vec<u8> = (0..300_000u32).map(|i| (i * 7 % 251) as u8).collect();
        let files: [(&str, &[u8]); 4] = [
            ("share/hello/empty", b""),
            ("bin/hello", b"\x7fELF..."),
            ("share/hello/big", &big),
            ("apps/hello.app", b"x"),
        ];
        for compression in [Compression::Stored, Compression::Deflate] {
            let bytes = package(&files, compression);
            let archive = Archive::open(&bytes).unwrap();
            assert_eq!(archive.manifest.label(), "hello 1.0");
            assert_eq!(archive.unpacked(), 300_008);
            let paths: Vec<&str> = archive.files.iter().map(|e| e.path.as_str()).collect();
            assert_eq!(paths, ["apps/hello.app", "bin/hello", "share/hello/big", "share/hello/empty"]);
            let out = unpacked(&archive).unwrap();
            assert_eq!(out[2].1, big);
            assert_eq!(out[3], (String::from("share/hello/empty"), Vec::new()));
            assert_eq!(archive.files[1].sha256, Digest::of(b"\x7fELF..."));
        }
        // Compressed, the repetitive file shrinks.
        assert!(package(&files, Compression::Deflate).len() < 10_000);
    }

    #[test]
    fn packages_build_the_same_every_time() {
        let a = package(&[("bin/a", b"1"), ("bin/b", b"2")], Compression::Deflate);
        let b = package(&[("bin/b", b"2"), ("bin/a", b"1")], Compression::Deflate);
        assert_eq!(a, b);
    }

    #[test]
    fn only_paths_under_the_package_directories_are_taken() {
        for good in ["bin/x", "share/a/b/c.txt", "licences/hello/COPYING", "lib/.hidden"] {
            assert_eq!(check_path(good), Ok(()), "{good}");
        }
        let long = alloc::format!("share/{}", "x".repeat(limits::COMPONENT + 1));
        for bad in [
            "", "bin", "bin/", "/bin/x", "bin//x", "bin/./x", "bin/../x", "x/y", ".pkg/x", "home/x", "bin/a\\b",
            "bin/a\nb", &long,
        ] {
            assert!(check_path(bad).is_err(), "{bad:?}");
        }
        let mut b = Builder::new(manifest("hello 1.0"));
        b.add("bin/x", Vec::new()).unwrap();
        assert!(matches!(b.add("bin/x", Vec::new()), Err(Error::Path(_, "is added twice"))));
        b.add("bin/x/y", Vec::new()).unwrap();
        assert!(matches!(b.build(Compression::Stored), Err(Error::Path(p, _)) if p == "bin/x/y"));
    }

    /// `bytes` with the metadata's JSON replaced by `json` (lengths
    /// mended).
    fn with_metadata(bytes: &[u8], json: &str) -> Vec<u8> {
        let old = u64_at(bytes, 16) as usize;
        let mut out = bytes[..HEADER].to_vec();
        out[16..24].copy_from_slice(&(json.len() as u64).to_le_bytes());
        out.extend_from_slice(json.as_bytes());
        out.extend_from_slice(&bytes[HEADER + old..]);
        out
    }

    #[test]
    fn damaged_packages_are_refused_before_anything_is_unpacked() {
        let good = package(&[("bin/a", b"aaaa"), ("bin/b", b"bbbb")], Compression::Stored);
        let json = core::str::from_utf8(&good[HEADER..HEADER + u64_at(&good, 16) as usize]).unwrap().to_string();
        let mut cases: Vec<(Vec<u8>, &str)> = Vec::new();
        cases.push((b"VEDAPK".to_vec(), "cut short"));
        cases.push((
            b"PK\x03\x04 a zip file, not a package at all........................................".to_vec(),
            "not a Veda package",
        ));
        let mut c = good.clone();
        c[8] = 2;
        cases.push((c, "format 2"));
        let mut c = good.clone();
        c[12] = 9;
        cases.push((c, "compressed in a way"));
        let mut c = good.clone();
        c.push(0);
        cases.push((c, "cut short"));
        cases.push((good[..good.len() - 1].to_vec(), "cut short"));
        let mut c = good.clone();
        c[50] = 1;
        cases.push((c, "leaves zero"));
        let mut c = good.clone();
        c[32] += 1;
        cases.push((c, "do not add up"));
        cases.push((with_metadata(&good, &json.replace("bin/b", "../b")), "'../b'"));
        cases.push((with_metadata(&good, &json.replace("bin/b", "bin/a")), "out of order"));
        cases.push((with_metadata(&good, &json.replace("\"files\"", "\"scripts\":[],\"files\"")), "not valid"));
        cases.push((with_metadata(&good, "{"), "not valid"));
        for (bytes, why) in cases {
            let e = Archive::open(&bytes).unwrap_err();
            assert!(e.to_string().contains(why), "{why}: {e}");
        }
    }

    #[test]
    fn damaged_contents_are_caught_as_they_unpack() {
        // A byte changed in a stored file: its SHA-256 no longer matches.
        let mut stored = package(&[("bin/a", b"aaaa"), ("bin/b", b"bbbb")], Compression::Stored);
        let at = stored.len() - 2;
        stored[at] ^= 1;
        let e = unpacked(&Archive::open(&stored).unwrap()).unwrap_err();
        assert!(e.to_string().contains("bin/b is not what was packed"), "{e}");
        // A deflate stream cut short, or followed by more.
        let deflated = package(&[("bin/a", &[7u8; 5000])], Compression::Deflate);
        let mut short = deflated[..deflated.len() - 3].to_vec();
        let payload = (u64_at(&short, 24) - 3).to_le_bytes();
        short[24..32].copy_from_slice(&payload);
        assert!(matches!(unpacked(&Archive::open(&short).unwrap()), Err(Error::Corrupt(_))));
        let mut long = deflated.clone();
        long.extend_from_slice(b"more");
        let payload = (u64_at(&long, 24) + 4).to_le_bytes();
        long[24..32].copy_from_slice(&payload);
        assert!(matches!(unpacked(&Archive::open(&long).unwrap()), Err(Error::Corrupt(_))));
    }

    #[test]
    fn a_stream_that_says_less_than_it_holds_is_cut_off() {
        // The list says a 10-byte file; the stream holds a megabyte.
        let mut b = Builder::new(manifest("hello 1.0"));
        b.add("bin/a", vec![0u8; 1 << 20]).unwrap();
        let bytes = b.build(Compression::Deflate).unwrap();
        let json = core::str::from_utf8(&bytes[HEADER..HEADER + u64_at(&bytes, 16) as usize]).unwrap().to_string();
        let mut bomb = with_metadata(&bytes, &json.replace("1048576", "10"));
        bomb[32..40].copy_from_slice(&10u64.to_le_bytes());
        let archive = Archive::open(&bomb).unwrap();
        let mut seen = 0;
        let r = archive.unpack(|piece| -> Result<(), ()> {
            if let Piece::Data(d) = piece {
                seen += d.len();
            }
            Ok(())
        });
        assert!(matches!(r, Err(UnpackError::Archive(Error::Corrupt(_)))));
        assert!(seen <= 10, "{seen} bytes given out");
    }

    #[test]
    fn what_the_sink_refuses_stops_unpacking() {
        let bytes = package(&[("bin/a", b"aaaa"), ("bin/b", b"bbbb")], Compression::Deflate);
        let archive = Archive::open(&bytes).unwrap();
        let mut begun = Vec::new();
        let r = archive.unpack(|piece| match piece {
            Piece::Begin(e) if e.path == "bin/b" => Err("no room"),
            Piece::Begin(e) => {
                begun.push(e.path.clone());
                Ok(())
            }
            _ => Ok(()),
        });
        assert_eq!(r, Err(UnpackError::Sink("no room")));
        assert_eq!(begun, ["bin/a"]);
    }
}
