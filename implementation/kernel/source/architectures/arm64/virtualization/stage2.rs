// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Stage 2 translation: a guest's physical address space.
//!
//! Four levels of tables, as the processor's own, but with the permissions
//! the guest has and the memory type in each leaf; a guest's own
//! translation combines with them. Leaves map 4 KiB pages. A table is freed
//! with the address space. Each address space has a VMID, which tags its
//! translations in the TLB: guests' caches stay apart, and nothing is
//! flushed as processors go from one to the other.
//!
//! What a guest reaches at the addresses of its interrupt controller's
//! registers is no memory: they stay unmapped, and the kernel answers for
//! them. Those registers are kept here: they are the guest's, for all its
//! processors.

use alloc::vec::Vec;
use core::arch::asm;

use hv::arm64::gic;

use super::super::memory::{flush_cache_range, in_direct_map};
use crate::memory::paging::Cache;
use crate::memory::{phys, phys_to_virt};
use crate::synchronization::SpinLock;
use crate::virtualization::Access;

const VALID: u64 = 1 << 0;
/// In an entry of levels 3 to 1, a table below; of level 0, always set: a
/// page.
const TABLE_OR_PAGE: u64 = 1 << 1;
/// Memory types of leaves (`MemAttr`, bits 2-5): Device-nGnRE, Normal not
/// cached, Normal write-back.
const TYPE_DEVICE: u64 = 0b0001 << 2;
const TYPE_NON_CACHEABLE: u64 = 0b0101 << 2;
const TYPE_WRITE_BACK: u64 = 0b1111 << 2;
const READ: u64 = 1 << 6;
const WRITE: u64 = 1 << 7;
const INNER_SHAREABLE: u64 = 3 << 8;
const ACCESSED: u64 = 1 << 10;
/// The guest may not run it.
const NOT_EXECUTABLE: u64 = 2 << 53;
const ADDR_MASK: u64 = 0x0000_FFFF_FFFF_F000;

/// VMIDs: from 1 up, as 0 is no guest's. 8 bits at least, which is enough
/// for as many guests as anything here makes; those of guests that have
/// ended come first.
struct Vmids {
    next: u16,
    free: Vec<u16>,
}

static VMIDS: SpinLock<Vmids> = SpinLock::new(Vmids { next: 1, free: Vec::new() });

pub struct Stage2 {
    root: u64,
    vmid: u16,
    /// The interrupt controller's distributor.
    pub distributor: gic::Shared,
}

fn table(frame: u64) -> &'static mut [u64; 512] {
    // SAFETY: table frames are owned by their `Stage2` and direct-mapped.
    unsafe { &mut *(phys_to_virt(frame) as *mut [u64; 512]) }
}

fn index(gpa: u64, level: u32) -> usize {
    ((gpa >> (12 + 9 * level)) & 0x1FF) as usize
}

/// Lets walkers see the tables as they now are.
fn publish() {
    // SAFETY: a barrier only orders.
    unsafe { asm!("dsb ishst", options(nostack, preserves_flags)) };
}

impl Stage2 {
    /// Guest-physical addresses are below this (four-level tables).
    pub const LIMIT: u64 = 1 << 48;

    pub fn new() -> Option<Stage2> {
        let vmid = {
            let mut vmids = VMIDS.lock();
            match vmids.free.pop() {
                Some(vmid) => vmid,
                None if vmids.next <= u8::MAX as u16 => {
                    vmids.next += 1;
                    vmids.next - 1
                }
                None => return None,
            }
        };
        let Some(root) = phys::alloc_zeroed() else {
            VMIDS.lock().free.push(vmid);
            return None;
        };
        publish();
        Some(Stage2 { root, vmid, distributor: gic::Shared::new() })
    }

    /// What `VTTBR_EL2` holds to run the guest: the root, and the VMID.
    pub fn pointer(&self) -> u64 {
        self.root | (self.vmid as u64) << 48
    }

