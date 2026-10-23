// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! A convenient client of the VFS service: whole-file reads (copied,
//! bounded or mapped) and writes, appends, listings, copies, moves and
//! removals of whole trees, and how full a file system is.
//!
//! [`Fs`] wraps `proto::vfs::Client` and flattens its two error layers
//! (IPC and file system) into one [`Error`]. Every call has a deadline (see
//! [`Fs::connect`]): a VFS that stops answering fails calls rather than
//! freezes the program.
//!
//! Walks over trees ([`Fs::copy_with`], [`Fs::move_to_with`],
//! [`Fs::remove_all_with`], [`Fs::measure`]) keep a stack of their own,
//! however deep the tree, list folders a page at a time, and tell a
//! [`Hook`] of every item they do, which can show their progress and stop
//! them. Files are copied by the VFS, which shares their contents between
//! the copies until one changes, so a copy never holds a file in memory.

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;
use core::ops::{Add, ControlFlow, Deref};

use ipc::{Bytes, IpcError};
use proto::fs::{MAX_IO, open_flags, vfs};
use rt::object::Vmo;
use rt::vm::Mapping;

pub use proto::fs::{DirEntry, DirPage, FsError, Space, Stat};

use crate::path::{is_within, join, same_volume, unique_name};

/// How long a call waits for the VFS's answer before it fails with
/// [`Error::Ipc`]. The VFS answers its clients one call at a time, so a
/// call can wait behind another program's whole-file read or write, or a
/// save of the home directory to its disk, which for large ones take
/// seconds under emulation; a VFS silent for this long is stuck.
const TIMEOUT_NS: u64 = 60_000_000_000;
/// What a whole-file read or write adds to its deadline for every MiB it
/// moves: the VFS copies the data once and the kernel commits its pages,
/// at no less than 8 MiB/s even under emulation.
const NS_PER_MIB: u64 = 125_000_000;
/// The deadline of [`Fs::sync`], which writes all of `/home` to the home
/// disk.
const SYNC_TIMEOUT_NS: u64 = 600_000_000_000;

/// A file system operation failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The VFS refused the operation.
    Fs(FsError),
    /// The VFS stopped answering.
    Ipc,
    /// There is no VFS service.
    Unavailable,
    /// A folder cannot be copied or moved into itself.
    IntoItself,
    /// The item is in the Trash already.
    InTrash,
    /// The item holds the Trash, so it cannot go there.
    HoldsTrash,
    /// Where an item in the Trash was is not known.
    UnknownOrigin,
    /// The file is larger than what was asked for, or than memory holds.
    TooLarge,
    /// A [`Hook`] stopped the walk.
    Cancelled,
}

impl Error {
    /// True if the operation failed because the file system is full.
    pub fn is_no_space(&self) -> bool {
        *self == Error::Fs(FsError::NoSpace)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Fs(FsError::NoSpace) => f.write_str("there is not enough free space"),
            Error::Fs(e) => write!(f, "{e}"),
            Error::Ipc => f.write_str("the file system service is not responding"),
            Error::Unavailable => f.write_str("the file system service is unavailable"),
            Error::IntoItself => f.write_str("a folder cannot be copied or moved into itself"),
            Error::InTrash => f.write_str("it is in the Trash already"),
            Error::HoldsTrash => f.write_str("the Trash is inside it"),
            Error::UnknownOrigin => f.write_str("where it was deleted from is not known"),
            Error::TooLarge => f.write_str("the file is too large"),
            Error::Cancelled => f.write_str("it was stopped"),
        }
    }
}

impl From<FsError> for Error {
    fn from(e: FsError) -> Self {
        Error::Fs(e)
    }
}

pub type Result<T> = core::result::Result<T, Error>;

/// Flattens an IPC result carrying a file system result.
fn flat<T>(r: core::result::Result<core::result::Result<T, FsError>, IpcError>) -> Result<T> {
    match r {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(Error::Fs(e)),
        // A request too large to send (a path longer than a message), or
        // one the VFS could not take, is one it would not do.
        Err(IpcError::TooLarge | IpcError::Refused(_)) => Err(Error::Fs(FsError::Invalid)),
        Err(_) => Err(Error::Ipc),
    }
}

/// The deadline of a call that moves `bytes` bytes.
fn transfer_timeout(bytes: u64) -> u64 {
    TIMEOUT_NS.saturating_add((bytes >> 20).saturating_mul(NS_PER_MIB))
}

