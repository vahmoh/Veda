// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The per-device PCI protocol served by `device-manager`, and what drivers
//! build on it.
//!
//! A driver is started with a channel speaking this protocol for exactly one
//! device: it can read and write that device's configuration space, map its
//! memory BARs, enable it, and allocate MSI interrupts and DMA memory (or
//! have the line its INTx is wired to). It also
//! learns what the firmware describes below the device (ACPI: the
//! amplifiers on an SPI controller, say), and drives the GPIO pins those
//! devices are wired to, and has their interrupts — those pins only.
//! Nothing else.
//!
//! What drivers do alike with it is here too: walking the device's
//! capability list ([`capabilities`]), setting up MSI or MSI-X
//! ([`enable_msi`], [`enable_msix`]), reaching its registers
//! ([`Registers`], a BAR mapped) and memory it reads and writes itself
//! ([`DmaBuffer`]). What a device reports (where its capabilities and
//! tables are, how long they are) steers none of their accesses anywhere
//! else: each is checked against the memory it is in.

use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;
use core::ptr::{read_volatile, write_volatile};

use abi::{PAGE_SIZE, map_flags};
use ipc::{IpcError, enumeration, message, protocol, union};
use rt::object::{Interrupt, Resource, Vmo};
use rt::vm::Mapping;

message! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Bar {
        pub index: u8,
        /// 0 = memory, 1 = I/O.
        pub io: bool,
        pub address: u64,
        pub size: u64,
        pub prefetchable: bool,
    }
}

message! {
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct DeviceInfo {
        pub vendor: u16,
        pub device: u16,
        pub class: u8,
        pub subclass: u8,
        pub prog_if: u8,
        pub revision: u8,
        pub bus: u8,
        pub slot: u8,
        pub function: u8,
        pub irq_line: u8,
        pub irq_pin: u8,
        /// The BARs a driver may have: its I/O BARs, and its memory BARs
        /// on pages no other function's BAR is on (see `map_bar`).
        pub bars: Vec<Bar>,
    }
}

message! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct MsiAddress {
        pub address: u64,
        pub data: u32,
    }
}

message! {
    /// An interrupt controller's input (a GSI), and how it signals.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct InterruptLine {
        pub gsi: u32,
        pub level: bool,
        pub active_low: bool,
    }
}

message! {
    /// Memory the firmware keeps for a function (an RMRR of the IOMMU's
    /// table: a GPU's stolen memory, which it and the firmware's
    /// framebuffer use): where it is, and a VMO of it.
    #[derive(Debug)]
    pub struct ReservedMemory {
        pub base: u64,
        pub size: u64,
        pub memory: Vmo,
    }
}

message! {
    /// What the firmware gives an Intel integrated GPU's driver: its
    /// OpRegion (the memory its configuration space's ASLS names: the
    /// display's description and the firmware's mailboxes), and its raw VBT
    /// if the OpRegion keeps that outside itself (empty if not).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct OpRegion {
        pub opregion: Vec<u8>,
        pub vbt: Vec<u8>,
    }
}

message! {
    /// The registers of the PC's host bridge (00:00.0) that an Intel
    /// integrated GPU's driver reads: its ids, class and subsystem (the
    /// dwords at 0x00, 0x08 and 0x2C), its MCHBAR (0x48) and its graphics
    /// control (the dword at 0x50).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct HostBridge {
        pub ids: u32,
        pub class: u32,
        pub subsystem: u32,
        pub mchbar: u64,
        pub graphics_control: u32,
    }
}

message! {
    /// Where a function's INTx goes, as the firmware routes it (`_PRT`).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct IntxLine {
        /// The function's pin: 1 for INTA# to 4.
        pub pin: u8,
        /// The interrupt controller's input (GSI) it is wired to, and how.
        pub gsi: u32,
        pub level: bool,
        pub active_low: bool,
    }
}

