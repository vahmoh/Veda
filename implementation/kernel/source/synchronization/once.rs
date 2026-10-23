// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Values initialised once during boot, then read without a lock.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicU8, Ordering};

/// A value initialised exactly once during boot, then read-only.
pub struct Once<T> {
    /// [`EMPTY`](Self::EMPTY), [`WRITING`](Self::WRITING) or
    /// [`READY`](Self::READY).
    state: AtomicU8,
    value: UnsafeCell<Option<T>>,
}

// SAFETY: written once, by the one caller that claimed it, before `READY`
// is published; read-only afterwards.
unsafe impl<T: Send + Sync> Sync for Once<T> {}

impl<T> Once<T> {
    const EMPTY: u8 = 0;
    const WRITING: u8 = 1;
    const READY: u8 = 2;

    pub const fn new() -> Self {
        Once { state: AtomicU8::new(Self::EMPTY), value: UnsafeCell::new(None) }
    }

    /// Stores the value. Panics if called twice.
    pub fn set(&self, value: T) {
        let claimed = self.state.compare_exchange(Self::EMPTY, Self::WRITING, Ordering::Acquire, Ordering::Relaxed);
        assert!(claimed.is_ok(), "Once::set called twice");
        // SAFETY: this caller alone moved the state from EMPTY, and no
        // reader looks at the value before it is READY.
        unsafe { *self.value.get() = Some(value) };
        self.state.store(Self::READY, Ordering::Release);
    }

    pub fn get(&self) -> Option<&T> {
        if self.state.load(Ordering::Acquire) == Self::READY {
            // SAFETY: published and never mutated again.
            unsafe { (*self.value.get()).as_ref() }
        } else {
            None
        }
    }

    /// Returns the value, panicking if it was not initialised.
    pub fn expect(&self) -> &T {
        self.get().expect("Once value used before initialisation")
    }
}
