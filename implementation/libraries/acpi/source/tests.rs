// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The crate's tests: resource templates, names, loading, initialising and
//! evaluating AML written with the assembler (as firmware writes it), and
//! the tables from the RSDP.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use std::collections::BTreeMap;

use crate::aml::{Error, Namespace, Object};
use crate::asm::*;
use crate::device::{self, ResourcesError, eisa_id};
use crate::name::{NameString, Path};
use crate::resource::{self, Gpio, Resource, Spi};
use crate::value::Value;
use crate::{Memory, PciFunction};

/// Firmware memory: bytes at given addresses, nothing elsewhere; and the
/// configuration space of given PCI functions (zeros but what is put).
#[derive(Default)]
struct Ram {
    memory: BTreeMap<u64, u8>,
    pci: BTreeMap<PciFunction, [u8; 256]>,
}

impl Ram {
    fn put(&mut self, address: u64, bytes: &[u8]) {
        for (i, &b) in bytes.iter().enumerate() {
            self.memory.insert(address + i as u64, b);
        }
    }

    /// `bytes` at `offset` in the configuration space of function
    /// `bus:device.function` of segment 0.
    fn put_pci(&mut self, (bus, device, function): (u8, u8, u8), offset: usize, bytes: &[u8]) {
        let space = self.pci.entry(PciFunction { segment: 0, bus, device, function }).or_insert([0; 256]);
        space[offset..offset + bytes.len()].copy_from_slice(bytes);
    }
}

impl Memory for Ram {
    fn read(&self, address: u64, buf: &mut [u8]) -> bool {
        for (i, b) in buf.iter_mut().enumerate() {
            match self.memory.get(&(address + i as u64)) {
                Some(&v) => *b = v,
                None => return false,
            }
        }
        true
    }

    fn read_pci(&self, function: PciFunction, offset: u16, buf: &mut [u8]) -> bool {
        let at = offset as usize;
        match self.pci.get(&function).and_then(|space| space.get(at..at + buf.len())) {
            Some(bytes) => {
                buf.copy_from_slice(bytes);
                true
            }
            None => false,
        }
    }
}

fn path(text: &str) -> Path {
    Path::parse(text).unwrap()
}

fn eval(ns: &Namespace, p: &str, memory: &dyn Memory) -> Result<Value, Error> {
    ns.evaluate(&path(p), &[], memory)
}

// --- Resource templates ----------------------------------------------------

#[test]
fn amplifier_resources() {
    let template = cat(&[
        &spi_descriptor(0, 4_000_000, "\\_SB.PC00.SPI1"),
        &spi_descriptor(1, 4_000_000, "\\_SB.PC00.SPI1"),
        &gpio_descriptor(false, 2, 1, 0, 23, "\\_SB.GPI0"),
        &gpio_descriptor(false, 2, 2, 0, 305, "\\_SB.GPI0"),
        &gpio_descriptor(true, 0x0D, 1, 100, 303, "\\_SB.GPI0"),
        &END_TAG,
    ]);
    let r = resource::parse(&template).unwrap();
    assert_eq!(r.len(), 5);
    assert_eq!(
        r[1],
        Resource::Spi(Spi {
            controller: "\\_SB.PC00.SPI1".into(),
            chip_select: 1,
            speed_hz: 4_000_000,
            bits: 8,
            cpol: false,
            cpha: false,
            cs_active_high: false,
            three_wire: false,
        })
    );
    let Resource::Gpio(cs) = &r[2] else { panic!("{:?}", r[2]) };
    assert_eq!((cs.interrupt, cs.pins.as_slice(), cs.restriction, cs.pull), (false, &[23u16][..], 2, 1));
    assert_eq!(cs.controller, "\\_SB.GPI0");
    let Resource::Gpio(irq) = &r[4] else { panic!() };
    assert_eq!(
        *irq,
        Gpio {
            interrupt: true,
            pins: vec![303],
            controller: "\\_SB.GPI0".into(),
            pull: 1,
            restriction: 0,
            edge: true,
            polarity: 2,
            shared: true,
            wake: false,
            debounce: 100,
        }
    );
    // A template cut short is an error, not a panic.
    assert!(resource::parse(&template[..30]).is_err());
}

#[test]
fn memory_interrupt_and_window_resources() {
    let template = [
        // Interrupt (ResourceConsumer, Level, ActiveLow, Shared) {14}
        0x89, 0x06, 0x00, 0x0D, 0x01, 0x0E, 0x00, 0x00, 0x00,
        // Memory32Fixed (ReadWrite, 0xFD6E0000, 0x10000)
        0x86, 0x09, 0x00, 0x01, 0x00, 0x00, 0x6E, 0xFD, 0x00, 0x00, 0x01, 0x00,
        // IO (Decode16, 0x60, 0x60, 1, 1)
        0x47, 0x01, 0x60, 0x00, 0x60, 0x00, 0x01, 0x01, // IRQNoFlags () {1}
        0x22, 0x02, 0x00, // WordBusNumber (0..0xFF)
        0x88, 0x0D, 0x00, 0x02, 0x0C, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFF, 0x00, 0x00, 0x00, 0x00, 0x01, 0x79, 0x00,
    ];
    let r = resource::parse(&template).unwrap();
    assert_eq!(r[0], Resource::Irq { irqs: vec![14], edge: false, active_low: true, shared: true, wake: false });
    assert_eq!(r[1], Resource::Memory { base: 0xFD6E_0000, length: 0x1_0000, writable: true });
    assert_eq!(r[2], Resource::Io { base: 0x60, length: 1 });
    assert_eq!(r[3], Resource::Irq { irqs: vec![1], edge: true, active_low: false, shared: false, wake: false });
    assert_eq!(r[4], Resource::Window { kind: 2, min: 0, max: 0xFF, translation: 0, length: 0x100 });
}

// --- Names -----------------------------------------------------------------

#[test]
fn paths_and_names() {
    let p = path("\\_SB.PC00.SPI1");
    assert_eq!(alloc::format!("{}", p), "\\_SB.PC00.SPI1");
    assert_eq!(p.0[0], *b"_SB_");
    assert_eq!(p.parent().unwrap(), path("\\_SB.PC00"));
    assert_eq!(alloc::format!("{}", Path::root()), "\\");
    let n = NameString::parse("^^GPI0").unwrap();
    assert_eq!(n.in_scope(&path("\\_SB.PC00.SPI1")), path("\\_SB.GPI0"));
    assert!(NameString::parse("TOOLONG").is_none());
    assert_eq!(eisa_id(0x080A_D041), "PNP0A08");
    assert_eq!(eisa_id(0x0C0C_D041), "PNP0C0C");
}

// --- Loading and evaluating -------------------------------------------------

