// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Interval timers (`setitimer`, `getitimer`; `alarm` is musl's, on
//! `ITIMER_REAL`) and POSIX timers (`timer_create` and the rest): the
//! layer's own, as timerfd's are, which a thread of the layer's (started
//! with the process's first timer) expires, sending each its signal: to the
//! process (`SIGALRM`, `SIGVTALRM`, `SIGPROF`, or a POSIX timer's own), or
//! to a thread of it (`SIGEV_THREAD_ID`: musl's `SIGEV_THREAD` timers have
//! a thread of their own wait for the signal of each expiration). The
//! thread runs only the layer's code, on `rt`'s heap and locks.
//!
//! Real-time timers count down on the monotonic clock: an absolute time of
//! `CLOCK_REALTIME` is the time from now until then when it is set (Veda's
//! clock is never set). Timers of processor time (`ITIMER_VIRTUAL`,
//! `ITIMER_PROF`, POSIX timers on a CPU-time clock) count the process's
//! (or a thread's) processor time, user and system time as one, which the
//! thread looks at every [`CPU_POLL_NS`] while one is armed.
//!
//! An expiration while the timer's signal is still pending sends nothing
//! more: it counts as an overrun, which the signal says when it is taken
//! (`si_overrun`), as `timer_getoverrun` does then (Linux's way).

use alloc::collections::BTreeMap;

use abi::signals::SIGNALED;
use rt::object::Event;
use rt::sync::Mutex;

use crate::error::{self, SysResult};
use crate::linux::errno::{EAGAIN, EINVAL, ENOMEM};
use crate::linux::timer::*;
use crate::linux::{Itimerspec, Itimerval, Sigevent, Timespec, Timeval, clock, sig};
use crate::signal::Info;
use crate::{signal, time, user};

/// How often the timers' thread looks at the processor time used while a
/// timer of processor time is armed.
const CPU_POLL_NS: u64 = 10_000_000;

/// The most POSIX timers a process has (Linux's limit is the signals that
/// may be pending, in the thousands).
const MAX_TIMERS: usize = 1024;

/// What a timer counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Clock {
    /// The monotonic clock (real time).
    Monotonic,
    /// The process's processor time.
    Process,
    /// A thread's processor time.
    Thread(i32),
}

impl Clock {
    /// Its reading now, in nanoseconds.
    fn now(self) -> u64 {
        match self {
            Clock::Monotonic => time::monotonic_ns(),
            Clock::Process => rt::object::process_self_info().map_or(0, |i| i.cpu_time_ns),
            Clock::Thread(tid) => crate::thread::cpu_time_ns(tid).unwrap_or(0),
        }
    }
}

/// How a timer tells of its expirations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Notify {
    /// It does not (`SIGEV_NONE`).
    Nothing,
    /// Signal `signo`, saying `info`, to the process, or to thread `tid`.
    Signal { signo: u32, info: Info, tid: Option<i32> },
}

#[derive(Clone, Copy, Debug)]
struct Timer {
    clock: Clock,
    /// Made for `CLOCK_REALTIME`: an absolute time is that clock's.
    realtime: bool,
    /// The next expiration, on its clock, while armed.
    deadline: Option<u64>,
    /// The period (ns); 0 for a timer that expires once.
    interval: u64,
    notify: Notify,
    /// The overruns the signal last taken said (`timer_getoverrun`).
    overrun: i32,
}

impl Timer {
    /// Counts the expirations due by `now` (on its clock): how many, and
    /// moves its deadline on (or disarms it).
    fn expire(&mut self, now: u64) -> u64 {
        let Some(d) = self.deadline.filter(|&d| now >= d) else { return 0 };
        let n = (now - d).checked_div(self.interval).map_or(1, |late| 1 + late);
        self.deadline = (self.interval > 0).then(|| d.saturating_add(n.saturating_mul(self.interval)));
        n
    }

    /// What `timer_gettime` and `getitimer` say: the time left until the
    /// next expiration (0: disarmed; at least 1 ns while armed), and the
    /// period.
    fn left(&self, now: u64) -> (u64, u64) {
        (self.deadline.map_or(0, |d| d.saturating_sub(now).max(1)), self.interval)
    }

    /// Arms it for `value` from `now` (0: disarms it), then every
    /// `interval`; `absolute`: `value` is the time on its clock (the real
    /// time of `realtime_now`, for a timer of the real-time clock).
    fn set(&mut self, now: u64, value: u64, interval: u64, absolute: bool, realtime_now: u64) {
        self.interval = interval;
        self.deadline = match (value, absolute) {
            (0, _) => None,
            (v, false) => Some(now.saturating_add(v)),
            (v, true) if self.realtime => Some(now.saturating_add(v.saturating_sub(realtime_now))),
            (v, true) => Some(v),
        };
    }
}

