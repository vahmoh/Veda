// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Helpers for the `build.rs` scripts of Veda user-space crates:
//! [`user_program`] gives the linker (LLVM's, in its ELF flavour) what
//! makes a `no_std` binary a Veda program: a position-independent ELF64
//! executable without a C runtime ([`LINK_ARGS`]), which its loader places
//! at a random address and relocates; on ARM64's soft-float target, whose
//! `core` is not position-independent, one whose read-only data its loader
//! relocates too ([`arm64::SOFT_FLOAT_LINK_ARGS`]).
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
/// position-independent executable):
///
/// * it starts at `runtime`'s entry point, which takes the bootstrap
///   channel;
/// * its relative relocations are packed (`DT_RELR`: a word for up to 63 of
///   them, where `DT_RELA` takes 24 bytes for each);
/// * functions with the same code are kept once (Rust promises no function
///   an address of its own).
pub const LINK_ARGS: &[&str] = &["--entry=_rt_start", "-zpack-relative-relocs", "--icf=all"];

/// Whether `target` is one Veda programs are built for: the user target,
/// or the soft-float one of a program that leaves the vector registers
/// alone.
fn is_user_target(target: &str) -> bool {
    [amd64::TARGET, arm64::TARGET, amd64::SOFT_FLOAT_TARGET, arm64::SOFT_FLOAT_TARGET].contains(&target)
}

/// Emits the linker arguments for a Veda user-space executable, and those
/// that make it position-independent on ARM64's soft-float target. Call
/// this from the `build.rs` of every program crate.
pub fn user_program() {
    println!("cargo:rerun-if-changed=build.rs");
    let target = std::env::var("TARGET").unwrap_or_default();
    if is_user_target(&target) {
        for arg in LINK_ARGS {
            println!("cargo:rustc-link-arg-bins={arg}");
        }
    }
    if target == arm64::SOFT_FLOAT_TARGET {
        for arg in arm64::SOFT_FLOAT_LINK_ARGS {
            println!("cargo:rustc-link-arg-bins={arg}");
        }
    }
}