/// A board like the laptop this was written for: firmware settings in a
/// memory region decide which devices exist and where the GPIO
/// controller's registers are.
fn board() -> (Namespace, Ram) {
    let gpio_crs = method(
        "_CRS",
        0,
        &cat(&[
            &name(
                "RBFL",
                &buffer(&[
                    0x86, 0x09, 0x00, 0x01, 0, 0, 0, 0, 0x00, 0x00, 0x01, 0x00, //
                    0x86, 0x09, 0x00, 0x01, 0, 0, 0, 0, 0x00, 0x00, 0x01, 0x00, //
                    0x79, 0x00,
                ]),
            ),
            &cat(&[&[0x8A], &nm("RBFL"), &int(4), &nm("CML0")]),
            &store(&add(&nm("SBRG"), &int(0x006E_0000)), &nm("CML0")),
            &cat(&[&[0x8A], &nm("RBFL"), &int(16), &nm("CML4")]),
            &store(&add(&nm("SBRG"), &int(0x006A_0000)), &nm("CML4")),
            &ret(&nm("RBFL")),
        ]),
    );
    let gpio = device(
        "GPI0",
        &cat(&[
            &method(
                "_HID",
                0,
                &cat(&[&if_(&lequal(&nm("GPHD"), &int(1)), &ret(&string("PNP0C02"))), &ret(&string("INTC1055"))]),
            ),
            &gpio_crs,
        ]),
    );
    let spi = device(
        "SPI1",
        &cat(&[
            &if_(&lequal(&nm("SM01"), &int(2)), &name("_STA", &int(8))),
            &if_(&lequal(&nm("SM01"), &int(1)), &method("_ADR", 0, &ret(&int(0x001E_0003)))),
        ]),
    );
    // A device whose condition reads memory nobody can read: dropped.
    let unknown = if_(&lequal(&nm("\\NOPE"), &int(1)), &device("GONE", &name("_ADR", &int(1))));
    let dsdt = cat(&[
        &region("GNVS", 0, &int(0x5000), &int(0x10)),
        &field("GNVS", 0x10, &[("SM01", 8), ("GPHD", 8), ("", 16), ("SBRG", 32)]),
        &region("BAD", 0, &int(0x9000), &int(4)),
        &field("BAD", 0x10, &[("NOPE", 8)]),
        &scope(
            "\\_SB",
            &cat(&[
                &device("PC00", &cat(&[&name("_HID", &[0x0C, 0x41, 0xD0, 0x0A, 0x08]), &name("_ADR", &int(0)), &spi])),
                &gpio,
                &unknown,
            ]),
        ),
    ]);
    let ssdt = cat(&[
        // If (Zero) { External (...) }, as some compilers emit.
        &if_(&[0x00], &cat(&[&[0x15], &nm("\\_SB.GPI0"), &[6, 0]])),
        &scope(
            "\\_SB.PC00.SPI1",
            &device(
                "SPK1",
                &cat(&[
                    &name("_HID", &string("CSC3551")),
                    &name("_SUB", &string("10431F62")),
                    &name("_UID", &int(1)),
                    &method(
                        "_CRS",
                        0,
                        &cat(&[
                            &name(
                                "SBUF",
                                &buffer(&cat(&[
                                    &spi_descriptor(0, 4_000_000, "\\_SB.PC00.SPI1"),
                                    &gpio_descriptor(false, 2, 2, 0, 305, "\\_SB.GPI0"),
                                    &END_TAG,
                                ])),
                            ),
                            &ret(&nm("SBUF")),
                        ]),
                    ),
                    &method("_STA", 0, &ret(&int(0x0F))),
                ]),
            ),
        ),
    ]);
    let mut ram = Ram::default();
    // SM01 = 1 (the SPI controller on PCI), GPHD = 0, SBRG = 0xFD000000.
    ram.put(0x5000, &[1, 0, 0, 0, 0x00, 0x00, 0x00, 0xFD, 0, 0, 0, 0, 0, 0, 0, 0]);
    let mut ns = Namespace::new();
    ns.load(&dsdt, &ram).unwrap();
    ns.load(&ssdt, &ram).unwrap();
    (ns, ram)
}

#[test]
fn devices_of_a_board() {
    let (ns, ram) = board();
    let devices: Vec<String> = ns.devices().map(|p| alloc::format!("{}", p)).collect();
    assert_eq!(devices, ["\\_SB.GPI0", "\\_SB.PC00", "\\_SB.PC00.SPI1", "\\_SB.PC00.SPI1.SPK1"]);
    assert_eq!(ns.skipped, 1);

    let root = device::identify(&ns, &path("\\_SB.PC00"), &ram);
    assert_eq!((root.hid.as_deref(), root.adr), (Some("PNP0A08"), Some(0)));
    // _ADR exists because SM01 is 1; _STA does not (SM01 is not 2).
    let spi = device::identify(&ns, &path("\\_SB.PC00.SPI1"), &ram);
    assert_eq!((spi.adr, spi.status), (Some(0x001E_0003), 0xF));

    let amp = device::identify(&ns, &path("\\_SB.PC00.SPI1.SPK1"), &ram);
    assert_eq!(amp.hid.as_deref(), Some("CSC3551"));
    assert_eq!((amp.sub.as_deref(), amp.uid.as_deref()), (Some("10431F62"), Some("1")));
    assert!(amp.present());
    let r = device::resources(&ns, &path("\\_SB.PC00.SPI1.SPK1"), &ram).unwrap();
    assert!(matches!(&r[1], Resource::Gpio(g) if g.pins == [305] && g.controller == "\\_SB.GPI0"));
    assert_eq!(device::children(&ns, &path("\\_SB.PC00.SPI1")), [path("\\_SB.PC00.SPI1.SPK1")]);

    // The GPIO controller: its id and registers come from the settings.
    let gpio = device::identify(&ns, &path("\\_SB.GPI0"), &ram);
    assert_eq!(gpio.hid.as_deref(), Some("INTC1055"));
    let r = device::resources(&ns, &path("\\_SB.GPI0"), &ram).unwrap();
    assert_eq!(r[0], Resource::Memory { base: 0xFD6E_0000, length: 0x1_0000, writable: true });
    assert_eq!(r[1], Resource::Memory { base: 0xFD6A_0000, length: 0x1_0000, writable: true });
    // The firmware's own buffer is not changed by evaluating _CRS.
    assert!(matches!(ns.get(&path("\\_SB.GPI0._CRS")), Some(Object::Method { .. })));
    // Names resolve the way a resource source names its controller.
    let n = NameString::parse("\\_SB.GPI0").unwrap();
    assert_eq!(ns.resolve(&path("\\_SB.PC00.SPI1.SPK1"), &n), Some(path("\\_SB.GPI0")));
}

#[test]
fn arithmetic_loops_and_calls() {
    // Method (SUM, 2) { Local0 = 0; Local1 = Arg0; While (Local1 <= Arg1) { Local0 += Local1; Local1++ }; Return (Local0) }
    let sum = method(
        "SUM",
        2,
        &cat(&[
            &store(&int(0), &[LOCAL0]),
            &store(&[ARG0], &[LOCAL1]),
            &while_(
                &[0x92, 0x94, LOCAL1, ARG1],
                &cat(&[&binary(0x72, &[LOCAL0], &[LOCAL1], &[LOCAL0]), &[0x75, LOCAL1]]),
            ),
            &ret(&[LOCAL0]),
        ]),
    );
    // Method (TEST) { Return (SUM (1, 10) * 2 + (0xF0 >> 4)) }
    let test = method(
        "TEST",
        0,
        &ret(&add(
            &binary(0x77, &cat(&[&nm("SUM"), &int(1), &int(10)]), &int(2), &[0]),
            &binary(0x7A, &int(0xF0), &int(4), &[0]),
        )),
    );
    // Method (OSIW) { If (_OSI ("Windows 2015")) { If (_OSI ("Linux")) { Return (2) } Return (1) } Return (0) }
    let osi = method(
        "OSIW",
        0,
        &cat(&[
            &if_(
                &cat(&[&nm("_OSI"), &string("Windows 2015")]),
                &cat(&[&if_(&cat(&[&nm("_OSI"), &string("Linux")]), &ret(&int(2))), &ret(&int(1))]),
            ),
            &ret(&int(0)),
        ]),
    );
    // Method (LOOP) { While (One) { } }
    let forever = method("LOOP", 0, &while_(&[0x01], &[]));
    // Method (PKG) { Name (P, Package () { 5, "x", Buffer () {1, 2} }); Store (7, Index (P, 0)); Return (DerefOf (Index (P, 0)) + SizeOf (P)) }
    let pkg_test = method(
        "PKG",
        0,
        &cat(&[
            &name("P___", &package(&[int(5), string("x"), buffer(&[1, 2])])),
            &store(&int(7), &cat(&[&[0x88], &nm("P___"), &int(0), &[0]])),
            &ret(&add(&cat(&[&[0x83, 0x88], &nm("P___"), &int(0), &[0]]), &cat(&[&[0x87], &nm("P___")]))),
        ]),
    );
    // Method (ELSE, 1) { If (Arg0 == 3) { Return ("three") } Else { Return ("other") } }
    let else_test = method(
        "ELSE",
        1,
        &cat(&[&if_(&lequal(&[ARG0], &int(3)), &ret(&string("three"))), &else_(&ret(&string("other")))]),
    );
    let code = cat(&[&sum, &test, &osi, &forever, &pkg_test, &else_test]);
    let mut ns = Namespace::new();
    ns.load(&code, &crate::NoMemory).unwrap();
    let m = &crate::NoMemory;
    assert_eq!(eval(&ns, "\\TEST", m), Ok(Value::Integer(55 * 2 + 15)));
    assert_eq!(eval(&ns, "\\OSIW", m), Ok(Value::Integer(1)));
    assert_eq!(eval(&ns, "\\LOOP", m), Err(Error::Limit));
    assert_eq!(eval(&ns, "\\PKG_", m), Ok(Value::Integer(7 + 3)));
    assert_eq!(ns.evaluate(&path("\\ELSE"), &[Value::Integer(3)], m), Ok(Value::String("three".into())));
    assert_eq!(ns.evaluate(&path("\\ELSE"), &[Value::Integer(4)], m), Ok(Value::String("other".into())));
}

