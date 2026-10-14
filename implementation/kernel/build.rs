// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Link the kernel as an ELF64 executable at a fixed address in the higher
//! half, as its architecture says (`src/arch/<architecture>/link.rs`).

#[path = "src/arch/amd64/link.rs"]
mod amd64;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/arch/amd64/link.rs");
    let target = std::env::var("TARGET").unwrap_or_default();
    let link_args = match target.as_str() {
        amd64::TARGET => amd64::LINK_ARGS,
        // Building for the host (e.g. `cargo check --workspace`): nothing to do.
        _ => return,
    };
    for arg in link_args {
        println!("cargo:rustc-link-arg-bins={arg}");
    }
}
