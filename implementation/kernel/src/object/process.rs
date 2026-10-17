// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Processes: an address space, a handle table and a set of threads.

use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use abi::{ProcessInfo, process_state};

use super::handle::HandleTable;
use super::{Name, Signals};
use crate::memory::aspace::AddressSpace;
use crate::scheduler::{self, Thread, WakeReason};
use crate::synchronization::SpinLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessState {
    /// Created, no thread started yet.
    Created,
    Running,
    /// All threads have exited.
    Terminated {
        code: i64,
    },
}

pub struct Process {
    pub koid: u64,
    pub parent_koid: u64,
    pub name: Name,
    aspace: SpinLock<Option<Arc<AddressSpace>>>,
    pub handles: SpinLock<HandleTable>,
    threads: SpinLock<Vec<Arc<Thread>>>,
    state: SpinLock<ProcessState>,
    /// Exit code requested by `process_exit`/kill, applied at termination.
    pending_code: SpinLock<Option<i64>>,
    pub signals: Signals,
    exited_threads_cpu_ns: AtomicU64,
    /// What it gave up ([`abi::restrict`]), for good.
    restrictions: AtomicUsize,
}

/// Every process ever created that is still referenced somewhere.
static PROCESSES: SpinLock<Vec<Weak<Process>>> = SpinLock::new(Vec::new());

/// The most threads a process may have.
pub const MAX_THREADS: usize = 1024;
/// The most processes that may be alive (not terminated) at once, and the
/// most of them one process may have started: each holds kernel memory (an
/// address space, a handle table), so neither one process nor its
/// descendants may exhaust it.
pub const MAX_PROCESSES: usize = 4096;
pub const MAX_CHILDREN: usize = 1024;

/// Whether `parent` may start one more process (`LimitReached` if not).
pub fn may_create(parent: u64) -> Result<(), abi::Error> {
    let all = all();
    let live = all.iter().filter(|p| !p.is_terminated());
    let (total, children) = live.fold((0, 0), |(t, c), p| (t + 1, c + usize::from(p.parent_koid == parent)));
    if total >= MAX_PROCESSES || children >= MAX_CHILDREN { Err(abi::Error::LimitReached) } else { Ok(()) }
}

impl Process {
    /// Takes `flags` ([`abi::restrict`]) from the process, for good.
    pub fn restrict(&self, flags: usize) {
        self.restrictions.fetch_or(flags, Ordering::AcqRel);
    }

    /// Whether the process gave up `flag`.
    pub fn restricted(&self, flag: usize) -> bool {
        self.restrictions.load(Ordering::Acquire) & flag != 0
    }

    pub fn new(name: &str, parent_koid: u64) -> Option<Arc<Process>> {
        let p = Arc::new(Process {
            koid: super::new_koid(),
            parent_koid,
            name: Name::new(name),
            aspace: SpinLock::new(Some(AddressSpace::new()?)),
            handles: SpinLock::new(HandleTable::new()),
            threads: SpinLock::new(Vec::new()),
            state: SpinLock::new(ProcessState::Created),
            pending_code: SpinLock::new(None),
            signals: Signals::new(0),
            exited_threads_cpu_ns: AtomicU64::new(0),
            restrictions: AtomicUsize::new(0),
        });
        let mut list = PROCESSES.lock();
        list.retain(|w| w.strong_count() > 0);
        list.push(Arc::downgrade(&p));
        Some(p)
    }

    /// The address space, unless the process has terminated.
    pub fn aspace(&self) -> Option<Arc<AddressSpace>> {
        self.aspace.lock().clone()
    }

    pub fn state(&self) -> ProcessState {
        *self.state.lock()
    }

    pub fn is_terminated(&self) -> bool {
        matches!(self.state(), ProcessState::Terminated { .. })
    }

    /// Registers a new thread: `BadState` once the process has terminated,
    /// `LimitReached` if it has [`MAX_THREADS`] already.
    pub fn add_thread(&self, t: Arc<Thread>) -> Result<(), abi::Error> {
        if self.is_terminated() {
            return Err(abi::Error::BadState);
        }
        let mut threads = self.threads.lock();
        if threads.len() >= MAX_THREADS {
            return Err(abi::Error::LimitReached);
        }
        threads.push(t);
        Ok(())
    }

    /// Marks the process running (first thread started).
    pub fn mark_started(&self) {
        let mut s = self.state.lock();
        if *s == ProcessState::Created {
            *s = ProcessState::Running;
        }
    }

