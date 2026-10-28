// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `epoll`: an interest list of descriptors, and waiting until some of them
//! are ready.
//!
//! An epoll instance has a port of the kernel's. A registration is looked
//! at — its readiness worked out as `poll` does ([`io::readiness`]) — when
//! it is added or changed, when its object raises a signal it waits for (a
//! packet on the port), when the layer sees its description change in a
//! way no signal says ([`Description::changed`]: a program met it not
//! ready, or emptied or filled it with a read or write that moved less than
//! asked, as tokio counts on; an eventfd was written to), and, while it is
//! ready, at every wait if it is level-triggered. A wait costs what changed, however many
//! descriptors are registered.
//!
//! A registration that is not ready waits with the port for the signals
//! that make it so. An edge-triggered one that is ready waits for what
//! makes it ready anew (a message on a network socket's channel: data, send
//! credit), and is reported when it becomes ready in a way it was not at
//! the last look, or when it is ready and changed since it was last
//! reported: each write to an eventfd is an event, which is what async
//! runtimes' wakers count on. Level-triggered ones are reported while they
//! are ready, one-shot ones once (until `EPOLL_CTL_MOD`).
//!
//! As on Linux, a registration is of a descriptor and its open file
//! description, and goes when the description does (when every descriptor
//! of it is closed). An epoll descriptor cannot be added to another, and
//! files, directories and the like, which are always ready, cannot be added
//! (`EPERM`, as Linux says of files). Pipes, sockets, eventfds and child
//! processes' descriptors (`pidfd_open`) can.

use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};

use abi::signals::SIGNALED;
use abi::{PortPacket, RawHandle};
use runtime::object::{self, Event, Port};
use runtime::sync::Mutex;

use crate::error::{self, SysResult};
use crate::fd::{self, Description, Object};
use crate::io;
use crate::linux::errno::*;
use crate::linux::{EpollEvent, epoll};
use crate::{time, user};

/// The key of a watcher's own event on its port.
const KICK: u64 = 0;
/// Packets taken from the port at a time.
const PACKETS: usize = 64;
/// The most events one wait reports (Linux's limit).
const MAX_EVENTS: usize = i32::MAX as usize / size_of::<EpollEvent>();

/// Registrations in the process's epoll instances: while there are none,
/// nothing needs telling what the program's reads and writes found.
static REGISTRATIONS: AtomicUsize = AtomicUsize::new(0);

/// Whether any epoll instance of the process has registrations.
pub fn watching() -> bool {
    REGISTRATIONS.load(Ordering::Relaxed) > 0
}

/// A registration: a descriptor and its description, and what it is
/// waited for.
struct Registration {
    /// The descriptor and its description (`Arc::as_ptr`), which key it.
    id: (i32, usize),
    desc: Weak<Description>,
    /// The events asked for, with the flags (`EPOLLET`, `EPOLLONESHOT`).
    events: u32,
    data: u64,
    /// The events it was ready for at the last look.
    seen: u32,
    /// Its description's changes ([`io::Ready::seq`]) when last reported.
    seq: u64,
    /// One-shot and reported: nothing more until `EPOLL_CTL_MOD`.
    disabled: bool,
    /// The object it waits on with the port, if it does.
    armed: Option<RawHandle>,
}

#[derive(Default)]
struct State {
    registrations: BTreeMap<u64, Registration>,
    keys: BTreeMap<(i32, usize), u64>,
    last_key: u64,
    /// The registrations to look at in the next wait, in turn, and whether
    /// each is there.
    queue: VecDeque<u64>,
    queued: BTreeSet<u64>,
}

impl State {
    fn queue(&mut self, key: u64) {
        if self.queued.insert(key) {
            self.queue.push_back(key);
        }
    }

    /// Undoes registration `key`'s wait with the port, if it has one.
    fn disarm(&mut self, port: &Port, key: u64) {
        if let Some(h) = self.registrations.get_mut(&key).and_then(|r| r.armed.take()) {
            let _ = object::wait_async_cancel(h, port, key);
        }
    }

