// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The platform's ACPI tables: the machine as they describe it to the
//! guest, as a PC's firmware describes a PC: hardware-reduced ACPI (no
//! fixed hardware, no SCI) and no MADT (the processors are the platform's,
//! which the guest learns of as its architecture's platform says). The
//! DSDT holds the PCI root (bus 0, the windows the BARs are in), its
//! functions and where their INTx go (`_PRT`: the GSIs the guest has,
//! level-triggered), and the devices of the architecture's machine
//! ([`Platform`]: a PC's keyboard controller, `crate::amd64::acpi`). Below a
//! function are the devices the PC's firmware describes there that the
//! guest has — a touchpad on an I2C controller, amplifiers on an SPI
//! controller — with their ids, their place on the function's bus, the
//! lines their interrupts are on, the GPIO pins they are wired to, what
//! their `_DSM` answers HID over I2C, and constant data of theirs and the
//! function's (an I2C controller's timing). Those pins are on GPIO
//! controllers of the platform's (`VEDA0001`), one for each of the PC's
//! that the pins are on, with the PC's numbers for them: the pins it has
//! (`veda,pins` in its `_DSD`), and the lines the ones that interrupt are
//! on (its `_CRS`, in the order of `veda,interrupt-pins`). The FADT has the
//! flags of the architecture's boot architecture. Tables of the PC's
//! firmware that describe a function's hardware come with it, as the
//! firmware has them (an Intel audio controller's NHLT: the links of its
//! DSP).
//!
//! The tables go where the guest's memory map has room for them
//! ([`Machine::at`]); the boot parameters say where the RSDP is.
//!
//! What the PC's firmware describes comes from devmgr, as text: what of it
//! the tables cannot hold (a name that is not an ACPI name, an id with a
//! NUL in it, more GPIO pins than a descriptor takes) is left out of them,
//! and said ([`describe`]).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;

use vacpi::asm::{
    DEVICE_PROPERTIES, OEM_ID, cat, checksum, device, dsm, dword_memory, eisa, i2c_descriptor, int, interrupt_of, name,
    package, qword_memory, resource_template, scope, spi_descriptor_of, string, table, try_device,
    try_gpio_descriptor_of, try_name, try_string, word_bus_number,
};
use vacpi::resource::Spi;
use vacpi::tables::HEADER_LEN as HEADER;

/// A GPIO connection, as [`Resource::Gpio`] holds it.
pub use vacpi::resource::Gpio;

/// A PCI function as the guest has it.
#[derive(Debug, Clone)]
pub struct Function {
    /// Where on the guest's bus 0: device << 3 | function.
    pub devfn: u8,
    /// Its INTx, if the guest has the line: the pin (1 for INTA# to 4)
    /// and the GSI.
    pub intx: Option<(u8, u32)>,
    /// Constant objects of its own the firmware gives its driver (an I2C
    /// controller's timing, `FMCN`...): names and AML terms.
    pub data: Vec<(String, Vec<u8>)>,
    /// The devices the firmware describes below it that the guest has.
    pub children: Vec<Child>,
}

/// A device the PC's firmware describes below a function: on the bus it
/// controls.
#[derive(Debug, Clone)]
pub struct Child {
    /// Its name, the firmware's (`TPD0`).
    pub name: String,
    pub hid: Option<String>,
    pub cids: Vec<String>,
    pub uid: Option<String>,
    pub sub: Option<String>,
    pub resources: Vec<Resource>,
    /// What its `_DSM` answers HID over I2C: its HID descriptor's address.
    pub hid_descriptor: Option<u16>,
    /// Constant objects (`_DSD`): names and AML terms.
    pub data: Vec<(String, Vec<u8>)>,
}

/// A resource of a described device, as the guest has it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resource {
    /// Its address on its function's I2C bus.
    I2c { address: u16, speed_hz: u32, ten_bit: bool },
    /// Its connection to its function's SPI bus.
    Spi { chip_select: u16, speed_hz: u32, bits: u8, cpol: bool, cpha: bool, cs_active_high: bool, three_wire: bool },
    /// An interrupt line the guest has (a GSI).
    Interrupt { gsi: u32, edge: bool, active_low: bool, shared: bool, wake: bool },
    /// GPIO pins it is wired to, on a controller of the guest's
    /// (`controller`: its path, `\_SB.GPI0`), by the PC's numbers.
    Gpio(Gpio),
}

/// The hardware id of the platform's GPIO controllers.
pub const GPIO_CONTROLLER: &str = "VEDA0001";

