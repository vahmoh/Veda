// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `fork`: a process that is a copy of this one.
//!
//! The kernel makes the copy (`process_fork`): its memory is this
//! process's, copied as either writes it, its one thread goes on from the
//! call as the calling thread does, and the handles named in the call move
//! to it under the same names. What the layer keeps in memory is then the
//! copy's too, and holds there for what is a process's own (its signal
//! dispositions, its working directory, its limits); what goes through a
//! handle needs a handle of the copy's own, which this process makes for it
//! before the call, and which the copy takes up after it ([`Gift`]), as
//! Linux's `fork` has it:
//!
//! | What | In the copy |
//! |------|-------------|
//! | descriptors | the same open file descriptions, as a program this one starts has them (`crate::carry`): a file's connection duplicated (one offset for both, and the `flock` locks of both), a pipe's or a socket's endpoint, a memory file's memory, a child's process; an eventfd, a timerfd or an epoll instance, which no handle carries, a copy of its own, with the same count, timer or interest list |
//! | the registry | a channel of its own, a clone of this process's |
//! | the VFS, the network service | connections of its own, made as it first needs them |
//! | the terminal | its state, shared |
//! | its parent | this process, which it may signal |
//! | threads | the calling one alone, whose id is the copy's process id |
//! | signals | the dispositions, and the calling thread's mask; none pending |
//! | children, timers, record locks | none, as on Linux |
//!
//! Other threads may be anywhere in the layer as the copy is made, holding
//! its locks: the copy has none of those threads, but its memory has the
//! locks as they held them, and what the locks guard as they left it. So
//! the layer holds its heap still across the call, which no thread is then
//! changing, and the copy makes every other part of its state anew from
//! what this process wrote down for it before the call (each part taken
//! whole, under its lock), forgetting what its memory says of that part
//! (`runtime::sync::Mutex::reset_after_fork`). Descriptions are made anew
//! too: a thread may have been inside one, as a read that waits is.
//!
//! The copy's descriptions are its own: their status flags (`O_NONBLOCK`,
//! `O_APPEND`) change on one side only, as for a program this one starts.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use abi::{Error, RawHandle, Rights};
use runtime::object::{Channel, Handle, Process, Vmo};

use crate::carry::{self, ToFork};
use crate::epoll::{self, Epoll};
use crate::error::{self, SysResult};
use crate::eventfd::EventFd;
use crate::fd::{self, Description, Object};
use crate::linux::Rlimit;
use crate::linux::errno::{EAGAIN, EIO, ENOMEM, ENOSYS};
use crate::timerfd::{self, TimerFd};

/// Where the calling thread is once [`fork`] has returned.
pub enum Forked {
    /// In the original: the copy's process id.
    Parent(i32),
    /// In the copy.
    Child,
}

/// What an open file description of this process is in the copy.
enum Copied {
    /// What carries it, as to a program this one starts.
    Carried(ToFork),
    /// An eventfd's count, and whether a read takes 1.
    EventFd((u64, bool)),
    TimerFd(timerfd::Copy),
    Epoll(epoll::Copy),
    /// Nothing: a socket still connecting, which cannot go.
    Absent,
}

/// An open file description on its way to the copy.
struct Going {
    /// Its address here, by which epoll instances know it.
    at: usize,
    /// Its access mode and status flags.
    flags: u32,
    copied: Copied,
}

/// What this process makes for the copy before the call, and what it
/// writes down of its state, which the copy takes up after it.
struct Gift {
    descriptions: Vec<Going>,
    /// Each descriptor: its number, its description's place in
    /// `descriptions`, and its close-on-exec flag.
    fds: Vec<(i32, usize, bool)>,
    /// The copy's registry channel, the original as its parent, and the
    /// terminal's state, as names of handles that move to it.
    registry: Option<RawHandle>,
    parent: Option<RawHandle>,
    tty: Option<RawHandle>,
    signals: crate::signal::Copy,
    cwd: String,
    nice: i32,
    limits: [Option<Rlimit>; 16],
    main_stack: (usize, usize),
    startup: runtime::env::StartupCopy,
}

