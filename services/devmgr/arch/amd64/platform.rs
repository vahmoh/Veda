// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The PC's own devices the driver VM gets: its keyboard controller (the
//! i8042): its data port, its command port (the ports around them stay
//! Veda's), and its keyboard's and mouse's interrupts (ISA 1 and 12). It
//! does no DMA, so it needs no IOMMU; the driver VM's monitor keeps it from
//! resetting the machine.

use alloc::vec::Vec;

use vabi::irq_flags;
use vrt::object::{Handle, Interrupt, IoPorts, Resource};

/// Roles of the keyboard controller's data port, its command port, and its
/// keyboard's and mouse's interrupts, which the driver VM gets.
const DRIVERVM_I8042_ROLES: [u32; 4] = [
    vabi::startup::role::USER + 3,
    vabi::startup::role::USER + 4,
    vabi::startup::role::USER + 5,
    vabi::startup::role::USER + 6,
];

struct I8042 {
    data: IoPorts,
    command: IoPorts,
    keyboard: Interrupt,
    mouse: Interrupt,
}

impl I8042 {
    fn claim(io: &Resource, irq: &Resource) -> Option<I8042> {
        Some(I8042 {
            data: IoPorts::create(io, 0x60, 1).ok()?,
            command: IoPorts::create(io, 0x64, 1).ok()?,
            keyboard: Interrupt::create(irq, 1, irq_flags::ISA).ok()?,
            mouse: Interrupt::create(irq, 12, irq_flags::ISA).ok()?,
        })
    }

    /// Copies of its handles, by role, for a run of the driver VM.
    fn handles(&self) -> Option<Vec<(u32, Handle)>> {
        let handles = [
            self.data.0.duplicate(None),
            self.command.0.duplicate(None),
            self.keyboard.0.duplicate(None),
            self.mouse.0.duplicate(None),
        ];
        DRIVERVM_I8042_ROLES.into_iter().zip(handles).map(|(role, h)| Some((role, h.ok()?))).collect()
    }
}

/// The PC's own devices the driver VM gets.
pub struct Platform {
    i8042: Option<I8042>,
}

impl Platform {
    /// Claims them, out of what `io` and `irq` grant. Their ports and
    /// interrupts are the PC's whether or not anything answers there.
    pub fn claim(io: &Resource, irq: &Resource) -> Platform {
        Platform { i8042: I8042::claim(io, irq) }
    }

    /// None of them could be claimed.
    pub fn none() -> Platform {
        Platform { i8042: None }
    }

    pub fn is_empty(&self) -> bool {
        self.i8042.is_none()
    }

    /// Copies of their handles, by role, for a run of the driver VM.
    pub fn handles(&self) -> Vec<(u32, Handle)> {
        self.i8042.as_ref().and_then(I8042::handles).unwrap_or_default()
    }
}
