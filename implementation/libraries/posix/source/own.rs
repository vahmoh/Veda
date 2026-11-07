// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What a thread has of its own when it is a process of its own: a child
//! that `vfork` (`clone` with `CLONE_VM`) made, which runs in its parent's
//! memory until it replaces its program (`execve`, which starts a process
//! of Veda's for it) or ends. On Linux such a child is a process: its
//! descriptors, working directory and signal dispositions are copies of
//! its parent's (unless `CLONE_FILES`, `CLONE_FS` or `CLONE_SIGHAND` share
//! them), and its id is its own. Here it is a thread of the parent's
//! process, which a host (Veda's Linux compatibility) made, and the layer
//! gives it those of its own: the descriptors and working directory here
//! ([`Own`]), the dispositions with the signals (`crate::signal`), its id
//! that of the process its `execve` will start in, made beforehand
//! (`crate::process::reserve_child`).
//!
//! Each thread's own state is found by its thread pointer, and only while
//! some thread has one ([`current`]): the layer's calls cost nothing more
//! otherwise.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicUsize, Ordering};

use runtime::sync::Mutex;

use crate::fd::Table;

/// What a thread that is a process of its own has of its own.
pub struct Own {
    /// Its descriptors, unless it shares its parent's (`CLONE_FILES`).
    pub fds: Option<Mutex<Table>>,
    /// Its working directory (the VFS's path), unless it shares its
    /// parent's (`CLONE_FS`).
    pub cwd: Option<Mutex<String>>,
    /// Its process id, and its parent's.
    pub pid: i32,
    pub ppid: i32,
}

/// The threads that are processes of their own, by thread pointer.
static OWN: Mutex<BTreeMap<usize, Arc<Own>>> = Mutex::new(BTreeMap::new());

/// How many there are: none, most of the time.
static COUNT: AtomicUsize = AtomicUsize::new(0);

/// In a process `fork` made: no thread is a process of its own (the one it
/// has made it, and is none: `crate::fork`).
///
/// # Safety
/// As `runtime::sync::Mutex::reset_after_fork`: in the copy, before it has
/// another thread.
pub unsafe fn forked() {
    // SAFETY: per the caller.
    unsafe { OWN.reset_after_fork(BTreeMap::new()) };
    COUNT.store(0, Ordering::Release);
}

/// The calling thread's own state, if it is a process of its own.
pub fn current() -> Option<Arc<Own>> {
    if COUNT.load(Ordering::Acquire) == 0 || !crate::signal::threads_are_ready() {
        return None;
    }
    OWN.lock().get(&crate::architecture::thread_pointer()).cloned()
}

/// Makes the thread whose thread pointer is `tp` a process of its own,
/// with id `pid`: its descriptors and working directory copies of the
/// calling thread's unless it shares them (`files`, `fs`), its signal
/// dispositions a copy unless it shares them (`handlers`).
pub fn begin(tp: usize, pid: i32, files: bool, fs: bool, handlers: bool) {
    let own = Own {
        fds: (!files).then(|| Mutex::new(crate::fd::copy())),
        cwd: (!fs).then(|| Mutex::new(crate::vfs::cwd())),
        pid,
        ppid: crate::process::pid(),
    };
    crate::signal::set_apart(tp, handlers);
    if OWN.lock().insert(tp, Arc::new(own)).is_none() {
        COUNT.fetch_add(1, Ordering::AcqRel);
    }
}

/// The thread whose thread pointer is `tp` is no process of its own any
/// more (it has ended): its descriptors close.
pub fn end(tp: usize) {
    let gone = OWN.lock().remove(&tp);
    if gone.is_some() {
        COUNT.fetch_sub(1, Ordering::AcqRel);
    }
    drop(gone);
}