/// Makes a process that is a copy of this one, as Linux's `fork` does (see
/// the module's documentation). `hold` is called just before the copy is
/// made, and what it returns dropped just after, in both processes: it
/// holds still what the caller keeps that the copy must find whole (the
/// heap, last: nothing may allocate meanwhile). `ENOSYS` in a child `vfork`
/// made, which shares its parent's memory (`crate::own`); `EAGAIN` or
/// `ENOMEM` when no process can be made.
pub fn fork<H>(hold: impl FnOnce() -> H) -> Result<Forked, isize> {
    if crate::own::current().is_some() {
        return Err(ENOSYS);
    }
    // What the clocks' first reading reads, before anything is held.
    let _ = crate::time::monotonic_ns();
    let gift = Gift::prepare()?;
    let names = gift.names();
    let made = {
        let _held = hold();
        runtime::process::fork(&names)
    };
    match made {
        Ok(Some(child)) => {
            // Its handles are the copy's now: nothing of them to close.
            drop(gift);
            crate::process::adopt_child(child).map(Forked::Parent)
        }
        Ok(None) => {
            // SAFETY: this is the copy, whose one thread this is.
            unsafe { gift.take_up() };
            Ok(Forked::Child)
        }
        Err(e) => {
            gift.close();
            Err(match e {
                Error::LimitReached | Error::BadState => EAGAIN,
                Error::NoMemory => ENOMEM,
                e => error::kernel(e),
            })
        }
    }
}

/// `fork` of a C program (Linux's `fork`, `vfork`, and `clone` with
/// `SIGCHLD` alone, which the C library makes): 0 in the copy, its process
/// id here. The C library holds its own state still meanwhile, the layer
/// its heap.
pub fn program() -> SysResult {
    #[cfg(feature = "layer")]
    let hold = || crate::HEAP.hold();
    #[cfg(not(feature = "layer"))]
    let hold = || ();
    match fork(hold)? {
        Forked::Parent(pid) => Ok(pid as usize),
        Forked::Child => Ok(0),
    }
}

/// `clone` of a C program: a copy of this process, which sends `SIGCHLD`
/// as it ends, with no other flag (`fork` as the C library makes it where
/// Linux has no `fork` call); threads are made otherwise (`__clone`), and
/// other processes by `posix_spawn` (`ENOSYS`).
pub fn clone(flags: usize) -> SysResult {
    if flags != crate::linux::sig::CHLD as usize {
        return Err(ENOSYS);
    }
    program()
}

impl Gift {
    /// Makes what the copy needs, and writes down what it keeps; whatever
    /// fails, what was made is closed.
    fn prepare() -> Result<Gift, isize> {
        let mut gift = Gift {
            descriptions: Vec::new(),
            fds: Vec::new(),
            registry: None,
            parent: None,
            tty: None,
            signals: crate::signal::fork_copy(),
            cwd: crate::vfs::cwd(),
            nice: crate::sched::nice(),
            limits: crate::system::limits(),
            main_stack: crate::thread::main_stack(),
            startup: runtime::env::before_fork(),
        };
        match gift.make() {
            Ok(()) => Ok(gift),
            Err(e) => {
                gift.close();
                Err(e)
            }
        }
    }

    fn make(&mut self) -> Result<(), isize> {
        // Each description once, for all its descriptors.
        let mut places: BTreeMap<usize, usize> = BTreeMap::new();
        for (n, desc, cloexec) in fd::snapshot() {
            let at = Arc::as_ptr(&desc) as usize;
            let place = match places.get(&at) {
                Some(&place) => place,
                None => {
                    let copied = match &desc.object {
                        Object::EventFd(e) => Copied::EventFd(e.fork_copy()),
                        Object::TimerFd(t) => Copied::TimerFd(t.fork_copy()),
                        Object::Epoll(e) => Copied::Epoll(e.fork_copy()),
                        _ => carry::for_fork(&desc)?.map_or(Copied::Absent, Copied::Carried),
                    };
                    self.descriptions.push(Going { at, flags: desc.flags(), copied });
                    places.insert(at, self.descriptions.len() - 1);
                    self.descriptions.len() - 1
                }
            };
            self.fds.push((n, place, cloexec));
        }
        let registry = proto::with_registry(|r| r.clone_registry()).map_err(|_| EIO)?;
        let Ok(Ok(registry)) = registry else { return Err(EIO) };
        self.registry = Some(registry.into_handle().into_raw());
        // The copy may signal this process, and wait for it, as a program
        // this one starts may.
        let rights = Rights(Rights::SIGNAL.0 | Rights::WAIT.0 | Rights::GET_INFO.0 | Rights::TRANSFER.0);
        let me = Process::current().map_err(error::kernel)?;
        self.parent = Some(me.0.duplicate(Some(rights)).map_err(error::kernel)?.into_raw());
        self.tty = crate::tty::vmo_for_child().map(|v| v.into_handle().into_raw());
        Ok(())
    }

