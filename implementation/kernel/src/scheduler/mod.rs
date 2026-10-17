// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The scheduler.
//!
//! * 32 priority levels with round-robin within a level; a global run queue
//!   shared by all CPUs (the BKL makes a global queue cheap and fair).
//! * Preemptive: a running thread is preempted when its 10 ms time slice
//!   ends and a thread of equal or higher priority is ready, or immediately
//!   when a higher-priority thread wakes up.
//! * Tickless: each CPU arms its timer (`arch::timer`) for its next event
//!   (time slice end or the earliest sleeping thread's deadline).
//! * Every CPU has an idle thread that halts until an interrupt arrives.

pub mod thread;

use alloc::collections::{BTreeMap, VecDeque};
use alloc::sync::{Arc, Weak};
use core::sync::atomic::Ordering;

pub use thread::{State, Thread, WakeReason};

use crate::arch::trap::TrapFrame;
use crate::arch::{self, cpu};
use crate::percpu;
use crate::synchronization::{SpinLock, bkl};
use crate::time;

/// Length of a time slice.
pub const TIME_SLICE_NS: u64 = 10_000_000;
/// Longest an idle CPU sleeps without a timer event.
const IDLE_MAX_NS: u64 = 500_000_000;
/// While the serial port has not all of the log, the boot processor's
/// timer comes this soon, to give it more: its transmitter empties its
/// FIFO in a millisecond or two (16 bytes at 115200 baud).
const SERIAL_NS: u64 = 1_000_000;

/// Per-CPU scheduler state (lives in `PerCpu`).
pub struct CpuSched {
    pub current: Option<Arc<Thread>>,
    pub idle: Option<Arc<Thread>>,
    /// The thread we just switched away from, kept alive until the switch has
    /// completed (its stack was in use during the switch).
    pub prev: Option<Arc<Thread>>,
    pub need_resched: bool,
    pub slice_end: u64,
    pub switched_in_at: u64,
}

impl CpuSched {
    pub const fn new() -> Self {
        CpuSched { current: None, idle: None, prev: None, need_resched: false, slice_end: 0, switched_in_at: 0 }
    }
}

#[allow(clippy::mut_from_ref)]
fn cpu_sched() -> &'static mut CpuSched {
    // SAFETY: each CPU only touches its own scheduler state, with interrupts
    // disabled and the BKL held; callers keep the borrow short.
    unsafe { &mut *percpu::current().sched.get() }
}

struct RunQueue {
    levels: [VecDeque<Arc<Thread>>; 32],
    bitmap: u32,
    len: usize,
}

impl RunQueue {
    fn push(&mut self, t: Arc<Thread>, priority: u8) {
        let p = priority as usize & 31;
        self.levels[p].push_back(t);
        self.bitmap |= 1 << p;
        self.len += 1;
    }

    fn highest(&self) -> Option<u8> {
        if self.bitmap == 0 { None } else { Some(31 - self.bitmap.leading_zeros() as u8) }
    }

    fn pop(&mut self) -> Option<Arc<Thread>> {
        let p = self.highest()? as usize;
        let t = self.levels[p].pop_front();
        if self.levels[p].is_empty() {
            self.bitmap &= !(1 << p);
        }
        self.len -= 1;
        t
    }
}

static RUNQ: SpinLock<RunQueue> =
    SpinLock::new(RunQueue { levels: [const { VecDeque::new() }; 32], bitmap: 0, len: 0 });

/// Blocked threads with a deadline, keyed by (deadline, koid).
static TIMEOUTS: SpinLock<BTreeMap<(u64, u64), Weak<Thread>>> = SpinLock::new(BTreeMap::new());

/// The thread running on this CPU.
pub fn current() -> Arc<Thread> {
    cpu_sched().current.clone().expect("scheduler not initialised on this CPU")
}

/// Like [`current`] but usable before the scheduler is up.
pub fn try_current() -> Option<Arc<Thread>> {
    cpu_sched().current.clone()
}

