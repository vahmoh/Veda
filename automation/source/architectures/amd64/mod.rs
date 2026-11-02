// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Building Veda for AMD64 (x86-64), and running it on QEMU's PC.

pub mod acpitest;
pub mod qemu;

use std::process::Command;

use crate::architecture::{AcpiTests, Architecture, Qemu};
use crate::util::Result;

pub const ARCHITECTURE: Architecture = Architecture {
    name: "amd64",
    // Directly in `target`: the first architecture's place, as Cargo keeps
    // what it builds for its host at the top of `target`.
    outputs: &["target"],
    // UEFI's target, freestanding and soft-float, whose programs are PE/COFF
    // images, the one format UEFI's firmware starts.
    uefi_target: "x86_64-unknown-uefi",
    // Freestanding and soft-float, ELF64 output (the kernel's `build.rs`
    // links it at its address).
    kernel_target: "x86_64-unknown-none",
    // `no_std` position-independent ELF64 executables for the System V ABI,
    // built for Linux's musl target, which gives them hard-float SSE code on
    // stable Rust, and linked by LLVM's linker with nothing of Linux's or
    // musl's (`implementation/libraries/build`).
    user_target: build::amd64::TARGET,
    bare_target: "x86_64-unknown-none",
    // Linux's, on musl. It is the user-space programs' too: these are built
    // apart, with musl (`implementation/guests/linux/.cargo/config.toml`).
    guest_target: "x86_64-unknown-linux-musl",
    // The rust port's own, which the toolchain builds, on the C library
    // (`implementation/ports/rust`, its settings in
    // `implementation/ports/architectures/amd64/settings.sh`).
    std_target: "x86_64-unknown-veda",
    // `x86_64-veda-gcc` and friends, as the ports build them (the same).
    c_target: "x86_64-veda",
    // `x86_64-linux-musl-gcc` and friends, Linux's own, as the ports build
    // them (the same).
    guest_c_target: "x86_64-linux-musl",
    // nasm assembles the media libraries' x86 code.
    port_tools: &["nasm"],
    check_port_tools,
    intel_gpus: true,
    machine: elf::amd64::MACHINE,
    // The boot program UEFI starts from a disk of its own on x64.
    uefi_boot_path: "EFI/BOOT/BOOTX64.EFI",
    nested_virtualization,
    qemu: Qemu {
        binary: qemu::BINARY,
        firmware_dirs: qemu::FIRMWARE_DIRS,
        firmware_files: qemu::FIRMWARE_FILES,
        firmware_packages: qemu::FIRMWARE_PACKAGES,
        blank_at_reset: qemu::blank_at_reset,
        has_keyboard: true,
        machine_args: qemu::machine_args,
        display_args: qemu::display_args,
        fixed_screen: None,
        hda_controller: qemu::HDA_CONTROLLER,
        debug_exit: Some(qemu::DEBUG_EXIT),
        processors: qemu::processors,
    },
    acpi_tests: AcpiTests { names: acpitest::NAMES, table: acpitest::table },
};

/// Whether KVM lets Veda's hypervisor run in QEMU: it gives its guests VMX
/// of their own (nested virtualization, on Intel's processors), which Veda's
/// hypervisor runs on.
fn nested_virtualization() -> Result {
    let nested = std::fs::read_to_string("/sys/module/kvm_intel/parameters/nested").unwrap_or_default();
    if matches!(nested.trim(), "Y" | "1") {
        Ok(())
    } else {
        Err("KVM gives guests no VMX here (kvm_intel nested=1)".into())
    }
}

/// The oldest nasm that assembles dav1d (2.14, for its AVX-512 code).
const NASM_MIN: (u32, u32) = (2, 14);

/// Whether the tools the ports need, which are there, are recent enough.
fn check_port_tools() -> Result {
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
