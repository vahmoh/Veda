// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Flattened device trees, which describe an ARM64 guest to Linux, as ACPI
//! tables do an AMD64 one: a builder of the format (version 17) of the
//! Devicetree Specification, with no memory reservations, and the
//! [`describe`] of a guest.

use alloc::vec::Vec;

use super::memory::{GIC_DISTRIBUTOR, GIC_DISTRIBUTOR_SIZE, GIC_REDISTRIBUTOR_SIZE, GIC_REDISTRIBUTORS};
use super::platform::{CALLBACK_INTID, COMPATIBLE, affinity_of};

const MAGIC: u32 = 0xD00D_FEED;
const VERSION: u32 = 17;
const LAST_COMPATIBLE: u32 = 16;
const HEADER: usize = 40;
/// The empty list of memory reservations.
const RESERVATIONS: usize = 16;

const TOKEN_BEGIN_NODE: u32 = 1;
const TOKEN_END_NODE: u32 = 2;
const TOKEN_PROP: u32 = 3;
const TOKEN_END: u32 = 9;

/// A device tree being written.
#[derive(Debug, Default)]
pub struct Builder {
    structure: Vec<u8>,
    strings: Vec<u8>,
}

impl Builder {
    fn token(&mut self, token: u32) {
        self.structure.extend(token.to_be_bytes());
    }

    fn align(&mut self) {
        while !self.structure.len().is_multiple_of(4) {
            self.structure.push(0);
        }
    }

    /// Begins a node named `name` (`""` for the root).
    pub fn begin(&mut self, name: &str) {
        self.token(TOKEN_BEGIN_NODE);
        self.structure.extend(name.as_bytes());
        self.structure.push(0);
        self.align();
    }

    /// Ends the node last begun and not ended.
    pub fn end(&mut self) {
        self.token(TOKEN_END_NODE);
    }

    /// Where `name` is among the strings, added if it is not there.
    fn string(&mut self, name: &str) -> u32 {
        let mut needle = alloc::vec::Vec::with_capacity(name.len() + 1);
        needle.extend(name.as_bytes());
        needle.push(0);
        // Only from the start of a string, or it would be another's end,
        // which a parser reads the same: any match will do.
        if let Some(at) = self.strings.windows(needle.len()).position(|w| w == needle) {
            return at as u32;
        }
        let at = self.strings.len() as u32;
        self.strings.extend(needle);
        at
    }

    /// A property of bytes `value`.
    pub fn bytes(&mut self, name: &str, value: &[u8]) {
        let at = self.string(name);
        self.token(TOKEN_PROP);
        self.structure.extend((value.len() as u32).to_be_bytes());
        self.structure.extend(at.to_be_bytes());
        self.structure.extend(value);
        self.align();
    }

    /// A property with no value.
    pub fn flag(&mut self, name: &str) {
        self.bytes(name, &[]);
    }

    /// A string property.
    pub fn string_property(&mut self, name: &str, value: &str) {
        let mut bytes = Vec::with_capacity(value.len() + 1);
        bytes.extend(value.as_bytes());
        bytes.push(0);
        self.bytes(name, &bytes);
    }

    /// A property of 32-bit cells.
    pub fn cells(&mut self, name: &str, value: &[u32]) {
        let bytes: Vec<u8> = value.iter().flat_map(|c| c.to_be_bytes()).collect();
        self.bytes(name, &bytes);
    }

    /// A property of 64-bit values, two cells each.
    pub fn quads(&mut self, name: &str, value: &[u64]) {
        let bytes: Vec<u8> = value.iter().flat_map(|c| c.to_be_bytes()).collect();
        self.bytes(name, &bytes);
    }

    /// The finished blob.
    pub fn finish(mut self) -> Vec<u8> {
        self.token(TOKEN_END);
        let off_struct = HEADER + RESERVATIONS;
        let off_strings = off_struct + self.structure.len();
        let total = off_strings + self.strings.len();
        let header = [
            MAGIC,
            total as u32,
            off_struct as u32,
            off_strings as u32,
            HEADER as u32,
            VERSION,
            LAST_COMPATIBLE,
            0,
            self.strings.len() as u32,
            self.structure.len() as u32,
        ];
        let mut blob = Vec::with_capacity(total);
        blob.extend(header.iter().flat_map(|w| w.to_be_bytes()));
        blob.resize(off_struct, 0);
        blob.extend(&self.structure);
        blob.extend(&self.strings);
        blob
    }
}

