// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `timerfd`: timers that are descriptors (what event loops time their
//! sleeps with: GLib's, WebKit's). They are the layer's own: a timer's
//! state behind a lock, and an event object signaled while expirations
//! wait to be read, which `poll`, `epoll` and the reads that wait watch as
//! they watch an eventfd's.
//!
//! Veda's kernel has no timer objects: a thread of the layer's, started
//! with the process's first timer, sleeps until the next timer expires and
//! signals it. Whoever looks at a timer (a read, `poll`, `timerfd_gettime`)
//! counts the expirations due first, so none waits on that thread to see
//! one. The thread runs only the layer's code, on `runtime`'s heap and
//! locks, never the C library's.
//!
//! Every clock's timer counts down on the monotonic clock: an absolute
//! time of `CLOCK_REALTIME` is taken as the time from now until then when
//! it is set (`TFD_TIMER_CANCEL_ON_SET` is accepted, and the clock is never
//! set: such a timer is never cancelled).

use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;

use abi::signals::SIGNALED;
use runtime::object::Event;
use runtime::sync::Mutex;

use crate::error::{self, SysResult};
use crate::fd::{self, Description, Object};
use crate::linux::errno::{EAGAIN, EINVAL, ENOMEM};
use crate::linux::{Itimerspec, Timespec, clock, o, tfd};
use crate::{time, user};

/// The event's signal while expirations wait to be read.
pub const READABLE: u32 = SIGNALED;

/// A timer descriptor's object.
pub struct TimerFd {
    timer: Arc<Timer>,
}

struct Timer {
    /// The clock it was made for (`CLOCK_REALTIME` or a monotonic one).
    realtime: bool,
    state: Mutex<State>,
    event: Event,
}

#[derive(Default)]
struct State {
    /// The next expiration (monotonic ns), while armed.
    deadline: Option<u64>,
    /// The period (ns), kept while the timer is disarmed; 0 for a timer
    /// that expires once.
    interval: u64,
    /// Expirations not read yet.
    expirations: u64,
}

impl Timer {
    /// Counts the expirations due by `now` (monotonic ns), and signals them;
    /// the next deadline (`u64::MAX`: none).
    fn expire(&self, now: u64) -> u64 {
        let mut s = self.state.lock();
        if let Some(d) = s.deadline
            && now >= d
        {
            let n = (now - d).checked_div(s.interval).map_or(1, |late| 1 + late);
            s.expirations = s.expirations.saturating_add(n);
            s.deadline = (s.interval > 0).then(|| d.saturating_add(n.saturating_mul(s.interval)));
            let _ = self.event.0.signal(0, READABLE);
        }
        s.deadline.unwrap_or(u64::MAX)
    }

    /// What `timerfd_gettime` says: the time until the next expiration (0:
    /// disarmed) and the period.
    fn get(&self) -> Itimerspec {
        let now = time::monotonic_ns();
        self.expire(now);
        let s = self.state.lock();
        Itimerspec {
            interval: Timespec::from_ns(s.interval),
            value: Timespec::from_ns(s.deadline.map_or(0, |d| d.saturating_sub(now).max(1))),
        }
    }
}

impl TimerFd {
    /// Whether a read would not wait.
    pub fn readable(&self) -> bool {
        self.timer.expire(time::monotonic_ns());
        self.timer.state.lock().expirations > 0
    }

    /// The event object `poll` and `epoll` wait on.
    pub fn event(&self) -> &Event {
        &self.timer.event
    }

    /// `read`: the expirations since the last read, as 8 bytes, waiting for
    /// one unless `nonblock`.
    pub fn read(&self, buf: &mut [u8], nonblock: bool) -> SysResult {
        if buf.len() < 8 {
            return Err(EINVAL);
        }
        loop {
            self.timer.expire(time::monotonic_ns());
            {
                let mut s = self.timer.state.lock();
                if s.expirations > 0 {
                    let n = core::mem::take(&mut s.expirations);
                    let _ = self.timer.event.0.signal(READABLE, 0);
                    buf[..8].copy_from_slice(&n.to_ne_bytes());
                    return Ok(8);
                }
            }
            if nonblock {
                return Err(EAGAIN);
            }
            // The timers' thread signals the expiration.
            crate::stream::wait_signal(&self.timer.event.0, READABLE)?;
        }
    }
}

