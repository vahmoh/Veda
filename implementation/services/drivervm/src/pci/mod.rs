// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The PCI functions the guest gets. devmgr hands each over as a `pcidev`
//! channel, and each is given to the guest whole ([`give`]): its DMA
//! reaches the guest's memory and nothing else (the IOMMU), its memory BARs
//! are mapped into the guest's memory, its MSIs are bound to the guest's
//! processors, and the line its INTx is wired to is the guest's (`lines`;
//! functions on one line share it). Linux finds them on bus 0 of the
//! guest's PCI, their configuration space as `hv::pci` makes it, through
//! the platform's hypercalls. What the PC's firmware describes below a
//! function goes into the guest's ACPI tables ([`describe`]); the
//! firmware's tables that describe a function's hardware go there whole (an
//! Intel audio controller's NHLT: the links of its DSP, and the microphones
//! on them).
//!
//! A function reports no errors to the host, which a PC may make NMIs of:
//! its error reporting is turned off before the guest runs, and stays off.
//!
//! Memory the firmware keeps for a function (an RMRR: a GPU's stolen
//! memory, which it and the firmware's framebuffer use) the guest has where
//! the PC has it, in a free span of its address space: the function's DMA
//! reaches it there, as the firmware left it. A function the guest cannot
//! be given (its firmware's memory where the guest has its own, no room on
//! the bus or for its BARs, its DMA not the guest's) is given nothing of
//! the guest's, nor the guest anything of it: it is said so and let go of,
//! its channel closes, which tells devmgr it stays the host's.
//!
//! Where a function is, and its BARs: `drivervm_core::functions`.
//!
//! An Intel integrated GPU's driver reads more of the PC's firmware: its
//! OpRegion (the display's description), which the guest has a copy of
//! (`hv::igd`), and the PC's host bridge, which the guest has a stand-in
//! for at 00:00.0, read-only (`hv::pci::HostBridge`). Its stolen memory is
//! the memory the firmware keeps for it, where the PC has it.
//!
//! While the guest runs, its functions' configuration space goes through
//! here, and its MSIs: as many for each as its MSI or MSI-X capability
//! says it can send, [`crate::architecture::MAX_MSIS`] for all of them at
//! once, each made when the guest routes it and freed when it lets go of
//! it.
//!
//! The kernel's command line says where each function's memory BARs are on
//! the host ([`Devices::host_options`]): a driver in the guest names the
//! host's memory it drives so (a display, its firmware's framebuffer), to
//! services that know the host's addresses.

mod describe;
mod give;

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

pub use drivervm_core::functions::Windows;
use drivervm_core::functions::places;
use hv::acpi::{self, Function, GpioController};
use hv::pci::{ConfigSpace, HostBridge, Write};
use hv::platform::{error, gpio};
use proto::pci::{DeviceInfo, pcidev};
use rt::object::{Channel, Guest, Interrupt, Vcpu};
use rt::println;
use rt::sync::Mutex;

use crate::gpio::{Owner, Pins};
use crate::lines::Lines;
use describe::{Given, describe};
use give::{Placed, give, intel_gpu, is_intel_gpu};

/// A function the guest has.
struct Device {
    /// Where the guest has it on bus 0: device << 3 | function.
    devfn: u8,
    info: DeviceInfo,
    /// What the guest's ACPI tables say about it (its INTx among it).
    description: Function,
    /// How many MSIs it can send (their indices are below).
    msis: u32,
    /// Whether an MSI the guest asked for was refused (said once).
    refused: AtomicBool,
    state: Mutex<State>,
}

struct State {
    pci: pcidev::Client,
    config: ConfigSpace,
    /// Its MSIs, by index: the interrupt, and the message for it.
    msis: BTreeMap<u64, (Interrupt, u64)>,
}

/// The functions the guest has.
pub struct Devices {
    list: Vec<Device>,
    /// The GPIO pins their devices are wired to.
    pins: Pins,
    /// The firmware's tables that came with them (one of each).
    firmware_tables: Vec<Vec<u8>>,
    /// The memory the firmware keeps for them, where the guest has it.
    reserved: Vec<Range<u64>>,
    /// An Intel GPU's: the guest's copy of its OpRegion (for
    /// `crate::architecture::OPREGION_AT`), and the stand-in for the PC's
    /// host bridge.
    opregion: Option<Vec<u8>>,
    host_bridge: Option<HostBridge>,
    /// Their MSIs, together.
    msis: AtomicU32,
}

impl Devices {
    /// No functions.
    pub fn none() -> Devices {
        Devices {
            list: Vec::new(),
            pins: Pins::default(),
            firmware_tables: Vec::new(),
            reserved: Vec::new(),
            opregion: None,
            host_bridge: None,
            msis: AtomicU32::new(0),
        }
    }

