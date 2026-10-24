// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Processes: an address space, a handle table and a set of threads, and
//! the POSIX signals other processes send them.
//!
//! A process that takes signals has a *signal entry* (`SIGNAL_ENTRY_SET`),
//! where its threads go when interrupted: a signal another process sends
//! it waits among its *incoming* signals, which it takes
//! (`SIGNAL_TAKE`), and the thread that set the entry (another, once that
//! one has ended) is interrupted to take them. Which of its threads acts
//! on each, by what it blocks and handles, is the process's own affair (its
//! POSIX layer's). A process without an entry yet has the signals it
//! inherited as ignored and as blocked so (`PROCESS_SIGNALS_INHERITED`): an
//! ignored one does nothing, and a blocked one waits among its incoming
//! signals for it to take once it has an entry; another ends it if it ends
//! a process by default, and leaves it alone if not.

use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering};

use abi::{ProcessInfo, SentSignals, process_state};

use super::handle::HandleTable;
use super::{Name, Signals};
use crate::memory::aspace::AddressSpace;
use crate::scheduler::{self, Budget, Thread, WakeReason};
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
    /// What its threads above `NORMAL` used of their budget there.
    pub elevated: SpinLock<Budget>,
    exited_threads_cpu_ns: AtomicU64,
    /// What it gave up ([`abi::restrict`]), for good.
    restrictions: AtomicUsize,
    /// The highest priority its threads may run at, given at its creation
    /// and never above its creator's: the scheduler runs the highest ready,
    /// so this bounds which processes its threads can keep from the
    /// processors.
    pub max_priority: u8,
    /// What the system ends first when memory runs out
    /// ([`abi::memory_class`]): its creator's at first, and only ever more
    /// expendable ([`Process::set_memory_class`]).
    memory_class: AtomicU8,
    /// Where its threads go when interrupted (0: nowhere, it takes no
    /// signals).
    signal_entry: AtomicU64,
    /// The thread that other processes' signals interrupt.
    signal_receiver: SpinLock<Weak<Thread>>,
    /// The signals other processes sent it, not taken yet.
    incoming: SpinLock<SentSignals>,
    /// The signals it inherited as ignored, and as blocked, which it has so
    /// until it has a signal entry.
    inherited_ignored: AtomicU64,
    inherited_blocked: AtomicU64,
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

    /// What the system ends first when memory runs out
    /// ([`abi::memory_class`]).
    pub fn memory_class(&self) -> u8 {
        self.memory_class.load(Ordering::Acquire)
    }

    /// Moves the process to `class`: `InvalidArgs` for none of
    /// [`abi::memory_class`]'s, `AccessDenied` for one less expendable than
    /// its own, which would shield it more than its creator chose.
    pub fn set_memory_class(&self, class: usize) -> Result<(), abi::Error> {
        if class > abi::memory_class::EXPENDABLE {
            return Err(abi::Error::InvalidArgs);
        }
        self.memory_class
            .try_update(Ordering::AcqRel, Ordering::Acquire, |now| (class as u8 >= now).then_some(class as u8))
            .map(|_| ())
            .map_err(|_| abi::Error::AccessDenied)
    }

    /// The code it was asked to end with ([`Process::kill`]), if it was,
    /// whether or not its threads have all ended yet.
    pub fn ending_with(&self) -> Option<i64> {
        *self.pending_code.lock()
    }

    pub fn new(name: &str, parent_koid: u64, max_priority: u8, memory_class: u8) -> Option<Arc<Process>> {
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
            elevated: SpinLock::new(Budget::new()),
            exited_threads_cpu_ns: AtomicU64::new(0),
            restrictions: AtomicUsize::new(0),
            max_priority,
            memory_class: AtomicU8::new(memory_class),
            signal_entry: AtomicU64::new(0),
            signal_receiver: SpinLock::new(Weak::new()),
            incoming: SpinLock::new(SentSignals::default()),
            inherited_ignored: AtomicU64::new(0),
            inherited_blocked: AtomicU64::new(0),
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

    /// Called by a thread of this process as it exits. The processor time
    /// it used comes to [`Process::keep_cpu_time`] as it is switched out
    /// for the last time. An interruption it had not acted on goes to
    /// another thread, for the signals it may have been for.
    pub fn thread_exited(self: &Arc<Self>, t: &Thread) {
        let last = {
            let mut threads = self.threads.lock();
            threads.retain(|x| x.koid != t.koid);
            threads.iter().all(|x| x.is_dead() || x.state() == scheduler::State::New)
        };
        if last {
            let code = self.pending_code.lock().unwrap_or(0);
            self.terminate(code);
        } else if t.interrupt.load(Ordering::Acquire) {
            self.interrupt_receiver(Some(t.koid));
        }
    }

    /// Where its threads go when interrupted, if it takes signals.
    pub fn signal_entry(&self) -> Option<u64> {
        match self.signal_entry.load(Ordering::Acquire) {
            0 => None,
            entry => Some(entry),
        }
    }

    /// Sets where its threads go when interrupted (`SIGNAL_ENTRY_SET`), and
    /// the thread that other processes' signals interrupt, which takes at
    /// once those that waited for it (it inherited them blocked).
    pub fn set_signal_entry(&self, entry: u64, receiver: &Arc<Thread>) {
        *self.signal_receiver.lock() = Arc::downgrade(receiver);
        self.signal_entry.store(entry, Ordering::Release);
        if self.incoming.lock().set != 0 {
            self.interrupt_receiver(None);
        }
    }

    /// Sets the signals it inherited as ignored and as blocked
    /// (`PROCESS_SIGNALS_INHERITED`), bit `n - 1` for signal `n`.
    pub fn set_inherited_signals(&self, ignored: u64, blocked: u64) {
        let kill = 1 << (abi::posix_signal::KILL - 1);
        self.inherited_ignored.store(ignored & !kill, Ordering::Release);
        self.inherited_blocked.store(blocked & !kill, Ordering::Release);
    }

    /// Sends it POSIX signal `s` from process `sender` (`PROCESS_SIGNAL`):
    /// it waits among the incoming signals of a process with a signal
    /// entry, whose receiving thread is interrupted; a process without one
    /// does with it as it inherited it (nothing if ignored; blocked, it
    /// waits among the incoming signals), else is ended if `s` ends a
    /// process by default. `SIGKILL` ends it.
    pub fn signal(self: &Arc<Self>, s: usize, sender: u64) {
        let bit = 1 << (s - 1);
        if self.signal_entry().is_none() {
            if self.inherited_ignored.load(Ordering::Acquire) & bit != 0 {
                return;
            }
            if self.inherited_blocked.load(Ordering::Acquire) & bit != 0 {
                self.incoming.lock().add(s, sender);
                return;
            }
        }
        if s == abi::posix_signal::KILL || self.signal_entry().is_none() {
            if abi::posix_signal::ends_process(s) {
                self.kill(abi::EXIT_CODE_SIGNALED - s as i64);
            }
            return;
        }
        self.incoming.lock().add(s, sender);
        self.interrupt_receiver(None);
    }

    /// The signals other processes sent it, not taken yet.
    pub fn sent_signals(&self) -> SentSignals {
        *self.incoming.lock()
    }

    /// Takes signals `set` from those sent it ([`Process::sent_signals`]).
    pub fn take_signals(&self, set: u64) {
        let mut incoming = self.incoming.lock();
        incoming.set &= !set;
    }

    /// Interrupts the thread that takes the signals other processes send it:
    /// the one that set its signal entry or, once that one has ended (or is
    /// `but`), another that has started and acts on interruptions.
    pub fn interrupt_receiver(&self, but: Option<u64>) {
        let eligible = |t: &Arc<Thread>| {
            but != Some(t.koid)
                && !matches!(t.state(), scheduler::State::Dead | scheduler::State::New)
                && scheduler::takes_interruptions(t)
        };
        let receiver = self.signal_receiver.lock().upgrade().filter(eligible);
        let t = receiver.or_else(|| self.threads.lock().iter().find(|t| eligible(t)).cloned());
        if let Some(t) = t {
            scheduler::interrupt(&t);
        }
    }

    /// Keeps `ns`, the processor time of a thread of this process that has
    /// exited, as the scheduler switches it out for the last time: until
    /// then (with the BKL held throughout) it is in neither its threads'
    /// time nor this.
    pub fn keep_cpu_time(&self, ns: u64) {
        self.exited_threads_cpu_ns.fetch_add(ns, Ordering::Relaxed);
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
                    if cpu != crate::scheduler::percpu::cpu_id_or_boot() {
                        crate::architecture::smp::reschedule(cpu);
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
        let level = match code {
            abi::EXIT_CODE_CRASHED | abi::EXIT_CODE_OUT_OF_MEMORY => crate::diagnostics::log::Level::Warn,
            _ => crate::diagnostics::log::Level::Debug,
        };
        crate::diagnostics::log::write(
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
        let running_cpu: u64 = threads.iter().map(|t| scheduler::cpu_time_ns(t)).sum();
        let state = match self.state() {
            ProcessState::Created | ProcessState::Running => process_state::RUNNING,
            ProcessState::Terminated { code } if code == abi::EXIT_CODE_CRASHED => process_state::CRASHED,
            ProcessState::Terminated { code } if code == abi::EXIT_CODE_KILLED => process_state::KILLED,
            ProcessState::Terminated { code } if code == abi::EXIT_CODE_OUT_OF_MEMORY => process_state::OUT_OF_MEMORY,
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
            max_priority: self.max_priority.into(),
            memory_class: self.memory_class().into(),
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

/// The `n` live processes that hold the most memory, the most first, each
/// with the bytes committed to what it maps (for the log, when memory runs
/// out: what took it).
pub fn largest(n: usize) -> Vec<(Arc<Process>, u64)> {
    let mut sized: Vec<(Arc<Process>, u64)> = all()
        .into_iter()
        .filter(|p| !p.is_terminated())
        .map(|p| {
            let bytes = p.aspace().map_or(0, |a| a.committed_bytes());
            (p, bytes)
        })
        .collect();
    sized.sort_unstable_by_key(|&(_, bytes)| core::cmp::Reverse(bytes));
    sized.truncate(n);
    sized
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
