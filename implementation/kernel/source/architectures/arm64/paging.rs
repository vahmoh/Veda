// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! AArch64 translation tables: four levels, 4 KiB granules, 48-bit
//! addresses.
//!
//! An address space is its `TTBR0` value: the physical address of its
//! top-level table and its ASID, which the rest of the kernel takes as an
//! opaque root. The kernel's half of addresses is `TTBR1`'s, in every
//! address space alike, so nothing of it is copied or shared.
//!
//! The TLB keeps user translations of many address spaces, told apart by
//! ASID, so changing address spaces flushes nothing. Each address space has
//! an ASID of its own while there are enough of them; past that, address
//! spaces share ASID 0, and a CPU forgets ASID 0's translations whenever
//! it loads one, as if it had no ASIDs.
//!
//! Tables are written while processors walk them, and unlike a PC's they
//! may be read by the walker, or held in its TLB, in any order the
//! architecture allows. So every change ends with a barrier that lets
//! walkers see it, a valid entry never changes address without first being
//! invalid in every TLB (break-before-make), and a page fault does not
//! itself forget what the TLB held: overwriting a mapping invalidates it.

use alloc::vec::Vec;
use core::arch::asm;
use core::sync::atomic::Ordering;

use bootinfo::architecture::memory_attribute;
use bootinfo::{BootInfo, MemoryKind};

use super::cpu::{read_register, write_register};
use crate::memory::aspace::Perms;
use crate::memory::paging::{Cache, Flags, MapError};
use crate::memory::{phys, phys_to_virt};
use crate::synchronization::{Once, SpinLock};

/// The size of a page.
pub const PAGE_SIZE: u64 = 4096;

const VALID: u64 = 1 << 0;
/// In an entry of levels 3 to 1, a table below (otherwise a block); in one
/// of level 0, always set: a page.
pub(in crate::architecture) const TABLE_OR_PAGE: u64 = 1 << 1;
const USER: u64 = 1 << 6;
const READ_ONLY: u64 = 1 << 7;
const INNER_SHAREABLE: u64 = 3 << 8;
const ACCESSED: u64 = 1 << 10;
const NOT_GLOBAL: u64 = 1 << 11;
const NOT_EXECUTABLE_KERNEL: u64 = 1 << 53;
const NOT_EXECUTABLE_USER: u64 = 1 << 54;
const ADDR_MASK: u64 = 0x0000_FFFF_FFFF_F000;
const GIB: u64 = 1 << 30;
const MIB2: u64 = 2 << 20;

/// Where the ASID is in a root.
const ASID_SHIFT: u32 = 48;

/// A memory type's bits in an entry: its index in `MAIR`.
fn cache_bits(cache: Cache) -> u64 {
    let index = match cache {
        Cache::WriteBack => memory_attribute::WRITE_BACK,
        Cache::WriteCombining => memory_attribute::NON_CACHEABLE,
        Cache::Uncached => memory_attribute::DEVICE,
    };
    index << 2
}

/// A leaf entry's bits for `flags`. Nothing is executable both by the
/// kernel and by user code. User mappings are never global; the kernel's
/// that are not (its stacks) are held by the TLB for each ASID, and forgotten
/// for all of them (`tlb::shootdown`).
fn bits(flags: Flags) -> u64 {
    let mut b = VALID | TABLE_OR_PAGE | ACCESSED | INNER_SHAREABLE | cache_bits(flags.cache);
    if !flags.writable {
        b |= READ_ONLY;
    }
    if flags.user || !flags.global {
        b |= NOT_GLOBAL;
    }
    if flags.user {
        b |= USER | NOT_EXECUTABLE_KERNEL;
        if !flags.executable {
            b |= NOT_EXECUTABLE_USER;
        }
    } else {
        b |= NOT_EXECUTABLE_USER;
        if !flags.executable {
            b |= NOT_EXECUTABLE_KERNEL;
        }
    }
    b
}

/// The flags a leaf entry has.
fn flags_of(entry: u64) -> Flags {
    let user = entry & USER != 0;
    let index = (entry >> 2) & 7;
    let cache = if index == memory_attribute::WRITE_BACK {
        Cache::WriteBack
    } else if index == memory_attribute::NON_CACHEABLE {
        Cache::WriteCombining
    } else {
        Cache::Uncached
    };
    Flags {
        writable: entry & READ_ONLY == 0,
        executable: entry & if user { NOT_EXECUTABLE_USER } else { NOT_EXECUTABLE_KERNEL } == 0,
        user,
        global: entry & NOT_GLOBAL == 0,
        cache,
    }
}

/// What the tables grant for `perms`: a page user code can write it can
/// read, and Veda gives programs no execute-only pages, so any access at
/// all lets them be read.
pub fn effective(perms: Perms) -> Perms {
    Perms { read: perms.read || perms.write || perms.exec, ..perms }
}

