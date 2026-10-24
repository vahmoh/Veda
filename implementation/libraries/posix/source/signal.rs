// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Signals, and the threads they are for.
//!
//! Veda delivers no asynchronous signals: other processes stop a program
//! by killing it (which is what `SIGKILL`, and `SIGINT` from the terminal,
//! do anyway). What remains is what a program does to itself — `raise`,
//! `abort`, `SIGPIPE` from writing to a closed pipe — and what its threads
//! send each other (`pthread_kill`, `tgkill`), and that is handled here
//! faithfully: dispositions (per process), the signal mask and pending
//! signals (per thread), and handlers called synchronously by the thread a
//! signal is for, once it does not block it: at once for a signal a thread
//! raises itself; for one from another thread, as the next system call the
//! thread makes returns. A signal the program sends itself (`kill` of its
//! own process) is the calling thread's if it does not block it, else
//! another's that does not; if every thread blocks it, it stays pending for
//! the process, for the first thread to unblock it or wait for it. Such a signal ends the thread's wait if it waits
//! for signals (`sigsuspend`, `sigwait`, `pause`, a `poll` of nothing) or on
//! a futex (a lock, a condition, a join: the wait ends with `EINTR`, which
//! is how the C library's cancellation and `__synccall` reach a thread that
//! waits); other blocking calls (a read from a pipe) carry on.
//!
//! A thread is known by its thread pointer
//! (`architecture::thread_pointer`), unique while the thread lives, and has
//! a thread id, unique among the process's live threads: the main thread's
//! is the process id, as on Linux, and the others' are handed out as they
//! start ([`thread_created`]) and free again once they end
//! ([`thread_exited`]).

use alloc::collections::BTreeMap;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use rt::sync::{Condvar, Mutex};

use crate::error::SysResult;
use crate::linux::errno::{EAGAIN, EINTR, EINVAL, ESRCH};
use crate::linux::{Sigaction, Siginfo, Timespec, sig, ucontext};
use crate::{process, time, user};

/// Thread ids are below this: the C library keeps flags in the high bits of
/// the words that hold them (a mutex's owner).
const TID_LIMIT: i32 = 1 << 30;

/// A thread's id, name and signal state.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
struct ThreadState {
    /// 0 for a thread this layer has not met (the main thread before the
    /// C library set up thread-local storage).
    tid: i32,
    /// The name it gave itself (`PR_SET_NAME`), NUL-padded, or one it was
    /// given by the thread that created it: none (all zeros) is the
    /// process's.
    name: [u8; THREAD_NAME_SIZE],
    mask: u64,
    pending: u64,
    /// The address of the futex it waits on (0: none), which a signal for
    /// it wakes.
    futex: usize,
}

impl ThreadState {
    /// The pending signals it does not block.
    fn ready(&self) -> u64 {
        self.pending & !self.mask
    }
}

struct State {
    actions: [Sigaction; sig::NSIG as usize],
    /// Threads by thread pointer.
    threads: BTreeMap<usize, ThreadState>,
    /// Thread pointers by thread id.
    tids: BTreeMap<i32, usize>,
    /// Where the search for a free thread id starts.
    next_tid: i32,
    /// The number of threads with a pending signal they do not block (kept
    /// apart from the lock: [`deliver_if_any`] reads it without taking it).
    ready_threads: &'static AtomicUsize,
    /// Signals the process sent itself while every thread blocked them.
    pending: u64,
}

static READY_THREADS: AtomicUsize = AtomicUsize::new(0);

static STATE: Mutex<State> = Mutex::new(State {
    actions: [Sigaction { handler: sig::DFL, flags: 0, restorer: 0, mask: 0 }; sig::NSIG as usize],
    threads: BTreeMap::new(),
    tids: BTreeMap::new(),
    next_tid: 1,
    ready_threads: &READY_THREADS,
    pending: 0,
});

/// Notified when a signal becomes pending for a thread other than the one
/// sending it: wakes the threads that wait for signals.
static ARRIVED: Condvar = Condvar::new();

/// Thread pointers exist (the C library has set up thread-local storage).
static THREADS_READY: AtomicBool = AtomicBool::new(false);

