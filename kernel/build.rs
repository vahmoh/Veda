//! Link the kernel as a fixed-address, higher-half PE image, as its
//! architecture says (`arch/<architecture>/link.rs`).

#[path = "arch/amd64/link.rs"]
mod amd64;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=arch/amd64/link.rs");
    let target = std::env::var("TARGET").unwrap_or_default();
    let link_args = match target.as_str() {
        amd64::TARGET => amd64::LINK_ARGS,
        // Building for the host (e.g. `cargo check --workspace`): nothing to do.
        _ => return,
    };
    for arg in link_args {
        println!("cargo:rustc-link-arg-bins={arg}");
    }
    // A linker map lets the build tool produce a symbol table for backtraces.
    let out = std::env::var("OUT_DIR").unwrap();
    println!("cargo:rustc-link-arg-bins=/MAP:{out}/vkernel.map");
    println!("cargo:rustc-env=VKERNEL_MAP={out}/vkernel.map");
}
