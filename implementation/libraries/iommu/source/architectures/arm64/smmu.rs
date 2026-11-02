// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! An SMMUv3: its registers and what it reads from memory, as Arm's
//! architecture specification has them.

/// The registers, from an SMMU's base; those of the event queue's indices
/// are a page further, at [`PAGE1`].
pub mod register {
    pub const IDR0: u64 = 0x00;
    pub const IDR1: u64 = 0x04;
    pub const IDR5: u64 = 0x14;
    pub const CR0: u64 = 0x20;
    pub const CR0ACK: u64 = 0x24;
    pub const CR1: u64 = 0x28;
    pub const CR2: u64 = 0x2C;
    pub const IRQ_CTRL: u64 = 0x50;
    pub const IRQ_CTRLACK: u64 = 0x54;
    pub const GERROR: u64 = 0x60;
    pub const GERRORN: u64 = 0x64;
    pub const STRTAB_BASE: u64 = 0x80;
    pub const STRTAB_BASE_CFG: u64 = 0x88;
    pub const CMDQ_BASE: u64 = 0x90;
    pub const CMDQ_PROD: u64 = 0x98;
    pub const CMDQ_CONS: u64 = 0x9C;
    pub const EVENTQ_BASE: u64 = 0xA0;
    /// The second page of registers.
    pub const PAGE1: u64 = 0x1_0000;
    pub const EVENTQ_PROD: u64 = PAGE1 + 0xA8;
    pub const EVENTQ_CONS: u64 = PAGE1 + 0xAC;
}

/// `CR0`: translation, and the queues.
pub const CR0_SMMUEN: u32 = 1 << 0;
pub const CR0_EVENTQEN: u32 = 1 << 2;
pub const CR0_CMDQEN: u32 = 1 << 3;
/// `CR1`: tables and queues in write-back memory, shared with the
/// processors.
pub const CR1_WRITE_BACK: u32 = 1 | 1 << 2 | 3 << 4 | 1 << 6 | 1 << 8 | 3 << 10;
/// `CR2`: invalid stream ids are reported.
pub const CR2_RECINVSID: u32 = 1 << 1;
/// `IRQ_CTRL`: the interrupts of global errors and of the event queue.
pub const IRQ_GERROR: u32 = 1 << 0;
pub const IRQ_EVENTQ: u32 = 1 << 2;

/// The address of a queue or table in its `*_BASE` register, as much of it
/// as holds it, and the read-allocate hint.
const BASE_ADDRESS: u64 = 0x000F_FFFF_FFFF_FFE0;
const BASE_READ_ALLOCATE: u64 = 1 << 62;

/// What `IDR0` to `IDR5` say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// Stage 2 translation, of the AArch64 format.
    pub stage2: bool,
    /// Its walks see the processors' caches.
    pub coherent: bool,
    /// 16-bit VMIDs (otherwise 8-bit).
    pub vmid16: bool,
    /// Bits of stream ids, of the command queue's and of the event queue's
    /// entries.
    pub stream_bits: u32,
    pub command_bits: u32,
    pub event_bits: u32,
    /// 4 KiB granules, and how wide output addresses are (`OAS`).
    pub granule_4k: bool,
    pub output_size: u32,
}

impl Capabilities {
    /// Reads them from `IDR0`, `IDR1` and `IDR5`.
    pub fn read(idr0: u32, idr1: u32, idr5: u32) -> Capabilities {
        Capabilities {
            stage2: idr0 & 1 != 0 && (idr0 >> 2) & 3 != 1,
            coherent: idr0 & (1 << 4) != 0,
            vmid16: idr0 & (1 << 18) != 0,
            stream_bits: idr1 & 0x3F,
            command_bits: (idr1 >> 21) & 0x1F,
            event_bits: (idr1 >> 16) & 0x1F,
            granule_4k: idr5 & (1 << 4) != 0,
            output_size: idr5 & 7,
        }
    }

    /// How wide addresses are through stage 2.
    pub fn address_bits(&self) -> u32 {
        address_bits(self.output_size)
    }
}

/// The addresses `OAS` says, as far as four levels of 4 KiB granules go.
fn address_bits(output: u32) -> u32 {
    match output {
        0 => 32,
        1 => 36,
        2 => 40,
        3 => 42,
        4 => 44,
        _ => 48,
    }
}

