// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! An in-memory hierarchical file system.
//!
//! The same structure backs both the read-only system image (built from the
//! initrd) and the writable file system. File contents are bytes of the
//! file system's own, which copies share until one of them changes
//! (copy-on-write), or a borrowed slice of the initrd, copied on the first
//! write, which makes populating the home directory with sample files
//! free.
//!
//! No path is longer than [`MAX_PATH`], so that every file can be named by
//! its path; [`components`] refuses longer ones, and a move that would take
//! something deeper fails. Walks over a subtree keep their own stack, as a
//! tree that deep would overflow a thread's if they recursed.
//!
//! A directory's modification time changes whenever an entry is added to
//! it, removed from it or renamed in it, as POSIX's do, so that a program
//! can tell from a stat whether a listing it has is still current.
//!
//! Besides files and directories, the writable file system holds the Unix
//! domain sockets POSIX programs bind to paths: nodes that name a socket
//! of the service's ([`Kind::Socket`]), which have no contents and are not
//! kept on the home disk.

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::ops::Bound;

use proto::fs::{DIR_PAGE_BYTES, DirEntry, DirPage, FsError, MAX_PATH, Stat};

/// Names a file or directory of a tree, never again another while the
/// system runs (it is the inode number of `Stat`).
pub type NodeId = u64;
/// The root directory.
pub const ROOT: NodeId = 1;
/// The longest name of a file or directory, in bytes.
pub const MAX_NAME: usize = 255;

/// The contents of a file.
pub enum Data {
    /// Bytes of the file system's own, shared by copies of the file until
    /// one of them changes.
    Owned(Rc<Vec<u8>>),
    /// Bytes of the system image, which never change.
    Static(&'static [u8]),
}

impl Data {
    /// No contents.
    pub fn empty() -> Data {
        Data::owned(Vec::new())
    }

    /// Contents of the file system's own.
    pub fn owned(bytes: Vec<u8>) -> Data {
        Data::Owned(Rc::new(bytes))
    }

    /// The contents.
    pub fn bytes(&self) -> &[u8] {
        match self {
            Data::Owned(v) => v,
            Data::Static(s) => s,
        }
    }

    /// The contents of a copy: the same bytes, until either changes.
    pub fn share(&self) -> Data {
        match self {
            Data::Owned(v) => Data::Owned(Rc::clone(v)),
            Data::Static(s) => Data::Static(s),
        }
    }

    /// Whether a change must copy the contents first: they are shared with
    /// a copy, or are the system image's.
    pub fn is_shared(&self) -> bool {
        match self {
            Data::Owned(v) => Rc::strong_count(v) > 1 || Rc::weak_count(v) > 0,
            Data::Static(_) => true,
        }
    }

    /// The most memory a [`resize`](Self::resize) to `len` bytes takes
    /// besides what the contents have now: a copy of
    /// [shared](Self::is_shared) contents, or the new room of contents that
    /// outgrow theirs, which holds them before the old room is let go.
    pub fn resize_cost(&self, len: usize) -> usize {
        match self {
            _ if self.is_shared() => len,
            Data::Owned(v) if len > v.capacity() => room(v.capacity(), len),
            _ => 0,
        }
    }

    /// Makes the contents `len` bytes long (cut short, or filled out with
    /// zeros) and the file's own (a copy of shared contents), to change
    /// them; `NoSpace` if there is no memory for that.
    ///
    /// Contents that outgrow their room get an eighth more than they had:
    /// a file written a piece at a time then moves to bigger room now and
    /// then, about eight times its size in all, rather than whole at every
    /// piece, and keeps at most an eighth more memory than its size. Cut
    /// well short of their room, contents give the rest back.
    pub fn resize(&mut self, len: usize) -> Result<&mut [u8], FsError> {
        if self.is_shared() {
            let bytes = self.bytes();
            let mut own = Vec::new();
            own.try_reserve_exact(len).map_err(|_| FsError::NoSpace)?;
            own.extend_from_slice(&bytes[..len.min(bytes.len())]);
            *self = Data::owned(own);
        }
        // Not shared now: the only reference.
        let Data::Owned(v) = self else { return Err(FsError::Io) };
        let v = Rc::get_mut(v).ok_or(FsError::Io)?;
        let was = v.len();
        if len > v.capacity() {
            v.try_reserve_exact(room(v.capacity(), len) - was).map_err(|_| FsError::NoSpace)?;
        }
        v.resize(len, 0);
        if len < was && v.capacity() - len > len / 8 {
            give_back(v);
        }
        Ok(v)
    }
}

/// The room contents that outgrow `room` bytes get to hold `len`: what
/// they need, and an eighth more than they had at least.
fn room(room: usize, len: usize) -> usize {
    len.max(room.saturating_add(room / 8))
}

/// Moves contents to room of just their size, giving the rest back, if
/// there is memory for the move (`shrink_to_fit` would end the service if
/// there were not).
fn give_back(v: &mut Vec<u8>) {
    let mut fit = Vec::new();
    if fit.try_reserve_exact(v.len()).is_ok() {
        fit.extend_from_slice(v);
        *v = fit;
    }
}

/// A file and its contents, a directory and its entries by name, or a
/// Unix domain socket.
pub enum Kind {
    File(Data),
    Dir(BTreeMap<String, NodeId>),
    /// The socket bound there: the service's id for it.
    Socket(u64),
}

impl Kind {
    /// An empty directory.
    pub fn dir() -> Kind {
        Kind::Dir(BTreeMap::new())
    }

