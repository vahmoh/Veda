// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The IOMMU of ARM64 machines: Arm's SMMUv3.
//!
//! * [`iort`]: the firmware's IORT table: where the SMMU is, its interrupts,
//!   and which stream each PCI function is.
//! * [`smmu`]: an SMMU's registers, and the structures it reads from memory:
//!   stream table entries, stage 2 tables and its queues' entries.

pub mod iort;
pub mod smmu;
