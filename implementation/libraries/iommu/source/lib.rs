// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `iommu` — what Veda knows of IOMMUs that touches no hardware.
//!
//! Each architecture's IOMMU is in
//! `implementation/libraries/iommu/source/architectures/<architecture>/`:
//! [`amd64`], Intel VT-d. The kernel drives the units with it (its
//! architecture's `iommu`), and `device-manager` reads what the firmware's
//! tables say of them; the host's unit tests check the encodings against
//! the specification.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

#[path = "architectures/amd64/mod.rs"]
pub mod amd64;