    /// Called by a thread of this process as it exits.
    pub fn thread_exited(self: &Arc<Self>, t: &Thread) {
        self.exited_threads_cpu_ns.fetch_add(t.cpu_time_ns.load(Ordering::Relaxed), Ordering::Relaxed);
        let last = {
            let mut threads = self.threads.lock();
            threads.retain(|x| x.koid != t.koid);
            threads.iter().all(|x| x.is_dead() || x.state() == scheduler::State::New)
        };
        if last {
            let code = self.pending_code.lock().unwrap_or(0);
            self.terminate(code);
        }
    }

    /// Requests termination of every thread with `code` (kill or crash).
    pub fn kill(self: &Arc<Self>, code: i64) {
        {
            let mut pc = self.pending_code.lock();
            if pc.is_none() {
                *pc = Some(code);
            }
        }
        let threads: Vec<Arc<Thread>> = self.threads.lock().clone();
        if threads.iter().all(|t| t.state() == scheduler::State::New) {
            // Nothing is running: terminate right away.
            self.terminate(code);
            return;
        }
        for t in threads {
            t.mark_kill();
            match t.state() {
                scheduler::State::Blocked => scheduler::wake(&t, WakeReason::Killed),
                scheduler::State::Running => {
                    let cpu = t.sched.lock().cpu;
                    if cpu != crate::percpu::cpu_id_or_boot() {
                        crate::arch::smp::reschedule(cpu);
                    }
                }
                _ => {}
            }
        }
    }

    /// Final teardown: close all handles, release the address space and
    /// signal `TERMINATED`.
    fn terminate(&self, code: i64) {
        {
            let mut s = self.state.lock();
            if matches!(*s, ProcessState::Terminated { .. }) {
                return;
            }
            *s = ProcessState::Terminated { code };
        }
        let handles = self.handles.lock().take_all();
        drop(handles);
        let unstarted: Vec<Arc<Thread>> = core::mem::take(&mut *self.threads.lock());
        drop(unstarted);
        let aspace = self.aspace.lock().take();
        drop(aspace);
        let level = if code == abi::EXIT_CODE_CRASHED { crate::log::Level::Warn } else { crate::log::Level::Debug };
        crate::log::write(
            level,
            format_args!("process {} ({}) exited with code {}", self.name.as_str(), self.koid, code),
        );
        self.signals.update(0, abi::signals::TERMINATED);
    }

    pub fn exit_code(&self) -> Option<i64> {
        match self.state() {
            ProcessState::Terminated { code } => Some(code),
            _ => None,
        }
    }

    pub fn info(&self) -> ProcessInfo {
        let threads = self.threads.lock();
        let running_cpu: u64 = threads.iter().map(|t| t.cpu_time_ns.load(Ordering::Relaxed)).sum();
        let state = match self.state() {
            ProcessState::Created | ProcessState::Running => process_state::RUNNING,
            ProcessState::Terminated { code } if code == abi::EXIT_CODE_CRASHED => process_state::CRASHED,
            ProcessState::Terminated { code } if code == abi::EXIT_CODE_KILLED => process_state::KILLED,
            ProcessState::Terminated { .. } => process_state::EXITED,
        };
        ProcessInfo {
            koid: self.koid,
            parent_koid: self.parent_koid,
            name: self.name.raw(),
            state,
            threads: threads.len() as u32,
            exit_code: self.exit_code().unwrap_or(0),
            memory_bytes: self.aspace().map(|a| a.committed_bytes()).unwrap_or(0),
            cpu_time_ns: running_cpu + self.exited_threads_cpu_ns.load(Ordering::Relaxed),
            handles: self.handles.lock().len() as u32,
            _reserved: 0,
        }
    }

    pub fn thread_count(&self) -> usize {
        self.threads.lock().len()
    }
}

/// All live processes.
pub fn all() -> Vec<Arc<Process>> {
    PROCESSES.lock().iter().filter_map(|w| w.upgrade()).collect()
}

/// The live processes `koid` started, those they started, and so on.
pub fn descendants(koid: u64) -> Vec<Arc<Process>> {
    // Sorted by parent, each process's children are one run: one pass
    // over the processes, then a lookup per process found.
    let mut all = all();
    all.sort_unstable_by_key(|p| p.parent_koid);
    let mut found: Vec<Arc<Process>> = Vec::new();
    let mut parents = alloc::vec![koid];
    while let Some(parent) = parents.pop() {
        let first = all.partition_point(|p| p.parent_koid < parent);
        for p in all[first..].iter().take_while(|p| p.parent_koid == parent) {
            // Process ids are unique and parents older than their children,
            // so no process is found twice.
            parents.push(p.koid);
            found.push(p.clone());
        }
    }
    found
}

pub fn find(koid: u64) -> Option<Arc<Process>> {
    PROCESSES.lock().iter().filter_map(|w| w.upgrade()).find(|p| p.koid == koid)
}
