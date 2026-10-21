// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The firmware's description of the machine (ACPI): which devices it
//! places below which PCI functions, what they are and which resources
//! they use, for their drivers. A driver only learns about the devices
//! below its own PCI function.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::ops::Range;

use abi::{KernelBootInfo, cache_policy, map_flags};
use acpi::aml::Namespace;
use acpi::device::{self, Described, Identity, ResourcesError};
use acpi::name::Path;
use acpi::resource::Resource as AcpiResource;
use acpi::tables;
use acpi::{Memory, PciFunction};
use proto::pci::DeviceInfo;
use rt::object::{Resource, Vmo};
use rt::println;

use crate::pci::{Address, ConfigSpace};

const PAGE: u64 = 4096;

/// Firmware memory, mapped as it is first read: the ACPI tables and the
/// firmware's variables. The tables, and what the kernel reported as the
/// firmware's ACPI memory, are RAM and are mapped cached; anything else
/// AML reads (device registers) uncached, with the accesses its fields'
/// access types say. And PCI configuration space, as devmgr reads it (the
/// host bridge's registers, which a root bridge's `_CRS` may size its
/// windows by).
pub struct FirmwareMemory {
    mmio: Resource,
    ram: Vec<(u64, u64)>,
    windows: RefCell<Vec<Window>>,
    config: Rc<ConfigSpace>,
}

struct Window {
    start: u64,
    end: u64,
    cached: bool,
    address: usize,
    _vmo: Vmo,
}

impl FirmwareMemory {
    fn new(mmio: Resource, boot: &KernelBootInfo, config: Rc<ConfigSpace>) -> FirmwareMemory {
        let n = (boot.acpi_memory_count as usize).min(boot.acpi_memory.len());
        let ram = boot.acpi_memory[..n].iter().map(|r| (r[0], r[1])).collect();
        FirmwareMemory { mmio, ram, windows: RefCell::new(Vec::new()), config }
    }

    fn is_ram(&self, start: u64, end: u64) -> bool {
        self.ram.iter().any(|&(s, e)| start >= s && end <= e)
    }

    /// Maps the pages covering `[start, end)`, or finds them mapped.
    fn window(&self, start: u64, end: u64, cached: bool) -> Option<usize> {
        let found = self
            .windows
            .borrow()
            .iter()
            .find(|w| w.cached == cached && start >= w.start && end <= w.end)
            .map(|w| w.address + (start - w.start) as usize);
        if found.is_some() {
            return found;
        }
        let first = start & !(PAGE - 1);
        let mut last = end.div_ceil(PAGE) * PAGE;
        if cached {
            // Tables are read in pieces: map 64 KiB at a time (inside the
            // firmware's range, when it is one the kernel reported).
            let limit = self.ram.iter().find(|&&(s, e)| first >= s && first < e).map_or(last, |&(_, e)| e);
            last = last.max((first + 16 * PAGE).min(limit));
        }
        let size = (last - first) as usize;
        let cache = if cached { cache_policy::WRITE_BACK } else { cache_policy::UNCACHED };
        let vmo = Vmo::create_physical(&self.mmio, first, size, cache).ok()?;
        let address = vmo.map(0, size, map_flags::READ).ok()?;
        self.windows.borrow_mut().push(Window { start: first, end: last, cached, address, _vmo: vmo });
        Some(address + (start - first) as usize)
    }

    /// Reads `buf.len()` bytes at `address`: copied if `cached` (RAM), else
    /// as device registers are, with accesses of `width` bytes (1, 2, 4 or
    /// 8) one after the other; one its width does not align (in a region
    /// that is not) is made a byte at a time.
    fn read_as(&self, address: u64, buf: &mut [u8], cached: bool, width: usize) -> bool {
        let Some(end) = address.checked_add(buf.len() as u64) else { return false };
        let Some(at) = self.window(address, end, cached) else { return false };
        // SAFETY: `at` maps `buf.len()` readable bytes for as long as the
        // window lives (the life of the process); each access is inside
        // them, and aligned to its width.
        unsafe {
            if cached {
                core::ptr::copy_nonoverlapping(at as *const u8, buf.as_mut_ptr(), buf.len());
                return true;
            }
            let width = width.max(1);
            for (i, chunk) in buf.chunks_mut(width).enumerate() {
                let p = at + i * width;
                match chunk.len() {
                    8 if p.is_multiple_of(8) => {
                        chunk.copy_from_slice(&core::ptr::read_volatile(p as *const u64).to_le_bytes())
                    }
                    4 if p.is_multiple_of(4) => {
                        chunk.copy_from_slice(&core::ptr::read_volatile(p as *const u32).to_le_bytes())
                    }
                    2 if p.is_multiple_of(2) => {
                        chunk.copy_from_slice(&core::ptr::read_volatile(p as *const u16).to_le_bytes())
                    }
                    _ => {
                        for (j, b) in chunk.iter_mut().enumerate() {
                            *b = core::ptr::read_volatile((p + j) as *const u8);
                        }
                    }
                }
            }
        }
        true
    }

