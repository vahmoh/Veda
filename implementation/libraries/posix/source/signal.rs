// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Signals, and the threads they are for.
//!
//! The layer keeps what Linux's kernel keeps of signals: the dispositions
//! (per process), each thread's mask and pending signals, the signals
//! pending for the process while every thread blocks them, and what each
//! pending signal says of itself (`siginfo_t`: who sent it, a timer's
//! value). A signal comes from the process itself (`raise`, `kill` of its
//! own process, `pthread_kill`, `SIGPIPE`, its timers: `crate::timer`) or
//! from another process, by way of the kernel, which keeps it among the
//! process's incoming signals until a thread takes them ([`take_incoming`]).
//! As on Linux, a signal for the process goes to a thread that does not
//! block it (the one that takes it, if it may; else one that waits for it,
//! `sigwait`; else any), and stays pending for the process while every
//! thread blocks it; a signal ignored when it comes is dropped, unless
//! blocked.
//!
//! A thread is interrupted when a signal becomes one it is to act on
//! (`crate::interrupt`): it runs the handler, or the default action, as its
//! system call returns, or at once if it runs the program's code, with the
//! signal and the handler's mask blocked meanwhile; a blocking call the
//! signal ended returns what it did, or `EINTR`, or starts again after the
//! handlers if each had `SA_RESTART` ([`returning`]). Handlers get the
//! registers of the code they interrupted (as their `ucontext_t`), and
//! where it resumes is what they leave there. A default action ends the
//! process (as the signal ends it: its parent's wait status says so) or
//! does nothing; nothing stops a process (`SIGSTOP`, `SIGTSTP`...).
//!
//! A thread is known by its thread pointer
//! (`architecture::thread_pointer`), unique while the thread lives, and has
//! a thread id, unique among the process's live threads: the main thread's
//! is the process id, as on Linux, and the others' are handed out as they
//! start ([`thread_created`]) and free again once they end
//! ([`thread_exited`]).

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use abi::ThreadState as Registers;
use runtime::sync::Mutex;

use crate::architecture::signal as arch;
use crate::error::SysResult;
use crate::linux::errno::{EAGAIN, EINTR, EINVAL, EPERM, ERESTARTSYS, ESRCH};
use crate::linux::{Sigaction, Siginfo, Timespec, sig, ucontext};
use crate::{interrupt, process, thread, time, user};

/// Thread ids are below this: the C library keeps flags in the high bits of
/// the words that hold them (a mutex's owner).
const TID_LIMIT: i32 = 1 << 30;

/// Where [`State::infos`] keeps what the signals pending for the process
/// say (no thread has this thread pointer).
const PROCESS: usize = usize::MAX;

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
    /// The mask it goes back to once a handler has run, which a call that
    /// waits with a mask of its own sets (`sigsuspend`, `ppoll`): the
    /// handler runs with that call's mask, and restores this one.
    saved_mask: Option<u64>,
    /// Signals it waits for (`sigtimedwait`), blocked as they are: one that
    /// comes is its.
    waiting: u64,
}

impl ThreadState {
    /// The pending signals it does not block.
    fn ready(&self) -> u64 {
        self.pending & !self.mask
    }

    /// Whether signal `s` would be its: it does not block it, or waits for
    /// it.
    fn wants(&self, s: u32) -> bool {
        (!self.mask | self.waiting) & bit(s) != 0
    }
}

/// What a pending signal says of itself (`siginfo_t`).
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Info {
    /// `si_code`: who sent it, and how (`sig::SI_USER`, ...).
    pub code: i32,
    /// The process that sent it (`kill`, `sigqueue`), or a timer's id.
    pub pid: i32,
    /// A timer's expirations it stands for, beyond the first.
    pub overrun: i32,
    /// `sigqueue`'s or a timer's value.
    pub value: u64,
}

impl Info {
    /// What `kill` from process `pid` (this one, or another) says.
    pub fn user(pid: i32) -> Info {
        Info { code: sig::SI_USER, pid, ..Info::default() }
    }

    /// What `tkill`, `tgkill` and `raise` say.
    pub fn tkill() -> Info {
        Info { code: sig::SI_TKILL, pid: process::pid(), ..Info::default() }
    }

    /// The `siginfo_t` of signal `s`.
    fn siginfo(&self, s: u32) -> Siginfo {
        let mut info = Siginfo { signo: s as i32, code: self.code, pid: self.pid, ..Siginfo::default() };
        if self.code == sig::SI_TIMER {
            // `si_timerid` and `si_overrun`, where a sender's id is.
            info.uid = self.overrun as u32;
        } else {
            info.uid = crate::vfs::UID;
        }
        if matches!(self.code, sig::SI_TIMER | sig::SI_QUEUE) {
            info.set_value(self.value);
        }
        info
    }
}

/// How a signal is aimed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    /// At one thread (`pthread_kill`, a timer's `SIGEV_THREAD_ID`).
    Thread(usize),
    /// At the process, preferably at this thread, if it may take it (the
    /// one that sends it, or takes it from the kernel).
    Process(Option<usize>),
}

