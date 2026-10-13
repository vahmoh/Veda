//! How the AMD64 kernel is linked (for `build.rs`): a PE image for the UEFI
//! target, at a fixed address in the higher half.

/// The target the kernel is built for.
pub const TARGET: &str = "x86_64-unknown-uefi";

/// The linker's arguments. The base address is `bootinfo::KERNEL_BASE`,
/// which the loader checks the image against.
pub const LINK_ARGS: &[&str] = &[
    "/ENTRY:kernel_entry",
    "/SUBSYSTEM:NATIVE",
    "/BASE:0xFFFFFFFF80000000",
    "/FIXED",
    "/DYNAMICBASE:NO",
    "/MERGE:.eh_fram=.rdata",
];
