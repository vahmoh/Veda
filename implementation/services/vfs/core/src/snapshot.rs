// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The snapshot of `/home` that the home disk keeps.
//!
//! A snapshot is a sequence of records, little-endian, strings as their
//! length (`u32`) and bytes:
//!
//! * for each sample the system image has ([`R_KNOWN_SAMPLE`]), its path
//!   there, so that samples the user deleted stay deleted, while samples
//!   added to the system image later still appear;
//! * then every directory and file of `/home`, parents before children: a
//!   directory ([`R_DIR`]: its path, such as `home/user/Music`, and
//!   modification time), a file ([`R_FILE`]: path, time, and its contents
//!   as a `u32` length and bytes), or a file that still has a sample's
//!   contents ([`R_SAMPLE_FILE`]: path, time, and the sample's path in the
//!   system image, rather than a copy).
//!
//! Snapshots are written to a [`Sink`] and read from a [`Source`] as a
//! stream, never whole in memory: a snapshot is as large as the home
//! directory, which may take most of the memory itself. Both walk the tree
//! with stacks of their own (see [`crate::tree`]).

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

use crate::HOME;
use crate::tree::{self, Data, Kind, MAX_NAME, Node, NodeId, Tree};

/// A directory.
pub const R_DIR: u8 = 1;
/// A file and its contents.
pub const R_FILE: u8 = 2;
/// A file whose contents are a sample in the system image.
pub const R_SAMPLE_FILE: u8 = 3;
/// A sample that existed when the snapshot was taken.
pub const R_KNOWN_SAMPLE: u8 = 4;

/// The sample files in the system image (`samples/...`), which seed the
/// user's home directory.
pub struct Samples {
    /// Address of a sample's contents → its path in the system image.
    by_ptr: BTreeMap<usize, &'static str>,
    /// Path in the system image → contents.
    by_path: BTreeMap<&'static str, &'static [u8]>,
}

impl Samples {
    /// The samples among the system image's files (path, contents).
    pub fn new(files: impl IntoIterator<Item = (&'static str, &'static [u8])>) -> Samples {
        let mut by_ptr = BTreeMap::new();
        let mut by_path = BTreeMap::new();
        for (path, data) in files.into_iter().filter(|(path, _)| path.starts_with("samples/")) {
            if !data.is_empty() {
                by_ptr.insert(data.as_ptr() as usize, path);
            }
            by_path.insert(path, data);
        }
        Samples { by_ptr, by_path }
    }

    /// The sample `d` still is (an unmodified copy, which snapshots store as
    /// a reference): its path in the system image.
    pub fn source_of(&self, d: &Data) -> Option<&'static str> {
        match d {
            Data::Static(s) => self.by_ptr.get(&(s.as_ptr() as usize)).copied(),
            Data::Owned(_) => None,
        }
    }

    /// Where a sample goes in the home directory.
    fn home_path(src: &str) -> Option<String> {
        src.strip_prefix("samples/").map(|rest| format!("home/user/{rest}"))
    }

    /// Copies samples into the home directory: all of them, or those not in
    /// `known`. Existing files are never replaced.
    pub fn seed(&self, ram: &mut Tree, known: Option<&BTreeSet<String>>) {
        for (&src, &data) in &self.by_path {
            if known.is_some_and(|k| k.contains(src)) {
                continue;
            }
            if let Some(path) = Samples::home_path(src)
                && let Ok(comps) = tree::components(&path)
            {
                let _ = ram.add_file(&comps, Data::Static(data));
            }
        }
    }
}

