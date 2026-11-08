// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Veda's user programs on ARM64.

/// The target user programs are built for: Linux's on musl, for the
/// AAPCS64's ELF64 executables and the hard-float `core` stable Rust ships
/// for it (nothing of Linux or musl is linked; see
/// `.cargo/architectures/arm64`).
pub const TARGET: &str = "aarch64-unknown-linux-musl";

/// The target of a program that must leave the vector registers alone
/// (Veda's Linux compatibility, whose Linux program has them):
/// freestanding and soft-float, as the kernel's.
pub const SOFT_FLOAT_TARGET: &str = "aarch64-unknown-none-softfloat";

/// Where a program for [`SOFT_FLOAT_TARGET`] is linked. That target is the
/// kernel's, whose `core` Rust ships compiled for a fixed address, so its
/// programs cannot be position-independent, as the others are: they are
/// linked at this address, which is clear of every other program's place
/// beside Linux compatibility's. A Linux program linked for a fixed
/// address has a low one (LLVM's linker's 0x200000, GNU's 0x400000, Go's
/// 0x10000); and this is not where Veda places the rest of a process
/// (`abi::PROGRAM_BASE` and up, the kernel's mappings from 16 TiB, Linux
/// compatibility's libraries and vDSO below 127 TiB), nor where Go's
/// runtime asks for its heap (`0x..c0_0000_0000`).
pub const SOFT_FLOAT_IMAGE_BASE: u64 = 0x2000_0000_0000;
