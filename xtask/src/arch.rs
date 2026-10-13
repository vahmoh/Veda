//! The architecture Veda is built for.
//!
//! Everything about building Veda and running it in QEMU that depends on
//! the architecture Veda is built for (not on the one xtask runs on) is in
//! `xtask/arch/<architecture>/`. Veda is built for AMD64
//! (`xtask/arch/amd64/`). The rest of xtask reaches it only through the
//! interface below, which every architecture provides with the same names
//! and meanings:
//!
//! | Item | What it provides |
//! |------|------------------|
//! | `NAME` | the architecture's directories' name (`arch/<NAME>` throughout the tree) |
//! | `UEFI_TARGET`, `USER_TARGET`, `BARE_TARGET`, `GUEST_TARGET` | Rust's targets: of the loader and the kernel; of user-space programs; of the code C programs link; of the driver VM's programs |
//! | `C_TARGET`, `GUEST_C_TARGET` | the C toolchains' targets: Veda's, and the driver VM's |
//! | `PORT_TOOLS`, `check_port_tools` | what the ports need from the system besides what every architecture's need |
//! | `KERNEL_MACHINE` | the machine of the kernel's image |
//! | `UEFI_BOOT_PATH` | where the loader goes on the boot partition |
//! | `nested_virtualization` | whether QEMU lets Veda's hypervisor run |
//! | `qemu` | QEMU's machine: its emulator, firmware and arguments, its chipset's devices, its processors' registers |
//! | `acpitest` | ACPI tables that tests add to the machine's |
//!
//! Code in `arch/<architecture>/` may use the rest of xtask; the rest of
//! xtask uses nothing of it but this.

#[path = "../arch/amd64/mod.rs"]
mod amd64;

pub use amd64::{
    BARE_TARGET, C_TARGET, GUEST_C_TARGET, GUEST_TARGET, KERNEL_MACHINE, NAME, PORT_TOOLS, UEFI_BOOT_PATH, UEFI_TARGET,
    USER_TARGET, acpitest, check_port_tools, nested_virtualization, qemu,
};
