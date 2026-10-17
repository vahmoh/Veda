// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The big kernel lock: held by a CPU whenever it runs kernel code that
//! touches shared state (see the [concurrency model](super)).

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use super::NO_OWNER;
use crate::percpu;

static LOCKED: AtomicBool = AtomicBool::new(false);
static OWNER: AtomicU32 = AtomicU32::new(NO_OWNER);

/// Acquires the BKL. While spinning, this CPU keeps servicing TLB
/// shootdown requests so that the holder can safely wait for them.
pub fn acquire() {
    debug_assert!(!crate::arch::cpu::interrupts_enabled(), "the BKL is taken with interrupts disabled");
    let me = percpu::cpu_id_or_boot() + 1;
    loop {
        if LOCKED.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_ok() {
            OWNER.store(me, Ordering::Relaxed);
            return;
        }
        if OWNER.load(Ordering::Relaxed) == me {
            panic!("BKL acquired recursively on CPU {}", me - 1);
        }
        crate::arch::tlb::service_pending();
        core::hint::spin_loop();
    }
}

pub fn release() {
    debug_assert!(held_by_me(), "BKL released by a CPU that does not hold it");
    OWNER.store(NO_OWNER, Ordering::Relaxed);
    LOCKED.store(false, Ordering::Release);
}

pub fn held_by_me() -> bool {
    LOCKED.load(Ordering::Relaxed) && OWNER.load(Ordering::Relaxed) == percpu::cpu_id_or_boot() + 1
}

/// Lets the other CPUs into the kernel for a moment in the middle of a
/// long operation (committing, zeroing or copying many pages), so that
/// no system call holds the lock for a time a process chooses: releases
/// the lock, gives a CPU that waits for it the chance to take it, and
/// takes it again. Only between steps that keep no reference into state
/// that another CPU may change meanwhile, and with no other lock held
/// (a CPU in the kernel may wait for that one while holding this).
pub fn relax() {
    release();
    // The lock is not fair: taken back at once, it would usually stay
    // here.
    for _ in 0..256 {
        if LOCKED.load(Ordering::Relaxed) {
            break;
        }
        core::hint::spin_loop();
    }
    acquire();
}