impl State {
    /// Changes thread `tp`'s state (a fresh one if it has none), keeping the
    /// count of threads with signals ready right.
    fn update<R>(&mut self, tp: usize, f: impl FnOnce(&mut ThreadState) -> R) -> R {
        let t = self.threads.entry(tp).or_default();
        let was = t.ready() != 0;
        let r = f(t);
        let is = t.ready() != 0;
        if was != is {
            if is {
                self.ready_threads.fetch_add(1, Ordering::Relaxed);
            } else {
                self.ready_threads.fetch_sub(1, Ordering::Relaxed);
            }
        }
        r
    }

    /// Thread `tp`'s state, without making one.
    fn get(&self, tp: usize) -> ThreadState {
        self.threads.get(&tp).copied().unwrap_or_default()
    }

    /// Forgets thread `tp`: its state and its id.
    fn forget(&mut self, tp: usize) {
        if let Some(t) = self.threads.remove(&tp) {
            if t.ready() != 0 {
                self.ready_threads.fetch_sub(1, Ordering::Relaxed);
            }
            if self.tids.get(&t.tid) == Some(&tp) {
                self.tids.remove(&t.tid);
            }
        }
    }

    /// A thread id that neither a live thread nor the process (`pid`) has:
    /// the first from where the last search ended (as Linux numbers them,
    /// after the process's), wrapping below [`TID_LIMIT`].
    fn free_tid(&mut self, pid: i32) -> Option<i32> {
        // Among this many consecutive ids, one is free.
        for _ in 0..self.tids.len() + 2 {
            let tid = self.next_tid;
            self.next_tid = if tid + 1 >= TID_LIMIT { 1 } else { tid + 1 };
            if tid != pid && !self.tids.contains_key(&tid) {
                return Some(tid);
            }
        }
        None
    }

    /// Gives thread `tp` the signals pending for the process that it does
    /// not block, or that it waits for (`want`).
    fn claim(&mut self, tp: usize, want: u64) {
        let claimed = self.pending & (!self.get(tp).mask | want);
        if claimed != 0 {
            self.pending &= !claimed;
            self.update(tp, |t| t.pending |= claimed);
        }
    }

    /// The thread, but `me`, that does not block signal `s`, if any.
    fn unblocked_but(&self, me: usize, s: u32) -> Option<usize> {
        self.threads.iter().find(|&(&tp, t)| tp != me && t.mask & bit(s) == 0).map(|(&tp, _)| tp)
    }

    /// Takes the lowest pending signal of `set` from thread `tp`.
    fn take(&mut self, tp: usize, set: u64) -> Option<u32> {
        self.update(tp, |t| {
            let found = t.pending & set;
            (found != 0).then(|| {
                let s = found.trailing_zeros() + 1;
                t.pending &= !bit(s);
                s
            })
        })
    }
}

/// The calling thread's thread pointer (0 before there are any).
fn current() -> usize {
    if !THREADS_READY.load(Ordering::Acquire) {
        return 0;
    }
    crate::architecture::thread_pointer()
}

/// From now on threads have thread pointers: the calling thread, the main
/// one, gets the process id for its thread id.
pub fn threads_ready() {
    THREADS_READY.store(true, Ordering::Release);
    let (tp, pid) = (current(), process::pid());
    let mut st = STATE.lock();
    // What was recorded before thread pointers existed is the main thread's.
    let early = st.threads.get(&0).copied().unwrap_or_default();
    st.forget(0);
    st.update(tp, |t| *t = ThreadState { tid: pid, ..early });
    st.tids.insert(pid, tp);
    st.next_tid = if pid + 1 >= TID_LIMIT { 1 } else { pid + 1 };
}

/// A thread with thread pointer `tp` is about to start: its id, and a fresh
/// state with the creating thread's mask and name (as a thread inherits
/// them on Linux). `EAGAIN` if no id is free.
pub fn thread_created(tp: usize) -> Result<i32, isize> {
    let me = current();
    let pid = process::pid();
    let mut st = STATE.lock();
    // A thread that had this pointer has ended.
    st.forget(tp);
    let tid = st.free_tid(pid).ok_or(EAGAIN)?;
    let ThreadState { mask, name, .. } = st.get(me);
    st.update(tp, |t| *t = ThreadState { tid, mask, name, ..Default::default() });
    st.tids.insert(tid, tp);
    Ok(tid)
}

