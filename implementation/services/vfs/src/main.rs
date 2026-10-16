// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `vfs` — the Veda file system service.
//!
//! Namespace:
//! * `/system` — the read-only system image (the initrd),
//! * `/dev` — the devices (`null`, `zero`, `full`, `random`, `urandom`;
//!   see [`dev`]), and
//! * everything else — a writable in-memory file system holding the user's
//!   home directory and `/tmp`.
//!
//! The file systems themselves, their snapshots and the rules of who may
//! reach what are `vfs-core`'s (`core/`), where the host tests them; this
//! program serves them.
//!
//! The home directory starts with the sample files of the system image. If
//! a home disk is attached, `/home` is restored from it at boot and saved
//! back shortly after every change (see [`persist`]).
//!
//! Files are open in one of two ways. Each client connection has its own
//! table of descriptors (`vfs::open`), read and written at explicit
//! offsets. And `vfs::open_file` gives a file a connection of its own (the
//! `file` protocol): an open file description in the POSIX sense, with an
//! offset shared by every connection duplicated from it, which can be
//! passed to other processes. As in POSIX, a file removed while open stays
//! readable and writable through what has it open, and goes away with the
//! last of them; one moved while open is wherever it is now (a file moved
//! into `/home` counts against the home disk from then on).
//!
//! **Unix domain sockets.** A POSIX program's socket bound to a path is a
//! node of the writable file system, which `stat` shows and `remove` takes
//! away; the VFS keeps what it needs to hand the socket what comes for it,
//! connections and datagrams (see [`sockets`]). Sockets are not kept on the
//! home disk: they are no more once the system restarts.
//!
//! **Private directories.** `/home/.private/<service>` belongs to the
//! system service of that name (as the registry identifies the client):
//! only it can see and change what is inside, and `.private` does not
//! appear in listings of `/home` for anyone else. Services keep secrets and
//! personal data there (the agent its Deepgram key and what it remembers
//! about the user), on the home disk like everything in `/home`. A
//! connection can give that access up (`vfs::renounce_private`). The root,
//! `/home`, `/home/user`, `/tmp` and `/home/.private` cannot be removed,
//! moved or replaced (see `vfs_core::access`).
//!
//! **Limits.** No path is longer than `vproto::fs::MAX_PATH`. A program
//! holds a bounded number of connections, and all of them together stay
//! within what the kernel waits on at once (`vfs_core::access`). Files grow
//! only while memory is left for the rest of the system, and in `/home`
//! while they fit on the home disk.

#![no_std]
#![no_main]

extern crate alloc;

mod dev;
mod persist;
mod sockets;

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use vabi::signals;
use vipc::{Bytes, WaitSet};
use vproto::fs::{
    DirEntry, DirPage, FsError, MAX_DATAGRAM, MAX_IO, MAX_SOCKET_NAME, SocketAddress, SocketDatagram, SocketPeer,
    Space, Stat, device, file, open_flags, seek, socket_kind, vfs,
};
use vproto::init::ClientIdentity;
use vrt::object::{Channel, Handle, Vmo};
use vrt::println;

use dev::Dev;
use persist::Store;
use sockets::Sockets;
use vfs_core::access::{self, Connections, PRIVATE};
use vfs_core::snapshot::{self, Samples};
use vfs_core::tree::{self, Data, Kind, Node, NodeId, Tree};
use vfs_core::{HOME, USER};

vrt::entry!(main);

const SYSTEM: &str = "system";
const DEV: &str = "dev";
/// Directories every home has.
const HOME_DIRS: [&str; 5] =
    ["home/user/Documents", "home/user/Pictures", "home/user/Music", "home/user/Videos", "home/user/Desktop"];
/// Changes are saved once nothing changed for this long...
const SAVE_QUIET_NS: u64 = 500_000_000;
/// ... or at the latest this long after the first unsaved change.
const SAVE_MAX_DELAY_NS: u64 = 5_000_000_000;
/// A save that failed is tried again this long after (and at once by
/// `vfs::sync`).
const SAVE_RETRY_NS: u64 = 30_000_000_000;
/// Largest file in the writable file system.
const MAX_FILE: u64 = 1 << 31;
/// Memory the writable file system leaves free for the rest of the system.
const MEMORY_RESERVE: u64 = 64 << 20;
/// Memory a file or directory takes besides its name and contents: its
/// node and its directory's entry, with the B-trees' slack (a generous
/// estimate).
const NODE_MEMORY: usize = 256;
/// Descriptors per client connection (`vfs::open`).
const MAX_FDS: usize = 256;

/// Which tree a path lives in.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Mount {
    System,
    Ram,
    Dev,
}

/// A descriptor of the `vfs::open` kind.
struct OpenFile {
    mount: Mount,
    node: NodeId,
    flags: u32,
}

/// What an open file description refers to.
#[derive(Clone, Copy)]
enum Target {
    Node(Mount, NodeId),
    Dev(Dev),
}

/// An open file description (behind every connection of the `file` kind
/// duplicated from one `open_file`).
struct Description {
    target: Target,
    flags: u32,
    offset: u64,
    /// The program that opened it, whose connections its connections are.
    owner: u64,
}

type Shared = Rc<RefCell<Description>>;

/// Whether a path is in `/home`, which is kept on the home disk.
fn in_home(m: Mount, comps: &[&str]) -> bool {
    m == Mount::Ram && comps.first() == Some(&HOME)
}

struct Fs {
    system: Tree,
    /// The bytes of the system image's files (it never changes).
    system_bytes: u64,
    ram: Tree,
    samples: Samples,
    /// The home disk, if one is attached.
    store: Option<Store>,
    /// Times of the first and the latest unsaved change.
    dirty: Option<(u64, u64)>,
    /// No save is tried before this (after one failed).
    retry_at: u64,
    /// How many descriptors and descriptions have each node open.
    open: BTreeMap<(Mount, NodeId), u32>,
    /// Nodes removed while open, forgotten when the last closes.
    orphans: BTreeSet<(Mount, NodeId)>,
    connections: Connections,
    /// The Unix domain sockets bound to paths.
    sockets: Sockets,
    /// The size of `/home`'s snapshot, while known: changes move it by what
    /// they add to the snapshot and take from it, and every save measures
    /// it afresh, so that checking a write against the home disk does not
    /// walk all of `/home`.
    home_len: Cell<Option<usize>>,
}

