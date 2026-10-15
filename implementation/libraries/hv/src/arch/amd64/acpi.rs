// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What the ACPI tables (`crate::acpi`) say of the PC a guest has: its
//! keyboard controller (the i8042), where the guest has it, with the ports
//! and lines a PC's is at; and the IA-PC boot architecture's flags in the
//! FADT: whether there is a keyboard controller, and that there is no VGA
//! and no CMOS clock.

use alloc::vec::Vec;

use vacpi::asm::{cat, device, eisa, io, irq_no_flags, name, resource_template};

use crate::acpi::Platform;

/// The PC's keyboard controller: its data port, its status and command
/// port, and the keyboard's and the mouse's interrupt lines (GSIs).
pub const I8042_DATA: u16 = 0x60;
pub const I8042_COMMAND: u16 = 0x64;
pub const KEYBOARD_GSI: u32 = 1;
pub const MOUSE_GSI: u32 = 12;

/// Where the FADT has its IA-PC boot architecture flags, and three of them.
const IAPC_BOOT_ARCH: usize = 109;
const BOOT_8042: u16 = 1 << 1;
const BOOT_VGA_NOT_PRESENT: u16 = 1 << 2;
const BOOT_CMOS_RTC_NOT_PRESENT: u16 = 1 << 5;

/// What the tables say of the PC a guest has, with the keyboard
/// controller if `i8042`.
pub fn platform(i8042: bool) -> Platform {
    let mut devices = Vec::new();
    if i8042 {
        let keyboard = cat(&[&io(I8042_DATA, 1), &io(I8042_COMMAND, 1), &irq_no_flags(KEYBOARD_GSI as u8)]);
        let mouse = irq_no_flags(MOUSE_GSI as u8);
        devices.extend_from_slice(&device(
            "PS2K",
            &cat(&[&name("_HID", &eisa("PNP0303")), &name("_CRS", &resource_template(&keyboard))]),
        ));
        devices.extend_from_slice(&device(
            "PS2M",
            &cat(&[&name("_HID", &eisa("PNP0F13")), &name("_CRS", &resource_template(&mouse))]),
        ));
    }
    let flags = BOOT_VGA_NOT_PRESENT | BOOT_CMOS_RTC_NOT_PRESENT | if i8042 { BOOT_8042 } else { 0 };
    Platform { devices, boot_flags: Some((IAPC_BOOT_ARCH, flags)) }
}

#[cfg(test)]
mod tests {
    use vacpi::NoMemory;
    use vacpi::aml::Namespace;
    use vacpi::device;
    use vacpi::name::Path;
    use vacpi::resource::Resource;

    use super::*;
    use crate::acpi::{Machine, describe};

    #[test]
    fn a_pcs_keyboard_controller() {
        let at = super::super::memory::ACPI_AT;
        let pc = platform(true);
        let m = Machine {
            functions: &[],
            gpio: &[],
            platform: &pc,
            at,
            pci_low: 0xC000_0000..0xFEC0_0000,
            pci_high: 0..1,
            firmware_tables: &[],
        };
        let t = describe(&m).tables;
        assert!(t.len() <= super::super::memory::ACPI_ROOM);
        // The FADT's boot architecture flags.
        assert_eq!(&t[96..100], b"FACP");
        let flags = u16::from_le_bytes([t[96 + IAPC_BOOT_ARCH], t[96 + IAPC_BOOT_ARCH + 1]]);
        assert_eq!(flags, BOOT_8042 | BOOT_VGA_NOT_PRESENT | BOOT_CMOS_RTC_NOT_PRESENT);
        // What an OS finds in the DSDT.
        let dsdt = (u64::from_le_bytes(t[96 + 140..96 + 148].try_into().unwrap()) - at) as usize;
        let len = u32::from_le_bytes(t[dsdt + 4..dsdt + 8].try_into().unwrap()) as usize;
        let mut ns = Namespace::new();
        ns.load(&t[dsdt + 36..dsdt + len], &NoMemory).unwrap();
        let path = |p| Path::parse(p).unwrap();
        let keyboard = device::resources(&ns, &path("\\_SB.PS2K"), &NoMemory).unwrap();
        assert_eq!(keyboard[..2], [Resource::Io { base: 0x60, length: 1 }, Resource::Io { base: 0x64, length: 1 }]);
        assert!(matches!(&keyboard[2], Resource::Irq { irqs, edge: true, .. } if irqs == &[1]));
        assert!(device::identify(&ns, &path("\\_SB.PS2M"), &NoMemory).is("PNP0F13"));
        // Without the controller, the flags say so.
        let flags = platform(false).boot_flags.map(|(_, f)| f);
        assert_eq!(flags, Some(BOOT_VGA_NOT_PRESENT | BOOT_CMOS_RTC_NOT_PRESENT));
    }
}
