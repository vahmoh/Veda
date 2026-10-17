// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Helpers for the `build.rs` scripts of Veda user-space crates:
//! [`user_program`] gives the linker (LLVM's, in its ELF flavour) what
//! makes a `no_std` binary a Veda program: a position-independent ELF64
//! executable without a C runtime ([`LINK_ARGS`]), which its loader places
//! at a random address and relocates.
//!
//! What depends on the architecture (its user target) is in
//! `implementation/libraries/build/src/architectures/<architecture>/`. A build
//! script runs on the build machine, so the target being built picks the
//! architecture, not the machine the script runs on.

#[path = "architectures/amd64/mod.rs"]
pub mod amd64;

/// The linker's arguments for a Veda program, beyond the target's (a static
/// position-independent executable):
///
/// * it starts at `vrt`'s entry point, which takes the bootstrap channel;
/// * its relative relocations are packed (`DT_RELR`: a word for up to 63 of
///   them, where `DT_RELA` takes 24 bytes for each);
/// * functions with the same code are kept once (Rust promises no function
///   an address of its own).
pub const LINK_ARGS: &[&str] = &["--entry=_vrt_start", "-zpack-relative-relocs", "--icf=all"];

/// Whether `target` is one Veda programs are built for.
fn is_user_target(target: &str) -> bool {
    target == amd64::TARGET
}

/// Emits the linker arguments for a Veda user-space executable. Call this
/// from the `build.rs` of every program crate.
pub fn user_program() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("TARGET").is_ok_and(|t| is_user_target(&t)) {
        for arg in LINK_ARGS {
            println!("cargo:rustc-link-arg-bins={arg}");
        }
    }
}
