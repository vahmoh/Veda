// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What the firmware's ACPI tables say of an ARM64 machine: its processors
//! and interrupt controllers (MADT), its timers' interrupts (GTDT), the
//! console it keeps for an operating system (SPCR), and how to call PSCI
//! (the FADT).

use alloc::vec::Vec;

use crate::firmware::acpi::{self, u8_at, u16_at, u32_at, u64_at};
use crate::synchronization::Once;

/// The MADT's entries, by type.
const MADT_GICC: u8 = 0x0B;
const MADT_GICD: u8 = 0x0C;
const MADT_GICR: u8 = 0x0E;
const MADT_ITS: u8 = 0x0F;

/// The FADT's ARM boot architecture flags, and their bits: PSCI's there,
/// and it is called with `HVC` (otherwise `SMC`).
const FADT_ARM_BOOT_FLAGS: usize = 129;
const ARM_PSCI_COMPLIANT: u16 = 1 << 0;
const ARM_PSCI_USE_HVC: u16 = 1 << 1;

/// SPCR's interface types: a PL011, and the subset of it Arm's server
/// standards ask for.
const SPCR_PL011: u8 = 3;
const SPCR_SBSA_GENERIC: u8 = 0x0E;

/// The usual interrupts of the timers (Arm's Base System Architecture),
/// where the GTDT gives none.
const TIMER_EL1_VIRTUAL: u32 = 27;
const TIMER_EL2_PHYSICAL: u32 = 26;

#[derive(Debug, Clone, Copy)]
pub struct CpuEntry {
    /// Its affinity, as `MPIDR_EL1` has it.
    pub affinity: u64,
}

#[derive(Debug, Default)]
pub struct Platform {
    pub cpus: Vec<CpuEntry>,
    /// The GIC's distributor.
    pub distributor: u64,
    /// Where its redistributors are: each range's base and length, or else
    /// each CPU's own in `per_cpu_redistributors`.
    pub redistributors: Vec<(u64, u32)>,
    pub per_cpu_redistributors: Vec<u64>,
    /// Its Interrupt Translation Services.
    pub its: Vec<u64>,
    /// The interrupts of the kernel's timer, at EL1 and at EL2.
    pub timer_el1: u32,
    pub timer_el2: u32,
    /// The console's PL011.
    pub uart: Option<u64>,
    /// PSCI is called with `HVC` (otherwise `SMC`).
    pub psci_hvc: bool,
}

pub static PLATFORM: Once<Platform> = Once::new();

/// Reads the MADT `t`'s entries (each its type, its length, then what that
/// type has; one shorter than its type's is left out).
fn parse_madt(t: &[u8], p: &mut Platform) {
    let mut off = 44;
    while let (Some(ty), Some(len)) = (u8_at(t, off), u8_at(t, off + 1)) {
        let len = len as usize;
        let Some(e) = t.get(off..off + len).filter(|_| len >= 2) else { break };
        match ty {
            MADT_GICC if len >= 76 => {
                let flags = u32_at(e, 12).unwrap_or(0);
                if flags & 1 != 0 {
                    p.cpus.push(CpuEntry { affinity: u64_at(e, 68).unwrap_or(0) & 0xFF_00FF_FFFF });
                    match u64_at(e, 60) {
                        Some(gicr) if gicr != 0 => p.per_cpu_redistributors.push(gicr),
                        _ => {}
                    }
                }
            }
            MADT_GICD if len >= 24 => p.distributor = u64_at(e, 8).unwrap_or(0),
            MADT_GICR if len >= 16 => {
                p.redistributors.push((u64_at(e, 4).unwrap_or(0), u32_at(e, 12).unwrap_or(0)));
            }
            MADT_ITS if len >= 20 => p.its.push(u64_at(e, 8).unwrap_or(0)),
            _ => {}
        }
        off += len;
    }
}

/// Reads the machine's tables (after `crate::firmware::acpi::init`).
pub fn init() {
    let mut p = Platform { timer_el1: TIMER_EL1_VIRTUAL, timer_el2: TIMER_EL2_PHYSICAL, ..Platform::default() };
    for madt in acpi::find_all(b"APIC") {
        parse_madt(madt.bytes, &mut p);
    }
    if let Some(gtdt) = acpi::find_all(b"GTDT").last() {
        let bytes = gtdt.bytes;
        p.timer_el1 = u32_at(bytes, 64).filter(|&i| i != 0).unwrap_or(p.timer_el1);
        p.timer_el2 = u32_at(bytes, 72).filter(|&i| i != 0).unwrap_or(p.timer_el2);
    }
    if let Some(spcr) = acpi::find_all(b"SPCR").last()
        && let Some(SPCR_PL011 | SPCR_SBSA_GENERIC) = u8_at(spcr.bytes, 36)
    {
        p.uart = u64_at(spcr.bytes, 44).filter(|&a| a != 0);
    }
    if let Some(fadt) = acpi::find_all(b"FACP").last() {
        let flags = u16_at(fadt.bytes, FADT_ARM_BOOT_FLAGS).unwrap_or(0);
        if flags & ARM_PSCI_COMPLIANT == 0 {
            crate::kwarn!("acpi: the firmware does not say it has PSCI");
        }
        p.psci_hvc = flags & ARM_PSCI_USE_HVC != 0;
    }
    if p.cpus.is_empty() {
        crate::kwarn!("acpi: no processors in the tables; assuming a single CPU");
        p.cpus.push(CpuEntry { affinity: super::cpu::affinity() });
    }
    let info = acpi::ACPI.expect();
    crate::kinfo!(
        "acpi: {} CPU(s), GICv3 at {:#x}, {} ITS(s), timer interrupts {} and {}, mcfg={}, psci by {}",
        p.cpus.len(),
        p.distributor,
        p.its.len(),
        p.timer_el1,
        p.timer_el2,
        info.mcfg.len(),
        if p.psci_hvc { "HVC" } else { "SMC" }
    );
    PLATFORM.set(p);
}