    /// Maps the 4 KiB page at `gpa` to `frame`. The memory type follows
    /// the host's mapping of the memory (`cache`); the guest's own page
    /// attributes combine with it, and may take it past the caches, where
    /// what the page held before lies under what was written since: so that
    /// is written back first.
    pub fn map(&mut self, gpa: u64, frame: u64, access: Access, cache: Cache) -> Result<(), ()> {
        if cache == Cache::WriteBack && in_direct_map(frame) {
            flush_cache_range(phys_to_virt(frame & ADDR_MASK), 4096);
        }
        let mut t = self.root;
        for level in (1..=3).rev() {
            let e = &mut table(t)[index(gpa, level)];
            if *e & VALID == 0 {
                let next = phys::alloc_zeroed().ok_or(())?;
                publish();
                // SAFETY: a valid reference; walkers read the entry.
                unsafe { core::ptr::write_volatile(e, next | VALID | TABLE_OR_PAGE) };
            }
            t = *e & ADDR_MASK;
        }
        let memory_type = match cache {
            Cache::WriteBack => TYPE_WRITE_BACK,
            Cache::WriteCombining => TYPE_NON_CACHEABLE,
            Cache::Uncached => TYPE_DEVICE,
        };
        let mut bits = VALID | TABLE_OR_PAGE | ACCESSED | INNER_SHAREABLE | memory_type;
        if access.read {
            bits |= READ;
        }
        if access.write {
            bits |= WRITE;
        }
        if !access.execute {
            bits |= NOT_EXECUTABLE;
        }
        // SAFETY: a valid reference; walkers read the entry.
        unsafe { core::ptr::write_volatile(&mut table(t)[index(gpa, 0)], (frame & ADDR_MASK) | bits) };
        publish();
        Ok(())
    }

    /// Removes the mapping of the page at `gpa`, if any. The caller makes
    /// every processor forget it before the page is reused.
    pub fn unmap(&mut self, gpa: u64) {
        let mut t = self.root;
        for level in (1..=3).rev() {
            let e = table(t)[index(gpa, level)];
            if e & VALID == 0 {
                return;
            }
            t = e & ADDR_MASK;
        }
        // SAFETY: a valid reference; walkers read the entry.
        unsafe { core::ptr::write_volatile(&mut table(t)[index(gpa, 0)], 0) };
    }

    /// Writes all the guest's memory back: a processor whose MMU is off
    /// reads around the caches, as a guest starts, and what its monitor
    /// wrote into its memory may be in them. What its monitor wrote may
    /// also be code the instruction caches know otherwise.
    pub fn clean(&self) {
        let mut tables: Vec<(u64, u32)> = alloc::vec![(self.root, 3)];
        while let Some((t, level)) = tables.pop() {
            for &e in table(t).iter() {
                if e & VALID == 0 {
                    continue;
                }
                if level > 0 {
                    tables.push((e & ADDR_MASK, level - 1));
                } else if e & (0xF << 2) == TYPE_WRITE_BACK && in_direct_map(e & ADDR_MASK) {
                    flush_cache_range(phys_to_virt(e & ADDR_MASK), 4096);
                }
            }
        }
        // SAFETY: invalidating instruction caches only makes them fetch.
        unsafe { asm!("ic ialluis", "dsb ish", "isb", options(nostack, preserves_flags)) };
    }
}

impl Drop for Stage2 {
    fn drop(&mut self) {
        // Every table, leaves' frames excepted (they belong to VMOs). No
        // processor walks them any more: the guest's `Drop` saw to that.
        let mut tables: Vec<(u64, u32)> = alloc::vec![(self.root, 3)];
        while let Some((t, level)) = tables.pop() {
            if level > 0 {
                for &e in table(t).iter() {
                    if e & VALID != 0 {
                        tables.push((e & ADDR_MASK, level - 1));
                    }
                }
            }
            phys::free(t);
        }
        VMIDS.lock().free.push(self.vmid);
    }
}