    /// The names of the handles that move to the copy.
    fn names(&self) -> Vec<RawHandle> {
        let carried = self.descriptions.iter().filter_map(|d| match &d.copied {
            Copied::Carried(c) => Some(c.names()),
            _ => None,
        });
        let own = [self.registry, self.parent, self.tty];
        carried.flatten().copied().chain(own.into_iter().flatten()).collect()
    }

    /// No copy was made: what was made for it is closed.
    fn close(self) {
        for d in self.descriptions {
            if let Copied::Carried(c) = d.copied {
                c.close();
            }
        }
        for name in [self.registry, self.parent, self.tty].into_iter().flatten() {
            // SAFETY: a handle this process made for the copy, which no one
            // else names.
            drop(unsafe { Handle::from_raw(name) });
        }
    }

    /// In the copy: the layer's state is what was written down for it, with
    /// the handles that moved to it, and its descriptors are made again.
    ///
    /// # Safety
    /// In the copy, before it has another thread.
    unsafe fn take_up(self) {
        let Gift { descriptions, fds, registry, parent, tty, signals, cwd, nice, limits, main_stack, startup } = self;
        // SAFETY (whole block): the copy, before it has another thread, per
        // the caller; the handles named moved to it.
        unsafe {
            // First: what tells which process this is (`process::pid`).
            crate::own::forked();
            runtime::env::forked(startup);
            if let Some(r) = registry {
                proto::forked(Channel::from_handle(Handle::from_raw(r)));
            }
            if let Some(p) = parent {
                crate::process::forked(Process::from_handle(Handle::from_raw(p)));
            }
            let pid = crate::process::pid();
            crate::signal::forked(signals, pid);
            crate::thread::forked(pid, main_stack);
            crate::vfs::forked(cwd);
            crate::network::forked();
            crate::tty::forked(tty.map(|t| Vmo::from_handle(Handle::from_raw(t))));
            crate::timer::forked();
            crate::timerfd::forked();
            crate::lock::forked();
            crate::sched::forked(nice);
            crate::system::forked(limits);
        }
        epoll::forked();
        // The descriptions, made again: the epoll instances last, whose
        // interest lists are of the others.
        let mut made: Vec<Option<Arc<Description>>> = descriptions.iter().map(|_| None).collect();
        let mut by_address: BTreeMap<usize, Arc<Description>> = BTreeMap::new();
        let mut epolls = Vec::new();
        for (place, going) in descriptions.into_iter().enumerate() {
            let object = match going.copied {
                Copied::Carried(c) => {
                    if let Some(desc) = c.adopt() {
                        by_address.insert(going.at, desc.clone());
                        made[place] = Some(desc);
                    }
                    continue;
                }
                Copied::EventFd(copy) => EventFd::restored(copy).map(Object::EventFd),
                Copied::TimerFd(copy) => TimerFd::restored(copy).map(Object::TimerFd),
                Copied::Epoll(copy) => {
                    epolls.push((place, going.flags, copy));
                    continue;
                }
                Copied::Absent => continue,
            };
            if let Ok(object) = object {
                let desc = Description::new(object, going.flags);
                by_address.insert(going.at, desc.clone());
                made[place] = Some(desc);
            }
        }
        for (place, flags, copy) in epolls {
            if let Ok(e) = Epoll::restored(copy, |at| by_address.get(&at).cloned()) {
                made[place] = Some(Description::new(Object::Epoll(e), flags));
            }
        }
        let entries: Vec<(i32, Arc<Description>, bool)> =
            fds.into_iter().filter_map(|(n, place, cloexec)| made[place].clone().map(|d| (n, d, cloexec))).collect();
        // SAFETY: as above.
        unsafe { fd::forked(entries) };
    }
}