/// How far a walk over a tree has got: what it has copied, moved, removed
/// or counted so far.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Progress {
    /// Files and folders.
    pub items: u64,
    /// Bytes of the files.
    pub bytes: u64,
}

impl Add for Progress {
    type Output = Progress;

    fn add(self, other: Progress) -> Progress {
        Progress { items: self.items.saturating_add(other.items), bytes: self.bytes.saturating_add(other.bytes) }
    }
}

/// What a walk tells of every item it has done: the item's path, and the
/// progress with the item counted. `ControlFlow::Break` stops the walk,
/// which then fails with [`Error::Cancelled`]; what it did so far stays
/// done (a copy stopped halfway leaves what it copied).
pub type Hook<'a> = &'a mut dyn FnMut(&str, Progress) -> ControlFlow<()>;

/// A hook that lets a walk go on to the end.
fn to_the_end(_: &str, _: Progress) -> ControlFlow<()> {
    ControlFlow::Continue(())
}

/// Counts an item done and tells the hook.
fn done(
    hook: &mut dyn FnMut(&str, Progress) -> ControlFlow<()>,
    path: &str,
    p: &mut Progress,
    bytes: u64,
) -> Result<()> {
    *p = *p + Progress { items: 1, bytes };
    match hook(path, *p) {
        ControlFlow::Continue(()) => Ok(()),
        ControlFlow::Break(()) => Err(Error::Cancelled),
    }
}

/// A whole file mapped into memory, read-only ([`Fs::map`]).
pub struct MappedFile {
    mapping: Mapping,
    len: usize,
}

impl MappedFile {
    /// Maps the first `len` bytes of `vmo`, which the VFS made for this
    /// read alone.
    fn new(vmo: Vmo, len: u64) -> Result<MappedFile> {
        let len = usize::try_from(len).map_err(|_| Error::TooLarge)?;
        if vmo.size().map_err(|_| Error::Fs(FsError::Io))? < len {
            return Err(Error::Fs(FsError::Io));
        }
        let mapping = Mapping::new(vmo, len.max(1), abi::map_flags::READ).map_err(|_| Error::TooLarge)?;
        Ok(MappedFile { mapping, len })
    }

    /// The file's bytes.
    pub fn bytes(&self) -> &[u8] {
        // SAFETY: the mapping is read-only, of a VMO the VFS made for this
        // one read and handed over with no other handle to it, so nothing
        // changes its bytes; it lives as long as `self`, and is at least
        // `len` bytes long (`MappedFile::new` checks the VMO's size).
        unsafe { &self.mapping.as_slice()[..self.len] }
    }

    /// The file's length in bytes.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the file is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl Deref for MappedFile {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        self.bytes()
    }
}

/// A connection to the VFS. Every call fails with [`Error::Unavailable`] if
/// the service could not be reached.
pub struct Fs {
    client: Option<vfs::Client>,
}

impl Fs {
    /// Connects to the VFS service. A call fails with [`Error::Ipc`] when
    /// the VFS has not answered it within a minute, plus, for whole-file
    /// reads and writes, time for their size.
    pub fn connect() -> Fs {
        let client = proto::connect(vfs::NAME).ok().map(vfs::Client::new);
        if let Some(c) = &client {
            c.set_timeout(TIMEOUT_NS);
        }
        Fs { client }
    }

    /// A connection to nothing: every call fails with
    /// [`Error::Unavailable`].
    pub fn unavailable() -> Fs {
        Fs { client: None }
    }

    /// The underlying protocol client, if connected.
    pub fn client(&self) -> Option<&vfs::Client> {
        self.client.as_ref()
    }

    fn c(&self) -> Result<&vfs::Client> {
        self.client.as_ref().ok_or(Error::Unavailable)
    }

    /// Makes a call that may take `ns` nanoseconds rather than the usual
    /// deadline.
    fn call_within<T>(
        &self,
        ns: u64,
        call: impl FnOnce(&vfs::Client) -> core::result::Result<core::result::Result<T, FsError>, IpcError>,
    ) -> Result<T> {
        let c = self.c()?;
        c.set_timeout(ns);
        let r = call(c);
        c.set_timeout(TIMEOUT_NS);
        flat(r)
    }

    pub fn stat(&self, path: &str) -> Result<Stat> {
        flat(self.c()?.stat(path.into()))
    }

