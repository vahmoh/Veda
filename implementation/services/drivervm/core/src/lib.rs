// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What the driver VM's monitor decides that reaches no object of Veda's,
//! apart from the program (`implementation/services/drivervm`) so that its
//! unit tests run on the host. The program keeps the objects and makes the system calls; these
//! keep the books and say what may be done:
//!
//! * [`waits`]: the guest's waits that go on, within what the kernel waits
//!   on at once, and what becomes of each at the end of a round.
//! * [`window`]: where in the bridge's window of guest-physical memory the
//!   VMOs the guest maps go.
//! * [`ledger`]: what the guest makes Veda keep for it (memory, mappings),
//!   within its budget.
//! * [`functions`]: where the guest's PCI functions go on its bus, where
//!   their BARs go, how many MSIs each may have.
//! * [`arch`]: what is particular to the architecture; on AMD64 the PC's
//!   keyboard controller, as the guest may drive it.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

#[cfg(target_arch = "x86_64")]
#[path = "architectures/amd64/mod.rs"]
pub mod arch;
pub mod functions;
pub mod ledger;
pub mod waits;
pub mod window;
