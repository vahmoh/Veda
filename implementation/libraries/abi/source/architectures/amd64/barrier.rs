// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The order devices see a processor's accesses in, on AMD64: the order it
//! made them, so that only the compiler has to be told, but for what is
//! written to write-combining memory, which a fence drains.

use core::arch::asm;
use core::sync::atomic::{Ordering, compiler_fence};

/// What this processor wrote to memory reaches devices before what it
/// writes next, to memory or to their registers: between filling in what a
/// device is to read and telling it to.
#[inline]
pub fn device_write() {
    compiler_fence(Ordering::SeqCst);
}

/// What this processor reads of memory next is no older than what it read
/// so far, from memory or from devices' registers: between seeing that a
/// device is done and reading what it wrote.
#[inline]
pub fn device_read() {
    compiler_fence(Ordering::SeqCst);
}

/// What this processor wrote has reached its destination, out of the
/// processor's buffers, even from write-combining memory: before telling
/// another agent, such as the driver of a display, to read it.
#[inline]
pub fn device_complete() {
    // SAFETY: a fence only orders.
    unsafe { asm!("sfence", options(nostack, preserves_flags)) };
}
