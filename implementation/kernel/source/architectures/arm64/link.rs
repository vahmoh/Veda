// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! How the ARM64 kernel is linked (for `build.rs`): an ELF64 executable for
//! the freestanding target, at a fixed address in the top 2 GiB, as on
//! AMD64.

/// The target the kernel is built for.
pub const TARGET: &str = "aarch64-unknown-none-softfloat";

/// The linker's arguments (LLVM's linker, as the target has it). The
/// kernel is linked at `bootinfo::KERNEL_BASE`, which the loader checks the
/// executable against, with segments on 4 KiB pages. Functions with the
/// same code are kept once (Rust promises no function an address of its
/// own).
pub const LINK_ARGS: &[&str] =
    &["--no-pie", "--image-base=0xffffffff80000000", "--entry=kernel_entry", "--icf=all", "-zmax-page-size=4096"];
