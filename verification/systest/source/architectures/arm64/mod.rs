// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What `systest` does on ARM64 that is the processor's own.

/// Faults on purpose, with an undefined instruction (`udf`): the kernel
/// ends the process.
pub fn crash() -> ! {
    // SAFETY: `udf` raises the exception and nothing runs after it.
    unsafe { core::arch::asm!("udf #0", options(noreturn)) }
}
