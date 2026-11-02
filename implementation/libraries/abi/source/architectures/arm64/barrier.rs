// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The order devices see a processor's accesses in, on ARM64: any, unless
//! it says otherwise. Devices are in the outer shareable domain, which
//! `core::sync::atomic::fence`, for the processors of the inner one, does
//! not reach.

use core::arch::asm;

/// What this processor wrote to memory reaches devices before what it
/// writes next, to memory or to their registers: between filling in what a
/// device is to read and telling it to.
#[inline]
pub fn device_write() {
    // SAFETY: a barrier only orders.
    unsafe { asm!("dmb oshst", options(nostack, preserves_flags)) };
}

/// What this processor reads of memory next is no older than what it read
/// so far, from memory or from devices' registers: between seeing that a
/// device is done and reading what it wrote.
#[inline]
pub fn device_read() {
    // SAFETY: a barrier only orders.
    unsafe { asm!("dmb oshld", options(nostack, preserves_flags)) };
}

/// What this processor wrote has reached its destination, out of the
/// processor's buffers, even from memory that is not cached: before telling
/// another agent, such as the driver of a display, to read it.
#[inline]
pub fn device_complete() {
    // SAFETY: a barrier only orders.
    unsafe { asm!("dsb st", options(nostack, preserves_flags)) };
}
