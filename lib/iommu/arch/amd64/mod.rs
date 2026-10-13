// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The IOMMU of AMD64 machines: Intel VT-d.
//!
//! * [`dmar`]: the firmware's DMAR table: the remapping units, the devices
//!   each translates, the memory the firmware's devices use.
//! * [`vtd`]: a unit's registers and capabilities, and the structures it
//!   reads from memory: context entries, second-level page tables,
//!   interrupt remapping entries and the invalidation queue's descriptors.

pub mod dmar;
pub mod vtd;
