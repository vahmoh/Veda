// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Building Veda for ARM64 (AArch64), and running it on QEMU's `virt`
//! machine.

pub mod qemu;

use crate::architecture::{AcpiTests, Architecture, Qemu};
use crate::util::Result;

pub const ARCHITECTURE: Architecture = Architecture {
    name: "arm64",
    outputs: &["target", "arm64"],
    // UEFI's target, whose programs are PE/COFF images, the one format
    // UEFI's firmware starts.
    uefi_target: "aarch64-unknown-uefi",
    // Freestanding and soft-float, as the kernel keeps no user's floating
    // point and SIMD registers while it runs; ELF64 output (the kernel's
    // `build.rs` links it at its address).
    kernel_target: "aarch64-unknown-none-softfloat",
    // `no_std` position-independent ELF64 executables for the AAPCS64,
    // built for Linux's musl target, which gives them hard-float code on
    // stable Rust, and linked by LLVM's linker with nothing of Linux's or
    // musl's (`implementation/libraries/build`).
    user_target: build::arm64::TARGET,
    // Freestanding and soft-float: the kernel's own, for what runs beside a
    // Linux program in its process and must not touch its vector registers.
    soft_float_target: build::arm64::SOFT_FLOAT_TARGET,
    // Linux's on musl, as Rust's bare-metal target here is compiled to run
    // where it is linked, `core` included, which a shared library such as the
    // C library cannot hold. Nothing of Linux or musl comes with it.
    bare_target: "aarch64-unknown-linux-musl",
    // Linux's, on musl. It is the user-space programs' too: these are built
    // apart, with musl (`implementation/guests/linux/.cargo/config.toml`).
    guest_target: "aarch64-unknown-linux-musl",
    // The rust port's own, which the toolchain builds, on the C library
    // (`implementation/ports/rust`, its settings in
    // `implementation/ports/architectures/arm64/settings.sh`).
    std_target: "aarch64-unknown-veda",
    // `aarch64-veda-gcc` and friends, as the ports build them (the same).
    c_target: "aarch64-veda",
    // `aarch64-linux-musl-gcc` and friends, Linux's own, as the ports build
    // them (the same).
    guest_c_target: "aarch64-linux-musl",
    // GCC's assembler assembles the media libraries' Arm code.
    port_tools: &[],
    check_port_tools,
    intel_gpus: false,
    machine: elf::arm64::MACHINE,
    // The boot program UEFI starts from a disk of its own on AArch64.
    uefi_boot_path: "EFI/BOOT/BOOTAA64.EFI",
    nested_virtualization,
    qemu: Qemu {
        binary: qemu::BINARY,
        firmware_dirs: qemu::FIRMWARE_DIRS,
        firmware_files: qemu::FIRMWARE_FILES,
        firmware_packages: qemu::FIRMWARE_PACKAGES,
        blank_at_reset: |_| None,
        has_keyboard: false,
        machine_args: qemu::machine_args,
        display_args: qemu::display_args,
        fixed_screen: Some(qemu::SCREEN),
        hda_controller: qemu::HDA_CONTROLLER,
        // PSCI's `SYSTEM_OFF` ends QEMU by itself.
        debug_exit: None,
        processors: qemu::processors,
    },
    // No test needs tables of a PC's firmware here yet.
    acpi_tests: AcpiTests { names: &[], table: |_| None },
};

/// Whether Veda's hypervisor can run in QEMU: its emulator gives the
/// processors EL2 of their own wherever it does not run on KVM
/// (`qemu::machine_args`).
fn nested_virtualization() -> Result {
    if cfg!(target_arch = "aarch64") { Err("KVM gives guests no EL2 here".into()) } else { Ok(()) }
}

/// The ports need nothing beyond what every architecture's do.
fn check_port_tools() -> Result {
    Ok(())
}
