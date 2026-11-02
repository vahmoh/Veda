// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Builds the initial AArch64 translation tables handed to the kernel: four
//! levels of 512 entries over 48 bits of address, in 4 KiB granules, one
//! set for each half of addresses.
//!
//! While boot services are active the firmware identity-maps all memory, so
//! table pages can be written through their physical addresses.

use bootinfo::architecture::memory_attribute;

use crate::FrameSource;

const VALID: u64 = 1 << 0;
/// In an entry of levels 3 to 1, a table below; of level 0, a page.
const TABLE_OR_PAGE: u64 = 1 << 1;
const INNER_SHAREABLE: u64 = 3 << 8;
const ACCESSED: u64 = 1 << 10;
const READ_ONLY: u64 = 1 << 7;
const NOT_EXECUTABLE_HERE: u64 = 1 << 53;
const NOT_EXECUTABLE_BELOW: u64 = 1 << 54;

/// Mapping flags: writable (otherwise read-only), not executable, and not
/// global (it goes when the low half's translations do).
pub const WRITABLE: u64 = 1 << 62;
pub const NO_EXECUTE: u64 = 1 << 63;
pub const NOT_GLOBAL: u64 = 1 << 11;

const ADDR_MASK: u64 = 0x0000_FFFF_FFFF_F000;
const GIB: u64 = 1 << 30;
const MIB2: u64 = 2 << 20;
const PAGE: u64 = 4096;

fn table(phys: u64) -> &'static mut [u64; 512] {
    // SAFETY: `phys` is a table page we allocated; the firmware's identity
    // map makes it addressable at its physical address.
    unsafe { &mut *(phys as *mut [u64; 512]) }
}

fn index(virt: u64, level: u32) -> usize {
    ((virt >> (12 + 9 * level)) & 0x1FF) as usize
}

/// A mapping's entry for `phys`, with `flags`: write-back memory, shared
/// with the other processors.
fn entry(phys: u64, flags: u64) -> u64 {
    let mut e = (phys & ADDR_MASK)
        | VALID
        | ACCESSED
        | INNER_SHAREABLE
        | memory_attribute::WRITE_BACK << 2
        | NOT_EXECUTABLE_BELOW
        | (flags & NOT_GLOBAL);
    if flags & WRITABLE == 0 {
        e |= READ_ONLY;
    }
    if flags & NO_EXECUTE != 0 {
        e |= NOT_EXECUTABLE_HERE;
    }
    e
}

/// The tables of one half of addresses.
pub struct Tables {
    root: u64,
}

impl Tables {
    pub fn new<F: FrameSource>(frames: &mut F) -> Result<Tables, &'static str> {
        Ok(Tables { root: frames.alloc_zeroed_page()? })
    }

    pub fn root(&self) -> u64 {
        self.root
    }

    /// Returns the table entry `idx` of `table_phys` refers to, creating it.
    fn next<F: FrameSource>(frames: &mut F, table_phys: u64, idx: usize) -> Result<u64, &'static str> {
        let t = table(table_phys);
        if t[idx] & VALID == 0 {
            t[idx] = frames.alloc_zeroed_page()? | VALID | TABLE_OR_PAGE;
        } else if t[idx] & TABLE_OR_PAGE == 0 {
            return Err("mapping conflicts with an existing block");
        }
        Ok(t[idx] & ADDR_MASK)
    }

    /// Maps one 4 KiB page.
    pub fn map_4k<F: FrameSource>(
        &mut self,
        frames: &mut F,
        virt: u64,
        phys: u64,
        flags: u64,
    ) -> Result<(), &'static str> {
        let l2 = Self::next(frames, self.root, index(virt, 3))?;
        let l1 = Self::next(frames, l2, index(virt, 2))?;
        let l0 = Self::next(frames, l1, index(virt, 1))?;
        table(l0)[index(virt, 0)] = entry(phys, flags) | TABLE_OR_PAGE;
        Ok(())
    }

    /// Maps `size` bytes of physical memory from `phys` at `virt`, with the
    /// largest blocks that fit.
    pub fn map_range<F: FrameSource>(
        &mut self,
        frames: &mut F,
        mut virt: u64,
        mut phys: u64,
        mut size: u64,
        flags: u64,
    ) -> Result<(), &'static str> {
        while size > 0 {
            let aligned = |bytes: u64| (virt | phys).is_multiple_of(bytes) && size >= bytes;
            let step = if aligned(GIB) {
                let l2 = Self::next(frames, self.root, index(virt, 3))?;
                table(l2)[index(virt, 2)] = entry(phys, flags);
                GIB
            } else if aligned(MIB2) {
                let l2 = Self::next(frames, self.root, index(virt, 3))?;
                let l1 = Self::next(frames, l2, index(virt, 2))?;
                table(l1)[index(virt, 1)] = entry(phys, flags);
                MIB2
            } else {
                self.map_4k(frames, virt, phys, flags)?;
                PAGE
            };
            (virt, phys, size) = (virt + step, phys + step, size - step);
        }
        Ok(())
    }
}
