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

/// The linker's arguments for a program for [`SOFT_FLOAT_TARGET`], beyond
/// [`LINK_ARGS`](crate::LINK_ARGS). That target is the kernel's, which Rust
/// links at a fixed address, and its `core` and `alloc` are compiled for
/// one: their constants that hold addresses (a trait object's table, where
/// a panic is) are in read-only data. Such a program is linked
/// position-independent all the same (`-pie`), for its loader to place at a
/// random address, its read-only data relocated too (`-z notext`, which
/// marks it `DF_TEXTREL`): the loader relocates those pages in a copy of
/// its own before the program runs, and maps them read-only
/// (`elf::Elf::relocate`).
pub const SOFT_FLOAT_LINK_ARGS: &[&str] = &["-pie", "-znotext"];
