// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Devices' interrupts on AMD64.
//!
//! Each device interrupt has a vector of its own, from one pool: an MSI's,
//! or an I/O APIC input's (a line, a GSI). Both go to the boot processor,
//! remapped by the IOMMU where there is one (see `super::apic`).

use core::ops::RangeInclusive;

use super::{apic, idt};

/// How many vectors there are (the IDT's entries).
pub const VECTORS: usize = 256;

/// The vectors devices' interrupts are given.
pub const DEVICE_VECTORS: RangeInclusive<u8> = idt::DEVICE_VECTOR_FIRST..=idt::DEVICE_VECTOR_LAST;

/// Routes line `gsi` to `vector`, unmasked. False if no I/O APIC has it.
pub fn route_line(gsi: u32, vector: u8, level: bool, active_low: bool) -> bool {
    apic::route_gsi(gsi, vector, level, active_low, false)
}

/// Masks or unmasks line `gsi`.
pub fn mask_line(gsi: u32, masked: bool) {
    apic::set_gsi_masked(gsi, masked);
}

/// The address and data that make an MSI of PCI function `requester` (its
/// requester id) raise `vector`.
pub fn msi_message(vector: u8, requester: u16) -> (u64, u32) {
    apic::msi_message(vector, requester)
}

/// No device's interrupt reaches `vector` any more.
pub fn release(vector: u8) {
    apic::release_vector(vector);
}

/// The line legacy ISA IRQ `irq` (0-15) is wired to, and whether it is
/// level-triggered and active low, as the firmware's interrupt source
/// overrides say. `None` for a number that is no ISA IRQ.
pub fn isa_line(irq: usize) -> Option<(u32, bool, bool)> {
    let irq = u8::try_from(irq).ok().filter(|&irq| irq <= 15)?;
    Some(super::acpi::isa_irq_to_gsi(irq))
}