/// The thread [`thread_created`] made ready for `tp` did not start.
pub fn thread_not_started(tp: usize) {
    STATE.lock().forget(tp);
}

/// The calling thread is ending: its id is free again, its state gone.
pub fn thread_exited() {
    let tp = current();
    STATE.lock().forget(tp);
}

/// The calling thread's id.
pub fn current_tid() -> i32 {
    let tid = STATE.lock().get(current()).tid;
    if tid != 0 { tid } else { process::pid() }
}

/// How many bytes a thread's name takes, its NUL with them (Linux's
/// `TASK_COMM_LEN`).
pub const THREAD_NAME_SIZE: usize = 16;

/// The calling thread's name, NUL-padded, if it or the thread that created
/// it gave it one (`PR_SET_NAME`).
pub fn thread_name() -> Option<[u8; THREAD_NAME_SIZE]> {
    let name = STATE.lock().get(current()).name;
    (name != [0; THREAD_NAME_SIZE]).then_some(name)
}

/// Names the calling thread `name` (cut to fit, as Linux cuts it, with its
/// NUL).
pub fn set_thread_name(name: &[u8]) {
    let mut padded = [0u8; THREAD_NAME_SIZE];
    let n = name.iter().position(|&b| b == 0).unwrap_or(name.len()).min(THREAD_NAME_SIZE - 1);
    padded[..n].copy_from_slice(&name[..n]);
    let tp = current();
    STATE.lock().update(tp, |t| t.name = padded);
}

fn bit(s: u32) -> u64 {
    1 << (s - 1)
}

/// Signals that can be neither caught, ignored nor blocked.
const UNCATCHABLE: u64 = (1 << (sig::KILL - 1)) | (1 << (sig::STOP - 1));

fn valid(s: u32) -> bool {
    (1..sig::NSIG).contains(&s)
}

/// What a signal does when nothing handles it: true if it ends the process.
pub(crate) fn terminates(s: u32) -> bool {
    !matches!(s, sig::CHLD | sig::URG | sig::WINCH | sig::CONT | sig::STOP | sig::TSTP | sig::TTIN | sig::TTOU)
}

pub unsafe fn rt_sigaction(s: u32, act: usize, old: usize, size: usize) -> SysResult {
    if !valid(s) || size != 8 {
        return Err(EINVAL);
    }
    let new: Option<Sigaction> = if act != 0 {
        // SAFETY: the program passed a sigaction.
        Some(unsafe { user::read(act)? })
    } else {
        None
    };
    if new.is_some() && bit(s) & UNCATCHABLE != 0 {
        return Err(EINVAL);
    }
    let prev = {
        let mut st = STATE.lock();
        let prev = st.actions[s as usize];
        if let Some(a) = new {
            st.actions[s as usize] = a;
            // Ignoring a pending signal discards it.
            if a.handler == sig::IGN {
                let threads: alloc::vec::Vec<usize> = st.threads.keys().copied().collect();
                for tp in threads {
                    st.update(tp, |t| t.pending &= !bit(s));
                }
            }
        }
        prev
    };
    if old != 0 {
        // SAFETY: as above.
        unsafe { user::write(old, prev)? };
    }
    Ok(0)
}

pub unsafe fn rt_sigprocmask(how: u32, set: usize, old: usize, size: usize) -> SysResult {
    if size != 8 {
        return Err(EINVAL);
    }
    let new = if set != 0 {
        // SAFETY: the program passed a sigset.
        Some(unsafe { user::read::<u64>(set)? })
    } else {
        None
    };
    let tp = current();
    let prev = {
        let mut st = STATE.lock();
        let prev = st.update(tp, |t| {
            let prev = t.mask;
            if let Some(v) = new {
                t.mask = match how {
                    sig::BLOCK => prev | v,
                    sig::UNBLOCK => prev & !v,
                    sig::SETMASK => v,
                    _ => return Err(EINVAL),
                } & !UNCATCHABLE;
            }
            Ok(prev)
        })?;
        st.claim(tp, 0);
        prev
    };
    if old != 0 {
        // SAFETY: as above.
        unsafe { user::write(old, prev)? };
    }
    deliver_pending();
    Ok(0)
}

