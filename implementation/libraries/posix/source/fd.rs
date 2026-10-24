// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! File descriptors.
//!
//! A descriptor refers to an open file description ([`Description`]),
//! shared by the descriptors `dup` makes and, through the handles behind
//! it, by other processes:
//!
//! | Object | Behind it | Passed to children as |
//! |--------|-----------|-----------------------|
//! | [`Object::File`] | a `file` connection to the VFS (which keeps the offset) | a duplicated connection |
//! | [`Object::Dir`] | a directory's path | its path, in the startup message |
//! | [`Object::Stream`] | a socket endpoint: a pipe or the terminal | a duplicated handle |
//! | [`Object::Inet`] | a socket of the network service (TCP, UDP): a channel to it, and a connection's socket endpoint for its bytes | the channel, shared, a connection's endpoint too, and what makes it a socket ([`crate::carry`]); one still connecting not passed |
//! | [`Object::Unix`] | a Unix domain socket: a connection's socket endpoint or channel, an inbox the VFS writes on | the one that carries it, shared, and what makes it a socket ([`crate::carry`]) |
//! | [`Object::Memfd`] | a memory file (`memfd_create`): a VMO | a handle to it, with the access the descriptor has, and its size and seals ([`crate::carry`]) |
//! | [`Object::EventFd`] | a counter of the layer's (`eventfd`), and an event object | (not passed) |
//! | [`Object::TimerFd`] | a timer of the layer's (`timerfd`), and an event object | (not passed) |
//! | [`Object::Epoll`] | an interest list of the layer's (`epoll`), and a port | (not passed) |
//! | [`Object::Pidfd`] | a child process (`pidfd_open`): readable once it has ended | (not passed) |
//! | [`Object::Null`] | nothing: reads end at once, writes vanish | itself, in the startup message |
//! | [`Object::Log`] | the kernel log (output of programs without a terminal) | itself, in the startup message |
//!
//! The table itself is per process; descriptors carry only the close-on-exec
//! flag.
//!
//! A description tells the `epoll` instances that watch it when it changes
//! in ways the kernel's signals do not say ([`Description::changed`]): a
//! program met it not ready (`EAGAIN`) or emptied or filled it, an eventfd
//! was written to.

use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use rt::object::Process;
use rt::sync::Mutex;

use crate::epoll::{Epoll, Watcher};
use crate::eventfd::EventFd;
use crate::file::{Directory, VfsFile};
use crate::inet::Inet;
use crate::linux::errno::{EBADF, EMFILE};
use crate::linux::o;
use crate::log::Log;
use crate::memfd::Memfd;
use crate::stream::Stream;
use crate::timerfd::TimerFd;
use crate::unix::Unix;

/// Descriptors per process (`RLIMIT_NOFILE`).
pub const MAX_FDS: usize = 1024;

pub enum Object {
    File(VfsFile),
    Dir(Directory),
    Stream(Stream),
    Inet(Inet),
    Unix(Unix),
    Memfd(Memfd),
    EventFd(EventFd),
    TimerFd(TimerFd),
    Epoll(Epoll),
    Pidfd(Process),
    Null,
    Log(Log),
}

/// An open file description.
pub struct Description {
    pub object: Object,
    /// The access mode and status flags (`O_APPEND`, `O_NONBLOCK`, `O_SYNC`).
    flags: AtomicU32,
    /// Advanced as it changes in ways the kernel's signals do not say, which
    /// edge-triggered `epoll` compares.
    seq: AtomicU64,
    /// The `epoll` instances watching it, to be told of those changes.
    watchers: Mutex<Vec<(Weak<Watcher>, u64)>>,
    /// Who `SIGIO` and `SIGURG` would go to (`F_SETOWN`): the kind of owner
    /// (`F_OWNER_*`) above, its id below.
    owner: AtomicU64,
}

impl Description {
    pub fn new(object: Object, flags: u32) -> Arc<Description> {
        Arc::new(Description {
            object,
            flags: AtomicU32::new(flags & STATUS_FLAGS),
            seq: AtomicU64::new(0),
            watchers: Mutex::new(Vec::new()),
            owner: AtomicU64::new(Self::packed_owner(crate::linux::fcntl::F_OWNER_PID, 0)),
        })
    }