    fn is_cached(&self, address: u64, len: usize) -> bool {
        address.checked_add(len as u64).is_some_and(|end| self.is_ram(address, end))
    }
}

impl Memory for FirmwareMemory {
    /// Device registers in whole 32-bit reads where they are aligned.
    fn read(&self, address: u64, buf: &mut [u8]) -> bool {
        self.read_as(address, buf, self.is_cached(address, buf.len()), 4)
    }

    fn read_field(&self, address: u64, width: usize, buf: &mut [u8]) -> bool {
        self.read_as(address, buf, self.is_cached(address, buf.len()), width)
    }

    fn read_pci(&self, f: PciFunction, offset: u16, buf: &mut [u8]) -> bool {
        self.read_pci_field(f, offset, 1, buf)
    }

    /// Through the configuration mechanism (`architecture::config`):
    /// segment 0, as much of each function's space as it reaches, in
    /// accesses of the field's width (configuration space has none wider
    /// than 4 bytes).
    fn read_pci_field(&self, f: PciFunction, offset: u16, width: usize, buf: &mut [u8]) -> bool {
        let reach = crate::architecture::config::CONFIG_BYTES as usize;
        if f.segment != 0 || f.device > 31 || f.function > 7 || offset as usize + buf.len() > reach {
            return false;
        }
        let a = Address { bus: f.bus, slot: f.device, function: f.function };
        let width = width.clamp(1, 4);
        for (i, chunk) in buf.chunks_mut(width).enumerate() {
            let at = offset + (i * width) as u16;
            if matches!(chunk.len(), 1 | 2 | 4) && at.is_multiple_of(chunk.len() as u16) {
                let v = self.config.read(a, at, chunk.len() as u8);
                chunk.copy_from_slice(&v.to_le_bytes()[..chunk.len()]);
            } else {
                for (j, b) in chunk.iter_mut().enumerate() {
                    *b = self.config.read(a, at + j as u16, 1) as u8;
                }
            }
        }
        true
    }
}

/// The tables themselves, which are always in RAM.
struct Tables<'a>(&'a FirmwareMemory);

impl Memory for Tables<'_> {
    fn read(&self, address: u64, buf: &mut [u8]) -> bool {
        self.0.read_as(address, buf, true, 1)
    }
}

/// A table that describes a device's hardware rather than the machine's,
/// which goes with the device to its driver (`pcidev::acpi_tables`): its
/// signature, and the devices it describes.
struct DeviceTable {
    signature: &'static [u8; 4],
    describes: fn(&DeviceInfo) -> bool,
}

/// An Intel audio controller's NHLT: the links of its DSP, and the
/// microphones and ports on them.
const DEVICE_TABLES: [DeviceTable; 1] = [DeviceTable { signature: b"NHLT", describes: intel_audio }];

fn intel_audio(info: &DeviceInfo) -> bool {
    info.vendor == 0x8086 && info.class == 0x04 && matches!(info.subclass, 0x01 | 0x03)
}

pub struct Acpi {
    pub ns: Namespace,
    pub memory: FirmwareMemory,
    /// PCI root bridges: their bus number and path.
    roots: Vec<(u8, Path)>,
    /// The firmware's [`DEVICE_TABLES`].
    device_tables: Vec<tables::Table>,
    /// The memory the firmware keeps for devices (`architecture::dmar`):
    /// each range, with the requester ids of the functions it is for.
    reserved: Vec<(Vec<u16>, Range<u64>)>,
}

