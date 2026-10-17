// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `cargo orchestration clippy`: Clippy over every crate, each built as Veda
//! builds it, its warnings errors:
//!
//! * the crates with host unit tests, for the host, their tests too;
//! * the loader, the kernel and the POSIX layer, for their freestanding
//!   targets;
//! * the programs on `rt` alone, for the user target;
//! * the programs on the standard library, for Veda's own target, with the
//!   toolchain's Clippy, which knows it (the build machine's does not).
//!
//! Clippy checks the workspace's crates a program builds with as well, as
//! they build for it: a library used both by programs on `rt` and by
//! programs on the standard library is checked as each has it.

use crate::arch::{BARE_TARGET, KERNEL_TARGET, STD_TARGET, UEFI_TARGET, USER_TARGET};
use crate::components::{PROGRAMS, Program};
use crate::runner;
use crate::toolchain;
use crate::util::{self, Result};

/// What follows Cargo's arguments: Clippy's, warnings as errors.
const DENY_WARNINGS: [&str; 2] = ["--", "-Dwarnings"];

/// `cargo orchestration clippy`.
pub fn command(args: &[String]) -> Result {
    if let Some(arg) = args.first() {
        return Err(format!("clippy takes no arguments ({arg})"));
    }
    util::status("Checking", "the crates with host tests, and their tests");
    util::run(runner::host_crates(&["clippy", "--quiet", "--all-targets"]).args(DENY_WARNINGS))?;
    for package in runner::HOST_TESTED_ALONE {
        util::run(
            util::cargo().args(["clippy", "--quiet", "--all-targets", "--package", package]).args(DENY_WARNINGS),
        )?;
    }
    for (package, target) in [("bootloader", UEFI_TARGET), ("kernel", KERNEL_TARGET), ("posix", BARE_TARGET)] {
        util::status("Checking", format!("{package} ({target})"));
        util::run(
            util::cargo().args(["clippy", "--quiet", "--package", package, "--target", target]).args(DENY_WARNINGS),
        )?;
    }
    let (on_std, native): (Vec<&Program>, Vec<&Program>) = PROGRAMS.iter().partition(|p| p.std);
    util::status("Checking", format!("{} programs on rt ({USER_TARGET})", native.len()));
    let mut cmd = util::cargo();
    cmd.args(["clippy", "--quiet", "--target", USER_TARGET]);
    for p in &native {
        cmd.args(["--package", p.package]);
    }
    util::run(cmd.args(DENY_WARNINGS))?;
    if !on_std.is_empty() {
        util::status("Checking", format!("{} programs on the standard library ({STD_TARGET})", on_std.len()));
        let mut cmd = toolchain::std_rust_tools()?.cargo_clippy()?;
        cmd.args(["--quiet", "--target", STD_TARGET]);
        for p in &on_std {
            cmd.args(["--package", p.package]);
        }
        util::run(cmd.args(DENY_WARNINGS))?;
    }
    Ok(())
}
