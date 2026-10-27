// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `filesystem` — the Veda file system service.
//!
//! Namespace:
//! * `/system` — the read-only system image (the initrd),
//! * `/dev` — the devices (`null`, `zero`, `full`, `random`, `urandom`;
//!   see [`dev`]),
//! * `/dev/shm` — the shared memory objects (`filesystem_core::shm`,
//!   [`shm`]), and
//! * everything else — a writable in-memory file system holding the user's
//!   home directory and `/tmp`.
//!
//! The file systems themselves, their snapshots and the rules of who may
//! reach what are `filesystem-core`'s (`core/`), where the host tests them;
//! this program serves them.
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
//! **Shared memory objects.** A file of `/dev/shm` is memory of its own (a
//! VMO), which its file connections read and write, and hand out to map
//! (`file::memory`): every process that maps it shares it, as POSIX
//! programs share memory with `shm_open` and semaphores with `sem_open`.
//! An object may have several names (`vfs::link`), and lives while it has
//! one or is open; it is never kept on the home disk.
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
//! about the user), on the home disk like everything in `/home`. A renamed
//! service's directory follows the service: it moves from the old name to
//! the new as the file system starts (`filesystem_core::renamed`). A
//! connection can give that access up (`vfs::renounce_private`). The root,
//! `/home`, `/home/user`, `/tmp` and `/home/.private` cannot be removed,
//! moved or replaced (see `filesystem_core::access`).
//!
//! **Confined connections.** A connection can give up, for good, all but a
//! few places of the namespace (`vfs::confine`, `filesystem_core::view`):
//! it then reaches what is inside them, each to read or to change too, and
//! the directories on the way to them, whose listings show it what leads on
//! to a place; nothing else is there for it. A program that handles what it
//! cannot trust (the browser's page processes) confines its connection
//! before it does, and no longer reaches the VFS's registry name, so that
//! it opens no other. Its `sync` returns at once, what it changed being
//! saved as anything is. The VFS logs the first few paths each such
//! connection was refused where there is something.
//!
//! **Loans.** A connection that is not confined can lend a confined one a
//! place to read (`vfs::lend`, `vfs::borrow`, `filesystem_core::loans`):
//! the browser lends its network process the folder of a page the user
//! opens. The loan is a secret of random bytes, which the lender hands on
//! over a channel of its own and which is taken once, for the place it was
//! made for; the confined connection's view gains that place, to read, and
//! nothing else, until the lender takes the loan back with its secret
//! (`vfs::take_back`), or closes: the place leaves the view, and nothing
//! else does. A confined connection lends nothing, and no connection lends
//! the user's home folder whole, nor what holds it. The VFS logs each
//! place so lent, and so taken back.
//!
//! **Locks.** The VFS arbitrates advisory locks between processes, as
//! Linux does (`file::lock`, `filesystem_core::locks`): `flock`'s, held by
//! open file descriptions, and locks on bytes, held by processes (POSIX's
//! record locks) or by open file descriptions (OFD locks). An open file's
//! go as its last connection closes; a process owns its locks by a secret
//! drawn for the connection that speaks for it (`vfs::lock_owner`), and
//! they go as that connection closes, as the process ends; all of a file's
//! go when nothing has it open. A request that waits for a lock has a
//! channel of its own, on which the VFS says when it is granted, and which
//! counts as a connection of its program.
//!
//! **Limits.** No path is longer than `proto::fs::MAX_PATH`. A program
//! holds a bounded number of connections, and all of them together stay
//! within what the kernel waits on at once (`filesystem_core::access`).
//! Files grow only while memory is left for the rest of the system, and in
//! `/home` while they fit on the home disk.

#![no_std]
#![no_main]

extern crate alloc;

mod dev;
mod persist;
mod shm;
mod sockets;

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use abi::signals;
use ipc::{Bytes, WaitSet};
use proto::fs::{
    DirEntry, DirPage, FsError, LOAN_SIZE, LOCK_END, LOCK_OWNER_SIZE, LockHolder, LockOutcome, LockRange, MAX_DATAGRAM,
    MAX_IO, MAX_PLACES, MAX_SOCKET_NAME, Place, SocketAddress, SocketDatagram, SocketPeer, Space, Stat, device, file,
    lock_event, lock_kind, open_flags, seek, socket_kind, vfs,
};
use proto::init::ClientIdentity;
use rt::object::{Channel, Handle, Vmo};
use rt::println;

use dev::Dev;
use filesystem_core::access::{self, Connections, PACKAGES, PRIVATE};
use filesystem_core::loans::{Loans, TakenBack};
use filesystem_core::locks::{self, FileId, Locks, Outcome, Owner, Owners};
use filesystem_core::renamed;
use filesystem_core::shm::Shm;
use filesystem_core::snapshot::{self, Samples};
use filesystem_core::tree::{self, Data, Kind, Node, NodeId, Tree};
use filesystem_core::view::{Reach, View};
use filesystem_core::{HOME, USER};
use persist::Store;
use sockets::Sockets;

rt::entry!(main);

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
/// The paths a confined connection was refused that the log shows: the
/// first few, which say what the program looks for, and no more, whatever
/// it asks.
const LOGGED_REFUSALS: u32 = 8;

/// Which tree a path lives in.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Mount {
    System,
    Ram,
    Dev,
    /// `/dev/shm`, the shared memory objects.
    Shm,
}

impl Mount {
    /// The `Stat::device` of its files.
    fn device(self) -> u64 {
        match self {
            Mount::System => device::SYSTEM,
            Mount::Ram => device::RAM,
            Mount::Dev => device::DEV,
            Mount::Shm => device::SHM,
        }
    }
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
    /// A shared memory object.
    Memory(filesystem_core::shm::Id),
}

impl Target {
    /// The file, as its locks know it.
    fn file(self) -> FileId {
        match self {
            Target::Node(m, node) => (m.device(), node),
            Target::Dev(d) => (device::DEV, Dev::stat(Some(d)).inode),
            Target::Memory(id) => (device::SHM, id),
        }
    }
}

/// An open file description (behind every connection of the `file` kind
/// duplicated from one `open_file`).
struct Description {
    /// Its number, which its locks know it by (`Owner::Open`).
    id: u64,
    target: Target,
    flags: u32,
    offset: u64,
    /// The program that opened it, whose connections its connections are.
    owner: u64,
    /// Its file's modification time may be set through it: not when a
    /// confined connection opened it in a place it only reads.
    may_touch: bool,
}

