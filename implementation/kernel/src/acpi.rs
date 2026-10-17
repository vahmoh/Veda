// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The firmware's ACPI tables, as far as the kernel reads them: the tables
//! the root table lists (`vacpi::tables`, over the direct map: what a
//! table says is beyond it is not read), PCI Express configuration space
//! (MCFG) and power control (FADT, `\_S5`). The tables that describe the architecture's own devices
//! (its processors, interrupt controllers and IOMMU) are read by its code.
//! The kernel loads no AML namespace (devmgr does): `\_S5`'s package is
//! found and decoded where the DSDT defines it (`vacpi::aml::sleep_type`).

use alloc::vec::Vec;

use vacpi::tables::{self, Entry};

use crate::memory::phys_to_virt;
use crate::synchronization::Once;

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

/// A table of the firmware's, which stays where it is.
#[derive(Debug, Clone, Copy)]
pub struct Table {
    pub signature: [u8; 4],
    /// All of it, its header included.
    pub bytes: &'static [u8],
}

impl Table {
    /// Whether its bytes add up to 0, as they should.
    pub fn checksum_ok(&self) -> bool {
        self.bytes.iter().fold(0u8, |s, &b| s.wrapping_add(b)) == 0
    }
}

#[derive(Debug, Default)]
pub struct AcpiInfo {
    /// Every table the root table lists, in its order.
    pub tables: Vec<Table>,
    pub mcfg: Vec<McfgEntry>,
    pub power: PowerInfo,
}

pub static ACPI: Once<AcpiInfo> = Once::new();

/// The firmware's memory, as the direct map has it: the tables are in RAM,
/// and what a table says is beyond the direct map cannot be read.
struct Firmware {
    /// The end of the direct map (physical).
    limit: u64,
}

impl Firmware {
    /// The `len` bytes at physical `address`, if the direct map has them.
    fn bytes(&self, address: u64, len: usize) -> Option<&'static [u8]> {
        let end = address.checked_add(len as u64)?;
        // SAFETY: the direct map covers [0, limit), and the firmware's
        // tables stay where they are.
        (end <= self.limit).then(|| unsafe { core::slice::from_raw_parts(phys_to_virt(address) as *const u8, len) })
    }

    /// The table whose header is `entry`, if it is all in the direct map.
    fn table(&self, entry: Entry) -> Option<Table> {
        let bytes = self.bytes(entry.address, entry.length as usize)?;
        Some(Table { signature: entry.signature, bytes })
    }
}

impl vacpi::Memory for Firmware {
    fn read(&self, address: u64, buf: &mut [u8]) -> bool {
        let Some(bytes) = self.bytes(address, buf.len()) else { return false };
        buf.copy_from_slice(bytes);
        true
    }
}

/// The byte at `at` of a table, if it has it.
pub fn u8_at(b: &[u8], at: usize) -> Option<u8> {
    b.get(at).copied()
}

/// The little-endian 16-bit word at `at` of a table, if it has it.
pub fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at.checked_add(2)?)?.try_into().ok()?))
}

/// The little-endian 32-bit word at `at` of a table, if it has it.
pub fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

/// The little-endian 64-bit word at `at` of a table, if it has it.
pub fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at.checked_add(8)?)?.try_into().ok()?))
}

fn parse_mcfg(t: &[u8], info: &mut AcpiInfo) {
    // Its entries, 16 bytes each, after the header and 8 reserved bytes.
    for e in t.get(44..).unwrap_or_default().as_chunks::<16>().0 {
        info.mcfg.push(McfgEntry {
            base: u64_at(e, 0).unwrap_or(0),
            segment: u16_at(e, 8).unwrap_or(0),
            bus_start: e[10],
            bus_end: e[11],
        });
    }
}

fn parse_fadt(fw: &Firmware, t: &[u8], info: &mut AcpiInfo) {
    info.power.pm1a_cnt = u32_at(t, 64).unwrap_or(0) as u16;
    info.power.pm1b_cnt = u32_at(t, 68).unwrap_or(0) as u16;
    let flags = u32_at(t, 112).unwrap_or(0);
    if flags & (1 << 10) != 0
        && let (Some(space), Some(address), Some(value)) = (u8_at(t, 116), u64_at(t, 120), u8_at(t, 128))
    {
        info.power.reset = Some((GenericAddressRaw { space, address }, value));
    }
    // `\_S5`'s package, where the DSDT defines it.
    let dsdt = tables::dsdt_address(t).and_then(|a| Entry::at(fw, a)).and_then(|e| fw.table(e));
    if let Some(dsdt) = dsdt.filter(|d| &d.signature == b"DSDT")
        && let Some((a, b)) = vacpi::aml::sleep_type(dsdt.bytes, 5)
    {
        info.power.slp_typ_a = a;
        info.power.slp_typ_b = b;
        info.power.s5_found = true;
    }
}

/// Parses the ACPI tables reachable from the RSDP, reading only what the
/// direct map, which ends at `direct_map_limit`, has.
pub fn init(rsdp: u64, direct_map_limit: u64) {
    let fw = Firmware { limit: direct_map_limit };
    let mut info = AcpiInfo::default();
    match tables::entries(&fw, rsdp) {
        Ok(entries) => info.tables = entries.filter_map(|e| fw.table(e)).collect(),
        Err(e) => crate::kwarn!("acpi: {}", e),
    }
    for i in 0..info.tables.len() {
        let t = info.tables[i];
        match &t.signature {
            b"MCFG" => parse_mcfg(t.bytes, &mut info),
            b"FACP" => parse_fadt(&fw, t.bytes, &mut info),
            _ => {}
        }
    }
    for m in &info.mcfg {
        crate::kdebug!("acpi: PCIe ECAM at {:#x} (segment {}, buses {}-{})", m.base, m.segment, m.bus_start, m.bus_end);
    }
    ACPI.set(info);
}

/// The tables with signature `sig`, in the root table's order.
pub fn find_all(sig: &[u8; 4]) -> impl Iterator<Item = &'static Table> + '_ {
    ACPI.get().into_iter().flat_map(|info| info.tables.iter()).filter(move |t| &t.signature == sig)
}