/// Installs the idle thread of this CPU as its current thread.
pub fn init_cpu(idle: Arc<Thread>) {
    let cs = cpu_sched();
    cs.idle = Some(idle.clone());
    cs.current = Some(idle);
    cs.switched_in_at = time::now_ns();
}

/// Makes a new or blocked thread runnable.
pub fn make_ready(t: &Arc<Thread>) {
    let prio = {
        let mut s = t.sched.lock();
        debug_assert!(matches!(s.state, State::New | State::Blocked));
        s.state = State::Ready;
        s.priority
    };
    RUNQ.lock().push(t.clone(), prio);
    after_wakeup(prio);
}

/// Preemption and idle-CPU wake-up after `prio` became runnable.
fn after_wakeup(prio: u8) {
    let me = percpu::cpu_id_or_boot();
    if let Some(cur) = try_current()
        && (cur.is_idle || prio > cur.priority())
    {
        cpu_sched().need_resched = true;
    }
    // Wake one idle CPU (other than us) to pick up the work: one that has
    // not been woken already, so that threads made ready together (a thread
    // pool's workers, say) start on different CPUs at once.
    for p in percpu::online() {
        if p.cpu_id == me {
            continue;
        }
        // SAFETY: reading another CPU's current thread pointer under the BKL;
        // it only changes while that CPU holds the BKL.
        let idle = unsafe { (*p.sched.get()).current.as_ref().is_some_and(|t| t.is_idle) };
        if idle && !p.resched_pending.swap(true, Ordering::AcqRel) {
            arch::smp::reschedule(p.cpu_id);
            break;
        }
    }
}

/// Wakes a blocked thread. No effect on threads in other states.
pub fn wake(t: &Arc<Thread>, reason: WakeReason) {
    let prio = {
        let mut s = t.sched.lock();
        if s.state != State::Blocked {
            return;
        }
        s.state = State::Ready;
        s.wake_reason = reason;
        if let Some(key) = s.timeout_key.take() {
            TIMEOUTS.lock().remove(&key);
        }
        s.priority
    };
    RUNQ.lock().push(t.clone(), prio);
    after_wakeup(prio);
}

/// Blocks the current thread until [`wake`] is called or `deadline`
/// (monotonic ns) passes.
pub fn block(deadline: Option<u64>) -> WakeReason {
    let cur = current();
    if cur.kill_pending.load(Ordering::Acquire) {
        return WakeReason::Killed;
    }
    if let Some(d) = deadline
        && d <= time::now_ns()
    {
        return WakeReason::TimedOut;
    }
    {
        let mut s = cur.sched.lock();
        s.state = State::Blocked;
        s.wake_reason = WakeReason::None;
        if let Some(d) = deadline {
            let key = (d, cur.koid);
            TIMEOUTS.lock().insert(key, Arc::downgrade(&cur));
            s.timeout_key = Some(key);
        }
    }
    schedule();
    let mut s = cur.sched.lock();
    if let Some(key) = s.timeout_key.take() {
        TIMEOUTS.lock().remove(&key);
    }
    s.wake_reason
}

/// Sleeps until `deadline` (monotonic ns).
pub fn sleep_until(deadline: u64) -> WakeReason {
    loop {
        match block(Some(deadline)) {
            WakeReason::TimedOut => return WakeReason::TimedOut,
            WakeReason::Killed => return WakeReason::Killed,
            _ if time::now_ns() >= deadline => return WakeReason::TimedOut,
            _ => {}
        }
    }
}

/// Wakes every thread whose deadline has passed.
fn expire_timeouts(now: u64) {
    loop {
        let t = {
            let mut q = TIMEOUTS.lock();
            match q.first_key_value() {
                Some((&(d, _), _)) if d <= now => q.pop_first().and_then(|(_, w)| w.upgrade()),
                _ => break,
            }
        };
        if let Some(t) = t {
            // The key was already removed; clear it so `wake` does not look.
            t.sched.lock().timeout_key = None;
            wake(&t, WakeReason::TimedOut);
        }
    }
}

fn earliest_timeout() -> Option<u64> {
    TIMEOUTS.lock().first_key_value().map(|(&(d, _), _)| d)
}