/// The GIC's handle (`phandle`) in the tree.
const GIC_PHANDLE: u32 = 1;

/// The cells of an interrupt: GICv3's, SPI (0) or PPI (1), its number
/// among them, and its flags.
const GIC_SPI: u32 = 0;
const GIC_PPI: u32 = 1;
/// Rising edge, high level.
const EDGE: u32 = 1;
const LEVEL: u32 = 4;

/// The PPIs of the architected timers: secure, non-secure EL1 physical,
/// virtual, EL2 physical, as `interrupts` of `arm,armv8-timer` lists them.
const TIMER_PPIS: [u32; 4] = [13, 14, 11, 10];

/// What the device tree says of a guest.
#[derive(Debug, Clone, Copy)]
pub struct Machine<'a> {
    /// Its processors.
    pub cpus: u32,
    /// Its RAM, from 0.
    pub memory: u64,
    /// Linux's command line.
    pub cmdline: &'a str,
    /// The initial RAM file system: where it starts and ends.
    pub initramfs: (u64, u64),
    /// Where BARs of PCI functions go: below 4 GiB, and above.
    pub pci_low: (u64, u64),
    pub pci_high: (u64, u64),
    /// The PCI functions it has: where each is on bus 0 (device << 3 |
    /// function).
    pub functions: &'a [u8],
}

