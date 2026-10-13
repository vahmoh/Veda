// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! ELF64 executables for AMD64 (x86-64), as Linux has them.

use crate::Machine;

/// x86-64: `EM_X86_64`, and the platform the auxiliary vector names.
pub const MACHINE: Machine = Machine { code: 62, name: "x86-64", platform: b"x86_64" };
