// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What is particular to ARM64 guests: the virtual machine a guest finds,
//! on any host (see the crate's introduction).
//!
//! * [`platform`]: how the paravirtual platform looks from an ARM64 guest.
//! * [`memory`]: where a guest has what is not its RAM.
//! * [`gic`]: the guest's interrupt controller as far as the processor
//!   does not do it itself: a GICv3's distributor and redistributors.
//! * [`features`]: what guests see of the processor.
//! * [`fdt`]: flattened device trees, which describe a guest to Linux.
//! * [`linux`]: Linux's boot protocol on ARM64.

pub mod fdt;
pub mod features;
pub mod gic;
pub mod linux;
pub mod memory;
pub mod platform;