message! {
    /// A device the firmware (ACPI) describes below a PCI function: what
    /// it is, and the resources it uses.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct AcpiDevice {
        /// Its path in the ACPI namespace (`\_SB.PC00.SPI1.SPK1`).
        pub path: String,
        /// Hardware id (`CSC3551`); empty if none.
        pub hid: String,
        /// Compatible ids (`PNP0C50`).
        pub cids: Vec<String>,
        /// Unique id; empty if none.
        pub uid: String,
        /// Subsystem id: the board's, for devices that need per-board
        /// settings (`10431F62`); empty if none.
        pub sub: String,
        /// `_STA` (bit 0: present).
        pub status: u32,
        pub resources: Vec<AcpiResource>,
        /// Why its resources could not be read; empty if they could.
        pub resource_error: String,
    }
}

union! {
    /// One resource an ACPI device uses.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum AcpiResource {
        1 => Memory { base: u64, length: u64 },
        2 => Io { base: u16, length: u16 },
        3 => Irq { irqs: Vec<u32>, edge: bool, active_low: bool, shared: bool },
        /// A GPIO connection: an input or output (`GpioIo`), or an
        /// interrupt (`GpioInt`). `pull`: 0 default, 1 up, 2 down, 3 none;
        /// `restriction`: 0 either way, 1 input only, 2 output only. An
        /// interrupt is an edge or level-triggered (`edge`), `polarity` 0
        /// active high (or rising), 1 active low (or falling), 2 both
        /// edges, and may wake the machine (`wake`); `debounce` is in
        /// hundredths of a millisecond. The controllers of connections are
        /// absolute paths where the name resolves (as written where it
        /// does not).
        4 => Gpio {
            interrupt: bool,
            pins: Vec<u16>,
            controller: String,
            pull: u8,
            restriction: u8,
            shared: bool,
            edge: bool,
            polarity: u8,
            wake: bool,
            debounce: u16,
        },
        /// The device's connection to an SPI controller.
        5 => Spi {
            controller: String,
            chip_select: u16,
            speed_hz: u32,
            bits: u8,
            cpol: bool,
            cpha: bool,
            cs_active_high: bool,
        },
        6 => I2c { controller: String, address: u16, speed_hz: u32, ten_bit: bool },
        /// Something else (its descriptor type).
        7 => Other { kind: u8 },
    }
}

enumeration! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum PciError {
        BadOffset = 1,
        NoSuchBar = 2,
        Denied = 3,
        NoResources = 4,
        /// No such ACPI device or GPIO connection.
        NotFound = 5,
        /// A GPIO controller the device manager does not drive.
        Unsupported = 6,
        /// The pin belongs to the firmware, or its settings are locked.
        Busy = 7,
    }
}

impl core::fmt::Display for PciError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            PciError::BadOffset => "no such register",
            PciError::NoSuchBar => "no such BAR",
            PciError::Denied => "denied",
            PciError::NoResources => "out of resources",
            PciError::NotFound => "not found",
            PciError::Unsupported => "not supported",
            PciError::Busy => "busy",
        })
    }
}

