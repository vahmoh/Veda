// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! QEMU's `virt` machine for ARM64, a server-class machine as Arm's
//! standards have one (UEFI, ACPI, GICv3, PSCI, SMMUv3), with EDK II's
//! AAVMF for its firmware.

use crate::qemu::Screen;

/// QEMU's emulator of ARM64 machines.
pub const BINARY: &str = "qemu-system-aarch64";

/// Where Linux distributions install AAVMF, besides QEMU's own data
/// directories: Debian and Ubuntu, then Fedora and Arch.
pub const FIRMWARE_DIRS: &[&str] = &["/usr/share/AAVMF", "/usr/share/edk2/aarch64"];

/// The firmware and its variable store, as QEMU's own builds name them and
/// as Linux distributions do (Debian and Ubuntu, Fedora, Arch): both from
/// one place, as their sizes must match. Without Secure Boot, which Veda's
/// loader is not signed for.
pub const FIRMWARE_FILES: &[(&str, &str)] = &[
    ("edk2-aarch64-code.fd", "edk2-arm-vars.fd"),
    ("AAVMF_CODE.no-secboot.fd", "AAVMF_VARS.fd"),
    ("AAVMF_CODE.fd", "AAVMF_VARS.fd"),
    ("QEMU_EFI.fd", "QEMU_VARS.fd"),
];

/// What installs the firmware.
pub const FIRMWARE_PACKAGES: &str = "the qemu-efi-aarch64 or edk2-aarch64 package";

/// The machine's own arguments: the `virt` machine, with GICv3 and its ITS
/// (Veda's devices' MSIs), and ACPI tables rather than a device tree; with
/// an IOMMU if `iommu`, an SMMUv3 in front of its PCI Express root, which
/// the ITS sees devices through. It has no keyboard of its own, whatever
/// `keyboard` says.
///
/// On an ARM64 build machine, KVM runs its processors as they are, without
/// EL2 of their own. Elsewhere QEMU's emulator runs its most capable
/// processor, with EL2 (Veda's hypervisor): its pointer authentication
/// keeps to QEMU's quick algorithm, which only QEMU has to agree with.
pub fn machine_args(_keyboard: bool, iommu: bool) -> Vec<String> {
    let mut machine = String::from("virt,gic-version=3,its=on,acpi=on");
    if iommu {
        machine.push_str(",iommu=smmuv3");
    }
    let processor = if cfg!(target_arch = "aarch64") {
        "host"
    } else {
        machine.push_str(",virtualization=on");
        "max,pauth-impdef=on"
    };
    let accelerator = if cfg!(target_arch = "aarch64") { "kvm" } else { "tcg,thread=multi" };
    let mut args: Vec<String> =
        ["-machine", &machine, "-accel", accelerator, "-cpu", processor].map(String::from).to_vec();
    if iommu {
        // Stage 2 is what Veda translates its guests' devices with.
        args.extend(["-global", "arm-smmuv3.stage=2"].map(String::from));
    }
    args
}

/// The largest mode AAVMF gives `ramfb`, the one it starts in.
pub const SCREEN: (u32, u32) = (1024, 768);

/// The display: `ramfb`, a framebuffer in the machine's memory, which QEMU
/// shows as `id`. It is the one display AAVMF gives a framebuffer of, as it
/// drives neither QEMU's VGA nor, but through its blocks' calls, virtio-gpu.
/// Its modes are AAVMF's own ([`SCREEN`]), so `screen` has no say in them.
pub fn display_args(id: &str, _screen: Screen) -> Vec<String> {
    vec![String::from("-device"), format!("ramfb,id={id}")]
}

/// The HD Audio controller a PCI Express root can have.
pub const HDA_CONTROLLER: &str = "intel-hda";

/// Where each processor is, from QEMU's `info registers -a`: its exception
/// level and the program counter, and whether it halted.
pub fn processors(registers: &str) -> String {
    let mut out = Vec::new();
    let (mut cpu, mut pc) = ("", "");
    for line in registers.lines() {
        let line = line.trim_start();
        if let Some(n) = line.strip_prefix("CPU#") {
            cpu = n.trim();
        } else if let Some(rest) = line.strip_prefix("PC=") {
            pc = rest.split_whitespace().next().unwrap_or_default();
        } else if let Some(rest) = line.strip_prefix("PSTATE=") {
            // PSTATE=20000005 --C- EL1h BTYPE=0
            let level = rest.split_whitespace().find(|w| w.starts_with("EL")).unwrap_or_default();
            out.push(format!("processor {cpu} at {level}:{pc}"));
        }
    }
    if out.is_empty() { String::from("no processors") } else { out.join(", ") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_machine_has_what_it_is_asked_for() {
        let args = machine_args(true, false);
        assert_eq!(args[0], "-machine");
        assert!(args[1].starts_with("virt,gic-version=3,its=on,acpi=on"));
        assert!(!args[1].contains("smmuv3"));
        assert!(machine_args(false, true)[1].contains(",iommu=smmuv3"));
    }

    #[test]
    fn processors_are_where_their_registers_say() {
        let registers = "CPU#0\n PC=ffffffff80029bf0 X00=0000000000000000 X01=0000000000000000\n\
            PSTATE=20000009 --C- EL2h    BTYPE=0\n\
            CPU#1\n PC=0000555500012340 X00=0000000000000001\n\
            PSTATE=60000000 -ZC- EL0t\n";
        assert_eq!(processors(registers), "processor 0 at EL2h:ffffffff80029bf0, processor 1 at EL0t:0000555500012340");
        assert_eq!(processors(""), "no processors");
    }
}