/// Arms this CPU's timer for its next event.
fn arm_timer(running_idle: bool) {
    let now = time::now_ns();
    let mut deadline = if running_idle { now + IDLE_MAX_NS } else { cpu_sched().slice_end };
    if let Some(t) = earliest_timeout() {
        deadline = deadline.min(t);
    }
    if percpu::cpu_id_or_boot() == 0 && crate::log::serial_pending() {
        deadline = deadline.min(now + SERIAL_NS);
    }
    let deadline = deadline.max(now + 50_000);
    arch::timer::arm(deadline, now);
}

/// Picks the next thread and switches to it if it differs from the current
/// one. The current thread must already have set its state if it is
/// blocking or exiting; a `Running` current thread is preempted only if
/// another thread of at least its priority is ready.
pub fn schedule() {
    percpu::current().resched_pending.store(false, Ordering::Release);
    let cs = cpu_sched();
    cs.need_resched = false;
    let cur = cs.current.clone().expect("schedule() without a current thread");
    let cur_state = cur.state();
    let next = {
        let mut rq = RUNQ.lock();
        if cur_state == State::Running && !cur.is_idle {
            match rq.highest() {
                Some(p) if p >= cur.priority() => {
                    let next = rq.pop().unwrap();
                    cur.sched.lock().state = State::Ready;
                    let prio = cur.priority();
                    rq.push(cur.clone(), prio);
                    next
                }
                _ => {
                    drop(rq);
                    // Keep running; start a fresh slice if this one ended.
                    if time::now_ns() >= cs.slice_end {
                        cs.slice_end = time::now_ns() + TIME_SLICE_NS;
                    }
                    arm_timer(false);
                    return;
                }
            }
        } else {
            match rq.pop() {
                Some(t) => t,
                None if cur.is_idle => {
                    drop(rq);
                    arm_timer(true);
                    return;
                }
                None => cs.idle.clone().expect("no idle thread"),
            }
        }
    };
    switch_to(cur, next);
}

/// Performs the actual switch from `prev` (the current thread) to `next`.
fn switch_to(prev: Arc<Thread>, next: Arc<Thread>) {
    let cs = cpu_sched();
    let now = time::now_ns();
    let ran = now.saturating_sub(cs.switched_in_at);
    prev.cpu_time_ns.fetch_add(ran, Ordering::Relaxed);
    if prev.is_idle {
        percpu::current().idle_ns.fetch_add(ran, Ordering::Relaxed);
    }
    cs.switched_in_at = now;

    match &next.aspace {
        Some(a) => a.activate(),
        None => crate::memory::paging::activate_kernel(),
    }
    {
        let mut s = next.sched.lock();
        s.state = State::Running;
        s.cpu = percpu::cpu_id_or_boot();
    }
    cs.slice_end = now + TIME_SLICE_NS;
    arm_timer(next.is_idle);

    let (prev_ctx, next_ctx) = (prev.ctx_ptr(), next.ctx_ptr());
    // SAFETY: contexts are only touched here, under the BKL.
    if !unsafe { (*next_ctx).is_ready() } {
        panic!(
            "switching to thread {} ({} of {}, idle={}, state={:?}) with no saved context; prev {} ({})",
            next.koid,
            next.name.as_str(),
            next.process_name(),
            next.is_idle,
            next.state(),
            prev.koid,
            prev.process_name()
        );
    }
    let next_stack_top = next.kernel_stack_top;
    cs.current = Some(next);
    cs.prev = Some(prev);
    // SAFETY: `next` was prepared by `Thread::new_*` or saved by a previous
    // switch; `prev` stays alive in `cs.prev` until the switch is complete.
    unsafe { arch::thread::switch(prev_ctx, next_ctx, next_stack_top) };
    finish_switch();
}

/// Runs on the incoming thread right after a switch: releases the outgoing
/// thread (possibly freeing it if it died).
fn finish_switch() {
    let prev = cpu_sched().prev.take();
    drop(prev);
}

/// First Rust code run by a new user thread.
pub fn thread_start_hook() {
    finish_switch();
    let cur = current();
    if cur.kill_pending.load(Ordering::Acquire) {
        exit_current();
    }
    bkl::release();
}