protocol! {
    /// Access to one PCI function.
    pub mod pcidev = "pcidev" {
        1 => fn info() -> DeviceInfo;
        2 => fn config_read(offset: u16, width: u8) -> Result<u32, PciError>;
        3 => fn config_write(offset: u16, width: u8, value: u32) -> Result<(), PciError>;
        /// Maps a memory BAR (uncached physical VMO): the pages it is on,
        /// from the one it starts on (see [`Registers`]).
        4 => fn map_bar(index: u8) -> Result<Vmo, PciError>;
        /// Enables memory and I/O decoding and, optionally, bus mastering
        /// (DMA).
        5 => fn enable(bus_master: bool) -> Result<(), PciError>;
        /// Allocates an MSI/MSI-X interrupt; program the returned address and
        /// data into the device. No more than the function can raise (the
        /// entries of its MSI-X table and the messages of its MSI
        /// capability): `NoResources`.
        6 => fn alloc_msi() -> Result<(Interrupt, MsiAddress), PciError>;
        /// A resource that allows allocating DMA (physically contiguous) memory.
        7 => fn dma_resource() -> Result<Resource, PciError>;
        /// The devices the firmware describes below this one; none if it
        /// describes none (or there are no ACPI tables).
        9 => fn acpi_devices() -> Vec<AcpiDevice>;
        /// The level of GPIO pin `index` of ACPI device `device` (its index
        /// in `acpi_devices`), counting the pins of its `Gpio` resources in
        /// order: what it drives, if it is an output; what it reads, made
        /// an input if it is neither.
        10 => fn gpio_read(device: u32, index: u32) -> Result<bool, PciError>;
        /// Drives GPIO pin `index` of ACPI device `device` high or low,
        /// making it an output if it is not one.
        11 => fn gpio_write(device: u32, index: u32, high: bool) -> Result<(), PciError>;
        /// The resource that names this function (a PCI resource of its
        /// requester id), for giving it to a virtual machine: only the
        /// driver VM's devices have one (`Denied`).
        12 => fn device_resource() -> Result<Resource, PciError>;
        /// The interrupt line the function's INTx is wired to (`NotFound`:
        /// it has none, or the firmware does not say where it goes). Every
        /// function on a line shares the one interrupt; a driver that
        /// takes it leaves the function's MSIs off.
        13 => fn intx() -> Result<(Interrupt, IntxLine), PciError>;
        /// What `_DSM` of ACPI device `device` (its index in
        /// `acpi_devices`) answers function `function` of `uuid` (its 16
        /// bytes, as `ToUUID` makes them) at `revision`, without arguments:
        /// an integer (`NotFound` if it has no `_DSM` or answers anything
        /// else).
        14 => fn acpi_dsm(device: u32, uuid: Vec<u8>, revision: u64, function: u64) -> Result<u64, PciError>;
        /// Constant object `name` (`_DSD`, an I2C controller's `FMCN`) of
        /// ACPI device `device`, or of the function's own device with
        /// `device` `u32::MAX`, as AML (`NotFound` if it has none;
        /// `Unsupported` if it is no constant: integers, strings, buffers
        /// and packages of them).
        15 => fn acpi_data(device: u32, name: String) -> Result<Vec<u8>, PciError>;
        /// The interrupt line of interrupt `index` of ACPI device `device`
        /// (counting the interrupts of its `Irq` resources in order), as
        /// `intx` gives a function's: shared by whoever else is on it.
        16 => fn acpi_interrupt(device: u32, index: u32) -> Result<(Interrupt, InterruptLine), PciError>;
        /// Makes GPIO pin `index` of ACPI device `device` an input.
        17 => fn gpio_input(device: u32, index: u32) -> Result<(), PciError>;
        /// Whether GPIO pin `index` of ACPI device `device` drives its level
        /// (an output) rather than reading it.
        18 => fn gpio_is_output(device: u32, index: u32) -> Result<bool, PciError>;
        /// The interrupt of GPIO pin `index` of ACPI device `device`, whose
        /// connection is an interrupt (`GpioInt`; `NotFound` if it is
        /// not), the pin set up to interrupt as the connection says: raised
        /// when the pin does — an edge, or level-triggered, raised (and the
        /// pin masked) until it is ended (acknowledged, or the guest's
        /// end-of-interrupt). `Busy` if the firmware keeps the pin.
        19 => fn gpio_interrupt(device: u32, index: u32) -> Result<Interrupt, PciError>;
        /// The firmware's tables that describe the function's hardware
        /// rather than the machine's, whole, as the firmware has them: an
        /// Intel audio controller's NHLT (the links of its DSP, and the
        /// microphones and ports on them). None if there are none.
        20 => fn acpi_tables() -> Vec<Vec<u8>>;
        /// The memory the firmware keeps for the function, which a guest
        /// it is given to must have where the PC has it (its IOMMU domain
        /// maps it there): only the driver VM's functions get it
        /// (`Denied`).
        21 => fn reserved_memory() -> Result<Vec<ReservedMemory>, PciError>;
        /// An Intel integrated GPU's OpRegion, as the firmware has it
        /// (`NotFound`: it has none; `Unsupported`: the function is no
        /// Intel GPU).
        22 => fn opregion() -> Result<OpRegion, PciError>;
        /// The PC's host bridge's registers that an Intel integrated GPU's
        /// driver reads (`Unsupported`: the function is no Intel GPU).
        23 => fn host_bridge() -> Result<HostBridge, PciError>;
    }
}

