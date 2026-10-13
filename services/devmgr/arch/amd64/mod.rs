// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The device manager on AMD64 (a PC): its PCI configuration mechanism
//! (`config`), the memory the firmware keeps for devices (`dmar`), and the
//! PC's own devices the driver VM gets (`platform`).

pub mod config;
pub mod dmar;
pub mod platform;

/// The memory below this the driver VM's guest keeps as a PC does (its
/// legacy area): memory the firmware keeps for a device there takes no
/// room of the guest's RAM.
pub const GUEST_LOW_RESERVED: u64 = 1 << 20;
