// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! QEMU's PC: the q35 machine (its ICH9 chipset), with EDK II's OVMF for
//! its firmware.

/// QEMU's emulator of the PC.
pub const BINARY: &str = "qemu-system-x86_64";

/// Where Linux distributions install OVMF, besides QEMU's own data
/// directories: Debian and Ubuntu, Fedora, Arch.
pub const FIRMWARE_DIRS: &[&str] = &["/usr/share/OVMF", "/usr/share/edk2/ovmf", "/usr/share/edk2/x64"];

/// The firmware and its variable store, as QEMU's own builds name them and
/// as Linux distributions do (Debian and Ubuntu, Fedora, Arch): both from
/// one place, as their sizes must match.
pub const FIRMWARE_FILES: &[(&str, &str)] = &[
    ("edk2-x86_64-code.fd", "edk2-i386-vars.fd"),
    ("OVMF_CODE.fd", "OVMF_VARS.fd"),
    ("OVMF_CODE_4M.fd", "OVMF_VARS_4M.fd"),
    ("OVMF_CODE.4m.fd", "OVMF_VARS.4m.fd"),
];

/// What installs the firmware.
pub const FIRMWARE_PACKAGES: &str = "the ovmf or edk2-ovmf package";

/// Memory that the firmware `code` needs blank at every reset, as it is at
/// power-on (its start and size), if any: OVMF's IGVM parameter area, from
/// the IGVM descriptor its reset vector carries (`OvmfPkg/ResetVector/X64/
/// IgvmMetadata.asm`): `IGVM`, its length, version 1 and the number of its
/// sections, then the sections, each a start (or offset), a size and an id,
/// the parameter area's 0x100.
///
/// OVMF reads the area at every boot for the number of processors and a
/// memory map that a VM started from an IGVM file is given there, without
/// telling whether this boot is one: a count it finds is taken as the
/// machine's. Before edk2's e4f99a8554 (December 2025) it also leaves the
/// area to the operating system as ordinary memory, which QEMU's resets
/// keep. So whatever Veda left there can pass for a processor count after a
/// reset (a program's header once had a 6 there), and the firmware then
/// waits for ever for processors that do not exist.
pub fn blank_at_reset(code: &[u8]) -> Option<(u64, u64)> {
    const PARAM_AREA: u32 = 0x100;
    let word = |at: usize| code.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let mut from = 0;
    while let Some(at) = code[from..].windows(4).position(|w| w == b"IGVM").map(|i| from + i) {
        from = at + 4;
        let (Some(length), Some(1), Some(sections)) = (word(at + 4), word(at + 8), word(at + 12)) else {
            continue;
        };
        if sections > 16 || length != 16 + 12 * sections {
            continue;
        }
        for section in (0..sections as usize).map(|i| at + 16 + 12 * i) {
            if let (Some(start), Some(size), Some(PARAM_AREA)) = (word(section), word(section + 4), word(section + 8))
                && size > 0
            {
                return Some((start as u64, size as u64));
            }
        }
    }
    None
}

/// The machine's own arguments: the PC (q35), with its keyboard controller
/// (PS/2's, the i8042) if `keyboard`, and without it as on many PCs, keys
/// then only coming through USB; with an IOMMU if `iommu`: Intel's VT-d,
/// remapping interrupts, which needs the I/O APIC in QEMU, not in KVM, and
/// comes before the devices it translates; and QEMU's most capable
/// processor.
pub fn machine_args(keyboard: bool, iommu: bool) -> Vec<String> {
    let mut machine = String::from("q35");
    if !keyboard {
        machine.push_str(",i8042=off");
    }
    if iommu {
        machine.push_str(",kernel-irqchip=split");
    }
    let mut args = vec!["-machine".to_string(), machine];
    if iommu {
        args.extend(["-device", "intel-iommu,intremap=on,eim=on"].map(String::from));
    }
    args.extend(["-cpu", "max"].map(String::from));
    args
}

/// The machine's HD Audio controller: the ICH9's.
pub const HDA_CONTROLLER: &str = "ich9-intel-hda";