/// What a `*_BASE` register holds of a queue of 2^`bits` entries at
/// `address`.
pub fn queue_base(address: u64, bits: u32) -> u64 {
    BASE_READ_ALLOCATE | (address & BASE_ADDRESS) | bits as u64
}

/// What `STRTAB_BASE` holds of a table at `address`.
pub fn table_base(address: u64) -> u64 {
    BASE_READ_ALLOCATE | (address & 0x000F_FFFF_FFFF_FFC0)
}

/// What `STRTAB_BASE_CFG` holds of a linear table of 2^`bits` entries.
pub fn table_config(bits: u32) -> u32 {
    bits
}

/// A stream table entry's 8 words.
pub type Ste = [u64; 8];
/// The size of a stream table entry.
pub const STE_BYTES: u64 = 64;

const STE_VALID: u64 = 1 << 0;
const STE_CONFIG: u32 = 1;
const STE_CONFIG_ABORT: u64 = 0;
const STE_CONFIG_BYPASS: u64 = 4;
const STE_CONFIG_STAGE2: u64 = 6;
/// Word 1, what a stream that bypasses keeps: its own shareability, as the
/// device says it.
const STE_SHCFG_INCOMING: u64 = 1 << 44;

/// A stream that goes through untranslated.
pub const fn ste_bypass() -> Ste {
    [STE_VALID | STE_CONFIG_BYPASS << STE_CONFIG, STE_SHCFG_INCOMING, 0, 0, 0, 0, 0, 0]
}

/// A stream that reaches nothing.
pub const fn ste_abort() -> Ste {
    [STE_VALID | STE_CONFIG_ABORT << STE_CONFIG, 0, 0, 0, 0, 0, 0, 0]
}

/// A stream translated by the stage 2 tables at `root`, of VMID `vmid`:
/// four levels of 4 KiB granules, write-back tables shared with the
/// processors, 64-bit, faults recorded. `output` is the SMMU's `OAS`: its
/// addresses in, which stage 2 cannot have wider than those out, are as
/// wide ([`Capabilities::address_bits`]).
pub fn ste_stage2(root: u64, vmid: u16, output: u32) -> Ste {
    let t0sz = 64 - address_bits(output) as u64;
    const START_LEVEL_0: u64 = 2;
    const WRITE_BACK: u64 = 1;
    const INNER_SHAREABLE: u64 = 3;
    let word2 = vmid as u64
        | t0sz << 32
        | START_LEVEL_0 << 38
        | WRITE_BACK << 40
        | WRITE_BACK << 42
        | INNER_SHAREABLE << 44
        | (output.min(5) as u64) << 48
        | 1 << 51 // S2AA64
        | 1 << 58; // S2R
    [STE_VALID | STE_CONFIG_STAGE2 << STE_CONFIG, 0, word2, root & 0x000F_FFFF_FFFF_FFF0, 0, 0, 0, 0]
}

/// Commands.
pub mod command {
    /// A command's two words.
    pub type Command = [u64; 2];

    const CFGI_STE: u64 = 0x03;
    const CFGI_ALL: u64 = 0x04;
    const TLBI_NSNH_ALL: u64 = 0x30;
    const TLBI_S12_VMALL: u64 = 0x28;
    const SYNC: u64 = 0x46;

    /// The SMMU forgets what it read of `stream`'s entry.
    pub const fn forget_stream(stream: u32) -> Command {
        [CFGI_STE | (stream as u64) << 32, 1]
    }

    /// The SMMU forgets what it read of every stream.
    pub const fn forget_streams() -> Command {
        [CFGI_ALL, 31]
    }

    /// The SMMU forgets every translation.
    pub const fn forget_translations() -> Command {
        [TLBI_NSNH_ALL, 0]
    }

    /// The SMMU forgets the translations of VMID `vmid`.
    pub const fn forget_vmid(vmid: u16) -> Command {
        [TLBI_S12_VMALL | (vmid as u64) << 32, 0]
    }

    /// Everything before is done when the queue has got past this.
    pub const fn sync() -> Command {
        [SYNC, 0]
    }
}

