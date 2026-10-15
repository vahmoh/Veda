// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! ACPI tables that tests add to QEMU's (`-acpitable`), describing what a
//! PC's firmware does and QEMU's does not, for the driver VM to pass on:
//! devices on QEMU's PC, named as its firmware names them, on its I/O
//! APIC's lines.

use vacpi::asm::{
    HID_OVER_I2C, cat, device, dsm, gpio_descriptor_of, i2c_descriptor, int, interrupt_of, name, resource_template,
    scope, string, table as whole, uuid,
};
use vacpi::resource::Gpio;

/// The tables [`table`] makes.
pub const NAMES: &[&str] = &["touchpad", "gpio", "nhlt"];

/// The table `name` stands for: `touchpad`, an SSDT with a HID-over-I2C
/// device (as a laptop's touchpad is described, on GSI 10, active high,
/// since nothing drives that input of QEMU's I/O APIC) below QEMU's virtio
/// GPU at 00:10.0 (the firmware's `\_SB.PCI0.S80`), on its "bus"; `gpio`,
/// an SSDT with a device there wired to pins of a simulated GPIO
/// controller of devmgr's; `nhlt`, an NHLT (an Intel audio DSP's links),
/// with none.
pub fn table(name_of: &str) -> Option<Vec<u8>> {
    if name_of == "nhlt" {
        return Some(whole(b"NHLT", 0, b"TESTNHLT", &[0]));
    }
    let aml = match name_of {
        "touchpad" => {
            let controller = "\\_SB.PCI0.S80";
            let crs = cat(&[
                &i2c_descriptor(0x15, 400_000, false, controller),
                &interrupt_of(10, false, false, false, false),
            ]);
            let body = cat(&[
                &name("_HID", &string("VTST0001")),
                &name("_CID", &string("PNP0C50")),
                &name("_UID", &int(1)),
                &name("_CRS", &resource_template(&crs)),
                &dsm(&uuid(HID_OVER_I2C)?, 1, &int(0x20)),
            ]);
            scope(controller, &device("TPD0", &body))
        }
        "gpio" => {
            // One of devmgr's simulated GPIO controllers, and a device below
            // QEMU's virtio GPU wired to three of its pins: 0 an output, 1
            // an input that interrupts on both edges, 3 an input.
            let controller = "\\_SB.TGPI";
            let pin = |pin: u16, interrupt: bool, restriction: u8| Gpio {
                interrupt,
                pins: vec![pin],
                controller: controller.into(),
                pull: 3,
                restriction,
                edge: interrupt,
                polarity: if interrupt { 2 } else { 0 },
                shared: false,
                wake: false,
                debounce: 0,
            };
            let crs = cat(&[
                &gpio_descriptor_of(&pin(0, false, 2)),
                &gpio_descriptor_of(&pin(1, false, 1)),
                &gpio_descriptor_of(&pin(1, true, 0)),
                &gpio_descriptor_of(&pin(3, false, 1)),
            ]);
            let simulated = device("TGPI", &cat(&[&name("_HID", &string("VTST0002")), &name("_UID", &int(0))]));
            let wired =
                device("GPT0", &cat(&[&name("_HID", &string("VTST0003")), &name("_CRS", &resource_template(&crs))]));
            cat(&[&scope("\\_SB", &simulated), &scope("\\_SB.PCI0.S80", &wired)])
        }
        _ => return None,
    };
    Some(whole(b"SSDT", 2, b"TESTSSDT", &aml))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every table is a whole one (its length, its checksum), as the
    /// driver VM takes a firmware's table only whole; the NHLT keeps the
    /// name its script finds in Linux's log.
    #[test]
    fn the_tables_are_whole() {
        for name in NAMES {
            let table = table(name).unwrap_or_else(|| panic!("no table {name}"));
            assert!(vhv::acpi::is_table(&table), "{name}");
        }
        let nhlt = table("nhlt").unwrap();
        assert_eq!((&nhlt[..4], &nhlt[16..24], nhlt.len()), (&b"NHLT"[..], &b"TESTNHLT"[..], 37));
        assert_eq!(table("battery"), None);
    }
}