/// Where snapshot records go.
pub trait Sink {
    fn bytes(&mut self, b: &[u8]);
    fn u8(&mut self, v: u8) {
        self.bytes(&[v]);
    }
    fn u32(&mut self, v: u32) {
        self.bytes(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.bytes(&v.to_le_bytes());
    }
    fn str(&mut self, s: &str) {
        self.u32(s.len() as u32);
        self.bytes(s.as_bytes());
    }
}

/// Counts the bytes of a snapshot without keeping them, so that the size
/// and the snapshot itself can never disagree.
pub struct Counter(pub usize);

impl Sink for Counter {
    fn bytes(&mut self, b: &[u8]) {
        self.0 += b.len();
    }
}

impl Sink for Vec<u8> {
    fn bytes(&mut self, b: &[u8]) {
        self.extend_from_slice(b);
    }
}

/// Writes the snapshot of `/home` in `ram`.
pub fn write(ram: &Tree, samples: &Samples, out: &mut impl Sink) {
    for src in samples.by_path.keys() {
        out.u8(R_KNOWN_SAMPLE);
        out.str(src);
    }
    let Ok(home) = ram.lookup(&[HOME]) else { return };
    let mut path = String::from(HOME);
    record(ram, home, &path, samples, out);
    // The directories being written, each with the entries still to come
    // and the length of its path.
    let mut open = Vec::new();
    if let Some(Node { kind: Kind::Dir(children), .. }) = ram.node(home) {
        open.push((children.iter(), path.len()));
    }
    while let Some((children, len)) = open.last_mut() {
        let Some((name, &id)) = children.next() else {
            open.pop();
            continue;
        };
        path.truncate(*len);
        path.push('/');
        path.push_str(name);
        record(ram, id, &path, samples, out);
        if let Some(Node { kind: Kind::Dir(children), .. }) = ram.node(id) {
            open.push((children.iter(), path.len()));
        }
    }
}

/// Writes the record of node `id`, whose path is `path`.
fn record(ram: &Tree, id: NodeId, path: &str, samples: &Samples, out: &mut impl Sink) {
    let Some(node) = ram.node(id) else { return };
    match &node.kind {
        Kind::Dir(_) => {
            out.u8(R_DIR);
            out.str(path);
            out.u64(node.modified);
        }
        Kind::File(d) => match samples.source_of(d) {
            Some(src) => {
                out.u8(R_SAMPLE_FILE);
                out.str(path);
                out.u64(node.modified);
                out.str(src);
            }
            None => {
                out.u8(R_FILE);
                out.str(path);
                out.u64(node.modified);
                out.u32(d.bytes().len() as u32);
                out.bytes(d.bytes());
            }
        },
    }
}

/// The size of [`write`]'s snapshot, found without copying any data.
pub fn len(ram: &Tree, samples: &Samples) -> usize {
    let mut c = Counter(0);
    write(ram, samples, &mut c);
    c.0
}

/// The length of the path records give the file or directory at `comps`
/// (`home/user/x` for `["home", "user", "x"]`).
pub fn path_len(comps: &[&str]) -> usize {
    tree::path_len(comps).saturating_sub(1)
}

/// The bytes of the record of a directory (`None`) or of a file whose
/// contents take `stored` bytes (its own, or its sample's path), at a path
/// `path_len` bytes long.
pub fn record_len(path_len: usize, stored: Option<usize>) -> usize {
    // The kind, the path, the time; then a file's contents, or its sample's
    // path.
    1 + 4 + path_len + 8 + stored.map_or(0, |n| 4 + n)
}

/// What the record of a file with contents `d` stores of them: its sample's
/// path, or the contents themselves.
pub fn stored_len(d: &Data, samples: &Samples) -> usize {
    samples.source_of(d).map_or(d.bytes().len(), str::len)
}

/// The bytes of the records of the subtree of `id` if it were where the
/// path `path_len` bytes long is (as records write paths: see
/// [`path_len`]).
pub fn subtree_len(ram: &Tree, id: NodeId, path_len: usize, samples: &Samples) -> usize {
    let mut total = 0usize;
    let mut todo = vec![(id, path_len)];
    while let Some((n, len)) = todo.pop() {
        let Some(node) = ram.node(n) else { continue };
        match &node.kind {
            Kind::Dir(children) => {
                total += record_len(len, None);
                todo.extend(children.iter().map(|(name, &c)| (c, len + 1 + name.len())));
            }
            Kind::File(d) => total += record_len(len, Some(stored_len(d, samples))),
        }
    }
    total
}

/// Where snapshot records come from.
pub trait Source {
    /// Fills `buf` with the next bytes; `false` if they cannot be had.
    fn read(&mut self, buf: &mut [u8]) -> bool;
}

impl Source for &[u8] {
    fn read(&mut self, buf: &mut [u8]) -> bool {
        let Some((head, rest)) = self.split_at_checked(buf.len()) else { return false };
        buf.copy_from_slice(head);
        *self = rest;
        true
    }
}

/// Why a snapshot could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadError {
    /// It ends inside a record, or its disk failed to give it.
    Truncated,
    /// A record is not one this file system writes.
    Malformed,
    /// Its contents do not fit in memory.
    NoMemory,
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ReadError::Truncated => "it is cut short",
            ReadError::Malformed => "it holds a record of no known kind",
            ReadError::NoMemory => "it does not fit in memory",
        })
    }
}