/// The interval timers (`ITIMER_REAL`, `ITIMER_VIRTUAL`, `ITIMER_PROF`),
/// then the POSIX timers by id.
struct Timers {
    itimers: [Timer; 3],
    posix: BTreeMap<i32, Timer>,
}

const fn itimer(clock: Clock, signo: u32) -> Timer {
    let info = Info { code: sig::SI_KERNEL, pid: 0, overrun: 0, value: 0 };
    let notify = Notify::Signal { signo, info, tid: None };
    Timer { clock, realtime: false, deadline: None, interval: 0, notify, overrun: 0 }
}

static TIMERS: Mutex<Timers> = Mutex::new(Timers {
    itimers: [
        itimer(Clock::Monotonic, sig::ALRM),
        itimer(Clock::Process, sig::VTALRM),
        itimer(Clock::Process, sig::PROF),
    ],
    posix: BTreeMap::new(),
});

/// The event that wakes the timers' thread, once it has started.
static WAKE: Mutex<Option<Event>> = Mutex::new(None);

/// Has the timers' thread look at the timers again (one was set), starting
/// it if it has not started.
fn wake_timers() -> Result<(), isize> {
    let mut wake = WAKE.lock();
    if let Some(e) = wake.as_ref() {
        let _ = e.signal();
        return Ok(());
    }
    let event = Event::create().map_err(error::kernel)?;
    let theirs = Event::from_handle(event.0.duplicate(None).map_err(error::kernel)?);
    rt::thread::Builder::new().name("timers").spawn(move || timers_thread(theirs)).map_err(|_| ENOMEM)?;
    *wake = Some(event);
    Ok(())
}

/// Expires the timers due, sending their signals; when to look again
/// (monotonic ns, `u64::MAX`: never, until one is set).
fn expire_all() -> u64 {
    let mono = time::monotonic_ns();
    let mut next = u64::MAX;
    let mut due = alloc::vec::Vec::new();
    {
        let mut t = TIMERS.lock();
        let Timers { itimers, posix } = &mut *t;
        for (id, timer) in itimers.iter_mut().map(|t| (None, t)).chain(posix.iter_mut().map(|(&id, t)| (Some(id), t))) {
            if timer.deadline.is_none() {
                continue;
            }
            let now = timer.clock.now();
            let n = timer.expire(now);
            if n > 0
                && let Notify::Signal { signo, mut info, tid } = timer.notify
            {
                if id.is_some() {
                    info.overrun = (n - 1).min(i32::MAX as u64) as i32;
                }
                due.push((signo, info, tid));
            }
            if let Some(d) = timer.deadline {
                next = next.min(match timer.clock {
                    Clock::Monotonic => d,
                    _ => mono.saturating_add((d - now.min(d)).clamp(1, CPU_POLL_NS)),
                });
            }
        }
    }
    // Sent with the timers' lock let go: taking the signal takes it.
    for (signo, info, tid) in due {
        match tid {
            Some(tid) => {
                signal::send_thread(tid, signo, info);
            }
            None => signal::send_process(signo, info),
        }
    }
    next
}

/// The timers' thread: expires each timer as it comes due, sleeping until
/// the next one does or one is set.
fn timers_thread(wake: Event) {
    loop {
        // Cleared before looking, so that a timer set meanwhile wakes the
        // sleep that follows.
        let _ = wake.clear();
        let next = expire_all();
        let _ = wake.wait(SIGNALED, next);
    }
}

/// A POSIX timer's signal was taken (delivered, or by `sigwaitinfo`),
/// saying `overrun` overruns: what `timer_getoverrun` says from then on.
pub fn taken(id: i32, overrun: i32) {
    if let Some(t) = TIMERS.lock().posix.get_mut(&id) {
        t.overrun = overrun;
    }
}

fn timeval_ns(t: Timeval) -> Result<u64, isize> {
    if t.sec < 0 || !(0..1_000_000).contains(&t.usec) {
        return Err(EINVAL);
    }
    Ok((t.sec as u64).saturating_mul(1_000_000_000).saturating_add(t.usec as u64 * 1000))
}

/// A time as a `timeval`, rounded up to a microsecond, as Linux rounds
/// what `getitimer` says (a timer that has not expired never reads 0).
fn ns_timeval(ns: u64) -> Timeval {
    let us = ns.div_ceil(1000);
    Timeval { sec: (us / 1_000_000) as i64, usec: (us % 1_000_000) as i64 }
}

/// The interval timer `which`.
fn itimer_index(which: u32) -> Result<usize, isize> {
    match which {
        ITIMER_REAL => Ok(0),
        ITIMER_VIRTUAL => Ok(1),
        ITIMER_PROF => Ok(2),
        _ => Err(EINVAL),
    }
}

