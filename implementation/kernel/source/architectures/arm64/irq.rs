// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Devices' interrupts on ARM64.
//!
//! Each device interrupt has a vector of its own, from one pool: an MSI's,
//! through the ITS, or a line's (an SPI, whose INTID is its GSI). Vectors
//! are the kernel's own numbers, with no meaning to the processor. Both go
//! to the boot processor (see `super::gic`).

use core::ops::RangeInclusive;

use super::{gic, its};

/// How many vectors there are.
pub const VECTORS: usize = 256;

/// The vectors devices' interrupts are given.
pub const DEVICE_VECTORS: RangeInclusive<u8> = 1..=254;

/// Routes line `gsi` to `vector`, unmasked. False if the interrupt
/// controller has no such line. Lines are active high or rising: a device
/// of the other sense has an inverter on its way to the GIC.
pub fn route_line(gsi: u32, vector: u8, level: bool, _active_low: bool) -> bool {
    gic::route_line(gsi, vector, level)
}

/// Masks or unmasks line `gsi`.
pub fn mask_line(gsi: u32, masked: bool) {
    gic::set_line_masked(gsi, masked);
}

/// The address and data that make an MSI of PCI function `requester` (its
/// requester id) raise `vector`; zeroes, which raise nothing, without an
/// ITS for it.
pub fn msi_message(vector: u8, requester: u16) -> (u64, u32) {
    its::message(vector, requester).unwrap_or((0, 0))
}

/// No device's interrupt reaches `vector` any more.
pub fn release(vector: u8) {
    gic::release_lines(vector);
    its::release(vector);
}

/// The line legacy ISA IRQ `irq` is wired to: none here, as ARM64 machines
/// have no ISA.
pub fn isa_line(_irq: usize) -> Option<(u32, bool, bool)> {
    None
}