/// Stage 2 table entries, as the processors' own.
pub mod stage2 {
    pub const VALID: u64 = 1 << 0;
    /// In an entry of levels 3 to 1, a table below; of level 0, always set:
    /// a page.
    pub const TABLE_OR_PAGE: u64 = 1 << 1;
    /// Normal write-back memory; device registers.
    pub const WRITE_BACK: u64 = 0b1111 << 2;
    pub const DEVICE: u64 = 0b0001 << 2;
    pub const READ: u64 = 1 << 6;
    pub const WRITE: u64 = 1 << 7;
    pub const INNER_SHAREABLE: u64 = 3 << 8;
    pub const ACCESSED: u64 = 1 << 10;
    pub const ADDRESS: u64 = 0x0000_FFFF_FFFF_F000;
}

/// A fault event, from the event queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Event {
    /// What it was (`F_*`).
    pub kind: u8,
    pub stream: u32,
    /// The address the device reached.
    pub address: u64,
    /// The access was a write.
    pub write: bool,
}

/// `F_TRANSLATION`, `F_ADDR_SIZE`, `F_ACCESS`, `F_PERMISSION`, the faults a
/// device's reach makes; `C_BAD_STE`, a stream that reaches nothing because
/// its entry says so.
pub const F_STREAM_DISABLED: u8 = 0x06;
pub const F_TRANSLATION: u8 = 0x10;

/// Reads a 32-byte event.
pub fn event(words: [u64; 4]) -> Event {
    Event { kind: words[0] as u8, stream: (words[0] >> 32) as u32, address: words[2], write: words[1] & (1 << 35) != 0 }
}

/// What an event means.
pub fn event_name(kind: u8) -> &'static str {
    match kind {
        0x01 => "an unknown event",
        0x02 => "a stream out of range",
        0x04 => "a bad stream table entry",
        0x05 => "a bad stream table access",
        F_STREAM_DISABLED => "a stream that reaches nothing",
        F_TRANSLATION => "no translation",
        0x11 => "an address too large",
        0x12 => "no access to the page",
        0x13 => "no permission",
        _ => "a fault",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_are_read_as_the_specification_has_them() {
        // Stage 1 and 2, AArch64 tables, coherent, 16-bit VMIDs; 16-bit
        // streams, queues of 2^7 and 2^19; 4 KiB granules, 48 bits.
        let c = Capabilities::read(1 | 1 << 1 | 2 << 2 | 1 << 4 | 1 << 18, 16 | 19 << 16 | 7 << 21, 5 | 1 << 4);
        assert!(c.stage2 && c.coherent && c.vmid16 && c.granule_4k);
        assert_eq!((c.stream_bits, c.event_bits, c.command_bits, c.output_size), (16, 19, 7, 5));
        // A table format of AArch32 alone is no use.
        assert!(!Capabilities::read(1 | 1 << 2, 0, 0).stage2);
    }

    #[test]
    fn a_bypassing_stream_is_valid_and_keeps_its_attributes() {
        let ste = ste_bypass();
        assert_eq!(ste[0] & 0xF, 1 | 4 << 1);
        assert_eq!(ste[1], STE_SHCFG_INCOMING);
    }

    #[test]
    fn a_stream_that_reaches_nothing_aborts() {
        assert_eq!(ste_abort()[0] & 0xF, 1);
    }

    #[test]
    fn a_translated_stream_names_its_tables() {
        // QEMU's: 44 bits' worth, and the input with them.
        assert_eq!((ste_stage2(0x8000_1000, 7, 4)[2] >> 32) & 0x3F, 20);
        assert_eq!((ste_stage2(0x8000_1000, 7, 6)[2] >> 32) & 0x3F, 16);
        let ste = ste_stage2(0x8000_1000, 7, 5);
        assert_eq!(ste[0] & 0xF, 1 | 6 << 1);
        assert_eq!(ste[2] & 0xFFFF, 7);
        assert_eq!((ste[2] >> 32) & 0x3F, 16);
        assert_eq!((ste[2] >> 48) & 7, 5);
        assert_eq!(ste[3], 0x8000_1000);
    }

    #[test]
    fn queues_name_their_address_and_size() {
        assert_eq!(queue_base(0x8000_0000, 8) & 0xFFFF_FFFF, 0x8000_0008);
        assert_eq!(queue_base(0x8000_0000, 8) >> 62, 1);
    }

    #[test]
    fn an_event_says_what_reached_what() {
        let e = event([0x10 | 0x208 << 32, 1 << 35, 0x4000_1000, 0]);
        assert_eq!(e, Event { kind: F_TRANSLATION, stream: 0x208, address: 0x4000_1000, write: true });
    }
}
