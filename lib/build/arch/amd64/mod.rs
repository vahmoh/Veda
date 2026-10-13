// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Veda's user programs on AMD64.

/// The target user programs are built for.
pub const TARGET: &str = "x86_64-pc-windows-msvc";

/// Image base of every Veda executable (the PE default for x86-64).
pub const IMAGE_BASE: &str = "0x140000000";
