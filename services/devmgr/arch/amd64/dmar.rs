//! The memory the firmware keeps for devices, as a PC's says it: the DMAR
//! table's (Intel VT-d's) reserved memory regions, RMRRs.

use alloc::vec::Vec;
use core::ops::Range;

use vacpi::tables::Table;
use viommu::amd64::dmar;
use vrt::println;

/// The memory the firmware keeps for devices: each range, with the
/// requester ids of the functions it is for (on segment 0).
pub fn reserved_memory(tables: &[Table]) -> Vec<(Vec<u16>, Range<u64>)> {
    let regions = match tables.iter().find(|t| t.is(b"DMAR")).map(|t| dmar::parse(&t.data)) {
        Some(Ok(dmar)) => dmar.reserved,
        Some(Err(e)) => {
            println!("acpi: the DMAR table: {:?}", e);
            Vec::new()
        }
        None => Vec::new(),
    };
    regions
        .iter()
        .filter(|r| r.segment == 0)
        .map(|r| (r.scopes.iter().filter_map(|s| s.source_id()).collect(), r.base..r.limit + 1))
        .collect()
}