/// Reads `len` bytes of records from `src`.
struct Reader<'a, S> {
    src: &'a mut S,
    left: u64,
}

impl<S: Source> Reader<'_, S> {
    fn fill(&mut self, buf: &mut [u8]) -> Result<(), ReadError> {
        if buf.len() as u64 > self.left || !self.src.read(buf) {
            return Err(ReadError::Truncated);
        }
        self.left -= buf.len() as u64;
        Ok(())
    }

    fn u8(&mut self) -> Result<u8, ReadError> {
        let mut b = [0; 1];
        self.fill(&mut b)?;
        Ok(b[0])
    }

    fn u32(&mut self) -> Result<u32, ReadError> {
        let mut b = [0; 4];
        self.fill(&mut b)?;
        Ok(u32::from_le_bytes(b))
    }

    fn u64(&mut self) -> Result<u64, ReadError> {
        let mut b = [0; 8];
        self.fill(&mut b)?;
        Ok(u64::from_le_bytes(b))
    }

    /// `len` bytes, in memory of their own.
    fn bytes(&mut self, len: u32) -> Result<Vec<u8>, ReadError> {
        if u64::from(len) > self.left {
            return Err(ReadError::Truncated);
        }
        let mut v = Vec::new();
        v.try_reserve_exact(len as usize).map_err(|_| ReadError::NoMemory)?;
        v.resize(len as usize, 0);
        self.fill(&mut v)?;
        Ok(v)
    }

    fn string(&mut self) -> Result<String, ReadError> {
        let len = self.u32()?;
        String::from_utf8(self.bytes(len)?).map_err(|_| ReadError::Malformed)
    }
}

/// The components of a record's path, which names something in `/home`.
fn record_components(path: &str) -> Option<Vec<&str>> {
    let comps: Vec<&str> = path.split('/').collect();
    let named = |c: &&str| !c.is_empty() && *c != "." && *c != ".." && c.len() <= MAX_NAME;
    (comps.first() == Some(&HOME) && comps.iter().all(named)).then_some(comps)
}