/// A GPIO controller of the guest's: the pins of one of the PC's that its
/// devices are wired to, by the PC's numbers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpioController {
    /// Its name in `\_SB` (the PC's controller's, `GPI0`).
    pub name: String,
    /// Its number for [`crate::platform::hypercall::GPIO`] (its `_UID`).
    pub uid: u32,
    /// Its pins, in order.
    pub pins: Vec<u16>,
    /// The pins that interrupt, and the lines they do on.
    pub interrupts: Vec<GpioInterrupt>,
}

/// A GPIO pin's interrupt: the line (a GSI the platform makes), as the
/// pin's connection describes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpioInterrupt {
    pub pin: u16,
    pub gsi: u32,
    pub edge: bool,
    pub active_low: bool,
}

/// HID over I2C's `_DSM` (function 1: the HID descriptor's address).
pub use vacpi::asm::HID_OVER_I2C;

/// The bytes `ToUUID` makes of a UUID's text.
pub use vacpi::asm::uuid;

/// What the architecture's machine adds to the tables.
#[derive(Debug, Clone, Default)]
pub struct Platform {
    /// The definitions of its own devices, in `\_SB` (AML).
    pub devices: Vec<u8>,
    /// Its boot architecture's flags in the FADT: where they are, and
    /// their value.
    pub boot_flags: Option<(usize, u16)>,
}

/// What the tables describe.
pub struct Machine<'a> {
    pub functions: &'a [Function],
    pub gpio: &'a [GpioController],
    /// What the architecture's machine adds.
    pub platform: &'a Platform,
    /// Where the tables go in the guest's memory, the RSDP first.
    pub at: u64,
    /// Where the functions' BARs are, below and above 4 GiB.
    pub pci_low: Range<u64>,
    pub pci_high: Range<u64>,
    /// Tables of the PC's firmware that describe the functions' hardware,
    /// whole ([`is_table`]).
    pub firmware_tables: &'a [Vec<u8>],
}

const OEM_TABLE_ID: &[u8; 8] = b"DRIVERVM";
/// The FADT of ACPI 6.5, whole.
const FADT_LENGTH: usize = 276;

/// FADT flags: hardware-reduced ACPI.
const HW_REDUCED_ACPI: u32 = 1 << 20;

/// The tables, and what they leave out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Description {
    /// The tables, to be written at [`Machine::at`].
    pub tables: Vec<u8>,
    /// What of the machine they leave out, as it cannot be described: for
    /// the monitor to say.
    pub left_out: Vec<String>,
}

/// The tables, to be written at [`Machine::at`]: the RSDP, the XSDT, the
/// FADT, the DSDT and the firmware's, one after the other, each at a
/// multiple of 16; and what of the machine they leave out.
pub fn describe(m: &Machine) -> Description {
    let mut left_out = Vec::new();
    let xsdt_at = m.at + 48;
    let xsdt_length = HEADER + 8 * (1 + m.firmware_tables.len());
    let fadt_at = (xsdt_at + xsdt_length as u64).next_multiple_of(16);
    let dsdt_at = (fadt_at + FADT_LENGTH as u64).next_multiple_of(16);
    let dsdt = table(b"DSDT", 2, OEM_TABLE_ID, &dsdt(m, &mut left_out));
    let mut end = dsdt_at + dsdt.len() as u64;
    let firmware_at: Vec<u64> = m
        .firmware_tables
        .iter()
        .map(|t| {
            let at = end.next_multiple_of(16);
            end = at + t.len() as u64;
            at
        })
        .collect();
    let listed: Vec<u8> =
        core::iter::once(fadt_at).chain(firmware_at.iter().copied()).flat_map(u64::to_le_bytes).collect();
    let mut out = Vec::new();
    out.extend_from_slice(&rsdp(xsdt_at));
    out.resize((xsdt_at - m.at) as usize, 0);
    out.extend_from_slice(&table(b"XSDT", 1, OEM_TABLE_ID, &listed));
    out.resize((fadt_at - m.at) as usize, 0);
    out.extend_from_slice(&table(b"FACP", 6, OEM_TABLE_ID, &fadt(m, dsdt_at)));
    out.resize((dsdt_at - m.at) as usize, 0);
    out.extend_from_slice(&dsdt);
    for (t, at) in m.firmware_tables.iter().zip(firmware_at) {
        out.resize((at - m.at) as usize, 0);
        out.extend_from_slice(t);
    }
    Description { tables: out, left_out }
}

