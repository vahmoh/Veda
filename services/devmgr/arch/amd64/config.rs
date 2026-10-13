// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! PCI configuration space on a PC: configuration mechanism #1, an address
//! written to port 0xCF8, the data at port 0xCFC (segment 0, the first 256
//! bytes of each function's).

use vrt::object::{IoPorts, Resource};

use crate::pci::Address;

const ADDRESS: u16 = 0xCF8;
const DATA: u16 = 0xCFC;

/// How many bytes of each function's configuration space the mechanism
/// reaches.
pub const CONFIG_BYTES: u16 = 256;

pub struct ConfigAccess {
    ports: IoPorts,
}

impl ConfigAccess {
    /// Takes the configuration ports, out of the ports `io` grants.
    pub fn open(io: &Resource) -> Result<ConfigAccess, vabi::Error> {
        Ok(ConfigAccess { ports: IoPorts::create(io, ADDRESS, 8)? })
    }

    fn select(&self, a: Address, offset: u16) {
        let addr = 0x8000_0000u32
            | (a.bus as u32) << 16
            | (a.slot as u32) << 11
            | (a.function as u32) << 8
            | (offset as u32 & 0xFC);
        self.ports.out32(ADDRESS, addr);
    }

    /// Reads the aligned 32-bit word at `offset`.
    pub fn read32(&self, a: Address, offset: u16) -> u32 {
        self.select(a, offset);
        self.ports.in32(DATA)
    }

    /// Writes the aligned 32-bit word at `offset`.
    pub fn write32(&self, a: Address, offset: u16, value: u32) {
        self.select(a, offset);
        self.ports.out32(DATA, value);
    }
}
