// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Foreign code: code of another system's ABI that a thread runs, whose
//! system calls and exceptions come back to the code that ran it (see
//! `abi`'s "Foreign code").

use abi::{Error, ForeignState};

/// Runs the calling thread's foreign code from the registers in `state`
/// until it makes a system call or takes an exception (what it returns,
/// one of `abi::foreign_exit`) or the thread is interrupted
/// (`Interrupted`; at once, without running it, if it had been, or with
/// `abi::wait_flags::DEFERRED` in `flags` if it deferred an
/// interruption): `state` then holds the foreign code's registers as it
/// left them. The x87, SSE and AVX registers (the floating point and SIMD
/// ones on ARM64) are as the foreign code left them.
pub fn run(state: &mut ForeignState, flags: usize) -> Result<usize, Error> {
    Error::from_return(crate::architecture::foreign_run(state, flags))
}
