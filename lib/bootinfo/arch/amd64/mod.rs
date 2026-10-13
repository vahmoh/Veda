// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The boot handoff protocol on AMD64.
//!
//! The loader enters the kernel in 64-bit long mode, with interrupts
//! disabled, no-execute pages and supervisor write protection enabled,
//! through `extern "sysv64" fn(&'static BootInfo) -> !` (the `BootInfo` in
//! `rdi`), on page tables in which the direct map, at [`HHDM_BASE`], covers
//! all RAM and the low 4 GiB (where a PC's devices' registers are), and the
//! kernel is in the top 2 GiB, at [`KERNEL_BASE`].

/// Virtual base address of the higher-half direct map of physical memory.
pub const HHDM_BASE: u64 = 0xFFFF_8000_0000_0000;

/// Link address of the kernel image.
pub const KERNEL_BASE: u64 = 0xFFFF_FFFF_8000_0000;

/// The processor's memory types (the MTRRs' and PAT's encodings).
pub mod memory_type {
    pub const UNCACHED: u8 = 0;
    pub const WRITE_COMBINING: u8 = 1;
    pub const WRITE_THROUGH: u8 = 4;
    pub const WRITE_PROTECTED: u8 = 5;
    pub const WRITE_BACK: u8 = 6;
    /// The PAT's UC- (uncached unless the MTRRs say write-combining).
    pub const UNCACHED_MINUS: u8 = 7;
    /// The loader could not tell.
    pub const UNKNOWN: u8 = 0xFF;

    /// A name for the log.
    pub fn name(t: u8) -> &'static str {
        match t {
            UNCACHED | UNCACHED_MINUS => "uncached",
            WRITE_COMBINING => "write-combining",
            WRITE_THROUGH => "write-through",
            WRITE_PROTECTED => "write-protected",
            WRITE_BACK => "write-back",
            _ => "of an unknown memory type",
        }
    }
}