impl Fs {
    /// Resolves a path to a tree and the components inside it.
    fn route<'p>(&self, path: &'p str) -> Result<(Mount, Vec<&'p str>), FsError> {
        let comps = tree::components(path)?;
        match comps.first() {
            Some(&SYSTEM) => Ok((Mount::System, comps[1..].to_vec())),
            Some(&DEV) => Ok((Mount::Dev, comps[1..].to_vec())),
            _ => Ok((Mount::Ram, comps)),
        }
    }

    /// The tree of a mount (`/dev` has none: nothing there can change).
    fn tree(&self, m: Mount) -> Result<&Tree, FsError> {
        match m {
            Mount::System => Ok(&self.system),
            Mount::Ram => Ok(&self.ram),
            Mount::Dev => Err(FsError::ReadOnly),
        }
    }

    fn tree_mut(&mut self, m: Mount) -> Result<&mut Tree, FsError> {
        match m {
            Mount::System => Ok(&mut self.system),
            Mount::Ram => Ok(&mut self.ram),
            Mount::Dev => Err(FsError::ReadOnly),
        }
    }

    /// Something opened `node`.
    fn hold(&mut self, m: Mount, node: NodeId) {
        *self.open.entry((m, node)).or_default() += 1;
    }

    /// Something that had `node` open closed it.
    fn release(&mut self, m: Mount, node: NodeId) {
        let Some(n) = self.open.get_mut(&(m, node)) else { return };
        *n -= 1;
        if *n == 0 {
            self.open.remove(&(m, node));
            if self.orphans.remove(&(m, node))
                && let Ok(t) = self.tree_mut(m)
            {
                t.forget(node);
            }
        }
    }

    /// Disposes of a node taken out of its tree: now, or when the last
    /// thing that has it open closes it. A socket's program may go on
    /// with it, without its name in the file system.
    fn dispose(&mut self, m: Mount, node: NodeId) {
        if let Ok(t) = self.tree(m)
            && let Some(Node { kind: Kind::Socket(id), .. }) = t.node(node)
        {
            self.sockets.unlinked(*id);
        }
        if self.open.contains_key(&(m, node)) {
            self.orphans.insert((m, node));
        } else if let Ok(t) = self.tree_mut(m) {
            t.forget(node);
        }
    }

    /// Whether `node` is in `/home` now, which the home disk keeps (one
    /// removed while open is nowhere).
    fn node_in_home(&self, m: Mount, node: NodeId) -> bool {
        m == Mount::Ram
            && !self.orphans.contains(&(m, node))
            && self.ram.lookup(&[HOME]).is_ok_and(|home| self.ram.is_within(node, home))
    }

    /// What the record of a file with contents `d` stores of them.
    fn stored(&self, d: &Data) -> usize {
        snapshot::stored_len(d, &self.samples)
    }

    /// The size of `/home`'s snapshot now.
    fn home_len(&self) -> usize {
        if let Some(len) = self.home_len.get() {
            return len;
        }
        let len = snapshot::len(&self.ram, &self.samples);
        self.home_len.set(Some(len));
        len
    }

    /// A change added `added` bytes to `/home`'s snapshot and took
    /// `removed`.
    fn home_changed(&self, added: usize, removed: usize) {
        if let Some(len) = self.home_len.get() {
            self.home_len.set((len + added).checked_sub(removed));
        }
    }

    /// Fails with `NoSpace` unless `/home` can grow by `extra` bytes and
    /// still fit on the home disk (without one there is nothing to check).
    fn reserve(&self, extra: usize) -> Result<(), FsError> {
        match &self.store {
            Some(store) if extra > 0 && self.home_len() + extra > store.capacity() => Err(FsError::NoSpace),
            _ => Ok(()),
        }
    }

    /// Whether the writable file system may take `more` bytes of memory,
    /// where everything in it is kept: the system keeps [`MEMORY_RESERVE`]
    /// free for everything else. Beyond that, `NoSpace`, as a full disk
    /// would say.
    fn memory_for(&self, more: usize) -> Result<(), FsError> {
        if more == 0 {
            return Ok(());
        }
        let free = vrt::object::system_info().map_or(0, |i| i.free_memory);
        if (more as u64).saturating_add(MEMORY_RESERVE) > free {
            return Err(FsError::NoSpace);
        }
        Ok(())
    }

    /// Records a change to the writable file system.
    fn changed(&mut self) {
        if self.store.is_some() {
            let now = vrt::time::now_ns();
            let first = self.dirty.map_or(now, |(first, _)| first);
            self.dirty = Some((first, now));
        }
    }

    /// When unsaved changes are due to be saved.
    fn save_deadline(&self) -> u64 {
        match self.dirty {
            Some((first, last)) => {
                last.saturating_add(SAVE_QUIET_NS).min(first.saturating_add(SAVE_MAX_DELAY_NS)).max(self.retry_at)
            }
            None => vabi::DEADLINE_INFINITE,
        }
    }

    /// Saves `/home` to the home disk if anything changed. Changes a save
    /// failed to keep stay unsaved, to be tried again.
    fn save(&mut self) -> Result<(), FsError> {
        let Some(store) = &mut self.store else { return Ok(()) };
        let Some(dirty) = self.dirty.take() else { return Ok(()) };
        // Measured afresh, which keeps the running size true.
        let len = snapshot::len(&self.ram, &self.samples);
        self.home_len.set(Some(len));
        let r = store.save(&self.ram, &self.samples, len);
        if let Err(e) = r {
            println!("cannot save the home directory: {}", e);
            self.dirty = Some(dirty);
            self.retry_at = vrt::time::now_ns().saturating_add(SAVE_RETRY_NS);
        }
        r
    }

    /// Creates a file, directory or socket (`kind`), if there is room for
    /// it: memory for its node, and in `/home` room on the home disk for
    /// its record and contents (a socket has none).
    fn create_node(&mut self, m: Mount, comps: &[&str], kind: Kind) -> Result<NodeId, FsError> {
        let t = self.tree(m)?;
        // What is there, and what cannot be made, are said before what
        // there is no room for.
        if t.lookup(comps).is_ok() {
            return Err(FsError::Exists);
        }
        if t.read_only {
            return Err(FsError::ReadOnly);
        }
        let (name, dirs) = comps.split_last().ok_or(FsError::Exists)?;
        t.lookup(dirs)?;
        self.memory_for(NODE_MEMORY + name.len())?;
        let record = match &kind {
            _ if !in_home(m, comps) => None,
            Kind::File(d) => Some(snapshot::record_len(snapshot::path_len(comps), Some(self.stored(d)))),
            Kind::Dir(_) => Some(snapshot::record_len(snapshot::path_len(comps), None)),
            Kind::Socket(_) => None,
        };
        if let Some(record) = record {
            self.reserve(record)?;
        }
        let node = self.tree_mut(m)?.create(comps, kind)?;
        self.home_changed(record.unwrap_or(0), 0);
        self.changed();
        Ok(node)
    }

    /// Reads up to `len` bytes of an open file at `offset`.
    fn read_node(&self, m: Mount, node: NodeId, offset: u64, len: u32) -> Result<Vec<u8>, FsError> {
        let Some(Node { kind: Kind::File(d), .. }) = self.tree(m)?.node(node) else {
            return Err(FsError::BadFd);
        };
        let bytes = d.bytes();
        let start = offset.min(bytes.len() as u64) as usize;
        let end = (start + len.min(MAX_IO) as usize).min(bytes.len());
        Ok(bytes[start..end].to_vec())
    }

    /// Checks that an open file `node` may become `new_len` bytes long
    /// (`d` is it now): in memory, where its contents become its own first,
    /// and on the home disk if it is in `/home`. Returns what its record
    /// stores now if it is.
    fn room_for(&self, m: Mount, node: NodeId, d: &Data, new_len: usize) -> Result<Option<usize>, FsError> {
        let stored = self.node_in_home(m, node).then(|| self.stored(d));
        if let Some(stored) = stored {
            self.reserve(new_len.saturating_sub(stored))?;
        }
        self.memory_for(d.copy_cost() + new_len.saturating_sub(d.bytes().len()))?;
        Ok(stored)
    }

    /// Writes `data` into an open file at `offset` (at its end with
    /// `append`); returns where the data went.
    fn write_node(&mut self, m: Mount, node: NodeId, offset: u64, append: bool, data: &[u8]) -> Result<u64, FsError> {
        let t = self.tree(m)?;
        if t.read_only {
            return Err(FsError::ReadOnly);
        }
        let Some(Node { kind: Kind::File(d), .. }) = t.node(node) else { return Err(FsError::BadFd) };
        let len = d.bytes().len();
        let at = if append { len as u64 } else { offset };
        let end = at.checked_add(data.len() as u64).ok_or(FsError::NoSpace)?;
        if end > MAX_FILE {
            return Err(FsError::NoSpace);
        }
        if data.is_empty() {
            // Nothing to write: the file counts as changed now (`vfiles`'
            // `touch`), without its contents becoming its own.
            self.tree_mut(m)?.touch(node);
            self.changed();
            return Ok(at);
        }
        let new_len = len.max(end as usize);
        let stored = self.room_for(m, node, d, new_len)?;
        let tree = self.tree_mut(m)?;
        let Some(Node { kind: Kind::File(d), .. }) = tree.node_mut(node) else { return Err(FsError::BadFd) };
        let v = d.make_mut()?;
        let (at, end) = (at as usize, end as usize);
        if v.len() < end {
            v.try_reserve_exact(end - v.len()).map_err(|_| FsError::NoSpace)?;
            v.resize(end, 0);
        }
        v[at..end].copy_from_slice(data);
        tree.touch(node);
        if let Some(stored) = stored {
            self.home_changed(new_len, stored);
        }
        self.changed();
        Ok(at as u64)
    }

    /// Sets the length of an open file.
    fn truncate_node(&mut self, m: Mount, node: NodeId, len: u64) -> Result<(), FsError> {
        let t = self.tree(m)?;
        if t.read_only {
            return Err(FsError::ReadOnly);
        }
        if len > MAX_FILE {
            return Err(FsError::NoSpace);
        }
        let Some(Node { kind: Kind::File(d), .. }) = t.node(node) else { return Err(FsError::BadFd) };
        let len = len as usize;
        if len == d.bytes().len() {
            // Its length stays: it counts as changed, without its contents
            // becoming its own.
            self.tree_mut(m)?.touch(node);
            self.changed();
            return Ok(());
        }
        let stored = self.room_for(m, node, d, len)?;
        let tree = self.tree_mut(m)?;
        let Some(Node { kind: Kind::File(d), .. }) = tree.node_mut(node) else { return Err(FsError::BadFd) };
        let v = d.make_mut()?;
        if v.len() < len {
            v.try_reserve_exact(len - v.len()).map_err(|_| FsError::NoSpace)?;
        }
        v.resize(len, 0);
        tree.touch(node);
        if let Some(stored) = stored {
            self.home_changed(len, stored);
        }
        self.changed();
        Ok(())
    }

    /// Finds or creates the file at `comps` for opening with `flags`
    /// (truncating it if asked to).
    fn open_node(&mut self, m: Mount, comps: &[&str], flags: u32) -> Result<NodeId, FsError> {
        let writing = flags & (open_flags::WRITE | open_flags::CREATE | open_flags::TRUNCATE) != 0;
        if writing && self.tree(m)?.read_only {
            return Err(FsError::ReadOnly);
        }
        let node = match self.tree(m)?.lookup(comps) {
            Ok(_) if flags & open_flags::CREATE != 0 && flags & open_flags::EXCLUSIVE != 0 => {
                return Err(FsError::Exists);
            }
            Ok(n) => n,
            Err(FsError::NotFound) if flags & open_flags::CREATE != 0 => self.create_node(m, comps, Kind::file())?,
            Err(e) => return Err(e),
        };
        // What its record stores, if it is in /home and is to be emptied.
        let stored = match self.tree(m)?.node(node).map(|n| &n.kind) {
            Some(Kind::Dir(_)) => return Err(FsError::IsDir),
            Some(Kind::Socket(_)) => return Err(FsError::IsSocket),
            Some(Kind::File(d)) if flags & open_flags::TRUNCATE != 0 && !d.bytes().is_empty() => {
                Some(self.node_in_home(m, node).then(|| self.stored(d)))
            }
            Some(Kind::File(_)) => None,
            None => return Err(FsError::NotFound),
        };
        if flags & open_flags::TRUNCATE != 0 {
            let tree = self.tree_mut(m)?;
            if let (Some(_), Some(Node { kind: Kind::File(d), .. })) = (stored, tree.node_mut(node)) {
                *d = Data::empty();
            }
            tree.touch(node);
        }
        if let Some(home) = stored {
            self.home_changed(0, home.unwrap_or(0));
            self.changed();
        }
        Ok(node)
    }
}