/// The device through which the guest ends QEMU with an exit code
/// (`isa-debug-exit`), at the I/O port the kernel writes it at
/// (`implementation/kernel/src/architectures/amd64/power.rs`).
pub const DEBUG_EXIT: &str = "isa-debug-exit,iobase=0xf4,iosize=0x04";

/// Where each processor is, from QEMU's `info registers -a`: the code
/// segment's selector and the instruction pointer, and whether it halted.
pub fn processors(registers: &str) -> String {
    let mut out = Vec::new();
    let (mut cpu, mut rip, mut halted) = ("", "", false);
    for line in registers.lines() {
        if let Some(n) = line.strip_prefix("CPU#") {
            cpu = n.trim();
        } else if let Some(rest) = line.strip_prefix("RIP=") {
            rip = rest.split_whitespace().next().unwrap_or_default();
            halted = rest.split_whitespace().any(|w| w == "HLT=1");
        } else if let Some(rest) = line.strip_prefix("CS =") {
            let cs = rest.split_whitespace().next().unwrap_or_default();
            out.push(format!("processor {cpu} at {cs}:{rip}{}", if halted { " halted" } else { "" }));
        }
    }
    if out.is_empty() { String::from("no processors") } else { out.join(", ") }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An IGVM descriptor as OVMF's reset vector carries it, with `sections`
    /// (start or offset, size, id), amid other bytes.
    fn descriptor(sections: &[(u32, u32, u32)], version: u32) -> Vec<u8> {
        let mut image = b"..IGVM?..".to_vec();
        image.extend(b"IGVM");
        for v in [16 + 12 * sections.len() as u32, version, sections.len() as u32] {
            image.extend(v.to_le_bytes());
        }
        for &(a, b, c) in sections {
            image.extend([a, b, c].iter().flat_map(|v| v.to_le_bytes()));
        }
        image.extend(b"more");
        image
    }

    #[test]
    fn the_igvm_parameter_area_is_blanked() {
        // As Ubuntu's OVMF 2025.11 has it: the area, its memory map and its
        // processor count, then the HOB area.
        let ovmf =
            descriptor(&[(0x812000, 0x1000, 0x100), (0, 0xc0, 0x101), (0xc0, 16, 0x102), (0x813000, 0x1000, 0x200)], 1);
        assert_eq!(blank_at_reset(&ovmf), Some((0x812000, 0x1000)));
        // Without one (an older OVMF, or a build without the area), or of
        // another version: nothing to blank.
        assert_eq!(blank_at_reset(&descriptor(&[(0x813000, 0x1000, 0x200)], 1)), None);
        assert_eq!(blank_at_reset(&descriptor(&[(0x812000, 0x1000, 0x100)], 2)), None);
        assert_eq!(blank_at_reset(b"no descriptor here"), None);
    }

    #[test]
    fn the_machine_has_what_it_is_asked_for() {
        assert_eq!(machine_args(true, false), ["-machine", "q35", "-cpu", "max"]);
        assert_eq!(
            machine_args(false, true),
            [
                "-machine",
                "q35,i8042=off,kernel-irqchip=split",
                "-device",
                "intel-iommu,intremap=on,eim=on",
                "-cpu",
                "max"
            ]
        );
    }

    #[test]
    fn processors_are_where_their_registers_say() {
        let registers = "CPU#0\nRAX=0000000000000000 RBX=0000000000000000\n\
            RIP=000000000000fff0 RFL=00000002 [-------] CPL=0 II=0 A20=1 SMM=0 HLT=0\n\
            ES =0000 0000000000000000 0000ffff 00009300\n\
            CS =f000 00000000ffff0000 0000ffff 00009b00\n\
            CPU#1\nRIP=ffffffff80029bf0 RFL=00000246 [---Z-P-] CPL=0 II=0 A20=1 SMM=0 HLT=1\n\
            CS =0008 0000000000000000 ffffffff 00a09b00 DPL=0 CS64 [-RA]\n";
        assert_eq!(
            processors(registers),
            "processor 0 at f000:000000000000fff0, processor 1 at 0008:ffffffff80029bf0 halted"
        );
        assert_eq!(processors(""), "no processors");
    }
}
