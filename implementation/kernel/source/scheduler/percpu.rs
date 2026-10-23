// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Per-CPU data: what the kernel keeps for each CPU.
//!
//! A CPU finds its own [`PerCpu`] by its index, which the architecture keeps
//! where the CPU reaches it at once (`architecture::percpu`), beside
//! whatever else the architecture keeps for the CPU.

use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::architecture;
use crate::scheduler::CpuSched;

/// The most CPUs the kernel runs on.
pub const MAX_CPUS: usize = 32;

pub struct PerCpu {
    pub cpu_id: u32,
    /// Scheduler state owned by this CPU.
    pub sched: UnsafeCell<CpuSched>,
    pub online: AtomicBool,
    /// Accumulated time spent in the idle thread (ns).
    pub idle_ns: AtomicU64,
    /// Timer interrupts this CPU has taken.
    pub timer_interrupts: AtomicU64,
    /// This idle CPU was sent a reschedule IPI and has not run the
    /// scheduler since: further wake-ups pick another CPU.
    pub resched_pending: AtomicBool,
}

// SAFETY: each PerCpu is mutated only by its own CPU (or under the BKL);
// cross-CPU fields are atomics.
unsafe impl Sync for PerCpu {}

static PERCPU: [PerCpu; MAX_CPUS] = {
    // `[expr; N]` needs Copy, so build the array element by element.
    let mut arr: [MaybeUninit<PerCpu>; MAX_CPUS] = [const { MaybeUninit::uninit() }; MAX_CPUS];
    let mut i = 0;
    while i < MAX_CPUS {
        arr[i] = MaybeUninit::new(PerCpu {
            cpu_id: i as u32,
            sched: UnsafeCell::new(CpuSched::new()),
            online: AtomicBool::new(false),
            idle_ns: AtomicU64::new(0),
            timer_interrupts: AtomicU64::new(0),
            resched_pending: AtomicBool::new(false),
        });
        i += 1;
    }
    // SAFETY: every element was initialised above.
    unsafe { core::mem::transmute::<[MaybeUninit<PerCpu>; MAX_CPUS], [PerCpu; MAX_CPUS]>(arr) }
};

/// The current CPU's data (once the architecture has set the CPU up).
#[inline]
pub fn current() -> &'static PerCpu {
    &PERCPU[architecture::percpu::current_id()]
}

/// The current CPU's index, or 0 (the boot CPU's) before it has one.
#[inline]
pub fn cpu_id_or_boot() -> u32 {
    architecture::percpu::cpu_id_or_boot()
}

/// The data of CPU `id`.
pub fn get(id: usize) -> &'static PerCpu {
    &PERCPU[id]
}

/// Iterates over all CPUs that have come online.
pub fn online() -> impl Iterator<Item = &'static PerCpu> {
    PERCPU.iter().filter(|p| p.online.load(Ordering::Acquire))
}