/// File connections created while handling a request, for the service
/// loop to wait on.
type NewFiles = Vec<(Channel, Shared)>;

/// The file systems listed in the root besides the writable one's entries,
/// in the order of their names.
const MOUNTS: [&str; 2] = [DEV, SYSTEM];

/// One client connection.
struct Session<'a> {
    fs: &'a mut Fs,
    fds: &'a mut BTreeMap<u32, OpenFile>,
    /// Who the client is (decides access to private directories).
    who: &'a ClientIdentity,
    /// It gave up access to private directories (`vfs::renounce_private`).
    renounced: &'a mut bool,
    new_files: &'a mut NewFiles,
}

impl Session<'_> {
    /// The client, as far as private directories go.
    fn access(&self) -> access::Client<'_> {
        access::Client { service: self.who.service && !*self.renounced, name: &self.who.name }
    }

    /// Routes a path, refusing paths inside private directories the client
    /// may not reach.
    fn resolve<'p>(&self, path: &'p str) -> Result<(Mount, Vec<&'p str>), FsError> {
        let (m, comps) = self.fs.route(path)?;
        if m == Mount::Ram && !self.access().may_reach(&comps) {
            return Err(FsError::Denied);
        }
        Ok((m, comps))
    }

    /// How many entries a listing of the writable file system's directory
    /// `comps`, which holds `size`, shows the client (as `Stat::size` says):
    /// the root shows the other file systems too, and private directories
    /// are hidden.
    fn entries_seen(&self, comps: &[&str], size: u64) -> u64 {
        let ram = &self.fs.ram;
        match comps {
            [] => size + MOUNTS.len() as u64,
            [HOME] if !self.access().sees(comps, PRIVATE) && ram.lookup(&[HOME, PRIVATE]).is_ok() => {
                size.saturating_sub(1)
            }
            [HOME, PRIVATE] => {
                (self.access().service && ram.lookup(&[HOME, PRIVATE, self.who.name.as_str()]).is_ok()) as u64
            }
            _ => size,
        }
    }

    /// An entry for a file system listed in the root.
    fn mount_entry(&self, name: &str) -> DirEntry {
        let size = match name {
            DEV => Dev::list().len() as u64,
            _ => self.fs.system.stat(tree::ROOT).map_or(0, |s| s.size),
        };
        DirEntry {
            name: name.into(),
            is_dir: true,
            size,
            modified: 0,
            inode: tree::ROOT,
            executable: false,
            socket: false,
        }
    }

    /// Renames `from` to `to`, replacing what is there if asked to.
    fn move_node(&mut self, from: &str, to: &str, replace: bool) -> Result<(), FsError> {
        let (m1, c1) = self.resolve(from)?;
        let (m2, c2) = self.resolve(to)?;
        if m1 != m2 {
            return Err(FsError::Invalid);
        }
        if m1 == Mount::Ram && (access::fixed(&c1) || (replace && access::fixed(&c2))) {
            return Err(FsError::Denied);
        }
        // What the move adds to /home's snapshot and takes from it.
        let (home1, home2) = (in_home(m1, &c1), in_home(m2, &c2));
        let (mut added, mut removed) = (0, 0);
        if self.fs.store.is_some() && (home1 || home2) {
            let t = self.fs.tree(m1)?;
            let node = t.lookup(&c1)?;
            let (p1, p2) = (snapshot::path_len(&c1), snapshot::path_len(&c2));
            let samples = &self.fs.samples;
            match (home1, home2) {
                // Within /home only the paths in the records change.
                (true, true) if p1 != p2 => {
                    let nodes = t.measure(node).nodes;
                    if p2 > p1 {
                        added = nodes * (p2 - p1);
                    } else {
                        removed = nodes * (p1 - p2);
                    }
                }
                // Moving in from elsewhere (/tmp) brings the contents along.
                (false, true) => added = snapshot::subtree_len(t, node, p2, samples),
                (true, false) => removed = snapshot::subtree_len(t, node, p1, samples),
                _ => {}
            }
            if replace
                && home2
                && let Ok(old) = t.lookup(&c2)
                && old != node
            {
                removed += match t.node(old).map(|n| &n.kind) {
                    Some(Kind::File(d)) => snapshot::record_len(p2, Some(self.fs.stored(d))),
                    Some(Kind::Socket(_)) => 0,
                    _ => snapshot::record_len(p2, None),
                };
            }
            self.fs.reserve(added.saturating_sub(removed))?;
        }
        if let Some(old) = self.fs.tree_mut(m1)?.rename(&c1, &c2, replace)? {
            self.fs.dispose(m1, old);
        }
        self.fs.home_changed(added, removed);
        self.fs.changed();
        Ok(())
    }

    /// Makes a connection for an open file description, counted against
    /// the program that opened it.
    fn connect_file(&mut self, desc: Shared) -> Result<Channel, FsError> {
        let (ours, theirs) = Channel::create().map_err(|_| FsError::NoSpace)?;
        self.new_files.push((ours, desc));
        Ok(theirs)
    }
}

