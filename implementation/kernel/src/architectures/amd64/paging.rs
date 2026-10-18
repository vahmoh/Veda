// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! x86-64 four-level page tables.
//!
//! An address space is the physical address of its top-level table (a
//! PML4). The kernel half of every address space (PML4 slots 256..512) is
//! shared: all 256 kernel PDPTs are allocated at boot, and new address
//! spaces copy those 256 entries, so later kernel mappings are visible
//! everywhere.

use crate::memory::aspace::Perms;
use crate::memory::paging::{Cache, Flags, MapError};
use crate::memory::{phys, phys_to_virt};
use crate::synchronization::Once;

/// The size of a page.
pub const PAGE_SIZE: u64 = 4096;

pub(in crate::arch) const PRESENT: u64 = 1 << 0;
pub(in crate::arch) const WRITABLE: u64 = 1 << 1;
const USER: u64 = 1 << 2;
const PWT: u64 = 1 << 3;
const PCD: u64 = 1 << 4;
pub(in crate::arch) const HUGE: u64 = 1 << 7;
const GLOBAL: u64 = 1 << 8;
const NO_EXECUTE: u64 = 1 << 63;
const ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;

/// A memory type's bits in an entry (see the PAT layout in `cpu`).
fn cache_bits(cache: Cache) -> u64 {
    match cache {
        Cache::WriteBack => 0,
        Cache::WriteCombining => PWT,
        Cache::Uncached => PCD | PWT,
    }
}

/// A leaf entry's bits for `flags`.
fn bits(flags: Flags) -> u64 {
    let mut b = PRESENT | cache_bits(flags.cache);
    if flags.writable {
        b |= WRITABLE;
    }
    if !flags.executable {
        b |= NO_EXECUTE;
    }
    if flags.user {
        b |= USER;
    }
    if flags.global {
        b |= GLOBAL;
    }
    b
}

/// The flags a leaf entry has.
fn flags_of(entry: u64) -> Flags {
    let cache = match entry & (PCD | PWT) {
        0 => Cache::WriteBack,
        PWT => Cache::WriteCombining,
        _ => Cache::Uncached,
    };
    Flags {
        writable: entry & WRITABLE != 0,
        executable: entry & NO_EXECUTE == 0,
        user: entry & USER != 0,
        global: entry & GLOBAL != 0,
        cache,
    }
}

/// What the page tables grant for `perms`: AMD64 has no pages that can be
/// written or executed but not read, so any access at all lets them be
/// read.
pub fn effective(perms: Perms) -> Perms {
    Perms { read: perms.read || perms.write || perms.exec, ..perms }
}

fn table(phys: u64) -> &'static mut [u64; 512] {
    // SAFETY: page-table pages are direct-mapped and owned by the paging code.
    unsafe { &mut *(phys_to_virt(phys) as *mut [u64; 512]) }
}

#[inline]
fn index(virt: u64, level: u32) -> usize {
    ((virt >> (12 + 9 * level)) & 0x1FF) as usize
}

/// Returns the next-level table for `entry`, allocating it if needed.
fn next_table(t: &mut [u64; 512], idx: usize, user: bool) -> Result<u64, MapError> {
    if t[idx] & PRESENT == 0 {
        let frame = phys::alloc_zeroed().ok_or(MapError::NoMemory)?;
        t[idx] = frame | PRESENT | WRITABLE | if user { USER } else { 0 };
    }
    debug_assert!(t[idx] & HUGE == 0, "walking through a huge page");
    Ok(t[idx] & ADDR_MASK)
}

/// Maps one page in the address space `root`.
pub fn map(root: u64, virt: u64, phys_addr: u64, flags: Flags, overwrite: bool) -> Result<(), MapError> {
    let user = flags.user;
    let pdpt = next_table(table(root), index(virt, 3), user)?;
    let pd = next_table(table(pdpt), index(virt, 2), user)?;
    let pt = next_table(table(pd), index(virt, 1), user)?;
    let e = &mut table(pt)[index(virt, 0)];
    if *e & PRESENT != 0 && !overwrite {
        return Err(MapError::AlreadyMapped);
    }
    *e = (phys_addr & ADDR_MASK) | bits(flags);
    Ok(())
}

/// Removes the mapping of the page at `virt`, returning the frame it
/// mapped. The caller is responsible for TLB invalidation.
pub fn unmap(root: u64, virt: u64) -> Option<u64> {
    let mut t = table(root);
    for level in (1..=3).rev() {
        let e = t[index(virt, level)];
        if e & PRESENT == 0 || e & HUGE != 0 {
            return None;
        }
        t = table(e & ADDR_MASK);
    }
    let e = &mut t[index(virt, 0)];
    if *e & PRESENT == 0 {
        return None;
    }
    let frame = *e & ADDR_MASK;
    *e = 0;
    Some(frame)
}

/// Translates `virt`, returning the physical address and the mapping's
/// flags.
pub fn translate(root: u64, virt: u64) -> Option<(u64, Flags)> {
    let mut t = table(root);
    for level in (1..=3).rev() {
        let e = t[index(virt, level)];
        if e & PRESENT == 0 {
            return None;
        }
        if e & HUGE != 0 {
            let size = 1u64 << (12 + 9 * level);
            return Some(((e & ADDR_MASK & !(size - 1)) + (virt & (size - 1)), flags_of(e)));
        }
        t = table(e & ADDR_MASK);
    }
    let e = t[index(virt, 0)];
    if e & PRESENT == 0 { None } else { Some(((e & ADDR_MASK) + (virt & 0xFFF), flags_of(e))) }
}

