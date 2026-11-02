// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The machine's own devices the driver VM gets: none, as everything an
//! ARM64 machine gives it is on PCI.

use alloc::vec::Vec;

use runtime::object::{Handle, Resource};

/// The machine's own devices the driver VM gets.
pub struct Platform;

impl Platform {
    /// Claims them, out of what `io` and `irq` grant.
    pub fn claim(_io: &Resource, _irq: &Resource) -> Platform {
        Platform
    }

    /// None of them could be claimed.
    pub fn none() -> Platform {
        Platform
    }

    pub fn is_empty(&self) -> bool {
        true
    }

    /// Copies of their handles, by role, for a run of the driver VM.
    pub fn handles(&self) -> Vec<(u32, Handle)> {
        Vec::new()
    }
}
