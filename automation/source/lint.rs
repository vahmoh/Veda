// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `cargo automation clippy`: Clippy over every crate, each built as Veda
//! builds it, its warnings errors:
//!
//! * the crates with host unit tests, for the host, their tests too (of a
//!   service's package, the library alone: the program builds for its
//!   target, below);
//! * the loader, the kernel and the POSIX layer, for their freestanding
//!   targets;
//! * the programs on `runtime` alone, for the user target;
//! * the programs on the standard library, for Veda's own target, with the
//!   toolchain's Clippy, which knows it (the build machine's does not).
//!
//! The programs linked with the web engine (the browser) are left out
//! until it is built (`cargo automation web`), as the image leaves them
//! out: their build scripts, which Clippy runs too, need it.
//!
//! Clippy checks the workspace's crates a program builds with as well, as
//! they build for it: a library used both by programs on `runtime` and by
//! programs on the standard library is checked as each has it.

use crate::architecture::{BARE_TARGET, KERNEL_TARGET, STD_TARGET, UEFI_TARGET, USER_TARGET};
use crate::components::{PROGRAMS, Program};
use crate::runner;
use crate::toolchain;
use crate::util::{self, Result};

/// What follows Cargo's arguments: Clippy's, warnings as errors.
const DENY_WARNINGS: [&str; 2] = ["--", "-Dwarnings"];

/// `cargo automation clippy`.
pub fn command(args: &[String]) -> Result {
    if let Some(arg) = args.first() {
        return Err(format!("clippy takes no arguments ({arg})"));
    }
    util::status("Checking", "the crates with host tests, and their tests");
    // A program's package among them (a service's, with its library) has
    // `runtime` on Veda's targets alone: its library and tests are checked
    // here, its program below.
    let program = |package: &str| PROGRAMS.iter().any(|p| p.package == package);
    util::run(runner::host_crates(&["clippy", "--quiet", "--all-targets"], |p| !program(p)).args(DENY_WARNINGS))?;
    util::run(runner::host_crates(&["clippy", "--quiet", "--lib", "--tests"], program).args(DENY_WARNINGS))?;
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
    let web = crate::web::built();
    if !web && PROGRAMS.iter().any(|p| p.web) {
        util::status("Note", "the browser is not checked: its web engine is not built (`cargo automation web`)");
    }
    let (on_std, native): (Vec<&Program>, Vec<&Program>) =
        PROGRAMS.iter().filter(|p| web || !p.web).partition(|p| p.std);
    util::status("Checking", format!("{} programs on Veda's runtime ({USER_TARGET})", native.len()));
    let mut cmd = util::cargo();
    cmd.args(["clippy", "--quiet", "--target", USER_TARGET]);
    for p in &native {
        cmd.args(["--package", p.package]);
    }
    util::run(cmd.args(DENY_WARNINGS))?;
    if !on_std.is_empty() {
        util::status("Checking", format!("{} programs on the standard library ({STD_TARGET})", on_std.len()));
        let mut cmd = toolchain::std_rust_tools()?.cargo_clippy()?;
        // Where the web engine is, for the programs linked with it.
        cmd.env("VEDA_WEB", crate::web::root());
        cmd.args(["--quiet", "--target", STD_TARGET]);
        for p in &on_std {
            cmd.args(["--package", p.package]);
        }
        util::run(cmd.args(DENY_WARNINGS))?;
    }
    Ok(())
}
