// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The ACPI tables: from the RSDP to the root table (the XSDT, or the RSDT
//! of older firmware), to every table it lists, and the DSDT the FADT
//! points to.
//!
//! [`entries`] walks the root table reading no more of each table than its
//! header, for a reader that maps the tables itself (the kernel's);
//! [`load`] reads them whole.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

use crate::Memory;

/// Length of the header every table starts with.
pub const HEADER_LEN: usize = 36;
/// Tables larger than this are not believed (a DSDT is under 1 MiB).
pub const MAX_TABLE: usize = 8 << 20;

/// One table, header included.
#[derive(Debug, Clone)]
pub struct Table {
    pub address: u64,
    pub data: Vec<u8>,
}

impl Table {
    /// Its signature (`DSDT`); zeros if it is too short to have one.
    pub fn signature(&self) -> [u8; 4] {
        let mut s = [0; 4];
        if let Some(b) = self.data.get(..4) {
            s.copy_from_slice(b);
        }
        s
    }

    /// The OEM's name for the table (`SPKRAMPS`, `CpuSsdt`), trimmed.
    pub fn oem_table_id(&self) -> String {
        String::from_utf8_lossy(self.data.get(16..24).unwrap_or_default()).trim_end_matches([' ', '\0']).into()
    }

    /// The AML of a DSDT or SSDT.
    pub fn aml(&self) -> &[u8] {
        self.data.get(HEADER_LEN..).unwrap_or_default()
    }

    /// Whether the bytes add up to zero, as they should.
    pub fn checksum_ok(&self) -> bool {
        self.data.iter().fold(0u8, |s, &b| s.wrapping_add(b)) == 0
    }

    /// Whether its signature is `signature`.
    pub fn is(&self, signature: &[u8; 4]) -> bool {
        &self.signature() == signature
    }
}

/// Why the tables cannot be found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableError {
    /// No RSDP at the address given.
    NoRsdp,
    /// The root table is missing or is not an XSDT or RSDT.
    BadRoot,
}

impl fmt::Display for TableError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            TableError::NoRsdp => "there is no RSDP where the firmware says",
            TableError::BadRoot => "the root table is missing, or is neither an XSDT nor an RSDT",
        })
    }
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u32_at(b, at) as u64 | (u32_at(b, at + 4) as u64) << 32
}

/// A table, as its header says: where it is, its signature and its length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub address: u64,
    pub signature: [u8; 4],
    /// Its length, header included (from [`HEADER_LEN`] to [`MAX_TABLE`]).
    pub length: u32,
}

impl Entry {
    /// The header of the table at `address`: `None` if it cannot be read,
    /// or its length is not a table's.
    pub fn at(memory: &dyn Memory, address: u64) -> Option<Entry> {
        let mut h = [0u8; HEADER_LEN];
        if address == 0 || !memory.read(address, &mut h) {
            return None;
        }
        let length = u32_at(&h, 4);
        let believed = (HEADER_LEN..=MAX_TABLE).contains(&(length as usize));
        believed.then_some(Entry { address, signature: [h[0], h[1], h[2], h[3]], length })
    }

    /// Reads the table whole: `None` if it cannot be read.
    pub fn read(&self, memory: &dyn Memory) -> Option<Table> {
        let mut data = vec![0u8; self.length as usize];
        memory.read(self.address, &mut data).then_some(Table { address: self.address, data })
    }
}

/// The tables a root table lists, in its order ([`entries`]).
pub struct Entries<'a> {
    memory: &'a dyn Memory,
    /// Where the root table's entries are, how many there are, and the
    /// size of each (an XSDT's 8 bytes, an RSDT's 4).
    first: u64,
    count: u64,
    size: usize,
    next: u64,
}

impl Iterator for Entries<'_> {
    type Item = Entry;

    fn next(&mut self) -> Option<Entry> {
        while self.next < self.count {
            let at = self.first.checked_add(self.next * self.size as u64)?;
            self.next += 1;
            let mut address = [0u8; 8];
            if self.memory.read(at, &mut address[..self.size])
                && let Some(entry) = Entry::at(self.memory, u64::from_le_bytes(address))
            {
                return Some(entry);
            }
        }
        None
    }
}

/// The tables the root table of the RSDP at `rsdp` lists, as their
/// headers say, in its order. Tables whose header cannot be read, or whose
/// length is not a table's, are left out.
pub fn entries(memory: &dyn Memory, rsdp: u64) -> Result<Entries<'_>, TableError> {
    let mut r = [0u8; 36];
    if rsdp == 0 || !memory.read(rsdp, &mut r[..20]) || &r[..8] != b"RSD PTR " {
        return Err(TableError::NoRsdp);
    }
    // ACPI 2.0 and later: a 64-bit XSDT address after the RSDT's.
    let xsdt = if r[15] >= 2 && memory.read(rsdp, &mut r) { u64_at(&r, 24) } else { 0 };
    let (root, size) = if xsdt != 0 { (xsdt, 8) } else { (u32_at(&r, 16) as u64, 4) };
    let root = Entry::at(memory, root).ok_or(TableError::BadRoot)?;
    if &root.signature != b"XSDT" && &root.signature != b"RSDT" {
        return Err(TableError::BadRoot);
    }
    let first = root.address.checked_add(HEADER_LEN as u64).ok_or(TableError::BadRoot)?;
    let count = ((root.length as usize - HEADER_LEN) / size) as u64;
    Ok(Entries { memory, first, count, size, next: 0 })
}

/// Where the DSDT is, as a FADT (whole, its header included) says: its
/// 64-bit address when it has one, else its 32-bit one. `None` if the
/// FADT is too short to say.
pub fn dsdt_address(fadt: &[u8]) -> Option<u64> {
    let wide = (fadt.len() >= 148).then(|| u64_at(fadt, 140)).filter(|&a| a != 0);
    wide.or_else(|| (fadt.len() >= 44).then(|| u32_at(fadt, 40) as u64).filter(|&a| a != 0))
}

/// Reads the table at `address`, or `None` if it cannot be read or its
/// length is absurd.
pub fn read(memory: &dyn Memory, address: u64) -> Option<Table> {
    Entry::at(memory, address)?.read(memory)
}

/// Every table reachable from the RSDP at `rsdp`: those the root table
/// lists, then the DSDT. Tables that cannot be read are left out.
pub fn load(memory: &dyn Memory, rsdp: u64) -> Result<Vec<Table>, TableError> {
    let mut tables: Vec<Table> = entries(memory, rsdp)?.filter_map(|e| e.read(memory)).collect();
    let dsdt = tables.iter().find(|t| t.is(b"FACP")).and_then(|fadt| dsdt_address(&fadt.data));
    if let Some(t) = dsdt.and_then(|a| read(memory, a)).filter(|t| t.is(b"DSDT")) {
        tables.push(t);
    }
    Ok(tables)
}