    fn packed_owner(kind: i32, id: i32) -> u64 {
        (kind as u32 as u64) << 32 | id as u32 as u64
    }

    /// Who `SIGIO` and `SIGURG` would go to: the kind of owner
    /// (`F_OWNER_*`) and its id (0: none).
    pub fn owner(&self) -> (i32, i32) {
        let packed = self.owner.load(Ordering::Acquire);
        ((packed >> 32) as i32, packed as u32 as i32)
    }

    /// Makes `id`, of kind `kind`, the owner.
    pub fn set_owner(&self, kind: i32, id: i32) {
        self.owner.store(Self::packed_owner(kind, id), Ordering::Release);
    }

    /// How often it changed in ways the kernel's signals do not say.
    pub fn seq(&self) -> u64 {
        self.seq.load(Ordering::Acquire)
    }

    /// It changed in a way the kernel's signals do not say: the `epoll`
    /// instances watching it look at it again.
    pub fn changed(&self) {
        self.seq.fetch_add(1, Ordering::AcqRel);
        let watchers: Vec<(Weak<Watcher>, u64)> = {
            let mut w = self.watchers.lock();
            if w.is_empty() {
                return;
            }
            w.retain(|(watcher, _)| watcher.strong_count() > 0);
            w.clone()
        };
        for (watcher, key) in watchers {
            if let Some(watcher) = watcher.upgrade() {
                watcher.kick(key);
            }
        }
    }

    /// An `epoll` instance watches it, as `key`.
    pub fn watch(&self, watcher: Weak<Watcher>, key: u64) {
        self.watchers.lock().push((watcher, key));
    }

    /// An `epoll` instance no longer watches it as `key`.
    pub fn unwatch(&self, watcher: &Arc<Watcher>, key: u64) {
        self.watchers.lock().retain(|(w, k)| !(*k == key && core::ptr::eq(w.as_ptr(), Arc::as_ptr(watcher))));
    }

    pub fn flags(&self) -> u32 {
        self.flags.load(Ordering::Relaxed)
    }

    /// Changes the flags `fcntl(F_SETFL)` may change.
    pub fn set_status(&self, flags: u32) {
        let keep = self.flags() & !SETTABLE;
        self.flags.store(keep | (flags & SETTABLE), Ordering::Relaxed);
    }

    pub fn readable(&self) -> bool {
        self.flags() & o::ACCMODE != o::WRONLY
    }

    pub fn writable(&self) -> bool {
        self.flags() & o::ACCMODE != o::RDONLY
    }

    pub fn nonblocking(&self) -> bool {
        self.flags() & o::NONBLOCK != 0
    }

    /// Whether writes reach persistent storage before they return
    /// (`O_SYNC`, `O_DSYNC`).
    pub fn synchronous(&self) -> bool {
        self.flags() & o::DSYNC != 0
    }
}

/// The flags a description keeps.
const STATUS_FLAGS: u32 = o::ACCMODE | o::APPEND | o::NONBLOCK | o::PATH | o::SYNC | o::DSYNC;
/// The flags `F_SETFL` changes.
const SETTABLE: u32 = o::APPEND | o::NONBLOCK;

#[derive(Clone)]
struct Slot {
    desc: Arc<Description>,
    cloexec: bool,
}

pub struct Table {
    slots: Vec<Option<Slot>>,
}

static TABLE: Mutex<Table> = Mutex::new(Table { slots: Vec::new() });

/// The description behind `fd`.
pub fn get(fd: i32) -> Result<Arc<Description>, isize> {
    let t = TABLE.lock();
    usize::try_from(fd).ok().and_then(|i| t.slots.get(i)).and_then(|s| s.as_ref()).map(|s| s.desc.clone()).ok_or(EBADF)
}

