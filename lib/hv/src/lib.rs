// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `vhv` — what Veda's hypervisor knows that touches no hardware.
//!
//! * [`platform`]: the paravirtual platform guests run on: its hypercalls.
//! * [`bridge`]: Veda's IPC for the guest's programs.
//! * [`pci`]: the configuration space of a PCI function given to a guest,
//!   and the stand-in for the PC's host bridge.
//! * [`igd`]: what an Intel integrated GPU's driver reads of the PC's
//!   firmware: its OpRegion, as the guest has it.
//! * [`acpi`]: the ACPI tables that describe the guest's machine.
//!
//! What is particular to the architecture its guests are (their virtual
//! processors' interrupt controller, what the processor tells them of
//! itself, how they find the platform and boot, the PC's devices and memory
//! map) is in `lib/hv/src/arch/<architecture>/`: [`amd64`]. It describes
//! those machines on any host, so its unit tests run anywhere.
//!
//! The kernel drives them under its virtual processors (its architecture's
//! `hv`), and the virtual machine monitor and the Linux guest's side use
//! the platform's definitions; the host's unit tests run them all.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod acpi;
#[path = "arch/amd64/mod.rs"]
pub mod amd64;
pub mod bridge;
pub mod igd;
pub mod pci;
pub mod platform;
