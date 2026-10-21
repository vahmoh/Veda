// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The big kernel lock: held by a CPU whenever it runs kernel code that
//! touches shared state (see the [concurrency model](super)).
//!
//! It is a queue lock (Mellor-Crummey and Scott's): CPUs take it in the
//! order they asked for it, so none waits for more than one turn of each of
//! the others, however busy they keep the kernel, and each waits spinning
//! on a word of its own, which only the CPU before it in the queue writes,
//! once, to hand the lock over. A lock that went to whichever CPU grabbed
//! it first would let some (above all the one that just let it go, which
//! still has it in its cache) take it again and again while another waited
//! for many milliseconds: a driver VM's processor, whose guest stops
//! meanwhile, or the window system. A ticket lock keeps the order, but all
//! its waiters spin on the one counter it hands the lock over with, which
//! every handover takes from all their caches and each then reads back:
//! the more CPUs wait, the longer each handover takes, while the holders'
//! turns stay as short, until the lock spends more time going from one CPU
//! to the next than held, and the kernel's work, done one CPU at a time,
//! falls behind with every CPU added.

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use super::NO_OWNER;
use crate::percpu::{self, MAX_CPUS};

/// A CPU's place in the queue, on a cache line of its own.
#[repr(align(64))]
struct Waiter {
    /// The CPU that queued after this one (its id + 1), once it has said
    /// so; 0 while none has.
    next: AtomicU32,
    /// Set as the CPU queues behind another, cleared by that one as it
    /// hands the lock over.
    waiting: AtomicBool,
}

static WAITERS: [Waiter; MAX_CPUS] =
    [const { Waiter { next: AtomicU32::new(0), waiting: AtomicBool::new(false) } }; MAX_CPUS];

/// A word on a cache line of its own.
#[repr(align(64))]
struct Line(AtomicU32);

/// The last CPU to queue (its id + 1), which holds the lock or waits for
/// it; 0 while no CPU does.
static TAIL: Line = Line(AtomicU32::new(0));
/// The CPU that holds the lock (its id + 1), for checks.
static OWNER: Line = Line(AtomicU32::new(NO_OWNER));

/// Acquires the BKL, after the CPUs that asked for it before. While
/// waiting, this CPU keeps servicing TLB shootdown requests so that the
/// holder can safely wait for them.
pub fn acquire() {
    debug_assert!(!crate::arch::cpu::interrupts_enabled(), "the BKL is taken with interrupts disabled");
    let me = percpu::cpu_id_or_boot() + 1;
    // Waiting for a turn of its own would never end.
    if OWNER.0.load(Ordering::Relaxed) == me {
        panic!("BKL acquired recursively on CPU {}", me - 1);
    }
    let waiter = &WAITERS[(me - 1) as usize];
    waiter.next.store(0, Ordering::Relaxed);
    waiter.waiting.store(true, Ordering::Relaxed);
    // AcqRel: what the last holder did is seen if it had already let the
    // lock go; this CPU's place is set before another can link behind it.
    let before = TAIL.0.swap(me, Ordering::AcqRel);
    if before != 0 {
        WAITERS[(before - 1) as usize].next.store(me, Ordering::Release);
        // Acquire: what the CPU before did is seen once it has handed over.
        while waiter.waiting.load(Ordering::Acquire) {
            crate::arch::tlb::service_pending();
            core::hint::spin_loop();
        }
    }
    OWNER.0.store(me, Ordering::Relaxed);
}

pub fn release() {
    debug_assert!(held_by_me(), "BKL released by a CPU that does not hold it");
    let me = percpu::cpu_id_or_boot() + 1;
    OWNER.0.store(NO_OWNER, Ordering::Relaxed);
    let waiter = &WAITERS[(me - 1) as usize];
    let mut next = waiter.next.load(Ordering::Acquire);
    if next == 0 {
        // Release: what this CPU did is seen by the next to take it.
        if TAIL.0.compare_exchange(me, 0, Ordering::Release, Ordering::Relaxed).is_ok() {
            return;
        }
        // A CPU is queueing behind this one: it says so at once (it runs
        // with interrupts disabled, between two instructions).
        loop {
            next = waiter.next.load(Ordering::Acquire);
            if next != 0 {
                break;
            }
            core::hint::spin_loop();
        }
    }
    // Release: what this CPU did is seen by the next.
    WAITERS[(next - 1) as usize].waiting.store(false, Ordering::Release);
}

pub fn held_by_me() -> bool {
    OWNER.0.load(Ordering::Relaxed) == percpu::cpu_id_or_boot() + 1
}

/// Lets the other CPUs into the kernel for a moment in the middle of a
/// long operation (committing, zeroing or copying many pages), so that
/// no system call holds the lock for a time a process chooses: every CPU
/// waiting for the lock takes its turn, then this one takes it again. Only
/// between steps that keep no reference into state that another CPU may
/// change meanwhile, and with no other lock held (a CPU in the kernel may
/// wait for that one while holding this).
pub fn relax() {
    // No CPU has queued behind this one: no one waits.
    if TAIL.0.load(Ordering::Relaxed) == percpu::cpu_id_or_boot() + 1 {
        return;
    }
    release();
    acquire();
}
