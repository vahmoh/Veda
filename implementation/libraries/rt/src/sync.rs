// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Synchronisation primitives for user space.
//!
//! [`Mutex`], [`Condvar`] and [`Once`] are futex-based: uncontended
//! operations never enter the kernel, and a waiter sleeps in the kernel
//! rather than spinning, so it never holds up the thread it waits for
//! (a lower-priority holder that was preempted gets its processor back).

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicU32, Ordering};

use abi::{Error, nr};

use crate::sys::call;

fn futex_wait(word: &AtomicU32, expected: u32, deadline: u64) -> Result<(), Error> {
    call(nr::FUTEX_WAIT, [word as *const AtomicU32 as usize, expected as usize, deadline as usize, 0, 0, 0]).map(|_| ())
}

fn futex_wake(word: &AtomicU32, count: usize) {
    let _ = call(nr::FUTEX_WAKE, [word as *const AtomicU32 as usize, count, 0, 0, 0, 0]);
}

/// A futex-based mutex (states: 0 unlocked, 1 locked, 2 locked + waiters).
pub struct Mutex<T: ?Sized> {
    state: AtomicU32,
    data: UnsafeCell<T>,
}

// SAFETY: the lock serialises access.
unsafe impl<T: ?Sized + Send> Sync for Mutex<T> {}
// SAFETY: as above.
unsafe impl<T: ?Sized + Send> Send for Mutex<T> {}

impl<T> Mutex<T> {
    /// An unlocked mutex holding `v`.
    pub const fn new(v: T) -> Self {
        Mutex { state: AtomicU32::new(0), data: UnsafeCell::new(v) }
    }

    /// The value, taken out of the mutex.
    pub fn into_inner(self) -> T {
        self.data.into_inner()
    }
}

impl<T: ?Sized> Mutex<T> {
    /// Locks the mutex, waiting (in the kernel, after a short spin) while
    /// another thread holds it. Not reentrant: a thread that locks a mutex
    /// it holds waits forever.
    pub fn lock(&self) -> MutexGuard<'_, T> {
        if self.state.compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed).is_err() {
            self.lock_slow();
        }
        MutexGuard { mutex: self }
    }

    #[cold]
    fn lock_slow(&self) {
        let mut spins = 0;
        loop {
            // Spin briefly before sleeping.
            if spins < 40 {
                if self.state.compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed).is_ok() {
                    return;
                }
                spins += 1;
                core::hint::spin_loop();
                continue;
            }
            if self.state.swap(2, Ordering::Acquire) == 0 {
                return;
            }
            let _ = futex_wait(&self.state, 2, abi::DEADLINE_INFINITE);
        }
    }

    /// Locks the mutex if no other thread holds it.
    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        self.state.compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed).ok().map(|_| MutexGuard { mutex: self })
    }

    fn unlock(&self) {
        if self.state.swap(0, Ordering::Release) == 2 {
            futex_wake(&self.state, 1);
        }
    }

    /// The value, through the exclusive reference (no locking needed).
    pub fn get_mut(&mut self) -> &mut T {
        self.data.get_mut()
    }
}

impl<T: Default> Default for Mutex<T> {
    fn default() -> Self {
        Mutex::new(T::default())
    }
}

/// Access to a [`Mutex`]'s value while it is locked; unlocks when dropped.
pub struct MutexGuard<'a, T: ?Sized> {
    mutex: &'a Mutex<T>,
}

impl<T: ?Sized> Deref for MutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: the guard proves exclusive access.
        unsafe { &*self.mutex.data.get() }
    }
}

impl<T: ?Sized> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: the guard proves exclusive access.
        unsafe { &mut *self.mutex.data.get() }
    }
}

impl<T: ?Sized> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        self.mutex.unlock();
    }
}

/// A condition variable for use with [`Mutex`].
///
/// A notification wakes the threads that wait at the time; a thread that
/// changes what they wait for must do so with the mutex held, as with any
/// condition variable. `waiters` counts the threads between registering
/// to wait and waking, so that notifying nobody makes no system call.
pub struct Condvar {
    /// Bumped by every notification: the futex word waiters sleep on.
    seq: AtomicU32,
    /// Threads waiting, or about to.
    waiters: AtomicU32,
}

impl Default for Condvar {
    fn default() -> Self {
        Self::new()
    }
}

impl Condvar {
    /// A condition variable nobody waits on.
    pub const fn new() -> Self {
        Condvar { seq: AtomicU32::new(0), waiters: AtomicU32::new(0) }
    }

