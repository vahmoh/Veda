// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The memory the firmware keeps for devices, as a PC's says it: the DMAR
//! table's (Intel VT-d's) reserved memory regions, RMRRs.

use alloc::vec::Vec;
use core::ops::Range;

use acpi::tables::Table;
use iommu::amd64::dmar;
use runtime::println;

/// The memory the firmware keeps for devices: each range, with the
/// requester ids of the functions it is for (on segment 0).
pub fn reserved_memory(tables: &[Table]) -> Vec<(Vec<u16>, Range<u64>)> {
    let regions = match tables.iter().find(|t| t.is(b"DMAR")).map(|t| dmar::parse(&t.data)) {
        Some(Ok(dmar)) => dmar.reserved,
        Some(Err(e)) => {
            println!("acpi: the DMAR table: {}", e);
            Vec::new()
        }
        None => Vec::new(),
    };
    regions
        .iter()
        .filter(|r| r.segment == 0)
        .filter_map(|r| {
            let Some(range) = r.range() else {
                // The kernel keeps its devices: they cannot be confined.
                println!(
                    "acpi: the DMAR table reserves {:#x} to {:#x}, which is not whole pages: its devices stay Veda's",
                    r.base, r.limit
                );
                return None;
            };
            Some((r.scopes.iter().filter_map(|s| s.source_id()).collect(), range))
        })
        .collect()
}
