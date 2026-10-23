// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What the firmware's ACPI tables say of an AMD64 machine: its processors
//! and interrupt controllers (MADT), its HPET, and where its IOMMU's table
//! is (DMAR, which `iommu` reads).

use alloc::vec::Vec;

use iommu::amd64::dmar;

use crate::firmware::acpi::{self, u8_at, u16_at, u32_at, u64_at};
use crate::synchronization::Once;

#[derive(Debug, Clone, Copy)]
pub struct CpuEntry {
    pub apic_id: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct IoApicEntry {
    pub id: u8,
    pub phys: u64,
    pub gsi_base: u32,
}

/// ISA IRQ → GSI mapping with polarity/trigger.
#[derive(Debug, Clone, Copy)]
pub struct IrqOverride {
    pub isa_irq: u8,
    pub gsi: u32,
    pub active_low: bool,
    pub level: bool,
}

#[derive(Debug, Default)]
pub struct Platform {
    pub lapic_phys: u64,
    pub cpus: Vec<CpuEntry>,
    pub ioapics: Vec<IoApicEntry>,
    pub overrides: Vec<IrqOverride>,
    pub hpet_phys: Option<u64>,
    /// The DMAR table.
    pub dmar: Option<&'static [u8]>,
    /// The firmware asks the system not to use x2APIC mode (the DMAR
    /// table's flag, which holds with interrupt remapping).
    pub x2apic_opt_out: bool,
}

pub static PLATFORM: Once<Platform> = Once::new();

/// Where the local APICs are, unless the MADT says otherwise.
const LAPIC_DEFAULT: u64 = 0xFEE0_0000;

/// Reads the MADT `t`'s entries (each its type, its length, then what that
/// type has; one shorter than its type's is left out).
fn parse_madt(t: &[u8], p: &mut Platform) {
    p.lapic_phys = u32_at(t, 36).unwrap_or(0) as u64;
    let mut off = 44;
    while let (Some(ty), Some(len)) = (u8_at(t, off), u8_at(t, off + 1)) {
        let len = len as usize;
        let Some(e) = t.get(off..off + len).filter(|_| len >= 2) else { break };
        match ty {
            0 if len >= 8 => {
                if u32_at(e, 4).is_some_and(|flags| flags & 0b11 != 0) {
                    p.cpus.push(CpuEntry { apic_id: e[3] as u32 });
                }
            }
            1 if len >= 12 => p.ioapics.push(IoApicEntry {
                id: e[2],
                phys: u32_at(e, 4).unwrap_or(0) as u64,
                gsi_base: u32_at(e, 8).unwrap_or(0),
            }),
            2 if len >= 10 => {
                let flags = u16_at(e, 8).unwrap_or(0);
                p.overrides.push(IrqOverride {
                    isa_irq: e[3],
                    gsi: u32_at(e, 4).unwrap_or(0),
                    active_low: flags & 0b11 == 0b11,
                    level: (flags >> 2) & 0b11 == 0b11,
                });
            }
            5 if len >= 12 => p.lapic_phys = u64_at(e, 4).unwrap_or(p.lapic_phys),
            9 if len >= 16 => {
                let (id, flags) = (u32_at(e, 4).unwrap_or(0), u32_at(e, 8).unwrap_or(0));
                if flags & 0b11 != 0 && !p.cpus.iter().any(|c| c.apic_id == id) {
                    p.cpus.push(CpuEntry { apic_id: id });
                }
            }
            _ => {}
        }
        off += len;
    }
}

/// Reads the machine's tables (after `crate::firmware::acpi::init`).
pub fn init() {
    let mut p = Platform::default();
    for madt in acpi::find_all(b"APIC") {
        parse_madt(madt.bytes, &mut p);
    }
    if p.cpus.is_empty() {
        crate::kwarn!("acpi: no processors in the tables; assuming a single CPU");
        p.cpus.push(CpuEntry { apic_id: 0 });
    }
    if p.lapic_phys == 0 {
        p.lapic_phys = LAPIC_DEFAULT;
    }
    // The HPET's registers: the address of the generic address at 40.
    p.hpet_phys = acpi::find_all(b"HPET").last().and_then(|hpet| u64_at(hpet.bytes, 44));
    p.dmar = acpi::find_all(b"DMAR").filter(|dmar| dmar.checksum_ok()).last().map(|dmar| dmar.bytes);
    let dmar_flags = p.dmar.and_then(|dmar| u8_at(dmar, 37)).unwrap_or(0);
    p.x2apic_opt_out = dmar_flags & dmar::INTR_REMAP != 0 && dmar_flags & dmar::X2APIC_OPT_OUT != 0;
    let info = acpi::ACPI.expect();
    crate::kinfo!(
        "acpi: {} CPU(s), {} I/O APIC(s), {} override(s), hpet={}, mcfg={}, s5={}",
        p.cpus.len(),
        p.ioapics.len(),
        p.overrides.len(),
        p.hpet_phys.is_some(),
        info.mcfg.len(),
        info.power.s5_found
    );
    PLATFORM.set(p);
}

/// Translates a legacy ISA IRQ into (GSI, level, active_low).
pub fn isa_irq_to_gsi(irq: u8) -> (u32, bool, bool) {
    if let Some(p) = PLATFORM.get()
        && let Some(o) = p.overrides.iter().find(|o| o.isa_irq == irq)
    {
        return (o.gsi, o.level, o.active_low);
    }
    (irq as u32, false, false)
}
