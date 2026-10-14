// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! ELF64 executables for AMD64 (x86-64), as the System V ABI's x86-64
//! supplement has them.

use crate::Machine;

/// x86-64: `EM_X86_64`, the platform the auxiliary vector names, and
/// `R_X86_64_RELATIVE`.
pub const MACHINE: Machine = Machine { code: 62, name: "x86-64", platform: b"x86_64", relative: 8 };
