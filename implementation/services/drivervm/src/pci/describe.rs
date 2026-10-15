// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What the guest's ACPI tables say about a function given (`vhv::acpi`):
//! its INTx, its constant data, and the devices the PC's firmware describes
//! below it, with their place on its bus (I2C or SPI), their interrupt
//! lines (which the guest gets), the GPIO pins they are wired to (`gpio`),
//! what their `_DSM` answers HID over I2C, and their constant data. What a
//! device uses that the guest cannot have is left out, and said.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use vhv::acpi::{self, Child, Function, Gpio, GpioInterrupt, HID_OVER_I2C, Resource};
use vproto::pci::{AcpiDevice, AcpiResource, IntxLine, pcidev};
use vrt::println;

use crate::gpio::{Owner, Pins};
use crate::lines::Lines;

/// The names of constant data an I2C controller's driver reads from its
/// device: the bus's timing at each speed.
const CONTROLLER_DATA: [&str; 4] = ["SSCN", "FMCN", "FPCN", "HSCN"];
/// HID over I2C's ids.
const HID_OVER_I2C_IDS: [&str; 2] = ["PNP0C50", "ACPI0C50"];

/// A function being given: its channel and name, its place among the
/// guest's functions, and the lines and GPIO pins its devices add to.
pub struct Given<'a> {
    pub pci: &'a pcidev::Client,
    pub name: &'a str,
    pub function: usize,
    pub lines: &'a mut Lines,
    pub pins: &'a mut Pins,
}

/// What the guest's tables say about function `f` at `devfn`: its INTx,
/// its constant data, and the devices the firmware describes below it,
/// whose interrupt lines and GPIO pins the guest gets. What a device uses
/// that the guest cannot have is left out, and said.
pub fn describe(f: &mut Given, devfn: u8, intx: Option<IntxLine>) -> Function {
    let pci = f.pci;
    let data_of = |device: u32, names: &[&str]| -> Vec<(String, Vec<u8>)> {
        names
            .iter()
            .filter_map(|n| match pci.acpi_data(device, String::from(*n)) {
                Ok(Ok(aml)) => Some((String::from(*n), aml)),
                _ => None,
            })
            .collect()
    };
    let children = pci
        .acpi_devices()
        .unwrap_or_default()
        .iter()
        .enumerate()
        .filter(|(_, d)| d.status & 1 != 0)
        .filter_map(|(i, d)| child(f, i as u32, d))
        .collect::<Vec<Child>>();
    Function { devfn, intx: intx.map(|l| (l.pin, l.gsi)), data: data_of(u32::MAX, &CONTROLLER_DATA), children }
}