/// Well-known capability ids.
pub mod cap {
    pub const MSI: u8 = 0x05;
    pub const VENDOR: u8 = 0x09;
    pub const MSI_X: u8 = 0x11;
}

/// The status register (its low byte), whose bit 4 says the function has a
/// capability list, and the register that points at the list's first
/// entry.
const STATUS: u16 = 0x06;
const STATUS_CAPABILITIES: u8 = 1 << 4;
const CAPABILITIES_POINTER: u16 = 0x34;
/// Capabilities are in the device's own part of the first 256 bytes of
/// configuration space, from here on, which holds at most 48 of them.
const DEVICE_PART: u16 = 0x40;
const MOST_CAPABILITIES: u8 = 48;

/// An entry of a function's capability list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capability {
    /// What it is (see [`cap`]).
    pub id: u8,
    /// Where it is in configuration space.
    pub offset: u16,
}

/// The entries of a function's capability list, in the order its device
/// links them. The links lead nowhere else, whatever the device says: the
/// list ends at a link out of the device's part of configuration space
/// (below 0x40), at an entry no device answers for (all ones), at a read
/// that fails, and after as many entries as fit (48), so that a list that
/// goes round ends too.
pub struct Capabilities<F> {
    /// Reads the byte at an offset of the function's configuration space
    /// (`None`: it cannot be read).
    read: F,
    /// Where the next entry is (0: there is none).
    next: u16,
    /// How many more entries there may be.
    left: u8,
}

impl<F: FnMut(u16) -> Option<u8>> Capabilities<F> {
    /// The capability list of the function whose configuration space `read`
    /// reads a byte of (`None`: it cannot be read).
    pub fn new(mut read: F) -> Capabilities<F> {
        // The pointer means something only if the status register says so.
        let listed = read(STATUS).is_some_and(|status| status & STATUS_CAPABILITIES != 0);
        let next = if listed { read(CAPABILITIES_POINTER).map_or(0, link) } else { 0 };
        Capabilities { read, next, left: MOST_CAPABILITIES }
    }
}

/// Where a link to an entry points (its two low bits are reserved).
fn link(pointer: u8) -> u16 {
    (pointer & 0xFC) as u16
}

impl<F: FnMut(u16) -> Option<u8>> Iterator for Capabilities<F> {
    type Item = Capability;

    fn next(&mut self) -> Option<Capability> {
        let at = core::mem::take(&mut self.next);
        if at < DEVICE_PART || self.left == 0 {
            return None;
        }
        self.left -= 1;
        let id = (self.read)(at).filter(|&id| id != 0xFF)?;
        self.next = (self.read)(at + 1).map_or(0, link);
        Some(Capability { id, offset: at })
    }
}

/// The capability list of the function `pci` is for.
pub fn capabilities(pci: &pcidev::Client) -> Capabilities<impl FnMut(u16) -> Option<u8> + '_> {
    Capabilities::new(move |offset| config_read(pci, offset, 1).map(|byte| byte as u8))
}

/// Where capability `id` is in the function's capability list, if it has
/// it.
pub fn find_capability(pci: &pcidev::Client, id: u8) -> Option<u16> {
    capabilities(pci).find(|c| c.id == id).map(|c| c.offset)
}

