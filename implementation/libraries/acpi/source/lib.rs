// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `acpi` — ACPI for Veda's device manager.
//!
//! Firmware describes the devices that are not on an enumerable bus, and
//! how the ones on PCI are wired to the rest of the board, in ACPI tables.
//! The DSDT and the SSDTs hold AML: bytecode that builds a namespace of
//! devices whose identity and resources (`_HID`, `_ADR`, `_STA`, `_CRS`)
//! are often computed by small methods from the firmware's settings.
//!
//! * [`tables`]: finding the tables from the RSDP;
//! * [`aml`]: loading the AML into a [`aml::Namespace`], initialising its
//!   devices and evaluating its objects. The interpreter reads firmware
//!   memory and PCI configuration space and never writes them: what a
//!   method stores stays inside that evaluation (what the devices' `_INI`
//!   methods store, in every evaluation after);
//! * [`device`]: what a device is (`_HID`, `_ADR`, `_STA`...) and the
//!   resources it uses;
//! * [`resource`]: the resource templates `_CRS` returns;
//! * [`asm`]: an AML assembler for tables written by hand (tests, simulated
//!   boards).
//!
//! Everything the firmware hands over is untrusted: malformed AML or
//! templates give errors, never panics, and every evaluation is bounded.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod aml;
pub mod asm;
pub mod device;
pub mod name;
pub mod resource;
pub mod tables;
pub mod value;

#[cfg(test)]
mod tests;

/// The machine as the caller can read it: physical memory (the tables, and
/// the firmware variables and device registers that AML reads through
/// `SystemMemory` operation regions), and the configuration space of PCI
/// functions (`PCI_Config` regions), which on PCI Express is memory too.
pub trait Memory {
    /// Reads `buf.len()` bytes at physical `address`. False if that memory
    /// cannot be read.
    fn read(&self, address: u64, buf: &mut [u8]) -> bool;

    /// Reads a field of a `SystemMemory` region: the `buf.len()` bytes at
    /// physical `address`, as accesses of `width` bytes each (1, 2, 4 or 8,
    /// as the field's access type says: a device's registers may answer
    /// only to accesses of their own width), one after the other from
    /// `address`. `buf.len()` is a multiple of `width`; `address` is too
    /// unless the region itself is not (the accesses are aligned within
    /// it). By default, as [`Memory::read`] reads.
    fn read_field(&self, address: u64, width: usize, buf: &mut [u8]) -> bool {
        let _ = width;
        self.read(address, buf)
    }

    /// Reads `buf.len()` bytes at `offset` in the configuration space of
    /// PCI function `function`. False if it cannot be read (none can, but
    /// where the caller says so).
    fn read_pci(&self, function: PciFunction, offset: u16, buf: &mut [u8]) -> bool {
        let _ = (function, offset, buf);
        false
    }

    /// Reads a field of a `PCI_Config` region, as [`Memory::read_field`]
    /// does a `SystemMemory` region's: accesses of `width` bytes at
    /// `offset` in the configuration space of `function`. By default, as
    /// [`Memory::read_pci`] reads.
    fn read_pci_field(&self, function: PciFunction, offset: u16, width: usize, buf: &mut [u8]) -> bool {
        let _ = width;
        self.read_pci(function, offset, buf)
    }
}

/// A PCI function: its segment, bus, device and function numbers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct PciFunction {
    pub segment: u16,
    pub bus: u8,
    pub device: u8,
    pub function: u8,
}

impl core::fmt::Display for PciFunction {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:04x}:{:02x}:{:02x}.{}", self.segment, self.bus, self.device, self.function)
    }
}

/// Memory that cannot be read at all (for evaluating AML that needs none).
pub struct NoMemory;

impl Memory for NoMemory {
    fn read(&self, _address: u64, _buf: &mut [u8]) -> bool {
        false
    }
}
