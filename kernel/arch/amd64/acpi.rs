//! What the firmware's ACPI tables say of an AMD64 machine: its processors
//! and interrupt controllers (MADT), its HPET, and where its IOMMU's table
//! is (DMAR, which `iommu` reads).

use alloc::vec::Vec;

use crate::acpi::{self, read_u8, read_u16, read_u32, read_u64};
use crate::sync::Once;

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
    /// The DMAR table (its physical address).
    pub dmar: Option<u64>,
}

pub static PLATFORM: Once<Platform> = Once::new();

/// Where the local APICs are, unless the MADT says otherwise.
const LAPIC_DEFAULT: u64 = 0xFEE0_0000;

fn parse_madt(addr: u64, p: &mut Platform) {
    p.lapic_phys = read_u32(addr + 36) as u64;
    let len = read_u32(addr + 4) as u64;
    let mut off = 44;
    while off + 2 <= len {
        let ty = read_u8(addr + off);
        let elen = read_u8(addr + off + 1) as u64;
        if elen < 2 {
            break;
        }
        let e = addr + off;
        match ty {
            0 => {
                let flags = read_u32(e + 4);
                if flags & 0b11 != 0 {
                    p.cpus.push(CpuEntry { apic_id: read_u8(e + 3) as u32 });
                }
            }
            1 => p.ioapics.push(IoApicEntry {
                id: read_u8(e + 2),
                phys: read_u32(e + 4) as u64,
                gsi_base: read_u32(e + 8),
            }),
            2 => {
                let flags = read_u16(e + 8);
                p.overrides.push(IrqOverride {
                    isa_irq: read_u8(e + 3),
                    gsi: read_u32(e + 4),
                    active_low: flags & 0b11 == 0b11,
                    level: (flags >> 2) & 0b11 == 0b11,
                });
            }
            5 => p.lapic_phys = read_u64(e + 4),
            9 => {
                let flags = read_u32(e + 8);
                let id = read_u32(e + 4);
                if flags & 0b11 != 0 && !p.cpus.iter().any(|c| c.apic_id == id) {
                    p.cpus.push(CpuEntry { apic_id: id });
                }
            }
            _ => {}
        }
        off += elen;
    }
}

/// Reads the machine's tables (after `crate::acpi::init`).
pub fn init() {
    let mut p = Platform::default();
    for madt in acpi::find_all(b"APIC") {
        parse_madt(madt, &mut p);
    }
    if p.cpus.is_empty() {
        crate::kwarn!("acpi: no processors in the tables; assuming a single CPU");
        p.cpus.push(CpuEntry { apic_id: 0 });
    }
    if p.lapic_phys == 0 {
        p.lapic_phys = LAPIC_DEFAULT;
    }
    p.hpet_phys = acpi::find_all(b"HPET").last().map(|hpet| read_u64(hpet + 44));
    p.dmar = acpi::find_all(b"DMAR").filter(|&dmar| acpi::checksum_ok(dmar, read_u32(dmar + 4))).last();
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