/// First Rust code run by a new kernel thread.
pub fn kernel_thread_start_hook() {
    finish_switch();
}

/// Whether this CPU should run the scheduler before it goes on with the
/// current thread (a higher-priority thread woke, or the slice ended).
pub fn need_resched() -> bool {
    cpu_sched().need_resched
}

/// Requests a reschedule at the next opportunity (RESCHED IPI).
pub fn request_resched() {
    cpu_sched().need_resched = true;
}

/// The CPU's timer interrupt.
pub fn timer_interrupt() {
    let now = time::now_ns();
    expire_timeouts(now);
    if percpu::cpu_id_or_boot() == 0 {
        crate::log::pump_serial();
    }
    let cs = cpu_sched();
    let cur_idle = cs.current.as_ref().is_some_and(|t| t.is_idle);
    if !cur_idle && now >= cs.slice_end {
        cs.need_resched = true;
    }
    if !cs.need_resched {
        arm_timer(cur_idle);
    }
}

/// Called on every return to user mode (with the BKL held): handles
/// preemption and pending kills.
pub fn return_to_user_hook(_frame: &mut TrapFrame) {
    loop {
        if current().kill_pending.load(Ordering::Acquire) {
            exit_current();
        }
        if cpu_sched().need_resched {
            schedule();
            continue;
        }
        break;
    }
}

/// Voluntarily gives up the CPU.
pub fn yield_now() {
    let cs = cpu_sched();
    cs.slice_end = 0;
    schedule();
}

/// Terminates the current thread. Never returns.
pub fn exit_current() -> ! {
    let cur = current();
    {
        let mut s = cur.sched.lock();
        s.state = State::Dead;
        if let Some(key) = s.timeout_key.take() {
            TIMEOUTS.lock().remove(&key);
        }
    }
    cur.signals.update(0, abi::signals::TERMINATED);
    if let Some(p) = cur.process.clone() {
        p.thread_exited(&cur);
    }
    drop(cur);
    schedule();
    unreachable!("a dead thread was rescheduled");
}

/// Handles a CPU exception raised by user code (BKL held).
pub fn user_exception(frame: &mut TrapFrame) {
    let cur = current();
    // Programs are placed at random addresses: where the code that failed
    // is in its program's image says what it was.
    let offset = cur.aspace.as_ref().and_then(|a| a.vmo_offset_of(frame.code_address()));
    let in_image = core::fmt::from_fn(move |f| match offset {
        Some(offset) => write!(f, " ({offset:#x} in its image)"),
        None => Ok(()),
    });
    if let Some(fault) = frame.page_fault() {
        if let Some(a) = &cur.aspace
            && a.handle_fault(fault.address, fault.write, fault.execute)
        {
            return;
        }
        crate::kerror!(
            "{} (thread {}): page fault at {:#x} ({} {}), {}{}",
            cur.process_name(),
            cur.koid,
            fault.address,
            if fault.execute {
                "execute"
            } else if fault.write {
                "write"
            } else {
                "read"
            },
            if fault.present { "protection violation" } else { "not mapped" },
            frame.location(),
            in_image
        );
    } else {
        crate::kerror!("{} (thread {}): {}{}", cur.process_name(), cur.koid, frame.describe(), in_image);
    }
    match &cur.process {
        Some(p) => p.kill(abi::EXIT_CODE_CRASHED),
        None => panic!("kernel thread took a user exception"),
    }
    drop(cur);
    exit_current();
}

/// The idle loop of a CPU. Entered with the BKL held.
pub fn idle_loop() -> ! {
    loop {
        schedule();
        // Nothing to run: halt until the next interrupt.
        bkl::release();
        cpu::wait_for_interrupt();
        bkl::acquire();
    }
}

/// Entry point of the idle threads of the CPUs started after the first.
pub fn idle_entry(_arg: u64) -> ! {
    idle_loop()
}

/// Snapshot used by `system_info`.
pub fn total_idle_ns() -> u64 {
    percpu::online().map(|p| p.idle_ns.load(Ordering::Relaxed)).sum()
}