    /// An empty file.
    pub fn file() -> Kind {
        Kind::File(Data::empty())
    }
}

/// A file, directory or socket.
pub struct Node {
    /// The directory it is in (the root's is itself). A node taken out of
    /// the tree keeps the one it was in.
    pub parent: NodeId,
    pub kind: Kind,
    /// Nanoseconds since the Unix epoch.
    pub modified: u64,
}

/// What a walk over a subtree found ([`Tree::measure`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Extent {
    /// Bytes of its files.
    pub bytes: usize,
    /// Its files and directories, its root included: what the home disk
    /// keeps a record of (sockets are not kept).
    pub nodes: usize,
    /// How much longer than its root's path the longest path in it is, in
    /// bytes (`/a/bc` is 5 bytes below its root's); 0 for a file.
    pub depth: usize,
}

/// Splits a path into normalised components: empty ones and `.` dropped,
/// `..` going up (never above the root). Names longer than [`MAX_NAME`]
/// bytes or holding NUL, which POSIX programs could not name, and paths
/// longer than [`MAX_PATH`] are `Invalid`.
pub fn components(path: &str) -> Result<Vec<&str>, FsError> {
    let mut out: Vec<&str> = Vec::new();
    for c in path.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            c if c.len() > MAX_NAME || c.contains('\0') => return Err(FsError::Invalid),
            c => out.push(c),
        }
    }
    if path_len(&out) > MAX_PATH {
        return Err(FsError::Invalid);
    }
    Ok(out)
}

/// The length of the path made of `comps` (`/a/b` for `["a", "b"]`).
pub fn path_len(comps: &[&str]) -> usize {
    comps.iter().map(|c| c.len() + 1).sum()
}

/// Whether file contents are a program: an ELF executable, or a script that
/// names its interpreter.
pub fn is_program(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x7fELF") || bytes.starts_with(b"#!")
}

/// A file system: its nodes by id, the root's first.
pub struct Tree {
    nodes: BTreeMap<NodeId, Node>,
    next: NodeId,
    pub read_only: bool,
    /// The `Stat::device` of its files.
    device: u64,
    /// The time now, in nanoseconds since the Unix epoch.
    clock: fn() -> u64,
}

impl Tree {
    /// An empty tree: its root directory alone.
    pub fn new(read_only: bool, device: u64, clock: fn() -> u64) -> Tree {
        let mut nodes = BTreeMap::new();
        nodes.insert(ROOT, Node { parent: ROOT, kind: Kind::dir(), modified: 0 });
        Tree { nodes, next: ROOT + 1, read_only, device, clock }
    }