pub unsafe fn rt_sigpending(set: usize) -> SysResult {
    let pending = {
        let st = STATE.lock();
        st.get(current()).pending | st.pending
    };
    // SAFETY: the program passed a sigset.
    unsafe { user::write(set, pending)? };
    Ok(0)
}

/// Sends signal `s` to the calling thread.
pub fn raise(s: u32) {
    if !valid(s) {
        return;
    }
    let tp = current();
    {
        let mut st = STATE.lock();
        if st.actions[s as usize].handler == sig::IGN && bit(s) & UNCATCHABLE == 0 {
            return;
        }
        st.update(tp, |t| t.pending |= bit(s));
    }
    deliver_pending();
}

/// Acts on the calling thread's pending signals that are not blocked if
/// any thread has some: what a system call does as it returns. Costs one
/// atomic load while no thread has.
pub fn deliver_if_any() {
    if READY_THREADS.load(Ordering::Relaxed) != 0 {
        deliver_pending();
    }
}

/// Acts on the calling thread's pending signals that are not blocked.
/// Returns how many handlers ran.
pub fn deliver_pending() -> usize {
    let tp = current();
    let mut handled = 0;
    loop {
        let (s, action) = {
            let mut st = STATE.lock();
            let ready = st.get(tp).ready();
            if ready == 0 {
                return handled;
            }
            let s = ready.trailing_zeros() + 1;
            st.update(tp, |t| t.pending &= !bit(s));
            let action = st.actions[s as usize];
            match action.handler {
                sig::IGN => continue,
                sig::DFL => (s, None),
                _ => {
                    if action.flags & sig::SA_RESETHAND != 0 {
                        st.actions[s as usize].handler = sig::DFL;
                    }
                    (s, Some(action))
                }
            }
        };
        match action {
            None => {
                if terminates(s) {
                    process::exit_by_signal(s);
                }
            }
            Some(a) => {
                call_handler(tp, s, &a);
                handled += 1;
            }
        }
    }
}

/// A signal handler's context (`ucontext_t`): nothing interrupted the
/// thread, so all of it is zero but the mask to restore when the handler
/// returns, which the handler may change (as the C library's thread
/// cancellation does).
#[repr(C, align(16))]
struct Context([u64; ucontext::SIZE.div_ceil(8)]);

impl Context {
    const MASK: usize = ucontext::SIGMASK / 8;

    fn new(mask: u64) -> Context {
        let mut c = Context([0; ucontext::SIZE.div_ceil(8)]);
        c.0[Self::MASK] = mask;
        c
    }

    fn mask(&self) -> u64 {
        self.0[Self::MASK]
    }
}

/// Runs a handler on this thread, with the signal (and the handler's mask)
/// blocked while it runs; the mask its context holds afterwards is the
/// thread's from then on.
fn call_handler(tp: usize, s: u32, a: &Sigaction) {
    let saved = {
        let mut st = STATE.lock();
        st.update(tp, |t| {
            let saved = t.mask;
            let mut block = a.mask;
            if a.flags & sig::SA_NODEFER == 0 {
                block |= bit(s);
            }
            t.mask |= block & !UNCATCHABLE;
            saved
        })
    };
    let restored = if a.flags & sig::SA_SIGINFO != 0 {
        let mut info = Siginfo { signo: s as i32, pid: process::pid(), uid: crate::vfs::UID, ..Default::default() };
        let mut context = Context::new(saved);
        // SAFETY: the program installed this three-argument handler.
        let f: extern "C" fn(i32, *mut Siginfo, *mut Context) = unsafe { core::mem::transmute(a.handler) };
        f(s as i32, &mut info, &mut context);
        context.mask()
    } else {
        // SAFETY: the program installed this handler.
        let f: extern "C" fn(i32) = unsafe { core::mem::transmute(a.handler) };
        f(s as i32);
        saved
    };
    STATE.lock().update(tp, |t| t.mask = restored & !UNCATCHABLE);
}

