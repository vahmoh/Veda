// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The system calls of Linux's AArch64 ABI beyond its generic one: none.
//! AArch64 was born with the generic ABI, which has no older forms of
//! calls.

use crate::error::SysResult;

/// Runs Linux system call `n` if it is one of AArch64's own; `None` for the
/// others, which is all of them.
///
/// # Safety
/// The arguments are the program's: pointers must be valid for the call.
pub unsafe fn dispatch(_n: usize, _a: [usize; 6]) -> Option<SysResult> {
    None
}