/// The process's timers, for the timers' thread, and the event that wakes
/// it (once it has started).
static TIMERS: Mutex<Vec<Weak<Timer>>> = Mutex::new(Vec::new());
static WAKE: Mutex<Option<Event>> = Mutex::new(None);

/// Has the timers' thread look at the timers again (one was set), starting
/// it if it has not started.
fn wake_timers() -> Result<(), isize> {
    let mut wake = WAKE.lock();
    if let Some(e) = wake.as_ref() {
        let _ = e.0.signal(0, SIGNALED);
        return Ok(());
    }
    let event = Event::create().map_err(error::kernel)?;
    let theirs = Event::from_handle(event.0.duplicate(None).map_err(error::kernel)?);
    runtime::thread::Builder::new().name("timerfd").spawn(move || timers_thread(theirs)).map_err(|_| ENOMEM)?;
    *wake = Some(event);
    Ok(())
}

/// The timers' thread: signals each timer as it expires, sleeping until the
/// next one does or one is set.
fn timers_thread(wake: Event) {
    loop {
        // Cleared before looking, so that a timer set meanwhile wakes the
        // sleep that follows.
        let _ = wake.0.signal(SIGNALED, 0);
        let now = time::monotonic_ns();
        let mut next = u64::MAX;
        {
            let mut timers = TIMERS.lock();
            timers.retain(|t| t.strong_count() > 0);
            for t in timers.iter().filter_map(Weak::upgrade) {
                next = next.min(t.expire(now));
            }
        }
        let _ = wake.wait(SIGNALED, next);
    }
}

/// `timerfd_create`.
pub fn create(clk: u32, flags: u32) -> SysResult {
    if flags & !(tfd::CLOEXEC | tfd::NONBLOCK) != 0 {
        return Err(EINVAL);
    }
    let realtime = match clk {
        clock::REALTIME => true,
        clock::MONOTONIC | clock::BOOTTIME => false,
        _ => return Err(EINVAL),
    };
    let event = Event::create().map_err(error::kernel)?;
    let timer = Arc::new(Timer { realtime, state: Mutex::new(State::default()), event });
    TIMERS.lock().push(Arc::downgrade(&timer));
    let desc = Description::new(Object::TimerFd(TimerFd { timer }), o::RDWR | (flags & tfd::NONBLOCK));
    fd::insert(desc, flags & tfd::CLOEXEC != 0, 0).map(|n| n as usize)
}

/// The timer of descriptor `fd`.
fn timer_of(fd: i32) -> Result<Arc<Timer>, isize> {
    let desc = fd::get(fd)?;
    match &desc.object {
        Object::TimerFd(t) => Ok(t.timer.clone()),
        _ => Err(EINVAL),
    }
}

/// `timerfd_settime`.
///
/// # Safety
/// `new` and `old` (which may be null) point at the program's
/// `itimerspec`s.
pub unsafe fn settime(fd: i32, flags: u32, new: usize, old: usize) -> SysResult {
    if flags & !(tfd::TIMER_ABSTIME | tfd::TIMER_CANCEL_ON_SET) != 0 {
        return Err(EINVAL);
    }
    let timer = timer_of(fd)?;
    // SAFETY: the program passed an itimerspec.
    let spec: Itimerspec = unsafe { user::read(new)? };
    let (Some(value), Some(interval)) = (spec.value.to_ns(), spec.interval.to_ns()) else { return Err(EINVAL) };
    if old != 0 {
        // SAFETY: as above.
        unsafe { user::write(old, timer.get())? };
    }
    let now = time::monotonic_ns();
    let deadline = match (value, flags & tfd::TIMER_ABSTIME != 0) {
        (0, _) => None,
        (v, false) => Some(now.saturating_add(v)),
        (v, true) if timer.realtime => Some(now.saturating_add(v.saturating_sub(time::realtime_ns()))),
        (v, true) => Some(v),
    };
    {
        // A timer disarmed keeps its period, as Linux's does: it says it.
        let mut s = timer.state.lock();
        *s = State { deadline, interval, expirations: 0 };
        let _ = timer.event.0.signal(READABLE, 0);
    }
    if deadline.is_some() {
        wake_timers()?;
    }
    Ok(0)
}

/// `timerfd_gettime`.
///
/// # Safety
/// `curr` points at the program's `itimerspec`.
pub unsafe fn gettime(fd: i32, curr: usize) -> SysResult {
    let timer = timer_of(fd)?;
    // SAFETY: the program passed an itimerspec.
    unsafe { user::write(curr, timer.get())? };
    Ok(0)
}