    fn remove(&mut self, port: &Port, key: u64) {
        self.disarm(port, key);
        if let Some(r) = self.registrations.remove(&key) {
            self.keys.remove(&r.id);
            REGISTRATIONS.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

impl Drop for State {
    fn drop(&mut self) {
        REGISTRATIONS.fetch_sub(self.registrations.len(), Ordering::Relaxed);
    }
}

/// What tells an epoll instance of what changes: its port, which its
/// registrations' objects signal, and an event on it, which the layer
/// signals when it sees a watched description change ([`Watcher::kick`]).
pub struct Watcher {
    port: Port,
    kick: Event,
    state: Mutex<State>,
}

impl Watcher {
    /// Registration `key` changed in a way no signal says: the next wait
    /// looks at it.
    pub fn kick(&self, key: u64) {
        self.state.lock().queue(key);
        let _ = self.kick.signal();
    }
}

/// An epoll instance (`Object::Epoll`).
pub struct Epoll {
    watcher: Arc<Watcher>,
}

impl Epoll {
    pub fn new() -> Result<Epoll, isize> {
        let port = Port::create().map_err(error::kernel)?;
        let kick = Event::create().map_err(error::kernel)?;
        object::wait_async(kick.raw(), &port, KICK, SIGNALED).map_err(error::kernel)?;
        Ok(Epoll { watcher: Arc::new(Watcher { port, kick, state: Mutex::new(State::default()) }) })
    }

    /// `epoll_ctl`'s operation `op` on descriptor `fd`, of description
    /// `target`.
    fn ctl(&self, op: u32, fd: i32, target: &Arc<Description>, event: EpollEvent) -> SysResult {
        match &target.object {
            Object::Stream(_)
            | Object::Inet(_)
            | Object::Unix(_)
            | Object::EventFd(_)
            | Object::TimerFd(_)
            | Object::Pidfd(_) => {}
            _ => return Err(EPERM),
        }
        let (events, data) = (event.events, event.data);
        let w = &self.watcher;
        let id = (fd, Arc::as_ptr(target) as usize);
        let mut st = w.state.lock();
        match op {
            epoll::CTL_ADD => {
                if st.keys.contains_key(&id) {
                    return Err(EEXIST);
                }
                st.last_key += 1;
                let key = st.last_key;
                let registration = Registration {
                    id,
                    desc: Arc::downgrade(target),
                    events,
                    data,
                    seen: 0,
                    seq: 0,
                    disabled: false,
                    armed: None,
                };
                st.registrations.insert(key, registration);
                st.keys.insert(id, key);
                REGISTRATIONS.fetch_add(1, Ordering::Relaxed);
                st.queue(key);
                drop(st);
                target.watch(Arc::downgrade(w), key);
            }
            epoll::CTL_MOD => {
                let key = *st.keys.get(&id).ok_or(ENOENT)?;
                // Exclusive wake-ups are for a registration as it is added,
                // as on Linux.
                if st.registrations.get(&key).is_some_and(|r| (events | r.events) & epoll::EXCLUSIVE != 0) {
                    return Err(EINVAL);
                }
                st.disarm(&w.port, key);
                if let Some(r) = st.registrations.get_mut(&key) {
                    r.events = events;
                    r.data = data;
                    r.seen = 0;
                    r.disabled = false;
                }
                st.queue(key);
                drop(st);
            }
            epoll::CTL_DEL => {
                let key = *st.keys.get(&id).ok_or(ENOENT)?;
                st.remove(&w.port, key);
                drop(st);
                target.unwatch(w, key);
                return Ok(0);
            }
            _ => return Err(EINVAL),
        }
        // A wait in progress looks at it too.
        let _ = w.kick.signal();
        Ok(0)
    }

    /// Waits until some registrations are ready, or `deadline` (monotonic
    /// ns; `None`: for ever) passes: at most `max` of them.
    fn wait(&self, max: usize, deadline: Option<u64>) -> Result<Vec<EpollEvent>, isize> {
        let mut packets = [PortPacket::default(); PACKETS];
        loop {
            // What the port has already is looked at with what is queued: a
            // level-triggered registration that stays ready would otherwise
            // keep those signalled meanwhile from being seen.
            if let Ok(n) = self.watcher.port.wait_packets(&mut packets, 0) {
                self.take(&packets[..n]);
            }
            let ready = self.collect(max);
            if !ready.is_empty() || deadline.is_some_and(|d| time::monotonic_ns() >= d) {
                return Ok(ready);
            }
            match crate::interrupt::port_wait(
                &self.watcher.port,
                &mut packets,
                deadline.unwrap_or(abi::DEADLINE_INFINITE),
            ) {
                Ok(n) => self.take(&packets[..n]),
                Err(abi::Error::TimedOut) => {}
                // A signal ends it, never to start again (as on Linux).
                Err(abi::Error::Interrupted) => return Err(EINTR),
                Err(e) => return Err(error::kernel(e)),
            }
        }
    }

    /// Takes in what the port says: whose objects signalled, and whether
    /// the layer saw something change (whose, the queue says already).
    fn take(&self, packets: &[PortPacket]) {
        let w = &self.watcher;
        let mut st = w.state.lock();
        for p in packets {
            if p.key == KICK {
                // Cleared before it is waited for again: a kick meanwhile
                // signals it anew.
                let _ = w.kick.clear();
                let _ = object::wait_async(w.kick.raw(), &w.port, KICK, SIGNALED);
            } else if let Some(r) = st.registrations.get_mut(&p.key) {
                r.armed = None;
                st.queue(p.key);
            }
        }
    }

    /// Looks at the registrations queued, in turn, until `max` are to be
    /// reported.
    fn collect(&self, max: usize) -> Vec<EpollEvent> {
        let w = &self.watcher;
        let mut st = w.state.lock();
        let mut ready = Vec::new();
        // Level-triggered and still ready: looked at in the next wait too.
        let mut again = Vec::new();
        for _ in 0..st.queue.len() {
            if ready.len() >= max {
                break;
            }
            let Some(key) = st.queue.pop_front() else { break };
            st.queued.remove(&key);
            if let Some(event) = look(w, &mut st, key, &mut again) {
                ready.push(event);
            }
        }
        for key in again {
            st.queue(key);
        }
        ready
    }
}

/// Looks at registration `key`: what to report of it, and otherwise what it
/// waits for. A level-triggered one that is reported goes to `again`.
fn look(w: &Watcher, st: &mut State, key: u64, again: &mut Vec<u64>) -> Option<EpollEvent> {
    let r = st.registrations.get_mut(&key)?;
    let Some(desc) = r.desc.upgrade() else {
        st.remove(&w.port, key);
        return None;
    };
    if r.disabled {
        return None;
    }
    // What it waited for no longer counts: what it is ready for now decides.
    if let Some(h) = r.armed.take() {
        let _ = object::wait_async_cancel(h, &w.port, key);
    }
    let wanted = r.events;
    let want_in = wanted & (epoll::IN | epoll::RDNORM | epoll::RDHUP) != 0;
    let want_out = wanted & (epoll::OUT | epoll::WRNORM) != 0;
    let now = io::readiness(&desc, want_in, want_out);
    let events = now.events & (wanted | epoll::ERR | epoll::HUP);
    let edge = wanted & epoll::ET != 0;
    let report = reported(wanted, events, r.seen, now.seq != r.seq);
    r.seen = events;
    let event = EpollEvent { events, data: r.data };
    if report {
        r.seq = now.seq;
        if wanted & epoll::ONESHOT != 0 {
            r.disabled = true;
            return Some(event);
        }
        if !edge {
            again.push(key);
            return Some(event);
        }
    }
    if let Some((what, signals)) = now.wait
        && let Some(h) = what.handle()
        && object::wait_async(h, &w.port, key, signals).is_ok()
    {
        r.armed = Some(h);
    }
    report.then_some(event)
}

/// Whether a registration that asked for `wanted` and is ready for `events`
/// now is reported: while it is ready if it is level-triggered; if it is
/// edge-triggered, when it is ready for something it was not at the last
/// look (`seen`), or it `changed` since it was last reported.
fn reported(wanted: u32, events: u32, seen: u32, changed: bool) -> bool {
    events != 0 && (wanted & epoll::ET == 0 || events & !seen != 0 || changed)
}

/// `epoll_create1` (and `epoll_create`).
pub fn epoll_create1(flags: u32) -> SysResult {
    if flags & !epoll::CLOEXEC != 0 {
        return Err(EINVAL);
    }
    let desc = Description::new(Object::Epoll(Epoll::new()?), crate::linux::o::RDWR);
    fd::insert(desc, flags & epoll::CLOEXEC != 0, 0).map(|n| n as usize)
}

/// `epoll_ctl`.
///
/// # Safety
/// `event` must be null or point at the program's `struct epoll_event`.
pub unsafe fn epoll_ctl(epfd: i32, op: u32, fd: i32, event: usize) -> SysResult {
    let ep = fd::get(epfd)?;
    let Object::Epoll(e) = &ep.object else { return Err(EINVAL) };
    let target = fd::get(fd)?;
    if Arc::ptr_eq(&ep, &target) {
        return Err(EINVAL);
    }
    let event = match op {
        epoll::CTL_DEL => EpollEvent::default(),
        // SAFETY: per the caller.
        _ => unsafe { user::read(event)? },
    };
    e.ctl(op, fd, &target, event)
}

/// `epoll_pwait2` (and `epoll_wait`, `epoll_pwait`): what is ready, into the
/// program's `events`, waiting until `deadline` (monotonic ns; `None`: for
/// ever) for something to be. The signal mask is not changed meanwhile, as
/// with `ppoll`: signals between threads end no wait but those for signals.
///
/// # Safety
/// `events` must have room for `max` `struct epoll_event`s.
pub unsafe fn epoll_wait(epfd: i32, events: usize, max: i32, deadline: Option<u64>) -> SysResult {
    let max = usize::try_from(max).ok().filter(|&m| (1..=MAX_EVENTS).contains(&m)).ok_or(EINVAL)?;
    let ep = fd::get(epfd)?;
    let Object::Epoll(e) = &ep.object else { return Err(EINVAL) };
    let ready = e.wait(max, deadline)?;
    for (i, event) in ready.iter().enumerate() {
        // SAFETY: per the caller.
        unsafe { user::write(events + i * size_of::<EpollEvent>(), *event)? };
    }
    Ok(ready.len())
}

/// The deadline of `epoll_wait`'s time-out in milliseconds (negative: none).
pub fn deadline_ms(ms: i32) -> Option<u64> {
    (ms >= 0).then(|| time::monotonic_ns().saturating_add(ms as u64 * 1_000_000))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linux::epoll::{ET, HUP, IN, OUT};

    #[test]
    fn level_triggered_registrations_are_reported_while_ready() {
        assert!(reported(IN, IN, IN, false));
        assert!(reported(IN | OUT, OUT, OUT, false));
        assert!(!reported(IN, 0, IN, true));
    }

    #[test]
    fn edge_triggered_ones_when_ready_anew_or_changed() {
        // Ready for the first time, or for something more.
        assert!(reported(ET | IN, IN, 0, false));
        assert!(reported(ET | IN | OUT, IN | OUT, IN, false));
        // A hang-up is news, whatever was asked.
        assert!(reported(ET | IN, IN | HUP, IN, false));
        // Still ready, and nothing changed: nothing to say.
        assert!(!reported(ET | IN, IN, IN, false));
        assert!(!reported(ET | IN | OUT, OUT, IN | OUT, false));
        // Ready, and changed since (an eventfd written to again, a program
        // met it not ready): reported again.
        assert!(reported(ET | IN, IN, IN, true));
        // Not ready: never.
        assert!(!reported(ET | IN, 0, 0, true));
    }

    #[test]
    fn events_are_laid_out_as_linux_has_them() {
        let e = EpollEvent { events: IN | ET, data: 0x1122_3344_5566_7788 };
        // SAFETY: plain old data of its size.
        let bytes: [u8; 12] = unsafe { core::mem::transmute(e) };
        assert_eq!(&bytes[..4], &(IN | ET).to_ne_bytes());
        assert_eq!(&bytes[4..], &0x1122_3344_5566_7788u64.to_ne_bytes());
    }
}
