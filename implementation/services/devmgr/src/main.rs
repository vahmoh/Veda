// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `devmgr` — the device manager.
//!
//! Enumerates the PCI bus, matches devices against the driver table, starts
//! each driver with a channel that speaks the `pcidev` protocol for exactly
//! that device, and serves those channels. Drivers therefore never see other
//! devices' configuration space or memory.
//!
//! It also reads the firmware's ACPI tables: a driver learns the devices
//! they describe below its PCI function (and only those), and the tables
//! that describe its function's hardware (an Intel audio controller's
//! NHLT), and devmgr drives the GPIO pins those devices are wired to on the
//! driver's behalf.
//!
//! Every other device goes to the driver VM, whose Linux drives it: all
//! but the disks Veda starts from, the platform's own functions, and the
//! devices Veda still has drivers for (see [`veda_keeps`]); the PC's
//! keyboard controller too, and the firmware's variables, which Linux's
//! drivers may read (`init` hands them over). The driver VM
//! gets their `pcidev` channels, through which it also gets the resource
//! that lets it give them to its guest. It starts when the machine can
//! give it devices: its processors run virtual machines, an IOMMU confines
//! the devices to the guest's memory, and the system image has its Linux.
//! `drivervm.devices=VID:DID,...` gives it devices Veda has drivers for
//! (a sound card), `drivervm` starts it even without devices (tests), and
//! `drivervm=off` keeps it off. When it ends without Linux having powered
//! it off (Linux crashed, or the monitor did), devmgr resets its devices
//! and starts it again, waiting longer each time it fails soon after
//! starting (or does not start at all), and giving up after a few such
//! failures.
//!
//! At the system's start the window system and the audio service wait for
//! what the driver VM brings: the display the firmware's framebuffer is on
//! (the screen stays black until its driver shows its first picture), and
//! sound (the startup sound plays as the desktop appears). Only devmgr
//! knows whether those are coming, so it tells them ([`Coming`]), and
//! tells them again should the driver VM give up.

#![no_std]
#![no_main]

extern crate alloc;

mod acpi;
#[cfg(target_arch = "x86_64")]
#[path = "architectures/amd64/mod.rs"]
mod arch;
mod gpio;
mod pci;
mod placement;

use alloc::collections::BTreeMap;
use alloc::collections::btree_map::Entry;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::ops::Range;

use ::acpi::Memory;
use ::acpi::name::Path;
use ::acpi::resource::Resource as FirmwareResource;
use ::acpi::value::Value;
use abi::{cache_policy, irq_flags, signals};
use ipc::WaitSet;
use proto::audio::audiodev;
use proto::displaydev::displaydev;
use proto::pci::{
    AcpiDevice, AcpiResource, DeviceInfo, HostBridge, InterruptLine, IntxLine, MsiAddress, OpRegion, PciError,
    ReservedMemory, pcidev,
};
use proto::startup::{self, devmgr as roles, drivervm::exit_code};
use rt::object::{Channel, Interrupt, Process, Resource, Vmo};
use rt::println;

use acpi::Acpi;
use arch::config::ConfigAccess;
use arch::platform::Platform;
use gpio::Gpio;
use pci::{Address, ConfigSpace, Placement};

rt::entry!(main);

/// Drivers for whole device classes: (class, subclass, programming
/// interface, driver).
const CLASS_DRIVERS: &[(u8, u8, u8, &str)] = &[
    // NVM Express controllers (the SSDs of most PCs since about 2016)
    (0x01, 0x08, 0x02, "nvme"),
];

/// Disk drivers. A live system (started with `live`) starts none of them:
/// it runs from memory and never touches the computer's disks.
const DISK_DRIVERS: [&str; 1] = ["nvme"];

/// What the firmware's AML may spend in all as the devices are placed and
/// described at the start: steps, and bytes of values made or copied (each
/// evaluation has its own bound too, a million steps). Sixteen times what
/// loading a table may spend.
const STARTUP_AML: (u64, u64) = (16_000_000, 256 << 20);

/// The driver for a device, if any.
fn driver_for(info: &DeviceInfo) -> Option<&'static str> {
    CLASS_DRIVERS
        .iter()
        .find(|(c, s, p, _)| (*c, *s, *p) == (info.class, info.subclass, info.prog_if))
        .map(|(_, _, _, d)| *d)
}

/// Whether Veda keeps a function rather than give it to the driver VM: the
/// disks it starts from (storage controllers, whether it drives them yet
/// or not: they are none of Linux's business), the platform's own (memory
/// controllers, bridges, system peripherals, processors, encryption and
/// signal processing controllers, the SMBus, and the controller of the
/// firmware's flash: the serial bus controller in device 31 of an Intel
/// PCH), and the devices it has drivers of its own for. Sound (HD Audio
/// controllers, with an audio DSP beside them or not) and the other
/// serial bus controllers (a laptop's I2C controllers with its touchpad,
/// its SPI controller with its speaker amplifiers) are Linux's.
fn veda_keeps(info: &DeviceInfo) -> bool {
    let flash = info.class == 0x0C && info.subclass == 0x80 && info.bus == 0 && info.slot == 0x1F;
    matches!(info.class, 0x01 | 0x05 | 0x06 | 0x08 | 0x0B | 0x10 | 0x11)
        || (info.class == 0x0C && info.subclass == 0x05)
        || flash
        || driver_for(info).is_some()
}

fn class_name(info: &DeviceInfo) -> &'static str {
    match (info.class, info.subclass) {
        (0x01, 0x06) => "SATA controller",
        (0x01, 0x08) => "NVM Express controller",
        (0x01, _) => "storage controller",
        (0x02, _) => "network controller",
        (0x03, _) => "display controller",
        (0x04, _) => "multimedia device",
        (0x06, 0x00) => "host bridge",
        (0x06, 0x01) => "ISA bridge",
        (0x06, 0x04) => "PCI bridge",
        (0x06, _) => "bridge",
        (0x09, _) => "input device",
        (0x0C, 0x03) => "USB controller",
        (0x0C, 0x05) => "SMBus controller",
        _ => "device",
    }
}

/// A device that has a driver attached.
struct Bound {
    address: Address,
    info: DeviceInfo,
    channel: Channel,
    /// Where the firmware placed it.
    placement: Placement,
    /// Its ACPI companion, and the devices the firmware describes below
    /// it.
    companion: Option<Path>,
    acpi: Vec<::acpi::device::Described>,
    /// The driver VM has it.
    guest: bool,
    /// How many MSIs it can raise (`ConfigSpace::msi_vectors`): its driver
    /// has no more at once.
    msi_vectors: u32,
}

/// A function the driver VM gets, with what the firmware describes below
/// it.
struct GuestDevice {
    address: Address,
    info: DeviceInfo,
    placement: Placement,
    companion: Option<Path>,
    acpi: Vec<::acpi::device::Described>,
    /// How many MSIs it can raise.
    msi_vectors: u32,
}

