// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Kernel objects, handles and signals.
//!
//! Every object user space can reach is a [`KObject`] stored in a process's
//! [`handle::HandleTable`]. Waitable objects carry a [`Signals`] word; threads
//! wait for signal bits with `object_wait_one/many`, or have a [`port`] told
//! (`object_wait_async`).

pub mod channel;
pub mod event;
pub mod handle;
pub mod interrupt;
pub mod ioport;
pub mod port;
pub mod process;
pub mod resource;
pub mod socket;

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use vabi::ObjectType;

use crate::mm::vmo::Vmo;
use crate::sched::{self, Thread, WakeReason};
use crate::sync::SpinLock;

static NEXT_KOID: AtomicU64 = AtomicU64::new(1);

/// Allocates a kernel object id (unique for the lifetime of the system).
pub fn new_koid() -> u64 {
    NEXT_KOID.fetch_add(1, Ordering::Relaxed)
}

/// A short, fixed-capacity name (processes and threads).
#[derive(Clone, Copy)]
pub struct Name {
    bytes: [u8; vabi::NAME_MAX],
    len: u8,
}

impl Name {
    pub fn new(s: &str) -> Name {
        let mut bytes = [0u8; vabi::NAME_MAX];
        // Truncate on a char boundary.
        let mut len = s.len().min(vabi::NAME_MAX);
        while !s.is_char_boundary(len) {
            len -= 1;
        }
        bytes[..len].copy_from_slice(&s.as_bytes()[..len]);
        Name { bytes, len: len as u8 }
    }

    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..self.len as usize]).unwrap_or("?")
    }

    pub fn raw(&self) -> [u8; vabi::NAME_MAX] {
        self.bytes
    }
}

struct Waiter {
    id: u64,
    thread: Arc<Thread>,
    mask: u32,
}

struct SignalsInner {
    active: u32,
    waiters: Vec<Waiter>,
    /// Ports' registrations ([`port::Observer`]).
    observers: Vec<port::Observer>,
}

/// The signal word of a waitable object and the threads waiting on it.
pub struct Signals {
    inner: SpinLock<SignalsInner>,
}

impl Signals {
    pub const fn new(initial: u32) -> Signals {
        Signals { inner: SpinLock::new(SignalsInner { active: initial, waiters: Vec::new(), observers: Vec::new() }) }
    }

    pub fn active(&self) -> u32 {
        self.inner.lock().active
    }

    /// Clears then sets bits, waking threads interested in the new state
    /// and firing the ports' registrations it satisfies (once the signals'
    /// lock is let go: a port's own signals change as it gets a packet).
    pub fn update(&self, clear: u32, set: u32) {
        let mut to_wake: Vec<Arc<Thread>> = Vec::new();
        let mut fired: Vec<port::Observer> = Vec::new();
        let active;
        {
            let mut s = self.inner.lock();
            let old = s.active;
            s.active = (s.active & !clear) | set;
            active = s.active;
            let newly = s.active & !old;
            if newly != 0 {
                for w in s.waiters.iter().filter(|w| w.mask & s.active != 0) {
                    to_wake.push(w.thread.clone());
                }
                if s.observers.iter().any(|o| o.mask & active != 0) {
                    let (now, later) =
                        core::mem::take(&mut s.observers).into_iter().partition(|o| o.mask & active != 0);
                    s.observers = later;
                    fired = now;
                }
            }
        }
        for t in to_wake {
            sched::wake(&t, WakeReason::Signaled);
        }
        for o in fired {
            o.fire(active);
        }
    }

    /// Adds a port's registration: it fires at once if any of its signals
    /// is active (`Err` gives it back to be fired), when one becomes so
    /// otherwise. Registrations whose ports are gone are dropped as the
    /// list grows, so that they cannot pile up on an object that lives.
    pub fn observe(&self, o: port::Observer) -> Result<(), (port::Observer, u32)> {
        let mut s = self.inner.lock();
        if o.mask & s.active != 0 {
            let active = s.active;
            return Err((o, active));
        }
        if s.observers.len() >= 16 && s.observers.len().is_power_of_two() {
            s.observers.retain(port::Observer::alive);
        }
        s.observers.push(o);
        Ok(())
    }

