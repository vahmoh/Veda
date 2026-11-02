// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! PCI configuration space on ARM64: PCI Express's enhanced mechanism
//! (ECAM), a window of memory with 4 KiB for each function, where the
//! firmware's MCFG table says. On segment 0.

use abi::{KernelBootInfo, cache_policy, map_flags};
use runtime::object::{Resource, Vmo};

use crate::pci::Address;

/// How many bytes of each function's configuration space the mechanism
/// reaches.
pub const CONFIG_BYTES: u16 = 4096;

pub struct ConfigAccess {
    /// Where the window is mapped.
    window: usize,
    first_bus: u8,
    last_bus: u8,
    /// The window's memory, kept with its mapping.
    _vmo: Vmo,
}

impl ConfigAccess {
    /// Maps the window, out of the registers `mmio` grants.
    pub fn open(_io: &Resource, mmio: &Resource, boot: Option<&KernelBootInfo>) -> Result<ConfigAccess, abi::Error> {
        let boot = boot.ok_or(abi::Error::NotFound)?;
        if boot.pci_config == 0 {
            return Err(abi::Error::NotFound);
        }
        let (first_bus, last_bus) = (boot.pci_config_buses[0], boot.pci_config_buses[1]);
        let size = (last_bus as usize - first_bus as usize + 1) << 20;
        let start = boot.pci_config + ((first_bus as u64) << 20);
        let vmo = Vmo::create_physical(mmio, start, size, cache_policy::UNCACHED)?;
        let window = vmo.map(0, size, map_flags::READ | map_flags::WRITE)?;
        Ok(ConfigAccess { window, first_bus, last_bus, _vmo: vmo })
    }

    /// The address of the aligned word at `offset`, on a bus the window has.
    fn word(&self, a: Address, offset: u16) -> Option<*mut u32> {
        if a.bus < self.first_bus || a.bus > self.last_bus || offset >= CONFIG_BYTES {
            return None;
        }
        let function =
            ((a.bus - self.first_bus) as usize) << 20 | (a.slot as usize) << 15 | (a.function as usize) << 12;
        Some((self.window + function + (offset & !3) as usize) as *mut u32)
    }

    /// Reads the aligned 32-bit word at `offset`.
    pub fn read32(&self, a: Address, offset: u16) -> u32 {
        // SAFETY: within the window, mapped as a device's; nothing that
        // does not answer reads as all ones.
        self.word(a, offset).map_or(u32::MAX, |word| unsafe { core::ptr::read_volatile(word) })
    }

    /// Writes the aligned 32-bit word at `offset`, and sees that it is done:
    /// a write to a device may be acknowledged on its way, and what comes
    /// next can need it to have happened.
    pub fn write32(&self, a: Address, offset: u16, value: u32) {
        if let Some(word) = self.word(a, offset) {
            // SAFETY: as in `read32`.
            unsafe {
                core::ptr::write_volatile(word, value);
                core::ptr::read_volatile(word);
            }
        }
    }
}
