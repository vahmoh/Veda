// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The scheduler.
//!
//! * 32 priority levels with round-robin within a level; a global run queue
//!   shared by all CPUs (the BKL makes a global queue cheap and fair).
//! * Preemptive: a running thread is preempted when its 10 ms time slice
//!   ends and a thread of equal or higher priority is ready, or immediately
//!   when a higher-priority thread wakes up: on the CPU that woke it, an
//!   idle one, or else the one running the least urgent thread, if that is
//!   below it.
//! * Priorities above `NORMAL` buy latency, not processor time: a process's
//!   threads run above `NORMAL` for at most [`ELEVATED_BUDGET_NS`] of every
//!   [`ELEVATED_WINDOW_NS`] together ([`Budget`]), and at `NORMAL` for the
//!   rest of the window once they have used that, so threads that keep
//!   processors busy (a guest's processors in a loop, however many it has)
//!   cannot starve the rest.
//! * No ready thread waits for ever: one below `NORMAL` that has waited
//!   [`STARVED_NS`] for a processor runs a time slice at `NORMAL`
//!   ([`boost_starved`]). Under a load of ordinary threads it would never
//!   run again otherwise, nor would whoever waits for it (for a lock it
//!   holds, which it lets go of in that slice).
//! * Tickless: each CPU arms its timer (`architecture::timer`) for its next
//!   event (time slice end, the end of the running thread's budget above
//!   `NORMAL`, or the earliest deadline of the threads that went to sleep
//!   on it, which it alone wakes).
//! * Every CPU has an idle thread that halts until an interrupt arrives.
//! * A thread can be interrupted ([`interrupt`]): the waits user code makes
//!   end ([`block_interruptible`]), and as it next returns to user code it
//!   runs its process's signal entry: how POSIX signals reach a thread,
//!   whatever it is doing.

pub mod percpu;
pub mod thread;

use alloc::collections::{BTreeMap, VecDeque};
use alloc::sync::{Arc, Weak};
use core::sync::atomic::{AtomicU64, Ordering};

pub use thread::{State, Thread, WakeReason};

use crate::architecture::trap::TrapFrame;
use crate::architecture::{self, cpu};
use crate::memory::aspace::Fault;
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
/// The soonest a CPU's timer is armed for: an event already due fires as
/// soon as the CPU takes interrupts again. A longer floor would make every
/// thread whose deadline is nearer late, a guest's processor halted until
/// its timer a few microseconds away too.
const TIMER_SOONEST_NS: u64 = 1_000;
/// How long a thread below `NORMAL` waits ready, at most, before it runs a
/// time slice at `NORMAL`.
pub const STARVED_NS: u64 = 1_000_000_000;
/// How often the run queue is looked through for threads that waited that
/// long.
const STARVED_SCAN_NS: u64 = 250_000_000;
/// The processor time a process's threads above `NORMAL` run there in
/// every [`ELEVATED_WINDOW_NS`], together: half a processor's (a little
/// more while several run at once, as each processor charges what its
/// thread used at least once a time slice). What needs
/// a high priority to answer in time (the sound mixer, a guest's
/// processors handling its interrupts) uses a few percent; a window ten
/// slices long lets a burst of work (a frame, a guest's driver starting)
/// finish at its priority.
pub const ELEVATED_BUDGET_NS: u64 = 50_000_000;
pub const ELEVATED_WINDOW_NS: u64 = 100_000_000;

/// What a process's threads above `NORMAL` used of the processor time they
/// may run there: [`ELEVATED_BUDGET_NS`] in each window of
/// [`ELEVATED_WINDOW_NS`].
pub struct Budget {
    used_ns: u64,
    window_start: u64,
}

impl Budget {
    pub const fn new() -> Budget {
        Budget { used_ns: 0, window_start: 0 }
    }

    /// Whether it is used up in the window `now` is in.
    pub fn spent(&self, now: u64) -> bool {
        self.used_ns >= ELEVATED_BUDGET_NS && now < self.window_start + ELEVATED_WINDOW_NS
    }

