// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Kernel virtual address regions outside the direct map:
//!
//! * MMIO windows (uncached or write-combining device registers), and
//! * kernel stacks, each preceded by an unmapped guard page so that a stack
//!   overflow faults instead of silently corrupting memory.

use alloc::vec::Vec;

use super::paging::{self, Cache, Flags};
use super::{PAGE_SIZE, phys};
use crate::synchronization::SpinLock;

const MMIO_BASE: u64 = crate::architecture::mm::MMIO_BASE;
const MMIO_LIMIT: u64 = MMIO_BASE + crate::architecture::mm::MMIO_SIZE;
const STACK_BASE: u64 = crate::architecture::mm::STACKS_BASE;
/// Virtual size reserved per stack slot (stack + guard).
const STACK_SLOT: u64 = 128 * 1024;

struct State {
    mmio_next: u64,
    stack_next: u64,
    free_stack_slots: Vec<u64>,
    /// The physical ranges of the registers mapped for the kernel itself
    /// (its interrupt controllers, timers, IOMMU): no driver may map them.
    kernel_mmio: Vec<(u64, u64)>,
}

static STATE: SpinLock<State> = SpinLock::new(State {
    mmio_next: MMIO_BASE,
    stack_next: STACK_BASE,
    free_stack_slots: Vec::new(),
    kernel_mmio: Vec::new(),
});

/// Maps a physical MMIO range and returns the virtual address of `phys`.
/// The range is the kernel's from then on: no driver may map it
/// ([`is_kernels`]).
pub fn map_mmio(phys_addr: u64, size: u64, cache: Cache) -> u64 {
    let start = phys_addr & !(PAGE_SIZE - 1);
    let pages = (phys_addr + size - start).div_ceil(PAGE_SIZE);
    let virt = {
        let mut s = STATE.lock();
        let v = s.mmio_next;
        s.mmio_next += (pages + 1) * PAGE_SIZE;
        assert!(s.mmio_next < MMIO_LIMIT, "kernel MMIO window exhausted");
        s.kernel_mmio.push((start, start + pages * PAGE_SIZE));
        v
    };
    let flags = Flags { writable: true, executable: false, user: false, global: true, cache };
    for i in 0..pages {
        paging::map(paging::kernel_root(), virt + i * PAGE_SIZE, start + i * PAGE_SIZE, flags, true)
            .expect("out of memory mapping MMIO");
    }
    virt + (phys_addr - start)
}

/// Whether `[base, base + size)` reaches registers the kernel keeps for
/// itself: those it has mapped, and those the architecture reserves
/// (`architecture::mm::RESERVED_MMIO`). A device whose BAR the firmware or
/// the device itself put over them must not give its driver the kernel's
/// interrupt controllers or IOMMU.
pub fn is_kernels(base: u64, size: u64) -> bool {
    let end = base.saturating_add(size);
    let overlaps = |s: u64, e: u64| base < e && s < end;
    let reserved = crate::architecture::mm::RESERVED_MMIO;
    overlaps(reserved.start, reserved.end) || STATE.lock().kernel_mmio.iter().any(|&(s, e)| overlaps(s, e))
}

/// A kernel stack with a guard page below it.
pub struct KernelStack {
    base: u64,
    pages: u64,
}

impl KernelStack {
    /// Allocates a stack of `pages` pages (at most `STACK_SLOT` minus guard).
    pub fn new(pages: u64) -> Option<KernelStack> {
        assert!((pages + 1) * PAGE_SIZE <= STACK_SLOT);
        let slot = {
            let mut s = STATE.lock();
            s.free_stack_slots.pop().unwrap_or_else(|| {
                let v = s.stack_next;
                s.stack_next += STACK_SLOT;
                v
            })
        };
        // The guard page is the lowest page of the slot and stays unmapped.
        let base = slot + STACK_SLOT - pages * PAGE_SIZE;
        // Not global: a freed stack must be forgotten by a flush of the TLB's
        // non-global entries.
        let flags = Flags { global: false, ..Flags::KERNEL_RW };
        for i in 0..pages {
            let mapped = phys::alloc().is_some_and(|frame| {
                let ok = paging::map(paging::kernel_root(), base + i * PAGE_SIZE, frame, flags, true).is_ok();
                if !ok {
                    phys::free(frame);
                }
                ok
            });
            if !mapped {
                // Give back what was mapped so far, and the slot.
                release(base, i, slot);
                return None;
            }
        }
        Some(KernelStack { base, pages })
    }

    pub fn top(&self) -> u64 {
        self.base + self.pages * PAGE_SIZE
    }
}

/// Unmaps the `pages` pages from `base`, frees their frames once no TLB
/// can reach them, and makes the stack slot `slot` free again.
fn release(base: u64, pages: u64, slot: u64) {
    let frames: alloc::vec::Vec<u64> =
        (0..pages).filter_map(|i| paging::unmap(paging::kernel_root(), base + i * PAGE_SIZE)).collect();
    // Other CPUs may still cache translations of the stack.
    crate::architecture::tlb::shootdown(base, pages * PAGE_SIZE);
    for frame in frames {
        phys::free(frame);
    }
    STATE.lock().free_stack_slots.push(slot);
}

impl Drop for KernelStack {
    fn drop(&mut self) {
        release(self.base, self.pages, self.base + self.pages * PAGE_SIZE - STACK_SLOT);
    }
}

/// Returns `true` if `addr` lies in a kernel stack guard page.
pub fn is_stack_guard(addr: u64) -> bool {
    (STACK_BASE..STACK_BASE + crate::architecture::mm::STACKS_SIZE).contains(&addr)
        && (addr - STACK_BASE) % STACK_SLOT < PAGE_SIZE
}