fn config_read(pci: &pcidev::Client, off: u16, width: u8) -> Option<u32> {
    pci.config_read(off, width).ok()?.ok()
}

/// Sets up a single MSI vector for every interrupt cause of a device with
/// the MSI capability, and returns the interrupt to wait on (`None`: no
/// MSI; poll the device instead).
pub fn enable_msi(pci: &pcidev::Client) -> Option<Interrupt> {
    let at = find_capability(pci, cap::MSI)?;
    let control = config_read(pci, at + 2, 2)? as u16;
    let (irq, msi) = pci.alloc_msi().ok()?.ok()?;
    let ok = |r: Result<Result<(), PciError>, IpcError>| matches!(r, Ok(Ok(())));
    let mut good = ok(pci.config_write(at + 4, 4, msi.address as u32));
    // 64-bit capable devices have an upper address register before the data.
    let data_at = if control & (1 << 7) != 0 {
        good &= ok(pci.config_write(at + 8, 4, (msi.address >> 32) as u32));
        at + 12
    } else {
        at + 8
    };
    good &= ok(pci.config_write(data_at, 2, msi.data & 0xFFFF));
    // Enable, with a single message.
    good &= ok(pci.config_write(at + 2, 2, ((control & !(0x7 << 4)) | 1) as u32));
    good.then_some(irq)
}

/// Bytes of an entry of an MSI-X table: the message's address (its low
/// dword, then its high one), its data, and its vector control (bit 0:
/// masked).
pub const MSIX_ENTRY: usize = 16;

/// Sets up entry 0 of the MSI-X table of a device with the MSI-X
/// capability for every interrupt cause it has that names entry 0 (a
/// controller's queues can each name one), and returns the interrupt to
/// wait on (`None`: no MSI-X, or a table not inside the BAR it says).
pub fn enable_msix(pci: &pcidev::Client) -> Option<Interrupt> {
    let at = find_capability(pci, cap::MSI_X)?;
    let control = config_read(pci, at + 2, 2)? as u16;
    let table = config_read(pci, at + 4, 4)?;
    let registers = Registers::map(pci, (table & 7) as u8).ok()?;
    let entry = inside((table & !7) as usize, MSIX_ENTRY, registers.len())?.start;
    let (irq, msi) = pci.alloc_msi().ok()?.ok()?;
    registers.write32(entry, msi.address as u32);
    registers.write32(entry + 4, (msi.address >> 32) as u32);
    registers.write32(entry + 8, msi.data);
    // Unmasked.
    registers.write32(entry + 12, 0);
    // Enabled, the function's mask off.
    let enabled = ((control | 0x8000) & !0x4000) as u32;
    matches!(pci.config_write(at + 2, 2, enabled), Ok(Ok(()))).then_some(irq)
}

/// The bytes `offset..offset + len` of something `total` bytes long, if
/// they are all inside it.
fn inside(offset: usize, len: usize, total: usize) -> Option<Range<usize>> {
    let end = offset.checked_add(len)?;
    (end <= total).then_some(offset..end)
}

/// Why a BAR could not be mapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapError {
    /// `device-manager` did not answer.
    Ipc(IpcError),
    /// `device-manager` refused (`NoSuchBar`: the function has no such
    /// memory BAR, or none a driver may have).
    Pci(PciError),
    /// The kernel would not map it.
    Kernel(abi::Error),
}

impl core::fmt::Display for MapError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            MapError::Ipc(e) => write!(f, "the device manager does not answer: {e}"),
            MapError::Pci(e) => write!(f, "the device manager refused: {e}"),
            MapError::Kernel(e) => write!(f, "it cannot be mapped: {e}"),
        }
    }
}

/// A memory BAR of a function, mapped (uncached): its registers. Accesses
/// stay inside the BAR, whatever offset a driver works out from what its
/// device reports: one outside it, or not aligned to its width, reaches
/// nothing (a read gives all ones, as a register no device answers for
/// does). Registers are little-endian, as PCI's are.
pub struct Registers {
    map: Mapping,
    /// Where the BAR starts in the mapping (which starts at the page the BAR
    /// starts on), and how long it is.
    start: usize,
    len: usize,
}