impl Acpi {
    /// Reads and loads the firmware's tables. Logs what it found.
    pub fn load(mmio: &Resource, boot: &KernelBootInfo, config: Rc<ConfigSpace>) -> Option<Acpi> {
        if boot.acpi_rsdp == 0 {
            println!("acpi: the firmware has no ACPI tables");
            return None;
        }
        let memory = FirmwareMemory::new(mmio.duplicate().ok()?, boot, config);
        let tables = match tables::load(&Tables(&memory), boot.acpi_rsdp) {
            Ok(t) => t,
            Err(e) => {
                println!("acpi: cannot read the tables: {}", e);
                return None;
            }
        };
        let mut ns = Namespace::new();
        let (mut loaded, mut blocks) = (0, 0);
        let aml = tables.iter().filter(|t| t.is(b"DSDT")).chain(tables.iter().filter(|t| t.is(b"SSDT")));
        for t in aml {
            blocks += 1;
            match ns.load_table(t, &memory) {
                Ok(()) => loaded += 1,
                Err(e) => {
                    let s = t.signature();
                    let name = String::from_utf8_lossy(&s);
                    println!("acpi: {} '{}' loaded only in part: {}", name, t.oem_table_id(), e);
                }
            }
        }
        // The devices' _INI methods finish describing them (a touchpad's id
        // and connections), as an OS runs them once the tables are loaded.
        let init = ns.initialize(&memory);
        if init.failures > 0 {
            println!("acpi: {} of the devices' _INI and _STA methods failed", init.failures);
            for (method, e) in &init.failed {
                println!("acpi: {}: {}", method, e);
            }
        }
        let roots = device::pci_roots(&ns, &memory);
        let devices = ns.devices().count();
        println!(
            "acpi: {} tables, {} of {} with device definitions loaded: {} devices ({} _INI methods run), {} PCI root \
             bridge(s){}",
            tables.len(),
            loaded,
            blocks,
            devices,
            init.ran,
            roots.len(),
            if ns.skipped > 0 {
                alloc::format!(" ({} conditional definitions left out)", ns.skipped)
            } else {
                String::new()
            }
        );
        let reserved = crate::architecture::dmar::reserved_memory(&tables);
        let device_tables = tables.into_iter().filter(|t| DEVICE_TABLES.iter().any(|d| t.is(d.signature))).collect();
        Some(Acpi { ns, memory, roots, device_tables, reserved })
    }

    /// The memory the firmware keeps for function `bus:slot.function`, as
    /// `[start, end)` ranges.
    pub fn reserved_memory(&self, bus: u8, slot: u8, function: u8) -> Vec<Range<u64>> {
        let sid = (bus as u16) << 8 | (slot as u16) << 3 | function as u16;
        self.reserved.iter().filter(|(of, _)| of.contains(&sid)).map(|(_, r)| r.clone()).collect()
    }

    /// The firmware's tables that describe the hardware of the function
    /// `info` is ([`DEVICE_TABLES`]), whole.
    pub fn device_tables(&self, info: &DeviceInfo) -> Vec<Vec<u8>> {
        let described = DEVICE_TABLES.iter().filter(|d| (d.describes)(info));
        described
            .filter_map(|d| self.device_tables.iter().find(|t| t.is(d.signature)))
            .map(|t| t.data.clone())
            .collect()
    }

    /// The ACPI device that describes PCI function `bus:slot.function`
    /// (devices on a root bus only).
    pub fn pci_companion(&self, bus: u8, slot: u8, function: u8) -> Option<Path> {
        device::pci_companion(&self.ns, &self.roots, (bus, slot, function), &self.memory)
    }

    /// The memory the firmware reserves for the motherboard (that its
    /// `PNP0C01` and `PNP0C02` devices use), which no BAR may be placed
    /// over. Logs what of it is not known.
    pub fn motherboard_memory(&self) -> Vec<Range<u64>> {
        let mut reserved = Vec::new();
        for r in device::motherboard_memory(&self.ns, &self.memory) {
            match r.memory {
                Ok(m) => reserved.extend(m),
                Err(e) => println!("acpi: {}: {}: what it reserves for the motherboard is not known", r.device, e),
            }
        }
        reserved
    }

    /// The memory windows of the PCI root bridge of bus `bus` (see
    /// [`device::pci_root_windows`]); none if no root bridge has that bus,
    /// or its `_CRS` fails (which it logs).
    pub fn pci_root_windows(&self, bus: u8) -> Vec<Range<u64>> {
        let Some((_, root)) = self.roots.iter().find(|(b, _)| *b == bus) else { return Vec::new() };
        device::pci_root_windows(&self.ns, root, &self.memory).unwrap_or_else(|e| {
            println!("acpi: {}: {}: its windows are not known", root, e);
            Vec::new()
        })
    }

    /// Where INTx `pin` (1 to 4) of the functions in `slot` of root bus
    /// `bus` goes (see [`device::pci_interrupt`]).
    pub fn pci_interrupt(&self, bus: u8, slot: u8, pin: u8) -> Option<device::IntxRoute> {
        device::pci_interrupt(&self.ns, &self.roots, (bus, slot), pin, &self.memory)
    }

    /// The devices directly below `path`, as their drivers see them.
    pub fn children(&self, path: &Path) -> Vec<Described> {
        device::describe_children(&self.ns, path, &self.memory)
    }

    /// A device's identity.
    pub fn identify(&self, path: &Path) -> Identity {
        device::identify(&self.ns, path, &self.memory)
    }

    /// A device's resources.
    pub fn resources(&self, path: &Path) -> Result<Vec<AcpiResource>, ResourcesError> {
        device::resources(&self.ns, path, &self.memory)
    }
}