/// Installs `desc` at the lowest free descriptor at least `min`.
pub fn insert(desc: Arc<Description>, cloexec: bool, min: usize) -> Result<i32, isize> {
    let mut t = TABLE.lock();
    let free = (min..MAX_FDS).find(|&i| t.slots.get(i).is_none_or(|s| s.is_none())).ok_or(EMFILE)?;
    if t.slots.len() <= free {
        t.slots.resize(free + 1, None);
    }
    t.slots[free] = Some(Slot { desc, cloexec });
    Ok(free as i32)
}

/// Installs `desc` at `fd`, closing what was there.
pub fn replace(fd: i32, desc: Arc<Description>, cloexec: bool) -> Result<(), isize> {
    let i = usize::try_from(fd).ok().filter(|&i| i < MAX_FDS).ok_or(EBADF)?;
    let old = {
        let mut t = TABLE.lock();
        if t.slots.len() <= i {
            t.slots.resize(i + 1, None);
        }
        t.slots[i].replace(Slot { desc, cloexec })
    };
    if let Some(old) = old {
        close(old);
    }
    Ok(())
}

/// Closes `fd`.
pub fn remove(fd: i32) -> Result<(), isize> {
    let old = {
        let mut t = TABLE.lock();
        usize::try_from(fd).ok().and_then(|i| t.slots.get_mut(i)).and_then(|s| s.take()).ok_or(EBADF)?
    };
    close(old);
    Ok(())
}

/// Closes a descriptor taken out of the table: as POSIX has it, the
/// process's record locks on a file go as it closes any descriptor of the
/// file ([`crate::lock::closed`]); its description closes with its last
/// descriptor. Closing may talk to a service: never under the table's lock.
fn close(slot: Slot) {
    if let Object::File(f) = &slot.desc.object {
        crate::lock::closed(f);
    }
}

/// The descriptor `fd` was met not ready (`EAGAIN`), or emptied or filled
/// by a read or write: the `epoll` instances watching it look at it again.
pub fn not_ready(fd: i32) {
    if let Ok(desc) = get(fd) {
        desc.changed();
    }
}

/// Writes what is left of a line on every log descriptor (as the program
/// ends: see [`crate::log`]).
pub fn flush_logs() {
    // At the program's end, another thread may hold the lock for ever.
    let Some(t) = TABLE.try_lock() else { return };
    for slot in t.slots.iter().flatten() {
        if let Object::Log(log) = &slot.desc.object {
            log.flush();
        }
    }
}

/// Closes every descriptor in `first..=last` (`close_range`).
pub fn remove_range(first: usize, last: usize) {
    let old: Vec<Slot> = {
        let mut t = TABLE.lock();
        let end = t.slots.len().min(last.saturating_add(1));
        (first.min(end)..end).filter_map(|i| t.slots[i].take()).collect()
    };
    old.into_iter().for_each(close);
}

/// Sets close-on-exec on every descriptor in `first..=last`.
pub fn set_cloexec_range(first: usize, last: usize) {
    let mut t = TABLE.lock();
    let end = t.slots.len().min(last.saturating_add(1));
    for s in t.slots[first.min(end)..end].iter_mut().flatten() {
        s.cloexec = true;
    }
}

pub fn cloexec(fd: i32) -> Result<bool, isize> {
    let t = TABLE.lock();
    usize::try_from(fd).ok().and_then(|i| t.slots.get(i)).and_then(|s| s.as_ref()).map(|s| s.cloexec).ok_or(EBADF)
}

pub fn set_cloexec(fd: i32, on: bool) -> Result<(), isize> {
    let mut t = TABLE.lock();
    let s = usize::try_from(fd).ok().and_then(|i| t.slots.get_mut(i)).and_then(|s| s.as_mut()).ok_or(EBADF)?;
    s.cloexec = on;
    Ok(())
}

/// Every open descriptor: `(fd, description, close-on-exec)`.
pub fn snapshot() -> Vec<(i32, Arc<Description>, bool)> {
    let t = TABLE.lock();
    t.slots.iter().enumerate().filter_map(|(i, s)| s.as_ref().map(|s| (i as i32, s.desc.clone(), s.cloexec))).collect()
}
