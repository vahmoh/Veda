// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Turning the machine off and resetting it, and starting processors: PSCI,
//! Arm's interface to the firmware below the kernel, through the
//! instruction the FADT says.

use core::arch::asm;

const CPU_ON: u64 = 0xC400_0003;
const SYSTEM_OFF: u64 = 0x8400_0008;
const SYSTEM_RESET: u64 = 0x8400_0009;

/// Calls PSCI's `function`.
///
/// # Safety
/// The call's arguments must be what it takes.
unsafe fn psci(function: u64, a1: u64, a2: u64, a3: u64) -> i64 {
    let hypervisor = super::acpi::PLATFORM.get().is_some_and(|p| p.psci_hvc);
    let mut result = function;
    // SAFETY: per the caller; the SMC calling convention keeps x18 and up.
    unsafe {
        macro_rules! call {
            ($instruction:literal) => {
                asm!($instruction, inlateout("x0") result, inlateout("x1") a1 => _, inlateout("x2") a2 => _,
                    inlateout("x3") a3 => _, lateout("x4") _, lateout("x5") _, lateout("x6") _, lateout("x7") _,
                    lateout("x8") _, lateout("x9") _, lateout("x10") _, lateout("x11") _, lateout("x12") _,
                    lateout("x13") _, lateout("x14") _, lateout("x15") _, lateout("x16") _, lateout("x17") _,
                    options(nostack))
            };
        }
        if hypervisor {
            call!("hvc #0");
        } else {
            call!("smc #0");
        }
    }
    result as i64
}

/// Starts the processor of affinity `target` at physical address `entry`,
/// with `context` in `x0`, its MMU off. The firmware's error if it cannot.
///
/// # Safety
/// `entry` must be where such a processor can run.
pub(in crate::architecture) unsafe fn cpu_on(target: u64, entry: u64, context: u64) -> Result<(), i64> {
    // SAFETY: per the caller.
    match unsafe { psci(CPU_ON, target, entry, context) } {
        0 => Ok(()),
        e => Err(e),
    }
}

/// Powers the machine off. Returns only on failure.
pub fn off() {
    // SAFETY: the call takes no arguments.
    unsafe { psci(SYSTEM_OFF, 0, 0, 0) };
}

/// Resets the machine. Returns only on failure.
pub fn reboot() {
    // SAFETY: the call takes no arguments.
    unsafe { psci(SYSTEM_RESET, 0, 0, 0) };
}