    /// What is left of it at `now`.
    pub fn left(&self, now: u64) -> u64 {
        if now >= self.window_start + ELEVATED_WINDOW_NS {
            ELEVATED_BUDGET_NS
        } else {
            ELEVATED_BUDGET_NS.saturating_sub(self.used_ns)
        }
    }

    /// Charges `ran` ns of processor time, used until `now`. A window that
    /// has passed is followed by one that begins where it ended or where
    /// this run began, whichever is later, and holds what of the run falls
    /// in it.
    pub fn charge(&mut self, ran: u64, now: u64) {
        let end = self.window_start + ELEVATED_WINDOW_NS;
        if now >= end {
            self.window_start = now.saturating_sub(ran).max(end);
            self.used_ns = now - self.window_start;
        } else {
            self.used_ns += ran;
        }
    }
}

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
    /// Until when the current thread's processor time is charged to its
    /// process's budget above `NORMAL` (`Thread::charge`).
    pub charged_to: u64,
}

impl CpuSched {
    pub const fn new() -> Self {
        CpuSched {
            current: None,
            idle: None,
            prev: None,
            need_resched: false,
            slice_end: 0,
            switched_in_at: 0,
            charged_to: 0,
        }
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
        t.ready_since.store(time::now_ns(), Ordering::Relaxed);
        let p = priority as usize & 31;
        self.levels[p].push_back(t);
        self.bitmap |= 1 << p;
        self.len += 1;
    }

