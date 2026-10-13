// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What `systest` does on AMD64 that is the processor's own.

/// Faults on purpose, with an invalid-opcode exception (`ud2`): the
/// kernel ends the process.
pub fn crash() -> ! {
    // SAFETY: `ud2` raises the exception and nothing runs after it.
    unsafe { core::arch::asm!("ud2", options(noreturn)) }
}