/// The table in frame `frame`.
fn table(frame: u64) -> &'static mut [u64; 512] {
    // SAFETY: table pages are direct-mapped and owned by the paging code.
    unsafe { &mut *(phys_to_virt(frame) as *mut [u64; 512]) }
}

#[inline]
fn index(virt: u64, level: u32) -> usize {
    ((virt >> (12 + 9 * level)) & 0x1FF) as usize
}

/// Writes table entry `e`, so that it reaches memory in one piece and in
/// its place among the others.
#[inline]
fn set(e: &mut u64, value: u64) {
    // SAFETY: a valid reference.
    unsafe { core::ptr::write_volatile(e, value) };
}

/// Lets walkers see the tables as they now are, before anything that
/// follows on this processor.
#[inline]
fn publish() {
    // SAFETY: barriers only order.
    unsafe { asm!("dsb ishst", "isb", options(nostack, preserves_flags)) };
}

/// Whether an entry of level 3 to 1 is a block rather than a table.
#[inline]
fn is_block(e: u64) -> bool {
    e & TABLE_OR_PAGE == 0
}

/// Returns the next-level table for `entry`, allocating it if needed.
fn next_table(t: &mut [u64; 512], idx: usize) -> Result<u64, MapError> {
    if t[idx] & VALID == 0 {
        let frame = phys::alloc_zeroed().ok_or(MapError::NoMemory)?;
        // The new table's zeroes reach walkers before the entry that leads
        // to it.
        publish();
        set(&mut t[idx], frame | VALID | TABLE_OR_PAGE);
    }
    debug_assert!(!is_block(t[idx]), "walking through a block");
    Ok(t[idx] & ADDR_MASK)
}

/// Maps one page in the address space `root`. A page that user code will
/// run is made ready to run.
pub fn map(root: u64, virt: u64, phys_addr: u64, flags: Flags, overwrite: bool) -> Result<(), MapError> {
    let l2 = next_table(table(root & ADDR_MASK), index(virt, 3))?;
    let l1 = next_table(table(l2), index(virt, 2))?;
    let l0 = next_table(table(l1), index(virt, 1))?;
    let e = &mut table(l0)[index(virt, 0)];
    let new = (phys_addr & ADDR_MASK) | bits(flags);
    if *e & VALID != 0 {
        if !overwrite {
            return Err(MapError::AlreadyMapped);
        }
        if *e == new {
            return Ok(());
        }
        set(e, 0);
        super::tlb::forget_page(root, virt);
    }
    if flags.user && flags.executable {
        super::memory::sync_instructions(phys_to_virt(phys_addr & ADDR_MASK), PAGE_SIZE);
    }
    // What is in the page, which may have been zeroed only now, reaches the
    // other processors before the entry that leads them to it does.
    // SAFETY: a barrier only orders.
    unsafe { asm!("dmb ishst", options(nostack, preserves_flags)) };
    set(e, new);
    publish();
    Ok(())
}

/// Removes the mapping of the page at `virt`, returning the frame it
/// mapped. The caller is responsible for TLB invalidation.
pub fn unmap(root: u64, virt: u64) -> Option<u64> {
    let mut t = table(root & ADDR_MASK);
    for level in (1..=3).rev() {
        let e = t[index(virt, level)];
        if e & VALID == 0 || is_block(e) {
            return None;
        }
        t = table(e & ADDR_MASK);
    }
    let e = &mut t[index(virt, 0)];
    if *e & VALID == 0 {
        return None;
    }
    let frame = *e & ADDR_MASK;
    set(e, 0);
    Some(frame)
}

/// Whether the kernel's copies may touch memory mapped with `flags`: they
/// reach any address in any size, which device memory faults on.
pub fn kernel_copies(flags: &Flags) -> bool {
    flags.cache != Cache::Uncached
}

/// Translates `virt`, returning the physical address and the mapping's
/// flags.
pub fn translate(root: u64, virt: u64) -> Option<(u64, Flags)> {
    let mut t = table(root & ADDR_MASK);
    for level in (1..=3).rev() {
        let e = t[index(virt, level)];
        if e & VALID == 0 {
            return None;
        }
        if is_block(e) {
            let size = 1u64 << (12 + 9 * level);
            return Some(((e & ADDR_MASK & !(size - 1)) + (virt & (size - 1)), flags_of(e)));
        }
        t = table(e & ADDR_MASK);
    }
    let e = t[index(virt, 0)];
    if e & VALID == 0 { None } else { Some(((e & ADDR_MASK) + (virt & 0xFFF), flags_of(e))) }
}