impl vfs::Server for Session<'_> {
    fn open(&mut self, path: String, flags: u32) -> Result<u32, FsError> {
        let (m, comps) = self.resolve(&path)?;
        if m == Mount::Dev {
            return Err(FsError::Invalid);
        }
        if self.fds.len() >= MAX_FDS {
            return Err(FsError::TooMany);
        }
        let node = self.fs.open_node(m, &comps, flags)?;
        self.fs.hold(m, node);
        let fd = (1..).find(|fd| !self.fds.contains_key(fd)).unwrap_or(0);
        self.fds.insert(fd, OpenFile { mount: m, node, flags });
        Ok(fd)
    }

    fn close(&mut self, fd: u32) -> Result<(), FsError> {
        let f = self.fds.remove(&fd).ok_or(FsError::BadFd)?;
        self.fs.release(f.mount, f.node);
        Ok(())
    }

    fn read(&mut self, fd: u32, offset: u64, len: u32) -> Result<Bytes, FsError> {
        let f = self.fds.get(&fd).ok_or(FsError::BadFd)?;
        self.fs.read_node(f.mount, f.node, offset, len).map(Bytes)
    }

    fn write(&mut self, fd: u32, offset: u64, data: Bytes) -> Result<u32, FsError> {
        let f = self.fds.get(&fd).ok_or(FsError::BadFd)?;
        if f.flags & open_flags::WRITE == 0 {
            return Err(FsError::BadFd);
        }
        let (m, node, append) = (f.mount, f.node, f.flags & open_flags::APPEND != 0);
        self.fs.write_node(m, node, offset, append, &data.0)?;
        Ok(data.0.len() as u32)
    }

    fn stat(&mut self, path: String) -> Result<Stat, FsError> {
        let (m, comps) = self.resolve(&path)?;
        if m == Mount::Dev {
            return Ok(Dev::stat(Dev::find(&comps)?));
        }
        let t = self.fs.tree(m)?;
        let mut st = t.stat(t.lookup(&comps)?)?;
        if m == Mount::Ram && st.is_dir {
            st.size = self.entries_seen(&comps, st.size);
        }
        Ok(st)
    }

    fn read_dir_page(&mut self, path: String, after: String, max: u32) -> Result<DirPage, FsError> {
        let (m, comps) = self.resolve(&path)?;
        if m == Mount::Dev {
            return match Dev::find(&comps)? {
                None => Ok(tree::page(Dev::list().into_iter().filter(|e| *e.name > *after), max)),
                Some(_) => Err(FsError::NotDir),
            };
        }
        let t = self.fs.tree(m)?;
        let entries = t.entries_after(t.lookup(&comps)?, &after)?;
        if m != Mount::Ram {
            return Ok(tree::page(entries, max));
        }
        let mounts: &[&str] = if comps.is_empty() { &MOUNTS } else { &[] };
        let mounts = mounts.iter().filter(|name| **name > after.as_str()).map(|name| self.mount_entry(name));
        let access = self.access();
        let mut page = tree::page(tree::merge(entries, mounts).filter(|e| access.sees(&comps, &e.name)), max);
        // A directory's size is what a listing of it shows (which differs
        // only for `/home` and `/home/.private`).
        if comps.len() < 2 {
            for e in page.entries.iter_mut().filter(|e| e.is_dir) {
                let child = [comps.as_slice(), &[e.name.as_str()]].concat();
                e.size = self.entries_seen(&child, e.size);
            }
        }
        Ok(page)
    }

    fn mkdir(&mut self, path: String) -> Result<(), FsError> {
        let (m, comps) = self.resolve(&path)?;
        if m == Mount::Dev {
            // Its devices and itself exist; nothing can be added.
            return Err(if Dev::find(&comps).is_ok() { FsError::Exists } else { FsError::ReadOnly });
        }
        self.fs.create_node(m, &comps, Kind::dir())?;
        Ok(())
    }

    fn remove(&mut self, path: String) -> Result<(), FsError> {
        let (m, comps) = self.resolve(&path)?;
        if m == Mount::Ram && access::fixed(&comps) {
            return Err(FsError::Denied);
        }
        // The record it takes from /home's snapshot.
        let record = match in_home(m, &comps) {
            true => {
                let t = self.fs.tree(m)?;
                match t.node(t.lookup(&comps)?).map(|n| &n.kind) {
                    Some(Kind::File(d)) => snapshot::record_len(snapshot::path_len(&comps), Some(self.fs.stored(d))),
                    Some(Kind::Socket(_)) => 0,
                    _ => snapshot::record_len(snapshot::path_len(&comps), None),
                }
            }
            false => 0,
        };
        let node = self.fs.tree_mut(m)?.unlink(&comps)?;
        self.fs.home_changed(0, record);
        self.fs.dispose(m, node);
        self.fs.changed();
        Ok(())
    }

    fn rename(&mut self, from: String, to: String) -> Result<(), FsError> {
        self.move_node(&from, &to, false)
    }

    fn read_file(&mut self, path: String) -> Result<(Vmo, u64), FsError> {
        let (m, comps) = self.resolve(&path)?;
        let t = self.fs.tree(m).map_err(|_| FsError::Invalid)?;
        let d = match t.node(t.lookup(&comps)?).map(|n| &n.kind) {
            Some(Kind::File(d)) => d,
            Some(Kind::Dir(_)) => return Err(FsError::IsDir),
            Some(Kind::Socket(_)) => return Err(FsError::IsSocket),
            None => return Err(FsError::NotFound),
        };
        let bytes = d.bytes();
        let vmo = Vmo::create(bytes.len().max(1)).map_err(|_| FsError::NoSpace)?;
        vmo.write(0, bytes).map_err(|_| FsError::Io)?;
        Ok((vmo, bytes.len() as u64))
    }

    fn write_file(&mut self, path: String, data: Vmo, len: u64) -> Result<(), FsError> {
        let (m, comps) = self.resolve(&path)?;
        if len > 1 << 30 {
            return Err(FsError::NoSpace);
        }
        let t = self.fs.tree(m)?;
        if t.read_only {
            return Err(FsError::ReadOnly);
        }
        let existing = match t.lookup(&comps) {
            Ok(node) => Some(node),
            Err(FsError::NotFound) => None,
            Err(e) => return Err(e),
        };
        // What the record of the file there stores, if it is in /home.
        let mut stored = None;
        if let Some(node) = existing {
            let d = match t.node(node).map(|n| &n.kind) {
                Some(Kind::File(d)) => d,
                Some(Kind::Socket(_)) => return Err(FsError::IsSocket),
                _ => return Err(FsError::IsDir),
            };
            if in_home(m, &comps) {
                stored = Some(self.fs.stored(d));
                self.fs.reserve((len as usize).saturating_sub(self.fs.stored(d)))?;
            }
        }
        // The new contents are all read before the old ones go.
        self.fs.memory_for(len as usize)?;
        let mut buf = Vec::new();
        buf.try_reserve_exact(len as usize).map_err(|_| FsError::NoSpace)?;
        buf.resize(len as usize, 0);
        data.read(0, &mut buf).map_err(|_| FsError::Invalid)?;
        match existing {
            Some(node) => {
                let tree = self.fs.tree_mut(m)?;
                if let Some(Node { kind: Kind::File(d), .. }) = tree.node_mut(node) {
                    *d = Data::owned(buf);
                }
                tree.touch(node);
                if let Some(stored) = stored {
                    self.fs.home_changed(len as usize, stored);
                }
                self.fs.changed();
            }
            None => {
                self.fs.create_node(m, &comps, Kind::File(Data::owned(buf)))?;
            }
        }
        Ok(())
    }

    fn truncate(&mut self, fd: u32, len: u64) -> Result<(), FsError> {
        let f = self.fds.get(&fd).ok_or(FsError::BadFd)?;
        if f.flags & open_flags::WRITE == 0 {
            return Err(FsError::BadFd);
        }
        let (m, node) = (f.mount, f.node);
        self.fs.truncate_node(m, node, len.min(MAX_FILE))
    }

    fn sync(&mut self) -> Result<(), FsError> {
        self.fs.save()
    }

    fn space(&mut self, path: String) -> Result<Space, FsError> {
        let (m, comps) = self.resolve(&path)?;
        if m == Mount::Dev {
            return Ok(Space { total: 0, used: 0, persistent: false });
        }
        let t = self.fs.tree(m)?;
        t.lookup(&comps)?;
        Ok(match (m, &self.fs.store) {
            (Mount::Ram, Some(store)) if in_home(m, &comps) => {
                Space { total: store.capacity() as u64, used: self.fs.home_len() as u64, persistent: true }
            }
            (Mount::Ram, _) => {
                // Kept in memory: as much as the free memory allows, but for
                // what is left to the rest of the system.
                let bytes = t.measure(tree::ROOT).bytes as u64;
                let free = vrt::object::system_info().map(|i| i.free_memory).unwrap_or(0);
                let room = free.saturating_sub(MEMORY_RESERVE);
                Space { total: bytes + room, used: bytes, persistent: false }
            }
            _ => Space { total: self.fs.system_bytes, used: self.fs.system_bytes, persistent: true },
        })
    }

    fn open_file(&mut self, path: String, flags: u32) -> Result<(Channel, Stat), FsError> {
        let (m, comps) = self.resolve(&path)?;
        let owner = self.who.koid;
        self.fs.connections.admit(owner)?;
        let opened = (|| {
            let (target, stat) = if m == Mount::Dev {
                match Dev::find(&comps)? {
                    Some(d) => (Target::Dev(d), Dev::stat(Some(d))),
                    None => return Err(FsError::IsDir),
                }
            } else {
                let node = self.fs.open_node(m, &comps, flags)?;
                (Target::Node(m, node), self.fs.tree(m)?.stat(node)?)
            };
            let desc = Rc::new(RefCell::new(Description { target, flags, offset: 0, owner }));
            let channel = self.connect_file(desc)?;
            if let Target::Node(m, node) = target {
                self.fs.hold(m, node);
            }
            Ok((channel, stat))
        })();
        if opened.is_err() {
            self.fs.connections.release(owner);
        }
        opened
    }

    fn replace(&mut self, from: String, to: String) -> Result<(), FsError> {
        self.move_node(&from, &to, true)
    }

    fn set_modified(&mut self, path: String, modified: u64) -> Result<(), FsError> {
        let (m, comps) = self.resolve(&path)?;
        let tree = self.fs.tree_mut(m)?;
        if tree.read_only {
            return Err(FsError::ReadOnly);
        }
        let node = tree.lookup(&comps)?;
        if modified == 0 {
            tree.touch(node);
        } else {
            tree.set_modified(node, modified);
        }
        self.fs.changed();
        Ok(())
    }

    fn copy_file(&mut self, from: String, to: String) -> Result<(), FsError> {
        let (m1, c1) = self.resolve(&from)?;
        let (m2, c2) = self.resolve(&to)?;
        if m1 == Mount::Dev {
            return Err(FsError::Invalid);
        }
        let t = self.fs.tree(m1)?;
        let contents = match t.node(t.lookup(&c1)?).map(|n| &n.kind) {
            Some(Kind::File(d)) => d.share(),
            Some(Kind::Dir(_)) => return Err(FsError::IsDir),
            Some(Kind::Socket(_)) => return Err(FsError::IsSocket),
            None => return Err(FsError::NotFound),
        };
        self.fs.create_node(m2, &c2, Kind::File(contents))?;
        Ok(())
    }

    fn renounce_private(&mut self) -> Result<(), FsError> {
        *self.renounced = true;
        Ok(())
    }

    fn bind_socket(
        &mut self,
        at: SocketAddress,
        kind: u32,
        name: Bytes,
        koid: u64,
        inbox: Channel,
    ) -> Result<u64, FsError> {
        if !matches!(kind, socket_kind::STREAM | socket_kind::DATAGRAM | socket_kind::SEQPACKET)
            || name.0.is_empty()
            || name.0.len() > MAX_SOCKET_NAME
            || at.abstract_name.0.len() >= MAX_SOCKET_NAME
        {
            return Err(FsError::Invalid);
        }
        let owner = self.who.koid;
        if at.is_abstract() {
            return self.fs.sockets.bind_abstract(owner, koid, kind, at.abstract_name.0, name.0, inbox);
        }
        let (m, comps) = self.resolve(&at.path)?;
        if m != Mount::Ram {
            // `/system` and `/dev` take nothing new.
            let exists = match m {
                Mount::Dev => Dev::find(&comps).is_ok(),
                _ => self.fs.tree(m)?.lookup(&comps).is_ok(),
            };
            return Err(if exists { FsError::Exists } else { FsError::ReadOnly });
        }
        let id = self.fs.sockets.reserve(owner)?;
        let node = self.fs.create_node(m, &comps, Kind::Socket(id))?;
        Ok(self.fs.sockets.bind(owner, koid, kind, name.0, inbox, node))
    }

    fn listen_socket(&mut self, id: u64) -> Result<(), FsError> {
        self.fs.sockets.listen(id, self.who.koid)
    }

    fn connect_socket(
        &mut self,
        to: SocketAddress,
        kind: u32,
        from: u64,
        koid: u64,
        ends: Vec<Handle>,
    ) -> Result<SocketPeer, FsError> {
        let id = self.socket_at(&to, kind)?;
        let me = self.fs.sockets.peer(from, self.who.koid, koid);
        self.fs.sockets.connect(id, kind, me, ends)
    }

    fn send_to_socket(
        &mut self,
        to: SocketAddress,
        from: u64,
        koid: u64,
        data: Bytes,
        rights: Bytes,
        handles: Vec<Handle>,
    ) -> Result<(), FsError> {
        if data.0.len() > MAX_DATAGRAM {
            return Err(FsError::Invalid);
        }
        let id = self.socket_at(&to, socket_kind::DATAGRAM)?;
        let from = self.fs.sockets.peer(from, self.who.koid, koid);
        self.fs.sockets.send(id, SocketDatagram { from, data, rights, handles })
    }
}

