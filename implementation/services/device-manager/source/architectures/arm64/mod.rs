// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The device manager on ARM64: its PCI configuration mechanism
//! (`config`), the memory the firmware keeps for devices (`dmar`), and the
//! machine's own devices the driver VM gets (`platform`).

pub mod config;
pub mod dmar;
pub mod platform;

/// Whether the driver VM's guest keeps `address` as a machine of its kind
/// does: none, as no ARM64 machine has a legacy area.
pub fn in_guest_low_memory(_address: u64) -> bool {
    false
}