/// The device tree of `machine`.
pub fn describe(machine: &Machine<'_>) -> Vec<u8> {
    let mut t = Builder::default();
    t.begin("");
    t.string_property("compatible", "veda,driver-vm");
    t.string_property("model", "Veda's driver VM");
    t.cells("#address-cells", &[2]);
    t.cells("#size-cells", &[2]);
    t.cells("interrupt-parent", &[GIC_PHANDLE]);

    t.begin("chosen");
    t.string_property("bootargs", machine.cmdline);
    t.quads("linux,initrd-start", &[machine.initramfs.0]);
    t.quads("linux,initrd-end", &[machine.initramfs.1]);
    t.end();

    t.begin("memory@0");
    t.string_property("device_type", "memory");
    t.quads("reg", &[0, machine.memory]);
    t.end();

    t.begin("cpus");
    t.cells("#address-cells", &[2]);
    t.cells("#size-cells", &[0]);
    for cpu in 0..machine.cpus {
        let affinity = affinity_of(cpu as u64);
        t.begin(&alloc::format!("cpu@{affinity:x}"));
        t.string_property("device_type", "cpu");
        t.string_property("compatible", "arm,armv8");
        t.quads("reg", &[affinity]);
        t.string_property("enable-method", "psci");
        t.end();
    }
    t.end();

    t.begin("psci");
    t.string_property("compatible", "arm,psci-1.0");
    t.string_property("method", "hvc");
    t.end();

    t.begin("timer");
    t.string_property("compatible", "arm,armv8-timer");
    let interrupts: alloc::vec::Vec<u32> = TIMER_PPIS.iter().flat_map(|&ppi| [GIC_PPI, ppi, LEVEL]).collect();
    t.cells("interrupts", &interrupts);
    t.end();

    t.begin(&alloc::format!("interrupt-controller@{GIC_DISTRIBUTOR:x}"));
    t.string_property("compatible", "arm,gic-v3");
    t.flag("interrupt-controller");
    t.cells("#interrupt-cells", &[3]);
    t.cells("phandle", &[GIC_PHANDLE]);
    t.cells("#redistributor-regions", &[1]);
    t.quads(
        "reg",
        &[GIC_DISTRIBUTOR, GIC_DISTRIBUTOR_SIZE, GIC_REDISTRIBUTORS, machine.cpus as u64 * GIC_REDISTRIBUTOR_SIZE],
    );
    t.end();

    t.begin("hypervisor");
    t.string_property("compatible", COMPATIBLE);
    t.cells("interrupts", &[GIC_SPI, CALLBACK_INTID - 32, EDGE]);
    t.end();

    t.begin(&alloc::format!("pci@{:x}", machine.pci_low.0));
    t.string_property("compatible", "veda,pci");
    t.string_property("device_type", "pci");
    t.cells("#address-cells", &[3]);
    t.cells("#size-cells", &[2]);
    t.cells("bus-range", &[0, 0xFF]);
    // The BARs stay where the monitor mapped them.
    t.cells("linux,pci-probe-only", &[1]);
    let range = |flags: u32, (start, end): (u64, u64)| {
        let size = end - start;
        [
            flags,
            (start >> 32) as u32,
            start as u32,
            (start >> 32) as u32,
            start as u32,
            (size >> 32) as u32,
            size as u32,
        ]
    };
    // 32-bit and 64-bit memory, each where the function's BARs are.
    let ranges: alloc::vec::Vec<u32> =
        [range(0x0200_0000, machine.pci_low), range(0x0300_0000, machine.pci_high)].concat();
    t.cells("ranges", &ranges);
    // Each function, a node of its own that Linux takes for its
    // description, as a PC's guest has an ACPI device of each: where it is,
    // in the first cell of `reg` (its bus, 0, from bit 16, then device and
    // function), with no space of its own.
    for &devfn in machine.functions {
        let (device, function) = (devfn >> 3, devfn & 7);
        t.begin(&if function == 0 {
            alloc::format!("function@{device:x}")
        } else {
            alloc::format!("function@{device:x},{function:x}")
        });
        t.cells("reg", &[(devfn as u32) << 8, 0, 0, 0, 0]);
        t.end();
    }
    t.end();

    t.end();
    t.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(blob: &[u8], at: usize) -> u32 {
        u32::from_be_bytes([blob[at], blob[at + 1], blob[at + 2], blob[at + 3]])
    }

    fn contains(blob: &[u8], text: &[u8]) -> bool {
        blob.windows(text.len()).any(|w| w == text)
    }

    #[test]
    fn a_tree_has_the_formats_header_and_ends_with_its_strings() {
        let mut t = Builder::default();
        t.begin("");
        t.string_property("model", "x");
        t.string_property("model", "y");
        t.end();
        let blob = t.finish();
        assert_eq!(word(&blob, 0), MAGIC);
        assert_eq!(word(&blob, 4) as usize, blob.len());
        assert_eq!(word(&blob, 20), VERSION);
        assert_eq!(word(&blob, 24), LAST_COMPATIBLE);
        // The structure holds the root's two properties, then the end.
        let (structure, strings) = (word(&blob, 8) as usize, word(&blob, 12) as usize);
        assert_eq!(word(&blob, structure), TOKEN_BEGIN_NODE);
        assert_eq!(word(&blob, strings - 4), TOKEN_END);
        assert_eq!(word(&blob, 36) as usize, strings - structure);
        // The name is kept once.
        assert_eq!(&blob[strings..], b"model\0");
    }

    #[test]
    fn a_guest_is_described_as_linux_reads_it() {
        let blob = describe(&Machine {
            cpus: 2,
            memory: 256 << 20,
            cmdline: "console=hvc0",
            initramfs: (0x800_0000, 0x900_0000),
            pci_low: (0xC000_0000, 0xFEC0_0000),
            pci_high: (128 << 30, 256 << 30),
            functions: &[3 << 3, 6 << 3 | 1],
        });
        for text in
            [&b"console=hvc0\0"[..], b"cpu@0\0", b"cpu@1\0", b"arm,gic-v3\0", b"arm,armv8-timer\0", b"veda,platform\0"]
        {
            assert!(contains(&blob, text), "{}", core::str::from_utf8(text).unwrap());
        }
        assert!(blob.len() < 4096);
    }

    #[test]
    fn each_function_is_a_node_where_it_is_on_the_bus() {
        let machine = |functions| Machine {
            cpus: 1,
            memory: 256 << 20,
            cmdline: "",
            initramfs: (0x800_0000, 0x900_0000),
            pci_low: (0xC000_0000, 0xFEC0_0000),
            pci_high: (128 << 30, 256 << 30),
            functions,
        };
        let blob = describe(&machine(&[3 << 3, 6 << 3 | 1]));
        // A function 0 by its device alone, as the PCI binding writes it.
        assert!(contains(&blob, b"function@3\0"));
        assert!(contains(&blob, b"function@6,1\0"));
        // `reg`'s first cell, then the four that are 0.
        let reg = |devfn: u32| [(devfn << 8).to_be_bytes(), [0; 4], [0; 4], [0; 4], [0; 4]].concat();
        assert!(contains(&blob, &reg(3 << 3)));
        assert!(contains(&blob, &reg(6 << 3 | 1)));
        assert!(!contains(&describe(&machine(&[])), b"function@"));
    }
}