/// Described device `index` (`d`) of function `f`, as the guest has it:
/// its connection to the function's bus, its interrupts (whose lines the
/// guest gets), the GPIO pins it is wired to, what its `_DSM` answers HID
/// over I2C, its `_DSD`.
fn child(f: &mut Given, index: u32, d: &AcpiDevice) -> Option<Child> {
    let (pci, name) = (f.pci, f.name);
    let (parent, own) = d.path.rsplit_once('.')?;
    let mut resources = Vec::new();
    let mut interrupts = 0;
    // The device's GPIO pins, counted in order.
    let mut pin_index = 0;
    for r in &d.resources {
        match r {
            AcpiResource::I2c { controller, address, speed_hz, ten_bit } if controller == parent => {
                resources.push(Resource::I2c { address: *address, speed_hz: *speed_hz, ten_bit: *ten_bit });
            }
            &AcpiResource::Spi { ref controller, chip_select, speed_hz, bits, cpol, cpha, cs_active_high }
                if controller == parent =>
            {
                let three_wire = false;
                resources.push(Resource::Spi { chip_select, speed_hz, bits, cpol, cpha, cs_active_high, three_wire });
            }
            AcpiResource::Gpio { .. } => {
                if let Some(g) = gpio_connection(f, index, d, r, &mut pin_index) {
                    resources.push(Resource::Gpio(g));
                }
            }
            AcpiResource::Irq { irqs, edge, active_low, shared } => {
                for _ in irqs {
                    let given = match pci.acpi_interrupt(index, interrupts) {
                        Ok(Ok((irq, line))) => {
                            (f.lines.has(line.gsi) || f.lines.add(line.gsi, irq, line.level)).then_some(line)
                        }
                        _ => None,
                    };
                    match given {
                        Some(line) => resources.push(Resource::Interrupt {
                            gsi: line.gsi,
                            edge: *edge,
                            active_low: *active_low,
                            shared: *shared,
                            wake: false,
                        }),
                        None => println!("{name}: {}'s interrupt {} cannot be given to the guest", d.path, interrupts),
                    }
                    interrupts += 1;
                }
            }
            other => println!("{name}: {} uses what the guest does not get: {:?}", d.path, other),
        }
    }
    let hid_over_i2c = HID_OVER_I2C_IDS.iter().any(|id| d.hid == *id || d.cids.iter().any(|c| c == id));
    let hid_descriptor = acpi::uuid(HID_OVER_I2C).filter(|_| hid_over_i2c).and_then(|uuid| {
        match pci.acpi_dsm(index, uuid.to_vec(), 1, 1) {
            Ok(Ok(address)) => u16::try_from(address).ok(),
            _ => None,
        }
    });
    let data = match pci.acpi_data(index, String::from("_DSD")) {
        Ok(Ok(aml)) => alloc::vec![(String::from("_DSD"), aml)],
        _ => Vec::new(),
    };
    let text = |s: &str| (!s.is_empty()).then(|| String::from(s));
    println!(
        "{name}: the guest has {} ({}) below it{}",
        d.path,
        if d.hid.is_empty() { "no id" } else { &d.hid },
        hid_descriptor.map(|a| format!(", its HID descriptor at {a:#x}")).unwrap_or_default()
    );
    Some(Child {
        name: String::from(own),
        hid: text(&d.hid),
        cids: d.cids.clone(),
        uid: text(&d.uid),
        sub: text(&d.sub),
        resources,
        hid_descriptor,
        data,
    })
}

/// GPIO connection `r` of described device `index` (`d`) of function `f`,
/// as the guest has it: its pins on the guest's controller for the PC's,
/// each driven through the function's channel (`pin_index` counts the
/// device's pins), an interrupt's pin raising a line of the platform's.
/// `None` if the guest gets none of its pins.
fn gpio_connection(f: &mut Given, index: u32, d: &AcpiDevice, r: &AcpiResource, pin_index: &mut u32) -> Option<Gpio> {
    let &AcpiResource::Gpio {
        interrupt,
        ref pins,
        ref controller,
        pull,
        restriction,
        shared,
        edge,
        polarity,
        wake,
        debounce,
    } = r
    else {
        return None;
    };
    let (path, uid) = f.pins.controller(controller);
    let mut given = Vec::new();
    for &pin in pins {
        let at = *pin_index;
        *pin_index += 1;
        if interrupt {
            let irq = match f.pci.gpio_interrupt(index, at) {
                Ok(Ok(irq)) => irq,
                e => {
                    println!(
                        "{}: {}'s interrupt on GPIO pin {} cannot be given to the guest: {:?}",
                        f.name, d.path, pin, e
                    );
                    continue;
                }
            };
            let Some(gsi) = f.lines.add_platform(irq, !edge) else {
                println!("{}: no line is left for {}'s GPIO pin {}", f.name, d.path, pin);
                continue;
            };
            f.pins.add_interrupt(uid, GpioInterrupt { pin, gsi, edge, active_low: polarity == 1 });
            println!("{}: {}'s GPIO pin {} interrupts on the guest's GSI {}", f.name, d.path, pin, gsi);
        }
        f.pins.add(uid, pin, Owner { function: f.function, device: index, index: at });
        given.push(pin);
    }
    if given.is_empty() {
        return None;
    }
    Some(Gpio { interrupt, pins: given, controller: path, pull, restriction, edge, polarity, shared, wake, debounce })
}