struct State {
    actions: [Sigaction; sig::NSIG as usize],
    /// Threads by thread pointer.
    threads: BTreeMap<usize, ThreadState>,
    /// Thread pointers by thread id.
    tids: BTreeMap<i32, usize>,
    /// Where the search for a free thread id starts.
    next_tid: i32,
    /// The main thread's thread pointer (0 before it has one), which takes
    /// a signal for the process that no thread sends, if it may.
    main: usize,
    /// Signals pending for the process: every thread blocked them as they
    /// came.
    pending: u64,
    /// What each pending signal says, by thread pointer ([`PROCESS`] for
    /// the process's) and signal.
    infos: BTreeMap<(usize, u32), Info>,
    /// Tells a thread whether it has signals ready (pending, not blocked):
    /// sets or clears the word `crate::interrupt` looks at as the thread
    /// leaves the layer.
    mark: fn(usize, bool),
}

static STATE: Mutex<State> = Mutex::new(State {
    actions: [Sigaction { handler: sig::DFL, flags: 0, restorer: 0, mask: 0 }; sig::NSIG as usize],
    threads: BTreeMap::new(),
    tids: BTreeMap::new(),
    next_tid: 1,
    main: 0,
    pending: 0,
    infos: BTreeMap::new(),
    mark: interrupt::mark_ready,
});

/// Thread pointers exist (the C library has set up thread-local storage).
static THREADS_READY: AtomicBool = AtomicBool::new(false);

impl State {
    /// Changes thread `tp`'s state (a fresh one if it has none), keeping
    /// what tells it whether it has signals ready right.
    fn update<R>(&mut self, tp: usize, f: impl FnOnce(&mut ThreadState) -> R) -> R {
        let t = self.threads.entry(tp).or_default();
        let was = t.ready() != 0;
        let r = f(t);
        let is = t.ready() != 0;
        if was != is && tp != 0 {
            (self.mark)(tp, is);
        }
        r
    }

    /// Thread `tp`'s state, without making one.
    fn get(&self, tp: usize) -> ThreadState {
        self.threads.get(&tp).copied().unwrap_or_default()
    }