/// Waits until the calling thread has run a signal handler, or `deadline`
/// passes; returns whether it ran one.
fn wait_for_handler(deadline: u64) -> bool {
    let tp = current();
    loop {
        {
            let mut st = STATE.lock();
            st.claim(tp, 0);
            while st.get(tp).ready() == 0 {
                let (guard, in_time) = ARRIVED.wait_until(st, deadline);
                st = guard;
                if !in_time && st.get(tp).ready() == 0 {
                    return false;
                }
            }
        }
        // Signals whose default does nothing do not end the wait.
        if deliver_pending() > 0 {
            return true;
        }
    }
}

/// `pause`: waits for a signal handler to run; then `EINTR`.
pub fn pause() -> SysResult {
    wait_for_handler(abi::DEADLINE_INFINITE);
    Err(EINTR)
}

/// Sleeps until `deadline` (monotonic ns) unless a signal handler runs
/// first; returns whether one did.
pub fn sleep_until(deadline: u64) -> bool {
    wait_for_handler(deadline)
}

pub unsafe fn rt_sigsuspend(mask: usize) -> SysResult {
    // SAFETY: the program passed a sigset.
    let m: u64 = unsafe { user::read(mask)? };
    let tp = current();
    let saved = STATE.lock().update(tp, |t| core::mem::replace(&mut t.mask, m & !UNCATCHABLE));
    wait_for_handler(abi::DEADLINE_INFINITE);
    STATE.lock().update(tp, |t| t.mask = saved);
    Err(EINTR)
}

/// `sigtimedwait`: takes a pending signal of `set`, waiting until `timeout`
/// (forever if there is none) for one.
pub unsafe fn rt_sigtimedwait(set: usize, info: usize, timeout: usize) -> SysResult {
    // SAFETY: the program passed a sigset.
    let want: u64 = unsafe { user::read(set)? };
    let deadline = if timeout == 0 {
        abi::DEADLINE_INFINITE
    } else {
        // SAFETY: the program passed a timespec.
        let t: Timespec = unsafe { user::read(timeout)? };
        time::monotonic_ns().saturating_add(t.to_ns().ok_or(EINVAL)?)
    };
    let tp = current();
    let taken = {
        let mut st = STATE.lock();
        loop {
            st.claim(tp, want);
            if let Some(s) = st.take(tp, want) {
                break s;
            }
            let (guard, in_time) = ARRIVED.wait_until(st, deadline);
            st = guard;
            if !in_time {
                st.claim(tp, want);
                match st.take(tp, want) {
                    Some(s) => break s,
                    None => return Err(EAGAIN),
                }
            }
        }
    };
    if info != 0 {
        // SAFETY: the program passed a siginfo.
        unsafe { user::write(info, Siginfo { signo: taken as i32, pid: process::pid(), ..Default::default() })? };
    }
    Ok(taken as usize)
}

/// `kill` aimed at this process: the calling thread takes it.
pub fn send_self(s: u32) -> SysResult {
    if s == 0 {
        return Ok(0);
    }
    if !valid(s) {
        return Err(EINVAL);
    }
    let me = current();
    let mut st = STATE.lock();
    if st.get(me).mask & bit(s) == 0 {
        drop(st);
        raise(s);
        return Ok(0);
    }
    if st.actions[s as usize].handler == sig::IGN {
        return Ok(0);
    }
    match st.unblocked_but(me, s) {
        Some(tp) => {
            st.update(tp, |t| t.pending |= bit(s));
            drop(st);
            ARRIVED.notify_all();
            wake_from_futex(tp, s);
        }
        // Every thread blocks it: the process keeps it pending.
        None => {
            st.pending |= bit(s);
            drop(st);
            ARRIVED.notify_all();
        }
    }
    Ok(0)
}

