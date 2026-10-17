// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Booting Linux on the guest the x86 way (`hv::amd64::linux`).
//!
//! The guest's first 64 KiB hold what the kernel is entered with (a GDT,
//! page tables that map the first 4 GiB as they are, the boot parameters
//! and the command line) and stay reserved: the processors Linux starts
//! later start on the same page tables. As on a PC, the memory map leaves
//! out the legacy area below 1 MiB, where the ACPI tables are (from
//! 0xE0000) and an Intel GPU's OpRegion. The kernel goes where it prefers
//! to be, the initial RAM file system at the top of memory, between them
//! the room the kernel unpacks itself into.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;

use abi::VcpuState;
use hv::amd64::linux::{self, E820_RAM, E820_RESERVED, MemoryRange};

pub use hv::amd64::linux::Kernel;

use super::{ACPI_AT, OPREGION_AT};
use crate::memory::GuestMemory;

const MIB: u64 = 1 << 20;
const GDT_AT: u64 = 0x1000;
const PAGE_TABLES_AT: u64 = 0x2000;
const BOOT_PARAMS_AT: u64 = 0x8000;
const CMDLINE_AT: u64 = 0x9000;
/// The loader's structures, which stay reserved.
const LOW_RESERVED: u64 = 0x1_0000;
/// The legacy video and firmware area below 1 MiB.
const LEGACY_START: u64 = 0x9_F000;
const LEGACY_END: u64 = 0x10_0000;
/// The register of the boot parameters at entry.
const RSI: usize = 6;

/// The kernel's image (a bzImage), as booting it needs it.
pub fn parse(image: &[u8]) -> Result<Kernel<'_>, String> {
    linux::parse(image).map_err(|e| format!("the kernel: {e}"))
}

/// What the guest boots: its memory (`size` bytes, the reserved ranges
/// in it), the kernel, its initial RAM file system and command line, and
/// what the machine puts below 1 MiB.
pub struct Load<'a> {
    pub memory: &'a GuestMemory,
    pub size: u64,
    pub reserved: &'a [Range<u64>],
    pub kernel: &'a Kernel<'a>,
    pub initramfs: &'a [u8],
    pub cmdline: &'a str,
    pub acpi_tables: &'a [u8],
    pub opregion: Option<&'a [u8]>,
}

/// Loads Linux into the guest's memory: the state its first processor
/// starts in.
pub fn load(l: &Load) -> Result<VcpuState, String> {
    let image = l.kernel;
    let too_little = || format!("{} MiB is too little memory for the guest", l.size / MIB);
    let kernel_at = image.preferred_address.max(LEGACY_END);
    let initrd_at = l.size.checked_sub(l.initramfs.len() as u64).ok_or_else(too_little)? & !0xFFF;
    let kernel_end = kernel_at.checked_add(image.init_size.max(image.protected.len() as u64));
    if kernel_end.is_none_or(|end| end > initrd_at) {
        return Err(too_little());
    }
    if l.cmdline.len() > image.cmdline_size as usize || l.cmdline.len() >= 4096 {
        return Err(String::from("the kernel's command line is too long"));
    }
    let mut cmdline = String::from(l.cmdline);
    cmdline.push('\0');
    let mut ranges = alloc::vec![
        MemoryRange { start: 0, len: LOW_RESERVED, kind: E820_RESERVED },
        MemoryRange { start: LOW_RESERVED, len: LEGACY_START - LOW_RESERVED, kind: E820_RAM },
        MemoryRange { start: LEGACY_START, len: LEGACY_END - LEGACY_START, kind: E820_RESERVED },
        MemoryRange { start: LEGACY_END, len: l.size - LEGACY_END, kind: E820_RAM },
    ];
    ranges.extend(l.reserved.iter().map(|r| MemoryRange { start: r.start, len: r.end - r.start, kind: E820_RESERVED }));
    ranges.sort_unstable_by_key(|r| r.start);
    let boot = linux::Boot {
        kernel: image,
        kernel_at,
        cmdline_at: CMDLINE_AT,
        initrd: Some((initrd_at, l.initramfs.len() as u64)),
        memory: &ranges,
        rsdp: Some(ACPI_AT),
    };
    let memory = l.memory;
    let gdt: Vec<u8> = linux::GDT.iter().flat_map(|e| e.to_le_bytes()).collect();
    let tables = linux::identity_page_tables(PAGE_TABLES_AT);
    let mut loaded = memory.write(GDT_AT, &gdt);
    for (i, t) in tables.iter().enumerate() {
        let bytes: Vec<u8> = t.iter().flat_map(|e| e.to_le_bytes()).collect();
        loaded &= memory.write(PAGE_TABLES_AT + i as u64 * 0x1000, &bytes);
    }
    loaded &= memory.write(BOOT_PARAMS_AT, &linux::boot_params(&boot));
    loaded &= memory.write(CMDLINE_AT, cmdline.as_bytes());
    loaded &= memory.write(ACPI_AT, l.acpi_tables);
    if let Some(opregion) = l.opregion {
        loaded &= memory.write(OPREGION_AT, opregion);
    }
    loaded &= memory.write(kernel_at, image.protected);
    loaded &= memory.write(initrd_at, l.initramfs);
    if !loaded {
        return Err(String::from("the kernel does not fit the guest's memory"));
    }
    let mut state = linux::long_mode(linux::entry(kernel_at), PAGE_TABLES_AT, GDT_AT);
    state.gprs[RSI] = BOOT_PARAMS_AT;
    Ok(state)
}

/// The state a processor the guest starts (`start_cpu`) starts in, at
/// `entry`: 64-bit mode on the page tables the first started on.
pub fn secondary(entry: u64) -> VcpuState {
    linux::long_mode(entry, PAGE_TABLES_AT, GDT_AT)
}