    /// Forgets thread `tp`: its state, its id, and its pending signals.
    fn forget(&mut self, tp: usize) {
        if let Some(t) = self.threads.remove(&tp)
            && self.tids.get(&t.tid) == Some(&tp)
        {
            self.tids.remove(&t.tid);
        }
        self.infos.retain(|&(owner, _), _| owner != tp);
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

    /// Whether signal `s` is ignored: by its disposition, or because its
    /// default action does nothing.
    fn ignores(&self, s: u32) -> bool {
        match self.actions[s as usize].handler {
            sig::IGN => true,
            sig::DFL => !abi::posix_signal::ends_process(s as usize),
            _ => false,
        }
    }

    /// Makes signal `s` pending for `target`, with what it says: returns
    /// the thread to interrupt, if any (one that is to act on it, or that
    /// waits for it). As on Linux, a signal ignored as it comes is dropped
    /// unless the thread it is for blocks it (until it is unblocked, which
    /// may find it handled); one for the process goes to the preferred
    /// thread if that may take it, else to a thread that waits for it, else
    /// to one that does not block it, and while every thread blocks it, it
    /// is the process's.
    fn generate(&mut self, s: u32, info: Info, target: Target) -> Option<usize> {
        let tp = match target {
            Target::Thread(tp) => {
                if self.ignores(s) && self.get(tp).mask & bit(s) == 0 {
                    return None;
                }
                Some(tp)
            }
            Target::Process(prefer) => {
                let receiver = prefer.or(Some(self.main)).filter(|tp| *tp != 0 && self.threads.contains_key(tp));
                let blocked = receiver.is_some_and(|tp| self.get(tp).mask & bit(s) != 0);
                if self.ignores(s) && !blocked {
                    return None;
                }
                receiver
                    .filter(|&tp| self.get(tp).wants(s))
                    .or_else(|| self.threads.iter().find(|(_, t)| t.waiting & bit(s) != 0).map(|(&tp, _)| tp))
                    .or_else(|| self.threads.iter().find(|(_, t)| t.mask & bit(s) == 0).map(|(&tp, _)| tp))
            }
        };
        let Some(tp) = tp else {
            if self.pending & bit(s) == 0 {
                self.pending |= bit(s);
                self.infos.insert((PROCESS, s), info);
            }
            return None;
        };
        // A signal pending is pending once (Linux queues only real-time
        // signals): the first keeps what it says, but that a timer's
        // expirations came meanwhile (its overruns).
        if self.get(tp).pending & bit(s) == 0 {
            self.update(tp, |t| t.pending |= bit(s));
            self.infos.insert((tp, s), info);
        } else if let Some(first) = self.infos.get_mut(&(tp, s))
            && first.code == sig::SI_TIMER
            && info.code == sig::SI_TIMER
            && first.pid == info.pid
        {
            first.overrun = first.overrun.saturating_add(1).saturating_add(info.overrun);
        }
        self.get(tp).wants(s).then_some(tp)
    }

    /// Gives thread `tp` the signals pending for the process that it does
    /// not block, or that it waits for (`want`).
    fn claim(&mut self, tp: usize, want: u64) {
        let claimed = self.pending & (!self.get(tp).mask | want);
        if claimed == 0 {
            return;
        }
        self.pending &= !claimed;
        for s in signals(claimed) {
            let info = self.infos.remove(&(PROCESS, s)).unwrap_or_default();
            self.infos.insert((tp, s), info);
        }
        self.update(tp, |t| t.pending |= claimed);
    }

    /// Takes the lowest pending signal of `set` from thread `tp`, with what
    /// it says.
    fn take(&mut self, tp: usize, set: u64) -> Option<(u32, Info)> {
        let s = self.update(tp, |t| {
            let found = t.pending & set;
            (found != 0).then(|| {
                let s = found.trailing_zeros() + 1;
                t.pending &= !bit(s);
                s
            })
        })?;
        Some((s, self.infos.remove(&(tp, s)).unwrap_or_default()))
    }

    /// Drops the signals ready for thread `tp` that are ignored now (their
    /// disposition changed since they came, or they were blocked then).
    fn drop_ignored(&mut self, tp: usize) {
        let ignored = signals(self.get(tp).ready()).filter(|&s| self.ignores(s)).fold(0, |set, s| set | bit(s));
        if ignored != 0 {
            self.update(tp, |t| t.pending &= !ignored);
            self.infos.retain(|&(owner, s), _| owner != tp || ignored & bit(s) == 0);
        }
    }
}

/// The signals of a set, lowest first.
fn signals(set: u64) -> impl Iterator<Item = u32> {
    (1..sig::NSIG).filter(move |&s| set & bit(s) != 0)
}

/// The calling thread's thread pointer (0 before there are any).
fn current() -> usize {
    if !THREADS_READY.load(Ordering::Acquire) {
        return 0;
    }
    crate::architecture::thread_pointer()
}

/// Whether the C library has set up thread-local storage: what the
/// layer's word for each thread needs.
pub fn threads_are_ready() -> bool {
    THREADS_READY.load(Ordering::Acquire)
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
    st.main = tp;
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

/// The calling thread is ending: its id is free again, its state gone, and
/// with it the signals pending for it alone (as on Linux).
pub fn thread_exited() {
    let tp = current();
    let mut st = STATE.lock();
    st.forget(tp);
    if st.main == tp {
        st.main = 0;
    }
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

/// Interrupts the threads `tps` (their thread pointers): for each, a signal
/// it is to act on, or waits for, came.
fn interrupt_threads(st: &State, tps: impl IntoIterator<Item = usize>) -> Vec<i32> {
    tps.into_iter().filter(|&tp| tp != 0).map(|tp| st.get(tp).tid).filter(|&tid| tid != 0).collect()
}

/// Has the threads with ids `tids` act on what came for them (after the
/// lock is let go: interrupting one takes the threads' handles).
fn poke(tids: Vec<i32>) {
    for tid in tids {
        thread::interrupt(tid);
    }
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
            // Ignoring a pending signal discards it, blocked or not.
            if st.ignores(s) {
                let threads: Vec<usize> = st.threads.keys().copied().collect();
                for tp in threads {
                    st.update(tp, |t| t.pending &= !bit(s));
                }
                st.pending &= !bit(s);
                st.infos.retain(|&(_, signal), _| signal != s);
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

/// Sets the calling thread's mask (`how` as `sigprocmask` has it) to
/// `new`, if given; returns the one it had. Signals pending for the process
/// that it no longer blocks become its own.
fn set_mask(how: u32, new: Option<u64>) -> Result<u64, isize> {
    let tp = current();
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
    Ok(prev)
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
    let prev = set_mask(how, new)?;
    if old != 0 {
        // SAFETY: as above.
        unsafe { user::write(old, prev)? };
    }
    // What it unblocked runs as the call returns.
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

/// Sends signal `s` to the calling thread (`SIGPIPE`, `raise`): it acts on
/// it as its system call returns.
pub fn raise(s: u32) {
    if !valid(s) {
        return;
    }
    let tp = current();
    STATE.lock().generate(s, Info::tkill(), Target::Thread(tp));
}

/// Takes the signals other processes sent this one from the kernel, and
/// makes each pending as `kill` from the process that sent it would (the
/// calling thread taking those it may): what a thread does when it is
/// interrupted, and has put off.
pub fn take_incoming() {
    interrupt::taking_incoming();
    let mut sent = abi::SentSignals::default();
    let taken = runtime::sys::call(abi::nr::SIGNAL_TAKE, [&raw mut sent as usize, 0, 0, 0, 0, 0]);
    if !matches!(taken, Ok(n) if n > 0) {
        return;
    }
    let me = current();
    let mut st = STATE.lock();
    let mut woken = Vec::new();
    for s in signals(sent.set) {
        let info = Info::user(process::pid_of(sent.senders[s as usize - 1]));
        woken.extend(st.generate(s, info, Target::Process(Some(me))).filter(|&tp| tp != me));
    }
    let tids = interrupt_threads(&st, woken);
    drop(st);
    poke(tids);
}

/// Whether the calling thread, interrupted in a wait, has a signal to act
/// on (or one it waits for, `sigtimedwait`), after taking what other
/// processes sent: then the blocking call it is in ends; else the wait goes
/// on. Ready signals now ignored are dropped.
pub fn interrupted() -> bool {
    take_incoming();
    let tp = current();
    let mut st = STATE.lock();
    st.drop_ignored(tp);
    let t = st.get(tp);
    t.ready() != 0 || t.pending & t.waiting != 0 || st.pending & t.waiting != 0
}

/// What delivering the signals ready for a thread came to.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Delivered {
    /// Handlers that ran.
    pub handlers: usize,
    /// Whether each that ran had `SA_RESTART`.
    pub restart: bool,
    /// Whether a handler changed where the thread resumes (or what with).
    pub redirected: bool,
}

/// Acts on the calling thread's ready signals: runs their handlers, with
/// `context` the registers of the code they interrupted (as their
/// `ucontext_t` has them, and as they leave them), or the default action.
/// A mask a call set to wait with ([`ThreadState::saved_mask`]) is the
/// thread's again once the first handler has run, or at the end.
pub fn deliver(mut context: Option<&mut Registers>) -> Delivered {
    let tp = current();
    let mut done = Delivered { restart: true, ..Delivered::default() };
    loop {
        let (s, info, action, restore) = {
            let mut st = STATE.lock();
            st.claim(tp, 0);
            let ready = st.get(tp).ready();
            if ready == 0 {
                if let Some(m) = st.get(tp).saved_mask {
                    st.update(tp, |t| {
                        t.mask = m;
                        t.saved_mask = None;
                    });
                    st.claim(tp, 0);
                    continue;
                }
                return done;
            }
            let s = ready.trailing_zeros() + 1;
            let (_, info) = st.take(tp, bit(s)).unwrap_or_default();
            let action = st.actions[s as usize];
            let handled = action.handler != sig::IGN && action.handler != sig::DFL;
            if handled && action.flags & sig::SA_RESETHAND != 0 {
                st.actions[s as usize].handler = sig::DFL;
            }
            // The mask a handler restores: one a call set aside, for the
            // first handler.
            let saved = if handled { st.update(tp, |t| t.saved_mask.take()) } else { None };
            let restore = saved.unwrap_or(st.get(tp).mask);
            (s, info, action, restore)
        };
        if info.code == sig::SI_TIMER {
            crate::timer::taken(info.pid, info.overrun);
        }
        match action.handler {
            sig::IGN => {}
            sig::DFL => {
                if abi::posix_signal::ends_process(s as usize) {
                    process::exit_by_signal(s);
                }
            }
            _ => {
                let changed = call_handler(tp, s, &info, &action, restore, context.as_deref_mut());
                done.handlers += 1;
                done.restart &= action.flags & sig::SA_RESTART != 0;
                done.redirected |= changed;
            }
        }
    }
}

/// What a system call returns once the signals ready for the calling
/// thread are acted on (as a system call returns on Linux): its result
/// `r`, the caller's registers being `caller`; `again` carries the call
/// out anew. A blocking call a signal ended (`ERESTARTSYS`) starts again
/// once the handlers have run if each had `SA_RESTART`, or no handler ran
/// (nothing is to come of the signal), unless a handler sent the thread
/// elsewhere; else it ends with `EINTR`.
pub fn returning(mut r: SysResult, caller: &mut arch::Caller, mut again: impl FnMut() -> SysResult) {
    loop {
        let interrupted = r == Err(ERESTARTSYS);
        let shown = if interrupted { Err(EINTR) } else { r };
        caller.rax = crate::error::encode(shown) as u64;
        if !has_ready() {
            if interrupted {
                // Nothing came of what interrupted it.
                r = again();
                continue;
            }
            return;
        }
        let done = act(caller);
        if interrupted && done.restart && !done.redirected {
            r = again();
            continue;
        }
        return;
    }
}

/// Acts on the calling thread's ready signals, if it has any, as its system
/// call returns to `caller`, whose registers the handlers see and may
/// change: what came after it looked as the call ended.
pub fn act_on_ready(caller: &mut arch::Caller) {
    if has_ready() {
        act(caller);
    }
}

/// [`deliver`]s with the caller's registers, which the handlers may change.
fn act(caller: &mut arch::Caller) -> Delivered {
    let before = caller.state();
    let mut regs = before;
    let mut done = deliver(Some(&mut regs));
    arch::redirected(&before, &mut regs);
    done.redirected |= regs != before;
    caller.resume(&regs);
    done
}

/// Whether the calling thread has signals ready, after taking those other
/// processes sent if it put that off: what a system call looks at as it
/// returns. Its word says whether either may be so, without the lock, as
/// every change of what is ready says it ([`State::mark`]): signals pending
/// for the process become a thread's only as it unblocks them, which
/// claims them, or as one is made ready.
fn has_ready() -> bool {
    if !threads_are_ready() || !interrupt::anything_to_do() {
        return false;
    }
    if interrupt::put_off() {
        take_incoming();
    }
    let tp = current();
    let mut st = STATE.lock();
    st.claim(tp, 0);
    st.drop_ignored(tp);
    let t = st.get(tp);
    t.ready() != 0 || t.saved_mask.is_some()
}

/// A signal handler's context (`ucontext_t`): the registers of the code it
/// interrupted (`uc_mcontext`, where it resumes), no signal stack, and the
/// mask to restore when it returns (`uc_sigmask`), which the handler may
/// change, as the C library's thread cancellation does.
#[repr(C, align(16))]
struct Context([u64; ucontext::SIZE.div_ceil(8)]);

impl Context {
    const MASK: usize = ucontext::SIGMASK / 8;

    fn new(registers: &Registers, mask: u64) -> Context {
        let mut c = Context([0; ucontext::SIZE.div_ceil(8)]);
        c.0[Self::MASK] = mask;
        c.0[ucontext::STACK_FLAGS / 8] = sig::SS_DISABLE as u64;
        // SAFETY: the 256 bytes of `uc_mcontext` are within the context.
        unsafe { arch::store_mcontext(registers, c.mcontext()) };
        c
    }

    fn mcontext(&mut self) -> *mut core::ffi::c_void {
        // SAFETY: within the context, whose size the architecture gives.
        unsafe { (self.0.as_mut_ptr() as *mut u8).add(ucontext::MCONTEXT).cast() }
    }

    fn mask(&self) -> u64 {
        self.0[Self::MASK]
    }

    /// The registers it holds, into `registers`.
    fn registers(&mut self, registers: &mut Registers) {
        // SAFETY: as in `mcontext`.
        unsafe { arch::load_mcontext(self.mcontext(), registers) };
    }
}

/// Runs a handler on this thread, outside the layer, with the signal (and
/// the handler's mask) blocked while it runs; `restore` is the mask it
/// leaves (its context's: the handler may change it), and `registers`
/// those of the code it interrupted (zeros if they are not known), which
/// it may change. Returns whether it changed them.
fn call_handler(
    tp: usize,
    s: u32,
    info: &Info,
    a: &Sigaction,
    restore: u64,
    registers: Option<&mut Registers>,
) -> bool {
    STATE.lock().update(tp, |t| {
        let mut block = a.mask;
        if a.flags & sig::SA_NODEFER == 0 {
            block |= bit(s);
        }
        t.mask = (t.mask | block) & !UNCATCHABLE;
    });
    let given = registers.as_ref().map_or(Registers::default(), |r| **r);
    let mut context = Context::new(&given, restore);
    if a.flags & sig::SA_SIGINFO != 0 {
        let mut siginfo = info.siginfo(s);
        // SAFETY: the program installed this three-argument handler.
        let f: extern "C" fn(i32, *mut Siginfo, *mut Context) = unsafe { core::mem::transmute(a.handler) };
        interrupt::outside_layer(|| f(s as i32, &mut siginfo, &mut context));
    } else {
        // SAFETY: the program installed this handler.
        let f: extern "C" fn(i32) = unsafe { core::mem::transmute(a.handler) };
        interrupt::outside_layer(|| f(s as i32));
    }
    let mut st = STATE.lock();
    st.update(tp, |t| t.mask = context.mask() & !UNCATCHABLE);
    st.claim(tp, 0);
    drop(st);
    let Some(registers) = registers else { return false };
    let mut left = given;
    context.registers(&mut left);
    let changed = left != given;
    *registers = left;
    changed
}

/// Waits, as a call that waits for a signal handler does (`pause`,
/// `sigsuspend`), until the calling thread has a signal to act on: it does
/// as the call returns, which then ends with `EINTR`.
fn wait_for_handler() -> SysResult {
    let tp = current();
    loop {
        {
            let mut st = STATE.lock();
            st.claim(tp, 0);
            st.drop_ignored(tp);
            if st.get(tp).ready() != 0 {
                return Err(EINTR);
            }
        }
        // Interrupted only with a signal to act on, which the next look
        // finds.
        let _ = interrupt::sleep_until(abi::DEADLINE_INFINITE);
    }
}

/// `pause`.
pub fn pause() -> SysResult {
    wait_for_handler()
}

/// Sleeps until `deadline` (monotonic ns) unless the calling thread has a
/// signal to act on first (it does as its call returns); returns whether it
/// had.
pub fn sleep_until(deadline: u64) -> bool {
    interrupt::sleep_until(deadline).is_err()
}

pub unsafe fn rt_sigsuspend(mask: usize) -> SysResult {
    // SAFETY: the program passed a sigset.
    let m: u64 = unsafe { user::read(mask)? };
    let tp = current();
    {
        let mut st = STATE.lock();
        st.update(tp, |t| {
            t.saved_mask = Some(t.mask);
            t.mask = m & !UNCATCHABLE;
        });
        st.claim(tp, 0);
    }
    // The handler runs with this mask as the call returns, and the thread's
    // own comes back after it.
    wait_for_handler()
}

/// Runs `call` (a wait: `ppoll`, `pselect6`, `epoll_pwait`) with the
/// calling thread's mask `mask`, if given: as `sigsuspend`, the handler of
/// a signal that ends it runs with that mask, and the thread's own comes
/// back afterwards; otherwise it is back as the call ends.
pub fn with_mask(mask: Option<u64>, call: impl FnOnce() -> SysResult) -> SysResult {
    let Some(m) = mask else { return call() };
    let tp = current();
    {
        let mut st = STATE.lock();
        st.update(tp, |t| {
            t.saved_mask = Some(t.mask);
            t.mask = m & !UNCATCHABLE;
        });
        st.claim(tp, 0);
    }
    let r = call();
    // Interrupted, the call ends with a signal ready, whose handler
    // restores the mask; else it is back now.
    let mut st = STATE.lock();
    if r != Err(EINTR) || st.get(tp).ready() == 0 {
        st.update(tp, |t| {
            if let Some(saved) = t.saved_mask.take() {
                t.mask = saved;
            }
        });
        st.claim(tp, 0);
    }
    r
}

/// `sigtimedwait`: takes a pending signal of `set`, waiting until `timeout`
/// (forever if there is none) for one; `EINTR` if a signal not of `set`
/// is to be acted on first (as the call returns).
pub unsafe fn rt_sigtimedwait(set: usize, info: usize, timeout: usize) -> SysResult {
    // SAFETY: the program passed a sigset.
    let want: u64 = unsafe { user::read::<u64>(set)? } & !UNCATCHABLE;
    let deadline = if timeout == 0 {
        abi::DEADLINE_INFINITE
    } else {
        // SAFETY: the program passed a timespec.
        let t: Timespec = unsafe { user::read(timeout)? };
        time::monotonic_ns().saturating_add(t.to_ns().ok_or(EINVAL)?)
    };
    let tp = current();
    let taken = loop {
        {
            // Waiting for them before looking: one that comes after is the
            // thread's.
            let mut st = STATE.lock();
            st.update(tp, |t| t.waiting = want);
            st.claim(tp, want);
            if let Some(found) = st.take(tp, want) {
                st.update(tp, |t| t.waiting = 0);
                break found;
            }
            st.drop_ignored(tp);
            if st.get(tp).ready() != 0 {
                st.update(tp, |t| t.waiting = 0);
                return Err(EINTR);
            }
        }
        if interrupt::sleep_until(deadline).is_ok() && time::monotonic_ns() >= deadline {
            let mut st = STATE.lock();
            st.update(tp, |t| t.waiting = 0);
            st.claim(tp, want);
            match st.take(tp, want) {
                Some(found) => break found,
                None => return Err(EAGAIN),
            }
        }
    };
    let (s, what) = taken;
    if what.code == sig::SI_TIMER {
        crate::timer::taken(what.pid, what.overrun);
    }
    if info != 0 {
        // SAFETY: the program passed a siginfo.
        unsafe { user::write(info, what.siginfo(s))? };
    }
    Ok(s as usize)
}

/// `kill` aimed at this process (signal 0: only whether it exists): it goes
/// to the calling thread if that does not block it (and is acted on as its
/// call returns), else to another as [`State::generate`] says. `SIGKILL`
/// ends the process.
pub fn send_self(s: u32) -> SysResult {
    send_self_with(s, Info::user(process::pid()))
}

/// [`send_self`], with what the signal says.
fn send_self_with(s: u32, info: Info) -> SysResult {
    if s == 0 {
        return Ok(0);
    }
    if !valid(s) {
        return Err(EINVAL);
    }
    if s == sig::KILL {
        process::exit_by_signal(s);
    }
    let me = current();
    let mut st = STATE.lock();
    let woken = st.generate(s, info, Target::Process(Some(me))).filter(|&tp| tp != me);
    let tids = interrupt_threads(&st, woken);
    drop(st);
    poke(tids);
    Ok(0)
}

/// Generates signal `s` for this process from inside the layer (a timer's,
/// from the timers' thread, which is no thread of the program's), with what
/// it says: it goes to a thread as [`State::generate`] says.
pub fn send_process(s: u32, info: Info) {
    let mut st = STATE.lock();
    let woken = st.generate(s, info, Target::Process(None));
    let tids = interrupt_threads(&st, woken);
    drop(st);
    poke(tids);
}

/// Generates signal `s` for thread `tid` of this process from inside the
/// layer (a timer's `SIGEV_THREAD_ID`, from the timers' thread, which has
/// no thread pointer); `false` if there is no such thread.
pub fn send_thread(tid: i32, s: u32, info: Info) -> bool {
    send_thread_from(0, tid, s, info)
}

/// [`send_thread`] from the thread whose thread pointer is `me` (0: none of
/// the program's), which acts on what it sends itself as its call returns.
fn send_thread_from(me: usize, tid: i32, s: u32, info: Info) -> bool {
    let mut st = STATE.lock();
    let Some(&tp) = st.tids.get(&tid) else { return false };
    let woken = st.generate(s, info, Target::Thread(tp)).filter(|&tp| tp != me);
    let tids = interrupt_threads(&st, woken);
    drop(st);
    poke(tids);
    true
}

/// `tkill` and `tgkill`: signal `s` (0: none, only whether the thread
/// exists) for thread `tid` of this process, which takes it as the module's
/// documentation says.
pub fn send_to_thread(tid: i32, s: u32) -> SysResult {
    send_to_thread_with(tid, s, Info::tkill())
}

fn send_to_thread_with(tid: i32, s: u32, info: Info) -> SysResult {
    if s != 0 && !valid(s) {
        return Err(EINVAL);
    }
    if !STATE.lock().tids.contains_key(&tid) {
        return Err(ESRCH);
    }
    if s == 0 {
        return Ok(0);
    }
    if s == sig::KILL {
        process::exit_by_signal(s);
    }
    if !send_thread_from(current(), tid, s, info) {
        return Err(ESRCH);
    }
    Ok(0)
}

/// `rt_sigqueueinfo` and `rt_tgsigqueueinfo` (`sigqueue`): signal `s` with
/// the value `info` carries, to this process (`tid` 0) or one of its
/// threads; another process gets the signal without it (`crate::process`).
pub unsafe fn sigqueue(pid: i32, tid: i32, s: u32, info: usize) -> SysResult {
    // SAFETY: the program passed a siginfo.
    let given: Siginfo = unsafe { user::read(info)? };
    // Only the kernel may say it sent a signal, as on Linux.
    if given.code >= 0 || given.code == sig::SI_TKILL {
        return Err(EPERM);
    }
    let what = Info { code: given.code, pid: process::pid(), value: given.value(), ..Info::default() };
    match (pid == process::pid(), tid) {
        (true, 0) => send_self_with(s, what),
        (true, tid) => send_to_thread_with(tid, s, what),
        (false, 0) => process::kill(pid, s),
        (false, _) => Err(ESRCH),
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
        unsafe { user::write::<[usize; 3]>(old, [0, sig::SS_DISABLE as usize, 0])? };
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    /// What [`State::mark`] was told, in the tests.
    static MARKS: StdMutex<Vec<(usize, bool)>> = StdMutex::new(Vec::new());

    fn record(tp: usize, ready: bool) {
        MARKS.lock().unwrap().push((tp, ready));
    }

    fn state() -> State {
        State {
            actions: [Sigaction::default(); sig::NSIG as usize],
            threads: BTreeMap::new(),
            tids: BTreeMap::new(),
            next_tid: 1,
            main: 0,
            pending: 0,
            infos: BTreeMap::new(),
            mark: |_, _| {},
        }
    }

    /// A state with threads `tps`, each with its mask, the first the main
    /// thread.
    fn with_threads(tps: &[(usize, u64)]) -> State {
        let mut st = state();
        for (i, &(tp, mask)) in tps.iter().enumerate() {
            let tid = 1000 + i as i32;
            st.update(tp, |t| {
                t.tid = tid;
                t.mask = mask;
            });
            st.tids.insert(tid, tp);
        }
        st.main = tps[0].0;
        st
    }

    const HANDLER: Sigaction = Sigaction { handler: 0x1000, flags: 0, restorer: 0, mask: 0 };

    #[test]
    fn signals_pending_for_the_process_go_to_the_thread_that_takes_them() {
        let mut st = with_threads(&[(0x100, bit(10)), (0x200, bit(10) | bit(12))]);
        st.actions[10] = HANDLER;
        // Every thread blocks 10: the process keeps it.
        assert_eq!(st.generate(10, Info::user(7), Target::Process(Some(0x200))), None);
        assert_eq!(st.pending, bit(10));
        // Blocked still, a thread does not take it; waiting for it, it does.
        st.claim(0x100, 0);
        assert_eq!((st.pending, st.get(0x100).pending), (bit(10), 0));
        st.claim(0x200, bit(10));
        assert_eq!(st.take(0x200, bit(10)), Some((10, Info::user(7))));
        assert_eq!(st.pending, 0);
        // Unblocked, it is the thread's, ready to run.
        st.generate(10, Info::user(8), Target::Process(None));
        st.update(0x100, |t| t.mask = 0);
        st.claim(0x100, 0);
        assert_eq!((st.pending, st.get(0x100).ready()), (0, bit(10)));
    }

    #[test]
    fn a_signal_for_the_process_goes_to_a_thread_that_may_take_it() {
        let mut st = with_threads(&[(0x100, bit(10)), (0x200, 0), (0x300, bit(10))]);
        st.actions[10] = HANDLER;
        // The preferred thread blocks it: another that does not takes it, and
        // is interrupted.
        assert_eq!(st.generate(10, Info::user(5), Target::Process(Some(0x100))), Some(0x200));
        assert_eq!(st.get(0x200).ready(), bit(10));
        // One that waits for it (`sigwait`) comes before one that does not
        // block it.
        st.take(0x200, bit(10));
        st.update(0x300, |t| t.waiting = bit(10));
        assert_eq!(st.generate(10, Info::user(5), Target::Process(Some(0x100))), Some(0x300));
        assert_eq!((st.get(0x300).pending, st.get(0x300).ready()), (bit(10), 0));
        // The preferred thread takes it if it may; with none preferred, the
        // main thread.
        st.update(0x100, |t| t.mask = 0);
        assert_eq!(st.generate(12, Info::user(5), Target::Process(Some(0x100))), Some(0x100));
        st.take(0x100, bit(12));
        assert_eq!(st.generate(12, Info::user(5), Target::Process(None)), Some(0x100));
    }

    #[test]
    fn a_signal_ignored_as_it_comes_is_dropped_unless_blocked() {
        let mut st = with_threads(&[(0x100, 0)]);
        // SIGCHLD does nothing by default, SIGUSR1 ignored: dropped.
        assert_eq!(st.generate(sig::CHLD, Info::user(5), Target::Thread(0x100)), None);
        st.actions[10].handler = sig::IGN;
        st.generate(10, Info::user(5), Target::Process(None));
        assert_eq!((st.get(0x100).pending, st.pending), (0, 0));
        // Blocked, it waits: for `sigwait`, or a handler by the time it is
        // unblocked; still ignored then, it is dropped.
        st.update(0x100, |t| t.mask = bit(sig::CHLD));
        st.generate(sig::CHLD, Info::user(5), Target::Thread(0x100));
        assert_eq!(st.get(0x100).pending, bit(sig::CHLD));
        st.update(0x100, |t| t.mask = 0);
        st.drop_ignored(0x100);
        assert_eq!(st.get(0x100).pending, 0);
        // SIGTERM ends the process by default: not dropped.
        st.generate(15, Info::user(5), Target::Thread(0x100));
        assert_eq!(st.get(0x100).ready(), bit(15));
    }

    #[test]
    fn a_pending_signal_keeps_what_its_first_sender_said() {
        let mut st = with_threads(&[(0x100, bit(10))]);
        st.actions[10] = HANDLER;
        st.generate(10, Info::user(5), Target::Thread(0x100));
        st.generate(10, Info::user(6), Target::Thread(0x100));
        let timer = Info { code: sig::SI_TIMER, pid: 3, overrun: 2, value: 99 };
        st.generate(14, timer, Target::Thread(0x100));
        assert_eq!(st.take(0x100, bit(10)), Some((10, Info::user(5))));
        assert_eq!(st.take(0x100, bit(10)), None);
        let (s, info) = st.take(0x100, bit(14)).unwrap();
        let si = info.siginfo(s);
        assert_eq!((si.signo, si.code, si.pid, si.uid, si.value()), (14, sig::SI_TIMER, 3, 2, 99));
        // A thread that ends takes what its signals said with it.
        st.generate(10, Info::user(5), Target::Thread(0x100));
        st.forget(0x100);
        assert!(st.infos.is_empty());
    }

    #[test]
    fn threads_are_told_when_they_have_signals_ready() {
        let mut st = with_threads(&[(0x100, 0), (0x200, bit(12))]);
        st.mark = record;
        st.actions[10] = HANDLER;
        st.actions[12] = HANDLER;
        MARKS.lock().unwrap().clear();
        st.generate(10, Info::user(5), Target::Thread(0x100));
        // Blocked: pending, not ready.
        st.generate(12, Info::user(5), Target::Thread(0x200));
        st.update(0x200, |t| t.mask = 0);
        st.take(0x200, bit(12) | bit(5));
        assert_eq!(*MARKS.lock().unwrap(), [(0x100, true), (0x200, true), (0x200, false)]);
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
    fn handler_contexts_hold_the_registers_and_mask_where_the_c_library_reads_them() {
        assert!(ucontext::SIGMASK.is_multiple_of(8) && ucontext::SIGMASK + 8 <= ucontext::SIZE);
        let regs = Registers { rip: 0x5000, rsp: 0x7000, ..Registers::default() };
        let mut c = Context::new(&regs, 0x8000_0000_0000_0001);
        let bytes = unsafe { core::slice::from_raw_parts(&c as *const Context as *const u8, size_of::<Context>()) };
        assert!(size_of::<Context>() >= ucontext::SIZE);
        assert_eq!(bytes[ucontext::SIGMASK..ucontext::SIGMASK + 8], 0x8000_0000_0000_0001u64.to_ne_bytes());
        assert_eq!(c.mask(), 0x8000_0000_0000_0001);
        // No signal stack (SS_DISABLE), and the registers given.
        assert_eq!(bytes[ucontext::STACK_FLAGS..ucontext::STACK_FLAGS + 4], 2i32.to_ne_bytes());
        let mut back = Registers::default();
        c.registers(&mut back);
        assert_eq!(back, regs);
    }
}
