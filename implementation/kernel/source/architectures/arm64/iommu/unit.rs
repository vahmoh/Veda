// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The SMMU itself: its registers, stream table and queues.

use alloc::vec::Vec;

use iommu::arm64::smmu::command::{self, Command};
use iommu::arm64::smmu::register as reg;
use iommu::arm64::smmu::{self, Capabilities, Event, STE_BYTES, Ste};

use crate::architecture::arm64::memory::flush_cache_range;
use crate::memory::kvirt;
use crate::memory::paging::Cache;
use crate::memory::{phys, phys_to_virt};
use crate::synchronization::SpinLock;

/// How long registers and the queue may keep the kernel waiting (spins).
const SPINS: u32 = 10_000_000;
/// The queues' sizes, at most: 256 commands, 256 events.
const QUEUE_BITS: u32 = 8;
const COMMAND_BYTES: u64 = 16;
const EVENT_BYTES: u64 = 32;
/// Streams, at most: the 16 bits of a requester id.
const STREAM_BITS: u32 = 16;
/// The narrowest addresses four levels of tables start from level 0 with.
const ADDRESS_BITS_LEAST: u32 = 40;

pub struct Unit {
    pub capabilities: Capabilities,
    base: u64,
    /// It reads memory around the caches.
    flush: bool,
    /// The stream table, a frame's physical address, and its entries.
    table: u64,
    streams: u32,
    /// Streams that are in a guest's domain.
    translated: SpinLock<Vec<u32>>,
    commands: SpinLock<Commands>,
    events: SpinLock<Events>,
}

struct Commands {
    base: u64,
    bits: u32,
    /// Where the next one goes, with the wrap bit above.
    prod: u32,
}

struct Events {
    base: u64,
    bits: u32,
    cons: u32,
}

impl Unit {
    fn read32(&self, register: u64) -> u32 {
        // SAFETY: the SMMU's registers, mapped as a device's.
        unsafe { core::ptr::read_volatile((self.base + register) as *const u32) }
    }

    fn write32(&self, register: u64, value: u32) {
        // SAFETY: the SMMU's registers, mapped as a device's.
        unsafe { core::ptr::write_volatile((self.base + register) as *mut u32, value) };
    }

    fn write64(&self, register: u64, value: u64) {
        // SAFETY: the SMMU's registers, mapped as a device's.
        unsafe { core::ptr::write_volatile((self.base + register) as *mut u64, value) };
    }

    fn publish(&self, phys: u64, len: u64) {
        if self.flush {
            flush_cache_range(phys_to_virt(phys), len);
        } else {
            abi::barrier::device_write();
        }
    }

    /// Whether its walks see the processors' caches.
    pub fn coherent(&self) -> bool {
        !self.flush
    }