struct Manager {
    /// Shared with the ACPI interpreter, which reads it for the firmware.
    config: Rc<ConfigSpace>,
    io: Resource,
    irq: Resource,
    mmio: Resource,
    dma: Resource,
    pci: Resource,
    acpi: Option<Acpi>,
    gpio: RefCell<Gpio>,
    /// The lines functions' INTx are wired to, by GSI: one interrupt each,
    /// which every function on the line shares.
    lines: RefCell<BTreeMap<u32, Interrupt>>,
}

/// `A` for INTA# (pin 1) to `D`.
fn intx_letter(pin: u8) -> char {
    (b'A' + pin.saturating_sub(1)) as char
}

/// A resource of the device at `scope`, as the `pcidev` protocol carries
/// it: the controllers of its connections resolved where they can be.
fn wire(r: &FirmwareResource, acpi: &Acpi, scope: &Path) -> AcpiResource {
    let controller = |name: &str| {
        let resolved = ::acpi::name::NameString::parse(name).and_then(|n| acpi.ns.resolve(scope, &n));
        resolved.map_or_else(|| String::from(name), |p| alloc::format!("{}", p))
    };
    match r {
        FirmwareResource::Memory { base, length, .. } => AcpiResource::Memory { base: *base, length: *length },
        FirmwareResource::Io { base, length } => AcpiResource::Io { base: *base, length: *length },
        FirmwareResource::Irq { irqs, edge, active_low, shared, .. } => {
            AcpiResource::Irq { irqs: irqs.clone(), edge: *edge, active_low: *active_low, shared: *shared }
        }
        FirmwareResource::Gpio(g) => AcpiResource::Gpio {
            interrupt: g.interrupt,
            pins: g.pins.clone(),
            controller: controller(&g.controller),
            pull: g.pull,
            restriction: g.restriction,
            shared: g.shared,
            edge: g.edge,
            polarity: g.polarity,
            wake: g.wake,
            debounce: g.debounce,
        },
        FirmwareResource::Spi(s) => AcpiResource::Spi {
            controller: controller(&s.controller),
            chip_select: s.chip_select,
            speed_hz: s.speed_hz,
            bits: s.bits,
            cpol: s.cpol,
            cpha: s.cpha,
            cs_active_high: s.cs_active_high,
        },
        FirmwareResource::I2c(i) => AcpiResource::I2c {
            controller: controller(&i.controller),
            address: i.address,
            speed_hz: i.speed_hz,
            ten_bit: i.ten_bit,
        },
        FirmwareResource::Window { .. } => AcpiResource::Other { kind: 0x87 },
        FirmwareResource::Other { kind } => AcpiResource::Other { kind: *kind },
    }
}

struct DeviceSession<'a> {
    mgr: &'a Manager,
    dev: &'a Bound,
    /// The key the session's channel is waited on with, which owns what
    /// it is given that goes when it does (its pins' interrupts).
    key: u64,
}

/// Whether the function is an Intel integrated GPU (a display controller of
/// Intel's on the root bus).
fn intel_gpu(info: &DeviceInfo) -> bool {
    info.vendor == 0x8086 && info.class == 0x03 && info.bus == 0
}

impl DeviceSession<'_> {
    /// That the function is an Intel integrated GPU the driver VM has.
    fn guests_intel_gpu(&self) -> Result<(), PciError> {
        if !self.dev.guest {
            Err(PciError::Denied)
        } else if !intel_gpu(&self.dev.info) {
            Err(PciError::Unsupported)
        } else {
            Ok(())
        }
    }

    /// The interrupt of line `gsi`: the one the lines' table has, or a
    /// new one, signalling as given.
    fn line(&self, gsi: u32, level: bool, active_low: bool) -> Result<Interrupt, PciError> {
        let mut lines = self.mgr.lines.borrow_mut();
        let irq = match lines.entry(gsi) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => {
                let flags =
                    if level { irq_flags::LEVEL } else { 0 } | if active_low { irq_flags::ACTIVE_LOW } else { 0 };
                e.insert(Interrupt::create(&self.mgr.irq, gsi as usize, flags).map_err(|_| PciError::NoResources)?)
            }
        };
        irq.0.duplicate(None).map(Interrupt).map_err(|_| PciError::NoResources)
    }
}

