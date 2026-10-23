// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The driver VM on AMD64: its guest is a PC, as far as Linux needs one.
//!
//! * [`boot`]: Linux booted the x86 way.
//! * [`platform`]: the PC's own devices the guest has (its keyboard
//!   controller), and the processors' exits that are not hypercalls.
//! * Where the guest has things below 4 GiB, as a PC has them, and how
//!   its processors' interrupts and MSIs look, and how many it may have.
//!
//! What of it reaches no object of Veda's is `drivervm_core::architecture`
//! (`implementation/services/drivervm/core/source/architectures/amd64`),
//! tested on the host.

pub mod boot;
mod i8042;
pub mod platform;

use core::ops::Range;

pub use drivervm_core::architecture::MAX_MSIS;
pub use hv::amd64::memory::{ACPI_AT, ACPI_ROOM, OPREGION_AT};
pub use hv::amd64::platform::msi_result;

/// Where PCI functions' BARs go below 4 GiB: from 3 GiB (RAM ends before)
/// to where a PC has its I/O APIC and local APICs.
pub const PCI_LOW: Range<u64> = 0xC000_0000..0xFEC0_0000;

/// Whether `vector` is one a processor takes interrupts at (32 to 255;
/// those below are its exceptions).
pub fn is_interrupt_vector(vector: u32) -> bool {
    (32..=255).contains(&vector)
}
