// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Kernel synchronisation.
//!
//! # Concurrency model
//!
//! Veda uses a *big kernel lock* (BKL), like seL4 on SMP: a CPU must hold
//! the BKL whenever it executes kernel code that touches shared state. Kernel
//! code runs with interrupts disabled and is never preempted, so the kernel is
//! effectively single-threaded and kernel operations are short. User code runs
//! in parallel on all CPUs.
//!
//! The lock is acquired on every entry from user mode (system call, interrupt,
//! exception) and released on the way back. A context switch hands the lock
//! from the outgoing to the incoming thread.
//!
//! Individual objects additionally use [`SpinLock`] for interior mutability so
//! that the code stays sound by construction. Under the BKL those locks are
//! never contended; they detect accidental recursive locking on one CPU, which
//! would otherwise be a silent deadlock.
//!
//! * [`bkl`]: the big kernel lock;
//! * [`SpinLock`]: a lock of one object's state;
//! * [`Once`]: a value initialised once during boot, then read-only;
//! * [`futex`]: the kernel half of user-space mutexes and condition
//!   variables.

pub mod bkl;
pub mod futex;
mod once;
mod spinlock;

pub use once::Once;
pub use spinlock::{SpinLock, SpinLockGuard};

/// Owner value of an unlocked lock. Owners are stored as `cpu_id + 1` so that
/// a fresh lock is all zeroes (large lock-protected statics then live in .bss).
const NO_OWNER: u32 = 0;
