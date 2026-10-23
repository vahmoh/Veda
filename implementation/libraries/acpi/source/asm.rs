// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! A small AML assembler, so that the tables Veda writes (the driver VM's,
//! and those of tests and simulated boards) read like ASL:
//! `device("SPK1", &cat(&[&name("_HID", &string("CSC3551")), ...]))`.
//! Each function returns the bytes of one term; `cat` joins them. Names are
//! ASL text (`\_SB.PC00`, `^GPI0`, `SBUF`).
//!
//! Most functions are for code written by hand, and panic on what cannot be
//! written: a malformed name, a string with a NUL in it, a package past the
//! format's 256 MiB. Those that take what comes from elsewhere (the
//! firmware's names and ids, which the driver VM's tables pass on) have
//! fallible versions, which say `None` instead: [`name_string`],
//! [`try_name`], [`try_device`], [`try_string`],
//! [`try_gpio_descriptor_of`].
//!
//! And whole tables: [`table`] writes one's header, as Veda's tables have
//! it; the UUIDs of the `_DSM` and `_DSD` objects Veda's tables answer are
//! here too.

use alloc::vec;
use alloc::vec::Vec;

use crate::name::NameString;
use crate::resource::{Gpio, Spi};
use crate::tables::HEADER_LEN;
use crate::value::Value;

/// The bytes of `body` behind its PkgLength, which counts its own bytes:
/// one for the shortest bodies, two to four for longer ones. Panics past
/// 256 MiB, the format's limit.
pub fn pkg(body: &[u8]) -> Vec<u8> {
    try_pkg(body).expect("an AML package is shorter than 256 MiB")
}

/// Whether a body of `n` bytes fits a package (with its PkgLength, of at
/// most four bytes, less than 256 MiB).
fn fits(n: usize) -> bool {
    n.checked_add(4).is_some_and(|total| total < 1 << 28)
}

/// [`pkg`], or `None` past the format's limit.
fn try_pkg(body: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let n = body.len();
    if !fits(n) {
        return None;
    }
    if n + 1 < 0x40 {
        out.push((n + 1) as u8);
    } else {
        let follow = if n + 2 < 1 << 12 {
            1
        } else if n + 3 < 1 << 20 {
            2
        } else {
            3
        };
        let total = n + 1 + follow;
        out.push((follow as u8) << 6 | (total & 0xF) as u8);
        out.extend((0..follow).map(|i| (total >> (4 + 8 * i)) as u8));
    }
    out.extend_from_slice(body);
    Some(out)
}

/// A NameString, or `None` if `text` is not a name AML can hold (a
/// segment that is not a name's, more than 255 of them).
pub fn name_string(text: &str) -> Option<Vec<u8>> {
    let n = NameString::parse(text)?;
    let mut out = Vec::new();
    if n.absolute {
        out.push(b'\\');
    }
    out.extend(core::iter::repeat_n(b'^', n.parents));
    match n.segs.len() {
        0 => out.push(0),
        1 => {}
        2 => out.push(0x2E),
        k => out.extend([0x2F, u8::try_from(k).ok()?]),
    }
    for s in &n.segs {
        out.extend_from_slice(s);
    }
    Some(out)
}

/// A NameString. Panics on a malformed name.
pub fn nm(text: &str) -> Vec<u8> {
    name_string(text).expect("a malformed name")
}