/// Rebuilds `/home` in a RAM file system that has none yet from the `len`
/// bytes of snapshot that `src` gives, then adds the samples the snapshot
/// did not know.
pub fn read(src: &mut impl Source, len: u64, ram: &mut Tree, samples: &Samples) -> Result<(), ReadError> {
    let mut r = Reader { src, left: len };
    let mut known = BTreeSet::new();
    while r.left > 0 {
        let kind = r.u8()?;
        if kind == R_KNOWN_SAMPLE {
            known.insert(r.string()?);
            continue;
        }
        let path = r.string()?;
        let modified = r.u64()?;
        let comps = record_components(&path).ok_or(ReadError::Malformed)?;
        // What cannot go where its record says (a file in the way of a
        // directory) is left out, as is a sample no longer in the system
        // image.
        let id = match kind {
            R_DIR => ram.mkdir_all(&comps).ok(),
            R_FILE => {
                let len = r.u32()?;
                let bytes = r.bytes(len)?;
                ram.add_file(&comps, Data::owned(bytes)).ok()
            }
            R_SAMPLE_FILE => {
                let src = r.string()?;
                samples.by_path.get(src.as_str()).and_then(|&d| ram.add_file(&comps, Data::Static(d)).ok())
            }
            _ => return Err(ReadError::Malformed),
        };
        if let Some(n) = id.and_then(|id| ram.node_mut(id)) {
            n.modified = modified;
        }
    }
    samples.seed(ram, Some(&known));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use core::sync::atomic::{AtomicU64, Ordering};

    fn clock() -> u64 {
        static NOW: AtomicU64 = AtomicU64::new(1_000);
        NOW.fetch_add(1, Ordering::Relaxed)
    }

    static SONG: &[u8] = b"a song";
    static PHOTO: &[u8] = b"a photo";

    fn samples() -> Samples {
        Samples::new([("samples/Music/song.qoa", SONG), ("samples/Pictures/photo.png", PHOTO), ("bin/init", &b"x"[..])])
    }

    fn fresh() -> Tree {
        let mut t = Tree::new(false, 1, clock);
        t.mkdir_all(&["tmp"]).unwrap();
        t.mkdir_all(&["home", "user"]).unwrap();
        t
    }

    /// Every path of `/home` with what it holds (`None` for a directory)
    /// and its time.
    fn contents(t: &Tree) -> Vec<(String, Option<Vec<u8>>, u64)> {
        let mut out = Vec::new();
        let mut todo = vec![(String::from("home"), t.lookup(&["home"]).unwrap())];
        while let Some((path, id)) = todo.pop() {
            let node = t.node(id).unwrap();
            match &node.kind {
                Kind::Dir(c) => {
                    out.push((path.clone(), None, node.modified));
                    todo.extend(c.iter().map(|(name, &child)| (format!("{path}/{name}"), child)));
                }
                Kind::File(d) => out.push((path, Some(d.bytes().to_vec()), node.modified)),
            }
        }
        out.sort();
        out
    }

    fn home(samples: &Samples) -> Tree {
        let mut t = fresh();
        samples.seed(&mut t, None);
        t.create(&["home", "user", "notes.txt"], Kind::File(Data::owned(b"hello".to_vec()))).unwrap();
        t.create(&["home", "user", "Empty"], Kind::dir()).unwrap();
        t.mkdir_all(&["home", ".private", "agent"]).unwrap();
        t.add_file(&["home", ".private", "agent", "key"], Data::owned(vec![0, 1, 2])).unwrap();
        t.add_file(&["tmp", "scratch"], Data::owned(b"not kept".to_vec())).unwrap();
        // A changed sample is kept whole.
        let photo = t.lookup(&["home", "user", "Pictures", "photo.png"]).unwrap();
        if let Some(Node { kind: Kind::File(d), .. }) = t.node_mut(photo) {
            d.make_mut().unwrap().extend_from_slice(b", edited");
        }
        t
    }

    #[test]
    fn snapshots_round_trip() {
        let s = samples();
        let t = home(&s);
        let mut bytes = Vec::new();
        write(&t, &s, &mut bytes);
        assert_eq!(bytes.len(), len(&t, &s));
        let mut back = fresh();
        read(&mut &bytes[..], bytes.len() as u64, &mut back, &s).unwrap();
        assert_eq!(contents(&back), contents(&t));
        assert!(back.lookup(&["tmp", "scratch"]).is_err());
        // An unchanged sample is a reference into the system image again.
        let song = back.lookup(&["home", "user", "Music", "song.qoa"]).unwrap();
        assert!(matches!(&back.node(song).unwrap().kind, Kind::File(d) if s.source_of(d).is_some()));
        // Written again, it is the same.
        let mut again = Vec::new();
        write(&back, &s, &mut again);
        assert_eq!(again, bytes);
    }

    #[test]
    fn samples_the_user_deleted_stay_deleted() {
        let s = samples();
        let mut t = home(&s);
        let song = t.unlink(&["home", "user", "Music", "song.qoa"]).unwrap();
        t.forget(song);
        let mut bytes = Vec::new();
        write(&t, &s, &mut bytes);
        // A sample added to the system image since appears.
        static NEW: &[u8] = b"new";
        let later = Samples::new([
            ("samples/Music/song.qoa", SONG),
            ("samples/Pictures/photo.png", PHOTO),
            ("samples/new.txt", NEW),
        ]);
        let mut back = fresh();
        read(&mut &bytes[..], bytes.len() as u64, &mut back, &later).unwrap();
        assert!(back.lookup(&["home", "user", "Music", "song.qoa"]).is_err());
        assert!(back.lookup(&["home", "user", "new.txt"]).is_ok());
    }

    #[test]
    fn subtrees_know_their_size_anywhere() {
        let s = samples();
        let t = home(&s);
        let root = t.lookup(&["home"]).unwrap();
        let known: usize = s.by_path.keys().map(|p| 1 + 4 + p.len()).sum();
        assert_eq!(known + subtree_len(&t, root, "home".len(), &s), len(&t, &s));
        // Moved to a longer path, every record's path is longer.
        let user = t.lookup(&["home", "user"]).unwrap();
        let nodes = t.measure(user).nodes;
        assert_eq!(path_len(&["home", "user"]), "home/user".len());
        let at = subtree_len(&t, user, path_len(&["home", "user"]), &s);
        assert_eq!(subtree_len(&t, user, "home/someone".len(), &s), at + 3 * nodes);
        // A new file's record is as large as a record of it will be.
        let mut t2 = home(&s);
        let before = len(&t2, &s);
        t2.create(&["home", "user", "new.txt"], Kind::File(Data::owned(b"12345".to_vec()))).unwrap();
        assert_eq!(len(&t2, &s), before + record_len(path_len(&["home", "user", "new.txt"]), Some(5)));
        t2.create(&["home", "user", "New"], Kind::dir()).unwrap();
        let song = t2.lookup(&["home", "user", "Music", "song.qoa"]).unwrap();
        let Some(Node { kind: Kind::File(d), .. }) = t2.node(song) else { unreachable!() };
        assert_eq!(stored_len(d, &s), "samples/Music/song.qoa".len());
    }

    #[test]
    fn deep_trees_are_written_without_recursing() {
        // Far deeper than a recursive walk could follow on a thread's
        // stack: measured (each record's path is written whole, so a
        // snapshot of it would be large)...
        let s = Samples::new([]);
        let mut t = fresh();
        let mut comps = vec!["home"];
        comps.extend(core::iter::repeat_n("d", 200_000));
        t.mkdir_all(&comps).unwrap();
        let home = t.lookup(&["home"]).unwrap();
        assert_eq!(len(&t, &s), subtree_len(&t, home, "home".len(), &s));
        // ... and, less deep, written and read back.
        let mut t = fresh();
        t.mkdir_all(&comps[..5_000]).unwrap();
        let mut bytes = Vec::new();
        write(&t, &s, &mut bytes);
        let mut back = fresh();
        read(&mut &bytes[..], bytes.len() as u64, &mut back, &s).unwrap();
        assert_eq!(contents(&back), contents(&t));
    }

    #[test]
    fn damaged_snapshots_are_refused() {
        let s = samples();
        let t = home(&s);
        let mut bytes = Vec::new();
        write(&t, &s, &mut bytes);
        // Cut short inside a record (the last is the five bytes of
        // `notes.txt`).
        for cut in [1, 5, bytes.len() - 3, bytes.len() - 1] {
            let mut back = fresh();
            assert_eq!(read(&mut &bytes[..cut], cut as u64, &mut back, &s), Err(ReadError::Truncated), "{cut}");
            let mut back = fresh();
            assert_eq!(read(&mut &bytes[..], cut as u64, &mut back, &s), Err(ReadError::Truncated), "{cut}");
        }
        let record = |kind: u8, path: &str| {
            let mut v = Vec::new();
            v.u8(kind);
            v.str(path);
            v.u64(0);
            v
        };
        // Paths outside /home, or that leave it, and kinds of no record.
        for bad in [record(R_DIR, "tmp/x"), record(R_DIR, "home/../tmp"), record(R_DIR, "home//x"), record(9, "home")] {
            let mut back = fresh();
            assert_eq!(read(&mut &bad[..], bad.len() as u64, &mut back, &s), Err(ReadError::Malformed));
            assert!(back.lookup(&["tmp", "x"]).is_err());
        }
        // A length beyond the end is refused before anything is allocated.
        let mut huge = record(R_FILE, "home/user/x");
        huge.u32(u32::MAX);
        let mut back = fresh();
        assert_eq!(read(&mut &huge[..], huge.len() as u64, &mut back, &s), Err(ReadError::Truncated));
        let not_utf8 = [R_KNOWN_SAMPLE, 1, 0, 0, 0, 0xFF];
        let mut back = fresh();
        assert_eq!(read(&mut &not_utf8[..], 6, &mut back, &s), Err(ReadError::Malformed));
        assert_eq!(ReadError::NoMemory.to_string(), "it does not fit in memory");
    }
}
