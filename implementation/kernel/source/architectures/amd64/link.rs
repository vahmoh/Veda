// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! How the AMD64 kernel is linked (for `build.rs`): an ELF64 executable for
//! the freestanding target, at a fixed address in the top 2 GiB (where the
//! target's kernel code model has it).

/// The target the kernel is built for.
pub const TARGET: &str = "x86_64-unknown-none";

/// The linker's arguments (LLVM's linker, as the target has it). The
/// target links position-independent executables; the kernel is linked at
/// `bootinfo::KERNEL_BASE` instead, which the loader checks the executable
/// against. Functions with the same code are kept once (Rust promises no
/// function an address of its own).
pub const LINK_ARGS: &[&str] = &["--no-pie", "--image-base=0xffffffff80000000", "--entry=kernel_entry", "--icf=all"];
