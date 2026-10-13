// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! PE32+ images for AMD64 (x86-64).

use crate::Machine;

/// The machine of AMD64 images (`IMAGE_FILE_MACHINE_AMD64`).
pub const MACHINE: Machine = Machine { code: 0x8664, name: "x86-64" };