    /// Node `id`, if it is (or, taken out, still is) in the tree.
    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(&id)
    }

    /// Node `id`, to change, if it is (or, taken out, still is) in the tree.
    pub fn node_mut(&mut self, id: NodeId) -> Option<&mut Node> {
        self.nodes.get_mut(&id)
    }

    /// The entries of directory `id`.
    fn children(&self, id: NodeId) -> Result<&BTreeMap<String, NodeId>, FsError> {
        match self.nodes.get(&id).map(|n| &n.kind) {
            Some(Kind::Dir(children)) => Ok(children),
            Some(Kind::File(_) | Kind::Socket(_)) => Err(FsError::NotDir),
            None => Err(FsError::NotFound),
        }
    }

    /// The node at the path made of `comps` ([`components`]).
    pub fn lookup(&self, comps: &[&str]) -> Result<NodeId, FsError> {
        let mut cur = ROOT;
        for c in comps {
            cur = *self.children(cur)?.get(*c).ok_or(FsError::NotFound)?;
        }
        Ok(cur)
    }

    /// Adds `name` to directory `parent`, leaving the directory's time
    /// alone (for populating a tree).
    fn insert(&mut self, parent: NodeId, name: &str, kind: Kind) -> Result<NodeId, FsError> {
        let id = self.next;
        let now = (self.clock)();
        let Some(Node { kind: Kind::Dir(children), .. }) = self.nodes.get_mut(&parent) else {
            return Err(FsError::NotDir);
        };
        if children.contains_key(name) {
            return Err(FsError::Exists);
        }
        children.insert(name.to_string(), id);
        self.next += 1;
        self.nodes.insert(id, Node { parent, kind, modified: now });
        Ok(id)
    }

    /// Creates every missing directory along `comps`; returns the last one
    /// (for populating a tree).
    pub fn mkdir_all(&mut self, comps: &[&str]) -> Result<NodeId, FsError> {
        let mut cur = ROOT;
        for c in comps {
            cur = match self.children(cur)?.get(*c).copied() {
                Some(id) => id,
                None => self.insert(cur, c, Kind::dir())?,
            };
        }
        Ok(cur)
    }

    /// Adds a file, creating parent directories (for populating a tree).
    pub fn add_file(&mut self, comps: &[&str], data: Data) -> Result<NodeId, FsError> {
        let (name, dirs) = comps.split_last().ok_or(FsError::Invalid)?;
        let parent = self.mkdir_all(dirs)?;
        self.insert(parent, name, Kind::File(data))
    }

    /// Creates a file, directory or socket (`kind`) at `comps`, whose
    /// directory must exist.
    pub fn create(&mut self, comps: &[&str], kind: Kind) -> Result<NodeId, FsError> {
        // What is there already is reported as such, even where nothing
        // can be made (as POSIX file systems do: `mkdir -p` relies on it).
        if self.lookup(comps).is_ok() {
            return Err(FsError::Exists);
        }
        if self.read_only {
            return Err(FsError::ReadOnly);
        }
        let (name, dirs) = comps.split_last().ok_or(FsError::Exists)?;
        let parent = self.lookup(dirs)?;
        let id = self.insert(parent, name, kind)?;
        self.touch(parent);
        Ok(id)
    }

    /// What there is to know of node `id` (a directory's size is its number
    /// of entries).
    pub fn stat(&self, id: NodeId) -> Result<Stat, FsError> {
        let n = self.nodes.get(&id).ok_or(FsError::NotFound)?;
        let (size, is_dir, executable) = match &n.kind {
            Kind::File(d) => (d.bytes().len() as u64, false, is_program(d.bytes())),
            Kind::Dir(c) => (c.len() as u64, true, false),
            Kind::Socket(_) => (0, false, false),
        };
        Ok(Stat {
            size,
            is_dir,
            read_only: self.read_only,
            modified: n.modified,
            inode: id,
            device: self.device,
            executable,
            char_device: false,
            socket: matches!(n.kind, Kind::Socket(_)),
        })
    }

    /// Walks the subtree of `id` (itself included).
    pub fn measure(&self, id: NodeId) -> Extent {
        let mut extent = Extent::default();
        // What is still to be looked at, with how far below `id` it is.
        let mut todo = vec![(id, 0usize)];
        while let Some((n, depth)) = todo.pop() {
            let Some(node) = self.nodes.get(&n) else { continue };
            extent.depth = extent.depth.max(depth);
            match &node.kind {
                Kind::File(d) => extent.bytes = extent.bytes.saturating_add(d.bytes().len()),
                Kind::Dir(children) => todo.extend(children.iter().map(|(name, &c)| (c, depth + 1 + name.len()))),
                Kind::Socket(_) => continue,
            }
            extent.nodes += 1;
        }
        extent
    }

    /// The entries of directory `id` whose names come after `after` in the
    /// order of their bytes, in that order.
    pub fn entries_after<'a>(
        &'a self,
        id: NodeId,
        after: &str,
    ) -> Result<impl Iterator<Item = DirEntry> + 'a, FsError> {
        let children = self.children(id)?;
        Ok(children.range::<str, _>((Bound::Excluded(after), Bound::Unbounded)).filter_map(|(name, &child)| {
            let s = self.stat(child).ok()?;
            Some(DirEntry {
                name: name.clone(),
                is_dir: s.is_dir,
                size: s.size,
                modified: s.modified,
                inode: child,
                executable: s.executable,
                socket: s.socket,
            })
        }))
    }

    /// Takes `comps` out of its directory (a directory must be empty) and
    /// returns its node. The node stays readable until it is
    /// [forgotten](Self::forget): POSIX keeps an unlinked file alive while
    /// it is open.
    pub fn unlink(&mut self, comps: &[&str]) -> Result<NodeId, FsError> {
        if self.read_only {
            return Err(FsError::ReadOnly);
        }
        let (name, dirs) = comps.split_last().ok_or(FsError::Invalid)?;
        let parent = self.lookup(dirs)?;
        let id = self.lookup(comps)?;
        if self.children(id).is_ok_and(|c| !c.is_empty()) {
            return Err(FsError::NotEmpty);
        }
        if let Some(Node { kind: Kind::Dir(children), .. }) = self.nodes.get_mut(&parent) {
            children.remove(*name);
        }
        self.touch(parent);
        Ok(id)
    }

    /// Deletes a node [unlinked](Self::unlink) before.
    pub fn forget(&mut self, id: NodeId) {
        if id != ROOT {
            self.nodes.remove(&id);
        }
    }

    /// Moves `from` to `to`. If `to` exists, the move fails with `Exists`
    /// unless `replace` is set: then `to` (a file, when `from` is a file; an
    /// empty directory, when `from` is a directory) is unlinked first and
    /// returned, to be [forgotten](Self::forget) once no longer open. A move
    /// that would take a path in the subtree beyond [`MAX_PATH`] is
    /// `Invalid`. Either everything happens or nothing does.
    pub fn rename(&mut self, from: &[&str], to: &[&str], replace: bool) -> Result<Option<NodeId>, FsError> {
        if self.read_only {
            return Err(FsError::ReadOnly);
        }
        let id = self.lookup(from)?;
        let (from_name, from_dirs) = from.split_last().ok_or(FsError::Invalid)?;
        let (to_name, to_dirs) = to.split_last().ok_or(FsError::Invalid)?;
        let from_parent = self.lookup(from_dirs)?;
        let to_parent = self.lookup(to_dirs)?;
        // Refuse to move a directory into itself.
        if self.is_within(to_parent, id) {
            return Err(FsError::Invalid);
        }
        let replaced = match self.children(to_parent)?.get(*to_name).copied() {
            None => None,
            // A name for itself: nothing to do.
            Some(old) if old == id => return Ok(None),
            Some(_) if !replace => return Err(FsError::Exists),
            Some(old) => {
                let kind = |n: NodeId| self.nodes.get(&n).map(|n| matches!(n.kind, Kind::Dir(_)));
                match (kind(id), kind(old)) {
                    (Some(false), Some(true)) => return Err(FsError::IsDir),
                    (Some(true), Some(false)) => return Err(FsError::NotDir),
                    (Some(true), Some(true)) if self.children(old).is_ok_and(|c| !c.is_empty()) => {
                        return Err(FsError::NotEmpty);
                    }
                    _ => {}
                }
                Some(old)
            }
        };
        // Only a longer path can take the subtree's deepest one too far.
        if path_len(to) > path_len(from) && path_len(to) + self.measure(id).depth > MAX_PATH {
            return Err(FsError::Invalid);
        }
        if let Some(Node { kind: Kind::Dir(c), .. }) = self.nodes.get_mut(&from_parent) {
            c.remove(*from_name);
        }
        if let Some(Node { kind: Kind::Dir(c), .. }) = self.nodes.get_mut(&to_parent) {
            c.insert(to_name.to_string(), id);
        }
        if let Some(n) = self.nodes.get_mut(&id) {
            n.parent = to_parent;
        }
        let now = (self.clock)();
        self.set_modified(from_parent, now);
        self.set_modified(to_parent, now);
        Ok(replaced)
    }

    /// Whether node `id` is `ancestor` or lies below it.
    pub fn is_within(&self, id: NodeId, ancestor: NodeId) -> bool {
        let mut cur = id;
        // Parents lead to the root in fewer steps than there are nodes.
        for _ in 0..=self.nodes.len() {
            if cur == ancestor {
                return true;
            }
            match self.nodes.get(&cur) {
                Some(n) if cur != ROOT => cur = n.parent,
                _ => return false,
            }
        }
        false
    }

    /// Sets a node's modification time to now.
    pub fn touch(&mut self, id: NodeId) {
        let now = (self.clock)();
        self.set_modified(id, now);
    }

    /// Sets a node's modification time (nanoseconds since the Unix epoch).
    pub fn set_modified(&mut self, id: NodeId, modified: u64) {
        if let Some(n) = self.nodes.get_mut(&id) {
            n.modified = modified;
        }
    }
}