/// Calls `f` on each valid leaf entry of `[start, end)` (page-aligned, in
/// the user half), skipping the tables that are not there: a walk over a
/// large range that is mostly unused (a reservation, say) costs what is
/// mapped in it, not its size.
fn for_each_leaf(root: u64, start: u64, end: u64, f: &mut dyn FnMut(&mut u64)) {
    fn walk(t: u64, level: u32, base: u64, start: u64, end: u64, f: &mut dyn FnMut(&mut u64)) {
        // The bytes one entry of this level covers.
        let span = 1u64 << (12 + 9 * level);
        let first = (start.max(base) - base) / span;
        let last = (end.min(base + 512 * span) - base).div_ceil(span);
        for i in first..last {
            let e = &mut table(t)[i as usize];
            if *e & VALID == 0 {
                continue;
            }
            if level == 0 {
                f(e);
            } else if !is_block(*e) {
                walk(*e & ADDR_MASK, level - 1, base + i * span, start, end, f);
            }
        }
    }
    if start < end {
        walk(root & ADDR_MASK, 3, 0, start, end, f);
    }
}

/// Removes the mappings of the pages in `[start, end)`. The caller is
/// responsible for TLB invalidation.
pub fn unmap_range(root: u64, start: u64, end: u64) {
    for_each_leaf(root, start, end, &mut |e| set(e, 0));
}

/// Changes the flags of the pages mapped in `[start, end)`. The caller is
/// responsible for TLB invalidation. Pages that become runnable by user
/// code are made ready to run.
pub fn protect_range(root: u64, start: u64, end: u64, flags: Flags) {
    let leaf = bits(flags);
    for_each_leaf(root, start, end, &mut |e| {
        let frame = *e & ADDR_MASK;
        if flags.user && flags.executable && *e & NOT_EXECUTABLE_USER != 0 {
            super::memory::sync_instructions(phys_to_virt(frame), PAGE_SIZE);
        }
        set(e, frame | leaf);
    });
    publish();
}

/// The kernel's own tables: those of its half of addresses.
static KERNEL_ROOT: Once<u64> = Once::new();

/// A table with nothing in it, which `TTBR0` holds when no address space is
/// loaded, with ASID 0.
static EMPTY_ROOT: Once<u64> = Once::new();

pub fn kernel_root() -> u64 {
    *KERNEL_ROOT.expect()
}

/// The `TTBR0` that means no address space, for processors as they start
/// (`smp`).
pub(in crate::architecture) fn empty_root() -> u64 {
    *EMPTY_ROOT.expect()
}

/// Maps physical `[start, end)` in the direct map, in the largest blocks
/// that fit. Not executable, as its pages may hold anything.
fn map_direct(root: u64, start: u64, end: u64) {
    let block =
        VALID | ACCESSED | INNER_SHAREABLE | cache_bits(Cache::WriteBack) | NOT_EXECUTABLE_KERNEL | NOT_EXECUTABLE_USER;
    let page = block | TABLE_OR_PAGE;
    let out_of_memory = "out of memory for the direct map";
    let mut phys_addr = start;
    while phys_addr < end {
        let virt = bootinfo::HHDM_BASE + phys_addr;
        let left = end - phys_addr;
        let l2 = next_table(table(root), index(virt, 3)).expect(out_of_memory);
        if phys_addr.is_multiple_of(GIB) && left >= GIB {
            table(l2)[index(virt, 2)] = phys_addr | block;
            phys_addr += GIB;
            continue;
        }
        let l1 = next_table(table(l2), index(virt, 2)).expect(out_of_memory);
        if phys_addr.is_multiple_of(MIB2) && left >= MIB2 {
            table(l1)[index(virt, 1)] = phys_addr | block;
            phys_addr += MIB2;
            continue;
        }
        let l0 = next_table(table(l1), index(virt, 1)).expect(out_of_memory);
        table(l0)[index(virt, 0)] = phys_addr | page;
        phys_addr += PAGE_SIZE;
    }
}

/// Begins the kernel's address space: the direct map of the memory of the
/// boot information's map. Nothing else goes in it, as a processor may
/// read ahead in what is mapped as memory: devices' registers are mapped
/// apart, as what they are. What else the kernel maps goes in with [`map`]
/// before [`install_kernel_space`].
pub fn new_kernel_space(boot: &BootInfo) -> u64 {
    let root = phys::alloc_zeroed().expect("out of memory for the kernel's tables");
    // SAFETY: the loader built a valid memory map.
    let regions = unsafe { boot.memory_map.as_slice() };
    let (mut start, mut end) = (0, 0);
    let memory =
        regions.iter().filter(|r| !matches!(r.kind, MemoryKind::Mmio | MemoryKind::Reserved | MemoryKind::Unusable));
    for r in memory {
        if r.base != end {
            map_direct(root, start, end);
            start = r.base;
        }
        end = r.end();
    }
    map_direct(root, start, end);
    root
}

