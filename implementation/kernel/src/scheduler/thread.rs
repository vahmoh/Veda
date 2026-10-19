// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Threads: the unit of scheduling.

use alloc::sync::Arc;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use crate::arch::thread::Context;
use crate::memory::aspace::AddressSpace;
use crate::memory::kvirt::KernelStack;
use crate::object::process::Process;
use crate::object::{Name, Signals};
use crate::synchronization::SpinLock;

/// Kernel stack size of every thread.
pub const KERNEL_STACK_PAGES: u64 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Created but not started.
    New,
    Ready,
    Running,
    Blocked,
    Dead,
}

/// Why a blocked thread was woken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeReason {
    None,
    Signaled,
    TimedOut,
    Killed,
}

pub struct SchedData {
    pub state: State,
    pub priority: u8,
    pub wake_reason: WakeReason,
    /// Key in the timeout queue while blocked with a deadline.
    pub timeout_key: Option<(u64, u64)>,
    pub cpu: u32,
}

pub struct Thread {
    pub koid: u64,
    pub name: Name,
    pub process: Option<Arc<Process>>,
    pub aspace: Option<Arc<AddressSpace>>,
    /// Owns the kernel stack, which is freed with the thread.
    _kstack: Option<KernelStack>,
    pub kernel_stack_top: u64,
    pub sched: SpinLock<SchedData>,
    ctx: UnsafeCell<Context>,
    pub signals: Signals,
    pub cpu_time_ns: AtomicU64,
    pub kill_pending: AtomicBool,
    pub is_idle: bool,
    /// User address of a 32-bit word to clear, and wake a futex waiter on,
    /// when the thread exits (0: none; see `THREAD_SET_EXIT_FUTEX`).
    pub exit_futex: AtomicU64,
    /// The suspensions in force (`THREAD_SUSPEND`s less `THREAD_RESUME`s,
    /// changed under the BKL): while there are any, the thread runs no
    /// user code.
    pub suspensions: AtomicU32,
    /// Whether the thread waits in the kernel for its suspensions to end,
    /// which only their end (or its death) wakes it from.
    pub parked: AtomicBool,
    /// When it was last put in the run queue (monotonic nanoseconds): how
    /// long a ready thread has waited for a processor
    /// (`super::boost_starved`).
    pub ready_since: AtomicU64,
    /// Until when it runs at `NORMAL` at least (monotonic nanoseconds): a
    /// time slice the scheduler gives a thread below `NORMAL` that waited
    /// too long ready (`super::boost_starved`), `u64::MAX` until the slice
    /// begins, as the thread next runs; 0 if none.
    pub boost_until: AtomicU64,
}

// SAFETY: `ctx` is only accessed by the scheduler under the BKL; everything
// else is synchronised.
unsafe impl Sync for Thread {}
// SAFETY: as above.
unsafe impl Send for Thread {}

impl Thread {
    fn build(
        name: &str,
        process: Option<Arc<Process>>,
        aspace: Option<Arc<AddressSpace>>,
        kstack: Option<KernelStack>,
        is_idle: bool,
    ) -> Option<Arc<Thread>> {
        let top = kstack.as_ref().map(|s| s.top()).unwrap_or(0);
        Some(Arc::new(Thread {
            koid: crate::object::new_koid(),
            name: Name::new(name),
            process,
            aspace,
            _kstack: kstack,
            kernel_stack_top: top,
            sched: SpinLock::new(SchedData {
                state: State::New,
                priority: abi::priority::NORMAL as u8,
                wake_reason: WakeReason::None,
                timeout_key: None,
                cpu: 0,
            }),
            ctx: UnsafeCell::new(Context::new(!is_idle)?),
            signals: Signals::new(0),
            cpu_time_ns: AtomicU64::new(0),
            kill_pending: AtomicBool::new(false),
            is_idle,
            exit_futex: AtomicU64::new(0),
            suspensions: AtomicU32::new(0),
            parked: AtomicBool::new(false),
            ready_since: AtomicU64::new(0),
            boost_until: AtomicU64::new(0),
        }))
    }