pub fn cat(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

/// An integer, in its shortest encoding.
pub fn int(v: u64) -> Vec<u8> {
    match v {
        0 => vec![0x00],
        1 => vec![0x01],
        v if v <= 0xFF => vec![0x0A, v as u8],
        v if v <= 0xFFFF => cat(&[&[0x0B], &(v as u16).to_le_bytes()]),
        v if v <= 0xFFFF_FFFF => cat(&[&[0x0C], &(v as u32).to_le_bytes()]),
        v => cat(&[&[0x0E], &v.to_le_bytes()]),
    }
}

/// A compressed EISA id (`EisaId ("PNP0A08")`). Panics if `id` is not
/// three capital letters and four hexadecimal digits.
pub fn eisa(id: &str) -> Vec<u8> {
    let b = id.as_bytes();
    let letter = |c: u8| (c - b'@') as u32 & 0x1F;
    let hex = u32::from_str_radix(&id[3..7], 16).expect("an EISA id");
    let v = letter(b[0]) << 26 | letter(b[1]) << 21 | letter(b[2]) << 16 | hex;
    cat(&[&[0x0C], &v.swap_bytes().to_le_bytes()])
}

/// A string. Panics if it has a NUL, which would end it.
pub fn string(s: &str) -> Vec<u8> {
    try_string(s).expect("a string without NUL")
}

/// A string, or `None` if it has a NUL, which would end it.
pub fn try_string(s: &str) -> Option<Vec<u8>> {
    (!s.as_bytes().contains(&0)).then(|| cat(&[&[0x0D], s.as_bytes(), &[0]]))
}

/// A buffer of `bytes`.
pub fn buffer(bytes: &[u8]) -> Vec<u8> {
    cat(&[&[0x11], &pkg(&cat(&[&int(bytes.len() as u64), bytes]))])
}

/// A package of `items`: a `Package` of up to 255, a `VarPackage` of more.
pub fn package(items: &[Vec<u8>]) -> Vec<u8> {
    match u8::try_from(items.len()) {
        Ok(n) => cat(&[&[0x12], &pkg(&cat(&[&[n], &items.concat()]))]),
        Err(_) => cat(&[&[0x13], &pkg(&cat(&[&int(items.len() as u64), &items.concat()]))]),
    }
}

/// `Scope (name) { body }`.
pub fn scope(name: &str, body: &[u8]) -> Vec<u8> {
    cat(&[&[0x10], &pkg(&cat(&[&nm(name), body]))])
}

/// `Device (name) { body }`.
pub fn device(name: &str, body: &[u8]) -> Vec<u8> {
    try_device(name, body).expect("a well-formed device name")
}

/// A device, or `None` if `name` is malformed or the body too long.
pub fn try_device(name: &str, body: &[u8]) -> Option<Vec<u8>> {
    Some(cat(&[&[0x5B, 0x82], &try_pkg(&cat(&[&name_string(name)?, body]))?]))
}

/// `Method (name, args) { body }`.
pub fn method(name: &str, args: u8, body: &[u8]) -> Vec<u8> {
    cat(&[&[0x14], &pkg(&cat(&[&nm(name), &[args], body]))])
}

/// `Name (n, value)`.
pub fn name(n: &str, value: &[u8]) -> Vec<u8> {
    cat(&[&[0x08], &nm(n), value])
}

/// A name, or `None` if `n` is malformed.
pub fn try_name(n: &str, value: &[u8]) -> Option<Vec<u8>> {
    Some(cat(&[&[0x08], &name_string(n)?, value]))
}

/// `External (name, kind, args)` (kind 6 a device, 8 a method).
pub fn external(n: &str, kind: u8, args: u8) -> Vec<u8> {
    cat(&[&[0x15], &nm(n), &[kind, args]])
}

/// `If (predicate) { body }`.
pub fn if_(predicate: &[u8], body: &[u8]) -> Vec<u8> {
    cat(&[&[0xA0], &pkg(&cat(&[predicate, body]))])
}

/// `Else { body }`, right after an `If`.
pub fn else_(body: &[u8]) -> Vec<u8> {
    cat(&[&[0xA1], &pkg(body)])
}

/// `While (predicate) { body }`.
pub fn while_(predicate: &[u8], body: &[u8]) -> Vec<u8> {
    cat(&[&[0xA2], &pkg(&cat(&[predicate, body]))])
}

/// `Return (v)`.
pub fn ret(v: &[u8]) -> Vec<u8> {
    cat(&[&[0xA4], v])
}

/// `Store (v, target)`.
pub fn store(v: &[u8], target: &[u8]) -> Vec<u8> {
    cat(&[&[0x70], v, target])
}

/// A two-operand operator with a target (`Add`, `And`, `ShiftLeft`...).
pub fn binary(op: u8, a: &[u8], b: &[u8], target: &[u8]) -> Vec<u8> {
    cat(&[&[op], a, b, target])
}

/// `Add (a, b)`, its result not stored.
pub fn add(a: &[u8], b: &[u8]) -> Vec<u8> {
    binary(0x72, a, b, &[0])
}

/// `LEqual (a, b)`.
pub fn lequal(a: &[u8], b: &[u8]) -> Vec<u8> {
    cat(&[&[0x93], a, b])
}

/// `OperationRegion (n, space, offset, length)`.
pub fn region(n: &str, space: u8, offset: &[u8], length: &[u8]) -> Vec<u8> {
    cat(&[&[0x5B, 0x80], &nm(n), &[space], offset, length])
}

/// A field list: named fields (`"SM01", 8`) and reserved bits (`"", 8`).
pub fn field(region: &str, flags: u8, fields: &[(&str, usize)]) -> Vec<u8> {
    let mut list = Vec::new();
    for &(n, bits) in fields {
        if n.is_empty() {
            list.push(0);
        } else {
            list.extend_from_slice(&nm(n));
        }
        // A field's length as a PkgLength value.
        if bits < 0x40 {
            list.push(bits as u8);
        } else if bits < 0x1000 {
            list.extend([0x40 | (bits & 0xF) as u8, (bits >> 4) as u8]);
        } else {
            list.extend([0x80 | (bits & 0xF) as u8, (bits >> 4) as u8, (bits >> 12) as u8]);
        }
    }
    cat(&[&[0x5B, 0x81], &pkg(&cat(&[&nm(region), &[flags], &list]))])
}

/// `Local0`, `Local1`, `Arg0`, `Arg1`, `Arg2`.
pub const LOCAL0: u8 = 0x60;
pub const LOCAL1: u8 = 0x61;
pub const ARG0: u8 = 0x68;
pub const ARG1: u8 = 0x69;
pub const ARG2: u8 = 0x6A;

/// The AML of a constant: an integer, a string, a buffer, or a package of
/// them (`None` for anything else, a reference say, or what AML cannot
/// hold: a string with a NUL).
pub fn constant(v: &Value) -> Option<Vec<u8>> {
    match v {
        Value::Integer(i) => Some(int(*i)),
        Value::String(s) => try_string(s),
        // A buffer's size and a package's count take up to 9 bytes.
        Value::Buffer(b) => fits(b.len() + 9).then(|| buffer(b)),
        Value::Package(items) => {
            let items = items.iter().map(constant).collect::<Option<Vec<_>>>()?;
            fits(items.iter().map(Vec::len).sum::<usize>() + 9).then(|| package(&items))
        }
        _ => None,
    }
}

/// The bytes `ToUUID ("...")` makes of a UUID's text: the first three
/// fields little-endian, the rest as written. `None` if it is not one.
pub fn uuid(text: &str) -> Option<[u8; 16]> {
    let hex: Vec<u8> = text
        .split('-')
        .flat_map(|g| g.as_bytes().chunks(2))
        .map(|pair| u8::from_str_radix(core::str::from_utf8(pair).ok()?, 16).ok())
        .collect::<Option<_>>()?;
    let groups: Vec<usize> = text.split('-').map(str::len).collect();
    if hex.len() != 16 || groups != [8, 4, 4, 4, 12] {
        return None;
    }
    let mut u = [0u8; 16];
    u[..4].copy_from_slice(&[hex[3], hex[2], hex[1], hex[0]]);
    u[4..8].copy_from_slice(&[hex[5], hex[4], hex[7], hex[6]]);
    u[8..].copy_from_slice(&hex[8..]);
    Some(u)
}

/// HID over I2C's `_DSM` (function 1: the HID descriptor's address).
pub const HID_OVER_I2C: &str = "3cdff6f7-4267-4555-ad05-b30a3d8938de";
/// Device properties' UUID (`_DSD`).
pub const DEVICE_PROPERTIES: &str = "daffd814-6eba-4d8c-8a91-bc9bbf4aa301";

/// A `_DSM` that answers function `function` of `uuid` (at any revision)
/// with `answer` (an AML term), and function 0 with the functions there
/// are (a bit each, in as many bytes as they take); anything else with
/// nothing.
pub fn dsm(uuid: &[u8; 16], function: u8, answer: &[u8]) -> Vec<u8> {
    let mut functions = vec![0u8; function as usize / 8 + 1];
    functions[0] |= 1;
    functions[function as usize / 8] |= 1 << (function % 8);
    let functions = buffer(&functions);
    method(
        "_DSM",
        4,
        &cat(&[
            &if_(
                &lequal(&[ARG0], &buffer(uuid)),
                &cat(&[
                    &if_(&lequal(&[ARG2], &int(0)), &ret(&functions)),
                    &if_(&lequal(&[ARG2], &int(function as u64)), &ret(answer)),
                ]),
            ),
            &ret(&buffer(&[0])),
        ]),
    )
}

// --- Tables ----------------------------------------------------------------

/// The OEM id of Veda's tables.
pub const OEM_ID: &[u8; 6] = b"VEDA  ";

/// A whole table, as Veda writes them: its header (signature, length,
/// `revision`, checksum, OEM id [`OEM_ID`], `oem_table_id`, OEM revision 1,
/// creator `VEDA`, creator revision 1), then `body`.
pub fn table(signature: &[u8; 4], revision: u8, oem_table_id: &[u8; 8], body: &[u8]) -> Vec<u8> {
    let mut t = Vec::with_capacity(HEADER_LEN + body.len());
    t.extend_from_slice(signature);
    t.extend_from_slice(&((HEADER_LEN + body.len()) as u32).to_le_bytes());
    t.push(revision);
    t.push(0);
    t.extend_from_slice(OEM_ID);
    t.extend_from_slice(oem_table_id);
    t.extend_from_slice(&1u32.to_le_bytes());
    t.extend_from_slice(b"VEDA");
    t.extend_from_slice(&1u32.to_le_bytes());
    t.extend_from_slice(body);
    t[9] = checksum(&t);
    t
}

/// What makes `bytes` add up to zero.
pub fn checksum(bytes: &[u8]) -> u8 {
    0u8.wrapping_sub(bytes.iter().fold(0u8, |a, &b| a.wrapping_add(b)))
}

// --- Resource descriptors ---------------------------------------------------

/// A large descriptor: its type, then the length and `body`; `None` if the
/// body is longer than a descriptor's.
fn large(kind: u8, body: &[u8]) -> Option<Vec<u8>> {
    let len = u16::try_from(body.len()).ok()?;
    Some(cat(&[&[0x80 | kind], &len.to_le_bytes(), body]))
}

/// A resource source's name, NUL-terminated: `None` if it has a NUL.
fn source(name: &str) -> Option<&[u8]> {
    (!name.as_bytes().contains(&0)).then_some(name.as_bytes())
}

/// `SpiSerialBusV2 (cs, PolarityLow, FourWireMode, 8, ControllerInitiated,
/// speed, ClockPolarityLow, ClockPhaseFirst, controller)`.
pub fn spi_descriptor(cs: u16, speed: u32, controller: &str) -> Vec<u8> {
    spi_descriptor_of(&Spi {
        controller: controller.into(),
        chip_select: cs,
        speed_hz: speed,
        bits: 8,
        cpol: false,
        cpha: false,
        cs_active_high: false,
        three_wire: false,
    })
}

/// The `SpiSerialBusV2` [`crate::resource::parse`] reads as `s`. Panics
/// on a controller's name with a NUL.
pub fn spi_descriptor_of(s: &Spi) -> Vec<u8> {
    let specific = s.three_wire as u16 | (s.cs_active_high as u16) << 1;
    let mut d = vec![2, 0, 2, 2];
    d.extend_from_slice(&specific.to_le_bytes());
    d.extend([1, 9, 0]);
    d.extend_from_slice(&s.speed_hz.to_le_bytes());
    d.extend([s.bits, s.cpha as u8, s.cpol as u8]);
    d.extend_from_slice(&s.chip_select.to_le_bytes());
    d.extend_from_slice(source(&s.controller).expect("a controller's name without NUL"));
    d.push(0);
    large(0x0E, &d).expect("a descriptor of at most 64 KiB")
}

/// `GpioIo` (or `GpioInt`) with one pin. `flags`: the I/O restriction and
/// sharing (`GpioIo`) or the interrupt's mode, polarity and sharing.
pub fn gpio_descriptor(interrupt: bool, flags: u16, pull: u8, debounce: u16, pin: u16, controller: &str) -> Vec<u8> {
    gpio_connection(interrupt, flags, pull, debounce, &[pin], controller).expect("a controller's name without NUL")
}

/// The `GpioIo` or `GpioInt` [`crate::resource::parse`] reads as `g`.
/// Panics if it cannot be written ([`try_gpio_descriptor_of`]).
pub fn gpio_descriptor_of(g: &Gpio) -> Vec<u8> {
    try_gpio_descriptor_of(g).expect("a GPIO connection a descriptor can hold")
}

/// The `GpioIo` or `GpioInt` [`crate::resource::parse`] reads as `g`, or
/// `None` if a descriptor cannot hold it (a controller's name with a NUL,
/// more pins than 64 KiB of descriptor take).
pub fn try_gpio_descriptor_of(g: &Gpio) -> Option<Vec<u8>> {
    let flags = if g.interrupt {
        g.edge as u16 | (g.polarity as u16 & 3) << 1 | (g.shared as u16) << 3 | (g.wake as u16) << 4
    } else {
        g.restriction as u16 & 3 | (g.shared as u16) << 3
    };
    gpio_connection(g.interrupt, flags, g.pull, g.debounce, &g.pins, &g.controller)
}

fn gpio_connection(
    interrupt: bool,
    flags: u16,
    pull: u8,
    debounce: u16,
    pins: &[u16],
    controller: &str,
) -> Option<Vec<u8>> {
    let controller = source(controller)?;
    let mut d = vec![1, if interrupt { 0 } else { 1 }, 1, 0];
    d.extend_from_slice(&flags.to_le_bytes());
    d.push(pull);
    d.extend_from_slice(&0u16.to_le_bytes());
    d.extend_from_slice(&debounce.to_le_bytes());
    // Pin table right after the fixed part (23 bytes in), then the name,
    // then (none of) the vendor's data: offsets from the descriptor's start.
    let name_at = u16::try_from(pins.len().checked_mul(2)?.checked_add(23)?).ok()?;
    let vendor_at = u16::try_from(name_at as usize + controller.len() + 1).ok()?;
    d.extend_from_slice(&23u16.to_le_bytes());
    d.push(0);
    d.extend_from_slice(&name_at.to_le_bytes());
    d.extend_from_slice(&vendor_at.to_le_bytes());
    d.extend_from_slice(&0u16.to_le_bytes());
    for pin in pins {
        d.extend_from_slice(&pin.to_le_bytes());
    }
    d.extend_from_slice(controller);
    d.push(0);
    large(0x0C, &d)
}

/// `Interrupt (ResourceConsumer, Level, ActiveLow, Shared) { irq }`.
pub fn interrupt(irq: u32) -> Vec<u8> {
    interrupt_of(irq, false, true, true, false)
}

/// `Interrupt (ResourceConsumer, ...) { irq }` as given: edge- or
/// level-triggered, active low or high, shared or not, a wake source or
/// not.
pub fn interrupt_of(irq: u32, edge: bool, active_low: bool, shared: bool, wake: bool) -> Vec<u8> {
    let flags = 1 | (edge as u8) << 1 | (active_low as u8) << 2 | (shared as u8) << 3 | (wake as u8) << 4;
    large(0x09, &cat(&[&[flags, 1], &irq.to_le_bytes()])).expect("a descriptor of a few bytes")
}

/// `I2cSerialBusV2 (address, ControllerInitiated, speed, 7- or 10-bit
/// addressing, controller)`. Panics on a controller's name with a NUL.
pub fn i2c_descriptor(address: u16, speed: u32, ten_bit: bool, controller: &str) -> Vec<u8> {
    let mut d = vec![2, 0, 1, 2];
    d.extend_from_slice(&(ten_bit as u16).to_le_bytes());
    d.push(1);
    d.extend_from_slice(&6u16.to_le_bytes());
    d.extend_from_slice(&speed.to_le_bytes());
    d.extend_from_slice(&address.to_le_bytes());
    d.extend_from_slice(source(controller).expect("a controller's name without NUL"));
    d.push(0);
    large(0x0E, &d).expect("a descriptor of at most 64 KiB")
}

/// `IO (Decode16, base, base, 1, length)`.
pub fn io(base: u16, length: u8) -> Vec<u8> {
    cat(&[&[0x47, 1], &base.to_le_bytes(), &base.to_le_bytes(), &[1, length]])
}

/// `IRQNoFlags () { irq }`: an ISA interrupt (an edge, active high).
pub fn irq_no_flags(irq: u8) -> Vec<u8> {
    cat(&[&[0x22], &(1u16 << (irq & 15)).to_le_bytes()])
}

/// `WordBusNumber (ResourceProducer, MinFixed, MaxFixed, PosDecode, 0,
/// min, max, 0, max - min + 1)`: the buses a bridge decodes.
pub fn word_bus_number(min: u16, max: u16) -> Vec<u8> {
    let fields = [0, min, max, 0, max - min + 1];
    large(0x08, &cat(&[&[2, 0x0C, 0], &fields.iter().flat_map(|f| f.to_le_bytes()).collect::<Vec<u8>>()]))
        .expect("a descriptor of a few bytes")
}

/// `DWordMemory (ResourceProducer, PosDecode, MinFixed, MaxFixed,
/// NonCacheable, ReadWrite, 0, min, max, 0, max - min + 1)`: memory a
/// bridge decodes, below 4 GiB.
pub fn dword_memory(min: u32, max: u32) -> Vec<u8> {
    let fields = [0, min, max, 0, max - min + 1];
    large(0x07, &cat(&[&[0, 0x0C, 1], &fields.iter().flat_map(|f| f.to_le_bytes()).collect::<Vec<u8>>()]))
        .expect("a descriptor of a few bytes")
}

/// `QWordMemory (...)`, as [`dword_memory`] for 64-bit addresses.
pub fn qword_memory(min: u64, max: u64) -> Vec<u8> {
    let fields = [0, min, max, 0, max - min + 1];
    large(0x0A, &cat(&[&[0, 0x0C, 1], &fields.iter().flat_map(|f| f.to_le_bytes()).collect::<Vec<u8>>()]))
        .expect("a descriptor of a few bytes")
}

/// A `ResourceTemplate` of `descriptors`: a buffer, with its end tag.
pub fn resource_template(descriptors: &[u8]) -> Vec<u8> {
    buffer(&cat(&[descriptors, &END_TAG]))
}

/// The end tag of a resource template (no checksum).
pub const END_TAG: [u8; 2] = [0x79, 0x00];
