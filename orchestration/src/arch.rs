// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The architecture Veda is built for.
//!
//! Everything about building Veda and running it in QEMU that depends on
//! the architecture Veda is built for (not on the one `orchestration` runs
//! on) is in `orchestration/src/arch/<architecture>/`. Veda is built for
//! AMD64 (`orchestration/src/arch/amd64/`). The rest of `orchestration`
//! reaches it only through the interface below, which every architecture
//! provides with the same names and meanings:
//!
//! | Item | What it provides |
//! |------|------------------|
//! | `NAME` | the architecture's directories' name (`arch/<NAME>` throughout the tree) |
//! | `UEFI_TARGET`, `KERNEL_TARGET`, `USER_TARGET`, `BARE_TARGET`, `GUEST_TARGET`, `STD_TARGET` | Rust's targets: of the loader; of the kernel; of user-space programs; of the code C programs link; of the driver VM's programs; of programs on Rust's standard library |
//! | `C_TARGET`, `GUEST_C_TARGET` | the C toolchains' targets: Veda's, and the driver VM's |
//! | `PORT_TOOLS`, `check_port_tools` | what the ports need from the system besides what every architecture's need |
//! | `MACHINE` | the machine of Veda's executables (the kernel's and the programs') |
//! | `UEFI_BOOT_PATH` | where the loader goes on the boot partition |
//! | `nested_virtualization` | whether QEMU lets Veda's hypervisor run |
//! | `qemu` | QEMU's machine: its emulator, firmware and arguments, its chipset's devices, its processors' registers |
//! | `acpitest` | ACPI tables that tests add to the machine's |
//!
//! Code in `src/arch/<architecture>/` may use the rest of `orchestration`;
//! the rest of `orchestration` uses nothing of it but this.

mod amd64;

pub use amd64::{
    BARE_TARGET, C_TARGET, GUEST_C_TARGET, GUEST_TARGET, KERNEL_TARGET, MACHINE, NAME, PORT_TOOLS, STD_TARGET,
    UEFI_BOOT_PATH, UEFI_TARGET, USER_TARGET, acpitest, check_port_tools, nested_virtualization, qemu,
};