    /// Undoes the registrations of `port` with `key`.
    pub fn forget(&self, port: &Arc<port::Port>, key: u64) {
        let gone: Vec<port::Observer> = {
            let mut s = self.inner.lock();
            let (gone, kept) = core::mem::take(&mut s.observers).into_iter().partition(|o| o.is(port, key));
            s.observers = kept;
            gone
        };
        // Their places go back to the port once the lock is let go.
        drop(gone);
    }

    pub fn add_waiter(&self, id: u64, thread: Arc<Thread>, mask: u32) {
        self.inner.lock().waiters.push(Waiter { id, thread, mask });
    }

    pub fn remove_waiter(&self, id: u64) {
        self.inner.lock().waiters.retain(|w| w.id != id);
    }
}

/// A reference to any kernel object.
#[derive(Clone)]
pub enum KObject {
    Process(Arc<process::Process>),
    Thread(Arc<Thread>),
    Channel(Arc<channel::ChannelEnd>),
    Socket(Arc<socket::SocketEnd>),
    Event(Arc<event::Event>),
    Vmo(Arc<Vmo>),
    Interrupt(Arc<interrupt::Interrupt>),
    IoPorts(Arc<ioport::IoPorts>),
    Resource(Arc<resource::Resource>),
    Guest(Arc<crate::hv::Guest>),
    Vcpu(Arc<crate::hv::Vcpu>),
    Port(Arc<port::Port>),
}

impl KObject {
    pub fn object_type(&self) -> ObjectType {
        match self {
            KObject::Process(_) => ObjectType::Process,
            KObject::Thread(_) => ObjectType::Thread,
            KObject::Channel(_) => ObjectType::Channel,
            KObject::Socket(_) => ObjectType::Socket,
            KObject::Event(_) => ObjectType::Event,
            KObject::Vmo(_) => ObjectType::Vmo,
            KObject::Interrupt(_) => ObjectType::Interrupt,
            KObject::IoPorts(_) => ObjectType::IoPorts,
            KObject::Resource(_) => ObjectType::Resource,
            KObject::Guest(_) => ObjectType::Guest,
            KObject::Vcpu(_) => ObjectType::Vcpu,
            KObject::Port(_) => ObjectType::Port,
        }
    }

    pub fn koid(&self) -> u64 {
        match self {
            KObject::Process(o) => o.koid,
            KObject::Thread(o) => o.koid,
            KObject::Channel(o) => o.koid(),
            KObject::Socket(o) => o.koid(),
            KObject::Event(o) => o.koid,
            KObject::Vmo(o) => o.koid,
            KObject::Interrupt(o) => o.koid,
            KObject::IoPorts(o) => o.koid,
            KObject::Resource(o) => o.koid,
            KObject::Guest(o) => o.koid,
            KObject::Vcpu(o) => o.koid,
            KObject::Port(o) => o.koid,
        }
    }

    /// The object's signal word, if it is waitable.
    pub fn signals(&self) -> Option<&Signals> {
        match self {
            KObject::Process(o) => Some(&o.signals),
            KObject::Thread(o) => Some(&o.signals),
            KObject::Channel(o) => Some(o.signals()),
            KObject::Socket(o) => Some(o.signals()),
            KObject::Event(o) => Some(&o.signals),
            KObject::Interrupt(o) => Some(&o.signals),
            KObject::Port(o) => Some(&o.signals),
            KObject::Vmo(_) | KObject::IoPorts(_) | KObject::Resource(_) | KObject::Guest(_) | KObject::Vcpu(_) => None,
        }
    }
}
