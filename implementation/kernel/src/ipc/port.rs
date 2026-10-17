// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Ports: where the signals of many objects are waited for at once.
//!
//! A thread registers interest in an object's signals with a port
//! (`object_wait_async`): once any of them is active, the port gets a
//! packet (the key the registration was made with, and the signals active
//! then), and the registration is done, so that each yields one packet;
//! the waiter registers again for more. A registration whose signals are
//! active already yields its packet at once. A thread waits on the port
//! (`port_wait`) and takes the packets queued: a wait costs what is ready,
//! not how many objects are watched, as `object_wait_many`'s does.
//!
//! A registration is an [`Observer`] on the object's [`Signals`], which
//! holds the port weakly: a port gone leaves its observers to go when they
//! fire, when their objects go, or when the objects they watch drop the
//! dead ones. A port holds at most `vabi::PORT_MAX_REGISTRATIONS`
//! registrations, those waiting and those done whose packets are not yet
//! taken (a registration's place passes to its packet), so that what it
//! keeps in the kernel is bounded.

use alloc::collections::VecDeque;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};

use vabi::PortPacket;
use vabi::signals::READABLE;

use crate::object::Signals;
use crate::synchronization::SpinLock;

pub struct Port {
    pub koid: u64,
    /// `READABLE` while it holds packets.
    pub signals: Signals,
    packets: SpinLock<VecDeque<PortPacket>>,
    /// The registrations it holds: waiting, or done with their packets
    /// queued.
    held: AtomicUsize,
}

impl Port {
    pub fn new() -> Arc<Port> {
        Arc::new(Port {
            koid: crate::object::new_koid(),
            signals: Signals::new(0),
            packets: SpinLock::new(VecDeque::new()),
            held: AtomicUsize::new(0),
        })
    }

    /// Takes a registration's place, if the port has one left.
    pub fn reserve(&self) -> bool {
        self.held
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| (n < vabi::PORT_MAX_REGISTRATIONS).then_some(n + 1))
            .is_ok()
    }

    fn release(&self, n: usize) {
        self.held.fetch_sub(n, Ordering::Relaxed);
    }

    /// Queues a packet in a place [`Port::reserve`]d for it (a
    /// registration's, done at once or when it fires).
    pub fn queue(&self, key: u64, observed: u32) {
        let mut q = self.packets.lock();
        q.push_back(PortPacket { key, observed, _reserved: 0 });
        drop(q);
        self.signals.update(0, READABLE);
    }

    /// Takes up to `max` packets, oldest first; their places are free.
    pub fn take(&self, max: usize) -> Vec<PortPacket> {
        let mut q = self.packets.lock();
        let n = max.min(q.len());
        let out: Vec<PortPacket> = q.drain(..n).collect();
        let empty = q.is_empty();
        drop(q);
        self.release(out.len());
        if empty {
            self.signals.update(READABLE, 0);
        }
        out
    }

    /// Puts packets taken back in front (they could not be given), in
    /// their order; they take their places again.
    pub fn put_back(&self, packets: Vec<PortPacket>) {
        if packets.is_empty() {
            return;
        }
        self.held.fetch_add(packets.len(), Ordering::Relaxed);
        let mut q = self.packets.lock();
        for p in packets.into_iter().rev() {
            q.push_front(p);
        }
        drop(q);
        self.signals.update(0, READABLE);
    }

    /// Drops the packets queued for `key` (its registration was undone).
    pub fn purge(&self, key: u64) {
        let mut q = self.packets.lock();
        let before = q.len();
        q.retain(|p| p.key != key);
        let gone = before - q.len();
        let empty = q.is_empty();
        drop(q);
        self.release(gone);
        if gone > 0 && empty {
            self.signals.update(READABLE, 0);
        }
    }
}

/// A registration: a packet for `port`, with `key`, once any of `mask` is
/// active on the object whose [`Signals`] hold it. It holds a place of the
/// port's, which it gives back if it goes without firing.
pub struct Observer {
    port: Weak<Port>,
    pub key: u64,
    pub mask: u32,
    /// It holds its place (not yet given to its packet).
    holds: bool,
}

impl Observer {
    /// A registration for `port`, in a place [`Port::reserve`]d for it.
    pub fn new(port: &Arc<Port>, key: u64, mask: u32) -> Observer {
        Observer { port: Arc::downgrade(port), key, mask, holds: true }
    }

    /// Whether it is `port`'s, with `key`.
    pub fn is(&self, port: &Arc<Port>, key: u64) -> bool {
        self.key == key && Weak::ptr_eq(&self.port, &Arc::downgrade(port))
    }

    /// Whether its port is still there.
    pub fn alive(&self) -> bool {
        self.port.strong_count() > 0
    }

    /// The registration is done: its port gets its packet, in its place.
    pub fn fire(mut self, observed: u32) {
        self.holds = false;
        if let Some(port) = self.port.upgrade() {
            port.queue(self.key, observed);
        }
    }
}

impl Drop for Observer {
    fn drop(&mut self) {
        if self.holds
            && let Some(port) = self.port.upgrade()
        {
            port.release(1);
        }
    }
}