impl Session<'_> {
    /// The socket bound at `to`: at its path (`Refused` for something else
    /// there, as Linux has it), or to its abstract name, of `kind`'s
    /// (`Refused` for none).
    fn socket_at(&mut self, to: &SocketAddress, kind: u32) -> Result<u64, FsError> {
        if to.is_abstract() {
            return self.fs.sockets.find_abstract(kind, &to.abstract_name.0);
        }
        let (m, comps) = self.resolve(&to.path)?;
        if m == Mount::Dev {
            return Dev::find(&comps).and(Err(FsError::Refused));
        }
        let t = self.fs.tree(m)?;
        match t.node(t.lookup(&comps)?).map(|n| &n.kind) {
            Some(Kind::Socket(id)) => Ok(*id),
            Some(_) => Err(FsError::Refused),
            None => Err(FsError::NotFound),
        }
    }
}

/// One connection to an open file.
struct FileSession<'a> {
    fs: &'a mut Fs,
    desc: &'a Shared,
    new_files: &'a mut NewFiles,
}

impl FileSession<'_> {
    fn readable(&self) -> Result<(), FsError> {
        if self.desc.borrow().flags & open_flags::READ != 0 { Ok(()) } else { Err(FsError::BadFd) }
    }

    fn writable(&self) -> Result<(), FsError> {
        if self.desc.borrow().flags & open_flags::WRITE != 0 { Ok(()) } else { Err(FsError::BadFd) }
    }

    fn read_at_offset(&mut self, offset: u64, len: u32) -> Result<Vec<u8>, FsError> {
        self.readable()?;
        let target = self.desc.borrow().target;
        match target {
            Target::Node(m, node) => self.fs.read_node(m, node, offset, len),
            Target::Dev(d) => d.read(len.min(MAX_IO) as usize),
        }
    }

    /// Writes at `offset` (at the end in append mode); returns where.
    fn write_at_offset(&mut self, offset: u64, data: &[u8]) -> Result<u64, FsError> {
        self.writable()?;
        let d = self.desc.borrow();
        let (target, append) = (d.target, d.flags & open_flags::APPEND != 0);
        drop(d);
        match target {
            Target::Node(m, node) => self.fs.write_node(m, node, offset, append, data),
            Target::Dev(dev) => dev.write(data.len()).map(|_| offset),
        }
    }
}

