// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Helpers for the `build.rs` scripts of Veda user-space crates:
//! [`user_program`] emits the linker options that turn a `no_std` binary
//! into a Veda PE executable (fixed base, custom entry point, no CRT), in
//! the Microsoft linker's syntax, which LLVM's linker (`rust-lld`) takes.
//!
//! What depends on the architecture (its user target and the image base)
//! is in `lib/build/src/arch/<architecture>/`. A build script runs on the
//! build machine, so the target being built picks the architecture, not
//! the machine the script runs on.

#[path = "arch/amd64/mod.rs"]
pub mod amd64;

/// The image base of Veda executables for `target`, if it is a target Veda
/// programs are built for.
fn image_base(target: &str) -> Option<&'static str> {
    match target {
        amd64::TARGET => Some(amd64::IMAGE_BASE),
        _ => None,
    }
}

/// Emits the linker arguments for a Veda user-space executable. Call this
/// from the `build.rs` of every program crate.
pub fn user_program() {
    println!("cargo:rerun-if-changed=build.rs");
    let Some(base) = std::env::var("TARGET").ok().and_then(|t| image_base(&t)) else { return };
    for arg in [
        "/NODEFAULTLIB",
        "/ENTRY:_vrt_start",
        "/SUBSYSTEM:NATIVE",
        &format!("/BASE:{base}"),
        "/FIXED",
        "/DYNAMICBASE:NO",
        "/INCREMENTAL:NO",
        "/MANIFEST:NO",
    ] {
        println!("cargo:rustc-link-arg-bins={arg}");
    }
}