/// Calls `f` on each present leaf entry of `[start, end)` (page-aligned,
/// in the user half), skipping the tables that are not there: a walk over
/// a large range that is mostly unused (a reservation, say) costs what is
/// mapped in it, not its size.
fn for_each_leaf(root: u64, start: u64, end: u64, f: &mut dyn FnMut(&mut u64)) {
    fn walk(t: u64, level: u32, base: u64, start: u64, end: u64, f: &mut dyn FnMut(&mut u64)) {
        // The bytes one entry of this level covers.
        let span = 1u64 << (12 + 9 * level);
        let first = (start.max(base) - base) / span;
        let last = (end.min(base + 512 * span) - base).div_ceil(span);
        for i in first..last {
            let e = &mut table(t)[i as usize];
            if *e & PRESENT == 0 {
                continue;
            }
            if level == 0 {
                f(e);
            } else if *e & HUGE == 0 {
                walk(*e & ADDR_MASK, level - 1, base + i * span, start, end, f);
            }
        }
    }
    if start < end {
        walk(root, 3, 0, start, end, f);
    }
}

/// Removes the mappings of the pages in `[start, end)`. The caller is
/// responsible for TLB invalidation.
pub fn unmap_range(root: u64, start: u64, end: u64) {
    for_each_leaf(root, start, end, &mut |e| *e = 0);
}

/// Changes the flags of the pages mapped in `[start, end)`. The caller is
/// responsible for TLB invalidation.
pub fn protect_range(root: u64, start: u64, end: u64, flags: Flags) {
    let leaf = bits(flags);
    for_each_leaf(root, start, end, &mut |e| *e = (*e & ADDR_MASK) | leaf);
}

/// The kernel's own address space (used by idle and kernel threads).
static KERNEL_ROOT: Once<u64> = Once::new();

pub fn kernel_root() -> u64 {
    *KERNEL_ROOT.expect()
}

/// Begins the kernel's address space: the tables of the shared kernel
/// half, and the direct map of physical memory below `direct_map_limit`
/// (1 GiB pages when the processor has them, otherwise 2 MiB pages). What
/// else the kernel maps goes in with [`map`] before [`install_kernel_space`].
pub fn new_kernel_space(direct_map_limit: u64) -> u64 {
    let pml4 = phys::alloc_zeroed().expect("out of memory for the kernel PML4");
    let pml4_t = table(pml4);
    for entry in &mut pml4_t[256..512] {
        let pdpt = phys::alloc_zeroed().expect("out of memory for kernel PDPTs");
        *entry = pdpt | PRESENT | WRITABLE;
    }
    let huge_1g = super::cpu::features().page_1g;
    let base = bootinfo::HHDM_BASE;
    let mut addr = 0u64;
    while addr < direct_map_limit {
        let virt = base + addr;
        let pdpt = table(pml4_t[index(virt, 3)] & ADDR_MASK);
        let flags = PRESENT | WRITABLE | GLOBAL | NO_EXECUTE;
        if huge_1g {
            pdpt[index(virt, 2)] = addr | flags | HUGE;
            addr += 1 << 30;
        } else {
            let pd = next_table(pdpt, index(virt, 2), false).expect("out of memory for the direct map");
            for i in 0..512u64 {
                table(pd)[i as usize] = (addr + i * (2 << 20)) | flags | HUGE;
            }
            addr += 1 << 30;
        }
    }
    pml4
}

/// Makes `root`, made by [`new_kernel_space`], the kernel's address space,
/// and switches to it.
pub fn install_kernel_space(root: u64) {
    KERNEL_ROOT.set(root);
    // SAFETY: the new tables map the kernel image, the direct map (which
    // holds the current stack) and everything the kernel references.
    unsafe { super::cpu::write_cr3(root) };
}

/// Creates a new address space sharing the kernel half.
pub fn new_user_root() -> Option<u64> {
    let pml4 = phys::alloc_zeroed()?;
    let src = table(kernel_root());
    let dst = table(pml4);
    dst[256..512].copy_from_slice(&src[256..512]);
    Some(pml4)
}

/// Frees all page-table pages of the user half (not the mapped frames) and
/// the PML4 itself.
pub fn free_user_root(root: u64) {
    fn free_level(t: u64, level: u32) {
        if level > 0 {
            for &e in table(t).iter() {
                if e & PRESENT != 0 && e & HUGE == 0 {
                    free_level(e & ADDR_MASK, level - 1);
                }
            }
        }
        phys::free(t);
    }
    for slot in 0..256 {
        let e = table(root)[slot];
        if e & PRESENT != 0 {
            free_level(e & ADDR_MASK, 2);
        }
    }
    phys::free(root);
}

/// Loads the address space `root` on this CPU (if it is not loaded).
pub fn activate(root: u64) {
    if super::cpu::read_cr3() != root {
        // SAFETY: every address space shares the kernel half, so the kernel
        // keeps running after the switch.
        unsafe { super::cpu::write_cr3(root) };
    }
}