unsafe extern "C" {
    /// Replaces `TTBR1`, from code at an address `TTBR0` maps.
    fn vk_replace_kernel_tables(new: u64, empty: u64);
}

// `TTBR1` goes through an empty table, in the TLB as well: the new tables
// map what the loader's do, perhaps in pages of other sizes, and the two
// must never be in the TLB at once (break-before-make). Nothing here is in
// the high half, and no memory is touched meanwhile: the stack is there.
core::arch::global_asm!(
    r#"
    .section .text
    .balign 8
    .globl vk_replace_kernel_tables
vk_replace_kernel_tables:
    msr     ttbr1_el1, x1
    isb
    tlbi    vmalle1
    dsb     nsh
    isb
    msr     ttbr1_el1, x0
    isb
    tlbi    vmalle1
    dsb     nsh
    isb
    ret
"#
);

/// Makes `root`, made by [`new_kernel_space`], the kernel's address space,
/// switches to it, and drops the loader's tables of low addresses.
pub fn install_kernel_space(root: u64) {
    KERNEL_ROOT.set(root);
    let empty = phys::alloc_zeroed().expect("out of memory for the kernel's tables");
    EMPTY_ROOT.set(empty);
    publish();
    let kernel = super::kernel_image();
    let replace = vk_replace_kernel_tables as *const () as u64 - kernel.virt_base + kernel.phys_base;
    // SAFETY: the loader's low tables run the replacing code where it is
    // in physical memory; the new tables map the kernel image, the direct
    // map (which holds the current stack) and everything the kernel
    // references, as the loader's do.
    unsafe {
        let replace = core::mem::transmute::<u64, unsafe extern "C" fn(u64, u64)>(replace);
        replace(root, empty);
        write_ttbr0(empty);
        asm!("tlbi vmalle1", "dsb nsh", "isb", options(nostack, preserves_flags));
    }
}

/// # Safety
/// `value` must be a valid `TTBR0` for what follows.
unsafe fn write_ttbr0(value: u64) {
    // SAFETY: forwarded to the caller.
    unsafe {
        write_register!("ttbr0_el1", value);
        asm!("isb", options(nostack, preserves_flags));
    }
}

/// The ASIDs that are free: from 1 up (0 is shared, and no address space's
/// when there are enough), those of address spaces that have ended.
struct Asids {
    next: u32,
    free: Vec<u16>,
}

static ASIDS: SpinLock<Asids> = SpinLock::new(Asids { next: 1, free: Vec::new() });

/// The ASID of the address space `root`.
pub(in crate::architecture) fn asid_of(root: u64) -> u16 {
    (root >> ASID_SHIFT) as u16
}

/// An ASID for a new address space; ASID 0, shared, when none is left.
fn take_asid() -> u16 {
    let limit = if super::cpu::features().asid16 { 1 << 16 } else { 1 << 8 };
    let mut asids = ASIDS.lock();
    asids.free.pop().unwrap_or_else(|| {
        if asids.next < limit {
            asids.next += 1;
            (asids.next - 1) as u16
        } else {
            0
        }
    })
}

/// Creates a new address space.
pub fn new_user_root() -> Option<u64> {
    let frame = phys::alloc_zeroed()?;
    publish();
    Some(frame | (take_asid() as u64) << ASID_SHIFT)
}

/// Frees all table pages of the address space (not the mapped frames), and
/// its ASID, which no TLB keeps translations of from then on.
pub fn free_user_root(root: u64) {
    fn free_level(t: u64, level: u32) {
        if level > 0 {
            for &e in table(t).iter() {
                if e & VALID != 0 && !is_block(e) {
                    free_level(e & ADDR_MASK, level - 1);
                }
            }
        }
        phys::free(t);
    }
    super::tlb::forget_space(root);
    free_level(root & ADDR_MASK, 3);
    let asid = asid_of(root);
    if asid != 0 {
        ASIDS.lock().free.push(asid);
    }
}

/// Loads the address space `root` on this CPU (if it is not loaded), with
/// the BKL held: memory barriers for an address space go to the CPUs that
/// have it loaded (`tlb::barrier_space`). The kernel's own has nothing of
/// the low half.
pub fn activate(root: u64) {
    let ttbr = if root == kernel_root() { empty_root() } else { root };
    if read_register!("ttbr0_el1") != ttbr {
        // SAFETY: the kernel runs in the other half of addresses, which
        // this does not change.
        unsafe { write_ttbr0(ttbr) };
        if asid_of(ttbr) == 0 {
            super::tlb::forget_shared_asid();
        }
    }
    super::percpu::current().loaded_root.store(root, Ordering::Release);
}