impl pcidev::Server for DeviceSession<'_> {
    fn info(&mut self) -> DeviceInfo {
        self.dev.info.clone()
    }

    fn config_read(&mut self, offset: u16, width: u8) -> Result<u32, PciError> {
        if offset >= 256 || !matches!(width, 1 | 2 | 4) || !offset.is_multiple_of(width as u16) {
            return Err(PciError::BadOffset);
        }
        Ok(self.mgr.config.read(self.dev.address, offset, width))
    }

    fn config_write(&mut self, offset: u16, width: u8, value: u32) -> Result<(), PciError> {
        // BARs (and the expansion ROM's) are owned by the firmware/devmgr;
        // drivers may not move them.
        if offset >= 256
            || !matches!(width, 1 | 2 | 4)
            || !offset.is_multiple_of(width as u16)
            || (0x10..0x28).contains(&offset)
            || (0x30..0x34).contains(&offset)
        {
            return Err(PciError::BadOffset);
        }
        let a = self.dev.address;
        if self.mgr.config.driver_write(a, &self.dev.placement, offset, width, value) {
            println!(
                "{:02x}:{:02x}.{} woke from D3hot or was reset, without its BARs: put back",
                a.bus, a.slot, a.function
            );
        }
        Ok(())
    }

    fn map_bar(&mut self, index: u8) -> Result<Vmo, PciError> {
        let bar = self.dev.info.bars.iter().find(|b| b.index == index && !b.io).ok_or(PciError::NoSuchBar)?;
        Vmo::create_physical(&self.mgr.mmio, bar.address, bar.size as usize, cache_policy::UNCACHED)
            .map_err(|_| PciError::Denied)
    }

    fn enable(&mut self, bus_master: bool) -> Result<(), PciError> {
        let cmd = self.mgr.config.read(self.dev.address, 0x04, 2);
        // I/O and memory decoding, and bus mastering if asked for.
        let new = cmd | 0b11 | if bus_master { 0b100 } else { 0 };
        self.mgr.config.write(self.dev.address, 0x04, 2, new);
        Ok(())
    }

    fn alloc_msi(&mut self) -> Result<(Interrupt, MsiAddress), PciError> {
        // No more at once than the function can raise (the kernel counts
        // those not yet freed): its driver (or a guest) has no use for
        // more, and the machine's interrupts are few.
        let (irq, info) = Interrupt::create_msi(&self.mgr.pci, self.dev.address.requester_id(), self.dev.msi_vectors)
            .map_err(|_| PciError::NoResources)?;
        Ok((irq, MsiAddress { address: info.address, data: info.data }))
    }

    fn dma_resource(&mut self) -> Result<Resource, PciError> {
        self.mgr.dma.duplicate().map_err(|_| PciError::Denied)
    }

    fn intx(&mut self) -> Result<(Interrupt, IntxLine), PciError> {
        let (a, pin) = (self.dev.address, self.dev.info.irq_pin);
        if !(1..=4).contains(&pin) {
            return Err(PciError::NotFound);
        }
        let route = self.mgr.acpi.as_ref().and_then(|acpi| acpi.pci_interrupt(a.bus, a.slot, pin));
        let Some(route) = route else {
            println!(
                "{:02x}:{:02x}.{}: the firmware does not say where INT{}# goes",
                a.bus,
                a.slot,
                a.function,
                intx_letter(pin)
            );
            return Err(PciError::NotFound);
        };
        let line = IntxLine { pin, gsi: route.gsi, level: route.level, active_low: route.active_low };
        let irq = self.line(route.gsi, route.level, route.active_low)?;
        println!(
            "{:02x}:{:02x}.{}: INT{}# on GSI {} ({}, active {})",
            a.bus,
            a.slot,
            a.function,
            intx_letter(pin),
            route.gsi,
            if route.level { "level" } else { "edge" },
            if route.active_low { "low" } else { "high" }
        );
        Ok((irq, line))
    }

    fn device_resource(&mut self) -> Result<Resource, PciError> {
        if !self.dev.guest {
            return Err(PciError::Denied);
        }
        let sid = self.dev.address.requester_id() as u64;
        self.mgr.pci.create(abi::resource_kind::PCI, sid, 1).map_err(|_| PciError::Denied)
    }

    fn reserved_memory(&mut self) -> Result<Vec<ReservedMemory>, PciError> {
        if !self.dev.guest {
            return Err(PciError::Denied);
        }
        let Some(acpi) = self.mgr.acpi.as_ref() else { return Ok(Vec::new()) };
        let a = self.dev.address;
        acpi.reserved_memory(a.bus, a.slot, a.function)
            .into_iter()
            .map(|r| {
                let size = r.end - r.start;
                let memory = Vmo::create_physical(&self.mgr.mmio, r.start, size as usize, cache_policy::UNCACHED)
                    .map_err(|_| PciError::NoResources)?;
                Ok(ReservedMemory { base: r.start, size, memory })
            })
            .collect()
    }

    fn opregion(&mut self) -> Result<OpRegion, PciError> {
        self.guests_intel_gpu()?;
        let acpi = self.mgr.acpi.as_ref().ok_or(PciError::NotFound)?;
        let at = self.mgr.config.read(self.dev.address, hv::pci::intel_graphics::ASLS, 4) as u64;
        let mut opregion = alloc::vec![0; hv::igd::OPREGION_SIZE];
        if at == 0 || !acpi.memory.read(at, &mut opregion) || !hv::igd::is_opregion(&opregion) {
            return Err(PciError::NotFound);
        }
        // Its raw VBT, if it keeps that outside, and if it can be read.
        let vbt = hv::igd::raw_vbt(&opregion, at)
            .filter(|&(_, size)| size <= hv::igd::OPREGION_ROOM)
            .and_then(|(vbt_at, size)| {
                let mut vbt = alloc::vec![0; size];
                acpi.memory.read(vbt_at, &mut vbt).then_some(vbt)
            })
            .unwrap_or_default();
        Ok(OpRegion { opregion, vbt })
    }

    fn host_bridge(&mut self) -> Result<HostBridge, PciError> {
        self.guests_intel_gpu()?;
        let bridge = Address { bus: 0, slot: 0, function: 0 };
        let dword = |offset| self.mgr.config.read(bridge, offset, 4);
        Ok(HostBridge {
            ids: dword(0x00),
            class: dword(0x08),
            subsystem: dword(0x2C),
            mchbar: dword(0x48) as u64 | (dword(0x4C) as u64) << 32,
            graphics_control: dword(hv::pci::intel_graphics::GGC),
        })
    }

    fn acpi_tables(&mut self) -> Vec<Vec<u8>> {
        self.mgr.acpi.as_ref().map(|a| a.device_tables(&self.dev.info)).unwrap_or_default()
    }

    fn acpi_devices(&mut self) -> Vec<AcpiDevice> {
        let Some(acpi) = self.mgr.acpi.as_ref() else { return Vec::new() };
        self.dev
            .acpi
            .iter()
            .map(|d| AcpiDevice {
                path: alloc::format!("{}", d.path),
                hid: d.identity.hid.clone().unwrap_or_default(),
                cids: d.identity.cids.clone(),
                uid: d.identity.uid.clone().unwrap_or_default(),
                sub: d.identity.sub.clone().unwrap_or_default(),
                status: d.identity.status as u32,
                resources: d
                    .resources
                    .as_ref()
                    .map(|r| r.iter().map(|r| wire(r, acpi, &d.path)).collect())
                    .unwrap_or_default(),
                resource_error: d.resources.as_ref().err().map(|e| alloc::format!("{}", e)).unwrap_or_default(),
            })
            .collect()
    }

    fn acpi_dsm(&mut self, device: u32, uuid: Vec<u8>, revision: u64, function: u64) -> Result<u64, PciError> {
        let acpi = self.mgr.acpi.as_ref().ok_or(PciError::NotFound)?;
        let d = self.dev.acpi.get(device as usize).ok_or(PciError::NotFound)?;
        let dsm = d.path.join("_DSM").ok_or(PciError::NotFound)?;
        let args =
            [Value::Buffer(uuid), Value::Integer(revision), Value::Integer(function), Value::Package(Vec::new())];
        let answer = acpi.ns.evaluate(&dsm, &args, &acpi.memory).map_err(|_| PciError::NotFound)?;
        match answer {
            Value::Integer(v) => Ok(v),
            _ => Err(PciError::NotFound),
        }
    }

    fn acpi_data(&mut self, device: u32, name: String) -> Result<Vec<u8>, PciError> {
        let acpi = self.mgr.acpi.as_ref().ok_or(PciError::NotFound)?;
        let owner = match device {
            u32::MAX => self.dev.companion.as_ref(),
            d => self.dev.acpi.get(d as usize).map(|d| &d.path),
        };
        let owner = owner.ok_or(PciError::NotFound)?;
        let value = acpi.ns.evaluate_child(owner, &name, &acpi.memory).ok_or(PciError::NotFound)?;
        let value = value.map_err(|_| PciError::Unsupported)?;
        ::acpi::asm::constant(&value).ok_or(PciError::Unsupported)
    }

    fn acpi_interrupt(&mut self, device: u32, index: u32) -> Result<(Interrupt, InterruptLine), PciError> {
        let d = self.dev.acpi.get(device as usize).ok_or(PciError::NotFound)?;
        let line = d
            .resources
            .as_ref()
            .ok()
            .and_then(|r| {
                r.iter()
                    .flat_map(|r| match r {
                        FirmwareResource::Irq { irqs, edge, active_low, .. } => irqs
                            .iter()
                            .map(|&gsi| InterruptLine { gsi, level: !*edge, active_low: *active_low })
                            .collect(),
                        _ => Vec::new(),
                    })
                    .nth(index as usize)
            })
            .ok_or(PciError::NotFound)?;
        println!(
            "{}: interrupt {} on GSI {} ({}, active {})",
            d.path,
            index,
            line.gsi,
            if line.level { "level" } else { "edge" },
            if line.active_low { "low" } else { "high" }
        );
        Ok((self.line(line.gsi, line.level, line.active_low)?, line))
    }

    fn gpio_read(&mut self, device: u32, index: u32) -> Result<bool, PciError> {
        let (acpi, controller, pin, _) = self.gpio_pin(device, index)?;
        self.mgr.gpio.borrow_mut().read(acpi, &controller, pin)
    }

    fn gpio_write(&mut self, device: u32, index: u32, high: bool) -> Result<(), PciError> {
        let (acpi, controller, pin, _) = self.gpio_pin(device, index)?;
        self.mgr.gpio.borrow_mut().write(acpi, &controller, pin, high)
    }

    fn gpio_input(&mut self, device: u32, index: u32) -> Result<(), PciError> {
        let (acpi, controller, pin, _) = self.gpio_pin(device, index)?;
        self.mgr.gpio.borrow_mut().input(acpi, &controller, pin)
    }

    fn gpio_is_output(&mut self, device: u32, index: u32) -> Result<bool, PciError> {
        let (acpi, controller, pin, _) = self.gpio_pin(device, index)?;
        self.mgr.gpio.borrow_mut().is_output(acpi, &controller, pin)
    }

    fn gpio_interrupt(&mut self, device: u32, index: u32) -> Result<Interrupt, PciError> {
        let (acpi, controller, pin, connection) = self.gpio_pin(device, index)?;
        if !connection.interrupt {
            return Err(PciError::NotFound);
        }
        let trigger = gpio::trigger(&connection);
        let line = |gsi, level, active_low| self.line(gsi, level, active_low);
        self.mgr.gpio.borrow_mut().interrupt(acpi, &controller, pin, trigger, self.key, line)
    }
}

