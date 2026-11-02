// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Booting Linux on the guest Arm's way (`hv::arm64::linux`): its `Image` a
//! little way into RAM, on a 2 MiB boundary as it asks, the initial RAM
//! file system at the top of memory, and below that the device tree that
//! describes the machine (`hv::arm64::fdt`). There is no firmware, and the
//! ACPI tables go unused.

use alloc::format;
use alloc::string::String;
use core::ops::Range;

use abi::VcpuState;
use hv::arm64::fdt;
use hv::arm64::linux::{self, ALIGNMENT, Image};

use super::PCI_LOW;
use crate::machine::PCI_HIGH;
use crate::memory::GuestMemory;

const MIB: u64 = 1 << 20;
/// A device tree is on an 8-byte boundary.
const FDT_ALIGNMENT: u64 = 8;

/// The kernel's image, as booting it needs it.
pub struct Kernel<'a> {
    file: &'a [u8],
    image: Image,
}

/// The kernel's image (an `Image`).
pub fn parse(file: &[u8]) -> Result<Kernel<'_>, String> {
    let image = linux::parse(file).map_err(|e| format!("the kernel: {e}"))?;
    Ok(Kernel { file, image })
}

/// What the guest boots: its memory (`size` bytes, the reserved ranges in
/// it), the kernel, its initial RAM file system and command line, and the
/// PCI functions it has (where each is on bus 0), which its device tree
/// describes. What the other architecture's machine puts below 1 MiB, and
/// its ACPI tables, are not for this one.
pub struct Load<'a> {
    pub memory: &'a GuestMemory,
    pub size: u64,
    pub reserved: &'a [Range<u64>],
    pub kernel: &'a Kernel<'a>,
    pub initramfs: &'a [u8],
    pub cmdline: &'a str,
    pub acpi_tables: &'a [u8],
    pub opregion: Option<&'a [u8]>,
    pub functions: &'a [u8],
}

/// Loads Linux into the guest's memory: the state its first processor
/// starts in. `cpus` is how many it has, which its device tree says.
pub fn load(l: &Load, cpus: u32) -> Result<VcpuState, String> {
    let too_little = || format!("{} MiB is too little memory for the guest", l.size / MIB);
    if !l.reserved.is_empty() || l.opregion.is_some() {
        return Err(String::from("the guest has no room for what a PC's firmware keeps"));
    }
    let _ = l.acpi_tables;
    let kernel_at = ALIGNMENT + l.kernel.image.text_offset;
    let initrd_at = l.size.checked_sub(l.initramfs.len() as u64).ok_or_else(too_little)? & !0xFFF;
    let tree = fdt::describe(&fdt::Machine {
        cpus,
        memory: l.size,
        cmdline: l.cmdline,
        initramfs: (initrd_at, initrd_at + l.initramfs.len() as u64),
        pci_low: (PCI_LOW.start, PCI_LOW.end),
        pci_high: (PCI_HIGH.start, PCI_HIGH.end),
        functions: l.functions,
    });
    let fdt_at = initrd_at.checked_sub(tree.len() as u64).ok_or_else(too_little)? & !(FDT_ALIGNMENT - 1);
    if kernel_at.checked_add(l.kernel.image.memory_size).is_none_or(|end| end > fdt_at) {
        return Err(too_little());
    }
    let wrote = l.memory.write(kernel_at, l.kernel.file)
        && l.memory.write(fdt_at, &tree)
        && l.memory.write(initrd_at, l.initramfs);
    if !wrote {
        return Err(too_little());
    }
    Ok(linux::state(kernel_at, fdt_at))
}

/// The state a processor starts in, at `entry`, with `context` in its first
/// register, as PSCI's `CPU_ON` has it.
pub fn secondary(entry: u64, context: u64) -> VcpuState {
    linux::secondary(entry, context)
}