/// `tkill` and `tgkill`: signal `s` (0: none, only whether the thread
/// exists) for thread `tid` of this process, which takes it as the module's
/// documentation says.
pub fn send_to_thread(tid: i32, s: u32) -> SysResult {
    if s != 0 && !valid(s) {
        return Err(EINVAL);
    }
    let me = current();
    let mut st = STATE.lock();
    let tp = *st.tids.get(&tid).ok_or(ESRCH)?;
    if s == 0 {
        return Ok(0);
    }
    if tp == me {
        drop(st);
        raise(s);
        return Ok(0);
    }
    if st.actions[s as usize].handler == sig::IGN && bit(s) & UNCATCHABLE == 0 {
        return Ok(0);
    }
    st.update(tp, |t| t.pending |= bit(s));
    drop(st);
    ARRIVED.notify_all();
    wake_from_futex(tp, s);
    Ok(0)
}

/// The calling thread is about to wait on the futex at `addr`, which a
/// signal for it wakes from then on; `false` if a signal it does not block
/// is pending already (the wait should not begin: `EINTR`).
pub fn futex_wait_begins(addr: usize) -> bool {
    let tp = current();
    let mut st = STATE.lock();
    st.update(tp, |t| {
        let free = t.ready() == 0;
        if free {
            t.futex = addr;
        }
        free
    })
}

/// The calling thread's futex wait is over; whether a signal it does not
/// block is pending (the wait ends with `EINTR`).
pub fn futex_wait_ends() -> bool {
    let tp = current();
    STATE.lock().update(tp, |t| {
        t.futex = 0;
        t.ready() != 0
    })
}

/// Wakes thread `tp` from the futex wait it is in, or about to be in, while
/// signal `s` is pending for it and not blocked. The wake reaches every
/// thread waiting on that futex (in other processes too, for a word of
/// memory they share), which the others take for a spurious one; it is
/// repeated, a moment apart, until the thread has left the wait, in case
/// it had not reached the kernel yet.
fn wake_from_futex(tp: usize, s: u32) {
    loop {
        let addr = {
            let st = STATE.lock();
            let t = st.get(tp);
            if t.ready() & bit(s) == 0 || t.futex == 0 {
                return;
            }
            t.futex
        };
        let _ = rt::sys::call(abi::nr::FUTEX_WAKE, [addr, i32::MAX as usize, 0, 0, 0, 0]);
        rt::time::sleep(rt::time::Duration::from_micros(200));
    }
}

/// The signals whose disposition is to be ignored, which a program it
/// starts inherits (handlers become the default there).
pub fn ignored() -> u64 {
    let st = STATE.lock();
    (1..sig::NSIG).filter(|&s| st.actions[s as usize].handler == sig::IGN).fold(0, |set, s| set | bit(s))
}

/// The calling thread's mask, which a program it starts inherits.
pub fn current_mask() -> u64 {
    STATE.lock().get(current()).mask
}

/// What a program inherits from the one that started it: the signals to
/// ignore, and the main thread's (the caller's) mask.
pub fn inherit(ignored: u64, mask: u64) {
    let tp = current();
    let mut st = STATE.lock();
    for s in (1..sig::NSIG).filter(|&s| ignored & bit(s) & !UNCATCHABLE != 0) {
        st.actions[s as usize].handler = sig::IGN;
    }
    st.update(tp, |t| t.mask = mask & !UNCATCHABLE);
}