    /// Gives the guest the functions of `channels` it can have: their DMA,
    /// the memory the firmware keeps for them, their BARs (in `windows`),
    /// their place on its bus 0, the lines their INTx are wired to (added
    /// to `lines`).
    pub fn attach(guest: &Guest, channels: Vec<Channel>, windows: &mut Windows, lines: &mut Lines) -> Devices {
        let mut given = Vec::new();
        for channel in channels {
            let pci = pcidev::Client::new(channel);
            match pci.info() {
                Ok(info) => given.push((pci, info)),
                Err(e) => println!("devmgr: {e}"),
            }
        }
        let places = places(&given.iter().map(|(_, i)| (i.bus, i.slot, i.function)).collect::<Vec<_>>());
        let mut devices = Devices::none();
        let mut intel_gpus = 0;
        for ((pci, info), place) in given.into_iter().zip(places) {
            let name = format!(
                "{:04x}:{:04x} at {:02x}:{:02x}.{}",
                info.vendor, info.device, info.bus, info.slot, info.function
            );
            let Some((devfn, multifunction)) = place else {
                println!("{name}: the guest's bus is full; it does not get it");
                continue;
            };
            let Placed { bars, pci_express, kept, msis } = match give(guest, &pci, &info, windows, &devices.reserved) {
                Ok(placed) => placed,
                Err(e) => {
                    println!("{name} cannot be given to the guest: {e}");
                    continue;
                }
            };
            // The line its INTx is wired to; other functions may have it
            // already.
            let intx = match pci.intx() {
                Ok(Ok((irq, line))) => (lines.has(line.gsi) || lines.add(line.gsi, irq, line.level)).then_some(line),
                _ => None,
            };
            let mut at: Vec<String> =
                bars.iter().map(|b| format!("BAR {} at {:#x} ({} KiB)", b.index, b.address, b.size / 1024)).collect();
            for k in &kept {
                at.push(format!("the memory the firmware keeps for it at {:#x}-{:#x}", k.start, k.end - 1));
            }
            for k in kept {
                if !devices.reserved.contains(&k) {
                    devices.reserved.push(k);
                }
            }
            if let Some(l) = intx {
                at.push(format!("INT{}# on GSI {}", (b'A' + l.pin.clamp(1, 4) - 1) as char, l.gsi));
            }
            if msis > 0 {
                at.push(format!("{msis} MSIs"));
            }
            println!("{} is the guest's 00:{:02x}.{}: {}", name, devfn >> 3, devfn & 7, at.join(", "));
            let function = devices.list.len();
            let mut given = Given { pci: &pci, name: &name, function, lines, pins: &mut devices.pins };
            let description = describe(&mut given, devfn, intx);
            for table in pci.acpi_tables().unwrap_or_default() {
                let signature = String::from_utf8_lossy(table.get(..4).unwrap_or_default()).into_owned();
                if !acpi::is_table(&table) {
                    println!("{name}: the firmware's {signature} is not a whole table: the guest does not get it");
                } else if !devices.firmware_tables.iter().any(|t| t[..4] == table[..4]) {
                    println!("{name} comes with the firmware's {signature} ({} bytes)", table.len());
                    devices.firmware_tables.push(table);
                }
            }
            let mut config = ConfigSpace::new(bars, multifunction, pci_express, intx.map(|l| (l.pin, l.gsi as u8)));
            // The first Intel GPU's OpRegion and host bridge are the guest's.
            if is_intel_gpu(&info) {
                if intel_gpus == 0 {
                    (devices.opregion, devices.host_bridge) = intel_gpu(&pci, &name);
                }
                let has_copy = intel_gpus == 0 && devices.opregion.is_some();
                config.intel_graphics(if has_copy { crate::architecture::OPREGION_AT as u32 } else { 0 });
                intel_gpus += 1;
            }
            let state = Mutex::new(State { pci, config, msis: BTreeMap::new() });
            devices.list.push(Device { devfn, info, description, msis, refused: AtomicBool::new(false), state });
        }
        devices
    }

    /// The kernel command line's options that say where the functions'
    /// memory BARs are on the host: ` veda.device=00:01.0,0:0x80000000,...`
    /// for each, its place in the guest then each BAR's index and host
    /// address.
    pub fn host_options(&self) -> String {
        let mut options = String::new();
        for d in &self.list {
            options.push_str(&format!(" veda.device=00:{:02x}.{}", d.devfn >> 3, d.devfn & 7));
            for b in d.info.bars.iter().filter(|b| !b.io) {
                options.push_str(&format!(",{}:{:#x}", b.index, b.address));
            }
        }
        options
    }

    /// The functions, as the ACPI tables describe them.
    pub fn functions(&self) -> Vec<Function> {
        self.list.iter().map(|d| d.description.clone()).collect()
    }

    /// The GPIO controllers of their devices' pins, as the ACPI tables
    /// describe them.
    pub fn gpio_controllers(&self) -> Vec<GpioController> {
        self.pins.controllers()
    }

    /// The firmware's tables that describe their hardware.
    pub fn firmware_tables(&self) -> &[Vec<u8>] {
        &self.firmware_tables
    }

    /// The memory the firmware keeps for them, where the guest has it.
    pub fn reserved(&self) -> &[Range<u64>] {
        &self.reserved
    }

    /// The guest's copy of an Intel GPU's OpRegion, for
    /// `crate::architecture::OPREGION_AT`.
    pub fn opregion(&self) -> Option<&[u8]> {
        self.opregion.as_deref()
    }

