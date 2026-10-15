// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Memory on AMD64, beyond the page tables: the layout of the kernel's
//! address space, the physical memory the architecture keeps for itself,
//! and the caches.

use core::arch::asm;

use super::cpu;

/// Physical memory below this is never handed out: the real-mode area,
/// where the trampoline that starts the other CPUs runs (`smp`).
pub const RESERVED_BELOW: u64 = 0x10_0000;

/// Physical addresses no driver may map, besides the registers the kernel
/// maps for itself (`kvirt::map_mmio` notes those): the processors' local
/// APICs' window, which is also where devices' interrupt messages go.
pub const RESERVED_MMIO: core::ops::Range<u64> = 0xFEE0_0000..0xFF00_0000;

/// The kernel's window for devices' registers (PML4 slot 384).
pub const MMIO_BASE: u64 = 0xFFFF_C000_0000_0000;
pub const MMIO_SIZE: u64 = 64 << 30;
/// Where kernel stacks are (PML4 slot 400).
pub const STACKS_BASE: u64 = 0xFFFF_C800_0000_0000;
pub const STACKS_SIZE: u64 = 1 << 39;

/// Fills `len` bytes (a multiple of 8) at `virt` with zeroes.
pub fn zero(virt: u64, len: u64) {
    // SAFETY: the caller's memory, mapped writable at `virt`.
    unsafe {
        asm!("rep stosq", inout("rdi") virt => _, inout("rcx") len / 8 => _, in("rax") 0u64,
            options(nostack, preserves_flags));
    }
}

/// Writes back and evicts every cache line of `[virt, virt + len)`
/// (CLFLUSH), then fences: memory then holds what was written there, and
/// no line of it is left in a cache. x86-64 processors' lines are at least
/// 64 bytes, so flushing every 64 bytes reaches each.
pub fn flush_cache_range(virt: u64, len: u64) {
    const LINE: u64 = 64;
    let mut p = virt & !(LINE - 1);
    while p < virt + len {
        // SAFETY: flushing a mapped line changes no data.
        unsafe { asm!("clflush [{}]", in(reg) p, options(nostack, preserves_flags)) };
        p += LINE;
    }
    // SAFETY: a fence only orders memory accesses.
    unsafe { asm!("mfence", options(nostack, preserves_flags)) };
}

/// Temporarily allows supervisor access to user pages (SMAP).
///
/// This and [`user_access_end`] are barriers to the compiler (their `asm!`
/// may touch memory, as far as it knows): the user accesses between them
/// stay between them.
#[inline]
pub fn user_access_begin() {
    if cpu::features().smap {
        // SAFETY: STAC only toggles RFLAGS.AC.
        unsafe { asm!("stac", options(nostack)) };
    }
}

/// Ends a [`user_access_begin`] section.
#[inline]
pub fn user_access_end() {
    if cpu::features().smap {
        // SAFETY: CLAC only toggles RFLAGS.AC.
        unsafe { asm!("clac", options(nostack)) };
    }
}