    pub fn exists(&self, path: &str) -> bool {
        self.stat(path).is_ok()
    }

    pub fn is_dir(&self, path: &str) -> bool {
        self.stat(path).is_ok_and(|s| s.is_dir)
    }

    /// The entries of a directory (folders first, then by name, case
    /// aside), however many: the VFS lists them a page at a time.
    pub fn read_dir(&self, path: &str) -> Result<Vec<DirEntry>> {
        let mut entries = flat(self.c()?.read_dir(path.into()))?;
        entries.sort_by_cached_key(|e| (!e.is_dir, e.name.to_lowercase()));
        Ok(entries)
    }

    /// A page of a directory's entries: at most `max` (0: as many as a page
    /// holds) of those whose names come after `after` in the order of their
    /// bytes (from the first for `""`). For showing a large folder as it
    /// comes; the next page is the one after the last name of this one.
    pub fn read_dir_page(&self, path: &str, after: &str, max: u32) -> Result<DirPage> {
        flat(self.c()?.read_dir_page(path.into(), after.into(), max))
    }

    /// How full the file system holding `path` is.
    pub fn space(&self, path: &str) -> Result<Space> {
        flat(self.c()?.space(path.into()))
    }

    pub fn mkdir(&self, path: &str) -> Result<()> {
        flat(self.c()?.mkdir(path.into()))
    }