    /// Operation `op` ([`gpio`]) on pin `pin` of GPIO controller
    /// `controller`, with `value` for a level: through the channel of the
    /// function the pin's device is below.
    pub fn gpio(&self, controller: u64, pin: u64, op: u64, value: u64) -> u64 {
        let Some(Owner { function, device, index }) = self.pins.owner(controller, pin) else {
            return error::INVALID;
        };
        let Some(d) = self.list.get(function) else { return error::INVALID };
        let s = d.state.lock();
        let done = match op {
            gpio::READ => s.pci.gpio_read(device, index).map(|r| r.map(u64::from)),
            gpio::WRITE => s.pci.gpio_write(device, index, value != 0).map(|r| r.map(|_| 0)),
            gpio::INPUT => s.pci.gpio_input(device, index).map(|r| r.map(|_| 0)),
            gpio::DIRECTION => s.pci.gpio_is_output(device, index).map(|r| r.map(u64::from)),
            _ => return error::INVALID,
        };
        match done {
            Ok(Ok(v)) => v,
            _ => error::INVALID,
        }
    }

    fn device(&self, function: u64) -> Option<&Device> {
        (function >> 8 == 0).then(|| self.list.iter().find(|d| d.devfn as u64 == function))?
    }

    /// A read of the configuration space of guest function `function`.
    pub fn config_read(&self, function: u64, offset: u64, width: u64) -> u64 {
        let all_ones = u32::MAX >> (32 - 8 * width.clamp(1, 4) as u32);
        let (Ok(offset), Ok(width)) = (u16::try_from(offset), u8::try_from(width)) else {
            return all_ones as u64;
        };
        let Some(d) = self.device(function) else {
            return match &self.host_bridge {
                Some(b) if function == 0 => b.read(offset, width) as u64,
                _ => all_ones as u64,
            };
        };
        let s = d.state.lock();
        s.config.read(offset, width, |at| s.pci.config_read(at, 4).ok().and_then(|r| r.ok()).unwrap_or(u32::MAX)) as u64
    }

    /// A write to the configuration space of guest function `function`.
    pub fn config_write(&self, function: u64, offset: u64, width: u64, value: u64) -> u64 {
        let (Ok(offset), Ok(width)) = (u16::try_from(offset), u8::try_from(width)) else {
            return error::INVALID;
        };
        let Some(d) = self.device(function) else {
            // The host bridge's stand-in is read-only.
            return if function == 0 && self.host_bridge.is_some() { 0 } else { error::INVALID };
        };
        let mut s = d.state.lock();
        if let Write::Device { offset, width, value } = s.config.write(offset, width, value as u32)
            && !matches!(s.pci.config_write(offset, width, value), Ok(Ok(())))
        {
            println!("{:04x}:{:04x}: devmgr refused a write at {:#x}", d.info.vendor, d.info.device, offset);
        }
        0
    }

    /// Routes MSI `index` of guest function `function` to `vector` of
    /// `vcpu`: the message the function sends for it, made the first time
    /// (an index its capabilities have, within the guest's MSIs). Vector 0:
    /// the guest let go of it: it reaches nothing any more, and its vector
    /// is the host's again.
    pub fn msi(&self, function: u64, index: u64, vcpu: &Vcpu, vector: u64) -> u64 {
        let (Some(d), Ok(vector)) = (self.device(function), u8::try_from(vector)) else {
            return error::INVALID;
        };
        let mut s = d.state.lock();
        if vector == 0 {
            if let Some((irq, _)) = s.msis.remove(&index) {
                let _ = vcpu.bind_interrupt(&irq, 0);
                drop(irq);
                self.msis.fetch_sub(1, Ordering::Relaxed);
            }
            return 0;
        }
        if !s.msis.contains_key(&index) {
            let refuse = |why: &str| {
                if !d.refused.swap(true, Ordering::Relaxed) {
                    println!("{:04x}:{:04x}: no MSI {index} for the guest: {why}", d.info.vendor, d.info.device);
                }
                error::INVALID
            };
            if index >= d.msis as u64 {
                return refuse(&format!("the function sends {} MSIs", d.msis));
            }
            let taken = self.msis.try_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                (n < crate::architecture::MAX_MSIS).then_some(n + 1)
            });
            if taken.is_err() {
                return refuse(&format!("the guest has {} MSIs, the most it may", crate::architecture::MAX_MSIS));
            }
            match s.pci.alloc_msi() {
                Ok(Ok((irq, msg))) => {
                    let message = crate::architecture::msi_result(msg.address, msg.data);
                    s.msis.insert(index, (irq, message));
                }
                _ => {
                    self.msis.fetch_sub(1, Ordering::Relaxed);
                    return refuse("devmgr has no interrupt for it");
                }
            }
        }
        let Some((irq, message)) = s.msis.get(&index) else { return error::INVALID };
        match vcpu.bind_interrupt(irq, vector) {
            Ok(()) => *message,
            Err(e) => {
                println!("{:04x}:{:04x}: MSI {} cannot go to the guest: {}", d.info.vendor, d.info.device, index, e);
                error::INVALID
            }
        }
    }
}