/// What `getitimer` says of interval timer `index`.
fn itimer_value(t: &Timer) -> Itimerval {
    let (value, interval) = t.left(t.clock.now());
    Itimerval { interval: ns_timeval(interval), value: ns_timeval(value) }
}

/// `getitimer`.
///
/// # Safety
/// `out` points at the program's `itimerval`.
pub unsafe fn getitimer(which: u32, out: usize) -> SysResult {
    let i = itimer_index(which)?;
    let value = itimer_value(&TIMERS.lock().itimers[i]);
    // SAFETY: per the caller.
    unsafe { user::write(out, value)? };
    Ok(0)
}

/// `setitimer`: arms interval timer `which` (or disarms it, for a value of
/// 0), saying in `old` (if not null) what it was.
///
/// # Safety
/// `new` and `old` (which may be null) point at the program's
/// `itimerval`s.
pub unsafe fn setitimer(which: u32, new: usize, old: usize) -> SysResult {
    let i = itimer_index(which)?;
    let spec: Itimerval = if new == 0 {
        Itimerval::default()
    } else {
        // SAFETY: per the caller.
        unsafe { user::read(new)? }
    };
    let (value, interval) = (timeval_ns(spec.value)?, timeval_ns(spec.interval)?);
    let was = {
        let mut t = TIMERS.lock();
        let timer = &mut t.itimers[i];
        let was = itimer_value(timer);
        let now = timer.clock.now();
        timer.set(now, value, interval, false, 0);
        was
    };
    if old != 0 {
        // SAFETY: per the caller.
        unsafe { user::write(old, was)? };
    }
    if value > 0 {
        wake_timers()?;
    }
    Ok(0)
}

/// `alarm` (x86-64's own system call; musl's `alarm` uses `setitimer`):
/// `ITIMER_REAL` in `seconds`, once; the seconds that were left of it
/// (rounded up).
pub fn alarm(seconds: u32) -> SysResult {
    let was = {
        let mut t = TIMERS.lock();
        let timer = &mut t.itimers[0];
        let now = timer.clock.now();
        let (left, _) = timer.left(now);
        timer.set(now, seconds as u64 * 1_000_000_000, 0, false, 0);
        left
    };
    if seconds > 0 {
        wake_timers()?;
    }
    Ok(was.div_ceil(1_000_000_000) as usize)
}

/// The clock of a POSIX timer on clock `clk`.
fn timer_clock(clk: u32) -> Result<Clock, isize> {
    match clk {
        clock::REALTIME | clock::MONOTONIC | clock::BOOTTIME => Ok(Clock::Monotonic),
        clock::PROCESS_CPUTIME_ID => Ok(Clock::Process),
        clock::THREAD_CPUTIME_ID => Ok(Clock::Thread(signal::current_tid())),
        _ => Err(EINVAL),
    }
}

/// `timer_create`: a timer on clock `clk` that tells of its expirations
/// as `sev` (null: `SIGALRM` to the process, with the timer's id for its
/// value) says; writes its id to `out`.
///
/// # Safety
/// `sev` (which may be null) points at the program's `sigevent`, `out` at
/// an `int`.
pub unsafe fn timer_create(clk: u32, sev: usize, out: usize) -> SysResult {
    let clock = timer_clock(clk)?;
    let ev: Option<Sigevent> = if sev == 0 {
        None
    } else {
        // SAFETY: per the caller.
        Some(unsafe { user::read(sev)? })
    };
    let mut t = TIMERS.lock();
    if t.posix.len() >= MAX_TIMERS {
        return Err(EAGAIN);
    }
    let id = (0..).find(|id| !t.posix.contains_key(id)).unwrap_or(0);
    let signal = |signo: i32, value: u64, tid: Option<i32>| {
        let signo = u32::try_from(signo).ok().filter(|&s| (1..sig::NSIG).contains(&s)).ok_or(EINVAL)?;
        let info = Info { code: sig::SI_TIMER, pid: id, overrun: 0, value };
        Ok::<Notify, isize>(Notify::Signal { signo, info, tid })
    };
    let notify = match ev {
        None => signal(sig::ALRM as i32, id as u64, None)?,
        Some(e) => match e.notify {
            SIGEV_NONE => Notify::Nothing,
            SIGEV_SIGNAL => signal(e.signo, e.value, None)?,
            SIGEV_THREAD_ID => {
                if !crate::thread::exists(e.tid) {
                    return Err(EINVAL);
                }
                signal(e.signo, e.value, Some(e.tid))?
            }
            // musl makes these `SIGEV_THREAD_ID` timers of a thread of its
            // own.
            _ => return Err(EINVAL),
        },
    };
    // SAFETY: per the caller.
    unsafe { user::write(out, id)? };
    let realtime = clk == clock::REALTIME;
    t.posix.insert(id, Timer { clock, realtime, deadline: None, interval: 0, notify, overrun: 0 });
    Ok(0)
}

