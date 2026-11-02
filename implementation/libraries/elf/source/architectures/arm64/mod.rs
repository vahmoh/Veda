// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! ELF64 executables for ARM64 (AArch64), as Arm's ELF ABI has them.

use crate::Machine;

/// AArch64: `EM_AARCH64`, the platform the auxiliary vector names, and
/// `R_AARCH64_RELATIVE`.
pub const MACHINE: Machine = Machine { code: 183, name: "AArch64", platform: b"aarch64", relative: 1027 };