impl DeviceSession<'_> {
    /// GPIO pin `index` of ACPI device `device` below this driver's PCI
    /// function: its controller, the pin, and the connection it is of.
    fn gpio_pin(&self, device: u32, index: u32) -> Result<(&Acpi, Path, u16, ::acpi::resource::Gpio), PciError> {
        let acpi = self.mgr.acpi.as_ref().ok_or(PciError::NotFound)?;
        let d = self.dev.acpi.get(device as usize).ok_or(PciError::NotFound)?;
        let (controller, pin, connection) = d.gpio(&acpi.ns, index as usize).ok_or(PciError::NotFound)?;
        Ok((acpi, controller, pin, connection.clone()))
    }
}

/// Starts `driver` for a device, returning the server end of its channel.
/// Driver images come straight from the system image (not through the file
/// system, which may itself be waiting for a disk driver).
fn start_driver(boot: &initrd::Archive<'static>, image: &Vmo, driver: &str, info: &DeviceInfo) -> Option<Channel> {
    let (ours, theirs) = Channel::create().ok()?;
    start_program(boot, image, driver, &[], alloc::vec![(startup::driver::PCIDEV, theirs.into_handle())])?;
    println!(
        "started {} for {:04x}:{:04x} at {:02x}:{:02x}.{}",
        driver, info.vendor, info.device, info.bus, info.slot, info.function
    );
    Some(ours)
}

/// Starts `bin/NAME` from the system image (`boot`, in `image`) with the
/// registry and `handles` (by role): the process, if it started.
fn start_program(
    boot: &initrd::Archive<'static>,
    image: &Vmo,
    name: &str,
    args: &[String],
    handles: Vec<(u32, rt::object::Handle)>,
) -> Option<Process> {
    let path = alloc::format!("bin/{name}");
    let Some(file) = boot.find(&path) else {
        println!("{} is not installed", name);
        return None;
    };
    let registry = proto::with_registry(|r| r.clone_registry()).ok()?.ok()?.ok()?;
    let file_path = alloc::format!("/system/{path}");
    let mut spawn =
        rt::process::Spawn::new(name).path(&file_path).handle(abi::startup::role::REGISTRY, registry.into_handle());
    for (role, h) in handles {
        spawn = spawn.handle(role, h);
    }
    for a in args {
        spawn = spawn.arg(a);
    }
    // It runs from the image's pages, which every process running it
    // shares (from a copy of them if they cannot be had).
    let started = match image.slice(file.offset as usize, file.data.len()) {
        Ok(pages) => spawn.start_file(&pages, file.data.len()),
        Err(_) => spawn.start(file.data),
    };
    match started {
        Ok(process) => Some(process),
        Err(e) => {
            println!("failed to start {}: {}", name, e);
            None
        }
    }
}

/// The boot information `init` hands over (where the ACPI tables are).
fn boot_info() -> Option<abi::KernelBootInfo> {
    let vmo = Vmo::from_handle(rt::env::take_handle(abi::startup::role::BOOT_INFO)?);
    let mut buf = [0u8; core::mem::size_of::<abi::KernelBootInfo>()];
    vmo.read(0, &mut buf).ok()?;
    // SAFETY: KernelBootInfo is plain old data written by the kernel.
    Some(unsafe { core::ptr::read_unaligned(buf.as_ptr() as *const abi::KernelBootInfo) })
}

/// The devices the firmware describes below PCI function `a`; logs them.
fn described_below(acpi: Option<&Acpi>, a: Address) -> (Option<Path>, Vec<::acpi::device::Described>) {
    let Some(acpi) = acpi else { return (None, Vec::new()) };
    let Some(companion) = acpi.pci_companion(a.bus, a.slot, a.function) else { return (None, Vec::new()) };
    let below = acpi.children(&companion);
    if !below.is_empty() {
        let names: Vec<String> = below
            .iter()
            .map(|d| alloc::format!("{} ({})", d.path, d.identity.hid.as_deref().unwrap_or("no id")))
            .collect();
        println!("acpi: {:02x}:{:02x}.{} is {}, with {}", a.bus, a.slot, a.function, companion, names.join(", "));
    }
    (Some(companion), below)
}