/// `timer_settime`.
///
/// # Safety
/// `new` and `old` (which may be null) point at the program's
/// `itimerspec`s.
pub unsafe fn timer_settime(id: i32, flags: u32, new: usize, old: usize) -> SysResult {
    if flags & !TIMER_ABSTIME != 0 {
        return Err(EINVAL);
    }
    // SAFETY: per the caller.
    let spec: Itimerspec = unsafe { user::read(new)? };
    let (Some(value), Some(interval)) = (spec.value.to_ns(), spec.interval.to_ns()) else { return Err(EINVAL) };
    let was = {
        let mut t = TIMERS.lock();
        let timer = t.posix.get_mut(&id).ok_or(EINVAL)?;
        let now = timer.clock.now();
        let (left, period) = timer.left(now);
        timer.set(now, value, interval, flags & TIMER_ABSTIME != 0, time::realtime_ns());
        Itimerspec { interval: Timespec::from_ns(period), value: Timespec::from_ns(left) }
    };
    if old != 0 {
        // SAFETY: per the caller.
        unsafe { user::write(old, was)? };
    }
    if value > 0 {
        wake_timers()?;
    }
    Ok(0)
}

/// `timer_gettime`.
///
/// # Safety
/// `out` points at the program's `itimerspec`.
pub unsafe fn timer_gettime(id: i32, out: usize) -> SysResult {
    let spec = {
        let t = TIMERS.lock();
        let timer = t.posix.get(&id).ok_or(EINVAL)?;
        let (left, period) = timer.left(timer.clock.now());
        Itimerspec { interval: Timespec::from_ns(period), value: Timespec::from_ns(left) }
    };
    // SAFETY: per the caller.
    unsafe { user::write(out, spec)? };
    Ok(0)
}

/// `timer_getoverrun`: the overruns the timer's signal said when it was
/// last taken.
pub fn timer_getoverrun(id: i32) -> SysResult {
    TIMERS.lock().posix.get(&id).map(|t| t.overrun as usize).ok_or(EINVAL)
}

/// `timer_delete`.
pub fn timer_delete(id: i32) -> SysResult {
    TIMERS.lock().posix.remove(&id).map(|_| 0).ok_or(EINVAL)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timer() -> Timer {
        Timer {
            clock: Clock::Monotonic,
            realtime: false,
            deadline: None,
            interval: 0,
            notify: Notify::Nothing,
            overrun: 0,
        }
    }

    #[test]
    fn a_periodic_timer_counts_every_expiration_it_missed() {
        let mut t = timer();
        t.set(1000, 500, 100, false, 0);
        assert_eq!(t.expire(1499), 0);
        assert_eq!(t.expire(1500), 1);
        assert_eq!(t.deadline, Some(1600));
        // Late by 3.5 periods: 4 expirations, the next on the period.
        assert_eq!(t.expire(1950), 4);
        assert_eq!(t.deadline, Some(2000));
        assert_eq!(t.left(1950), (50, 100));
    }

    #[test]
    fn a_timer_that_expires_once_is_disarmed_after() {
        let mut t = timer();
        t.set(0, 10, 0, false, 0);
        assert_eq!(t.expire(25), 1);
        assert_eq!((t.deadline, t.left(25)), (None, (0, 0)));
        // Absolute, and disarmed by a value of 0.
        t.set(100, 300, 0, true, 0);
        assert_eq!(t.deadline, Some(300));
        t.set(100, 0, 50, false, 0);
        assert_eq!((t.deadline, t.interval), (None, 50));
        // Absolute on the real-time clock: from now on the monotonic clock,
        // and at once for a time past.
        t.realtime = true;
        t.set(100, 5_000, 0, true, 4_000);
        assert_eq!(t.deadline, Some(1_100));
        t.set(100, 3_000, 0, true, 4_000);
        assert_eq!(t.deadline, Some(100));
    }

    #[test]
    fn a_timer_about_to_expire_never_reads_as_disarmed() {
        let mut t = timer();
        t.set(0, 10, 0, false, 0);
        assert_eq!(t.left(10), (1, 0));
        assert_eq!(ns_timeval(1), Timeval { sec: 0, usec: 1 });
        assert_eq!(ns_timeval(1_500_000_001), Timeval { sec: 1, usec: 500_001 });
        assert_eq!(timeval_ns(Timeval { sec: 2, usec: 5 }), Ok(2_000_005_000));
        assert_eq!(timeval_ns(Timeval { sec: 0, usec: 1_000_000 }), Err(EINVAL));
    }
}