#[test]
fn stores_stay_inside_an_evaluation() {
    // Name (CNT, 5); Method (BUMP) { CNT++; Return (CNT) }
    let code = cat(&[&name("CNT_", &int(5)), &method("BUMP", 0, &cat(&[&[0x75], &nm("CNT_"), &ret(&nm("CNT_"))]))]);
    let mut ns = Namespace::new();
    ns.load(&code, &crate::NoMemory).unwrap();
    assert_eq!(eval(&ns, "\\BUMP", &crate::NoMemory), Ok(Value::Integer(6)));
    assert_eq!(eval(&ns, "\\BUMP", &crate::NoMemory), Ok(Value::Integer(6)));
    assert_eq!(eval(&ns, "\\CNT_", &crate::NoMemory), Ok(Value::Integer(5)));
}

#[test]
fn spi_and_gpio_connections_read_back() {
    // The amplifiers' connections, as a guest's tables describe them.
    let spi = Spi {
        controller: "\\_SB.PCI0.SF3".into(),
        chip_select: 1,
        speed_hz: 4_000_000,
        bits: 8,
        cpol: true,
        cpha: true,
        cs_active_high: true,
        three_wire: false,
    };
    let reset = Gpio {
        interrupt: false,
        pins: alloc::vec![305, 23],
        controller: "\\_SB.GPI0".into(),
        pull: 2,
        restriction: 2,
        edge: false,
        polarity: 0,
        shared: true,
        wake: false,
        debounce: 0,
    };
    let irq = Gpio {
        interrupt: true,
        pins: alloc::vec![303],
        controller: "\\_SB.GPI0".into(),
        pull: 1,
        restriction: 0,
        edge: true,
        polarity: 2,
        shared: false,
        wake: true,
        debounce: 100,
    };
    let template = cat(&[&spi_descriptor_of(&spi), &gpio_descriptor_of(&reset), &gpio_descriptor_of(&irq), &END_TAG]);
    let r = resource::parse(&template).unwrap();
    assert_eq!(r, [Resource::Spi(spi), Resource::Gpio(reset), Resource::Gpio(irq)]);
}

#[test]
fn the_descriptors_veda_writes_read_back() {
    let template = cat(&[
        &io(0x60, 1),
        &irq_no_flags(12),
        &word_bus_number(0, 0),
        &dword_memory(0xC000_0000, 0xFEBF_FFFF),
        &qword_memory(0x20_0000_0000, 0x3F_FFFF_FFFF),
        &END_TAG,
    ]);
    let r = resource::parse(&template).unwrap();
    assert_eq!(r[0], Resource::Io { base: 0x60, length: 1 });
    assert!(matches!(&r[1], Resource::Irq { irqs, edge: true, active_low: false, .. } if irqs == &[12]));
    assert_eq!(r[2], Resource::Window { kind: 2, min: 0, max: 0, translation: 0, length: 1 });
    assert_eq!(
        r[3],
        Resource::Window { kind: 0, min: 0xC000_0000, max: 0xFEBF_FFFF, translation: 0, length: 0x3EC0_0000 }
    );
    assert_eq!(
        r[4],
        Resource::Window { kind: 0, min: 0x20_0000_0000, max: 0x3F_FFFF_FFFF, translation: 0, length: 0x20_0000_0000 }
    );
}

#[test]
fn connections_and_interrupts_veda_writes_read_back() {
    let template = cat(&[
        &i2c_descriptor(0x15, 400_000, false, "\\_SB.PCI0.S29"),
        &interrupt_of(40, false, true, false, true),
        &END_TAG,
    ]);
    let r = resource::parse(&template).unwrap();
    assert_eq!(
        r[0],
        Resource::I2c(crate::resource::I2c {
            controller: String::from("\\_SB.PCI0.S29"),
            speed_hz: 400_000,
            address: 0x15,
            ten_bit: false,
        })
    );
    assert!(
        matches!(&r[1], Resource::Irq { irqs, edge: false, active_low: true, shared: false, wake: true } if irqs == &[40])
    );
}

#[test]
fn a_dsm_answers_what_hid_over_i2c_asks() {
    let hid = uuid("3cdff6f7-4267-4555-ad05-b30a3d8938de").unwrap();
    assert_eq!(hid[..4], [0xF7, 0xF6, 0xDF, 0x3C]);
    assert_eq!(hid[8..10], [0xAD, 0x05]);
    assert_eq!(uuid("3cdff6f7-4267-4555-ad05"), None);
    let aml = device("TPD0", &dsm(&hid, 1, &int(0x20)));
    let mut ns = Namespace::new();
    ns.load(&aml, &crate::NoMemory).unwrap();
    let call = |u: &[u8; 16], function: u64| {
        let args = [Value::Buffer(u.to_vec()), Value::Integer(1), Value::Integer(function), Value::Package(vec![])];
        ns.evaluate(&path("\\TPD0._DSM"), &args, &crate::NoMemory)
    };
    assert_eq!(call(&hid, 1), Ok(Value::Integer(0x20)));
    assert_eq!(call(&hid, 0), Ok(Value::Buffer(vec![3])));
    assert_eq!(call(&[0; 16], 1), Ok(Value::Buffer(vec![0])));
}

#[test]
fn constants_are_written_as_they_evaluate() {
    let v = Value::Package(vec![
        Value::Integer(0x264),
        Value::String(String::from("cirrus,dev-index")),
        Value::Buffer(vec![1, 2]),
    ]);
    let mut ns = Namespace::new();
    ns.load(&name("DATA", &constant(&v).unwrap()), &crate::NoMemory).unwrap();
    assert_eq!(eval(&ns, "\\DATA", &crate::NoMemory), Ok(v));
    assert_eq!(constant(&Value::Reference(path("\\_SB"))), None);
}

#[test]
fn pci_interrupts_in_apic_mode() {
    // As QEMU's q35 has it: `_PRT` answers for the model `_PIC` sets, its
    // routes through interrupt link devices defined after it (in APIC
    // mode, to the I/O APIC's inputs from 16, level-triggered and active
    // high); a route can name a GSI itself too.
    let gsi_link = |irq: u32| cat(&[&[0x89, 6, 0, 0x09, 1], &irq.to_le_bytes(), &END_TAG]);
    let aml = cat(&[
        &name("PICF", &int(0)),
        &method("_PIC", 1, &store(&[ARG0], &nm("PICF"))),
        &scope(
            "\\_SB",
            &cat(&[
                &device(
                    "PCI0",
                    &cat(&[
                        &name("_HID", &eisa("PNP0A08")),
                        &name("PRTP", &package(&[package(&[int(0x0002_FFFF), int(0), nm("LNKA"), int(0)])])),
                        &name(
                            "PRTA",
                            &package(&[
                                package(&[int(0x0002_FFFF), int(0), nm("GSIA"), int(0)]),
                                package(&[int(0x0003_FFFF), int(1), int(0), int(0x11)]),
                            ]),
                        ),
                        &method("_PRT", 0, &cat(&[&if_(&nm("PICF"), &ret(&nm("PRTA"))), &ret(&nm("PRTP"))])),
                    ]),
                ),
                &device("GSIA", &cat(&[&name("_HID", &eisa("PNP0C0F")), &name("_CRS", &buffer(&gsi_link(16)))])),
                // IRQ (Level, ActiveLow, Shared) {11}: the PIC's.
                &device(
                    "LNKA",
                    &cat(&[
                        &name("_HID", &eisa("PNP0C0F")),
                        &name("_CRS", &buffer(&[0x23, 0x00, 0x08, 0x18, 0x79, 0])),
                    ]),
                ),
            ]),
        ),
    ]);
    let mut ns = Namespace::new();
    ns.load(&aml, &crate::NoMemory).unwrap();
    let roots = device::pci_roots(&ns, &crate::NoMemory);
    let route = |slot, pin| device::pci_interrupt(&ns, &roots, (0, slot), pin, &crate::NoMemory);
    assert_eq!(route(2, 1), Some(device::IntxRoute { gsi: 16, level: true, active_low: false }));
    assert_eq!(route(3, 2), Some(device::IntxRoute { gsi: 0x11, level: true, active_low: true }));
    assert_eq!(route(3, 1), None);
    // Nothing the evaluation stored stays: the namespace is in PIC mode.
    assert_eq!(eval(&ns, "\\PICF", &crate::NoMemory), Ok(Value::Integer(0)));
}

