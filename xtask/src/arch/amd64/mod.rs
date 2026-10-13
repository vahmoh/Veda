// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Building Veda for AMD64 (x86-64), and running it on QEMU's PC.

pub mod acpitest;
pub mod qemu;

use std::process::Command;

use crate::util::Result;

/// The architecture's directories: `arch/amd64` throughout the tree.
pub const NAME: &str = "amd64";

/// Rust's target of the loader and the kernel: freestanding, soft-float,
/// PE/COFF output.
pub const UEFI_TARGET: &str = "x86_64-unknown-uefi";

/// Rust's target of user-space programs: `no_std` PE executables, built
/// for Windows' target, which gives them hard-float SSE code on stable
/// Rust, and linked by LLVM's linker in the Microsoft linker's flavour
/// (`libraries/build`).
pub const USER_TARGET: &str = vbuild::amd64::TARGET;

/// Rust's target of the code C programs link: the POSIX layer and the media
/// decoder's Rust half.
pub const BARE_TARGET: &str = "x86_64-unknown-none";

/// Rust's target of the driver VM's programs: Linux's, on musl.
pub const GUEST_TARGET: &str = "x86_64-unknown-linux-musl";

/// The C toolchain's target (`x86_64-veda-gcc` and friends), as the ports
/// build it (`ports/arch/amd64/settings.sh`).
pub const C_TARGET: &str = "x86_64-veda";

/// The target of the driver VM's C toolchain (`x86_64-linux-musl-gcc` and
/// friends), Linux's own, as the ports build it (the same).
pub const GUEST_C_TARGET: &str = "x86_64-linux-musl";

/// The machine of the kernel's image.
pub const KERNEL_MACHINE: vpe::Machine = vpe::amd64::MACHINE;

/// Where the loader goes on the boot partition: the boot program UEFI
/// starts from a disk of its own on x64.
pub const UEFI_BOOT_PATH: &str = "EFI/BOOT/BOOTX64.EFI";

/// Whether KVM lets Veda's hypervisor run in QEMU: it gives its guests VMX
/// of their own (nested virtualization, on Intel's processors), which Veda's
/// hypervisor runs on.
pub fn nested_virtualization() -> Result {
    let nested = std::fs::read_to_string("/sys/module/kvm_intel/parameters/nested").unwrap_or_default();
    if matches!(nested.trim(), "Y" | "1") {
        Ok(())
    } else {
        Err("KVM gives guests no VMX here (kvm_intel nested=1)".into())
    }
}

/// What the ports need from the system, besides what every architecture's
/// need: nasm, which assembles the media libraries' x86 code.
pub const PORT_TOOLS: &[&str] = &["nasm"];

/// The oldest nasm that assembles dav1d (2.14, for its AVX-512 code).
const NASM_MIN: (u32, u32) = (2, 14);

/// Whether [`PORT_TOOLS`], which are there, are recent enough.
pub fn check_port_tools() -> Result {
    match nasm_version() {
        Some(v) if v >= NASM_MIN => Ok(()),
        _ => Err(format!(
            "nasm {}.{} or newer is needed: install it from the distribution's packages (see documentation/C.md)",
            NASM_MIN.0, NASM_MIN.1
        )),
    }
}

/// nasm's version (`NASM version 2.16.03 ...`).
fn nasm_version() -> Option<(u32, u32)> {
    let out = Command::new("nasm").arg("-v").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let version = text.split_whitespace().nth(2)?;
    let mut parts = version.split('.').map(|p| p.parse::<u32>().ok());
    Some((parts.next()??, parts.next().flatten().unwrap_or(0)))
}