impl file::Server for FileSession<'_> {
    fn read(&mut self, len: u32) -> Result<Bytes, FsError> {
        let offset = self.desc.borrow().offset;
        let data = self.read_at_offset(offset, len)?;
        let target = self.desc.borrow().target;
        if let Target::Node(..) = target {
            self.desc.borrow_mut().offset = offset + data.len() as u64;
        }
        Ok(Bytes(data))
    }

    fn write(&mut self, data: Bytes) -> Result<u32, FsError> {
        let offset = self.desc.borrow().offset;
        let at = self.write_at_offset(offset, &data.0)?;
        let target = self.desc.borrow().target;
        if let Target::Node(..) = target {
            self.desc.borrow_mut().offset = at + data.0.len() as u64;
        }
        Ok(data.0.len() as u32)
    }

    fn read_at(&mut self, offset: u64, len: u32) -> Result<Bytes, FsError> {
        self.read_at_offset(offset, len).map(Bytes)
    }

    fn write_at(&mut self, offset: u64, data: Bytes) -> Result<u32, FsError> {
        self.write_at_offset(offset, &data.0)?;
        Ok(data.0.len() as u32)
    }

    fn seek(&mut self, offset: i64, whence: u32) -> Result<u64, FsError> {
        let target = self.desc.borrow().target;
        let Target::Node(m, node) = target else { return Ok(0) };
        let base = match whence {
            seek::SET => 0,
            seek::CURRENT => self.desc.borrow().offset,
            seek::END => self.fs.tree(m)?.stat(node)?.size,
            _ => return Err(FsError::Invalid),
        };
        let new = base.checked_add_signed(offset).ok_or(FsError::Invalid)?;
        if new > i64::MAX as u64 {
            return Err(FsError::Invalid);
        }
        self.desc.borrow_mut().offset = new;
        Ok(new)
    }

    fn stat(&mut self) -> Result<Stat, FsError> {
        match self.desc.borrow().target {
            Target::Node(m, node) => self.fs.tree(m)?.stat(node),
            Target::Dev(d) => Ok(Dev::stat(Some(d))),
        }
    }

    fn truncate(&mut self, len: u64) -> Result<(), FsError> {
        self.writable()?;
        let target = self.desc.borrow().target;
        match target {
            Target::Node(m, node) => self.fs.truncate_node(m, node, len),
            Target::Dev(_) => Ok(()),
        }
    }

    fn duplicate(&mut self) -> Result<Channel, FsError> {
        let owner = self.desc.borrow().owner;
        self.fs.connections.admit(owner)?;
        match Channel::create() {
            Ok((ours, theirs)) => {
                self.new_files.push((ours, self.desc.clone()));
                Ok(theirs)
            }
            Err(_) => {
                self.fs.connections.release(owner);
                Err(FsError::NoSpace)
            }
        }
    }

    fn flags(&mut self) -> u32 {
        self.desc.borrow().flags
    }

    fn set_append(&mut self, append: bool) -> Result<(), FsError> {
        let mut d = self.desc.borrow_mut();
        if append {
            d.flags |= open_flags::APPEND;
        } else {
            d.flags &= !open_flags::APPEND;
        }
        Ok(())
    }

    fn set_modified(&mut self, modified: u64) -> Result<(), FsError> {
        let Target::Node(m, node) = self.desc.borrow().target else { return Ok(()) };
        let tree = self.fs.tree_mut(m)?;
        if tree.read_only {
            return Err(FsError::ReadOnly);
        }
        if modified == 0 {
            tree.touch(node);
        } else {
            tree.set_modified(node, modified);
        }
        self.fs.changed();
        Ok(())
    }
}

