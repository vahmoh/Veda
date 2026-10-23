// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Turning a PC off and resetting it.

use super::port::{outb, outw};
use crate::firmware::acpi::ACPI;

/// QEMU's `isa-debug-exit` device (automated tests): writing it ends QEMU.
const DEBUG_EXIT_PORT: u16 = 0xF4;
/// The keyboard controller's command port, and its command that pulses
/// the reset line.
const I8042_COMMAND: u16 = 0x64;
const I8042_RESET: u8 = 0xFE;

/// Powers the machine off through ACPI: `\_S5`'s sleep type into the PM1
/// control registers (I/O ports on a PC). Under test, QEMU's debug exit
/// device ends the machine instead. Returns only on failure.
pub fn off() {
    if let Some(info) = ACPI.get() {
        let p = &info.power;
        if p.pm1a_cnt != 0 && p.s5_found {
            // SAFETY: writing SLP_TYP|SLP_EN to the PM1 control registers.
            unsafe {
                outw(p.pm1a_cnt, (p.slp_typ_a << 10) | (1 << 13));
                if p.pm1b_cnt != 0 {
                    outw(p.pm1b_cnt, (p.slp_typ_b << 10) | (1 << 13));
                }
            }
        }
    }
    // SAFETY: harmless if the device is absent.
    unsafe { outb(DEBUG_EXIT_PORT, 0x10) };
}

/// Resets the machine (FADT reset register, then the keyboard controller).
/// Returns only on failure.
pub fn reboot() {
    if let Some((reg, value)) = ACPI.get().and_then(|i| i.power.reset)
        && reg.space == 1
    {
        // SAFETY: firmware-described reset port.
        unsafe { outb(reg.address as u16, value) };
    }
    // SAFETY: the 8042's command to pulse the reset line.
    unsafe { outb(I8042_COMMAND, I8042_RESET) };
}