impl Registers {
    /// Maps memory BAR `index` of the function `pci` is for.
    pub fn map(pci: &pcidev::Client, index: u8) -> Result<Registers, MapError> {
        let info = pci.info().map_err(MapError::Ipc)?;
        let bar = info.bars.iter().find(|b| b.index == index && !b.io).ok_or(MapError::Pci(PciError::NoSuchBar))?;
        let vmo = pci.map_bar(index).map_err(MapError::Ipc)?.map_err(MapError::Pci)?;
        let size = vmo.size().map_err(MapError::Kernel)?;
        let start = (bar.address % PAGE_SIZE as u64) as usize;
        // All of the BAR, unless the VMO were shorter: past it is nothing.
        let len = usize::try_from(bar.size).unwrap_or(usize::MAX).min(size.saturating_sub(start));
        let map = Mapping::new(vmo, size, map_flags::READ | map_flags::WRITE).map_err(MapError::Kernel)?;
        Ok(Registers { map, start, len })
    }

    /// Bytes of registers there are.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Where the register of `width` bytes at `offset` is mapped, if it is
    /// inside the BAR and aligned to its width.
    fn register(&self, offset: usize, width: usize) -> Option<*mut u8> {
        let at = inside(offset, width, self.len)?.start;
        // SAFETY: `start + len` bytes are mapped, and `at + width <= len`.
        let p = unsafe { self.map.as_ptr().add(self.start + at) };
        p.addr().is_multiple_of(width).then_some(p)
    }

    fn load<T: Copy>(&self, offset: usize, absent: T) -> T {
        match self.register(offset, size_of::<T>()) {
            // SAFETY: a register inside the mapped BAR, aligned to its
            // width; read as the device has it now.
            Some(p) => unsafe { read_volatile(p as *const T) },
            None => absent,
        }
    }

    fn store<T: Copy>(&self, offset: usize, value: T) {
        if let Some(p) = self.register(offset, size_of::<T>()) {
            // SAFETY: as in `load`; the device takes the store as it is.
            unsafe { write_volatile(p as *mut T, value) }
        }
    }

    /// The 8-bit register at `offset`.
    pub fn read8(&self, offset: usize) -> u8 {
        self.load(offset, u8::MAX)
    }

    /// The 16-bit register at `offset`.
    pub fn read16(&self, offset: usize) -> u16 {
        u16::from_le(self.load(offset, u16::MAX))
    }

    /// The 32-bit register at `offset`.
    pub fn read32(&self, offset: usize) -> u32 {
        u32::from_le(self.load(offset, u32::MAX))
    }

    /// Writes the 8-bit register at `offset`.
    pub fn write8(&self, offset: usize, value: u8) {
        self.store(offset, value)
    }

    /// Writes the 16-bit register at `offset`.
    pub fn write16(&self, offset: usize, value: u16) {
        self.store(offset, value.to_le())
    }

    /// Writes the 32-bit register at `offset`.
    pub fn write32(&self, offset: usize, value: u32) {
        self.store(offset, value.to_le())
    }
}

/// Physically contiguous memory a device reads and writes itself (DMA),
/// mapped: a driver's queues, command tables and data. Accesses are checked
/// against its length: one past it fails (`OutOfRange`), as a VMO's does.
/// What is in it is copied in and out, or read and written a value at a
/// time, never lent out, as the device may change it at any time. Values
/// are little-endian, as PCI's devices lay them out.
pub struct DmaBuffer {
    map: Mapping,
    phys: u64,
}

impl DmaBuffer {
    /// `len` bytes (whole pages) of zeroed memory.
    pub fn new(dma: &Resource, len: usize) -> Result<DmaBuffer, abi::Error> {
        DmaBuffer::mapped(Vmo::create_contiguous(dma, len)?)
    }