#[test]
fn a_root_bridges_windows_from_the_host_bridge_and_the_firmwares_variables() {
    // As an Alder Lake laptop's firmware has them: the root bridge's _CRS
    // sizes its bus range by the host bridge's PCIEXBAR, read through a
    // PCI_Config region of the device at _ADR 0 below it (00:00.0), and
    // takes its memory windows from the firmware's variables, emptying the
    // 64-bit one when there is none.
    let template = cat(&[&word_bus_number(0, 0xFF), &dword_memory(0, 0xDFFF_FFFF), &qword_memory(0x1_0000, 0x1_FFFF)]);
    let create = |op: u8, offset: u64, field: &str| cat(&[&[op], &nm("BUF0"), &int(offset), &nm(field)]);
    let (word, dword, qword) = (0x8B, 0x8A, 0x8F);
    let shift_right = |a: &[u8], b: &[u8], target: &[u8]| binary(0x7A, a, b, target);
    let minus = |a: &[u8], n: u64| binary(0x74, a, &int(n), &[0]);
    let buses = shift_right(&[LOCAL0], &int(20), &[0]);
    let crs = method(
        "_CRS",
        0,
        &cat(&[
            &shift_right(&int(0x1000_0000), &nm("^MC.PXSZ"), &[LOCAL0]),
            &create(word, 10, "PBMX"),
            &store(&minus(&buses, 2), &nm("PBMX")),
            &create(word, 14, "PBLN"),
            &store(&minus(&buses, 1), &nm("PBLN")),
            &create(dword, 26, "M1MN"),
            &create(dword, 30, "M1MX"),
            &create(dword, 38, "M1LN"),
            &store(&nm("M32L"), &nm("M1LN")),
            &store(&nm("M32B"), &nm("M1MN")),
            &store(&minus(&add(&nm("M1MN"), &nm("M1LN")), 1), &nm("M1MX")),
            &if_(&lequal(&nm("M64L"), &int(0)), &cat(&[&create(qword, 80, "MSLN"), &store(&int(0), &nm("MSLN"))])),
            &else_(&cat(&[
                &create(qword, 56, "M2MN"),
                &create(qword, 64, "M2MX"),
                &create(qword, 80, "M2LN"),
                &store(&nm("M64L"), &nm("M2LN")),
                &store(&nm("M64B"), &nm("M2MN")),
                &store(&minus(&add(&nm("M2MN"), &nm("M2LN")), 1), &nm("M2MX")),
            ])),
            &ret(&nm("BUF0")),
        ]),
    );
    let dsdt = cat(&[
        &region("SANV", 0, &int(0x5000), &int(0x18)),
        &field("SANV", 0, &[("M32B", 32), ("M32L", 32), ("M64B", 64), ("M64L", 64)]),
        &scope(
            "\\_SB",
            &device(
                "PC00",
                &cat(&[
                    &name("_HID", &eisa("PNP0A08")),
                    &name("_CID", &eisa("PNP0A03")),
                    &name("_ADR", &int(0)),
                    &device(
                        "MC",
                        &cat(&[
                            &name("_ADR", &int(0)),
                            &region("HBUS", 2, &int(0), &int(0x100)),
                            &field("HBUS", 3, &[("", 0x60 * 8), ("PXEN", 1), ("PXSZ", 3)]),
                        ]),
                    ),
                    &name("BUF0", &resource_template(&template)),
                    &crs,
                ]),
            ),
        ),
    ]);
    let mut ram = Ram::default();
    // The windows Linux finds on the laptop, where the firmware keeps them.
    ram.put(0x5000, &0x6880_0000u32.to_le_bytes());
    ram.put(0x5004, &0x5780_0000u32.to_le_bytes());
    ram.put(0x5008, &0x40_0000_0000u64.to_le_bytes());
    ram.put(0x5010, &0x40_0000_0000u64.to_le_bytes());
    // PCIEXBAR on, 128 MiB: 128 buses.
    ram.put_pci((0, 0, 0), 0x60, &[0x03]);
    let mut ns = Namespace::new();
    ns.load(&dsdt, &ram).unwrap();
    let root = path("\\_SB.PC00");
    assert_eq!(
        device::pci_root_windows(&ns, &root, &ram),
        Ok(vec![0x6880_0000..0xC000_0000, 0x40_0000_0000..0x80_0000_0000])
    );
    let r = device::resources(&ns, &root, &ram).unwrap();
    assert_eq!(r[0], Resource::Window { kind: 2, min: 0, max: 0x7E, translation: 0, length: 0x7F });
    // No 64-bit window: the firmware empties it, which leaves it out.
    ram.put(0x5010, &0u64.to_le_bytes());
    let low = 0x6880_0000..0xC000_0000;
    assert_eq!(device::pci_root_windows(&ns, &root, &ram), Ok(Vec::from([low])));
    // Without the host bridge's registers, the windows are not known.
    assert_eq!(
        device::pci_root_windows(&ns, &root, &crate::NoMemory),
        Err(ResourcesError::Aml(Error::PciConfig(PciFunction::default(), 0x60)))
    );
}

#[test]
fn configuration_space_below_bridges() {
    // A PCI_Config region is the configuration space of the function at
    // the _ADR of the device it is in, on its root bridge's bus (_BBN), or
    // on the secondary bus of the bridge it is below; one in the root
    // bridge's own scope is the root's. The tables' conditions read them
    // as they load.
    let id = |r: &str, f: &str| cat(&[&region(r, 2, &int(0), &int(0x100)), &field(r, 2, &[(f, 16)])]);
    let dsdt = scope(
        "\\_SB",
        &device(
            "PC10",
            &cat(&[
                &name("_HID", &eisa("PNP0A08")),
                &name("_BBN", &int(0x10)),
                &id("OWNC", "RVID"),
                &device(
                    "RP01",
                    &cat(&[
                        &name("_ADR", &int(0x001C_0000)),
                        &id("RPCS", "BVID"),
                        &device(
                            "PXSX",
                            &cat(&[&name("_ADR", &int(0)), &id("PCFG", "DVID"), &method("VEND", 0, &ret(&nm("DVID")))]),
                        ),
                    ]),
                ),
                &if_(&lequal(&nm("\\_SB.PC10.RP01.PXSX.DVID"), &int(0x3333)), &device("SEEN", &name("_ADR", &int(1)))),
                &device("RP02", &cat(&[&name("_ADR", &int(0x001D_0000)), &id("RPCS", "BVID")])),
            ]),
        ),
    );
    let mut ram = Ram::default();
    ram.put_pci((0x10, 0, 0), 0, &[0x11, 0x11]);
    // The root port: a bridge to bus 0x13.
    ram.put_pci((0x10, 0x1C, 0), 0, &[0x22, 0x22]);
    ram.put_pci((0x10, 0x1C, 0), 0x0E, &[0x01]);
    ram.put_pci((0x10, 0x1C, 0), 0x19, &[0x13]);
    ram.put_pci((0x13, 0, 0), 0, &[0x33, 0x33]);
    let mut ns = Namespace::new();
    ns.load(&dsdt, &ram).unwrap();
    assert_eq!(eval(&ns, "\\_SB.PC10.RVID", &ram), Ok(Value::Integer(0x1111)));
    assert_eq!(eval(&ns, "\\_SB.PC10.RP01.BVID", &ram), Ok(Value::Integer(0x2222)));
    assert_eq!(eval(&ns, "\\_SB.PC10.RP01.PXSX.VEND", &ram), Ok(Value::Integer(0x3333)));
    assert!(ns.get(&path("\\_SB.PC10.SEEN")).is_some());
    // A function the machine does not have.
    let rp02 = PciFunction { segment: 0, bus: 0x10, device: 0x1D, function: 0 };
    assert_eq!(eval(&ns, "\\_SB.PC10.RP02.BVID", &ram), Err(Error::PciConfig(rp02, 0)));
}

#[test]
fn integers_are_32_bits_under_a_revision_1_dsdt() {
    let aml = method("ONES", 0, &ret(&[0xFF]));
    let mut data = vec![0u8; 36];
    data[..4].copy_from_slice(b"DSDT");
    data[8] = 1;
    data.extend_from_slice(&aml);
    let len = data.len() as u32;
    data[4..8].copy_from_slice(&len.to_le_bytes());
    let table = crate::tables::Table { address: 0, data };
    let mut ns = Namespace::new();
    ns.load_table(&table, &crate::NoMemory).unwrap();
    assert_eq!(eval(&ns, "\\ONES", &crate::NoMemory), Ok(Value::Integer(0xFFFF_FFFF)));
}