/// Maps the system image (the initrd) that `init` hands over; also
/// returns it, for the driver VM.
fn boot_image() -> Option<(initrd::Archive<'static>, Vmo)> {
    let vmo = rt::object::Vmo::from_handle(rt::env::take_handle(abi::startup::role::INITRD)?);
    let size = vmo.size().ok()?;
    let addr = vmo.map(0, size, abi::map_flags::READ).ok()?;
    // SAFETY: a read-only mapping that lives as long as the process.
    let bytes: &'static [u8] = unsafe { core::slice::from_raw_parts(addr as *const u8, size) };
    Some((initrd::Archive::open(bytes).ok()?, vmo))
}

/// A run of the driver VM that ends sooner than this after it started is
/// a failure; the next start waits longer after each in a row, and stops
/// after a few.
const DRIVERVM_STABLE_NS: u64 = 60_000_000_000;
const DRIVERVM_MAX_FAILURES: u32 = 5;

/// How long telling the window system or the audio service what is coming
/// may take: they answer at once, but may not have started yet (what they
/// are told reaches them when they do).
const TELL_NS: u64 = 200_000_000;

/// What the window system and the audio service wait for at the system's
/// start: the driver of the display the firmware's framebuffer is on (the
/// display controller whose memory holds it), and of a sound card. Both
/// are the driver VM's (Veda has drivers for neither), each named by its
/// device for the log.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Coming {
    screen: Option<String>,
    sound: Option<String>,
}

impl Coming {
    /// What `devices`, the driver VM's, bring, the firmware's framebuffer
    /// at `framebuffer` (0: none).
    fn of(devices: &[GuestDevice], framebuffer: u64) -> Coming {
        let name = |d: &GuestDevice| {
            alloc::format!(
                "{:04x}:{:04x} at {:02x}:{:02x}.{}",
                d.info.vendor,
                d.info.device,
                d.address.bus,
                d.address.slot,
                d.address.function
            )
        };
        let holds_framebuffer = |d: &GuestDevice| {
            framebuffer != 0
                && d.info.bars.iter().any(|b| !b.io && framebuffer >= b.address && framebuffer - b.address < b.size)
        };
        // Audio devices and HD Audio controllers, with a DSP or not.
        let sound = |d: &GuestDevice| d.info.class == 0x04 && matches!(d.info.subclass, 0x01 | 0x03);
        Coming {
            screen: devices.iter().find(|d| d.info.class == 0x03 && holds_framebuffer(d)).map(name),
            sound: devices.iter().find(|d| sound(d)).map(name),
        }
    }
}

/// The window system's and the audio service's connections, on which
/// devmgr said what is coming: it holds while they are open. A service
/// that ends (init starts it again) closes its connection; devmgr then
/// connects again and says it once more, as a service started again waits
/// for word of what is coming as long as it does at the system's start.
struct Told {
    display: Option<displaydev::Client>,
    audio: Option<audiodev::Client>,
    /// What each connection was told (`None`: nothing yet).
    display_said: Option<Option<String>>,
    audio_said: Option<Option<String>>,
    /// When to connect again to a service whose connection closed, and
    /// how long the wait after that is: a connection the registry drops
    /// at once (the service gone for good) is not made again and again.
    again_at: Option<u64>,
    again_ns: u64,
    /// When devmgr last connected: a connection that lasted starts the
    /// waits afresh.
    since: u64,
}

/// How long devmgr waits before connecting again to a service whose
/// connection closed, at first and at most.
const TELL_AGAIN_NS: (u64, u64) = (100_000_000, 10_000_000_000);

impl Told {
    fn new() -> Told {
        let mut told = Told {
            display: None,
            audio: None,
            display_said: None,
            audio_said: None,
            again_at: None,
            again_ns: TELL_AGAIN_NS.0,
            since: 0,
        };
        told.connect();
        told
    }

    /// Connects to whichever service it has no connection to (one that has
    /// not registered yet gets it when it does).
    fn connect(&mut self) {
        self.since = rt::time::now_ns();
        if self.display.is_none()
            && let Ok(ch) = proto::connect(displaydev::NAME)
        {
            let c = displaydev::Client::new(ch);
            c.set_timeout(TELL_NS);
            self.display = Some(c);
            self.display_said = None;
        }
        if self.audio.is_none()
            && let Ok(ch) = proto::connect(audiodev::NAME)
        {
            let c = audiodev::Client::new(ch);
            c.set_timeout(TELL_NS);
            self.audio = Some(c);
            self.audio_said = None;
        }
        self.again_at = (self.display.is_none() || self.audio.is_none()).then(|| self.later());
    }

    /// When to try again, the wait growing each time.
    fn later(&mut self) -> u64 {
        let at = rt::time::now_ns().saturating_add(self.again_ns);
        self.again_ns = (self.again_ns * 2).min(TELL_AGAIN_NS.1);
        at
    }

    /// Says what is coming, to each connection that has not heard it.
    fn say(&mut self, coming: &Coming) {
        let device = |d: &Option<String>| String::from(d.as_deref().unwrap_or("none"));
        if let Some(display) = &self.display
            && self.display_said.as_ref() != Some(&coming.screen)
        {
            println!("coming: the display's driver: {}", device(&coming.screen));
            if let Err(e) = display.expect(coming.screen.clone()) {
                println!("the window system has not heard what is coming yet ({}); it will", e);
            }
            self.display_said = Some(coming.screen.clone());
        }
        if let Some(audio) = &self.audio
            && self.audio_said.as_ref() != Some(&coming.sound)
        {
            println!("coming: sound: {}", device(&coming.sound));
            if let Err(e) = audio.expect(coming.sound.clone()) {
                println!("the audio service has not heard what is coming yet ({}); it will", e);
            }
            self.audio_said = Some(coming.sound.clone());
        }
    }

    /// The connections, with the keys devmgr waits on them by.
    fn channels(&self) -> impl Iterator<Item = (abi::RawHandle, u64)> + '_ {
        let display = self.display.as_ref().map(|c| (c.channel().raw(), TOLD_DISPLAY));
        let audio = self.audio.as_ref().map(|c| (c.channel().raw(), TOLD_AUDIO));
        display.into_iter().chain(audio)
    }

    /// The connection waited on by `key` closed: its service ended.
    fn closed(&mut self, key: u64) {
        let name = if key == TOLD_DISPLAY {
            self.display = None;
            "the window system"
        } else {
            self.audio = None;
            "the audio service"
        };
        println!("{} went away: devmgr tells it again what is coming when it is back", name);
        if rt::time::now_ns().saturating_sub(self.since) >= 6 * TELL_AGAIN_NS.1 {
            self.again_ns = TELL_AGAIN_NS.0;
        }
        if self.again_at.is_none() {
            self.again_at = Some(self.later());
        }
    }
}

/// The keys devmgr waits on its connections to the window system and the
/// audio service by.
const TOLD_DISPLAY: u64 = u64::MAX - 1;
const TOLD_AUDIO: u64 = u64::MAX - 2;