    /// The same below 4 GiB, for devices limited to 32-bit addresses.
    pub fn new_below_4g(dma: &Resource, len: usize) -> Result<DmaBuffer, abi::Error> {
        DmaBuffer::mapped(Vmo::create_contiguous_below_4g(dma, len)?)
    }

    fn mapped(vmo: Vmo) -> Result<DmaBuffer, abi::Error> {
        let (len, phys) = (vmo.size()?, vmo.phys_addr(0)?);
        let map = Mapping::new(vmo, len, map_flags::READ | map_flags::WRITE)?;
        Ok(DmaBuffer { map, phys })
    }

    /// The physical address of its first byte, which its device is given.
    pub fn phys(&self) -> u64 {
        self.phys
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Where the `len` bytes at `offset` are mapped, if they are all inside
    /// it.
    fn at(&self, offset: usize, len: usize) -> Result<*mut u8, abi::Error> {
        let bytes = inside(offset, len, self.len()).ok_or(abi::Error::OutOfRange)?;
        // SAFETY: inside the mapping.
        Ok(unsafe { self.map.as_ptr().add(bytes.start) })
    }

    /// Where the value of type `T` at `offset` is mapped, if it is inside it
    /// and aligned to its size.
    fn value<T>(&self, offset: usize) -> Result<*mut T, abi::Error> {
        let p = self.at(offset, size_of::<T>())?;
        if p.addr().is_multiple_of(size_of::<T>()) { Ok(p as *mut T) } else { Err(abi::Error::InvalidArgs) }
    }

    fn load<T: Copy>(&self, offset: usize) -> Result<T, abi::Error> {
        let p = self.value::<T>(offset)?;
        // SAFETY: a value inside the mapping, aligned to its size; read as
        // it is now, whoever wrote it.
        Ok(unsafe { read_volatile(p) })
    }

    fn store<T: Copy>(&mut self, offset: usize, value: T) -> Result<(), abi::Error> {
        let p = self.value::<T>(offset)?;
        // SAFETY: as in `load`; written in one access, which the device
        // sees whole.
        unsafe { write_volatile(p, value) };
        Ok(())
    }

    /// Copies the bytes at `offset` into `out`.
    pub fn read(&self, offset: usize, out: &mut [u8]) -> Result<(), abi::Error> {
        let from = self.at(offset, out.len())?;
        // SAFETY: `out.len()` bytes inside the mapping, copied to memory of
        // this process's own, which is no part of it.
        unsafe { core::ptr::copy_nonoverlapping(from, out.as_mut_ptr(), out.len()) };
        Ok(())
    }

    /// The `len` bytes at `offset`, copied out.
    pub fn to_vec(&self, offset: usize, len: usize) -> Result<Vec<u8>, abi::Error> {
        inside(offset, len, self.len()).ok_or(abi::Error::OutOfRange)?;
        let mut bytes = alloc::vec![0; len];
        self.read(offset, &mut bytes)?;
        Ok(bytes)
    }

    /// Copies `data` to `offset`.
    pub fn write(&mut self, offset: usize, data: &[u8]) -> Result<(), abi::Error> {
        let to = self.at(offset, data.len())?;
        // SAFETY: as in `read`, the other way; `&mut self` keeps this
        // process's other writers of it out meanwhile.
        unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), to, data.len()) };
        Ok(())
    }

    /// The 16-bit value at `offset`, as it is now (its device may be
    /// writing it).
    pub fn read16(&self, offset: usize) -> Result<u16, abi::Error> {
        self.load(offset).map(u16::from_le)
    }

    /// The 32-bit value at `offset`, as it is now.
    pub fn read32(&self, offset: usize) -> Result<u32, abi::Error> {
        self.load(offset).map(u32::from_le)
    }

    /// Writes the 16-bit `value` at `offset`, in one access (its device may
    /// be reading it).
    pub fn write16(&mut self, offset: usize, value: u16) -> Result<(), abi::Error> {
        self.store(offset, value.to_le())
    }

    /// Writes the 32-bit `value` at `offset`, in one access.
    pub fn write32(&mut self, offset: usize, value: u32) -> Result<(), abi::Error> {
        self.store(offset, value.to_le())
    }

    /// Writes the 64-bit `value` at `offset`, in one access.
    pub fn write64(&mut self, offset: usize, value: u64) -> Result<(), abi::Error> {
        self.store(offset, value.to_le())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::vec::Vec;

    /// A function's configuration space: whether its status register says
    /// it has a capability list, where its pointer points, and its
    /// entries (where each is, its id, its link to the next).
    fn space(listed: bool, pointer: u8, entries: &[(u8, u8, u8)]) -> [u8; 256] {
        let mut space = [0u8; 256];
        space[STATUS as usize] = if listed { STATUS_CAPABILITIES } else { 0 };
        space[CAPABILITIES_POINTER as usize] = pointer;
        for &(at, id, next) in entries {
            space[at as usize] = id;
            space[at as usize + 1] = next;
        }
        space
    }

    fn listed(space: &[u8; 256]) -> Vec<(u8, u16)> {
        Capabilities::new(|offset| space.get(offset as usize).copied()).map(|c| (c.id, c.offset)).collect()
    }

    #[test]
    fn capabilities_are_listed_as_linked() {
        // The links' reserved low bits are no part of where they point.
        let s = space(true, 0x50, &[(0x50, cap::MSI, 0x73), (0x70, cap::VENDOR, 0x40), (0x40, cap::MSI_X, 0)]);
        assert_eq!(listed(&s), [(cap::MSI, 0x50), (cap::VENDOR, 0x70), (cap::MSI_X, 0x40)]);
    }

    #[test]
    fn a_function_whose_status_says_it_has_no_list_has_none() {
        let s = space(false, 0x50, &[(0x50, cap::MSI, 0)]);
        assert!(listed(&s).is_empty());
    }

    #[test]
    fn a_list_that_goes_round_ends() {
        let s = space(true, 0x40, &[(0x40, cap::VENDOR, 0x80), (0x80, cap::VENDOR, 0x40)]);
        assert_eq!(listed(&s).len(), MOST_CAPABILITIES as usize);
    }

    #[test]
    fn a_link_into_the_header_or_to_nothing_ends_the_list() {
        // Below 0x40 is the header, not a capability.
        let s = space(true, 0x50, &[(0x50, cap::MSI, 0x10), (0x10, cap::VENDOR, 0x50)]);
        assert_eq!(listed(&s), [(cap::MSI, 0x50)]);
        // No device answers for an entry of all ones.
        let s = space(true, 0x50, &[(0x50, cap::MSI, 0x60), (0x60, 0xFF, 0x70), (0x70, cap::VENDOR, 0)]);
        assert_eq!(listed(&s), [(cap::MSI, 0x50)]);
        let s = space(true, 0x08, &[(0x08, cap::MSI, 0)]);
        assert!(listed(&s).is_empty());
    }

    #[test]
    fn a_read_that_fails_ends_the_list() {
        let s = space(true, 0x50, &[(0x50, cap::MSI, 0x90), (0x90, cap::VENDOR, 0)]);
        let found: Vec<u8> =
            Capabilities::new(|offset| (offset < 0x80).then(|| s[offset as usize])).map(|c| c.id).collect();
        assert_eq!(found, [cap::MSI]);
    }

    #[test]
    fn accesses_stay_inside() {
        assert_eq!(inside(4, 4, 8), Some(4..8));
        assert_eq!(inside(8, 0, 8), Some(8..8));
        assert_eq!(inside(5, 4, 8), None);
        assert_eq!(inside(usize::MAX, 2, 8), None);
        assert_eq!(inside(2, usize::MAX, usize::MAX), None);
    }
}