/// A request that waits for a lock: the VFS's end of its channel, and the
/// program it counts as a connection of.
struct LockWait {
    channel: Channel,
    program: u64,
}

type Shared = Rc<RefCell<Description>>;

/// Whether a path is in `/home`, which is kept on the home disk.
fn in_home(m: Mount, comps: &[&str]) -> bool {
    m == Mount::Ram && comps.first() == Some(&HOME)
}

/// The system image (the initrd) in memory, which the system tree's files
/// are bytes of: slices of its VMO give them out as they are.
struct SystemImage {
    vmo: Vmo,
    /// Its mapping here.
    bytes: &'static [u8],
}

impl SystemImage {
    /// A VMO of `data`'s pages, if `data` is the image's from a page
    /// boundary (as its files are): read-only, as the image is, and shared
    /// with everyone else who reads them; programs run from these pages.
    fn pages_of(&self, data: &[u8]) -> Option<Vmo> {
        let offset = (data.as_ptr() as usize).checked_sub(self.bytes.as_ptr() as usize)?;
        let inside = offset.checked_add(data.len()).is_some_and(|end| end <= self.bytes.len());
        if data.is_empty() || !inside || !offset.is_multiple_of(abi::PAGE_SIZE) {
            return None;
        }
        self.vmo.slice(offset, data.len()).ok()
    }
}

struct Fs {
    system: Tree,
    /// The bytes of the system image's files (it never changes).
    system_bytes: u64,
    image: SystemImage,
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
    /// `/dev/shm`'s shared memory objects.
    shm: Shm<shm::Memory>,
    /// The loans made (`vfs::lend`): those not taken yet, and those whose
    /// place is in a view, which their lenders may take back.
    loans: Loans,
    /// The size of `/home`'s snapshot, while known: changes move it by what
    /// they add to the snapshot and take from it, and every save measures
    /// it afresh, so that checking a write against the home disk does not
    /// walk all of `/home`.
    home_len: Cell<Option<usize>>,
    /// The locks on files, and the requests waiting for one.
    locks: Locks,
    /// The processes that own locks (`vfs::lock_owner`).
    owners: Owners,
    /// The channels of the requests waiting for a lock, by their waiters'
    /// numbers.
    lock_waits: BTreeMap<u64, LockWait>,
    /// The number of the next open file description.
    next_open: u64,
}