/// The driver VM's memory when it gets an integrated GPU (MiB), whose
/// memory is the VM's own: a game's pictures take hundreds of MiB there
/// (Interceptor's 235 at 1280x720 and 341 at 1920x1080, on iris), beside
/// Linux's 170 and the desktop's, where the VM's own default (256) leaves
/// under 100 for them all.
const GPU_VM_MIB: u64 = 1024;
/// The driver VM's own default (MiB).
const VM_MIB: u64 = 256;

/// The memory the driver VM is to have for `guest`'s devices (MiB), where
/// they need more than its default: an integrated GPU's. Not up to the
/// memory the firmware keeps for them, which the guest has where the PC
/// has it (above the low memory the guest keeps as the PC does,
/// `arch::GUEST_LOW_RESERVED`).
fn vm_memory(guest: &[GuestDevice], acpi: Option<&Acpi>) -> Option<u64> {
    if !guest.iter().any(|d| intel_gpu(&d.info)) {
        return None;
    }
    let kept = guest
        .iter()
        .flat_map(|d| acpi.map(|a| a.reserved_memory(d.address.bus, d.address.slot, d.address.function)))
        .flatten()
        .filter(|r| r.start >= arch::GUEST_LOW_RESERVED)
        .map(|r| r.start >> 20)
        .min();
    Some(kept.unwrap_or(GPU_VM_MIB).clamp(VM_MIB, GPU_VM_MIB))
}

/// The driver VM: the devices it gets, and its process, which is started
/// again when it ends without Linux having powered the machine off.
struct DriverVm {
    /// Its options: `drivervm.NAME=VALUE` (`drivervm.memory=512`) but
    /// `devices`, which is devmgr's.
    args: Vec<String>,
    devices: Vec<GuestDevice>,
    /// The PC's own devices it gets (`arch::platform`).
    platform: Platform,
    /// The firmware's variables, for Linux's drivers that read them.
    variables: Option<Vmo>,
    hypervisor: Resource,
    process: Option<Process>,
    /// How many times it started, when it last did, the failures in a row
    /// (runs that ended soon), and when to start it next.
    starts: u32,
    started_ns: u64,
    failures: u32,
    next_start_ns: Option<u64>,
}

impl DriverVm {
    /// The driver VM of `devices`, with `options` (and `memory` MiB, where
    /// its devices need more than its default and the options say nothing).
    fn new(
        options: &[String],
        memory: Option<u64>,
        devices: Vec<GuestDevice>,
        platform: Platform,
        variables: Option<Vmo>,
        hypervisor: Resource,
    ) -> DriverVm {
        let mut args: Vec<String> = options
            .iter()
            .filter_map(|a| a.strip_prefix("drivervm."))
            .filter(|a| !a.starts_with("devices="))
            .map(Into::into)
            .collect();
        if let Some(mib) = memory
            && !args.iter().any(|a| a.starts_with("memory="))
        {
            println!("the driver VM gets {mib} MiB: its integrated GPU's memory is the VM's");
            args.push(alloc::format!("memory={mib}"));
        }
        DriverVm {
            args,
            devices,
            platform,
            variables,
            hypervisor,
            process: None,
            starts: 0,
            started_ns: 0,
            failures: 0,
            next_start_ns: Some(0),
        }
    }

    /// Starts it, with what it runs on: the hypervisor resource, the system
    /// image, which holds its Linux, new channels of its devices (which
    /// devmgr serves as `bound`'s), the PC's own devices, and the
    /// firmware's variables. A start that fails is a run that failed.
    fn start(
        &mut self,
        boot: &initrd::Archive<'static>,
        image: &Vmo,
        bound: &mut BTreeMap<u64, Bound>,
        next: &mut u64,
    ) {
        self.next_start_ns = None;
        let (Ok(h), Ok(i)) = (self.hypervisor.duplicate(), image.0.duplicate(None)) else {
            println!("cannot start drivervm: no handles for it");
            self.failures += 1;
            self.again();
            return;
        };
        let mut handles =
            alloc::vec![(startup::drivervm::HYPERVISOR_RESOURCE, h.into_handle()), (abi::startup::role::INITRD, i)];
        handles.extend(self.platform.handles());
        if let Some(variables) = self.variables.as_ref().and_then(|v| v.0.duplicate(None).ok()) {
            handles.push((startup::drivervm::VARIABLES, variables));
        }
        for d in &self.devices {
            let Ok((ours, theirs)) = Channel::create() else { continue };
            handles.push((startup::drivervm::DEVICE, theirs.into_handle()));
            let device = Bound {
                address: d.address,
                info: d.info.clone(),
                placement: d.placement,
                channel: ours,
                companion: d.companion.clone(),
                acpi: d.acpi.clone(),
                guest: true,
                msi_vectors: d.msi_vectors,
            };
            bound.insert(*next, device);
            *next += 1;
        }
        // A crash the tests ask for (`drivervm.crash=SECONDS`) is the first
        // run's.
        let args: Vec<String> =
            self.args.iter().filter(|a| self.starts == 0 || !a.starts_with("crash=")).cloned().collect();
        self.process = start_program(boot, image, "drivervm", &args, handles);
        if self.process.is_none() {
            // Its devices' channels went with the handles it did not get.
            self.failures += 1;
            self.again();
            return;
        }
        self.starts += 1;
        self.started_ns = rt::time::now_ns();
        let mut what = String::from(if self.starts > 1 { "started drivervm again" } else { "started drivervm" });
        for a in &args {
            what.push(' ');
            what.push_str(a);
        }
        match self.devices.len() {
            0 => println!("{}", what),
            n => println!("{} with {} device(s)", what, n),
        }
    }

    /// Has it start again later, the later the more of its runs in a row
    /// failed, or not at all once too many have.
    fn again(&mut self) {
        if self.failures > DRIVERVM_MAX_FAILURES {
            println!("the driver VM keeps failing: its devices stay off");
            return;
        }
        // At once the first time, then 1, 2, 4... seconds.
        let wait_ns = match self.failures {
            0 | 1 => 0,
            n => 1_000_000_000u64 << (n - 2).min(5),
        };
        self.next_start_ns = Some(rt::time::now_ns() + wait_ns);
    }

    /// What it brings, while it runs or is to run again (nothing, once it
    /// is off for good).
    fn coming(&self, framebuffer: u64) -> Coming {
        if self.process.is_none() && self.next_start_ns.is_none() {
            return Coming::default();
        }
        Coming::of(&self.devices, framebuffer)
    }

    /// Whether its monitor runs: the kernel says a process has ended
    /// before it closes its handles, the channels of its devices among
    /// them.
    fn running(&self) -> bool {
        self.process.as_ref().and_then(|p| p.info().ok()).is_some_and(|i| i.state == abi::process_state::RUNNING)
    }

    /// Its monitor did not take the device at `address` (and said why): it
    /// stays the host's, neither reset when the driver VM ends nor given to
    /// the next.
    fn turned_down(&mut self, address: Address) {
        self.devices.retain(|d| d.address != address);
    }

