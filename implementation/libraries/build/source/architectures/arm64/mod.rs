// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Veda's user programs on ARM64.

/// The target user programs are built for: Linux's on musl, for the
/// AAPCS64's ELF64 executables and the hard-float `core` stable Rust ships
/// for it (nothing of Linux or musl is linked; see
/// `.cargo/architectures/arm64`).
pub const TARGET: &str = "aarch64-unknown-linux-musl";
