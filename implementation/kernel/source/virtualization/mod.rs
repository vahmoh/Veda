// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The hypervisor: virtual machines for user-space monitors.
//!
//! The kernel gives a virtual machine monitor (a process holding a
//! hypervisor resource) two kinds of objects, and keeps to mechanism:
//!
//! * [`Guest`]: a guest-physical address space made of VMOs, whose pages
//!   stay committed while mapped;
//! * [`Vcpu`]: a virtual processor with its interrupt controller, which a
//!   thread of the monitor runs until the monitor is needed.
//!
//! What a guest's platform is (its memory map, how it boots, its devices
//! and hypercalls) is the monitor's. The kernel handles only what must be
//! fast or belongs to the processor. How guests run on the processor, and
//! how their physical addresses are translated, is the architecture's
//! (`architecture::virtualization`).

pub mod guest;

pub use guest::Guest;

pub use crate::architecture::virtualization::Vcpu;

/// What a guest may do with a page of its memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Access {
    pub read: bool,
    pub write: bool,
    pub execute: bool,
}
