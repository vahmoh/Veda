// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! TLB shootdowns.
//!
//! When the BKL holder changes a mapping that other CPUs may have cached,
//! it asks those CPUs to flush their TLBs and waits for the
//! acknowledgements: for a user address space, the CPUs that have its page
//! tables loaded, as no other can hold its translations (loading page
//! tables forgets the last ones', with no PCIDs); for the kernel's own
//! mappings, every CPU. CPUs running user code receive an IPI; CPUs
//! spinning on the BKL poll their request flag (see
//! `synchronization::bkl::acquire`); idle CPUs are woken by the IPI. No CPU
//! needs the BKL to acknowledge, so this cannot deadlock, and none can load
//! page tables meanwhile (that takes the BKL, which the requester holds).
//!
//! An unmap in one process so interrupts only the CPUs running it, never
//! those running others, idle ones or the driver VM's processors (each of
//! which leaves its guest for an IPI).
//!
//! Guests' physical memory (EPT) is shot down the same way: a CPU that
//! runs a guest leaves it for the IPI, and forgets every guest's
//! translations (`invept`).

use core::sync::atomic::Ordering;

use super::{apic, cpu, idt::TLB_VECTOR, percpu};
use crate::percpu::MAX_CPUS;

/// Flushes the local TLB, or the guests' translations, if another CPU
/// requested it.
pub fn service_pending() {
    let me = percpu::current();
    let tlb = me.tlb_flush_pending.swap(false, Ordering::AcqRel);
    let ept = me.ept_flush_pending.swap(false, Ordering::AcqRel);
    if tlb {
        cpu::flush_tlb();
    }
    if ept && me.vmx_on.load(Ordering::Relaxed) {
        super::hv::vmx::invept_all();
    }
    if tlb || ept {
        me.tlb_flush_done.fetch_add(1, Ordering::AcqRel);
    }
}

/// Handler for the shootdown IPI.
pub(in crate::architecture) fn handle_ipi() {
    service_pending();
}

/// Flushes `addr..addr+len` from this CPU's TLB.
fn flush_local(addr: u64, len: u64) {
    let pages = len.div_ceil(4096);
    if pages <= 32 {
        for i in 0..pages {
            cpu::invlpg(addr + i * 4096);
        }
    } else {
        cpu::flush_tlb();
    }
}

/// Flushes `addr..addr+len` locally and on every other online CPU (the
/// kernel's own mappings, which every CPU has).
pub fn shootdown(addr: u64, len: u64) {
    flush_local(addr, len);
    request(|_| true, |p| &p.tlb_flush_pending);
}

/// Flushes `addr..addr+len` of the user address space whose page tables
/// are `root` from the TLBs that can hold it: this CPU's and every other's
/// that has those tables loaded. With the BKL held.
pub fn shootdown_space(root: u64, addr: u64, len: u64) {
    if percpu::current().loaded_root.load(Ordering::Acquire) == root {
        flush_local(addr, len);
    }
    request(|p| p.loaded_root.load(Ordering::Acquire) == root, |p| &p.tlb_flush_pending);
}

/// Makes every CPU forget the translations of guests' physical memory it
/// caches, before memory unmapped from a guest is reused.
pub fn shootdown_guest_memory() {
    if percpu::current().vmx_on.load(Ordering::Relaxed) {
        super::hv::vmx::invept_all();
    }
    request(|_| true, |p| &p.ept_flush_pending);
}

/// Sets the flag `flag` picks on every other online CPU that `concerned`
/// accepts and waits until each has acknowledged it.
fn request(
    concerned: impl Fn(&percpu::Local) -> bool,
    flag: impl Fn(&percpu::Local) -> &core::sync::atomic::AtomicBool,
) {
    let me = percpu::cpu_id_or_boot();
    let mut targets: [(u32, u64); MAX_CPUS] = [(u32::MAX, 0); MAX_CPUS];
    let mut n = 0;
    for p in percpu::online() {
        if p.cpu_id == me || !concerned(p) {
            continue;
        }
        let before = p.tlb_flush_done.load(Ordering::Acquire);
        flag(p).store(true, Ordering::Release);
        apic::send_ipi(p.apic_id, TLB_VECTOR as u32);
        targets[n] = (p.cpu_id, before);
        n += 1;
    }
    for &(id, before) in &targets[..n] {
        let p = percpu::get(id as usize);
        let mut spins = 0u64;
        // The target clears `pending` before flushing, so only the
        // acknowledgement counter proves the flush has completed.
        while p.tlb_flush_done.load(Ordering::Acquire) == before {
            core::hint::spin_loop();
            spins += 1;
            if spins == 100_000_000 {
                crate::kwarn!("tlb: CPU {id} is slow to acknowledge a shootdown");
            }
        }
    }
}