/// Whether `bytes` are a whole table, as one of the firmware's must be to
/// go in the guest's: the length its header gives, and adding up to zero.
pub fn is_table(bytes: &[u8]) -> bool {
    bytes.len() >= HEADER
        && u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize == bytes.len()
        && checksum(bytes) == 0
}

/// The RSDP of ACPI 2.0 and later, which points to the XSDT.
fn rsdp(xsdt_at: u64) -> [u8; 36] {
    let mut r = [0u8; 36];
    r[..8].copy_from_slice(b"RSD PTR ");
    r[9..15].copy_from_slice(OEM_ID);
    r[15] = 2;
    r[20..24].copy_from_slice(&36u32.to_le_bytes());
    r[24..32].copy_from_slice(&xsdt_at.to_le_bytes());
    r[8] = checksum(&r[..20]);
    r[32] = checksum(&r);
    r
}

/// The FADT after its header.
fn fadt(m: &Machine, dsdt_at: u64) -> Vec<u8> {
    let mut f = [0u8; FADT_LENGTH];
    // The DSDT, below 4 GiB, at both of its addresses.
    f[40..44].copy_from_slice(&(dsdt_at as u32).to_le_bytes());
    if let Some((at, flags)) = m.platform.boot_flags {
        f[at..at + 2].copy_from_slice(&flags.to_le_bytes());
    }
    f[112..116].copy_from_slice(&HW_REDUCED_ACPI.to_le_bytes());
    // ACPI 6.5.
    f[131] = 5;
    f[140..148].copy_from_slice(&dsdt_at.to_le_bytes());
    f[268..276].copy_from_slice(b"VedaVeda");
    f[HEADER..].to_vec()
}

/// The DSDT's definitions; what cannot be described goes to `left_out`.
fn dsdt(m: &Machine, left_out: &mut Vec<String>) -> Vec<u8> {
    let mut sb = pci_root(m, left_out);
    for c in m.gpio {
        match gpio_controller(c) {
            Some(d) => sb.extend_from_slice(&d),
            None => left_out.push(format!("the GPIO controller {:?}: its name is no ACPI name", c.name)),
        }
    }
    sb.extend_from_slice(&m.platform.devices);
    scope("\\_SB", &sb)
}

/// Whether `text` is a name a device or an object of one can have: one
/// segment.
fn is_name(text: &str) -> bool {
    vacpi::name::seg(text).is_some()
}

/// The PCI root: bus 0, the windows the BARs are in, the functions and
/// where their INTx go.
fn pci_root(m: &Machine, left_out: &mut Vec<String>) -> Vec<u8> {
    let crs = cat(&[
        &word_bus_number(0, 0),
        &dword_memory(m.pci_low.start as u32, (m.pci_low.end - 1) as u32),
        &qword_memory(m.pci_high.start, m.pci_high.end - 1),
    ]);
    let mut body = cat(&[
        &name("_HID", &eisa("PNP0A03")),
        &name("_UID", &int(0)),
        &name("_SEG", &int(0)),
        &name("_BBN", &int(0)),
        &name("_CRS", &resource_template(&crs)),
    ]);
    let routes: Vec<Vec<u8>> = m
        .functions
        .iter()
        .filter_map(|f| {
            let (pin, gsi) = f.intx?;
            if !(1..=4).contains(&pin) {
                let (slot, function) = (f.devfn >> 3, f.devfn & 7);
                left_out.push(format!("00:{slot:02x}.{function}'s INTx: pin {pin} is none of INTA# to INTD#"));
                return None;
            }
            let all_functions = (f.devfn as u64 >> 3) << 16 | 0xFFFF;
            Some(package(&[int(all_functions), int(pin as u64 - 1), int(0), int(gsi as u64)]))
        })
        .collect();
    if !routes.is_empty() {
        body.extend_from_slice(&name("_PRT", &package(&routes)));
    }
    for f in m.functions {
        let path = format!("\\_SB.PCI0.{}", function_name(f.devfn));
        let address = (f.devfn as u64 >> 3) << 16 | (f.devfn as u64 & 7);
        let mut fd = name("_ADR", &int(address));
        data(&mut fd, &f.data, &path, left_out);
        for c in &f.children {
            match child(c, &path, left_out) {
                Some(d) => fd.extend_from_slice(&d),
                None => left_out.push(format!("a device below {path}: {:?} is no ACPI name", c.name)),
            }
        }
        body.extend_from_slice(&device(&function_name(f.devfn), &fd));
    }
    device("PCI0", &body)
}