#[test]
fn malformed_code_is_an_error() {
    let good = device("DEV0", &name("_ADR", &int(1)));
    let mut ns = Namespace::new();
    // A package length pointing past the end.
    let mut bad = good.clone();
    bad[2] = 0x3F;
    assert!(ns.load(&bad, &crate::NoMemory).is_err());
    // A method calling one that does not exist fails when it runs.
    let code = method("CALL", 0, &ret(&cat(&[&nm("MISS"), &int(1)])));
    let mut ns = Namespace::new();
    ns.load(&code, &crate::NoMemory).unwrap();
    assert!(eval(&ns, "\\CALL", &crate::NoMemory).is_err());
}

#[test]
fn tables_from_the_rsdp() {
    let mut ram = Ram::default();
    let table = |signature: &[u8; 4], body: &[u8]| -> Vec<u8> {
        let mut t = vec![0u8; 36];
        t[..4].copy_from_slice(signature);
        t[8] = 2;
        t.extend_from_slice(body);
        let len = t.len() as u32;
        t[4..8].copy_from_slice(&len.to_le_bytes());
        let sum = t.iter().fold(0u8, |s, &b| s.wrapping_add(b));
        t[9] = 0u8.wrapping_sub(sum);
        t
    };
    let dsdt = table(b"DSDT", &device("DEV0", &[]));
    let mut fadt_body = vec![0u8; 148 - 36];
    fadt_body[140 - 36..148 - 36].copy_from_slice(&0x3000u64.to_le_bytes());
    let fadt = table(b"FACP", &fadt_body);
    let ssdt = table(b"SSDT", &[]);
    let xsdt = table(b"XSDT", &cat(&[&0x2000u64.to_le_bytes(), &0x4000u64.to_le_bytes()]));
    let mut rsdp = b"RSD PTR ".to_vec();
    rsdp.extend([0, 0, 0, 0, 0, 0, 0, 2]);
    rsdp.extend_from_slice(&0u32.to_le_bytes());
    rsdp.extend_from_slice(&36u32.to_le_bytes());
    rsdp.extend_from_slice(&0x1000u64.to_le_bytes());
    rsdp.extend([0; 4]);
    ram.put(0x100, &rsdp);
    ram.put(0x1000, &xsdt);
    ram.put(0x2000, &fadt);
    ram.put(0x3000, &dsdt);
    ram.put(0x4000, &ssdt);
    let tables = crate::tables::load(&ram, 0x100).unwrap();
    let names: Vec<[u8; 4]> = tables.iter().map(|t| t.signature()).collect();
    assert_eq!(names, [*b"FACP", *b"SSDT", *b"DSDT"]);
    assert!(tables.iter().all(|t| t.checksum_ok()));
    assert_eq!(crate::tables::load(&ram, 0x200).unwrap_err(), crate::tables::TableError::NoRsdp);
    // The walk a reader that maps the tables itself makes: their headers only.
    let entries: Vec<_> =
        crate::tables::entries(&ram, 0x100).unwrap().map(|e| (e.address, e.signature, e.length)).collect();
    assert_eq!(entries, [(0x2000, *b"FACP", 148), (0x4000, *b"SSDT", 36)]);
    assert_eq!(crate::tables::dsdt_address(&fadt), Some(0x3000));
    // A FADT too short for the DSDT's addresses names none (and is not read
    // past).
    let short = table(b"FACP", &[0; 4]);
    ram.put(0x2000, &short);
    let names: Vec<[u8; 4]> = crate::tables::load(&ram, 0x100).unwrap().iter().map(|t| t.signature()).collect();
    assert_eq!(names, [*b"FACP", *b"SSDT"]);
    assert_eq!(crate::tables::dsdt_address(&short), None);
}

#[test]
fn sleep_types_in_every_encoding() {
    use crate::aml::sleep_type;
    // Name (\_S5, Package (0x04) { 0x05, 0x05, Zero, Zero }), as compilers
    // write it, at the top of a DSDT.
    let byte = name("\\_S5_", &package(&[vec![0x0A, 0x05], vec![0x0A, 0x05], vec![0x00], vec![0x00]]));
    assert_eq!(sleep_type(&byte, 5), Some((5, 5)));
    // Rooted or not, after other code, with Zero, One, words and dwords
    // (which the kernel's earlier byte scan read as their prefixes).
    let words = cat(&[
        &method("_PTS", 1, &ret(&nm("\\_S5_"))),
        &scope("\\", &name("_S5_", &package(&[vec![0x0B, 0x07, 0x00], vec![0x01]]))),
    ]);
    assert_eq!(sleep_type(&words, 5), Some((7, 1)));
    let dwords = name("_S5_", &package(&[vec![0x0C, 0x06, 0, 0, 0], vec![0x0E, 3, 0, 0, 0, 0, 0, 0, 0]]));
    assert_eq!(sleep_type(&dwords, 5), Some((6, 3)));
    // Other states; one element; a reference only; elements that are not
    // constants; a package cut short.
    let s3 = name("_S3_", &package(&[vec![0x0A, 0x01], vec![0x0A, 0x01]]));
    assert_eq!((sleep_type(&s3, 3), sleep_type(&s3, 5)), (Some((1, 1)), None));
    assert_eq!(sleep_type(&name("_S5_", &package(&[vec![0x0A, 0x07]])), 5), Some((7, 0)));
    assert_eq!(sleep_type(&method("_PTS", 1, &ret(&nm("\\_S5_"))), 5), None);
    assert_eq!(sleep_type(&name("_S5_", &package(&[nm("SLPA"), nm("SLPB")])), 5), None);
    assert_eq!(sleep_type(&byte[..byte.len() - 3], 5), None);
    assert_eq!(sleep_type(&[], 5), None);
}

#[test]
fn package_lengths_of_every_size() {
    // A buffer's PkgLength in each of its four forms, as the interpreter
    // reads them back.
    for size in [10, 0x3E, 0x3F, 0xFF0, 0xFFF, 0xF_FFF0, 0x10_0000] {
        let code = name("BUF_", &buffer(&vec![0x5A; size]));
        let mut ns = Namespace::new();
        ns.load(&code, &crate::NoMemory).unwrap();
        match eval(&ns, "\\BUF_", &crate::NoMemory) {
            Ok(Value::Buffer(b)) => assert_eq!(b.len(), size),
            other => panic!("{size}: {other:?}"),
        }
    }
}

// --- Control flow -------------------------------------------------------------

/// An operator and its operands (and target, if it has one).
fn op(code: u8, operands: &[&[u8]]) -> Vec<u8> {
    let mut v = vec![code];
    for o in operands {
        v.extend_from_slice(o);
    }
    v
}

/// `Buffer (size) {}`: a buffer of zeros.
fn zeros(size: u64) -> Vec<u8> {
    cat(&[&[0x11], &pkg(&int(size))])
}

#[test]
fn an_inner_if_does_not_take_the_else_of_the_if_around_it() {
    // Method (NEST, 2) { If (Arg0) { If (Arg1) { Return (1) } } Else { Return (2) } Return (3) }
    let nest =
        method("NEST", 2, &cat(&[&if_(&[ARG0], &if_(&[ARG1], &ret(&int(1)))), &else_(&ret(&int(2))), &ret(&int(3))]));
    // Switch (Arg0) { Case (1) { If (Arg1) { Return (0x11) } } Case (2) { Return (0x22) }
    // Default { Return (0x33) } } Return (0x44), as iasl compiles it: in a While (One), each case
    // an If in the Else of the one before, and a Break after them.
    let t = nm("_T_0");
    let switch = method(
        "SWCH",
        2,
        &cat(&[
            &name("_T_0", &int(0)),
            &while_(
                &[0x01],
                &cat(&[
                    &store(&[ARG0], &t),
                    &if_(&lequal(&t, &int(1)), &if_(&[ARG1], &ret(&int(0x11)))),
                    &else_(&cat(&[&if_(&lequal(&t, &int(2)), &ret(&int(0x22))), &else_(&ret(&int(0x33)))])),
                    &[0xA5],
                ]),
            ),
            &ret(&int(0x44)),
        ]),
    );
    // And outside methods: If (One) { If (Zero) { Device (DEVA) {} } } Else { Device (DEVB) {} }
    let conditional = cat(&[&if_(&[0x01], &if_(&[0x00], &device("DEVA", &[]))), &else_(&device("DEVB", &[]))]);
    let mut ns = Namespace::new();
    ns.load(&cat(&[&nest, &switch, &conditional]), &crate::NoMemory).unwrap();
    let call =
        |m: &str, a: u64, b: u64| ns.evaluate(&path(m), &[Value::Integer(a), Value::Integer(b)], &crate::NoMemory);
    assert_eq!(call("\\NEST", 1, 0), Ok(Value::Integer(3)));
    assert_eq!(call("\\NEST", 1, 1), Ok(Value::Integer(1)));
    assert_eq!(call("\\NEST", 0, 1), Ok(Value::Integer(2)));
    assert_eq!(call("\\SWCH", 1, 0), Ok(Value::Integer(0x44)));
    assert_eq!(call("\\SWCH", 1, 1), Ok(Value::Integer(0x11)));
    assert_eq!(call("\\SWCH", 2, 0), Ok(Value::Integer(0x22)));
    assert_eq!(call("\\SWCH", 7, 0), Ok(Value::Integer(0x33)));
    assert!(ns.get(&path("\\DEVA")).is_none() && ns.get(&path("\\DEVB")).is_none());
}

