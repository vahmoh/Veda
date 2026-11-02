// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! TLB maintenance.
//!
//! The processors do it themselves: an invalidation sent to the inner
//! shareable domain reaches every other processor's TLB, without
//! interrupting it, whatever it runs, and a `DSB` waits until it has. So
//! when the BKL holder changes a mapping, it interrupts nobody. Each
//! address space's translations carry its ASID, and so may outlast the time
//! it is loaded, which is why they are invalidated everywhere, not only
//! where it is loaded; the kernel's are invalidated for every ASID.
//!
//! Guests' physical memory goes the same way, but for all guests at once.
//!
//! A memory barrier for a user address space (`vm_barrier`) is the one
//! thing that takes an IPI: each CPU that has it loaded runs a `DSB` in the
//! handler, then acknowledges, so each has gone through a full barrier once
//! it has. No CPU needs the BKL to acknowledge, so this cannot deadlock:
//! CPUs spinning on the BKL poll their request flag (see
//! `synchronization::bkl::acquire`), and none can load tables meanwhile
//! (that takes the BKL, which the requester holds).

use core::arch::asm;
use core::sync::atomic::Ordering;

use super::paging::asid_of;
use super::{gic, percpu};
use crate::scheduler::percpu::MAX_CPUS;

/// Where an ASID goes in an invalidation's operand.
const ASID_SHIFT: u32 = 48;
/// Beyond this many pages a whole address space is invalidated, not each
/// page.
const MANY_PAGES: u64 = 64;

/// The page at `addr`, as an invalidation's operand has it: bits 12 and up
/// of the address, without what is above them, which would say something
/// else.
#[inline]
fn page_operand(addr: u64) -> u64 {
    (addr >> 12) & 0xFFF_FFFF_FFFF
}

/// Goes through the memory barrier another CPU requested, if it did, and
/// acknowledges it.
pub fn service_pending() {
    let me = percpu::current();
    if me.barrier_pending.swap(false, Ordering::AcqRel) {
        // SAFETY: a barrier only orders.
        unsafe { asm!("dsb ish", options(nostack, preserves_flags)) };
        me.barrier_done.fetch_add(1, Ordering::AcqRel);
    }
}

/// Handler for the barrier IPI.
pub(in crate::architecture) fn handle_ipi() {
    service_pending();
}

/// The tables as they are now are what every walk from here on reads.
#[inline]
fn tables_written() {
    // SAFETY: a barrier only orders.
    unsafe { asm!("dsb ishst", options(nostack, preserves_flags)) };
}

/// Every processor has done what was sent to it.
#[inline]
fn wait_for_others() {
    // SAFETY: barriers only order.
    unsafe { asm!("dsb ish", "isb", options(nostack, preserves_flags)) };
}

/// Flushes `addr..addr+len` from every CPU's TLB, for every ASID (the
/// kernel's own mappings, which every CPU has).
pub fn shootdown(addr: u64, len: u64) {
    let pages = len.div_ceil(4096);
    tables_written();
    // SAFETY: invalidating TLB entries is always safe.
    unsafe {
        if pages <= MANY_PAGES {
            for i in 0..pages {
                asm!("tlbi vaae1is, {}", in(reg) page_operand(addr + i * 4096), options(nostack, preserves_flags));
            }
        } else {
            asm!("tlbi vmalle1is", options(nostack, preserves_flags));
        }
    }
    wait_for_others();
}

/// Flushes `addr..addr+len` of the user address space whose tables are
/// `root` from the TLBs that can hold it, which is any. With the BKL held.
pub fn shootdown_space(root: u64, addr: u64, len: u64) {
    let pages = len.div_ceil(4096);
    if pages > MANY_PAGES {
        return forget_space(root);
    }
    let asid = (asid_of(root) as u64) << ASID_SHIFT;
    tables_written();
    for i in 0..pages {
        // SAFETY: invalidating TLB entries is always safe.
        unsafe {
            asm!("tlbi vae1is, {}", in(reg) asid | page_operand(addr + i * 4096), options(nostack, preserves_flags))
        };
    }
    wait_for_others();
}

/// Flushes the page at `virt` of the address space `root` (the kernel's or
/// a user's) from every TLB.
pub(in crate::architecture) fn forget_page(root: u64, virt: u64) {
    if virt >= bootinfo::HHDM_BASE {
        shootdown(virt, 4096);
    } else {
        shootdown_space(root, virt, 4096);
    }
}

/// Flushes the whole user address space `root` from every TLB.
pub(in crate::architecture) fn forget_space(root: u64) {
    let asid = (asid_of(root) as u64) << ASID_SHIFT;
    tables_written();
    // SAFETY: invalidating TLB entries is always safe.
    unsafe { asm!("tlbi aside1is, {}", in(reg) asid, options(nostack, preserves_flags)) };
    wait_for_others();
}

/// This CPU forgets what ASID 0 meant, as the address spaces that share it
/// do when one is loaded.
pub(in crate::architecture) fn forget_shared_asid() {
    // SAFETY: invalidating TLB entries is always safe.
    unsafe { asm!("tlbi aside1, xzr", "dsb nsh", "isb", options(nostack, preserves_flags)) };
}

/// Has every other CPU that has the user address space whose tables are
/// `root` loaded go through a full memory barrier, and waits until each
/// has. With the BKL held.
pub fn barrier_space(root: u64) {
    let me = percpu::cpu_id_or_boot();
    let mut targets: [(u32, u64); MAX_CPUS] = [(u32::MAX, 0); MAX_CPUS];
    let mut n = 0;
    for p in percpu::online() {
        if p.cpu_id == me || p.loaded_root.load(Ordering::Acquire) != root {
            continue;
        }
        let before = p.barrier_done.load(Ordering::Acquire);
        p.barrier_pending.store(true, Ordering::Release);
        gic::send_sgi(p.affinity, gic::SGI_BARRIER);
        targets[n] = (p.cpu_id, before);
        n += 1;
    }
    for &(id, before) in &targets[..n] {
        let p = percpu::get(id as usize);
        let mut spins = 0u64;
        // The target clears `pending` before its barrier, so only the
        // acknowledgement counter proves it has been through it.
        while p.barrier_done.load(Ordering::Acquire) == before {
            core::hint::spin_loop();
            spins += 1;
            if spins == 100_000_000 {
                crate::kwarn!("tlb: CPU {id} is slow to acknowledge a memory barrier");
            }
        }
    }
    // SAFETY: a barrier only orders.
    unsafe { asm!("dsb ish", options(nostack, preserves_flags)) };
}

/// Makes every CPU forget the translations of guests' physical memory it
/// caches, before memory unmapped from a guest is reused.
pub fn shootdown_guest_memory() {
    tables_written();
    if super::cpu::at_el2() {
        // SAFETY: invalidating TLB entries is always safe.
        unsafe { asm!("tlbi alle1is", options(nostack, preserves_flags)) };
    }
    wait_for_others();
}
