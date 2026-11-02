// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The memory the firmware keeps for devices: on these machines the IORT
//! table's reserved regions, which nothing here reads yet, as QEMU's machine
//! has none.

use alloc::vec::Vec;
use core::ops::Range;

use acpi::tables::Table;

/// The memory the firmware keeps for devices: each range, with the
/// requester ids of the functions it is for (on segment 0).
pub fn reserved_memory(_tables: &[Table]) -> Vec<(Vec<u16>, Range<u64>)> {
    Vec::new()
}
