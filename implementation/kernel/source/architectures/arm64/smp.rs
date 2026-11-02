// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Bringing up application processors (APs).
//!
//! PSCI's `CPU_ON` starts each AP where the kernel says, in physical
//! memory, at the exception level the boot processor started in, with its
//! MMU off and a word of the kernel's in `x0`. The AP reads what it needs
//! from a page that word points at, gets to where the kernel runs as the
//! loader took the boot processor there (`bootinfo::set_up_processor`), and
//! jumps to [`ap_entry`] on its idle thread's stack. It turns its MMU on in
//! the kernel's code at its physical address, so a temporary table maps that
//! address.

use alloc::sync::Arc;
use core::arch::{asm, global_asm};
use core::sync::atomic::{AtomicU32, Ordering};

use bootinfo::architecture::{
    CNTHCTL_EL1, CPTR_EL1, HCR_HOST, HCR_RW, ICC_SRE, MAIR, SCTLR, SCTLR_OFF, SPSR_EL1H, TCR, memory_attribute,
};

use super::cpu::write_register;
use super::memory::flush_cache_range;
use super::{cpu, gic, paging, percpu, power};
use crate::memory::{phys, phys_to_virt};
use crate::scheduler::percpu::MAX_CPUS;
use crate::scheduler::{self, Thread};
use crate::synchronization::SpinLock;
use crate::time;

/// What an AP reads as it starts, with its MMU off.
#[repr(C)]
struct Start {
    /// `TTBR0` and `TTBR1`, as `x3` and `x4` of `set_up_processor`.
    low: u64,
    high: u64,
    stack: u64,
    entry: u64,
    argument: u64,
}

