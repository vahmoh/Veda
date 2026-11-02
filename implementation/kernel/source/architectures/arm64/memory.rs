// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Memory on ARM64, beyond the translation tables: the layout of the
//! kernel's address space, and the caches.

use core::arch::asm;

use super::cpu::{self, read_register};

/// Physical memory below this is never handed out: only the first page,
/// where it is memory at all, as 0 is no address for a frame.
pub const RESERVED_BELOW: u64 = 0x1000;

/// Physical addresses no driver may map, besides the registers the kernel
/// maps for itself (`kvirt::map_mmio` notes those, its interrupt
/// controllers' among them): nothing else.
pub const RESERVED_MMIO: core::ops::Range<u64> = 0..0;

/// The kernel's window for devices' registers (as on AMD64).
pub const MMIO_BASE: u64 = 0xFFFF_C000_0000_0000;
pub const MMIO_SIZE: u64 = 64 << 30;
/// Where kernel stacks are.
pub const STACKS_BASE: u64 = 0xFFFF_C800_0000_0000;
pub const STACKS_SIZE: u64 = 1 << 39;

/// Whether the direct map has `phys`: RAM, as devices' registers are not
/// to be fetched from.
pub fn in_direct_map(phys: u64) -> bool {
    super::paging::translate(super::paging::kernel_root(), crate::memory::phys_to_virt(phys)).is_some()
}

/// Fills `len` bytes (a multiple of 16, from a multiple of 16) at `virt`
/// with zeroes.
pub fn zero(virt: u64, len: u64) {
    // SAFETY: the caller's memory, mapped writable at `virt`.
    unsafe {
        asm!(
            "2:",
            "stp xzr, xzr, [{at}], #16",
            "subs {left}, {left}, #16",
            "b.ne 2b",
            at = inout(reg) virt => _,
            left = inout(reg) len => _,
            options(nostack),
        );
    }
}

/// The smallest line of the data and of the instruction caches.
fn cache_lines() -> (u64, u64) {
    let ctr = read_register!("ctr_el0");
    (4 << ((ctr >> 16) & 0xF), 4 << (ctr & 0xF))
}

/// Writes back and evicts every cache line of `[virt, virt + len)`, then
/// waits: memory then holds what was written there, and no line of it is
/// left in a cache.
pub fn flush_cache_range(virt: u64, len: u64) {
    let (line, _) = cache_lines();
    for p in (virt & !(line - 1)..virt + len).step_by(line as usize) {
        // SAFETY: flushing a mapped line changes no data.
        unsafe { asm!("dc civac, {}", in(reg) p, options(nostack, preserves_flags)) };
    }
    // SAFETY: a barrier only orders memory accesses.
    unsafe { asm!("dsb sy", options(nostack, preserves_flags)) };
}

/// Makes the instruction caches see what `[virt, virt + len)` holds, which
/// they would not by themselves: for memory about to be run, whichever
/// processor wrote it. `CTR_EL0` says where the processor needs neither
/// step.
pub(in crate::architecture) fn sync_instructions(virt: u64, len: u64) {
    /// `CTR_EL0.IDC`, `DIC`: the data cache need not be cleaned, or the
    /// instruction cache invalidated, for the two to agree.
    const IDC: u64 = 1 << 28;
    const DIC: u64 = 1 << 29;
    let ctr = read_register!("ctr_el0");
    let (data, instruction) = cache_lines();
    if ctr & IDC == 0 {
        for p in (virt & !(data - 1)..virt + len).step_by(data as usize) {
            // SAFETY: cleaning a mapped line changes no data.
            unsafe { asm!("dc cvau, {}", in(reg) p, options(nostack, preserves_flags)) };
        }
    }
    // SAFETY: barriers only order; invalidating an instruction cache line
    // of a mapped address loses nothing.
    unsafe {
        asm!("dsb ish", options(nostack, preserves_flags));
        if ctr & DIC == 0 {
            for p in (virt & !(instruction - 1)..virt + len).step_by(instruction as usize) {
                asm!("ic ivau, {}", in(reg) p, options(nostack, preserves_flags));
            }
            asm!("dsb ish", options(nostack, preserves_flags));
        }
        asm!("isb", options(nostack, preserves_flags));
    }
}

/// Temporarily allows supervisor access to user pages (PAN).
///
/// This and [`user_access_end`] are barriers to the compiler: the user
/// accesses between them stay between them.
#[inline]
pub fn user_access_begin() {
    if cpu::features().pan {
        // SAFETY: the processor has PAN.
        unsafe { cpu::set_pan(false) };
    }
}

/// Ends a [`user_access_begin`] section.
#[inline]
pub fn user_access_end() {
    if cpu::features().pan {
        // SAFETY: the processor has PAN.
        unsafe { cpu::set_pan(true) };
    }
}