    /// Its process ended: unless Linux powered it off, its devices are
    /// reset and it starts again (later, the sooner it failed).
    fn ended(&mut self, config: &ConfigSpace) {
        let Some(process) = self.process.take() else { return };
        let info = process.info().ok();
        let why = match info.map(|i| (i.state, i.exit_code)) {
            Some((abi::process_state::EXITED, code)) if code == exit_code::POWER_OFF as i64 => {
                println!("Linux powered the driver VM off");
                return;
            }
            Some((abi::process_state::EXITED, code)) if code == exit_code::RESTART as i64 => {
                String::from("Linux restarted it")
            }
            Some((abi::process_state::EXITED, code)) if code == exit_code::CRASHED as i64 => {
                String::from("Linux crashed")
            }
            Some((abi::process_state::EXITED, code)) => alloc::format!("the monitor stopped it ({code})"),
            Some((abi::process_state::KILLED, _)) => String::from("the monitor was killed"),
            Some((abi::process_state::OUT_OF_MEMORY, _)) => String::from("memory ran out"),
            _ => String::from("the monitor crashed"),
        };
        let ran_ns = rt::time::now_ns().saturating_sub(self.started_ns);
        self.failures = if ran_ns < DRIVERVM_STABLE_NS { self.failures + 1 } else { 0 };
        let mut resets = Vec::new();
        for d in &self.devices {
            let how = config.reset(d.address).unwrap_or("no reset");
            resets.push(alloc::format!("{:04x}:{:04x} {}", d.info.vendor, d.info.device, how));
        }
        if resets.is_empty() {
            println!("the driver VM ended: {}", why);
        } else {
            println!("the driver VM ended: {}; its devices: {}", why, resets.join(", "));
        }
        self.again();
    }
}

/// The devices an option lists (`option` as `drivervm.devices=`, then
/// `VID:DID,...`).
fn listed_devices(options: &[String], option: &str) -> Vec<(u16, u16)> {
    let id = |s: &str| u16::from_str_radix(s, 16).ok();
    options
        .iter()
        .filter_map(|a| a.strip_prefix(option))
        .flat_map(|list| list.split(','))
        .filter_map(|d| d.split_once(':').and_then(|(v, d)| Some((id(v)?, id(d)?))))
        .collect()
}