impl Fs {
    /// Resolves a path to a tree and the components inside it.
    fn route<'p>(&self, path: &'p str) -> Result<(Mount, Vec<&'p str>), FsError> {
        Ok(Fs::route_comps(&tree::components(path)?))
    }

    /// The tree of the path of `comps` (from the root), and the components
    /// inside it.
    fn route_comps<'p>(comps: &[&'p str]) -> (Mount, Vec<&'p str>) {
        match comps {
            [SYSTEM, ..] => (Mount::System, comps[1..].to_vec()),
            [DEV, dev::SHM, ..] => (Mount::Shm, comps[2..].to_vec()),
            [DEV, ..] => (Mount::Dev, comps[1..].to_vec()),
            _ => (Mount::Ram, comps.to_vec()),
        }
    }

    /// Whether something is at `path`.
    fn exists(&self, path: &str) -> bool {
        match self.route(path) {
            Ok((Mount::Dev, comps)) => Dev::find(&comps).is_ok(),
            Ok((Mount::Shm, comps)) => self.shm.find(&comps).is_ok(),
            Ok((m, comps)) => self.tree(m).is_ok_and(|t| t.lookup(&comps).is_ok()),
            Err(_) => false,
        }
    }

    /// The tree of a mount (`/dev` has none: nothing there can change; nor
    /// has `/dev/shm`, whose objects are [`Fs::shm`]'s).
    fn tree(&self, m: Mount) -> Result<&Tree, FsError> {
        match m {
            Mount::System => Ok(&self.system),
            Mount::Ram => Ok(&self.ram),
            Mount::Dev | Mount::Shm => Err(FsError::ReadOnly),
        }
    }

    fn tree_mut(&mut self, m: Mount) -> Result<&mut Tree, FsError> {
        match m {
            Mount::System => Ok(&mut self.system),
            Mount::Ram => Ok(&mut self.ram),
            Mount::Dev | Mount::Shm => Err(FsError::ReadOnly),
        }
    }

    /// The entries of `/dev`: its devices, and the shared memory objects'
    /// directory, in the order of their names.
    fn dev_entries(&self) -> Vec<DirEntry> {
        let st = self.shm.stat(None).unwrap_or_default();
        let shm = DirEntry {
            name: dev::SHM.into(),
            is_dir: true,
            size: st.size,
            modified: st.modified,
            inode: st.inode,
            executable: false,
            socket: false,
        };
        tree::merge(Dev::list().into_iter(), core::iter::once(shm)).collect()
    }

    /// Copies `len` bytes of shared memory object `id` into a VMO of their
    /// own.
    fn copy_of_memory(&self, id: filesystem_core::shm::Id, len: u64) -> Result<Vmo, FsError> {
        self.memory_for(len as usize)?;
        let vmo = Vmo::create((len as usize).max(1)).map_err(|_| FsError::NoSpace)?;
        let mut at = 0;
        while at < len {
            let chunk = self.shm.read(id, at, MAX_IO as usize)?;
            if chunk.is_empty() {
                break;
            }
            vmo.write(at as usize, &chunk).map_err(|_| FsError::NoSpace)?;
            at += chunk.len() as u64;
        }
        Ok(vmo)
    }

    /// Something opened `node`.
    fn hold(&mut self, m: Mount, node: NodeId) {
        *self.open.entry((m, node)).or_default() += 1;
    }

    /// Something that had `node` open closed it. With the last, its locks
    /// go.
    fn release(&mut self, m: Mount, node: NodeId) {
        let Some(n) = self.open.get_mut(&(m, node)) else { return };
        *n -= 1;
        if *n == 0 {
            self.open.remove(&(m, node));
            self.locks.forget(Target::Node(m, node).file());
            if self.orphans.remove(&(m, node))
                && let Ok(t) = self.tree_mut(m)
            {
                t.forget(node);
            }
        }
    }

    /// Gives waiter `waiter`, a request that waits for a lock, a channel,
    /// counted as a connection of `program`: the client's end, on which the
    /// VFS says what became of the request ([`Fs::tell_lock_waits`]). If it
    /// cannot, the request waits no more.
    fn lock_wait(&mut self, waiter: u64, program: u64) -> Result<Channel, FsError> {
        let made = self.connections.admit(program).and_then(|()| {
            Channel::create().map_err(|_| {
                self.connections.release(program);
                FsError::NoSpace
            })
        });
        match made {
            Ok((ours, theirs)) => {
                self.lock_waits.insert(waiter, LockWait { channel: ours, program });
                Ok(theirs)
            }
            Err(e) => {
                self.locks.cancel(waiter);
                Err(e)
            }
        }
    }

    /// The request of waiter `waiter` waits no more: its channel closes.
    fn end_lock_wait(&mut self, waiter: u64) {
        if let Some(wait) = self.lock_waits.remove(&waiter) {
            self.connections.release(wait.program);
        }
    }

    /// Says on their channels what became of the requests that waited for
    /// locks (`lock_event::DONE`): each was granted, or never will be.
    fn tell_lock_waits(&mut self) {
        for notice in self.locks.notices() {
            if let Some(wait) = self.lock_waits.get(&notice.waiter) {
                // A client that gave up meanwhile is no longer there to tell.
                let _ = ipc::send_event(&wait.channel, lock_event::DONE, notice.result);
            }
            self.end_lock_wait(notice.waiter);
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
        let free = rt::object::system_info().map_or(0, |i| i.free_memory);
        if (more as u64).saturating_add(MEMORY_RESERVE) > free {
            return Err(FsError::NoSpace);
        }
        Ok(())
    }

    /// Records a change to the writable file system.
    fn changed(&mut self) {
        if self.store.is_some() {
            let now = rt::time::now_ns();
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
            None => abi::DEADLINE_INFINITE,
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
            self.retry_at = rt::time::now_ns().saturating_add(SAVE_RETRY_NS);
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
    /// (`d` is it now): in memory, for what resizing its contents takes
    /// ([`Data::resize_cost`]), and on the home disk if it is in `/home`.
    /// Returns what its record stores now if it is.
    fn room_for(&self, m: Mount, node: NodeId, d: &Data, new_len: usize) -> Result<Option<usize>, FsError> {
        let stored = self.node_in_home(m, node).then(|| self.stored(d));
        if let Some(stored) = stored {
            self.reserve(new_len.saturating_sub(stored))?;
        }
        self.memory_for(d.resize_cost(new_len))?;
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
            // Nothing to write: the file counts as changed now (`files`'
            // `touch`), without its contents becoming its own.
            self.tree_mut(m)?.touch(node);
            self.changed();
            return Ok(at);
        }
        let new_len = len.max(end as usize);
        let stored = self.room_for(m, node, d, new_len)?;
        let tree = self.tree_mut(m)?;
        let Some(Node { kind: Kind::File(d), .. }) = tree.node_mut(node) else { return Err(FsError::BadFd) };
        d.resize(new_len)?[at as usize..end as usize].copy_from_slice(data);
        tree.touch(node);
        if let Some(stored) = stored {
            self.home_changed(new_len, stored);
        }
        self.changed();
        Ok(at)
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
        d.resize(len)?;
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
    /// The connection's own number, which its loans are known by.
    id: u64,
    fds: &'a mut BTreeMap<u32, OpenFile>,
    /// Who the client is (decides access to private directories).
    who: &'a ClientIdentity,
    /// It gave up access to private directories (`vfs::renounce_private`).
    renounced: &'a mut bool,
    /// What it reaches, if it is confined (`vfs::confine`).
    view: &'a mut Option<View>,
    /// The refusals of its view so far, of which the log shows the first.
    refusals: &'a mut u32,
    new_files: &'a mut NewFiles,
    /// The places its requests took back from other connections' views,
    /// which the service loop takes out of them (`vfs::take_back`).
    taken_back: &'a mut Vec<TakenBack>,
}

impl Session<'_> {
    /// The client, as far as private directories go.
    fn access(&self) -> access::Client<'_> {
        access::Client { service: self.who.service && !*self.renounced, name: &self.who.name }
    }

    /// How the connection's view reaches the path of `full`, its
    /// components from the root: wholly, if it is not confined.
    fn reach(&self, full: &[&str]) -> Reach {
        self.view.as_ref().map_or(Reach::Write, |v| v.reach(full))
    }

    /// Routes a path that a request needs to reach as `need` (to look at,
    /// to read, to change), refusing what the connection's view does not
    /// reach so (`NotFound` where it reaches nothing), and paths inside
    /// private directories the client may not reach.
    fn resolve<'p>(&mut self, path: &'p str, need: Reach) -> Result<(Mount, Vec<&'p str>), FsError> {
        let reach = self.reach(&tree::components(path)?);
        if reach < need {
            // What is not there at all is no refusal worth a line of the
            // log (a program looks for files that may be there).
            let there = self.fs.exists(path);
            if reach == Reach::None {
                return Err(self.refuse(path, "not there for it", FsError::NotFound, there));
            }
            return Err(self.refuse(path, "it may only read it", FsError::Denied, there));
        }
        let (m, comps) = self.fs.route(path)?;
        if m == Mount::Ram && !self.access().may_reach(&comps) {
            return Err(FsError::Denied);
        }
        // The installed packages are the package manager's to change.
        if m == Mount::Ram && need == Reach::Write && !self.access().may_change(&comps) {
            return Err(FsError::Denied);
        }
        Ok((m, comps))
    }

    /// `e`, for what the connection's view refused it at `what` (`why`):
    /// logged if `log`, the first few times.
    fn refuse(&mut self, what: &str, why: &str, e: FsError, log: bool) -> FsError {
        if log {
            *self.refusals += 1;
            if *self.refusals <= LOGGED_REFUSALS {
                println!("{} (process {}), confined, was refused {}: {}", self.who.name, self.who.koid, what, why);
            }
        }
        e
    }

    /// Refuses what a confined connection has none of (abstract socket
    /// names): `Denied`.
    fn unconfined(&mut self, what: &str) -> Result<(), FsError> {
        if self.view.is_some() { Err(self.refuse(what, "it is confined", FsError::Denied, true)) } else { Ok(()) }
    }

    /// The request's need of a file it opens with `flags`.
    fn need_to_open(flags: u32) -> Reach {
        if flags & (open_flags::WRITE | open_flags::CREATE | open_flags::TRUNCATE) != 0 {
            Reach::Write
        } else {
            Reach::Read
        }
    }

    /// The entries of directory `comps` of mount `m` that a listing of it
    /// shows the client, from the first, without the view's say (private
    /// directories hidden, the other file systems in the root).
    fn entries(&self, m: Mount, comps: &[&str]) -> Result<Vec<DirEntry>, FsError> {
        if m == Mount::Dev {
            return match Dev::find(comps)? {
                None => Ok(self.fs.dev_entries()),
                Some(_) => Err(FsError::NotDir),
            };
        }
        if m == Mount::Shm {
            return match self.fs.shm.find(comps)? {
                None => Ok(self.fs.shm.entries_after("").collect()),
                Some(_) => Err(FsError::NotDir),
            };
        }
        let t = self.fs.tree(m)?;
        let entries = t.entries_after(t.lookup(comps)?, "")?;
        if m != Mount::Ram {
            return Ok(entries.collect());
        }
        let mounts: &[&str] = if comps.is_empty() { &MOUNTS } else { &[] };
        let access = self.access();
        let mounts = mounts.iter().map(|name| self.mount_entry(name));
        Ok(tree::merge(entries, mounts).filter(|e| access.sees(comps, &e.name)).collect())
    }

    /// How many entries a listing of directory `full` (`comps` of mount
    /// `m`) shows a connection whose view reaches it only on the way to its
    /// places: those that lead on to one.
    fn entries_on_the_way(&self, view: &View, m: Mount, comps: &[&str], full: &[&str]) -> Result<u64, FsError> {
        Ok(self.entries(m, comps)?.iter().filter(|e| view.shows(full, &e.name)).count() as u64)
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
            DEV => Dev::stat(None).size,
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
        let (m1, c1) = self.resolve(from, Reach::Write)?;
        let (m2, c2) = self.resolve(to, Reach::Write)?;
        if m1 != m2 {
            return Err(FsError::Invalid);
        }
        if m1 == Mount::Shm {
            return self.fs.shm.rename(&c1, &c2, replace);
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
        let (m, comps) = self.resolve(&path, Self::need_to_open(flags))?;
        // Devices and shared memory objects open as files of their own
        // (`open_file`).
        if m == Mount::Dev || m == Mount::Shm {
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
        let (m, comps) = self.resolve(&path, Reach::Through)?;
        let mut st = if m == Mount::Dev {
            Dev::stat(Dev::find(&comps)?)
        } else if m == Mount::Shm {
            self.fs.shm.stat(self.fs.shm.find(&comps)?)?
        } else {
            let t = self.fs.tree(m)?;
            let mut st = t.stat(t.lookup(&comps)?)?;
            if m == Mount::Ram && st.is_dir {
                st.size = self.entries_seen(&comps, st.size);
            }
            // What only the package manager changes is read-only to the
            // rest.
            st.read_only |= m == Mount::Ram && !self.access().may_change(&comps);
            st
        };
        // A confined connection is told what it may do: change only what
        // its view lets it, and see in a directory on the way to its places
        // only what leads on.
        if let Some(view) = self.view.as_ref() {
            let full = tree::components(&path)?;
            let reach = view.reach(&full);
            st.read_only |= reach < Reach::Write;
            if reach == Reach::Through && st.is_dir {
                st.size = self.entries_on_the_way(view, m, &comps, &full)?;
            }
        }
        Ok(st)
    }

    fn read_dir_page(&mut self, path: String, after: String, max: u32) -> Result<DirPage, FsError> {
        let (m, comps) = self.resolve(&path, Reach::Through)?;
        let full = tree::components(&path)?;
        // A confined connection's directory on the way to its places shows
        // what leads on to them alone.
        let on_the_way = self.view.as_ref().filter(|v| v.reach(&full) == Reach::Through);
        let shown = |e: &DirEntry| on_the_way.is_none_or(|v| v.shows(&full, &e.name));
        let mut page = if m == Mount::Dev {
            match Dev::find(&comps)? {
                None => tree::page(self.fs.dev_entries().into_iter().filter(|e| *e.name > *after && shown(e)), max),
                Some(_) => return Err(FsError::NotDir),
            }
        } else if m == Mount::Shm {
            match self.fs.shm.find(&comps)? {
                None => tree::page(self.fs.shm.entries_after(&after).filter(shown), max),
                Some(_) => return Err(FsError::NotDir),
            }
        } else {
            let t = self.fs.tree(m)?;
            let entries = t.entries_after(t.lookup(&comps)?, &after)?;
            if m != Mount::Ram {
                tree::page(entries.filter(shown), max)
            } else {
                let mounts: &[&str] = if comps.is_empty() { &MOUNTS } else { &[] };
                let mounts = mounts.iter().filter(|name| **name > after.as_str()).map(|name| self.mount_entry(name));
                let access = self.access();
                let entries = tree::merge(entries, mounts).filter(|e| access.sees(&comps, &e.name) && shown(e));
                let mut page = tree::page(entries, max);
                // A directory's size is what a listing of it shows (which
                // differs only for `/home` and `/home/.private`).
                if comps.len() < 2 {
                    for e in page.entries.iter_mut().filter(|e| e.is_dir) {
                        let child = [comps.as_slice(), &[e.name.as_str()]].concat();
                        e.size = self.entries_seen(&child, e.size);
                    }
                }
                page
            }
        };
        // And so does a directory of it on the way to them.
        if let Some(view) = self.view.as_ref() {
            for e in page.entries.iter_mut().filter(|e| e.is_dir) {
                let child = [full.as_slice(), &[e.name.as_str()]].concat();
                if view.reach(&child) == Reach::Through {
                    let (cm, cc) = Fs::route_comps(&child);
                    e.size = self.entries_on_the_way(view, cm, &cc, &child)?;
                }
            }
        }
        Ok(page)
    }

    fn mkdir(&mut self, path: String) -> Result<(), FsError> {
        let (m, comps) = self.resolve(&path, Reach::Write)?;
        if m == Mount::Dev {
            // Its devices and itself exist; nothing can be added.
            return Err(if Dev::find(&comps).is_ok() { FsError::Exists } else { FsError::ReadOnly });
        }
        if m == Mount::Shm {
            // It holds shared memory objects alone.
            return Err(if self.fs.shm.find(&comps).is_ok() { FsError::Exists } else { FsError::ReadOnly });
        }
        self.fs.create_node(m, &comps, Kind::dir())?;
        Ok(())
    }

    fn remove(&mut self, path: String) -> Result<(), FsError> {
        let (m, comps) = self.resolve(&path, Reach::Write)?;
        if m == Mount::Ram && access::fixed(&comps) {
            return Err(FsError::Denied);
        }
        if m == Mount::Shm {
            return self.fs.shm.unlink(&comps);
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
        let (m, comps) = self.resolve(&path, Reach::Read)?;
        if m == Mount::Shm {
            // A copy, as of the other files of the file system's own.
            let id = self.fs.shm.find(&comps)?.ok_or(FsError::IsDir)?;
            let size = self.fs.shm.size(id)?;
            return Ok((self.fs.copy_of_memory(id, size)?, size));
        }
        let t = self.fs.tree(m).map_err(|_| FsError::Invalid)?;
        let d = match t.node(t.lookup(&comps)?).map(|n| &n.kind) {
            Some(Kind::File(d)) => d,
            Some(Kind::Dir(_)) => return Err(FsError::IsDir),
            Some(Kind::Socket(_)) => return Err(FsError::IsSocket),
            None => return Err(FsError::NotFound),
        };
        // The system image's bytes go as they are, its pages; others in a
        // copy of their own.
        if let Data::Static(bytes) = d
            && let Some(vmo) = self.fs.image.pages_of(bytes)
        {
            return Ok((vmo, bytes.len() as u64));
        }
        let bytes = d.bytes();
        let vmo = Vmo::create(bytes.len().max(1)).map_err(|_| FsError::NoSpace)?;
        vmo.write(0, bytes).map_err(|_| FsError::Io)?;
        Ok((vmo, bytes.len() as u64))
    }

    fn write_file(&mut self, path: String, data: Vmo, len: u64) -> Result<(), FsError> {
        let (m, comps) = self.resolve(&path, Reach::Write)?;
        if len > 1 << 30 {
            return Err(FsError::NoSpace);
        }
        if m == Mount::Shm {
            self.fs.memory_for(len as usize)?;
            let flags = open_flags::WRITE | open_flags::CREATE | open_flags::TRUNCATE;
            let id = self.fs.shm.open(&comps, flags, shm::Memory::new)?;
            let mut chunk = alloc::vec![0; (len as usize).min(MAX_IO as usize)];
            let mut at = 0;
            while at < len {
                let n = (len - at).min(chunk.len() as u64) as usize;
                data.read(at as usize, &mut chunk[..n]).map_err(|_| FsError::Invalid)?;
                self.fs.shm.write(id, at, false, &chunk[..n])?;
                at += n as u64;
            }
            return Ok(());
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

    /// A confined connection's returns at once: what it changes is saved
    /// as anything is, once changes stop, and it cannot have the home
    /// directory saved when it likes. The databases it commits with `fsync`
    /// (the browser's network process's) go on, as a snapshot holds every
    /// write up to a moment, in order: a crash leaves them as one at that
    /// moment would.
    fn sync(&mut self) -> Result<(), FsError> {
        if self.view.is_some() {
            return Ok(());
        }
        self.fs.save()
    }

    fn space(&mut self, path: String) -> Result<Space, FsError> {
        let (m, comps) = self.resolve(&path, Reach::Through)?;
        if m == Mount::Dev {
            return Ok(Space { total: 0, used: 0, persistent: false });
        }
        if m == Mount::Shm {
            // Memory, as much as the free memory allows, but for what is
            // left to the rest of the system.
            self.fs.shm.find(&comps)?;
            let bytes = self.fs.shm.bytes();
            let free = rt::object::system_info().map(|i| i.free_memory).unwrap_or(0);
            return Ok(Space { total: bytes + free.saturating_sub(MEMORY_RESERVE), used: bytes, persistent: false });
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
                let free = rt::object::system_info().map(|i| i.free_memory).unwrap_or(0);
                let room = free.saturating_sub(MEMORY_RESERVE);
                Space { total: bytes + room, used: bytes, persistent: false }
            }
            _ => Space { total: self.fs.system_bytes, used: self.fs.system_bytes, persistent: true },
        })
    }

    fn open_file(&mut self, path: String, flags: u32) -> Result<(Channel, Stat), FsError> {
        let (m, comps) = self.resolve(&path, Self::need_to_open(flags))?;
        let may_touch = self.reach(&tree::components(&path)?) == Reach::Write;
        let owner = self.who.koid;
        self.fs.connections.admit(owner)?;
        let opened = (|| {
            let (target, stat) = match m {
                Mount::Dev => match Dev::find(&comps)? {
                    Some(d) => (Target::Dev(d), Dev::stat(Some(d))),
                    None => return Err(FsError::IsDir),
                },
                Mount::Shm => {
                    let id = self.fs.shm.open(&comps, flags, shm::Memory::new)?;
                    (Target::Memory(id), self.fs.shm.stat(Some(id))?)
                }
                _ => {
                    let node = self.fs.open_node(m, &comps, flags)?;
                    (Target::Node(m, node), self.fs.tree(m)?.stat(node)?)
                }
            };
            self.fs.next_open += 1;
            let id = self.fs.next_open;
            let desc = Rc::new(RefCell::new(Description { id, target, flags, offset: 0, owner, may_touch }));
            let channel = self.connect_file(desc)?;
            match target {
                Target::Node(m, node) => self.fs.hold(m, node),
                Target::Memory(id) => self.fs.shm.hold(id),
                Target::Dev(_) => {}
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
        let (m, comps) = self.resolve(&path, Reach::Write)?;
        if m == Mount::Shm {
            let id = self.fs.shm.find(&comps)?.ok_or(FsError::ReadOnly)?;
            return self.fs.shm.set_modified(id, modified);
        }
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
        let (m1, c1) = self.resolve(&from, Reach::Read)?;
        let (m2, c2) = self.resolve(&to, Reach::Write)?;
        // A shared memory object's bytes are no file's to share.
        if m1 == Mount::Dev || m1 == Mount::Shm || m2 == Mount::Shm {
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

    fn confine(&mut self, view: Vec<Place>) -> Result<(), FsError> {
        // The places it borrowed stay, and count.
        if view.len() + self.view.as_ref().map_or(0, View::borrowed) > MAX_PLACES {
            return Err(FsError::Invalid);
        }
        let mut places = Vec::new();
        for place in &view {
            if !place.path.starts_with('/') {
                return Err(FsError::Invalid);
            }
            places.push((tree::components(&place.path)?, place.write));
        }
        let view = View::new(places);
        match self.view.as_mut() {
            // Again, only narrower than its own places: what it borrowed
            // stays its lender's to take back.
            Some(old) => {
                if !old.confine(view) {
                    return Err(FsError::Denied);
                }
            }
            None => *self.view = Some(view),
        }
        Ok(())
    }

    fn lend(&mut self, path: String) -> Result<Bytes, FsError> {
        if !path.starts_with('/') {
            return Err(FsError::Invalid);
        }
        // A confined connection lends nothing: a sandboxed program hands on
        // no part of its sandbox.
        if self.view.is_some() {
            return Err(self.refuse(&path, "it may not lend", FsError::Denied, true));
        }
        let comps = tree::components(&path)?;
        // Nor is the user's home folder lent whole, or what holds it.
        if [HOME, USER].starts_with(&comps) {
            return Err(FsError::Denied);
        }
        let (m, inside) = self.resolve(&path, Reach::Read)?;
        match m {
            Mount::Dev => drop(Dev::find(&inside)?),
            Mount::Shm => drop(self.fs.shm.find(&inside)?),
            _ => drop(self.fs.tree(m)?.lookup(&inside)?),
        }
        let mut secret = [0u8; LOAN_SIZE];
        rt::object::try_random_bytes(&mut secret).map_err(|_| FsError::Io)?;
        self.fs.loans.lend(self.id, &comps, secret, rt::time::now_ns())?;
        Ok(Bytes(secret.to_vec()))
    }

    fn borrow(&mut self, path: String, loan: Bytes) -> Result<(), FsError> {
        if !path.starts_with('/') || loan.0.len() != LOAN_SIZE {
            return Err(FsError::Invalid);
        }
        let comps = tree::components(&path)?;
        // A full view takes no more places, and leaves the loan untaken.
        if let Some(view) = self.view.as_ref()
            && view.len() >= MAX_PLACES
            && view.would_borrow(&comps)
        {
            return Err(FsError::TooMany);
        }
        let Some(taken) = self.fs.loans.take(&loan.0, &comps, rt::time::now_ns()) else {
            if self.view.is_none() {
                return Err(FsError::Denied);
            }
            let there = self.fs.exists(&path);
            return Err(self.refuse(&path, "no such loan", FsError::Denied, there));
        };
        // Its lender may take back the place it adds; one the connection
        // reaches anyway is only taken.
        if let Some(view) = self.view.as_mut()
            && view.borrow(&comps, taken.id())
        {
            self.fs.loans.hold(taken, self.id);
            println!("{} (process {}), confined, was lent {} to read", self.who.name, self.who.koid, path);
        }
        Ok(())
    }

    fn take_back(&mut self, loan: Bytes) -> Result<(), FsError> {
        if loan.0.len() != LOAN_SIZE {
            return Err(FsError::Invalid);
        }
        // The service loop takes the place out of its borrower's view
        // before it reads another request.
        if let Some(back) = self.fs.loans.take_back(self.id, &loan.0) {
            self.taken_back.push(back);
        }
        Ok(())
    }

    fn link(&mut self, from: String, to: String) -> Result<(), FsError> {
        let (m1, c1) = self.resolve(&from, Reach::Write)?;
        let (m2, c2) = self.resolve(&to, Reach::Write)?;
        // Shared memory objects alone have links.
        if m1 != Mount::Shm || m2 != Mount::Shm {
            return Err(FsError::Invalid);
        }
        self.fs.shm.link(&c1, &c2)
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
            self.unconfined("an abstract socket name")?;
            return self.fs.sockets.bind_abstract(owner, koid, kind, at.abstract_name.0, name.0, inbox);
        }
        let (m, comps) = self.resolve(&at.path, Reach::Write)?;
        if m != Mount::Ram {
            // `/system` and `/dev` take nothing new.
            let exists = match m {
                Mount::Dev => Dev::find(&comps).is_ok(),
                Mount::Shm => self.fs.shm.find(&comps).is_ok(),
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

    fn lock_owner(&mut self, koid: u64) -> Result<Bytes, FsError> {
        if let Some(secret) = self.fs.owners.secret(self.id) {
            return Ok(Bytes(secret.to_vec()));
        }
        let mut secret = [0u8; LOCK_OWNER_SIZE];
        rt::object::try_random_bytes(&mut secret).map_err(|_| FsError::Io)?;
        self.fs.owners.add(self.id, koid, secret);
        Ok(Bytes(secret.to_vec()))
    }
}

impl Session<'_> {
    /// The socket bound at `to`: at its path (`Refused` for something else
    /// there, as Linux has it), or to its abstract name, of `kind`'s
    /// (`Refused` for none).
    fn socket_at(&mut self, to: &SocketAddress, kind: u32) -> Result<u64, FsError> {
        if to.is_abstract() {
            self.unconfined("an abstract socket name")?;
            return self.fs.sockets.find_abstract(kind, &to.abstract_name.0);
        }
        // Reaching a socket to connect or send is changing it, as for
        // OpenBSD's `unveil`.
        let (m, comps) = self.resolve(&to.path, Reach::Write)?;
        if m == Mount::Dev {
            return Dev::find(&comps).and(Err(FsError::Refused));
        }
        if m == Mount::Shm {
            return self.fs.shm.find(&comps).and(Err(FsError::Refused));
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
            Target::Memory(id) => self.fs.shm.read(id, offset, len.min(MAX_IO) as usize),
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
            Target::Memory(id) => {
                let size = self.fs.shm.size(id)?;
                let end = if append { size } else { offset }.saturating_add(data.len() as u64);
                self.fs.memory_for(end.saturating_sub(size) as usize)?;
                self.fs.shm.write(id, offset, append, data)
            }
        }
    }

    /// Whether the offset moves as the file is read and written: not for a
    /// device.
    fn has_offset(&self) -> bool {
        !matches!(self.desc.borrow().target, Target::Dev(_))
    }

    /// The owner a lock request names: this open file (no secret), or the
    /// process whose secret `secret` is (`Denied` for no one's).
    fn lock_owner(&self, secret: &[u8]) -> Result<Owner, FsError> {
        if secret.is_empty() {
            return Ok(Owner::Open(self.desc.borrow().id));
        }
        self.fs.owners.find(secret).map(Owner::Process).ok_or(FsError::Denied)
    }
}

/// The kind of lock `kind` (`lock_kind`) is: none for `NONE`.
fn lock_kind(kind: u32) -> Result<Option<locks::Kind>, FsError> {
    match kind {
        lock_kind::NONE => Ok(None),
        lock_kind::SHARED => Ok(Some(locks::Kind::Shared)),
        lock_kind::EXCLUSIVE => Ok(Some(locks::Kind::Exclusive)),
        _ => Err(FsError::Invalid),
    }
}

/// `lock_kind`'s number for `kind`.
fn lock_kind_number(kind: locks::Kind) -> u32 {
    match kind {
        locks::Kind::Shared => lock_kind::SHARED,
        locks::Kind::Exclusive => lock_kind::EXCLUSIVE,
    }
}

impl file::Server for FileSession<'_> {
    fn pages(&mut self, offset: u64, len: u64) -> Result<Vmo, FsError> {
        self.readable()?;
        let Target::Node(m, node) = self.desc.borrow().target else { return Err(FsError::Invalid) };
        let tree = self.fs.tree(m)?;
        let Some(Kind::File(Data::Static(bytes))) = tree.node(node).map(|n| &n.kind) else {
            return Err(FsError::Invalid);
        };
        let page = abi::PAGE_SIZE as u64;
        let file_pages = (bytes.len() as u64).next_multiple_of(page);
        let within = offset.checked_add(len).is_some_and(|end| end <= file_pages);
        if len == 0 || !offset.is_multiple_of(page) || !within {
            return Err(FsError::Invalid);
        }
        let pages = self.fs.image.pages_of(bytes).ok_or(FsError::Invalid)?;
        pages.slice(offset as usize, len as usize).map_err(|_| FsError::Invalid)
    }

    fn read(&mut self, len: u32) -> Result<Bytes, FsError> {
        let offset = self.desc.borrow().offset;
        let data = self.read_at_offset(offset, len)?;
        if self.has_offset() {
            self.desc.borrow_mut().offset = offset + data.len() as u64;
        }
        Ok(Bytes(data))
    }

    fn write(&mut self, data: Bytes) -> Result<u32, FsError> {
        let offset = self.desc.borrow().offset;
        let at = self.write_at_offset(offset, &data.0)?;
        if self.has_offset() {
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
        let size = match target {
            Target::Node(m, node) => self.fs.tree(m)?.stat(node)?.size,
            Target::Memory(id) => self.fs.shm.size(id)?,
            Target::Dev(_) => return Ok(0),
        };
        let base = match whence {
            seek::SET => 0,
            seek::CURRENT => self.desc.borrow().offset,
            seek::END => size,
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
            Target::Memory(id) => self.fs.shm.stat(Some(id)),
        }
    }

    fn truncate(&mut self, len: u64) -> Result<(), FsError> {
        self.writable()?;
        let target = self.desc.borrow().target;
        match target {
            Target::Node(m, node) => self.fs.truncate_node(m, node, len),
            Target::Dev(_) => Ok(()),
            Target::Memory(id) => {
                let size = self.fs.shm.size(id)?;
                self.fs.memory_for(len.saturating_sub(size) as usize)?;
                self.fs.shm.truncate(id, len)
            }
        }
    }

    fn memory(&mut self, len: u64) -> Result<Vmo, FsError> {
        self.readable()?;
        let Target::Memory(id) = self.desc.borrow().target else { return Err(FsError::Invalid) };
        let writable = self.desc.borrow().flags & open_flags::WRITE != 0;
        self.fs.shm.memory(id, len)?.handle(writable)
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
        let target = self.desc.borrow().target;
        if let Target::Memory(id) = target {
            return self.fs.shm.set_modified(id, modified);
        }
        let Target::Node(m, node) = target else { return Ok(()) };
        if !self.desc.borrow().may_touch {
            return Err(FsError::Denied);
        }
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

    fn lock(&mut self, owner: Bytes, whole: bool, lock: LockRange, wait: bool) -> Result<LockOutcome, FsError> {
        let owner = self.lock_owner(&owner.0)?;
        let kind = lock_kind(lock.kind)?;
        let d = self.desc.borrow();
        let (file, flags, program) = (d.target.file(), d.flags, d.owner);
        drop(d);
        // A lock on bytes needs the access its kind does, as POSIX has it:
        // a shared one, to read; an exclusive one, to write.
        let needs = match kind {
            Some(locks::Kind::Shared) => open_flags::READ,
            Some(locks::Kind::Exclusive) => open_flags::WRITE,
            None => 0,
        };
        if !whole && flags & needs != needs {
            return Err(FsError::BadFd);
        }
        let request = locks::Request { whole, kind, start: lock.start, end: lock.end };
        let waiting = match self.fs.locks.lock(file, owner, request, wait)? {
            Outcome::Done => None,
            Outcome::Waiting(waiter) => Some(self.fs.lock_wait(waiter, program)?),
        };
        Ok(LockOutcome { waiting, held: self.fs.locks.holds(file, owner) })
    }

    fn test_lock(&mut self, owner: Bytes, lock: LockRange) -> Result<Option<LockHolder>, FsError> {
        let owner = self.lock_owner(&owner.0)?;
        let kind = lock_kind(lock.kind)?;
        if lock.start > lock.end || lock.end > LOCK_END {
            return Err(FsError::Invalid);
        }
        let file = self.desc.borrow().target.file();
        let found = self.fs.locks.test(file, owner, kind, lock.start, lock.end);
        Ok(found.map(|l| LockHolder {
            lock: LockRange { kind: lock_kind_number(l.kind), start: l.start, end: l.end },
            process: match l.owner {
                Owner::Process(conn) => Some(self.fs.owners.koid(conn).unwrap_or(0)),
                Owner::Open(_) => None,
            },
        }))
    }
}

/// A writable tree with `/tmp` and an empty `/home`.
fn fresh_ram() -> Tree {
    let mut ram = Tree::new(false, device::RAM, rt::time::unix_time_ns);
    let _ = ram.mkdir_all(&[filesystem_core::TMP]);
    let _ = ram.mkdir_all(&[HOME, USER]);
    ram
}

/// Builds the file systems: the system image from the initrd, and the home
/// directory from the home disk or, failing that, the samples. A `live`
/// system keeps the home directory in memory without looking for a disk.
fn build(archive: &initrd::Archive<'static>, image: SystemImage, live: bool) -> Fs {
    let mut system = Tree::new(true, device::SYSTEM, rt::time::unix_time_ns);
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
    // Where the installed packages are, which every home has, made before
    // anything else could make something there.
    let _ = ram.mkdir_all(&[HOME, PACKAGES]);
    // And where the services' private directories are, which they would
    // otherwise all make at once at boot on a new home.
    let _ = ram.mkdir_all(&[HOME, PRIVATE]);
    let home = match (&store, found) {
        (None, _) if live => "in memory (live system)",
        (None, _) => "in memory (no home disk)",
        (Some(_), true) => "restored from the home disk",
        (Some(_), false) => "new, on the home disk",
    };
    println!("home directory {}", home);
    // A service renamed since the home was made finds its private directory
    // under its new name, before any service is there to look.
    let moved = renamed::move_private(&mut ram);
    for m in &moved {
        match m.result {
            Ok(()) => println!("moved /home/.private/{} to /home/.private/{}, its service's new name", m.from, m.to),
            Err(e) => println!("cannot move /home/.private/{} to /home/.private/{}: {}", m.from, m.to, e),
        }
    }
    let mut fs = Fs {
        system,
        system_bytes,
        image,
        ram,
        samples,
        store,
        dirty: None,
        retry_at: 0,
        open: BTreeMap::new(),
        orphans: BTreeSet::new(),
        connections: Connections::new(),
        sockets: Sockets::new(),
        shm: Shm::new(rt::time::unix_time_ns),
        loans: Loans::new(),
        home_len: Cell::new(None),
        locks: Locks::new(),
        owners: Owners::new(),
        lock_waits: BTreeMap::new(),
        next_open: 0,
    };
    // The home disk keeps the new names from its next save on.
    if moved.iter().any(|m| m.result.is_ok()) {
        fs.changed();
    }
    fs
}

fn main() -> i32 {
    let Some(initrd_vmo) = rt::env::take_handle(abi::startup::role::INITRD).map(Vmo::from_handle) else {
        println!("no initrd handle");
        return 1;
    };
    let size = initrd_vmo.size().unwrap_or(0);
    let Ok(addr) = initrd_vmo.map(0, size, abi::map_flags::READ) else {
        println!("cannot map the initrd");
        return 1;
    };
    // SAFETY: the mapping is read-only and lives as long as the process.
    let bytes: &'static [u8] = unsafe { core::slice::from_raw_parts(addr as *const u8, size) };
    let Ok(archive) = initrd::Archive::open(bytes) else {
        println!("the initrd is corrupt");
        return 1;
    };
    let image = SystemImage { vmo: initrd_vmo, bytes };
    let mut fs = build(&archive, image, rt::env::args().iter().any(|a| a == "live"));

    let Ok(listener) = proto::register(vfs::NAME) else {
        println!("cannot register the vfs service");
        return 1;
    };
    println!("serving /system ({} files), /dev, /dev/shm and /home", archive.len());

    struct Client {
        channel: Channel,
        fds: BTreeMap<u32, OpenFile>,
        who: ClientIdentity,
        /// It gave up access to private directories.
        renounced: bool,
        /// What it reaches, if it is confined.
        view: Option<View>,
        refusals: u32,
    }
    struct FileConn {
        channel: Channel,
        desc: Shared,
    }
    const FILE_KEY: u64 = 1 << 62;
    const LOCK_WAIT_KEY: u64 = 1 << 61;
    let mut clients: BTreeMap<u64, Client> = BTreeMap::new();
    let mut files: BTreeMap<u64, FileConn> = BTreeMap::new();
    let mut next_id = 1u64;
    let mut new_files = NewFiles::new();
    let mut taken_back = Vec::new();
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
        for (&waiter, wait) in &fs.lock_waits {
            ws.add(wait.channel.raw(), signals::PEER_CLOSED, LOCK_WAIT_KEY | waiter);
        }
        let Ok(ready) = ws.wait(fs.save_deadline()) else { continue };
        for (key, observed) in ready {
            if key == 0 {
                while let Some((ch, who)) = proto::accept_with_identity(&listener) {
                    // Refused: the channel closes, and the client's calls fail.
                    if let Err(e) = fs.connections.admit(who.koid) {
                        println!("refusing a connection of {} ({}): {}", who.name, who.koid, e);
                        continue;
                    }
                    let client =
                        Client { channel: ch, fds: BTreeMap::new(), who, renounced: false, view: None, refusals: 0 };
                    clients.insert(next_id, client);
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
                    // The last connection closes the open file, and its
                    // locks go.
                    if Rc::strong_count(&conn.desc) == 1 {
                        fs.locks.release(Owner::Open(desc.id));
                        match desc.target {
                            Target::Node(m, node) => fs.release(m, node),
                            Target::Memory(id) => fs.shm.release(id),
                            Target::Dev(_) => {}
                        }
                    }
                }
            } else if key & LOCK_WAIT_KEY != 0 {
                // The client gave up a request that waits for a lock, or
                // ended.
                let waiter = key & !LOCK_WAIT_KEY;
                if observed & signals::PEER_CLOSED != 0 {
                    fs.locks.cancel(waiter);
                    fs.end_lock_wait(waiter);
                }
            } else {
                let Some(client) = clients.get_mut(&key) else { continue };
                if observed & signals::READABLE != 0 {
                    while let Ok(msg) = client.channel.read() {
                        let mut session = Session {
                            fs: &mut fs,
                            id: key,
                            fds: &mut client.fds,
                            who: &client.who,
                            renounced: &mut client.renounced,
                            view: &mut client.view,
                            refusals: &mut client.refusals,
                            new_files: &mut new_files,
                            taken_back: &mut taken_back,
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
                    taken_back.extend(fs.loans.forget(key));
                    // The locks of the process it spoke for go with it, as
                    // they go as a process ends.
                    fs.locks.release(Owner::Process(key));
                    fs.owners.remove(key);
                    for f in client.fds.values() {
                        fs.release(f.mount, f.node);
                    }
                }
            }
            // Requests waiting for locks hear what became of them.
            fs.tell_lock_waits();
            // The places taken back leave their borrowers' views before
            // anyone's next request.
            for back in taken_back.drain(..) {
                if let Some(c) = clients.get_mut(&back.borrower)
                    && c.view.as_mut().is_some_and(|v| v.take_back(back.id))
                {
                    println!("{} (process {}), confined, had {} taken back", c.who.name, c.who.koid, back.path);
                }
            }
            for (channel, desc) in new_files.drain(..) {
                files.insert(next_id, FileConn { channel, desc });
                next_id += 1;
            }
        }
        if rt::time::now_ns() >= fs.save_deadline() {
            let _ = fs.save();
        }
    }
}
