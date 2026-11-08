// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Helpers for the `build.rs` scripts of Veda user-space crates:
//! [`user_program`] gives the linker (LLVM's, in its ELF flavour) what
//! makes a `no_std` binary a Veda program: a position-independent ELF64
//! executable without a C runtime ([`LINK_ARGS`]), which its loader places
//! at a random address and relocates. On ARM64, whose targets Rust links
//! at a fixed address, it is linked position-independent all the same
//! ([`arm64::LINK_ARGS`]); on its soft-float target, whose `core` is not
//! position-independent, as one whose read-only data its loader relocates
//! too ([`arm64::SOFT_FLOAT_LINK_ARGS`]).
//!
//! What depends on the architecture (its user target) is in
//! `implementation/libraries/build/source/architectures/<architecture>/`.
//! A build script runs on the build machine, so the target being built
//! picks the architecture, not the machine the script runs on.

#[path = "architectures/amd64/mod.rs"]
pub mod amd64;
#[path = "architectures/arm64/mod.rs"]
pub mod arm64;

/// The linker's arguments for a Veda program, beyond the target's (a static
/// executable, position-independent on AMD64; on ARM64,
/// [`arm64::LINK_ARGS`] makes it so):
///
/// * it starts at `runtime`'s entry point, which takes the bootstrap
///   channel;
/// * its relative relocations are packed (`DT_RELR`: a word for up to 63 of
///   them, where `DT_RELA` takes 24 bytes for each);
/// * functions with the same code are kept once (Rust promises no function
///   an address of its own).
pub const LINK_ARGS: &[&str] = &["--entry=_rt_start", "-zpack-relative-relocs", "--icf=all"];

/// The linker's arguments for a Veda program built for `target`, beyond
/// the target's; none if Veda programs are not built for it. They are built
/// for the user target, or for the soft-float one if they leave the vector
/// registers alone.
fn link_args(target: &str) -> Vec<&'static str> {
    match target {
        amd64::TARGET | amd64::SOFT_FLOAT_TARGET => LINK_ARGS.to_vec(),
        arm64::TARGET => [LINK_ARGS, arm64::LINK_ARGS].concat(),
        arm64::SOFT_FLOAT_TARGET => [LINK_ARGS, arm64::LINK_ARGS, arm64::SOFT_FLOAT_LINK_ARGS].concat(),
        _ => Vec::new(),
    }
}

/// Emits the linker's arguments for a Veda user-space executable, those of
/// the target it is built for. Call this from the `build.rs` of every
/// program crate.
pub fn user_program() {
    println!("cargo:rerun-if-changed=build.rs");
    for arg in link_args(&std::env::var("TARGET").unwrap_or_default()) {
        println!("cargo:rustc-link-arg-bins={arg}");
    }
}