    /// Atomically releases `guard`, waits for a notification (or `deadline`),
    /// and re-acquires the lock. Returns `false` on timeout.
    pub fn wait_until<'a, T: ?Sized>(&self, guard: MutexGuard<'a, T>, deadline: u64) -> (MutexGuard<'a, T>, bool) {
        // Both happen with the mutex held, before it is released below: a
        // notifier that changes the condition locks the mutex after that
        // (or before this thread took it, and then this thread saw the
        // change and does not wait), so its unlock-to-lock ordering makes
        // both visible to it. It then sees `waiters` counting this thread,
        // and bumps `seq` past the value read here, so the kernel either
        // finds the word changed and returns at once or has this thread
        // queued when the wake comes. Hence `Relaxed` suffices: the mutex
        // orders everything.
        self.waiters.fetch_add(1, Ordering::Relaxed);
        let seq = self.seq.load(Ordering::Relaxed);
        let mutex = guard.mutex;
        drop(guard);
        let r = futex_wait(&self.seq, seq, deadline);
        self.waiters.fetch_sub(1, Ordering::Relaxed);
        (mutex.lock(), r != Err(Error::TimedOut))
    }

    /// Atomically releases `guard`, waits for a notification and
    /// re-acquires the lock (spurious wake-ups are possible: wait in a loop
    /// that checks the condition).
    pub fn wait<'a, T: ?Sized>(&self, guard: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
        self.wait_until(guard, abi::DEADLINE_INFINITE).0
    }

    /// Wakes one waiting thread, if any (no system call if none waits).
    pub fn notify_one(&self) {
        self.notify(1);
    }

    /// Wakes every waiting thread (no system call if none waits).
    pub fn notify_all(&self) {
        self.notify(usize::MAX >> 1);
    }

    fn notify(&self, count: usize) {
        // See `wait_until` for why a waiter that matters is counted by now;
        // the bump, before the wake, is what makes a waiter that has not
        // reached the kernel yet return at once.
        self.seq.fetch_add(1, Ordering::Relaxed);
        if self.waiters.load(Ordering::Relaxed) != 0 {
            futex_wake(&self.seq, count);
        }
    }
}

/// One-time initialisation.
pub struct Once {
    /// [`Once::NEW`], [`Once::RUNNING`], [`Once::WAITED`] or [`Once::DONE`].
    state: AtomicU32,
}

impl Default for Once {
    fn default() -> Self {
        Self::new()
    }
}

impl Once {
    /// Nothing has run.
    const NEW: u32 = 0;
    /// A thread runs the initialisation.
    const RUNNING: u32 = 1;
    /// A thread runs the initialisation and others wait for it.
    const WAITED: u32 = 2;
    /// The initialisation has run.
    const DONE: u32 = 3;

    /// A `Once` that has not run.
    pub const fn new() -> Self {
        Once { state: AtomicU32::new(Self::NEW) }
    }

    /// Runs `f` if no call has yet, or waits for the call that runs it to
    /// finish. Only waiting enters the kernel (and waking the waiters).
    pub fn call_once(&self, f: impl FnOnce()) {
        if self.state.load(Ordering::Acquire) == Self::DONE {
            return;
        }
        if self.state.compare_exchange(Self::NEW, Self::RUNNING, Ordering::Acquire, Ordering::Acquire).is_ok() {
            f();
            // Release: what `f` did is seen by whoever sees `DONE`.
            if self.state.swap(Self::DONE, Ordering::Release) == Self::WAITED {
                futex_wake(&self.state, usize::MAX >> 1);
            }
            return;
        }
        loop {
            match self.state.load(Ordering::Acquire) {
                Self::DONE => return,
                // Say that someone waits, then wait while it is so (the
                // runner's swap to `DONE` makes the kernel return at once if
                // it came first).
                Self::RUNNING => {
                    let _ =
                        self.state.compare_exchange(Self::RUNNING, Self::WAITED, Ordering::Relaxed, Ordering::Relaxed);
                }
                _ => {
                    let _ = futex_wait(&self.state, Self::WAITED, abi::DEADLINE_INFINITE);
                }
            }
        }
    }
}

/// A value computed on first access.
pub struct Lazy<T, F = fn() -> T> {
    once: Once,
    init: UnsafeCell<Option<F>>,
    value: UnsafeCell<Option<T>>,
}

// SAFETY: initialisation is serialised by `Once`; afterwards the value is
// only read.
unsafe impl<T: Send + Sync, F: Send> Sync for Lazy<T, F> {}

impl<T, F: FnOnce() -> T> Lazy<T, F> {
    /// A value `f` computes when it is first needed.
    pub const fn new(f: F) -> Self {
        Lazy { once: Once::new(), init: UnsafeCell::new(Some(f)), value: UnsafeCell::new(None) }
    }

    /// The value, computed now if this is the first access.
    pub fn get(&self) -> &T {
        self.once.call_once(|| {
            // SAFETY: only one caller runs this closure.
            let f = unsafe { (*self.init.get()).take() }.expect("Lazy initialiser already taken");
            // SAFETY: as above.
            unsafe { *self.value.get() = Some(f()) };
        });
        // SAFETY: initialised and never mutated again.
        unsafe { (*self.value.get()).as_ref().unwrap() }
    }
}

impl<T, F: FnOnce() -> T> Deref for Lazy<T, F> {
    type Target = T;
    fn deref(&self) -> &T {
        self.get()
    }
}