/// A writable tree with `/tmp` and an empty `/home`.
fn fresh_ram() -> Tree {
    let mut ram = Tree::new(false, device::RAM, vrt::time::unix_time_ns);
    let _ = ram.mkdir_all(&[vfs_core::TMP]);
    let _ = ram.mkdir_all(&[HOME, USER]);
    ram
}

/// Builds the file systems: the system image from the initrd, and the home
/// directory from the home disk or, failing that, the samples. A `live`
/// system keeps the home directory in memory without looking for a disk.
fn build(archive: &initrd::Archive<'static>, live: bool) -> Fs {
    let mut system = Tree::new(true, device::SYSTEM, vrt::time::unix_time_ns);
    for f in archive.files() {
        if let Ok(comps) = tree::components(f.path) {
            let _ = system.add_file(&comps, Data::Static(f.data));
        }
    }
    let system_bytes = system.measure(tree::ROOT).bytes as u64;
    let samples = Samples::new(archive.files().map(|f| (f.path, f.data)));
    let mut store = if live { None } else { Store::connect() };
    let restored = store.as_mut().and_then(|s| s.load(fresh_ram, &samples));
    let found = restored.is_some();
    let mut ram = restored.unwrap_or_else(|| {
        let mut ram = fresh_ram();
        samples.seed(&mut ram, None);
        ram
    });
    for dir in HOME_DIRS {
        if let Ok(comps) = tree::components(dir) {
            let _ = ram.mkdir_all(&comps);
        }
    }
    let home = match (&store, found) {
        (None, _) if live => "in memory (live system)",
        (None, _) => "in memory (no home disk)",
        (Some(_), true) => "restored from the home disk",
        (Some(_), false) => "new, on the home disk",
    };
    println!("home directory {}", home);
    Fs {
        system,
        system_bytes,
        ram,
        samples,
        store,
        dirty: None,
        retry_at: 0,
        open: BTreeMap::new(),
        orphans: BTreeSet::new(),
        connections: Connections::new(),
        sockets: Sockets::new(),
        home_len: Cell::new(None),
    }
}

