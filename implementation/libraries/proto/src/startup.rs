// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What programs of the system's are started with, as the program that
//! starts each and the program itself agree: the roles of the handles it
//! gets (`vabi::startup::role::USER` and above), and what its exit code
//! says. One definition for both sides.

use vabi::startup::role::USER;

/// What `init` hands the device manager (`devmgr`): the hardware it shares
/// out among drivers.
pub mod devmgr {
    use super::USER;

    /// The I/O ports.
    pub const IOPORT_RESOURCE: u32 = USER + 1;
    /// The interrupts.
    pub const IRQ_RESOURCE: u32 = USER + 2;
    /// The physical memory of devices (MMIO).
    pub const MMIO_RESOURCE: u32 = USER + 3;
    /// Memory devices reach (DMA).
    pub const DMA_RESOURCE: u32 = USER + 4;
    /// Virtual machines, for the driver VM.
    pub const HYPERVISOR_RESOURCE: u32 = USER + 5;
    /// PCI functions: their MSIs, and giving them to virtual machines.
    pub const PCI_RESOURCE: u32 = USER + 6;
}

/// What the device manager hands a driver it starts.
pub mod driver {
    /// The `pcidev` channel of the driver's device ([`crate::pci`]).
    pub const PCIDEV: u32 = super::USER + 10;
}

/// What the device manager hands the driver VM's monitor (`drivervm`), and
/// what the monitor's exit code tells it.
pub mod drivervm {
    use super::USER;

    /// Virtual machines.
    pub const HYPERVISOR_RESOURCE: u32 = USER + 1;
    /// The `pcidev` channels of the guest's PCI functions, one each.
    pub const DEVICE: u32 = USER + 2;
    /// On AMD64, the PC's keyboard controller, if the guest gets it: its
    /// data port, its command port, its keyboard's interrupt and its
    /// mouse's.
    pub const I8042_DATA: u32 = USER + 3;
    pub const I8042_COMMAND: u32 = USER + 4;
    pub const I8042_KEYBOARD: u32 = USER + 5;
    pub const I8042_MOUSE: u32 = USER + 6;
    /// The PC's firmware variables, if it has some.
    pub const VARIABLES: u32 = USER + 7;

    /// How the monitor ended.
    pub mod exit_code {
        /// Linux powered the machine off.
        pub const POWER_OFF: i32 = 0;
        /// Linux asked for a restart.
        pub const RESTART: i32 = 2;
        /// Linux crashed, or shut its processor down.
        pub const CRASHED: i32 = 3;
        /// The guest did what the platform does not allow, or could not
        /// run.
        pub const FAULT: i32 = 4;
    }
}