pub unsafe fn sigaltstack(old: usize) -> SysResult {
    if old != 0 {
        // stack_t { ss_sp, ss_flags = SS_DISABLE, ss_size }.
        // SAFETY: the program passed a stack_t.
        unsafe { user::write::<[usize; 3]>(old, [0, 2, 0])? };
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::boxed::Box;

    fn state() -> State {
        State {
            actions: [Sigaction::default(); sig::NSIG as usize],
            threads: BTreeMap::new(),
            tids: BTreeMap::new(),
            next_tid: 1,
            ready_threads: Box::leak(Box::new(AtomicUsize::new(0))),
            pending: 0,
        }
    }

    #[test]
    fn signals_pending_for_the_process_go_to_the_thread_that_takes_them() {
        let mut st = state();
        st.update(0x100, |t| t.mask = bit(10));
        st.update(0x200, |t| t.mask = bit(10) | bit(12));
        // Every thread blocks 10: the process keeps it.
        assert_eq!(st.unblocked_but(0x100, 10), None);
        assert_eq!(st.unblocked_but(0x200, 12), Some(0x100));
        st.pending |= bit(10);
        // Blocked still, a thread does not take it; waiting for it, it does.
        st.claim(0x100, 0);
        assert_eq!((st.pending, st.get(0x100).pending), (bit(10), 0));
        st.claim(0x200, bit(10));
        assert_eq!(st.take(0x200, bit(10)), Some(10));
        assert_eq!(st.pending, 0);
        // Unblocked, it is the thread's, ready to run.
        st.pending |= bit(10);
        st.update(0x100, |t| t.mask = 0);
        st.claim(0x100, 0);
        assert_eq!((st.pending, st.get(0x100).ready()), (0, bit(10)));
    }

    #[test]
    fn thread_ids_are_unique_among_live_threads() {
        let mut st = state();
        let pid = 2;
        // Neither the process's id nor a live thread's; wrapping below the
        // limit.
        st.tids.insert(TID_LIMIT - 1, 0x1000);
        st.tids.insert(1, 0x2000);
        st.next_tid = TID_LIMIT - 2;
        assert_eq!(st.free_tid(pid), Some(TID_LIMIT - 2));
        assert_eq!(st.free_tid(pid), Some(3));
        assert_eq!(st.free_tid(pid), Some(4));
        // An ended thread's id comes back once the search reaches it.
        st.tids.remove(&1);
        st.next_tid = TID_LIMIT - 1;
        assert_eq!(st.free_tid(pid), Some(1));
        // Ids handed out are never 0 or above the limit.
        let mut st = state();
        let mut seen = alloc::collections::BTreeSet::new();
        for i in 0..1000 {
            let tid = st.free_tid(pid).unwrap();
            assert!(tid > 0 && tid < TID_LIMIT && tid != pid && seen.insert(tid));
            st.tids.insert(tid, 0x10_000 + i);
        }
    }

    #[test]
    fn threads_with_signals_ready_are_counted() {
        let mut st = state();
        let count = |st: &State| st.ready_threads.load(Ordering::Relaxed);
        st.update(0x100, |t| t.pending |= bit(10));
        st.update(0x200, |t| t.mask = bit(12));
        assert_eq!(count(&st), 1);
        // Blocked: pending, not ready.
        st.update(0x200, |t| t.pending |= bit(12));
        assert_eq!(count(&st), 1);
        st.update(0x200, |t| t.mask = 0);
        assert_eq!(count(&st), 2);
        assert_eq!(st.take(0x200, bit(12) | bit(5)), Some(12));
        assert_eq!(st.take(0x200, bit(12)), None);
        assert_eq!(count(&st), 1);
        // A thread that ends with signals ready is no longer counted.
        st.tids.insert(7, 0x100);
        st.update(0x100, |t| t.tid = 7);
        st.forget(0x100);
        assert_eq!((count(&st), st.tids.get(&7)), (0, None));
    }

    #[test]
    fn a_thread_names_itself_as_linux_cuts_the_name() {
        assert_eq!(thread_name(), None);
        set_thread_name(b"a-thread-of-many-words\0");
        assert_eq!(thread_name(), Some(*b"a-thread-of-man\0"));
        set_thread_name(b"short\0garbage");
        assert_eq!(thread_name().as_ref().map(|n| &n[..6]), Some(&b"short\0"[..]));
        // No name: the process's, again.
        set_thread_name(b"\0");
        assert_eq!(thread_name(), None);
    }

    #[test]
    fn handler_contexts_hold_the_mask_where_the_c_library_reads_it() {
        assert!(ucontext::SIGMASK.is_multiple_of(8) && ucontext::SIGMASK + 8 <= ucontext::SIZE);
        let c = Context::new(0x8000_0000_0000_0001);
        let bytes = unsafe { core::slice::from_raw_parts(&c as *const Context as *const u8, size_of::<Context>()) };
        assert!(size_of::<Context>() >= ucontext::SIZE);
        assert_eq!(bytes[ucontext::SIGMASK..ucontext::SIGMASK + 8], 0x8000_0000_0000_0001u64.to_ne_bytes());
        assert_eq!(c.mask(), 0x8000_0000_0000_0001);
    }
}