    /// A thread that will run user code in `process`, at `NORMAL` priority
    /// or the most the process may have, if that is lower. Its kernel stack
    /// is memory taken on user space's behalf, refused within the kernel's
    /// reserve (`phys::user_may_use`).
    pub fn new_user(process: &Arc<Process>, name: &str) -> Option<Arc<Thread>> {
        let aspace = process.aspace()?;
        if !crate::memory::phys::user_may_use(KERNEL_STACK_PAGES * crate::memory::PAGE_SIZE) {
            return None;
        }
        let stack = KernelStack::new(KERNEL_STACK_PAGES)?;
        let t = Thread::build(name, Some(process.clone()), Some(aspace), Some(stack), false)?;
        {
            let mut s = t.sched.lock();
            s.priority = s.priority.min(process.max_priority);
        }
        Some(t)
    }

    /// The highest priority the thread may be given: its process's maximum
    /// (a kernel thread's is any).
    pub fn max_priority(&self) -> u8 {
        self.process.as_ref().map_or(abi::priority::MAX as u8, |p| p.max_priority)
    }

    /// The idle thread representing the code already running on this CPU
    /// (its context is captured at the first switch away from it).
    pub fn new_idle_current(cpu: u32) -> Arc<Thread> {
        let t = Thread::build("idle", None, None, None, true).expect("out of memory creating the idle thread");
        {
            let mut s = t.sched.lock();
            s.state = State::Running;
            s.priority = 0;
            s.cpu = cpu;
        }
        t
    }

    /// A kernel thread that starts executing `entry(arg)`.
    pub fn new_kernel(name: &str, entry: fn(u64) -> !, arg: u64, idle: bool) -> Option<Arc<Thread>> {
        let stack = KernelStack::new(KERNEL_STACK_PAGES)?;
        let t = Thread::build(name, None, None, Some(stack), idle)?;
        // SAFETY: the thread has not run yet; its context is ours.
        unsafe { (*t.ctx.get()).prepare_kernel(t.kernel_stack_top, entry, arg) };
        if idle {
            t.sched.lock().priority = 0;
        }
        Some(t)
    }

    /// Prepares the kernel stack so that the first switch to this thread
    /// enters user mode at `entry` with the given stack and arguments.
    pub fn prepare_user_entry(&self, entry: u64, user_stack: u64, arg0: u64, arg1: u64) {
        // SAFETY: the thread has not run yet; its context is ours.
        unsafe { (*self.ctx.get()).prepare_user(self.kernel_stack_top, entry, user_stack, arg0, arg1) };
    }

    pub fn state(&self) -> State {
        self.sched.lock().state
    }

    pub fn priority(&self) -> u8 {
        self.sched.lock().priority
    }

    /// The priority it is scheduled at, at `now`: its own, or `NORMAL`
    /// during a time slice the scheduler gave it after it waited too long
    /// below `NORMAL` ([`Thread::boost_until`]).
    pub fn effective_priority(&self, now: u64) -> u8 {
        let priority = self.priority();
        let normal = abi::priority::NORMAL as u8;
        if priority < normal && now < self.boost_until.load(Ordering::Relaxed) { normal } else { priority }
    }

    /// The saved register context. Only the scheduler (under the BKL) and
    /// the thread itself may dereference it.
    pub fn ctx_ptr(&self) -> *mut Context {
        self.ctx.get()
    }

    pub fn process_name(&self) -> &str {
        self.process.as_ref().map(|p| p.name.as_str()).unwrap_or("kernel")
    }

    pub fn is_dead(&self) -> bool {
        self.state() == State::Dead
    }

    pub fn mark_kill(&self) {
        self.kill_pending.store(true, Ordering::Release);
    }
}
