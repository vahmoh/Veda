// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The clock's counter on ARM64: the generic timer's virtual count, which
//! the system counter drives, the same for every processor. It says its
//! own frequency, so there is nothing to measure and nothing to agree on.
//! What runs at EL1 sees the count offset by what a hypervisor set; at EL2
//! the offset is 0 (the loader or `smp` wrote it), for the kernel and for
//! guests alike.

use core::arch::asm;

use super::cpu::read_register;
use crate::time;

/// The counter's reading now, once earlier instructions are done with.
#[inline]
pub fn ticks() -> u64 {
    // SAFETY: an instruction barrier only orders.
    unsafe { asm!("isb", options(nomem, nostack, preserves_flags)) };
    read_register!("cntvct_el0")
}

/// Starts the monotonic clock at the frequency the counter gives.
pub(in crate::architecture) fn start() {
    time::start(read_register!("cntfrq_el0"));
}