/// Constant objects (`name`, AML term) of the device at `path`, added to
/// `body` as names of its, but those whose name is no ACPI name.
fn data(body: &mut Vec<u8>, data: &[(String, Vec<u8>)], path: &str, left_out: &mut Vec<String>) {
    for (n, value) in data {
        match try_name(n, value).filter(|_| is_name(n)) {
            Some(d) => body.extend_from_slice(&d),
            None => left_out.push(format!("{path}'s data {n:?}: no ACPI name")),
        }
    }
}

/// A GPIO controller of the platform's: `None` if its name is no ACPI
/// name.
fn gpio_controller(c: &GpioController) -> Option<Vec<u8>> {
    if !is_name(&c.name) {
        return None;
    }
    let lines: Vec<u8> =
        c.interrupts.iter().flat_map(|i| interrupt_of(i.gsi, i.edge, i.active_low, false, false)).collect();
    let numbers = |pins: &mut dyn Iterator<Item = u16>| package(&pins.map(|p| int(p as u64)).collect::<Vec<_>>());
    let properties = package(&[
        package(&[string("veda,pins"), numbers(&mut c.pins.iter().copied())]),
        package(&[string("veda,interrupt-pins"), numbers(&mut c.interrupts.iter().map(|i| i.pin))]),
    ]);
    let dsd = package(&[vacpi::asm::buffer(&uuid(DEVICE_PROPERTIES).unwrap_or_default()), properties]);
    try_device(
        &c.name,
        &cat(&[
            &name("_HID", &string(GPIO_CONTROLLER)),
            &name("_UID", &int(c.uid as u64)),
            &name("_CRS", &resource_template(&lines)),
            &name("_DSD", &dsd),
        ]),
    )
}

/// The name of the device of function `devfn` (`S18` for 00:03.0).
fn function_name(devfn: u8) -> String {
    format!("S{:02X}", devfn)
}

/// An id: compressed when it is an EISA one (`PNP0C50`), else a string;
/// `None` if it has a NUL, which no AML string can hold.
fn id(text: &str) -> Option<Vec<u8>> {
    let b = text.as_bytes();
    let eisa_form =
        b.len() == 7 && b[..3].iter().all(u8::is_ascii_uppercase) && b[3..].iter().all(u8::is_ascii_hexdigit);
    if eisa_form { Some(eisa(text)) } else { try_string(text) }
}

