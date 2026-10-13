// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The firmware's ACPI tables, as far as the kernel reads them: where each
//! table is, PCI Express configuration space (MCFG) and power control
//! (FADT, `\_S5`). The tables that describe the architecture's own devices
//! (its processors, interrupt controllers and IOMMU) are read by its code.
//! There is no AML interpreter; `\_S5` is located with a byte-pattern scan,
//! which is reliable for the firmware Veda targets.

use alloc::vec::Vec;

use crate::mm::phys_to_virt;
use crate::sync::Once;

#[derive(Debug, Clone, Copy)]
pub struct McfgEntry {
    pub base: u64,
    pub segment: u16,
    pub bus_start: u8,
    pub bus_end: u8,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PowerInfo {
    pub pm1a_cnt: u16,
    pub pm1b_cnt: u16,
    pub slp_typ_a: u16,
    pub slp_typ_b: u16,
    pub s5_found: bool,
    pub reset: Option<(GenericAddressRaw, u8)>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct GenericAddressRaw {
    pub space: u8,
    pub address: u64,
}

#[derive(Debug, Default)]
pub struct AcpiInfo {
    /// Every table the root table lists: its signature and physical address.
    pub tables: Vec<([u8; 4], u64)>,
    pub mcfg: Vec<McfgEntry>,
    pub power: PowerInfo,
}

pub static ACPI: Once<AcpiInfo> = Once::new();

/// Reads the byte of the firmware's tables at physical address `addr`
/// (they are in RAM, which the direct map covers).
pub fn read_u8(addr: u64) -> u8 {
    // SAFETY: ACPI tables are in RAM covered by the direct map.
    unsafe { core::ptr::read_unaligned(phys_to_virt(addr) as *const u8) }
}

/// Reads a 16-bit word of the firmware's tables (see [`read_u8`]).
pub fn read_u16(addr: u64) -> u16 {
    // SAFETY: as above.
    unsafe { core::ptr::read_unaligned(phys_to_virt(addr) as *const u16) }
}

/// Reads a 32-bit word of the firmware's tables (see [`read_u8`]).
pub fn read_u32(addr: u64) -> u32 {
    // SAFETY: as above.
    unsafe { core::ptr::read_unaligned(phys_to_virt(addr) as *const u32) }
}

/// Reads a 64-bit word of the firmware's tables (see [`read_u8`]).
pub fn read_u64(addr: u64) -> u64 {
    // SAFETY: as above.
    unsafe { core::ptr::read_unaligned(phys_to_virt(addr) as *const u64) }
}

fn signature(addr: u64) -> [u8; 4] {
    read_u32(addr).to_le_bytes()
}

/// Whether the `len` bytes of the table at `addr` add up to 0.
pub fn checksum_ok(addr: u64, len: u32) -> bool {
    (0..len as u64).fold(0u8, |s, i| s.wrapping_add(read_u8(addr + i))) == 0
}

/// Enumerates (signature, physical address) of every table in the RSDT/XSDT.
fn tables(rsdp: u64) -> Vec<([u8; 4], u64)> {
    let mut out = Vec::new();
    let revision = read_u8(rsdp + 15);
    let (root, entry_size) = if revision >= 2 && read_u64(rsdp + 24) != 0 {
        (read_u64(rsdp + 24), 8)
    } else {
        (read_u32(rsdp + 16) as u64, 4)
    };
    let len = read_u32(root + 4);
    if !checksum_ok(root, len) {
        crate::kwarn!("acpi: root table checksum mismatch");
    }
    let count = (len as u64 - 36) / entry_size;
    for i in 0..count {
        let addr = if entry_size == 8 { read_u64(root + 36 + i * 8) } else { read_u32(root + 36 + i * 4) as u64 };
        if addr != 0 {
            out.push((signature(addr), addr));
        }
    }
    out
}

fn parse_mcfg(addr: u64, info: &mut AcpiInfo) {
    let len = read_u32(addr + 4) as u64;
    let mut off = 44;
    while off + 16 <= len {
        let e = addr + off;
        info.mcfg.push(McfgEntry {
            base: read_u64(e),
            segment: read_u16(e + 8),
            bus_start: read_u8(e + 10),
            bus_end: read_u8(e + 11),
        });
        off += 16;
    }
}

/// Finds `\_S5` in the DSDT and decodes SLP_TYPa/b.
fn find_s5(dsdt: u64) -> Option<(u16, u16)> {
    let len = read_u32(dsdt + 4) as u64;
    let mut i = 36;
    while i + 4 < len {
        if read_u32(dsdt + i).to_le_bytes() == *b"_S5_" {
            // Expect: [NameOp 0x08] "_S5_" PackageOp(0x12) PkgLength NumElements ...
            let mut p = dsdt + i + 4;
            if read_u8(p) != 0x12 {
                i += 1;
                continue;
            }
            p += 1;
            let lead = read_u8(p);
            p += 1 + ((lead >> 6) as u64); // skip PkgLength bytes
            p += 1; // NumElements
            let value = |p: &mut u64| -> u16 {
                match read_u8(*p) {
                    0x0A => {
                        let v = read_u8(*p + 1) as u16;
                        *p += 2;
                        v
                    }
                    0x00 => {
                        *p += 1;
                        0
                    }
                    0x01 => {
                        *p += 1;
                        1
                    }
                    0xFF => {
                        *p += 1;
                        0xFF
                    }
                    v => {
                        *p += 1;
                        v as u16
                    }
                }
            };
            let a = value(&mut p);
            let b = value(&mut p);
            return Some((a, b));
        }
        i += 1;
    }
    None
}

fn parse_fadt(addr: u64, info: &mut AcpiInfo) {
    let len = read_u32(addr + 4);
    let dsdt = if len >= 148 && read_u64(addr + 140) != 0 { read_u64(addr + 140) } else { read_u32(addr + 40) as u64 };
    info.power.pm1a_cnt = read_u32(addr + 64) as u16;
    info.power.pm1b_cnt = read_u32(addr + 68) as u16;
    let flags = read_u32(addr + 112);
    if len >= 129 && flags & (1 << 10) != 0 {
        let reg = GenericAddressRaw { space: read_u8(addr + 116), address: read_u64(addr + 120) };
        info.power.reset = Some((reg, read_u8(addr + 128)));
    }
    if dsdt != 0
        && signature(dsdt) == *b"DSDT"
        && let Some((a, b)) = find_s5(dsdt)
    {
        info.power.slp_typ_a = a;
        info.power.slp_typ_b = b;
        info.power.s5_found = true;
    }
}

/// Parses the ACPI tables reachable from the RSDP.
pub fn init(rsdp: u64) {
    let mut info = AcpiInfo::default();
    if rsdp == 0 || read_u64(rsdp).to_le_bytes() != *b"RSD PTR " {
        crate::kwarn!("acpi: no valid RSDP");
        ACPI.set(info);
        return;
    }
    info.tables = tables(rsdp);
    for i in 0..info.tables.len() {
        let (sig, addr) = info.tables[i];
        match &sig {
            b"MCFG" => parse_mcfg(addr, &mut info),
            b"FACP" => parse_fadt(addr, &mut info),
            _ => {}
        }
    }
    for m in &info.mcfg {
        crate::kdebug!("acpi: PCIe ECAM at {:#x} (segment {}, buses {}-{})", m.base, m.segment, m.bus_start, m.bus_end);
    }
    ACPI.set(info);
}

/// The physical addresses of the tables with signature `sig`, in the root
/// table's order.
pub fn find_all(sig: &[u8; 4]) -> impl Iterator<Item = u64> + '_ {
    ACPI.get().into_iter().flat_map(|info| info.tables.iter()).filter(move |(s, _)| s == sig).map(|&(_, addr)| addr)
}

/// The bytes of the table at `addr`, its header included.
pub fn table_bytes(addr: u64) -> &'static [u8] {
    // SAFETY: as for the readers above; the firmware's tables stay.
    unsafe { core::slice::from_raw_parts(phys_to_virt(addr) as *const u8, read_u32(addr + 4) as usize) }
}