fn main() -> i32 {
    let Some(initrd_vmo) = vrt::env::take_handle(vabi::startup::role::INITRD).map(Vmo::from_handle) else {
        println!("no initrd handle");
        return 1;
    };
    let size = initrd_vmo.size().unwrap_or(0);
    let Ok(addr) = initrd_vmo.map(0, size, vabi::map_flags::READ) else {
        println!("cannot map the initrd");
        return 1;
    };
    // SAFETY: the mapping is read-only and lives as long as the process.
    let bytes: &'static [u8] = unsafe { core::slice::from_raw_parts(addr as *const u8, size) };
    let Ok(archive) = initrd::Archive::open(bytes) else {
        println!("the initrd is corrupt");
        return 1;
    };
    let mut fs = build(&archive, vrt::env::args().iter().any(|a| a == "live"));

    let Ok(listener) = vproto::register(vfs::NAME) else {
        println!("cannot register the vfs service");
        return 1;
    };
    println!("serving /system ({} files), /dev and /home", archive.len());

    struct Client {
        channel: Channel,
        fds: BTreeMap<u32, OpenFile>,
        who: ClientIdentity,
        /// It gave up access to private directories.
        renounced: bool,
    }
    struct FileConn {
        channel: Channel,
        desc: Shared,
    }
    const FILE_KEY: u64 = 1 << 62;
    let mut clients: BTreeMap<u64, Client> = BTreeMap::new();
    let mut files: BTreeMap<u64, FileConn> = BTreeMap::new();
    let mut next_id = 1u64;
    let mut new_files = NewFiles::new();
    // Kept from one wait to the next: its handles stay registered with the
    // kernel, so that a wait costs what is ready, however many files are
    // open.
    let mut ws = WaitSet::new();
    loop {
        ws.clear();
        ws.add(listener.raw(), signals::READABLE | signals::PEER_CLOSED, 0);
        for (&id, c) in &clients {
            ws.add(c.channel.raw(), signals::READABLE | signals::PEER_CLOSED, id);
        }
        for (&id, f) in &files {
            ws.add(f.channel.raw(), signals::READABLE | signals::PEER_CLOSED, FILE_KEY | id);
        }
        let Ok(ready) = ws.wait(fs.save_deadline()) else { continue };
        for (key, observed) in ready {
            if key == 0 {
                while let Some((ch, who)) = vproto::accept_with_identity(&listener) {
                    // Refused: the channel closes, and the client's calls fail.
                    if let Err(e) = fs.connections.admit(who.koid) {
                        println!("refusing a connection of {} ({}): {}", who.name, who.koid, e);
                        continue;
                    }
                    clients.insert(next_id, Client { channel: ch, fds: BTreeMap::new(), who, renounced: false });
                    next_id += 1;
                }
                continue;
            }
            if key & FILE_KEY != 0 {
                let id = key & !FILE_KEY;
                let Some(conn) = files.get(&id) else { continue };
                if observed & signals::READABLE != 0 {
                    while let Ok(msg) = conn.channel.read() {
                        let mut session = FileSession { fs: &mut fs, desc: &conn.desc, new_files: &mut new_files };
                        match file::dispatch(&mut session, msg) {
                            Ok(reply) => {
                                let _ = reply.send(&conn.channel);
                            }
                            Err(e) => println!("bad file request: {}", e),
                        }
                    }
                } else if observed & signals::PEER_CLOSED != 0
                    && let Some(conn) = files.remove(&id)
                {
                    let desc = conn.desc.borrow();
                    fs.connections.release(desc.owner);
                    // The last connection closes the open file.
                    if Rc::strong_count(&conn.desc) == 1
                        && let Target::Node(m, node) = desc.target
                    {
                        fs.release(m, node);
                    }
                }
            } else {
                let Some(client) = clients.get_mut(&key) else { continue };
                if observed & signals::READABLE != 0 {
                    while let Ok(msg) = client.channel.read() {
                        let mut session = Session {
                            fs: &mut fs,
                            fds: &mut client.fds,
                            who: &client.who,
                            renounced: &mut client.renounced,
                            new_files: &mut new_files,
                        };
                        match vfs::dispatch(&mut session, msg) {
                            Ok(reply) => {
                                let _ = reply.send(&client.channel);
                            }
                            Err(e) => println!("bad request: {}", e),
                        }
                    }
                } else if observed & signals::PEER_CLOSED != 0
                    && let Some(client) = clients.remove(&key)
                {
                    fs.connections.release(client.who.koid);
                    for f in client.fds.values() {
                        fs.release(f.mount, f.node);
                    }
                }
            }
            for (channel, desc) in new_files.drain(..) {
                files.insert(next_id, FileConn { channel, desc });
                next_id += 1;
            }
        }
        if vrt::time::now_ns() >= fs.save_deadline() {
            let _ = fs.save();
        }
    }
}
