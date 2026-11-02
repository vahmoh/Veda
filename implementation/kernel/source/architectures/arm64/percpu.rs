// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What ARM64 keeps for each CPU, reached through `TPIDR_EL1` while in the
//! kernel: the stack the entry code takes for the kernel's own faults, at a
//! fixed offset, the CPU's index, its affinity and its GIC redistributor,
//! its requests for memory barriers, and the tables it has loaded, which
//! say whether a barrier concerns it.
//!
//! `TPIDR_EL1` stays this register at EL2 too (the names VHE redirects are
//! not it), where the kernel uses it as such, and it is a guest's while a
//! guest runs: what is reached there through `TPIDR_EL2` instead.

use core::mem::{MaybeUninit, offset_of};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::cpu::{read_register, write_register};
use crate::scheduler::percpu::MAX_CPUS;

/// Offset used by assembly.
pub(in crate::architecture) const OFF_FAULT_STACK: usize = 0;

#[repr(C, align(64))]
pub(in crate::architecture) struct Local {
    /// Top of the stack the kernel's own faults run on (it may be its
    /// stack that faulted).
    pub fault_stack: u64,
    /// The CPU's index (`crate::scheduler::percpu`).
    pub cpu_id: u32,
    /// Its affinity, as `MPIDR_EL1` has it and the GIC addresses it.
    pub affinity: u64,
    /// Set by another CPU that wants this one to go through a memory
    /// barrier (`tlb::barrier_space`), which acknowledging it is.
    pub barrier_pending: AtomicBool,
    /// Acknowledgement counter for those.
    pub barrier_done: AtomicU64,
    /// The tables this CPU has loaded (`paging::activate`). Changed under
    /// the BKL.
    pub loaded_root: AtomicU64,
}

const _: () = assert!(offset_of!(Local, fault_stack) == OFF_FAULT_STACK);

// SAFETY: each Local is mutated only by its own CPU (or before it runs);
// cross-CPU fields are atomics.
unsafe impl Sync for Local {}

const fn new_local(id: u32) -> Local {
    Local {
        fault_stack: 0,
        cpu_id: id,
        affinity: 0,
        barrier_pending: AtomicBool::new(false),
        barrier_done: AtomicU64::new(0),
        loaded_root: AtomicU64::new(0),
    }
}

struct LocalArray([Local; MAX_CPUS]);
// SAFETY: see `Local`.
unsafe impl Sync for LocalArray {}

static LOCAL: LocalArray = {
    let mut i = 0;
    // `[expr; N]` needs Copy, so build the array element by element.
    let mut arr: [MaybeUninit<Local>; MAX_CPUS] = [const { MaybeUninit::uninit() }; MAX_CPUS];
    while i < MAX_CPUS {
        arr[i] = MaybeUninit::new(new_local(i as u32));
        i += 1;
    }
    // SAFETY: every element was initialised above.
    LocalArray(unsafe { core::mem::transmute::<[MaybeUninit<Local>; MAX_CPUS], [Local; MAX_CPUS]>(arr) })
};

static READY: AtomicBool = AtomicBool::new(false);

/// Points this CPU's `TPIDR_EL1` at its block. Must run before anything
/// else touches per-CPU state.
///
/// # Safety
/// Called once per CPU, with `id` unique.
pub(in crate::architecture) unsafe fn install(id: usize) {
    let p = &LOCAL.0[id];
    let ptr = p as *const Local as *mut Local;
    // SAFETY: this CPU is the only user of its slot during bring-up.
    unsafe {
        (*ptr).affinity = super::cpu::affinity();
        write_register!("tpidr_el1", ptr);
    }
    READY.store(true, Ordering::Release);
}

/// The current CPU's block.
#[inline]
pub(in crate::architecture) fn current() -> &'static Local {
    // SAFETY: `TPIDR_EL1` points at our Local while in the kernel, a static.
    unsafe { &*(read_register!("tpidr_el1") as *const Local) }
}

/// The current CPU's index (once [`install`]ed).
#[inline]
pub fn current_id() -> usize {
    current().cpu_id as usize
}

/// Records the stack this CPU's faults run on.
///
/// # Safety
/// Only this CPU's bring-up code.
pub(in crate::architecture) unsafe fn set_fault_stack(id: usize, top: u64) {
    let p = &LOCAL.0[id] as *const Local as *mut Local;
    // SAFETY: guaranteed by the caller.
    unsafe { (*p).fault_stack = top };
}

/// Current CPU index, or 0 before per-CPU data is installed.
#[inline]
pub fn cpu_id_or_boot() -> u32 {
    if !READY.load(Ordering::Relaxed) {
        return 0;
    }
    current_id() as u32
}

/// Block of CPU `id`.
pub(in crate::architecture) fn get(id: usize) -> &'static Local {
    &LOCAL.0[id]
}

/// The blocks of all CPUs that have come online.
pub(in crate::architecture) fn online() -> impl Iterator<Item = &'static Local> {
    crate::scheduler::percpu::online().map(|p| get(p.cpu_id as usize))
}