// --- Initialisation -----------------------------------------------------------

/// A laptop's touchpad as Intel's reference code describes it: a
/// placeholder `_HID`, and resource templates whose I2C address and GPIO
/// pin its `_INI` fills in from the firmware's settings, through buffer
/// fields over them; present from Windows 8 on (`OSYS`, which `\_SB._INI`
/// sets).
fn touchpad() -> (Namespace, Ram) {
    let create_word = |buffer: &str, at: u64, field: &str| op(0x8B, &[&nm(buffer), &int(at), &nm(field)]);
    let ini = method(
        "_INI",
        0,
        &cat(&[
            &if_(
                &lequal(&nm("TPDT"), &int(1)),
                &cat(&[&store(&string("ELAN0662"), &nm("_HID")), &store(&int(0x15), &nm("BADR"))]),
            ),
            &store(&nm("GPDI"), &nm("INT1")),
        ]),
    );
    // If (LLess (OSYS, 0x07DC)) { Return (Zero) } Return (0x0F)
    let sta =
        method("_STA", 0, &cat(&[&if_(&op(0x95, &[&nm("OSYS"), &int(0x07DC)]), &ret(&int(0))), &ret(&int(0x0F))]));
    let crs = method("_CRS", 0, &ret(&op(0x84, &[&nm("SBFB"), &nm("SBFG"), &[0]])));
    let tpd0 = device(
        "TPD0",
        &cat(&[
            &name("_HID", &string("XXXX0000")),
            &name("_CID", &string("PNP0C50")),
            &name("SBFB", &resource_template(&i2c_descriptor(0, 400_000, false, "\\_SB.PC00.I2C1"))),
            &name("SBFG", &resource_template(&gpio_descriptor(true, 0, 0, 0, 0, "\\_SB.GPI0"))),
            // The I2C address 16 bytes into its descriptor, the GPIO pin 23.
            &create_word("SBFB", 16, "BADR"),
            &create_word("SBFG", 23, "INT1"),
            &ini,
            &sta,
            &crs,
        ]),
    );
    let dsdt = cat(&[
        &region("GNVS", 0, &int(0x5000), &int(0x10)),
        &field("GNVS", 0x10, &[("OSYS", 16), ("TPDT", 8), ("GPDI", 16)]),
        &scope(
            "\\_SB",
            &cat(&[
                &method("_INI", 0, &store(&int(0x07DF), &nm("OSYS"))),
                &device(
                    "PC00",
                    &cat(&[
                        &name("_HID", &eisa("PNP0A08")),
                        &device("I2C1", &cat(&[&name("_ADR", &int(0x0015_0001)), &tpd0])),
                    ]),
                ),
            ]),
        ),
    ]);
    let mut ram = Ram::default();
    // OSYS 0 until \_SB._INI says Windows 2015; the touchpad ELAN's (TPDT 1), on GPIO pin 0x137.
    ram.put(0x5000, &[0, 0, 1, 0x37, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    let mut ns = Namespace::new();
    ns.load(&dsdt, &ram).unwrap();
    (ns, ram)
}

#[test]
fn initialisation_finishes_describing_a_touchpad() {
    let (mut ns, ram) = touchpad();
    let tp = path("\\_SB.PC00.I2C1.TPD0");
    let i2c_address = |r: &[Resource]| match &r[0] {
        Resource::I2c(i) => (i.address, i.speed_hz, i.controller.clone()),
        other => panic!("{other:?}"),
    };
    let pins = |r: &[Resource]| match &r[1] {
        Resource::Gpio(g) => g.pins.clone(),
        other => panic!("{other:?}"),
    };
    // As loaded: the placeholder, not present (before Windows 8), nothing in its templates.
    let id = device::identify(&ns, &tp, &ram);
    assert_eq!((id.hid.as_deref(), id.present()), (Some("XXXX0000"), false));
    let r = device::resources(&ns, &tp, &ram).unwrap();
    assert_eq!((i2c_address(&r).0, pins(&r)), (0, vec![0]));

    let done = ns.initialize(&ram);
    assert_eq!(done, crate::aml::Initialized { ran: 2, failures: 0, failed: Vec::new() });
    let id = device::identify(&ns, &tp, &ram);
    assert_eq!((id.hid.as_deref(), id.present()), (Some("ELAN0662"), true));
    assert_eq!(id.cids, [String::from("PNP0C50")]);
    let r = device::resources(&ns, &tp, &ram).unwrap();
    assert_eq!(i2c_address(&r), (0x15, 400_000, String::from("\\_SB.PC00.I2C1")));
    assert_eq!(pins(&r), [0x137]);

    // What an evaluation stores still stays in it: the next one starts from
    // what initialisation left.
    let set =
        scope("\\_SB.PC00.I2C1.TPD0", &method("SETA", 0, &cat(&[&store(&int(0x99), &nm("BADR")), &ret(&nm("BADR"))])));
    ns.load(&set, &ram).unwrap();
    assert_eq!(eval(&ns, "\\_SB.PC00.I2C1.TPD0.SETA", &ram), Ok(Value::Integer(0x99)));
    assert_eq!(eval(&ns, "\\_SB.PC00.I2C1.TPD0.BADR", &ram), Ok(Value::Integer(0x15)));
    assert_eq!(eval(&ns, "\\OSYS", &ram), Ok(Value::Integer(0x07DF)));
}

#[test]
fn initialisation_looks_below_present_or_functioning_devices_only() {
    let flag = |n: &str| method("_INI", 0, &store(&int(1), &nm(n)));
    let aml = cat(&[
        &name("F0", &int(0)),
        &name("F1", &int(0)),
        &name("F2", &int(0)),
        &name("F3", &int(0)),
        // Neither present nor functioning: its children are not looked at.
        &device("GONE", &cat(&[&name("_STA", &int(0)), &device("KID0", &flag("\\F0"))])),
        // Functioning but not present (a bridge): its _INI does not run, its children's do.
        &device("BRDG", &cat(&[&name("_STA", &int(8)), &device("KID1", &flag("\\F1")), &flag("\\F2")])),
        // An _STA that fails is passed over; the devices below are looked at.
        &device(
            "FAIL",
            &cat(&[&method("_STA", 0, &ret(&op(0x85, &[&int(1), &int(0), &[0]]))), &device("KID3", &flag("\\F3"))]),
        ),
    ]);
    let mut ns = Namespace::new();
    ns.load(&aml, &crate::NoMemory).unwrap();
    let done = ns.initialize(&crate::NoMemory);
    assert_eq!((done.ran, done.failures), (2, 1));
    assert_eq!(done.failed, [(path("\\FAIL._STA"), Error::DivideByZero)]);
    let flags: Vec<_> = ["\\F0", "\\F1", "\\F2", "\\F3"].iter().map(|f| eval(&ns, f, &crate::NoMemory)).collect();
    assert_eq!(flags, [0, 1, 0, 1].map(|v| Ok(Value::Integer(v))));
}

#[test]
fn a_buffer_fields_offset_may_wait_for_a_later_table() {
    // CreateDWordField (BUF0, \IDX0, FLD0), \IDX0 defined in the next table.
    let first = cat(&[
        &name("BUF0", &buffer(&[1, 2, 3, 4, 5, 6, 7, 8])),
        &op(0x8A, &[&nm("BUF0"), &nm("\\IDX0"), &nm("FLD0")]),
    ]);
    let mut ns = Namespace::new();
    ns.load(&first, &crate::NoMemory).unwrap();
    assert!(matches!(
        ns.get(&path("\\FLD0")),
        Some(Object::BufferField { bits: crate::aml::Bits::Deferred { .. }, .. })
    ));
    assert_eq!(eval(&ns, "\\FLD0", &crate::NoMemory), Err(Error::NoSuchName(path("\\IDX0"))));
    ns.load(&name("IDX0", &int(4)), &crate::NoMemory).unwrap();
    assert_eq!(eval(&ns, "\\FLD0", &crate::NoMemory), Ok(Value::Integer(0x0807_0605)));
}

// --- Bounds ---------------------------------------------------------------------

#[test]
fn nesting_is_bounded() {
    let m = &crate::NoMemory;
    // Add (Add (... Add (1, 1) ..., 1), 1), so many deep, in a method.
    let nested = |depth: usize| (0..depth).fold(int(1), |inner, _| add(&inner, &int(1)));
    let mut ns = Namespace::new();
    ns.load(&cat(&[&method("DEEP", 0, &ret(&nested(300))), &method("FINE", 0, &ret(&nested(200)))]), m).unwrap();
    assert_eq!(eval(&ns, "\\DEEP", m), Err(Error::Limit));
    assert_eq!(eval(&ns, "\\FINE", m), Ok(Value::Integer(201)));
    // Definitions nested as deep, at load.
    let devices = (0..300).fold(Vec::new(), |inner, _| device("NEST", &inner));
    assert_eq!(Namespace::new().load(&devices, m), Err(Error::Limit));
    // Values nested deeper by a loop: Local0 = Package () { Local0 }, Arg0 times.
    let count = |body: &[u8]| while_(&op(0x95, &[&[LOCAL1], &[ARG0]]), &cat(&[body, &[0x75, LOCAL1]]));
    let wrap = method("WRAP", 1, &cat(&[&count(&store(&package(&[vec![LOCAL0]]), &[LOCAL0])), &ret(&[LOCAL1])]));
    // And by storing a package into its own element: Local0[0] = Local0.
    let into = method(
        "INTO",
        1,
        &cat(&[
            &store(&package(&[int(0)]), &[LOCAL0]),
            &count(&store(&[LOCAL0], &op(0x88, &[&[LOCAL0], &int(0), &[0]]))),
            &ret(&[LOCAL1]),
        ]),
    );
    let mut ns = Namespace::new();
    ns.load(&cat(&[&wrap, &into]), m).unwrap();
    let run = |method: &str, n: u64| ns.evaluate(&path(method), &[Value::Integer(n)], m);
    assert_eq!(run("\\WRAP", 200), Ok(Value::Integer(200)));
    assert_eq!(run("\\WRAP", 300), Err(Error::Limit));
    assert_eq!(run("\\INTO", 200), Ok(Value::Integer(200)));
    assert_eq!(run("\\INTO", 300), Err(Error::Limit));
}

#[test]
fn the_deepest_code_allowed_runs_on_a_small_stack() {
    // What the nesting limit bounds: code as deep as it allows takes some
    // 250 KiB of stack in an optimised build (devmgr's is 1 MiB).
    let m = &crate::NoMemory;
    let nested = |inner: Vec<u8>, depth: usize| (0..depth).fold(inner, |inner, _| add(&inner, &int(1)));
    // Expressions 250 deep; Ifs 250 deep; 31 calls nested, each through
    // expressions 5 deep.
    let mut calls = Vec::new();
    for i in 0..31 {
        let callee = if i == 30 { int(1) } else { nm(&alloc::format!("M{:03}", i + 1)) };
        calls.extend(method(&alloc::format!("M{:03}", i), 0, &ret(&nested(callee, 5))));
    }
    let aml = cat(&[
        &method("EXPR", 0, &ret(&nested(int(1), 250))),
        &method("IFS_", 0, &(0..250).fold(ret(&int(7)), |inner, _| if_(&[0x01], &inner))),
        &calls,
    ]);
    // And at load, definitions nested 120 deep, the innermost a name whose
    // data nests 130 deeper: the two count together.
    let deep = (0..120).fold(name("DATA", &nested(int(1), 130)), |inner, _| device("NEST", &inner));
    let results = std::thread::Builder::new()
        .stack_size(512 << 10)
        .spawn(move || {
            let mut ns = Namespace::new();
            ns.load(&aml, m).unwrap();
            ns.load(&deep, m).unwrap();
            let data = alloc::format!("\\{}DATA", "NEST.".repeat(120));
            ["\\EXPR", "\\IFS_", "\\M000", &data].map(|p| eval(&ns, p, m))
        })
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(results, [251, 7, 156, 131].map(|v| Ok(Value::Integer(v))));
}

#[test]
fn values_and_their_copies_are_bounded() {
    let m = &crate::NoMemory;
    // Local0 = Buffer (1) {}; While (One) { Local0 = Concatenate (Local0, Local0) }: doubling.
    let double = method(
        "DBLE",
        0,
        &cat(&[&store(&buffer(&[0]), &[LOCAL0]), &while_(&[0x01], &op(0x73, &[&[LOCAL0], &[LOCAL0], &[LOCAL0]]))]),
    );
    // ToHexString of a buffer of 1 MiB: 5 MiB of text.
    let hex = method("HEXS", 0, &ret(&op(0x98, &[&zeros(1 << 20), &[0]])));
    // A copy of 1 MiB each time round a loop: Local1 = Local0.
    let copy =
        method("COPY", 0, &cat(&[&store(&zeros(1 << 20), &[LOCAL0]), &while_(&[0x01], &store(&[LOCAL0], &[LOCAL1]))]));
    // Bytes of a buffer of 1 MiB read where they are, fifty thousand times:
    // Local0 = DerefOf (Index (BIG, Local1)).
    let read = cat(&[
        &name("BIG_", &zeros(1 << 20)),
        &method(
            "READ",
            0,
            &cat(&[
                &while_(
                    &op(0x95, &[&[LOCAL1], &int(50_000)]),
                    &cat(&[
                        &store(&op(0x83, &[&op(0x88, &[&nm("BIG_"), &[LOCAL1], &[0]])]), &[LOCAL0]),
                        &[0x75, LOCAL1],
                    ]),
                ),
                &ret(&[LOCAL1]),
            ]),
        ),
    ]);
    let mut ns = Namespace::new();
    ns.load(&cat(&[&double, &hex, &copy, &read]), m).unwrap();
    assert_eq!(eval(&ns, "\\DBLE", m), Err(Error::Limit));
    assert_eq!(eval(&ns, "\\HEXS", m), Err(Error::Limit));
    assert_eq!(eval(&ns, "\\COPY", m), Err(Error::Limit));
    assert_eq!(eval(&ns, "\\READ", m), Ok(Value::Integer(50_000)));
}

#[test]
fn a_load_is_bounded_as_a_whole() {
    let m = &crate::NoMemory;
    // Names whose data each take most of what one evaluation may.
    let spin =
        method("SPIN", 0, &cat(&[&while_(&op(0x95, &[&[LOCAL0], &int(150_000)]), &[0x75, LOCAL0]), &ret(&[LOCAL0])]));
    let names: Vec<u8> = (0..8).flat_map(|i| name(&alloc::format!("N{i:03}"), &nm("SPIN"))).collect();
    let mut ns = Namespace::new();
    assert_eq!(ns.load(&cat(&[&spin, &names]), m), Err(Error::Limit));
    // What was defined before the load ran out stays.
    assert_eq!(eval(&ns, "\\N000", m), Ok(Value::Integer(150_000)));
    assert!(ns.get(&path("\\N007")).is_none());
}

#[test]
fn evaluations_together_are_bounded_as_their_owner_says() {
    let m = &crate::NoMemory;
    let mut ns = Namespace::new();
    ns.load(&cat(&[&method("LOOP", 0, &while_(&[0x01], &[])), &method("ONE_", 0, &ret(&int(1)))]), m).unwrap();
    assert_eq!(ns.left(), None);
    ns.limit(Some((1000, u64::MAX)));
    assert_eq!(eval(&ns, "\\LOOP", m), Err(Error::Limit));
    // The loop took what was left.
    assert_eq!(ns.left(), Some((0, u64::MAX)));
    assert_eq!(eval(&ns, "\\ONE_", m), Err(Error::Limit));
    ns.limit(None);
    assert_eq!(eval(&ns, "\\ONE_", m), Ok(Value::Integer(1)));
}

#[test]
fn firmware_values_that_overflow_are_errors() {
    let m = &crate::NoMemory;
    let outside = Err(Error::Type("a field inside its buffer"));
    let aml = cat(&[
        &name("BUF_", &buffer(&[0; 8])),
        // CreateDWordField (BUF, 0x2000000000000000, FFFF): its bit offset is past 64 bits.
        &method(
            "FAR_",
            0,
            &cat(&[&op(0x8A, &[&nm("BUF_"), &int(0x2000_0000_0000_0000), &nm("FFFF")]), &ret(&nm("FFFF"))]),
        ),
        // CreateField (BUF, 0xFFFFFFFFFFFFFFF0, 0x20, GGGG): its end is.
        &method(
            "WRAP",
            0,
            &cat(&[
                &[0x5B, 0x13],
                &nm("BUF_"),
                &int(0xFFFF_FFFF_FFFF_FFF0),
                &int(0x20),
                &nm("GGGG"),
                &ret(&nm("GGGG")),
            ]),
        ),
        &method(
            "SET_",
            0,
            &cat(&[
                &[0x5B, 0x13],
                &nm("BUF_"),
                &int(0xFFFF_FFFF_FFFF_FFF0),
                &int(0x20),
                &nm("GGGG"),
                &store(&int(1), &nm("GGGG")),
            ]),
        ),
        // A _PRT whose pin is Ones, and one whose pin is INTA#.
        &scope(
            "\\_SB",
            &device(
                "PCI0",
                &cat(&[
                    &name("_HID", &eisa("PNP0A08")),
                    &name(
                        "_PRT",
                        &package(&[
                            package(&[int(0x0003_FFFF), int(u64::MAX), int(0), int(0x11)]),
                            package(&[int(0x0003_FFFF), int(0), int(0), int(0x10)]),
                        ]),
                    ),
                ]),
            ),
        ),
    ]);
    let mut ns = Namespace::new();
    ns.load(&aml, m).unwrap();
    assert_eq!(eval(&ns, "\\FAR_", m), outside);
    assert_eq!(eval(&ns, "\\WRAP", m), outside);
    assert_eq!(eval(&ns, "\\SET_", m), outside);
    let roots = device::pci_roots(&ns, m);
    let route = |pin| device::pci_interrupt(&ns, &roots, (0, 3), pin, m);
    assert_eq!(route(1), Some(device::IntxRoute { gsi: 0x10, level: true, active_low: true }));
    // Pin 0 is no INTx.
    assert_eq!(route(0), None);
}

// --- Access widths ----------------------------------------------------------------

/// Firmware memory that says how it was read: the address (or offset) and
/// width of each access to a field.
#[derive(Default)]
struct Watched {
    ram: Ram,
    accesses: core::cell::RefCell<Vec<(u64, usize)>>,
}

impl Memory for Watched {
    fn read(&self, address: u64, buf: &mut [u8]) -> bool {
        self.ram.read(address, buf)
    }

    fn read_field(&self, address: u64, width: usize, buf: &mut [u8]) -> bool {
        let mut accesses = self.accesses.borrow_mut();
        accesses.extend((0..buf.len() / width).map(|i| (address + (i * width) as u64, width)));
        self.ram.read(address, buf)
    }

    fn read_pci_field(&self, function: PciFunction, offset: u16, width: usize, buf: &mut [u8]) -> bool {
        let mut accesses = self.accesses.borrow_mut();
        accesses.extend((0..buf.len() / width).map(|i| ((offset as usize + i * width) as u64, width)));
        self.ram.read_pci(function, offset, buf)
    }
}

#[test]
fn fields_are_read_with_their_access_widths() {
    let dsdt = cat(&[
        &region("DEVR", 0, &int(0x6000), &int(0x10)),
        // DWordAcc: REGA, 32, REGB, 8; ByteAcc: Offset (8), REGC, 16.
        &field("DEVR", 0x03, &[("REGA", 32), ("REGB", 8)]),
        &field("DEVR", 0x01, &[("", 64), ("REGC", 16)]),
        // WordAcc in the host bridge's configuration space: Offset (0x40), PCIW, 16.
        &scope(
            "\\_SB",
            &device(
                "PC00",
                &cat(&[
                    &name("_HID", &eisa("PNP0A08")),
                    &region("HBUS", 2, &int(0), &int(0x100)),
                    &field("HBUS", 0x02, &[("", 0x40 * 8), ("PCIW", 16)]),
                ]),
            ),
        ),
    ]);
    let mut memory = Watched::default();
    memory.ram.put(0x6000, &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]);
    memory.ram.put_pci((0, 0, 0), 0x40, &[0x34, 0x12]);
    let mut ns = Namespace::new();
    ns.load(&dsdt, &memory).unwrap();
    let read = |name: &str| {
        memory.accesses.borrow_mut().clear();
        let v = eval(&ns, name, &memory);
        (v, memory.accesses.borrow().clone())
    };
    assert_eq!(read("\\REGA"), (Ok(Value::Integer(0x0403_0201)), vec![(0x6000, 4)]));
    assert_eq!(read("\\REGB"), (Ok(Value::Integer(5)), vec![(0x6004, 4)]));
    assert_eq!(read("\\REGC"), (Ok(Value::Integer(0x0A09)), vec![(0x6008, 1), (0x6009, 1)]));
    assert_eq!(read("\\_SB.PC00.PCIW"), (Ok(Value::Integer(0x1234)), vec![(0x40, 2)]));
}

// --- What Veda writes ---------------------------------------------------------

#[test]
fn names_are_checked_as_aml_has_them() {
    assert_eq!(crate::name::seg("_SB"), Some(*b"_SB_"));
    for bad in ["", "TOOLONG", "1ABC", "ab", "A-B", "A B", "É"] {
        assert_eq!(crate::name::seg(bad), None, "{bad}");
    }
    assert!(NameString::parse("\\_SB.pc00").is_none());
    assert_eq!(name_string("\\_SB.PC00"), Some(nm("\\_SB.PC00")));
    assert_eq!(name_string("^^bad"), None);
    assert_eq!(name_string(&vec!["ABCD"; 256].join(".")), None);
    assert_eq!(try_string("a\0b"), None);
    assert_eq!(try_name("A.B..C", &int(1)), None);
    assert_eq!(try_device("TPD0", &[]), Some(device("TPD0", &[])));
    let mut g = Gpio {
        interrupt: false,
        pins: vec![1],
        controller: "\\_SB.GP\0".into(),
        pull: 0,
        restriction: 0,
        edge: false,
        polarity: 0,
        shared: false,
        wake: false,
        debounce: 0,
    };
    assert_eq!(try_gpio_descriptor_of(&g), None);
    g.controller = "\\_SB.GPI0".into();
    g.pins = vec![7; 40_000];
    assert_eq!(try_gpio_descriptor_of(&g), None);
}

#[test]
fn packages_of_more_than_255_elements() {
    let m = &crate::NoMemory;
    let v = Value::Package((0..300).map(Value::Integer).collect());
    let mut ns = Namespace::new();
    ns.load(&name("PINS", &constant(&v).unwrap()), m).unwrap();
    assert_eq!(eval(&ns, "\\PINS", m), Ok(v));
    // A string with a NUL is no constant AML can hold.
    assert_eq!(constant(&Value::String("a\0b".into())), None);
}

#[test]
fn a_dsm_answers_functions_past_the_first_byte() {
    let u = uuid(HID_OVER_I2C).unwrap();
    let mut ns = Namespace::new();
    ns.load(&device("DEV9", &dsm(&u, 9, &int(0x99))), &crate::NoMemory).unwrap();
    let call = |function: u64| {
        let args = [Value::Buffer(u.to_vec()), Value::Integer(1), Value::Integer(function), Value::Package(vec![])];
        ns.evaluate(&path("\\DEV9._DSM"), &args, &crate::NoMemory)
    };
    assert_eq!(call(0), Ok(Value::Buffer(vec![0x01, 0x02])));
    assert_eq!(call(9), Ok(Value::Integer(0x99)));
}

#[test]
fn veda_writes_whole_tables() {
    let t = table(b"SSDT", 2, b"TESTSSDT", &device("DEV0", &[]));
    let t = crate::tables::Table { address: 0, data: t };
    assert!(t.checksum_ok());
    assert_eq!((t.signature(), t.oem_table_id().as_str(), t.data.len()), (*b"SSDT", "TESTSSDT", 36 + t.aml().len()));
    assert_eq!(&t.data[10..16], OEM_ID);
    // A table too short for its header says nothing, and fails nothing.
    let short = crate::tables::Table { address: 0, data: vec![b'D', b'S'] };
    assert_eq!((short.signature(), short.aml(), short.oem_table_id().as_str()), ([0; 4], &[][..], ""));
}