/// A described device below the function at `parent`: `None` if its name
/// is no ACPI name. What of it cannot be described goes to `left_out`.
fn child(c: &Child, parent: &str, left_out: &mut Vec<String>) -> Option<Vec<u8>> {
    if !is_name(&c.name) {
        return None;
    }
    let path = format!("{parent}.{}", c.name);
    let mut say = |what: &str| left_out.push(format!("{path}: {what} cannot be described"));
    let mut body = Vec::new();
    if let Some(hid) = &c.hid {
        match id(hid) {
            Some(v) => body.extend_from_slice(&name("_HID", &v)),
            None => say(&format!("its _HID {hid:?}")),
        }
    }
    let mut cids = Vec::new();
    for cid in &c.cids {
        match id(cid) {
            Some(v) => cids.push(v),
            None => say(&format!("its _CID {cid:?}")),
        }
    }
    match cids.as_slice() {
        [] => {}
        [one] => body.extend_from_slice(&name("_CID", one)),
        many => body.extend_from_slice(&name("_CID", &package(many))),
    }
    if let Some(uid) = &c.uid {
        match uid.parse::<u64>().map(int).ok().or_else(|| try_string(uid)) {
            Some(v) => body.extend_from_slice(&name("_UID", &v)),
            None => say(&format!("its _UID {uid:?}")),
        }
    }
    if let Some(sub) = &c.sub {
        match try_string(sub) {
            Some(v) => body.extend_from_slice(&name("_SUB", &v)),
            None => say(&format!("its _SUB {sub:?}")),
        }
    }
    let mut descriptors = Vec::new();
    for r in &c.resources {
        match r {
            &Resource::I2c { address, speed_hz, ten_bit } => {
                descriptors.extend(i2c_descriptor(address, speed_hz, ten_bit, parent));
            }
            &Resource::Spi { chip_select, speed_hz, bits, cpol, cpha, cs_active_high, three_wire } => {
                let controller = parent.into();
                let spi = Spi { controller, chip_select, speed_hz, bits, cpol, cpha, cs_active_high, three_wire };
                descriptors.extend(spi_descriptor_of(&spi));
            }
            &Resource::Interrupt { gsi, edge, active_low, shared, wake } => {
                descriptors.extend(interrupt_of(gsi, edge, active_low, shared, wake));
            }
            Resource::Gpio(g) => match try_gpio_descriptor_of(g) {
                Some(d) => descriptors.extend(d),
                None => say(&format!("its GPIO connection to {:?}", g.controller)),
            },
        }
    }
    body.extend_from_slice(&name("_CRS", &resource_template(&descriptors)));
    if let (Some(address), Some(hid)) = (c.hid_descriptor, uuid(HID_OVER_I2C)) {
        body.extend_from_slice(&dsm(&hid, 1, &int(address as u64)));
    }
    data(&mut body, &c.data, &path, left_out);
    try_device(&c.name, &body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vacpi::NoMemory;
    use vacpi::aml::Namespace;
    use vacpi::device;
    use vacpi::name::Path;
    use vacpi::resource::Resource as Resource2;

    /// Where a test machine has its tables, and the room they have.
    pub(crate) const AT: u64 = 0xE_0000;
    const ROOM: usize = 0x2_0000;

    fn function(devfn: u8, intx: Option<(u8, u32)>) -> Function {
        Function { devfn, intx, data: Vec::new(), children: Vec::new() }
    }

    #[test]
    fn the_tables_add_up() {
        let functions = [function(0x18, Some((1, 22))), function(0x80, None)];
        let m = Machine {
            functions: &functions,
            gpio: &[],
            platform: &Platform::default(),
            at: AT,
            pci_low: 0xC000_0000..0xFEC0_0000,
            pci_high: 0..1,
            firmware_tables: &[],
        };
        let t = describe(&m).tables;
        assert!(t.len() <= ROOM);
        let sum = |b: &[u8]| b.iter().fold(0u8, |a, &x| a.wrapping_add(x));
        assert_eq!((&t[..8], sum(&t[..20]), sum(&t[..36])), (&b"RSD PTR "[..], 0, 0));
        // Each table sums to zero over its length.
        for at in [48, 96] {
            let len = u32::from_le_bytes(t[at + 4..at + 8].try_into().unwrap()) as usize;
            assert_eq!(sum(&t[at..at + len]), 0);
        }
        assert_eq!(&t[96..100], b"FACP");
        let dsdt = (u64::from_le_bytes(t[96 + 140..96 + 148].try_into().unwrap()) - AT) as usize;
        assert_eq!(&t[dsdt..dsdt + 4], b"DSDT");

        // What an OS finds in the DSDT.
        let len = u32::from_le_bytes(t[dsdt + 4..dsdt + 8].try_into().unwrap()) as usize;
        let mut ns = Namespace::new();
        ns.load(&t[dsdt + HEADER..dsdt + len], &NoMemory).unwrap();
        let roots = device::pci_roots(&ns, &NoMemory);
        assert_eq!(roots.len(), 1);
        let intx = |slot| device::pci_interrupt(&ns, &roots, (0, slot), 1, &NoMemory);
        assert_eq!(intx(3), Some(device::IntxRoute { gsi: 22, level: true, active_low: true }));
        assert_eq!(intx(16), None);
        let at = |p| Path::parse(p).unwrap();
        assert_eq!(device::pci_companion(&ns, &roots, (0, 16, 0), &NoMemory), Some(at("\\_SB.PCI0.S80")));
    }

    #[test]
    fn a_touchpad_below_its_i2c_controller() {
        // As the Zenbook's: an I2C HID touchpad at 0x15 on I2C1, its
        // interrupt GSI 40; the controller's fast-mode timing.
        let touchpad = Child {
            name: String::from("ETPD"),
            hid: Some(String::from("ASUP1413")),
            cids: alloc::vec![String::from("PNP0C50")],
            uid: Some(String::from("1")),
            sub: None,
            resources: alloc::vec![
                Resource::I2c { address: 0x15, speed_hz: 400_000, ten_bit: false },
                Resource::Interrupt { gsi: 40, edge: false, active_low: true, shared: false, wake: true },
            ],
            hid_descriptor: Some(0x20),
            data: Vec::new(),
        };
        let fmcn = package(&[int(0x64), int(0xD6), int(0x1C)]);
        let i2c = Function {
            devfn: 0xA9,
            intx: Some((2, 52)),
            data: alloc::vec![(String::from("FMCN"), fmcn)],
            children: alloc::vec![touchpad],
        };
        let functions = [i2c];
        let m = Machine {
            functions: &functions,
            gpio: &[],
            platform: &Platform::default(),
            at: AT,
            pci_low: 0xC000_0000..0xFEC0_0000,
            pci_high: 0..1,
            firmware_tables: &[],
        };
        let t = describe(&m).tables;
        let dsdt = (u64::from_le_bytes(t[96 + 140..96 + 148].try_into().unwrap()) - AT) as usize;
        let len = u32::from_le_bytes(t[dsdt + 4..dsdt + 8].try_into().unwrap()) as usize;
        let mut ns = Namespace::new();
        ns.load(&t[dsdt + HEADER..dsdt + len], &NoMemory).unwrap();
        let at = |p| Path::parse(p).unwrap();
        let tp = at("\\_SB.PCI0.SA9.ETPD");
        let id = device::identify(&ns, &tp, &NoMemory);
        assert!(id.is("ASUP1413") && id.is("PNP0C50"));
        assert_eq!(id.uid.as_deref(), Some("1"));
        let r = device::resources(&ns, &tp, &NoMemory).unwrap();
        assert!(matches!(&r[0], Resource2::I2c(i) if i.address == 0x15 && i.controller == "\\_SB.PCI0.SA9"));
        assert!(
            matches!(&r[1], Resource2::Irq { irqs, edge: false, active_low: true, wake: true, .. } if irqs == &[40])
        );
        let hid = vacpi::value::Value::Buffer(uuid(HID_OVER_I2C).unwrap().to_vec());
        let args = [
            hid,
            vacpi::value::Value::Integer(1),
            vacpi::value::Value::Integer(1),
            vacpi::value::Value::Package(Vec::new()),
        ];
        assert_eq!(
            ns.evaluate(&at("\\_SB.PCI0.SA9.ETPD._DSM"), &args, &NoMemory),
            Ok(vacpi::value::Value::Integer(0x20))
        );
        let timing = ns.evaluate(&at("\\_SB.PCI0.SA9.FMCN"), &[], &NoMemory).unwrap();
        assert!(matches!(timing, vacpi::value::Value::Package(p) if p.len() == 3));
    }

    #[test]
    fn amplifiers_below_their_spi_controller_and_their_pins() {
        // As the Zenbook's: two CS35L41 on SPI1 (00:1e.3), the second's chip
        // select, their reset and speaker id on GPIO pins, their
        // interrupt a pin's, on the platform's line 256.
        let pin = |interrupt: bool, pin: u16| Gpio {
            interrupt,
            pins: alloc::vec![pin],
            controller: String::from("\\_SB.GPI0"),
            pull: 0,
            restriction: 0,
            edge: false,
            polarity: if interrupt { 1 } else { 0 },
            shared: false,
            wake: false,
            debounce: 0,
        };
        let spi = |cs| Resource::Spi {
            chip_select: cs,
            speed_hz: 4_000_000,
            bits: 8,
            cpol: false,
            cpha: false,
            cs_active_high: false,
            three_wire: false,
        };
        let amps = Child {
            name: String::from("SPK1"),
            hid: Some(String::from("CSC3551")),
            cids: Vec::new(),
            uid: Some(String::from("1")),
            sub: Some(String::from("10431F62")),
            resources: alloc::vec![
                spi(0),
                spi(1),
                Resource::Gpio(pin(false, 23)),
                Resource::Gpio(pin(false, 305)),
                Resource::Gpio(pin(false, 302)),
                Resource::Gpio(pin(true, 303)),
            ],
            hid_descriptor: None,
            data: Vec::new(),
        };
        let functions = [Function { devfn: 0xF3, intx: Some((4, 37)), data: Vec::new(), children: alloc::vec![amps] }];
        let gpio = [GpioController {
            name: String::from("GPI0"),
            uid: 0,
            pins: alloc::vec![23, 302, 303, 305],
            interrupts: alloc::vec![GpioInterrupt { pin: 303, gsi: 256, edge: false, active_low: true }],
        }];
        let m = Machine {
            functions: &functions,
            gpio: &gpio,
            platform: &Platform::default(),
            at: AT,
            pci_low: 0xC000_0000..0xFEC0_0000,
            pci_high: 0..1,
            firmware_tables: &[],
        };
        let t = describe(&m).tables;
        let dsdt = (u64::from_le_bytes(t[96 + 140..96 + 148].try_into().unwrap()) - AT) as usize;
        let len = u32::from_le_bytes(t[dsdt + 4..dsdt + 8].try_into().unwrap()) as usize;
        let mut ns = Namespace::new();
        ns.load(&t[dsdt + HEADER..dsdt + len], &NoMemory).unwrap();
        let at = |p| Path::parse(p).unwrap();
        let r = device::resources(&ns, &at("\\_SB.PCI0.SF3.SPK1"), &NoMemory).unwrap();
        assert!(matches!(&r[1], Resource2::Spi(s) if s.chip_select == 1 && s.controller == "\\_SB.PCI0.SF3"));
        let gpios: Vec<_> =
            r[2..].iter().filter_map(|r| if let Resource2::Gpio(g) = r { Some(g.clone()) } else { None }).collect();
        assert_eq!(gpios, [pin(false, 23), pin(false, 305), pin(false, 302), pin(true, 303)]);
        assert_eq!(device::identify(&ns, &at("\\_SB.PCI0.SF3.SPK1"), &NoMemory).sub.as_deref(), Some("10431F62"));
        // The controller: its id and number, its pins, its line.
        let controller = at("\\_SB.GPI0");
        let id = device::identify(&ns, &controller, &NoMemory);
        assert_eq!((id.hid.as_deref(), id.uid.as_deref()), (Some(GPIO_CONTROLLER), Some("0")));
        let lines = device::resources(&ns, &controller, &NoMemory).unwrap();
        assert!(matches!(&lines[..], [Resource2::Irq { irqs, edge: false, active_low: true, .. }] if irqs == &[256]));
        use vacpi::value::Value;
        let Ok(Value::Package(dsd)) = ns.evaluate(&at("\\_SB.GPI0._DSD"), &[], &NoMemory) else { panic!() };
        assert_eq!(dsd[0], Value::Buffer(uuid(DEVICE_PROPERTIES).unwrap().to_vec()));
        let numbers = |v: &[u16]| Value::Package(v.iter().map(|&p| Value::Integer(p as u64)).collect());
        assert_eq!(
            dsd[1],
            Value::Package(alloc::vec![
                Value::Package(alloc::vec![Value::String("veda,pins".into()), numbers(&[23, 302, 303, 305])]),
                Value::Package(alloc::vec![Value::String("veda,interrupt-pins".into()), numbers(&[303])]),
            ])
        );
    }

    #[test]
    fn what_cannot_be_described_is_left_out_and_said() {
        // What devmgr says of the PC, as text: names that are no ACPI
        // names, ids with NULs, a connection of more pins than a descriptor
        // holds, a pin that is no INTx's; and a controller of 300 pins.
        let many_pins = Gpio {
            interrupt: false,
            pins: alloc::vec![1; 40_000],
            controller: String::from("\\_SB.GPI0"),
            pull: 0,
            restriction: 0,
            edge: false,
            polarity: 0,
            shared: false,
            wake: false,
            debounce: 0,
        };
        let touchpad = Child {
            name: String::from("TPD0"),
            hid: Some(String::from("ELAN\0")),
            cids: alloc::vec![String::from("PNP0C50"), String::from("A\0B")],
            uid: Some(String::from("x\0")),
            sub: None,
            resources: alloc::vec![
                Resource::Gpio(many_pins),
                Resource::I2c { address: 0x15, speed_hz: 400_000, ten_bit: false }
            ],
            hid_descriptor: None,
            data: alloc::vec![(String::from("BAD-"), int(1)), (String::from("_DSD"), int(2))],
        };
        let unnamed = Child { name: String::from("toolong"), ..touchpad.clone() };
        let functions = [Function {
            devfn: 0x18,
            intx: Some((0, 22)),
            data: alloc::vec![(String::from("\\XYZ"), int(3))],
            children: alloc::vec![touchpad, unnamed],
        }];
        let gpio = [
            GpioController { name: String::from("GP100"), uid: 256, pins: alloc::vec![1], interrupts: Vec::new() },
            GpioController { name: String::from("GPI0"), uid: 0, pins: (0..300).collect(), interrupts: Vec::new() },
        ];
        let m = Machine {
            functions: &functions,
            gpio: &gpio,
            platform: &Platform::default(),
            at: AT,
            pci_low: 0xC000_0000..0xFEC0_0000,
            pci_high: 0..1,
            firmware_tables: &[],
        };
        let d = describe(&m);
        assert_eq!(
            d.left_out,
            [
                "00:03.0's INTx: pin 0 is none of INTA# to INTD#",
                "\\_SB.PCI0.S18's data \"\\\\XYZ\": no ACPI name",
                "\\_SB.PCI0.S18.TPD0: its _HID \"ELAN\\0\" cannot be described",
                "\\_SB.PCI0.S18.TPD0: its _CID \"A\\0B\" cannot be described",
                "\\_SB.PCI0.S18.TPD0: its _UID \"x\\0\" cannot be described",
                "\\_SB.PCI0.S18.TPD0: its GPIO connection to \"\\\\_SB.GPI0\" cannot be described",
                "\\_SB.PCI0.S18.TPD0's data \"BAD-\": no ACPI name",
                "a device below \\_SB.PCI0.S18: \"toolong\" is no ACPI name",
                "the GPIO controller \"GP100\": its name is no ACPI name",
            ]
        );
        // The rest is there, as an OS reads it.
        let t = &d.tables;
        let dsdt = (u64::from_le_bytes(t[96 + 140..96 + 148].try_into().unwrap()) - AT) as usize;
        let len = u32::from_le_bytes(t[dsdt + 4..dsdt + 8].try_into().unwrap()) as usize;
        let mut ns = Namespace::new();
        ns.load(&t[dsdt + HEADER..dsdt + len], &NoMemory).unwrap();
        let at = |p| Path::parse(p).unwrap();
        let tp = at("\\_SB.PCI0.S18.TPD0");
        let id = device::identify(&ns, &tp, &NoMemory);
        assert_eq!((id.hid, id.cids, id.uid), (None, alloc::vec![String::from("PNP0C50")], None));
        let r = device::resources(&ns, &tp, &NoMemory).unwrap();
        assert!(matches!(&r[..], [Resource2::I2c(i)] if i.address == 0x15));
        assert_eq!(ns.evaluate(&at("\\_SB.PCI0.S18.TPD0._DSD"), &[], &NoMemory), Ok(vacpi::value::Value::Integer(2)));
        assert!(ns.get(&at("\\_SB.PCI0._PRT")).is_none());
        let Ok(vacpi::value::Value::Package(dsd)) = ns.evaluate(&at("\\_SB.GPI0._DSD"), &[], &NoMemory) else {
            panic!()
        };
        let vacpi::value::Value::Package(properties) = &dsd[1] else { panic!() };
        let vacpi::value::Value::Package(pins) = &properties[0] else { panic!() };
        assert!(matches!(&pins[1], vacpi::value::Value::Package(p) if p.len() == 300));
    }

    #[test]
    fn the_firmwares_tables_come_whole() {
        let nhlt = table(b"NHLT", 0, b"TESTNHLT", &[1, 2, 3, 4, 5]);
        assert!(is_table(&nhlt));
        let (mut cut, mut wrong) = (nhlt.clone(), nhlt.clone());
        cut.pop();
        wrong[40] ^= 1;
        assert!(!is_table(&cut) && !is_table(&wrong) && !is_table(&nhlt[..20]));

        let functions = [function(0xF8, None)];
        let firmware_tables = [nhlt.clone()];
        let m = Machine {
            functions: &functions,
            gpio: &[],
            platform: &Platform::default(),
            at: AT,
            pci_low: 0xC000_0000..0xFEC0_0000,
            pci_high: 0..1,
            firmware_tables: &firmware_tables,
        };
        let t = describe(&m).tables;
        let u64_at = |at: usize| u64::from_le_bytes(t[at..at + 8].try_into().unwrap());
        // The XSDT lists the FADT, then the firmware's table, which is as
        // it was; every table adds up.
        let xsdt = &t[48..];
        assert_eq!(&xsdt[..4], b"XSDT");
        assert_eq!(u32::from_le_bytes(xsdt[4..8].try_into().unwrap()) as usize, HEADER + 16);
        let (fadt, listed) = ((u64_at(48 + HEADER) - AT) as usize, (u64_at(48 + HEADER + 8) - AT) as usize);
        assert_eq!(&t[fadt..fadt + 4], b"FACP");
        assert_eq!(&t[listed..listed + nhlt.len()], &nhlt[..]);
        assert!(listed % 16 == 0 && t.len() == listed + nhlt.len());
        let dsdt = (u64::from_le_bytes(t[fadt + 140..fadt + 148].try_into().unwrap()) - AT) as usize;
        for at in [48, fadt, dsdt, listed] {
            let len = u32::from_le_bytes(t[at + 4..at + 8].try_into().unwrap()) as usize;
            assert!(is_table(&t[at..at + len]), "the table at {at:#x}");
        }
    }
}