/// The entries of two listings, each in the order of their names' bytes,
/// together in that order.
pub fn merge(a: impl Iterator<Item = DirEntry>, b: impl Iterator<Item = DirEntry>) -> impl Iterator<Item = DirEntry> {
    let (mut a, mut b) = (a.peekable(), b.peekable());
    core::iter::from_fn(move || match (a.peek(), b.peek()) {
        (Some(x), Some(y)) if y.name < x.name => b.next(),
        (Some(_), _) => a.next(),
        (None, _) => b.next(),
    })
}

/// A page of a listing from `entries`, which come in the order of their
/// names' bytes from where the page starts: at most `max` of them (0: no
/// limit), and no more than [`DIR_PAGE_BYTES`] (but at least one).
pub fn page(entries: impl Iterator<Item = DirEntry>, max: u32) -> DirPage {
    let max = if max == 0 { usize::MAX } else { max as usize };
    let mut entries = entries.peekable();
    let (mut out, mut bytes) = (Vec::new(), 0);
    while let Some(e) = entries.peek() {
        let len = e.encoded_len();
        if out.len() == max || (!out.is_empty() && bytes + len > DIR_PAGE_BYTES) {
            break;
        }
        bytes += len;
        out.extend(entries.next());
    }
    DirPage { more: entries.peek().is_some(), entries: out }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;
    use core::sync::atomic::{AtomicU64, Ordering};

    /// A clock that moves on a nanosecond every time it is read.
    fn clock() -> u64 {
        static NOW: AtomicU64 = AtomicU64::new(1_000);
        NOW.fetch_add(1, Ordering::Relaxed)
    }

    fn tree() -> Tree {
        Tree::new(false, 1, clock)
    }

    fn c(path: &str) -> Vec<&str> {
        components(path).unwrap()
    }

    fn modified(t: &Tree, path: &str) -> u64 {
        t.stat(t.lookup(&c(path)).unwrap()).unwrap().modified
    }

    fn names(page: &DirPage) -> Vec<&str> {
        page.entries.iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn paths_are_normalised_and_bounded() {
        assert_eq!(c("/a//b/./c/../d/"), ["a", "b", "d"]);
        assert_eq!(c("/../.."), Vec::<&str>::new());
        assert_eq!(components(&format!("/{}", "n".repeat(MAX_NAME))).map(|v| v.len()), Ok(1));
        assert_eq!(components(&format!("/{}", "n".repeat(MAX_NAME + 1))), Err(FsError::Invalid));
        assert_eq!(components("/a\0b"), Err(FsError::Invalid));
        // The longest path, and one byte more.
        let longest = "/a".repeat(MAX_PATH / 2);
        assert_eq!(components(&longest).map(|v| v.len()), Ok(MAX_PATH / 2));
        assert_eq!(components(&format!("{longest}/b")), Err(FsError::Invalid));
        // What `..` takes away does not count.
        assert!(components(&format!("{longest}/b/..")).is_ok());
    }

    #[test]
    fn listings_come_in_pages_in_name_order() {
        let mut t = tree();
        let dir = t.mkdir_all(&["d"]).unwrap();
        for i in 0..2000 {
            t.create(&c(&format!("/d/{:04}", 1999 - i)), Kind::file()).unwrap();
        }
        t.create(&c("/d/sub"), Kind::dir()).unwrap();
        // All of it, page by page, each page within its bytes.
        let (mut all, mut after, mut pages) = (Vec::new(), String::new(), 0);
        loop {
            let p = page(t.entries_after(dir, &after).unwrap(), 0);
            let bytes: usize = p.entries.iter().map(DirEntry::encoded_len).sum();
            assert!(bytes <= DIR_PAGE_BYTES && !p.entries.is_empty());
            after = p.entries.last().unwrap().name.clone();
            all.extend(p.entries.iter().map(|e| e.name.clone()));
            pages += 1;
            if !p.more {
                break;
            }
        }
        assert!(pages > 1);
        let mut expected: Vec<String> = (0..2000).map(|i| format!("{i:04}")).collect();
        expected.push("sub".into());
        assert_eq!(all, expected);
        // At most `max`, after any name, whether it exists or not.
        let p = page(t.entries_after(dir, "0007").unwrap(), 3);
        assert_eq!((names(&p), p.more), (vec!["0008", "0009", "0010"], true));
        let p = page(t.entries_after(dir, "1999x").unwrap(), 3);
        assert_eq!((names(&p), p.more), (vec!["sub"], false));
        let p = page(t.entries_after(dir, "sub").unwrap(), 0);
        assert_eq!((names(&p), p.more), (vec![], false));
        assert!(p.entries.is_empty());
        let sub = &page(t.entries_after(dir, "1999").unwrap(), 0).entries[0];
        assert!(sub.is_dir && sub.size == 0);
        // Files are not listed.
        let file = t.lookup(&c("/d/0001")).unwrap();
        assert_eq!(t.entries_after(file, "").err(), Some(FsError::NotDir));
    }

    fn entry(name: &str) -> DirEntry {
        DirEntry { name: name.into(), is_dir: false, size: 0, modified: 0, inode: 2, executable: false, socket: false }
    }

    #[test]
    fn a_page_holds_at_least_one_entry() {
        let huge = "x".repeat(DIR_PAGE_BYTES);
        let p = page([entry(&huge), entry("y")].into_iter(), 0);
        assert_eq!((p.entries.len(), p.more), (1, true));
    }

    #[test]
    fn listings_merge_in_name_order() {
        let list = |names: &[&str]| names.iter().map(|n| entry(n)).collect::<Vec<_>>().into_iter();
        let merged: Vec<String> =
            merge(list(&["a", "home", "tmp"]), list(&["dev", "system", "z"])).map(|e| e.name).collect();
        assert_eq!(merged, ["a", "dev", "home", "system", "tmp", "z"]);
        assert_eq!(merge(list(&[]), list(&["dev"])).count(), 1);
        assert_eq!(merge(list(&["a"]), list(&[])).count(), 1);
    }

    #[test]
    fn directories_change_when_their_entries_do() {
        let mut t = tree();
        t.mkdir_all(&["a", "b"]).unwrap();
        let before = modified(&t, "/a");
        t.create(&c("/a/f"), Kind::file()).unwrap();
        let created = modified(&t, "/a");
        assert!(created > before);
        // Not when something inside an entry changes.
        let f = t.lookup(&c("/a/f")).unwrap();
        t.touch(f);
        t.create(&c("/a/b/g"), Kind::file()).unwrap();
        assert_eq!(modified(&t, "/a"), created);
        // Renaming in one directory, and moving between two.
        t.rename(&c("/a/f"), &c("/a/h"), false).unwrap();
        let renamed = modified(&t, "/a");
        assert!(renamed > created);
        let b = modified(&t, "/a/b");
        t.rename(&c("/a/h"), &c("/a/b/h"), false).unwrap();
        assert!(modified(&t, "/a") > renamed && modified(&t, "/a/b") > b);
        // Removing.
        let b = modified(&t, "/a/b");
        t.unlink(&c("/a/b/h")).unwrap();
        assert!(modified(&t, "/a/b") > b);
        // Failures change nothing.
        let a = modified(&t, "/a");
        assert_eq!(t.create(&c("/a/b"), Kind::dir()), Err(FsError::Exists));
        assert_eq!(t.unlink(&c("/a/b")), Err(FsError::NotEmpty));
        assert_eq!(t.rename(&c("/a/b"), &c("/a/b/g/x"), false), Err(FsError::Invalid));
        assert_eq!(modified(&t, "/a"), a);
    }

    #[test]
    fn walks_reach_any_depth() {
        // A chain far deeper than a recursive walk could follow on a
        // thread's stack (a home disk written before paths were limited
        // may hold one).
        let mut t = tree();
        t.mkdir_all(&vec!["d"; 200_000]).unwrap();
        let e = t.measure(t.lookup(&["d"]).unwrap());
        assert_eq!((e.nodes, e.depth, e.bytes), (200_000, (200_000 - 1) * 2, 0));
    }

    #[test]
    fn programs_cannot_build_trees_deeper_than_paths_go() {
        // Moving a chain into a new directory again and again, its paths
        // short all along, stops at the longest path.
        let mut t = tree();
        t.mkdir_all(&["t"]).unwrap();
        let mut levels = 1;
        loop {
            t.create(&["n"], Kind::dir()).unwrap();
            if t.rename(&["t"], &["n", "t"], false).is_err() {
                break;
            }
            t.rename(&["n"], &["t"], false).unwrap();
            levels += 1;
        }
        assert_eq!(levels, MAX_PATH / 2);
        let deepest = "/t".repeat(levels);
        assert!(t.lookup(&c(&deepest)).is_ok());
        // Nor can it be renamed deeper; but it can be moved where its
        // paths are no longer.
        assert_eq!(t.rename(&["t"], &["tt"], false), Err(FsError::Invalid));
        assert_eq!(t.rename(&["t"], &["u"], false), Ok(None));
    }

    #[test]
    fn moves_keep_every_path_within_the_limit() {
        let mut t = tree();
        let deep = "/d".repeat(MAX_PATH / 2 - 1);
        t.mkdir_all(&c(&deep)).unwrap();
        t.mkdir_all(&["x"]).unwrap();
        t.mkdir_all(&["xy"]).unwrap();
        // `/x/d/d/...` is the longest path it may have.
        assert_eq!(t.rename(&["d"], &["x", "d"], false), Ok(None));
        assert_eq!(t.rename(&["x", "d"], &["xy", "d"], false), Err(FsError::Invalid));
        assert!(t.lookup(&c(&format!("/x{deep}"))).is_ok());
    }

    #[test]
    fn replacing() {
        let mut t = tree();
        t.add_file(&["a"], Data::owned(b"new".to_vec())).unwrap();
        let old = t.add_file(&["b"], Data::owned(b"old".to_vec())).unwrap();
        t.mkdir_all(&["empty"]).unwrap();
        t.mkdir_all(&["full", "x"]).unwrap();
        assert_eq!(t.rename(&["a"], &["b"], false), Err(FsError::Exists));
        assert_eq!(t.rename(&["a"], &["empty"], true), Err(FsError::IsDir));
        assert_eq!(t.rename(&["empty"], &["b"], true), Err(FsError::NotDir));
        assert_eq!(t.rename(&["empty"], &["full"], true), Err(FsError::NotEmpty));
        assert_eq!(t.rename(&["a"], &["b"], true), Ok(Some(old)));
        assert_eq!(t.lookup(&["a"]), Err(FsError::NotFound));
        let b = t.lookup(&["b"]).unwrap();
        assert!(matches!(&t.node(b).unwrap().kind, Kind::File(d) if d.bytes() == b"new"));
        // The replaced file stays readable until it is forgotten.
        assert!(matches!(&t.node(old).unwrap().kind, Kind::File(d) if d.bytes() == b"old"));
        t.forget(old);
        assert!(t.node(old).is_none());
        // A directory replaces an empty one.
        assert!(t.rename(&["full"], &["empty"], true).unwrap().is_some());
        assert!(t.lookup(&["empty", "x"]).is_ok());
    }

    #[test]
    fn copies_share_contents_until_one_changes() {
        let mut t = tree();
        let a = t.add_file(&["a"], Data::owned(b"abc".to_vec())).unwrap();
        let Some(Node { kind: Kind::File(d), .. }) = t.node(a) else { unreachable!() };
        let copy = d.share();
        assert!(d.is_shared() && copy.is_shared() && d.resize_cost(4) == 4);
        let b = t.create(&["b"], Kind::File(copy)).unwrap();
        let Some(Node { kind: Kind::File(d), .. }) = t.node_mut(b) else { unreachable!() };
        d.resize(4).unwrap()[3] = b'd';
        assert!(!d.is_shared() && d.resize_cost(4) == 0);
        let read = |t: &Tree, id| match &t.node(id).unwrap().kind {
            Kind::File(d) => d.bytes().to_vec(),
            Kind::Dir(_) | Kind::Socket(_) => unreachable!(),
        };
        assert_eq!((read(&t, a), read(&t, b)), (b"abc".to_vec(), b"abcd".to_vec()));
        // The original is its own again.
        let Some(Node { kind: Kind::File(d), .. }) = t.node(a) else { unreachable!() };
        assert!(!d.is_shared());
        // The system image's contents are copied on the first change.
        let mut s = Data::Static(b"image");
        assert_eq!(s.resize_cost(6), 6);
        s.resize(6).unwrap()[5] = b'!';
        assert_eq!(s.bytes(), b"image!");
        assert_eq!(s.resize_cost(6), 0);
        // A copy cut short copies what is kept alone.
        let whole = Data::owned(b"abcdef".to_vec());
        let mut cut = whole.share();
        assert_eq!(cut.resize_cost(3), 3);
        cut.resize(3).unwrap();
        assert_eq!((whole.bytes(), cut.bytes()), (&b"abcdef"[..], &b"abc"[..]));
    }

    /// The memory contents have room for.
    fn room_of(d: &Data) -> usize {
        match d {
            Data::Owned(v) => v.capacity(),
            Data::Static(_) => 0,
        }
    }

    #[test]
    fn contents_that_outgrow_their_room_get_an_eighth_more() {
        // Written whole, contents take just their size.
        let mut d = Data::empty();
        d.resize(800).unwrap();
        assert_eq!((room_of(&d), d.resize_cost(800)), (800, 0));
        // Outgrowing it, an eighth more than they had, or what they need.
        assert_eq!((d.resize_cost(801), d.resize_cost(2000)), (900, 2000));
        d.resize(801).unwrap();
        assert_eq!((room_of(&d), d.resize_cost(900)), (900, 0));
        // Written a piece at a time, a file is moved about eight times its
        // size in all, and keeps an eighth more memory at most.
        let mut d = Data::empty();
        let (piece, size) = (32, 1 << 20);
        let mut moved = 0;
        for len in (piece..=size).step_by(piece) {
            if len > room_of(&d) {
                moved += len - piece;
            }
            d.resize(len).unwrap()[len - piece..].fill(1);
        }
        assert!(moved < 9 * size && room_of(&d) <= size + size / 8, "{moved} {}", room_of(&d));
        assert!(d.bytes().len() == size && d.bytes().iter().all(|&b| b == 1));
    }

    #[test]
    fn contents_cut_well_short_give_memory_back() {
        let bytes: Vec<u8> = (0..=255).cycle().take(1000).collect();
        let mut d = Data::owned(bytes.clone());
        // Cut a little, they keep their room.
        d.resize(900).unwrap();
        assert_eq!((room_of(&d), d.bytes()), (1000, &bytes[..900]));
        // Cut by more than an eighth of what is left, they give it back.
        d.resize(800).unwrap();
        assert_eq!((room_of(&d), d.bytes()), (800, &bytes[..800]));
        // Grown again, the new bytes are zeros.
        d.resize(810).unwrap();
        assert!(d.bytes()[..800] == bytes[..800] && d.bytes()[800..] == [0; 10]);
        d.resize(0).unwrap();
        assert_eq!((room_of(&d), d.bytes()), (0, &[][..]));
    }

    #[test]
    fn ancestry() {
        let mut t = tree();
        let b = t.mkdir_all(&["a", "b"]).unwrap();
        let a = t.lookup(&["a"]).unwrap();
        let x = t.mkdir_all(&["x"]).unwrap();
        assert!(t.is_within(b, a) && t.is_within(b, b) && t.is_within(b, ROOT));
        assert!(!t.is_within(a, b) && !t.is_within(b, x));
        // A node taken out of the tree is in nothing that is forgotten.
        let gone = t.unlink(&["a", "b"]).unwrap();
        t.unlink(&["a"]).unwrap();
        t.forget(a);
        assert!(!t.is_within(gone, ROOT));
    }

    #[test]
    fn missing_nodes_are_not_found() {
        let mut t = tree();
        let f = t.add_file(&["f"], Data::empty()).unwrap();
        assert_eq!(t.lookup(&["f", "x"]), Err(FsError::NotDir));
        assert_eq!(t.create(&["f", "x"], Kind::file()), Err(FsError::NotDir));
        t.unlink(&["f"]).unwrap();
        t.forget(f);
        assert_eq!(t.stat(f), Err(FsError::NotFound));
        assert_eq!(t.measure(f), Extent::default());
        assert_eq!(t.entries_after(f, "").err(), Some(FsError::NotFound));
    }

    #[test]
    fn sockets_are_nodes_without_contents() {
        let mut t = tree();
        t.mkdir_all(&["tmp"]).unwrap();
        let sock = t.create(&c("/tmp/app.sock"), Kind::Socket(42)).unwrap();
        let s = t.stat(sock).unwrap();
        assert!(s.socket && !s.is_dir && !s.executable && s.size == 0);
        let tmp = t.lookup(&["tmp"]).unwrap();
        let listed: Vec<DirEntry> = t.entries_after(tmp, "").unwrap().collect();
        assert!(listed.len() == 1 && listed[0].socket && listed[0].name == "app.sock");
        // Nothing is inside one, and no file or socket can be made where
        // one is.
        assert_eq!(t.lookup(&c("/tmp/app.sock/x")), Err(FsError::NotDir));
        assert_eq!(t.create(&c("/tmp/app.sock/x"), Kind::file()), Err(FsError::NotDir));
        assert_eq!(t.create(&c("/tmp/app.sock"), Kind::Socket(43)), Err(FsError::Exists));
        // It moves, is replaced and removed as a file is.
        t.rename(&c("/tmp/app.sock"), &c("/tmp/b.sock"), false).unwrap();
        assert!(matches!(t.node(t.lookup(&c("/tmp/b.sock")).unwrap()).unwrap().kind, Kind::Socket(42)));
        t.add_file(&c("/tmp/f"), Data::empty()).unwrap();
        assert_eq!(t.rename(&c("/tmp/f"), &c("/tmp/b.sock"), true), Ok(Some(sock)));
        assert_eq!(t.measure(tmp).nodes, 2);
        let f = t.unlink(&c("/tmp/b.sock")).unwrap();
        t.forget(f);
        assert_eq!(t.entries_after(tmp, "").unwrap().count(), 0);
    }

    #[test]
    fn a_read_only_tree_changes_not() {
        let mut t = Tree::new(true, 2, clock);
        t.add_file(&["bin", "init"], Data::Static(b"\x7fELF")).unwrap();
        let init = t.lookup(&["bin", "init"]).unwrap();
        let s = t.stat(init).unwrap();
        assert!(s.read_only && s.executable && s.size == 4 && s.device == 2);
        assert_eq!(t.create(&["bin", "x"], Kind::file()), Err(FsError::ReadOnly));
        assert_eq!(t.create(&["bin", "init"], Kind::file()), Err(FsError::Exists));
        assert_eq!(t.unlink(&["bin", "init"]), Err(FsError::ReadOnly));
        assert_eq!(t.rename(&["bin"], &["sbin"], false), Err(FsError::ReadOnly));
    }
}