    /// Moves the threads below `NORMAL` that have waited [`STARVED_NS`]
    /// since `now` to the front of `NORMAL`'s, each to run its next time
    /// slice there ([`Thread::boost_until`]).
    fn boost_starved(&mut self, now: u64) {
        let normal = abi::priority::NORMAL;
        for p in 0..normal {
            if self.levels[p].iter().all(|t| now.saturating_sub(t.ready_since.load(Ordering::Relaxed)) < STARVED_NS) {
                continue;
            }
            let (starved, waiting): (VecDeque<_>, VecDeque<_>) = core::mem::take(&mut self.levels[p])
                .into_iter()
                .partition(|t| now.saturating_sub(t.ready_since.load(Ordering::Relaxed)) >= STARVED_NS);
            self.levels[p] = waiting;
            if self.levels[p].is_empty() {
                self.bitmap &= !(1 << p);
            }
            for t in starved {
                t.boost_until.store(u64::MAX, Ordering::Relaxed);
                self.levels[normal].push_front(t);
            }
            self.bitmap |= 1 << normal;
        }
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
type Timeouts = BTreeMap<(u64, u64), Weak<Thread>>;

/// For each CPU, the threads that blocked on it with a deadline: only its
/// timer is armed for their deadlines, and it alone wakes them. With one
/// queue for all, every CPU's timer was armed for the earliest deadline of
/// all, so each deadline interrupted every CPU, and each queued for the BKL
/// to find that the first had woken the thread; one already due made every
/// CPU that armed its timer meanwhile take an interrupt a microsecond
/// later, again and again while the BKL was busy. The more CPUs, the more
/// of each.
static TIMEOUTS: [SpinLock<Timeouts>; percpu::MAX_CPUS] = [const { SpinLock::new(BTreeMap::new()) }; percpu::MAX_CPUS];

/// Takes a blocked thread's deadline, `(cpu, key)` as its
/// [`SchedData::timeout_key`](thread::SchedData) has it, out of that CPU's
/// queue.
fn forget_timeout((cpu, key): (u32, (u64, u64))) {
    TIMEOUTS[cpu as usize].lock().remove(&key);
}

/// The thread running on this CPU.
pub fn current() -> Arc<Thread> {
    cpu_sched().current.clone().expect("scheduler not initialised on this CPU")
}

/// Like [`current`] but usable before the scheduler is up.
pub fn try_current() -> Option<Arc<Thread>> {
    cpu_sched().current.clone()
}

/// The processor time `t` has used: its turns that have ended (a turn ends
/// as [`switch_to`] switches the thread out), and the one it is running,
/// on whichever CPU, so far. Read under the BKL, which every switch holds,
/// it is never less than a reading before it, whichever CPU read that.
pub fn cpu_time_ns(t: &Thread) -> u64 {
    // The CPU that runs it, if any does: `switch_to` names it before it
    // makes the thread its current one.
    let cpu = t.sched.lock().cpu;
    let running = running_for(percpu::get(cpu as usize), time::now_ns(), |cur| cur.koid == t.koid);
    t.cpu_time_ns.load(Ordering::Relaxed) + running
}

/// How long CPU `p` has run its current thread at `now`, if `which` is
/// true of that thread (0 if not).
fn running_for(p: &percpu::PerCpu, now: u64, which: impl FnOnce(&Thread) -> bool) -> u64 {
    debug_assert!(bkl::held_by_me(), "a CPU's scheduler state read without the BKL");
    // SAFETY: a CPU's scheduler state changes only under the BKL, which the
    // caller holds (as `after_wakeup` reads other CPUs' current threads).
    let cs = unsafe { &*p.sched.get() };
    match &cs.current {
        Some(cur) if which(cur) => now.saturating_sub(cs.switched_in_at),
        _ => 0,
    }
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
    {
        let mut s = t.sched.lock();
        debug_assert!(matches!(s.state, State::New | State::Blocked));
        s.state = State::Ready;
    }
    let now = time::now_ns();
    let prio = t.effective_priority(now);
    RUNQ.lock().push(t.clone(), prio);
    after_wakeup(prio, now);
}

/// Preemption and idle-CPU wake-up after a thread of priority `prio`
/// became runnable: this CPU runs it next if its own thread is less
/// urgent. Otherwise an idle CPU picks it up, or, if none is idle, the CPU
/// running the least urgent thread does, if that is below it: else the
/// thread would wait for a time slice to end, though it outranks a running
/// one.
fn after_wakeup(prio: u8, now: u64) {
    let me = percpu::cpu_id_or_boot();
    let idle = try_current().is_some_and(|cur| cur.is_idle);
    // An idle CPU runs the first thread it makes ready itself, as it leaves
    // the interrupt that did (a timer's, a device's): another woken for it
    // would only race it for the thread, and queue for the BKL to lose.
    if idle && !cpu_sched().need_resched {
        cpu_sched().need_resched = true;
        return;
    }
    let here = idle || try_current().is_some_and(|cur| prio > cur.effective_priority(now));
    if here {
        cpu_sched().need_resched = true;
    }
    // Wake one idle CPU (other than us) to pick up the work: one that has
    // not been woken already, so that threads made ready together (a thread
    // pool's workers, say) start on different CPUs at once. (Here, it takes
    // the thread this CPU puts back.)
    let mut least: Option<(u8, &percpu::PerCpu)> = None;
    for p in percpu::online() {
        if p.cpu_id == me {
            continue;
        }
        // SAFETY: reading another CPU's current thread pointer under the BKL;
        // it only changes while that CPU holds the BKL.
        let Some(cur) = (unsafe { (*p.sched.get()).current.as_ref() }) else { continue };
        if cur.is_idle {
            if !p.resched_pending.swap(true, Ordering::AcqRel) {
                architecture::smp::reschedule(p.cpu_id);
                return;
            }
            continue;
        }
        let theirs = cur.effective_priority(now);
        if theirs < prio && least.is_none_or(|(l, _)| theirs < l) {
            least = Some((theirs, p));
        }
    }
    if !here
        && let Some((_, p)) = least
        && !p.resched_pending.swap(true, Ordering::AcqRel)
    {
        architecture::smp::reschedule(p.cpu_id);
    }
}

/// Wakes a blocked thread. No effect on threads in other states.
pub fn wake(t: &Arc<Thread>, reason: WakeReason) {
    {
        let mut s = t.sched.lock();
        if s.state != State::Blocked {
            return;
        }
        s.state = State::Ready;
        s.wake_reason = reason;
        if let Some(key) = s.timeout_key.take() {
            forget_timeout(key);
        }
    }
    let now = time::now_ns();
    let prio = t.effective_priority(now);
    RUNQ.lock().push(t.clone(), prio);
    after_wakeup(prio, now);
}

/// How an interruption ([`interrupt`]) bears on a wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interruptible {
    /// It does not end it: the kernel's own waits.
    No,
    /// It ends it (`Interrupted`), at once if one came before: the waits
    /// user code makes, when its thread can act on one
    /// ([`takes_interruptions`]).
    Yes,
    /// So does one the thread deferred and has not acted on yet
    /// ([`Thread::deferred`]): `wait_flags::DEFERRED`.
    OrDeferred,
}

impl Interruptible {
    /// What a wait call's flags make of its wait (`abi::wait_flags`; the
    /// call's own, `own`, it takes itself): `InvalidArgs` for flags there
    /// are not.
    pub fn from_flags(flags: usize, own: usize) -> Result<Interruptible, abi::Error> {
        if flags & !(abi::wait_flags::DEFERRED | own) != 0 {
            return Err(abi::Error::InvalidArgs);
        }
        Ok(if flags & abi::wait_flags::DEFERRED != 0 { Interruptible::OrDeferred } else { Interruptible::Yes })
    }
}

/// Blocks the current thread until [`wake`] is called or `deadline`
/// (monotonic ns) passes.
pub fn block(deadline: Option<u64>) -> WakeReason {
    block_as(deadline, Interruptible::No)
}

/// [`block`], which an interruption ends as `how` says.
pub fn block_interruptible(deadline: Option<u64>, how: Interruptible) -> WakeReason {
    block_as(deadline, how)
}

/// Whether `t` acts on an interruption: its process has a signal entry,
/// and it has a thread pointer, by which the entry finds the thread's
/// state. Another thread's waits are not ended by one, which would end
/// them again each time they began.
pub fn takes_interruptions(t: &Thread) -> bool {
    // SAFETY: the context's thread pointer, read under the BKL.
    t.process.as_ref().is_some_and(|p| p.signal_entry().is_some()) && unsafe { (*t.ctx_ptr()).tls() } != 0
}

fn block_as(deadline: Option<u64>, how: Interruptible) -> WakeReason {
    let cur = current();
    let interruptible = how != Interruptible::No && takes_interruptions(&cur);
    if cur.kill_pending.load(Ordering::Acquire) {
        return WakeReason::Killed;
    }
    if interruptible
        && (cur.interrupt.load(Ordering::Acquire)
            || (how == Interruptible::OrDeferred && cur.deferred.load(Ordering::Acquire)))
    {
        return WakeReason::Interrupted;
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
        s.interruptible = interruptible;
        if let Some(d) = deadline {
            let (cpu, key) = (percpu::cpu_id_or_boot(), (d, cur.koid));
            TIMEOUTS[cpu as usize].lock().insert(key, Arc::downgrade(&cur));
            s.timeout_key = Some((cpu, key));
        }
    }
    schedule();
    let mut s = cur.sched.lock();
    if let Some(key) = s.timeout_key.take() {
        forget_timeout(key);
    }
    s.wake_reason
}

/// Sleeps until `deadline` (monotonic ns), or an interruption as `how`
/// says.
pub fn sleep_until(deadline: u64, how: Interruptible) -> WakeReason {
    loop {
        match block_as(Some(deadline), how) {
            reason @ (WakeReason::TimedOut | WakeReason::Killed | WakeReason::Interrupted) => return reason,
            _ if time::now_ns() >= deadline => return WakeReason::TimedOut,
            _ => {}
        }
    }
}

/// Interrupts `t` (`THREAD_INTERRUPT`, a signal for its process): an
/// interruptible wait it is in ends, as one it begins will, and as it next
/// returns to user code it runs its process's signal entry
/// ([`return_to_user_hook`]). One running on another processor is made to
/// enter the kernel; one running here is the caller, which returns to user
/// code next.
pub fn interrupt(t: &Arc<Thread>) {
    t.interrupt.store(true, Ordering::Release);
    let (state, interruptible, cpu) = {
        let s = t.sched.lock();
        (s.state, s.interruptible, s.cpu)
    };
    match state {
        State::Blocked if interruptible => wake(t, WakeReason::Interrupted),
        State::Running if cpu != percpu::cpu_id_or_boot() => architecture::smp::reschedule(cpu),
        _ => {}
    }
}

/// Wakes every thread whose deadline has passed, of those that blocked on
/// this CPU.
fn expire_timeouts(now: u64) {
    let queue = &TIMEOUTS[percpu::cpu_id_or_boot() as usize];
    loop {
        let t = {
            let mut q = queue.lock();
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

/// The earliest deadline of the threads that blocked on this CPU.
fn earliest_timeout() -> Option<u64> {
    TIMEOUTS[percpu::cpu_id_or_boot() as usize].lock().first_key_value().map(|(&(d, _), _)| d)
}

/// Arms this CPU's timer for its next event, `running` being the thread it
/// runs from now on.
fn arm_timer(running: &Thread) {
    let now = time::now_ns();
    let cs = cpu_sched();
    let mut deadline = if running.is_idle { now + IDLE_MAX_NS } else { cs.slice_end };
    if !running.is_idle
        && let Some(end) = running.budget_end(cs.charged_to, now)
    {
        deadline = deadline.min(end);
    }
    if let Some(t) = earliest_timeout() {
        deadline = deadline.min(t);
    }
    if percpu::cpu_id_or_boot() == 0 && crate::diagnostics::log::serial_pending() {
        deadline = deadline.min(now + SERIAL_NS);
    }
    let deadline = deadline.max(now + TIMER_SOONEST_NS);
    architecture::timer::arm(deadline, now);
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
    let now = time::now_ns();
    let next = {
        let mut rq = RUNQ.lock();
        if cur_state == State::Running && !cur.is_idle {
            let prio = cur.effective_priority(now);
            match rq.highest() {
                Some(p) if p >= prio => {
                    let next = rq.pop().unwrap();
                    cur.sched.lock().state = State::Ready;
                    rq.push(cur.clone(), prio);
                    next
                }
                _ => {
                    drop(rq);
                    // Keep running; start a fresh slice if this one ended.
                    if now >= cs.slice_end {
                        cs.slice_end = now + TIME_SLICE_NS;
                    }
                    arm_timer(&cur);
                    return;
                }
            }
        } else {
            match rq.pop() {
                Some(t) => t,
                None if cur.is_idle => {
                    drop(rq);
                    arm_timer(&cur);
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
    // A thread that has ended has left its process's threads
    // (`Process::thread_exited`): the process keeps the time it used, this
    // last turn too.
    if let Some(p) = &prev.process
        && prev.is_dead()
    {
        p.keep_cpu_time(prev.cpu_time_ns.load(Ordering::Relaxed));
    }
    cs.switched_in_at = now;
    prev.charge(now.saturating_sub(cs.charged_to), now);
    cs.charged_to = now;

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
    // A slice it was given, after it waited too long, begins.
    if next.boost_until.load(Ordering::Relaxed) == u64::MAX {
        next.boost_until.store(cs.slice_end, Ordering::Relaxed);
    }
    arm_timer(&next);

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
    unsafe { architecture::thread::switch(prev_ctx, next_ctx, next_stack_top) };
    finish_switch();
}

/// Runs on the incoming thread right after a switch: releases the outgoing
/// thread (possibly freeing it if it died).
fn finish_switch() {
    let prev = cpu_sched().prev.take();
    drop(prev);
}

/// First Rust code run by a new user thread: a thread suspended before it
/// started stops before its first instruction.
pub fn thread_start_hook() {
    finish_switch();
    let cur = current();
    park_while_suspended(&cur);
    if cur.kill_pending.load(Ordering::Acquire) {
        drop(cur);
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

/// Gives the threads below `NORMAL` that have waited [`STARVED_NS`] for a
/// processor a time slice at `NORMAL`, looking every [`STARVED_SCAN_NS`]
/// (on whichever processor's timer comes first).
fn boost_starved(now: u64) {
    static SCANNED: AtomicU64 = AtomicU64::new(0);
    let last = SCANNED.load(Ordering::Relaxed);
    if now < last + STARVED_SCAN_NS
        || SCANNED.compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed).is_err()
    {
        return;
    }
    RUNQ.lock().boost_starved(now);
}

/// The CPU's timer interrupt.
pub fn timer_interrupt() {
    percpu::current().timer_interrupts.fetch_add(1, Ordering::Relaxed);
    let now = time::now_ns();
    expire_timeouts(now);
    boost_starved(now);
    if percpu::cpu_id_or_boot() == 0 {
        crate::diagnostics::log::pump_serial();
    }
    let cs = cpu_sched();
    let Some(cur) = cs.current.clone() else { return };
    if !cur.is_idle {
        // A thread whose process's budget above NORMAL ran out competes at
        // NORMAL from now on.
        let before = cur.effective_priority(now);
        cur.charge(now.saturating_sub(cs.charged_to), now);
        cs.charged_to = now;
        if cur.effective_priority(now) < before {
            cs.need_resched = true;
        }
    }
    if !cur.is_idle && now >= cs.slice_end {
        cs.need_resched = true;
    }
    if !cs.need_resched {
        arm_timer(&cur);
    }
}

/// Called on every return to user mode (with the BKL held): handles
/// preemption, pending kills and suspensions, then an interruption: the
/// thread runs its process's signal entry, once it has a thread pointer
/// (the signal entry finds its thread's state by it).
pub fn return_to_user_hook(frame: &mut TrapFrame) {
    loop {
        if current().kill_pending.load(Ordering::Acquire) {
            exit_current();
        }
        if cpu_sched().need_resched {
            schedule();
            continue;
        }
        let cur = current();
        if cur.suspensions.load(Ordering::Acquire) > 0 {
            park_while_suspended(&cur);
            continue;
        }
        break;
    }
    let cur = current();
    if !cur.interrupt.load(Ordering::Acquire) || !takes_interruptions(&cur) {
        return;
    }
    let Some(entry) = cur.process.as_ref().and_then(|p| p.signal_entry()) else { return };
    // SAFETY: the current thread's context, under the BKL.
    let tls = unsafe { (*cur.ctx_ptr()).tls() };
    cur.interrupt.store(false, Ordering::Release);
    // A stack it cannot be given its registers on (one being freed as the
    // thread ends) goes without: the signals wait for another thread.
    if frame.divert(entry, tls).is_err()
        && let Some(p) = &cur.process
    {
        p.interrupt_receiver(Some(cur.koid));
    }
}

/// Called as the current thread enters the kernel from user code (with
/// the BKL held): a thread being suspended is stopped from now on, its
/// user registers saved, which the thread that suspends it waits for.
pub fn entered_from_user() {
    let cur = current();
    if cur.suspensions.load(Ordering::Acquire) > 0 {
        cur.signals.update(0, abi::signals::SUSPENDED);
    }
}

/// Keeps the current thread in the kernel while suspensions are in force
/// on it, or until it is killed.
fn park_while_suspended(cur: &Arc<Thread>) {
    if cur.suspensions.load(Ordering::Acquire) == 0 {
        return;
    }
    cur.signals.update(0, abi::signals::SUSPENDED);
    cur.parked.store(true, Ordering::Release);
    while cur.suspensions.load(Ordering::Acquire) > 0 && !cur.kill_pending.load(Ordering::Acquire) {
        block(None);
    }
    cur.parked.store(false, Ordering::Release);
}

/// Stops `t`, another thread (`THREAD_SUSPEND`): it runs no user code until
/// [`resume`] has undone this. A thread that is not running is stopped at
/// once (its user registers were saved as it entered the kernel, or it has
/// not started); one running on another processor is interrupted, and the
/// caller waits until it has entered the kernel, or ended.
pub fn suspend(t: &Arc<Thread>) -> Result<(), abi::Error> {
    t.suspensions
        .try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_add(1))
        .map_err(|_| abi::Error::OutOfRange)?;
    let (state, cpu) = {
        let s = t.sched.lock();
        (s.state, s.cpu)
    };
    if state != State::Running {
        t.signals.update(0, abi::signals::SUSPENDED);
        return Ok(());
    }
    architecture::smp::reschedule(cpu);
    Ok(())
}

/// Undoes one suspension of `t` ([`suspend`]); it runs again once none is
/// left. `BadState` if none is in force.
pub fn resume(t: &Arc<Thread>) -> Result<(), abi::Error> {
    let before = t
        .suspensions
        .try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1))
        .map_err(|_| abi::Error::BadState)?;
    if before == 1 {
        t.signals.update(abi::signals::SUSPENDED, 0);
        if t.parked.load(Ordering::Acquire) {
            wake(t, WakeReason::Signaled);
        }
    }
    Ok(())
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
            forget_timeout(key);
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
    let mut code = abi::EXIT_CODE_CRASHED;
    if let Some(fault) = frame.page_fault() {
        let outcome =
            cur.aspace.as_ref().map_or(Fault::Invalid, |a| a.handle_fault(fault.address, fault.write, fault.execute));
        if outcome == Fault::Resolved {
            return;
        }
        if outcome == Fault::OutOfMemory {
            // A valid access the system has no page for: a more expendable
            // process is ended to make room, and the access is made again.
            if crate::memory::oom::make_room(&cur) {
                return;
            }
            if cur.process.as_ref().is_some_and(|p| p.ending_with().is_some()) {
                // Ended meanwhile (to make room for another, say): it goes
                // as it was told to.
                drop(cur);
                exit_current();
            }
            // This process is the one to go: say so, and what holds the
            // memory, rather than blame the program.
            code = abi::EXIT_CODE_OUT_OF_MEMORY;
            let (total, free) = crate::memory::phys::stats();
            let largest = crate::object::process::largest(4);
            let holders = core::fmt::from_fn(|f| {
                for (i, (p, bytes)) in largest.iter().enumerate() {
                    let sep = if i == 0 { "" } else { ", " };
                    write!(f, "{sep}{} (process {}) {} MiB", p.name.as_str(), p.koid, bytes >> 20)?;
                }
                Ok(())
            });
            crate::kerror!(
                "{} (thread {}): out of memory: no page for its access at {:#x}, {}{}; {} MiB of {} MiB free; \
                 the most memory: {}",
                cur.process_name(),
                cur.koid,
                fault.address,
                frame.location(),
                in_image,
                free >> 20,
                total >> 20,
                holders
            );
        } else {
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
        }
    } else {
        crate::kerror!("{} (thread {}): {}{}", cur.process_name(), cur.koid, frame.describe(), in_image);
    }
    match &cur.process {
        Some(p) => p.kill(code),
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

/// Snapshot used by `system_info`: the CPUs' idle time, the stretch an
/// idle one is in so far too (under the BKL, as [`cpu_time_ns`]).
pub fn total_idle_ns() -> u64 {
    let now = time::now_ns();
    percpu::online().map(|p| p.idle_ns.load(Ordering::Relaxed) + running_for(p, now, |cur| cur.is_idle)).sum()
}

/// The timer interrupts all CPUs have taken, for `system_info`.
pub fn total_timer_interrupts() -> u64 {
    percpu::online().map(|p| p.timer_interrupts.load(Ordering::Relaxed)).sum()
}
