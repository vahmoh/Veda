// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The driver VM on ARM64: its guest is a machine of Arm's, as far as Linux
//! needs one.
//!
//! * [`boot`]: Linux booted Arm's way, on a device tree.
//! * [`platform`]: the processors' exits that are not hypercalls, and PSCI.
//! * Where the guest has things below 4 GiB, and how its processors'
//!   interrupts and MSIs look, and how many it may have.
//!
//! What of it reaches no object of Veda's is the library's
//! (`driver_virtual_machine::architecture`, `lib.rs` beside this), tested
//! on the host.

pub mod boot;
pub mod platform;

use core::ops::Range;

pub use driver_virtual_machine::architecture::MAX_MSIS;
pub use hv::arm64::platform::msi_result;

/// Where PCI functions' BARs go below 4 GiB: from 3 GiB (RAM ends before)
/// to where the guest has its interrupt controller's registers.
pub const PCI_LOW: Range<u64> = 0xC000_0000..hv::arm64::memory::GIC_DISTRIBUTOR;

/// Where the ACPI tables would be: a device tree describes the guest
/// instead (`boot`), so that the tables go unused.
pub const ACPI_AT: u64 = 0;

/// Where the copy of an Intel GPU's OpRegion would be: no such GPU is on
/// these machines.
pub const OPREGION_AT: u64 = 0;

/// Whether `vector` is one a processor takes interrupts at: an SPI's INTID.
pub fn is_interrupt_vector(vector: u32) -> bool {
    (32..=255).contains(&vector)
}