fn main() -> i32 {
    let take = |role| rt::env::take_handle(role).map(Resource::from_handle);
    let (Some(io), Some(irq), Some(mmio), Some(dma), Some(pci)) = (
        take(roles::IOPORT_RESOURCE),
        take(roles::IRQ_RESOURCE),
        take(roles::MMIO_RESOURCE),
        take(roles::DMA_RESOURCE),
        take(roles::PCI_RESOURCE),
    ) else {
        println!("missing hardware resources");
        return 1;
    };
    let Ok(access) = ConfigAccess::open(&io) else {
        println!("cannot access PCI configuration space");
        return 1;
    };
    let Some((boot, image)) = boot_image() else {
        println!("no system image to load drivers from");
        return 1;
    };
    let config = Rc::new(ConfigSpace::new(access));
    let machine = boot_info();
    let acpi = machine.as_ref().and_then(|m| Acpi::load(&mmio, m, config.clone()));
    let platform = machine.map_or(0, |m| m.platform);
    let framebuffer = machine.map_or(0, |m| m.framebuffer_phys);
    let Ok(gpio_mmio) = mmio.duplicate() else {
        println!("cannot keep the MMIO resource for GPIO");
        return 1;
    };
    let mgr = Manager {
        config,
        io,
        irq,
        mmio,
        dma,
        pci,
        acpi,
        gpio: RefCell::new(Gpio::new(gpio_mmio)),
        lines: RefCell::new(BTreeMap::new()),
    };
    let args = rt::env::args();
    let live = args.iter().any(|a| a == "live");
    // The driver VM's options: `drivervm`, `drivervm=off`, and its own as
    // `drivervm.NAME=VALUE`.
    let drivervm: Vec<String> = args
        .iter()
        .filter(|a| *a == "drivervm" || a.starts_with("drivervm.") || a.starts_with("drivervm="))
        .cloned()
        .collect();
    let linux = boot.find("linux/kernel").is_some() && boot.find("linux/initramfs.cpio").is_some();
    let runs = if drivervm.iter().any(|a| a == "drivervm=off") {
        Err("drivervm=off")
    } else if platform & abi::platform::VIRTUALIZATION == 0 {
        Err("the processors cannot run virtual machines")
    } else if !linux {
        Err("the system image has no Linux for it")
    } else {
        Ok(())
    };
    // Devices go to it only where an IOMMU confines them to its memory.
    let gives = runs.is_ok() && platform & abi::platform::IOMMU != 0;
    match runs {
        Err(why) => println!("no driver VM ({}): the devices Veda has no driver for stay off", why),
        Ok(()) if !gives => println!("no IOMMU: no device can go to the driver VM"),
        Ok(()) => {}
    }
    let listed = listed_devices(&drivervm, "drivervm.devices=");
    let mut guest = Vec::new();

    let functions = mgr.config.scan();
    // Tests: these functions' BARs are taken as a firmware that leaves them
    // unplaced does (`devmgr.unplaced=VID:DID,...`).
    let unplaced = listed_devices(&args, "devmgr.unplaced=");
    for &a in &functions {
        let info = mgr.config.info(a);
        if unplaced.contains(&(info.vendor, info.device)) {
            mgr.config.forget_placement(a);
        }
    }
    // What the firmware's AML answers as devices are placed and described
    // here, all of it, is bounded: a firmware whose objects each take long
    // cannot make all of them take long (every evaluation is bounded on
    // its own too).
    if let Some(acpi) = &mgr.acpi {
        acpi.ns.limit(Some(STARTUP_AML));
    }
    placement::place(&mgr.config, &functions, mgr.acpi.as_ref());
    // What each function is, and the memory each one's BARs decode, as
    // placed: a driver gets a BAR only on pages no other function's is on.
    let infos: Vec<(Address, DeviceInfo)> = functions.iter().map(|&a| (a, mgr.config.info(a))).collect();
    let decoded: Vec<(Address, Range<u64>)> = infos
        .iter()
        .flat_map(|(a, info)| {
            info.bars.iter().filter(|b| !b.io).map(|b| (*a, b.address..b.address.saturating_add(b.size)))
        })
        .collect();

    let mut bound: BTreeMap<u64, Bound> = BTreeMap::new();
    let mut next = 1u64;
    for (a, info) in infos {
        let info = pci::own_pages(a, info, &decoded);
        let companion = mgr.acpi.as_ref().and_then(|acpi| acpi.pci_companion(a.bus, a.slot, a.function));
        println!(
            "pci {:02x}:{:02x}.{} {:04x}:{:04x} {}{}",
            a.bus,
            a.slot,
            a.function,
            info.vendor,
            info.device,
            class_name(&info),
            companion.map(|c| alloc::format!(" ({})", c)).unwrap_or_default()
        );
        if gives && (!veda_keeps(&info) || listed.contains(&(info.vendor, info.device))) {
            // An endpoint only: a bridge would give the guest what is
            // behind it. (Its I/O BARs the guest does not get.)
            if mgr.config.read(a, 0x0E, 1) & 0x7F != 0 {
                println!("{:04x}:{:04x} cannot go to the driver VM: not an endpoint", info.vendor, info.device);
                continue;
            }
            println!(
                "{:04x}:{:04x} at {:02x}:{:02x}.{} goes to the driver VM",
                info.vendor, info.device, a.bus, a.slot, a.function
            );
            if let Some(state) = mgr.config.power_state(a).filter(|&s| s != 0) {
                println!("{:02x}:{:02x}.{} is in D{}: the guest wakes it", a.bus, a.slot, a.function, state);
            }
            let (companion, acpi) = described_below(mgr.acpi.as_ref(), a);
            let placement = mgr.config.placement(a);
            let msi_vectors = mgr.config.msi_vectors(a);
            guest.push(GuestDevice { address: a, info, placement, companion, acpi, msi_vectors });
            continue;
        }
        let Some(driver) = driver_for(&info) else { continue };
        if live && DISK_DRIVERS.contains(&driver) {
            println!(
                "live system: {} not started for {:04x}:{:04x} at {:02x}:{:02x}.{}",
                driver, info.vendor, info.device, a.bus, a.slot, a.function
            );
            continue;
        }
        let (companion, below) = described_below(mgr.acpi.as_ref(), a);
        if let Some(ch) = start_driver(&boot, &image, driver, &info) {
            let placement = mgr.config.placement(a);
            let msi_vectors = mgr.config.msi_vectors(a);
            let device =
                Bound { address: a, info, placement, channel: ch, companion, acpi: below, guest: false, msi_vectors };
            bound.insert(next, device);
            next += 1;
        }
    }

    if let Some(acpi) = &mgr.acpi {
        if acpi.ns.left().is_some_and(|(steps, bytes)| steps == 0 || bytes == 0) {
            println!("acpi: the firmware's code took all the time it has as devices start: some are described less");
        }
        acpi.ns.limit(None);
    }

    // The PC's own devices go too (`arch::platform`: the keyboard
    // controller).
    let platform = if runs.is_ok() { Platform::claim(&mgr.io, &mgr.irq) } else { Platform::none() };
    // It starts for its devices, or because the boot options ask for it.
    let mut vm = None;
    if runs.is_ok() && (!guest.is_empty() || !platform.is_empty() || drivervm.iter().any(|a| a == "drivervm")) {
        match take(roles::HYPERVISOR_RESOURCE) {
            Some(hypervisor) => {
                let variables = rt::env::take_handle(abi::startup::role::FIRMWARE_VARIABLES).map(Vmo::from_handle);
                let memory = vm_memory(&guest, mgr.acpi.as_ref());
                vm = Some(DriverVm::new(&drivervm, memory, guest, platform, variables, hypervisor));
            }
            None => println!("no hypervisor resource for the driver VM"),
        }
    }

    // The window system and the audio service wait for what it brings.
    let mut told = Told::new();
    let coming = |vm: &Option<DriverVm>| vm.as_ref().map(|vm| vm.coming(framebuffer)).unwrap_or_default();
    told.say(&coming(&vm));

    /// The driver VM's process, among the channels.
    const DRIVERVM: u64 = u64::MAX;
    loop {
        if let Some(v) = vm.as_mut()
            && v.next_start_ns.is_some_and(|at| rt::time::now_ns() >= at)
        {
            v.start(&boot, &image, &mut bound, &mut next);
            // A start that failed for good brings nothing.
            told.say(&coming(&vm));
        }
        let mut ws = WaitSet::new();
        for (&k, b) in &bound {
            ws.add(b.channel.raw(), signals::READABLE | signals::PEER_CLOSED, k);
        }
        for (handle, wanted, k) in mgr.gpio.borrow().waits() {
            ws.add(handle, wanted, k);
        }
        for (handle, k) in told.channels() {
            ws.add(handle, signals::PEER_CLOSED, k);
        }
        let mut deadline = told.again_at.unwrap_or(abi::DEADLINE_INFINITE);
        if let Some(vm) = &vm {
            if let Some(p) = &vm.process {
                ws.add(p.raw(), signals::TERMINATED, DRIVERVM);
            }
            if let Some(at) = vm.next_start_ns {
                deadline = deadline.min(at);
            }
        }
        if ws.is_empty() && deadline == abi::DEADLINE_INFINITE {
            rt::time::sleep(rt::time::Duration::from_secs(3600));
            continue;
        }
        let Ok(ready) = ws.wait(deadline) else { continue };
        if told.again_at.is_some_and(|at| rt::time::now_ns() >= at) {
            told.connect();
            told.say(&coming(&vm));
        }
        let mut closed: Vec<u64> = Vec::new();
        for (k, observed) in ready {
            if k == TOLD_DISPLAY || k == TOLD_AUDIO {
                told.closed(k);
                continue;
            }
            if k == DRIVERVM {
                if let Some(vm) = vm.as_mut() {
                    vm.ended(&mgr.config);
                }
                told.say(&coming(&vm));
                continue;
            }
            if k & gpio::KEYS != 0 {
                mgr.gpio.borrow_mut().signalled(k);
                continue;
            }
            let Some(dev) = bound.get(&k) else { continue };
            if observed & signals::READABLE != 0 {
                while let Ok(msg) = dev.channel.read() {
                    let mut s = DeviceSession { mgr: &mgr, dev, key: k };
                    if let Ok(reply) = pcidev::dispatch(&mut s, msg) {
                        let _ = reply.send(&dev.channel);
                    }
                }
            } else if observed & signals::PEER_CLOSED != 0 {
                closed.push(k);
            }
        }
        for k in closed {
            mgr.gpio.borrow_mut().release(k);
            if let Some(b) = bound.remove(&k) {
                if b.guest {
                    match vm.as_mut() {
                        Some(v) if v.running() => {
                            v.turned_down(b.address);
                            println!(
                                "the driver VM did not take {:04x}:{:04x}; it stays the host's",
                                b.info.vendor, b.info.device
                            );
                            told.say(&coming(&vm));
                        }
                        _ => println!(
                            "the driver VM let go of {:04x}:{:04x}; it reaches nothing",
                            b.info.vendor, b.info.device
                        ),
                    }
                } else {
                    println!("driver for {:04x}:{:04x} exited", b.info.vendor, b.info.device);
                }
            }
        }
    }
}