    /// Writes `CR0`, and waits until it has taken.
    fn set_control(&self, value: u32) -> Result<(), &'static str> {
        self.write32(reg::CR0, value);
        for _ in 0..SPINS {
            if self.read32(reg::CR0ACK) == value {
                return Ok(());
            }
            core::hint::spin_loop();
        }
        Err("the SMMU does not take its control register")
    }

    /// Sets the SMMU up, every stream going through.
    pub fn new(phys_base: u64, coherent: bool) -> Result<Unit, &'static str> {
        let no_memory = "no memory for its tables";
        let base = kvirt::map_mmio(phys_base, 2 * reg::PAGE1, Cache::Uncached);
        // SAFETY: the SMMU's registers, mapped as a device's.
        let read = |r: u64| unsafe { core::ptr::read_volatile((base + r) as *const u32) };
        let capabilities = Capabilities::read(read(reg::IDR0), read(reg::IDR1), read(reg::IDR5));
        if !capabilities.stage2 {
            return Err("the SMMU has no stage 2 of the AArch64 format");
        }
        if !capabilities.granule_4k {
            return Err("the SMMU has no 4 KiB granules");
        }
        if capabilities.address_bits() < ADDRESS_BITS_LEAST {
            return Err("the SMMU's addresses are under 40 bits");
        }
        let bits = capabilities.stream_bits.min(STREAM_BITS);
        let streams = 1u32 << bits;
        let table_pages = (streams as u64 * STE_BYTES).div_ceil(4096);
        let table = phys::alloc_contiguous(table_pages, 1 << 22, u64::MAX).ok_or(no_memory)?;
        let command_bits = capabilities.command_bits.min(QUEUE_BITS);
        let event_bits = capabilities.event_bits.min(QUEUE_BITS);
        // On a boundary of their own size: the SMMU ignores the base's bits
        // below it.
        let queue = |bytes: u64| {
            phys::alloc_contiguous(bytes.div_ceil(4096), bytes.next_multiple_of(4096), u64::MAX).ok_or(no_memory)
        };
        let commands = queue(COMMAND_BYTES << command_bits)?;
        let events = queue(EVENT_BYTES << event_bits)?;
        let unit = Unit {
            capabilities,
            base,
            flush: !(coherent && capabilities.coherent),
            table,
            streams,
            translated: SpinLock::new(Vec::new()),
            commands: SpinLock::new(Commands { base: commands, bits: command_bits, prod: 0 }),
            events: SpinLock::new(Events { base: events, bits: event_bits, cons: 0 }),
        };
        unit.set_control(0)?;
        for stream in 0..streams {
            unit.store(stream, &smmu::ste_bypass());
        }
        unit.publish(table, streams as u64 * STE_BYTES);
        unit.write32(reg::CR1, smmu::CR1_WRITE_BACK);
        unit.write32(reg::CR2, smmu::CR2_RECINVSID);
        unit.write64(reg::STRTAB_BASE, smmu::table_base(table));
        unit.write32(reg::STRTAB_BASE_CFG, smmu::table_config(bits));
        unit.write64(reg::CMDQ_BASE, smmu::queue_base(commands, command_bits));
        unit.write32(reg::CMDQ_PROD, 0);
        unit.write32(reg::CMDQ_CONS, 0);
        unit.write64(reg::EVENTQ_BASE, smmu::queue_base(events, event_bits));
        unit.write32(reg::EVENTQ_PROD, 0);
        unit.write32(reg::EVENTQ_CONS, 0);
        unit.set_control(smmu::CR0_CMDQEN | smmu::CR0_EVENTQEN)?;
        unit.command(&[command::forget_streams(), command::forget_translations()])?;
        unit.set_control(smmu::CR0_CMDQEN | smmu::CR0_EVENTQEN | smmu::CR0_SMMUEN)?;
        Ok(unit)
    }

    /// Turns the interrupts on, whose lines are routed.
    pub fn enable_interrupts(&self) {
        let on = smmu::IRQ_GERROR | smmu::IRQ_EVENTQ;
        self.write32(reg::IRQ_CTRL, on);
        for _ in 0..SPINS {
            if self.read32(reg::IRQ_CTRLACK) == on {
                return;
            }
            core::hint::spin_loop();
        }
        crate::kwarn!("iommu: the SMMU does not take its interrupts' control");
    }

    /// Queues `commands` and a sync, and waits until the SMMU has done them.
    pub fn command(&self, commands: &[Command]) -> Result<(), &'static str> {
        let mut q = self.commands.lock();
        let entries = 1u32 << q.bits;
        let index = entries - 1;
        for c in commands.iter().chain(&[command::sync()]) {
            let at = q.base + ((q.prod & index) as u64) * COMMAND_BYTES;
            // SAFETY: an entry of the queue, whose frames are the SMMU's
            // and this code's.
            unsafe { core::ptr::write_volatile(phys_to_virt(at) as *mut Command, *c) };
            self.publish(at, COMMAND_BYTES);
            // The index, then the wrap bit above it.
            q.prod = (q.prod + 1) & (entries * 2 - 1);
            self.write32(reg::CMDQ_PROD, q.prod);
            // The SMMU keeps up: the queue is emptied before it can fill.
            for spin in 0..=SPINS {
                let cons = self.read32(reg::CMDQ_CONS) & (entries * 2 - 1);
                if cons == q.prod {
                    break;
                }
                if spin == SPINS {
                    return Err("the SMMU does not go through its commands");
                }
                core::hint::spin_loop();
            }
        }
        let error = self.read32(reg::GERROR) ^ self.read32(reg::GERRORN);
        if error & 1 != 0 { Err("the SMMU refused a command") } else { Ok(()) }
    }

    /// The entry of `stream`.
    fn entry(&self, stream: u32) -> *mut Ste {
        phys_to_virt(self.table + stream as u64 * STE_BYTES) as *mut Ste
    }

    /// Writes all of `stream`'s entry, as nothing reads it yet.
    fn store(&self, stream: u32, ste: &Ste) {
        // SAFETY: an entry of the table, of the frames the unit owns.
        unsafe { core::ptr::write_volatile(self.entry(stream), *ste) };
    }

    /// Changes `stream`'s entry to `ste` while the SMMU may be reading it:
    /// invalid first, so that it never reads half of it, then the rest,
    /// then the word that makes it valid.
    pub fn set_stream(&self, stream: u32, ste: &Ste) -> Result<(), &'static str> {
        if stream >= self.streams {
            return Err("a stream beyond the table");
        }
        let entry = self.entry(stream) as *mut u64;
        let at = self.table + stream as u64 * STE_BYTES;
        // SAFETY (all three): words of the entry.
        unsafe { core::ptr::write_volatile(entry, 0) };
        self.publish(at, STE_BYTES);
        self.command(&[command::forget_stream(stream)])?;
        for (i, &word) in ste.iter().enumerate().skip(1) {
            unsafe { core::ptr::write_volatile(entry.add(i), word) };
        }
        self.publish(at, STE_BYTES);
        unsafe { core::ptr::write_volatile(entry, ste[0]) };
        self.publish(at, STE_BYTES);
        self.command(&[command::forget_stream(stream)])
    }

    /// Whether `stream` is in a guest's domain.
    pub fn is_translated(&self, stream: u32) -> bool {
        self.translated.lock().contains(&stream)
    }

    /// Records whether `stream` is in a guest's domain.
    pub fn set_translated(&self, stream: u32, translated: bool) {
        let mut streams = self.translated.lock();
        streams.retain(|&s| s != stream);
        if translated {
            streams.push(stream);
        }
    }

    /// Calls `report` on each event in the queue, and acknowledges its
    /// errors.
    pub fn take_events(&self, mut report: impl FnMut(Event)) {
        let mut q = self.events.lock();
        let entries = 1u32 << q.bits;
        let (index, mask) = (entries - 1, entries * 2 - 1);
        loop {
            let prod = self.read32(reg::EVENTQ_PROD) & mask;
            if prod == q.cons {
                break;
            }
            let at = phys_to_virt(q.base + ((q.cons & index) as u64) * EVENT_BYTES);
            if self.flush {
                flush_cache_range(at, EVENT_BYTES);
            }
            // The entry, after the index that says it is there.
            abi::barrier::device_read();
            // SAFETY: an entry of the queue, which the SMMU has written.
            let words = unsafe { core::ptr::read_volatile(at as *const [u64; 4]) };
            // And before the index that gives its room back.
            abi::barrier::device_read();
            report(smmu::event(words));
            q.cons = (q.cons + 1) & mask;
            self.write32(reg::EVENTQ_CONS, q.cons);
        }
        drop(q);
        // Each error is acknowledged by flipping its bit to match.
        self.write32(reg::GERRORN, self.read32(reg::GERROR));
    }
}