    /// Creates `path` and every missing parent.
    pub fn mkdir_all(&self, path: &str) -> Result<()> {
        let mut cur = String::new();
        for comp in path.split('/').filter(|c| !c.is_empty()) {
            cur.push('/');
            cur.push_str(comp);
            match self.stat(&cur) {
                Ok(s) if s.is_dir => {}
                Ok(_) => return Err(Error::Fs(FsError::NotDir)),
                Err(Error::Fs(FsError::NotFound)) => self.mkdir(&cur)?,
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Removes a file or an empty directory.
    pub fn remove(&self, path: &str) -> Result<()> {
        flat(self.c()?.remove(path.into()))
    }

    /// Removes a file, or a directory with everything in it.
    pub fn remove_all(&self, path: &str) -> Result<()> {
        self.remove_all_with(path, &mut to_the_end).map(|_| ())
    }

    /// Removes a file, or a directory with everything in it (each folder's
    /// contents before it), telling `hook` of everything removed.
    pub fn remove_all_with(&self, path: &str, hook: Hook) -> Result<Progress> {
        remove_tree(self, path, hook)
    }

    /// Counts the items and bytes of a file, or of a folder with everything
    /// in it, telling `hook` of each (for knowing how far a copy or removal
    /// of it has to go).
    pub fn measure(&self, path: &str, hook: Hook) -> Result<Progress> {
        measure_tree(self, path, hook)
    }

    /// Renames or moves within one file system.
    pub fn rename(&self, from: &str, to: &str) -> Result<()> {
        flat(self.c()?.rename(from.into(), to.into()))
    }

    /// Moves `from` to `to` within one file system in one step, replacing
    /// what is there: a file, or an empty folder when `from` is a folder.
    /// If it fails, both are as they were.
    pub fn replace(&self, from: &str, to: &str) -> Result<()> {
        if to != from && is_within(to, from) {
            return Err(Error::IntoItself);
        }
        flat(self.c()?.replace(from.into(), to.into()))
    }

    /// Reads a whole file (`TooLarge` if memory cannot hold it).
    pub fn read(&self, path: &str) -> Result<Vec<u8>> {
        self.read_limited(path, usize::MAX)
    }

    /// Reads a whole file of at most `max` bytes: `TooLarge`, and nothing
    /// read, for a larger one.
    pub fn read_limited(&self, path: &str, max: usize) -> Result<Vec<u8>> {
        let size = self.stat(path)?.size;
        if size > max as u64 {
            return Err(Error::TooLarge);
        }
        let (vmo, len) = self.call_within(transfer_timeout(size), |c| c.read_file(path.into()))?;
        // It may have grown since.
        if len > max as u64 {
            return Err(Error::TooLarge);
        }
        let len = usize::try_from(len).map_err(|_| Error::TooLarge)?;
        let mut buf = Vec::new();
        buf.try_reserve_exact(len).map_err(|_| Error::TooLarge)?;
        buf.resize(len, 0);
        if len > 0 {
            vmo.read(0, &mut buf).map_err(|_| Error::Fs(FsError::Io))?;
        }
        Ok(buf)
    }

    /// Maps a whole file into memory, read-only, rather than copying it out
    /// of the VFS's answer: for large files read once (images, sounds,
    /// programs).
    pub fn map(&self, path: &str) -> Result<MappedFile> {
        let size = self.stat(path)?.size;
        let (vmo, len) = self.call_within(transfer_timeout(size), |c| c.read_file(path.into()))?;
        MappedFile::new(vmo, len)
    }

    /// Replaces (or creates) a file with `data`.
    pub fn write(&self, path: &str, data: &[u8]) -> Result<()> {
        let vmo = Vmo::create(data.len().max(1)).map_err(|_| Error::Fs(FsError::NoSpace))?;
        if !data.is_empty() {
            vmo.write(0, data).map_err(|_| Error::Fs(FsError::Io))?;
        }
        let len = data.len() as u64;
        self.call_within(transfer_timeout(len), |c| c.write_file(path.into(), vmo, len))
    }

    /// Appends `data` to a file, creating it if needed.
    pub fn append(&self, path: &str, data: &[u8]) -> Result<()> {
        let c = self.c()?;
        let fd = flat(c.open(path.into(), open_flags::WRITE | open_flags::CREATE | open_flags::APPEND))?;
        let mut result = Ok(());
        for chunk in data.chunks(MAX_IO as usize) {
            if let Err(e) = flat(c.write(fd, 0, Bytes(chunk.to_vec()))) {
                result = Err(e);
                break;
            }
        }
        let _ = c.close(fd);
        result
    }

    /// Creates a file with `data`, failing with `Exists` if there is one
    /// already: of two programs creating it at once, one fails.
    pub fn create_new(&self, path: &str, data: &[u8]) -> Result<()> {
        let c = self.c()?;
        let fd = flat(c.open(path.into(), open_flags::WRITE | open_flags::CREATE | open_flags::EXCLUSIVE))?;
        let mut result = Ok(());
        let mut offset = 0;
        for chunk in data.chunks(MAX_IO as usize) {
            if let Err(e) = flat(c.write(fd, offset, Bytes(chunk.to_vec()))) {
                result = Err(e);
                break;
            }
            offset += chunk.len() as u64;
        }
        let _ = c.close(fd);
        result
    }

    /// Creates an empty file, or updates the modification time of an
    /// existing one.
    pub fn touch(&self, path: &str) -> Result<()> {
        let c = self.c()?;
        let fd = flat(c.open(path.into(), open_flags::WRITE | open_flags::CREATE))?;
        let r = flat(c.write(fd, 0, Bytes(Vec::new()))).map(|_| ());
        let _ = c.close(fd);
        r
    }

    /// Writes all changes to persistent storage now.
    pub fn sync(&self) -> Result<()> {
        self.call_within(SYNC_TIMEOUT_NS, |c| c.sync())
    }

    /// Copies the file `from` to `to`, which must not exist yet, in one
    /// step. The VFS shares the contents between the two until either
    /// changes, so this takes neither time nor memory, however large the
    /// file.
    pub fn copy_file(&self, from: &str, to: &str) -> Result<()> {
        flat(self.c()?.copy_file(from.into(), to.into()))
    }

    /// Copies a file, or a directory with everything in it, to `to`, which
    /// must not exist yet.
    pub fn copy(&self, from: &str, to: &str) -> Result<()> {
        self.copy_with(from, to, &mut to_the_end).map(|_| ())
    }

    /// Copies a file, or a directory with everything in it, to `to`, which
    /// must not exist yet, telling `hook` of everything copied (each
    /// folder before its contents).
    pub fn copy_with(&self, from: &str, to: &str, hook: Hook) -> Result<Progress> {
        copy_tree(self, from, to, hook)
    }

    /// Moves `from` to `to`: a rename within one file system, a copy and a
    /// removal across file systems.
    pub fn move_to(&self, from: &str, to: &str) -> Result<()> {
        self.move_to_with(from, to, &mut to_the_end).map(|_| ())
    }

    /// Moves `from` to `to` as [`Fs::move_to`] does, telling `hook` of
    /// what it moved: the item, in one step, or across file systems
    /// everything copied (the original is removed once all is copied).
    pub fn move_to_with(&self, from: &str, to: &str, hook: Hook) -> Result<Progress> {
        if is_within(to, from) {
            return Err(Error::IntoItself);
        }
        match self.rename(from, to) {
            Ok(()) => {
                let mut progress = Progress::default();
                done(hook, to, &mut progress, 0)?;
                Ok(progress)
            }
            Err(Error::Fs(FsError::Invalid)) if !same_volume(from, to) => {
                let progress = self.copy_with(from, to, hook)?;
                self.remove_all(from)?;
                Ok(progress)
            }
            Err(e) => Err(e),
        }
    }

    /// A name for a new item in `dir` based on `base` that is not in use:
    /// `base`, else `stem (2).ext`, `stem (3).ext`, ...
    pub fn unique_name(&self, dir: &str, base: &str) -> String {
        unique_name(base, |name| self.exists(&join(dir, name)))
    }

    /// Gives up, for good, this connection's access to the system services'
    /// private directories (`/home/.private`): from now on the VFS treats
    /// it as an application's. For a service that acts on the user's files
    /// for others (the agent), so that nothing it is led to (a Trash item's
    /// info is a file anyone can write) reaches its own private data.
    pub fn renounce_private(&self) -> Result<()> {
        flat(self.c()?.renounce_private())
    }
}

/// The calls the walks over trees make: a VFS connection's, which tests
/// fake.
trait Calls {
    fn stat(&self, path: &str) -> Result<Stat>;
    /// The page of folder `dir`'s entries after the name `after`.
    fn page(&self, dir: &str, after: &str) -> Result<DirPage>;
    fn mkdir(&self, path: &str) -> Result<()>;
    fn copy_file(&self, from: &str, to: &str) -> Result<()>;
    fn remove(&self, path: &str) -> Result<()>;
}

impl Calls for Fs {
    fn stat(&self, path: &str) -> Result<Stat> {
        Fs::stat(self, path)
    }

    fn page(&self, dir: &str, after: &str) -> Result<DirPage> {
        self.read_dir_page(dir, after, 0)
    }

    fn mkdir(&self, path: &str) -> Result<()> {
        Fs::mkdir(self, path)
    }

    fn copy_file(&self, from: &str, to: &str) -> Result<()> {
        Fs::copy_file(self, from, to)
    }

    fn remove(&self, path: &str) -> Result<()> {
        Fs::remove(self, path)
    }
}

/// Calls `f` with every entry of folder `dir`, a page at a time, in the
/// order of their names' bytes.
fn for_each_entry(c: &impl Calls, dir: &str, mut f: impl FnMut(DirEntry) -> Result<()>) -> Result<()> {
    let mut after = String::new();
    loop {
        let page = c.page(dir, &after)?;
        let Some(last) = page.entries.last().filter(|last| last.name > after) else { return Ok(()) };
        after.clone_from(&last.name);
        for e in page.entries {
            f(e)?;
        }
        if !page.more {
            return Ok(());
        }
    }
}

/// [`Fs::remove_all_with`].
fn remove_tree(c: &impl Calls, path: &str, hook: Hook) -> Result<Progress> {
    let mut progress = Progress::default();
    let st = c.stat(path)?;
    if !st.is_dir {
        c.remove(path)?;
        done(hook, path, &mut progress, st.size)?;
        return Ok(progress);
    }
    // The folders being emptied, innermost last: each with the name its
    // listing goes on after, and whether it is all listed.
    let mut open = vec![(path.to_string(), String::new(), false)];
    while let Some(top) = open.len().checked_sub(1) {
        if open[top].2 {
            let (dir, ..) = open.swap_remove(top);
            c.remove(&dir)?;
            done(hook, &dir, &mut progress, 0)?;
            continue;
        }
        let page = c.page(&open[top].0, &open[top].1)?;
        let (dir, after, listed) = &mut open[top];
        match page.entries.last() {
            Some(last) if last.name > *after => {
                after.clone_from(&last.name);
                *listed = !page.more;
            }
            _ => *listed = true,
        }
        let dir = dir.clone();
        for e in page.entries {
            let p = join(&dir, &e.name);
            if e.is_dir {
                open.push((p, String::new(), false));
            } else {
                c.remove(&p)?;
                done(hook, &p, &mut progress, e.size)?;
            }
        }
    }
    Ok(progress)
}

/// [`Fs::copy_with`].
fn copy_tree(c: &impl Calls, from: &str, to: &str, hook: Hook) -> Result<Progress> {
    if is_within(to, from) {
        return Err(Error::IntoItself);
    }
    let st = c.stat(from)?;
    let mut progress = Progress::default();
    if !st.is_dir {
        c.copy_file(from, to)?;
        done(hook, to, &mut progress, st.size)?;
        return Ok(progress);
    }
    c.mkdir(to)?;
    done(hook, to, &mut progress, 0)?;
    // Folders made whose contents are still to be copied: from where, to
    // where.
    let mut todo = vec![(from.to_string(), to.to_string())];
    while let Some((src, dst)) = todo.pop() {
        for_each_entry(c, &src, |e| {
            let (s, d) = (join(&src, &e.name), join(&dst, &e.name));
            if e.is_dir {
                c.mkdir(&d)?;
                done(hook, &d, &mut progress, 0)?;
                todo.push((s, d));
            } else {
                c.copy_file(&s, &d)?;
                done(hook, &d, &mut progress, e.size)?;
            }
            Ok(())
        })?;
    }
    Ok(progress)
}

/// [`Fs::measure`].
fn measure_tree(c: &impl Calls, path: &str, hook: Hook) -> Result<Progress> {
    let st = c.stat(path)?;
    let mut progress = Progress::default();
    done(hook, path, &mut progress, if st.is_dir { 0 } else { st.size })?;
    let mut todo = if st.is_dir { vec![path.to_string()] } else { Vec::new() };
    while let Some(dir) = todo.pop() {
        for_each_entry(c, &dir, |e| {
            let p = join(&dir, &e.name);
            done(hook, &p, &mut progress, if e.is_dir { 0 } else { e.size })?;
            if e.is_dir {
                todo.push(p);
            }
            Ok(())
        })?;
    }
    Ok(progress)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path::{file_name, parent};
    use alloc::collections::BTreeMap;
    use alloc::string::ToString;
    use core::cell::RefCell;
    use core::ops::Bound;

    /// Entries in a page of the fake's listings: few, so that walks page.
    const PAGE: usize = 3;

    /// A file system in memory: (folder, name) → a file's size, or `None`
    /// for a folder. The root is always there.
    #[derive(Default)]
    struct Fake(RefCell<BTreeMap<(String, String), Option<u64>>>);

    fn key(path: &str) -> (String, String) {
        (parent(path).to_string(), file_name(path).to_string())
    }

    impl Fake {
        fn get(&self, path: &str) -> Option<Option<u64>> {
            if path == "/" { Some(None) } else { self.0.borrow().get(&key(path)).copied() }
        }

        fn add(&self, path: &str, size: Option<u64>) -> Result<()> {
            self.0.borrow_mut().insert(key(path), size);
            Ok(())
        }

        /// Every path below `dir`, with what it is.
        fn below(&self, dir: &str) -> Vec<(String, Option<u64>)> {
            let mut out: Vec<_> = self
                .0
                .borrow()
                .iter()
                .map(|((p, n), size)| (join(p, n), *size))
                .filter(|(path, _)| is_within(path, dir) && path != dir)
                .map(|(path, size)| (path[dir.len()..].to_string(), size))
                .collect();
            out.sort();
            out
        }
    }

    impl Calls for Fake {
        fn stat(&self, path: &str) -> Result<Stat> {
            match self.get(path) {
                Some(size) => Ok(Stat { is_dir: size.is_none(), size: size.unwrap_or(0), ..Default::default() }),
                None => Err(Error::Fs(FsError::NotFound)),
            }
        }

        fn page(&self, dir: &str, after: &str) -> Result<DirPage> {
            match self.get(dir) {
                Some(None) => {}
                Some(Some(_)) => return Err(Error::Fs(FsError::NotDir)),
                None => return Err(Error::Fs(FsError::NotFound)),
            }
            let nodes = self.0.borrow();
            let start = (dir.to_string(), after.to_string());
            let mut rest = nodes.range((Bound::Excluded(start), Bound::Unbounded)).take_while(|((p, _), _)| p == dir);
            let entries = rest
                .by_ref()
                .take(PAGE)
                .map(|((_, name), size)| DirEntry {
                    name: name.clone(),
                    is_dir: size.is_none(),
                    size: size.unwrap_or(0),
                    modified: 0,
                    inode: 0,
                    executable: false,
                    socket: false,
                })
                .collect();
            Ok(DirPage { entries, more: rest.next().is_some() })
        }

        fn mkdir(&self, path: &str) -> Result<()> {
            match (self.get(path), self.get(parent(path))) {
                (Some(_), _) => Err(Error::Fs(FsError::Exists)),
                (None, Some(None)) => self.add(path, None),
                _ => Err(Error::Fs(FsError::NotFound)),
            }
        }

        fn copy_file(&self, from: &str, to: &str) -> Result<()> {
            match (self.get(from), self.get(to), self.get(parent(to))) {
                (Some(Some(size)), None, Some(None)) => self.add(to, Some(size)),
                (Some(None), ..) => Err(Error::Fs(FsError::IsDir)),
                (_, Some(_), _) => Err(Error::Fs(FsError::Exists)),
                _ => Err(Error::Fs(FsError::NotFound)),
            }
        }

        fn remove(&self, path: &str) -> Result<()> {
            match self.get(path) {
                None => Err(Error::Fs(FsError::NotFound)),
                Some(None) if !self.page(path, "")?.entries.is_empty() => Err(Error::Fs(FsError::NotEmpty)),
                Some(_) => {
                    self.0.borrow_mut().remove(&key(path));
                    Ok(())
                }
            }
        }
    }

    /// A tree with folders in folders, and a folder of many files.
    fn sample() -> Fake {
        let fs = Fake::default();
        fs.add("/src", None).unwrap();
        fs.add("/src/a.txt", Some(3)).unwrap();
        fs.add("/src/b", None).unwrap();
        fs.add("/src/b/c.txt", Some(4)).unwrap();
        fs.add("/src/b/d", None).unwrap();
        fs.add("/src/b/d/e", Some(5)).unwrap();
        fs.add("/src/b/empty", None).unwrap();
        fs.add("/src/many", None).unwrap();
        for i in 0..10 {
            fs.add(&alloc::format!("/src/many/{i}"), Some(1)).unwrap();
        }
        fs
    }

    /// A hook that keeps what it was told.
    fn log(seen: &mut Vec<(String, Progress)>) -> impl FnMut(&str, Progress) -> ControlFlow<()> + '_ {
        |path, p| {
            seen.push((path.to_string(), p));
            ControlFlow::Continue(())
        }
    }

    fn position(seen: &[(String, Progress)], path: &str) -> usize {
        seen.iter().position(|(p, _)| p == path).unwrap()
    }

    #[test]
    fn walks_copy_count_and_remove_whole_trees() {
        let fs = sample();
        let mut seen = Vec::new();
        let copied = copy_tree(&fs, "/src", "/dst", &mut log(&mut seen)).unwrap();
        assert_eq!(fs.below("/dst"), fs.below("/src"));
        // The folder and the 17 items in it.
        assert_eq!(copied, Progress { items: 18, bytes: 22 });
        assert_eq!(seen.last().unwrap().1, copied);
        // Each folder before what is in it.
        assert!(position(&seen, "/dst/b") < position(&seen, "/dst/b/d/e"));
        assert_eq!(measure_tree(&fs, "/dst", &mut to_the_end), Ok(copied));
        assert_eq!(measure_tree(&fs, "/dst/a.txt", &mut to_the_end), Ok(Progress { items: 1, bytes: 3 }));
        let mut seen = Vec::new();
        assert_eq!(remove_tree(&fs, "/src", &mut log(&mut seen)), Ok(copied));
        assert!(fs.get("/src").is_none() && fs.below("/src").is_empty());
        // Each folder after what was in it.
        assert!(position(&seen, "/src/b/d/e") < position(&seen, "/src/b/d"));
        assert_eq!(seen.last().unwrap().0, "/src");
        assert_eq!(remove_tree(&fs, "/dst/a.txt", &mut to_the_end), Ok(Progress { items: 1, bytes: 3 }));
        assert!(fs.get("/dst/a.txt").is_none() && fs.get("/dst/b/c.txt").is_some());
    }

    #[test]
    fn copies_go_nowhere_taken() {
        let fs = sample();
        assert_eq!(copy_tree(&fs, "/src", "/src/b/x", &mut to_the_end), Err(Error::IntoItself));
        fs.add("/dst", None).unwrap();
        assert_eq!(copy_tree(&fs, "/src", "/dst", &mut to_the_end), Err(Error::Fs(FsError::Exists)));
        assert_eq!(copy_tree(&fs, "/src/a.txt", "/dst/x/a", &mut to_the_end), Err(Error::Fs(FsError::NotFound)));
        assert_eq!(copy_tree(&fs, "/nothing", "/x", &mut to_the_end), Err(Error::Fs(FsError::NotFound)));
        assert!(fs.below("/dst").is_empty());
    }

    #[test]
    fn walks_stop_when_told() {
        let fs = sample();
        let stop_at = |n: u64| {
            move |_: &str, p: Progress| if p.items == n { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
        };
        assert_eq!(copy_tree(&fs, "/src", "/dst", &mut stop_at(5)), Err(Error::Cancelled));
        // What it did before it stopped stays done: the folder and 4 items.
        assert_eq!(fs.below("/dst").len(), 4);
        assert_eq!(remove_tree(&fs, "/src", &mut stop_at(3)), Err(Error::Cancelled));
        assert_eq!(fs.below("/src").len(), 17 - 3);
        assert_eq!(measure_tree(&fs, "/src", &mut stop_at(1)), Err(Error::Cancelled));
    }

    #[test]
    fn walks_follow_any_depth_on_a_small_stack() {
        // Far deeper than a walk that recursed could go on this stack.
        std::thread::Builder::new()
            .stack_size(128 << 10)
            .spawn(|| {
                let fs = Fake::default();
                let mut path = String::from("/deep");
                fs.add(&path, None).unwrap();
                for _ in 0..3_000 {
                    path.push_str("/d");
                    fs.add(&path, None).unwrap();
                }
                fs.add(&join(&path, "end"), Some(7)).unwrap();
                let all = Progress { items: 3_002, bytes: 7 };
                assert_eq!(copy_tree(&fs, "/deep", "/copy", &mut to_the_end), Ok(all));
                assert_eq!(measure_tree(&fs, "/copy", &mut to_the_end), Ok(all));
                assert_eq!(remove_tree(&fs, "/deep", &mut to_the_end), Ok(all));
                assert!(fs.get("/deep").is_none());
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn errors_flatten() {
        assert_eq!(flat(Ok::<_, IpcError>(Ok::<u8, FsError>(1))), Ok(1));
        assert_eq!(flat::<u8>(Ok(Err(FsError::NotFound))), Err(Error::Fs(FsError::NotFound)));
        assert_eq!(flat::<u8>(Err(IpcError::PeerClosed)), Err(Error::Ipc));
        assert_eq!(flat::<u8>(Err(IpcError::Kernel(abi::Error::TimedOut))), Err(Error::Ipc));
        // What could not be asked, or answered, is not a VFS gone silent.
        assert_eq!(flat::<u8>(Err(IpcError::TooLarge)), Err(Error::Fs(FsError::Invalid)));
        assert_eq!(flat::<u8>(Err(IpcError::Refused(ipc::Refusal::Malformed))), Err(Error::Fs(FsError::Invalid)));
        assert_eq!(Error::Cancelled.to_string(), "it was stopped");
    }

    #[test]
    fn transfers_get_time_for_their_size() {
        assert_eq!(transfer_timeout(0), TIMEOUT_NS);
        assert_eq!(transfer_timeout(1 << 20), TIMEOUT_NS + NS_PER_MIB);
        // 2 GiB, the largest file, at 8 MiB/s.
        assert_eq!(transfer_timeout(2 << 30), TIMEOUT_NS + 256_000_000_000);
        assert_eq!(transfer_timeout(u64::MAX), u64::MAX);
    }

    #[test]
    fn hooks_count_and_stop() {
        let mut seen = Vec::new();
        let mut hook = |path: &str, p: Progress| {
            seen.push((path.to_string(), p));
            if p.items == 2 { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
        };
        let mut p = Progress::default();
        assert_eq!(done(&mut hook, "/a", &mut p, 10), Ok(()));
        assert_eq!(done(&mut hook, "/a/b", &mut p, 5), Err(Error::Cancelled));
        assert_eq!(p, Progress { items: 2, bytes: 15 });
        assert_eq!(seen[0], ("/a".to_string(), Progress { items: 1, bytes: 10 }));
        assert_eq!(Progress { items: 1, bytes: 2 } + Progress { items: 3, bytes: 4 }, Progress { items: 4, bytes: 6 });
    }
}