// The AP reads its `Start` (`x0`), and goes on as `set_up_processor` takes
// it, keeping `x0` to `x4`.
global_asm!(
    r#"
    .section .text
    .balign 16
    .globl vk_ap_start
vk_ap_start:
    ldp     x3, x4, [x0, #0]
    ldp     x1, x2, [x0, #16]
    ldr     x0, [x0, #32]
"#,
    bootinfo::set_up_processor!(),
    r#"
    mov     sp, x1
    mov     x29, #0
    mov     x30, #0
    br      x2
    .ltorg
    .globl vk_ap_end
vk_ap_end:
"#,
    hcr_host = const HCR_HOST,
    hcr_rw = const HCR_RW,
    icc_sre = const ICC_SRE,
    cnthctl_el1 = const CNTHCTL_EL1,
    cptr_el1 = const CPTR_EL1,
    spsr_el1h = const SPSR_EL1H,
    sctlr_off = const SCTLR_OFF,
    mair = const MAIR,
    tcr = const TCR,
    sctlr = const SCTLR,
);

unsafe extern "C" {
    static vk_ap_start: u8;
    static vk_ap_end: u8;
}

/// Idle threads created by the BSP for each AP.
static IDLE_THREADS: SpinLock<[Option<Arc<Thread>>; MAX_CPUS]> = SpinLock::new([const { None }; MAX_CPUS]);
static STARTED: AtomicU32 = AtomicU32::new(0);

/// The stack this CPU's faults run on (see `super::entry`).
fn init_fault_stack(id: usize) {
    let stack = crate::memory::kvirt::KernelStack::new(4).expect("allocating a fault stack");
    // SAFETY: only this CPU's bring-up code writes it.
    unsafe { percpu::set_fault_stack(id, stack.top()) };
    core::mem::forget(stack); // lives forever
}

/// Per-CPU initialisation shared by the BSP and APs (once its per-CPU block
/// is installed).
pub(in crate::architecture) fn init_this_cpu(id: usize) {
    cpu::init_cpu_features();
    init_fault_stack(id);
    super::virtualization::init_this_cpu();
}

extern "C" fn ap_entry(index: u64) -> ! {
    let id = index as usize;
    // SAFETY: per-CPU structures of this CPU, initialised exactly once;
    // nothing of the temporary table is used from here on.
    unsafe {
        percpu::install(id);
        write_register!("ttbr0_el1", paging::empty_root());
        asm!("isb", "tlbi vmalle1", "dsb nsh", "isb", options(nostack, preserves_flags));
    }
    init_this_cpu(id);
    gic::init_cpu();
    let idle = IDLE_THREADS.lock()[id].take().expect("AP started without an idle thread");
    crate::secondary_main(id, idle, || {
        STARTED.fetch_add(1, Ordering::AcqRel);
    })
}

/// Makes CPU `cpu` run its scheduler: it may be idle, or run a thread
/// that is to be preempted or ended.
pub fn reschedule(cpu: u32) {
    gic::send_sgi(percpu::get(cpu as usize).affinity, gic::SGI_RESCHEDULE);
}

/// Stops every other CPU for good (a panic).
pub fn halt_others() {
    let me = percpu::cpu_id_or_boot();
    for p in percpu::online() {
        if p.cpu_id != me {
            gic::send_sgi(p.affinity, gic::SGI_HALT);
        }
    }
}

/// A table that maps the GiB around `address` where it is, write-back and
/// executable, for the moment after an AP's MMU comes on.
fn identity_table(address: u64) -> u64 {
    /// A level 1 block's and a table entry's bits, as `paging` has them.
    const VALID: u64 = 1;
    const ACCESSED: u64 = 1 << 10;
    const INNER_SHAREABLE: u64 = 3 << 8;
    let root = phys::alloc_zeroed().expect("no memory for the AP's table");
    let middle = phys::alloc_zeroed().expect("no memory for the AP's table");
    let index = |level: u32| ((address >> (12 + 9 * level)) & 0x1FF) as usize;
    let block = (address & !((1 << 30) - 1)) | VALID | ACCESSED | INNER_SHAREABLE | memory_attribute::WRITE_BACK << 2;
    // SAFETY: frames just allocated, direct-mapped.
    unsafe {
        (*(phys_to_virt(root) as *mut [u64; 512]))[index(3)] = middle | VALID | paging::TABLE_OR_PAGE;
        (*(phys_to_virt(middle) as *mut [u64; 512]))[index(2)] = block;
    }
    flush_cache_range(phys_to_virt(root), 4096);
    flush_cache_range(phys_to_virt(middle), 4096);
    root
}

/// Starts the other CPUs (those the MADT lists), up to `max_cpus` CPUs in
/// all. Returns the number started.
pub fn start_secondary_cpus(max_cpus: usize) -> usize {
    let platform = super::acpi::PLATFORM.expect();
    let boot = percpu::get(0).affinity;
    let targets: alloc::vec::Vec<(usize, u64)> = platform
        .cpus
        .iter()
        .enumerate()
        .map(|(i, c)| (i, c.affinity))
        .filter(|&(_, a)| a != boot)
        .take(max_cpus.saturating_sub(1))
        .collect();
    if targets.is_empty() {
        return 0;
    }
    let kernel = super::kernel_image();
    let (start, end) = (core::ptr::addr_of!(vk_ap_start) as u64, core::ptr::addr_of!(vk_ap_end) as u64);
    let entry = start - kernel.virt_base + kernel.phys_base;
    // It runs with its caches off: memory must hold it.
    flush_cache_range(start, end - start);
    let low = identity_table(entry);
    let context = phys::alloc_zeroed().expect("no memory for the AP's start");
    for &(index, affinity) in &targets {
        if index >= MAX_CPUS {
            continue;
        }
        let Some(idle) = Thread::new_kernel("idle", scheduler::idle_entry, 0, true) else { break };
        let stack = idle.kernel_stack_top;
        IDLE_THREADS.lock()[index] = Some(idle);
        let page = phys_to_virt(context);
        let start = Start {
            low,
            high: paging::kernel_root(),
            stack,
            entry: ap_entry as *const () as u64,
            argument: index as u64,
        };
        // SAFETY: a page of ours, which no AP is reading: each has gone
        // through it before the next starts.
        unsafe { (page as *mut Start).write_volatile(start) };
        flush_cache_range(page, size_of::<Start>() as u64);
        let before = STARTED.load(Ordering::Acquire);
        // SAFETY: the entry is the code above, where it is, which memory
        // holds, and the AP's `Start` is at `context`.
        if let Err(e) = unsafe { power::cpu_on(affinity, entry, context) } {
            crate::kwarn!("smp: PSCI cannot start the CPU of affinity {:#x}: error {}", affinity, e);
            IDLE_THREADS.lock()[index] = None;
            continue;
        }
        // Allow a generous delay under emulation.
        let deadline = time::now_ns() + 5_000_000_000;
        while STARTED.load(Ordering::Acquire) == before && time::now_ns() < deadline {
            core::hint::spin_loop();
        }
        if STARTED.load(Ordering::Acquire) == before {
            crate::kwarn!("smp: the CPU of affinity {:#x} did not start", affinity);
            IDLE_THREADS.lock()[index] = None;
        }
    }
    STARTED.load(Ordering::Acquire) as usize
}
