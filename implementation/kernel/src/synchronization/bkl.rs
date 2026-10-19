// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The big kernel lock: held by a CPU whenever it runs kernel code that
//! touches shared state (see the [concurrency model](super)).
//!
//! It is a ticket lock: CPUs take it in the order they asked for it, so
//! none waits for more than one turn of each of the others, however busy
//! they keep the kernel. A lock that went to whichever CPU grabbed it
//! first would let some (above all the one that just let it go, which
//! still has it in its cache) take it again and again while another waited
//! for many milliseconds: a driver VM's processor, whose guest stops
//! meanwhile, or the window system.

use core::sync::atomic::{AtomicU32, Ordering};

use super::NO_OWNER;
use crate::percpu;

/// The next ticket to be handed out.
static NEXT: AtomicU32 = AtomicU32::new(0);
/// The ticket whose CPU holds the lock, or may take it while none does.
/// Only the holder changes it.
static SERVING: AtomicU32 = AtomicU32::new(0);
static OWNER: AtomicU32 = AtomicU32::new(NO_OWNER);

/// Acquires the BKL, after the CPUs that asked for it before. While
/// waiting, this CPU keeps servicing TLB shootdown requests so that the
/// holder can safely wait for them.
pub fn acquire() {
    debug_assert!(!crate::arch::cpu::interrupts_enabled(), "the BKL is taken with interrupts disabled");
    let me = percpu::cpu_id_or_boot() + 1;
    // Waiting for a ticket of its own would never end.
    if OWNER.load(Ordering::Relaxed) == me {
        panic!("BKL acquired recursively on CPU {}", me - 1);
    }
    let ticket = NEXT.fetch_add(1, Ordering::Relaxed);
    // Acquire: what the last holder did is seen once its turn has passed.
    while SERVING.load(Ordering::Acquire) != ticket {
        crate::arch::tlb::service_pending();
        core::hint::spin_loop();
    }
    OWNER.store(me, Ordering::Relaxed);
}

pub fn release() {
    debug_assert!(held_by_me(), "BKL released by a CPU that does not hold it");
    OWNER.store(NO_OWNER, Ordering::Relaxed);
    // Release: what this CPU did is seen by the next ticket's.
    SERVING.store(SERVING.load(Ordering::Relaxed).wrapping_add(1), Ordering::Release);
}

pub fn held_by_me() -> bool {
    OWNER.load(Ordering::Relaxed) == percpu::cpu_id_or_boot() + 1
}

/// Lets the other CPUs into the kernel for a moment in the middle of a
/// long operation (committing, zeroing or copying many pages), so that
/// no system call holds the lock for a time a process chooses: every CPU
/// waiting for the lock takes its turn, then this one takes it again. Only
/// between steps that keep no reference into state that another CPU may
/// change meanwhile, and with no other lock held (a CPU in the kernel may
/// wait for that one while holding this).
pub fn relax() {
    // No ticket but this CPU's is out: no one waits.
    if NEXT.load(Ordering::Relaxed) == SERVING.load(Ordering::Relaxed).wrapping_add(1) {
        return;
    }
    release();
    acquire();
}
