// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Spinlocks: the locks of individual objects' state, never contended
//! under the big kernel lock (see the [concurrency model](super)).

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use super::NO_OWNER;
use crate::scheduler::percpu;

/// A spinlock that panics on recursive acquisition by the same CPU.
pub struct SpinLock<T: ?Sized> {
    locked: AtomicBool,
    owner: AtomicU32,
    data: UnsafeCell<T>,
}

// SAFETY: the lock serialises all access to `data`.
unsafe impl<T: ?Sized + Send> Sync for SpinLock<T> {}
// SAFETY: as above.
unsafe impl<T: ?Sized + Send> Send for SpinLock<T> {}

impl<T> SpinLock<T> {
    pub const fn new(value: T) -> Self {
        SpinLock { locked: AtomicBool::new(false), owner: AtomicU32::new(NO_OWNER), data: UnsafeCell::new(value) }
    }
}

impl<T: ?Sized> SpinLock<T> {
    pub fn lock(&self) -> SpinLockGuard<'_, T> {
        let me = percpu::cpu_id_or_boot() + 1;
        loop {
            if self.locked.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_ok() {
                self.owner.store(me, Ordering::Relaxed);
                return SpinLockGuard { lock: self };
            }
            if self.owner.load(Ordering::Relaxed) == me {
                panic!("SpinLock: recursive lock on CPU {}", me - 1);
            }
            core::hint::spin_loop();
        }
    }

    /// The lock, if it is free now: for a panic's path, where its holder
    /// (a processor that stopped) may never let it go.
    pub fn try_lock(&self) -> Option<SpinLockGuard<'_, T>> {
        self.locked.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).ok()?;
        self.owner.store(percpu::cpu_id_or_boot() + 1, Ordering::Relaxed);
        Some(SpinLockGuard { lock: self })
    }
}

pub struct SpinLockGuard<'a, T: ?Sized> {
    lock: &'a SpinLock<T>,
}

impl<T: ?Sized> Deref for SpinLockGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: the guard proves exclusive access.
        unsafe { &*self.lock.data.get() }
    }
}

impl<T: ?Sized> DerefMut for SpinLockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: the guard proves exclusive access.
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T: ?Sized> Drop for SpinLockGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.owner.store(NO_OWNER, Ordering::Relaxed);
        self.lock.locked.store(false, Ordering::Release);
    }
}
