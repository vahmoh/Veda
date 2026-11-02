// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The GIC's Interrupt Translation Service: how a device's messages become
//! interrupts.
//!
//! A message is a write to the ITS's `GITS_TRANSLATER`, which the machine
//! stamps with the requester's DeviceID, whatever the device says, and
//! whose data is an EventID of that device. The ITS looks up the pair, so a
//! device raises only its own interrupts, whatever it or a guest that drives
//! it writes there: what a PC's IOMMU does for its MSIs with remapping.
//!
//! Each device is given a table of events (its ITT) when it asks for its
//! first message. Event V of a device raises vector V, an LPI
//! (`gic::message_intid`), of the boot processor. The ITS is driven through
//! a queue of commands in memory.
//!
//! The DeviceID of a PCI function is taken to be its requester id, as on
//! machines whose root ports map them one to one, as QEMU's do; the first
//! ITS the firmware lists takes them all.

use alloc::vec::Vec;

use super::gic;
use super::memory::flush_cache_range;
use crate::memory::kvirt;
use crate::memory::paging::Cache;
use crate::memory::{phys, phys_to_virt};
use crate::synchronization::{Once, SpinLock};

/// The registers.
const GITS_CTLR: u64 = 0x0000;
const GITS_TYPER: u64 = 0x0008;
const GITS_CBASER: u64 = 0x0080;
const GITS_CWRITER: u64 = 0x0088;
const GITS_CREADR: u64 = 0x0090;
const GITS_BASER: u64 = 0x0100;
const GITS_TRANSLATER: u64 = 0x1_0040;
/// `GITS_CTLR`: on.
const ENABLED: u32 = 1;

/// `GITS_BASER`'s table types.
const BASER_DEVICES: u64 = 1;
const BASER_COLLECTIONS: u64 = 4;
/// `GITS_BASER`: in use.
const BASER_VALID: u64 = 1 << 63;
/// `GITS_BASER`'s size of pages.
const PAGE_SIZE_AT: u32 = 8;
const PAGE_SIZE_FIELD: u64 = 3 << PAGE_SIZE_AT;
/// `GITS_CREADR`: a command failed, and the ITS waits.
const CREADR_STALLED: u64 = 1;
/// Tables and the queue are write-back, shared with the processors.
const ATTRIBUTES: u64 = 7 << 59 | 1 << 10;

/// The size of the command queue, in 4 KiB pages.
const QUEUE_PAGES: u64 = 16;
const QUEUE_BYTES: u64 = QUEUE_PAGES * 4096;
const COMMAND_BYTES: u64 = 32;

/// Commands.
const CMD_SYNC: u64 = 0x05;
const CMD_MAPD: u64 = 0x08;
const CMD_MAPC: u64 = 0x09;
const CMD_MAPTI: u64 = 0x0A;
const CMD_INV: u64 = 0x0C;
const CMD_DISCARD: u64 = 0x0F;
/// The valid bit of `MAPD` and `MAPC`.
const VALID: u64 = 1 << 63;

/// The only collection: the boot processor's.
const COLLECTION: u64 = 0;
/// Events each device has: one per vector.
const EVENT_BITS: u64 = 8;

/// An LPI's configuration: on, at the priority of everything.
const LPI_ENABLED: u8 = 0xA0 | 1;

struct Its {
    /// The registers, mapped, and where they really are.
    base: u64,
    translater: u64,
    /// The bytes an ITT takes per event.
    event_bytes: u64,
    /// The boot processor's redistributor, as commands name it.
    target: u64,
    /// The configuration table of the LPIs (frames in the direct map).
    properties: u64,
    queue: SpinLock<Queue>,
    /// What the ITS knows.
    state: SpinLock<State>,
}

struct Queue {
    /// Its frames, and where the next command goes.
    base: u64,
    write: u64,
}

#[derive(Default)]
struct State {
    /// The devices with a table of events: (DeviceID, the table).
    devices: Vec<(u32, u64)>,
    /// The device each event (vector) of the boot processor belongs to.
    owners: Vec<(u8, u32)>,
}

static ITS: Once<Its> = Once::new();

impl Its {
    fn read64(&self, register: u64) -> u64 {
        // SAFETY: the ITS's registers, mapped as a device's.
        unsafe { core::ptr::read_volatile((self.base + register) as *const u64) }
    }

    fn write64(&self, register: u64, value: u64) {
        // SAFETY: the ITS's registers, mapped as a device's.
        unsafe { core::ptr::write_volatile((self.base + register) as *mut u64, value) };
    }

    /// Queues `commands`, and waits until the ITS has done them.
    fn command(&self, commands: &[[u64; 4]]) {
        let mut queue = self.queue.lock();
        for command in commands.iter().chain(&[[CMD_SYNC, 0, self.target << 16, 0]]) {
            let next = (queue.write + COMMAND_BYTES) % QUEUE_BYTES;
            // The queue never fills: it holds more than the commands of a
            // call, each of which waits for the last to finish.
            let slot = phys_to_virt(queue.base + queue.write);
            // SAFETY: a slot of the queue, whose frames are the ITS's and
            // this code's.
            unsafe { core::ptr::write_volatile(slot as *mut [u64; 4], *command) };
            flush_cache_range(slot, COMMAND_BYTES);
            queue.write = next;
        }
        self.write64(GITS_CWRITER, queue.write);
        loop {
            let read = self.read64(GITS_CREADR);
            if read == queue.write {
                break;
            }
            if read & CREADR_STALLED != 0 {
                crate::kwarn!("gic: the ITS stopped at a command");
                break;
            }
            core::hint::spin_loop();
        }
    }
}

/// The ITS's table of type `kind` among its `GITS_BASER`s: zeroed memory
/// for `entries` entries, in pages of the size it wants.
fn allocate_table(its: &Its, kind: u64, entries: u64) -> Result<(), &'static str> {
    for index in 0..8 {
        let register = GITS_BASER + index * 8;
        let baser = its.read64(register);
        if (baser >> 56) & 7 != kind {
            continue;
        }
        let entry = ((baser >> 48) & 0x1F) + 1;
        // The smallest of its pages that it takes: 4 KiB, 16 KiB or 64 KiB.
        let taken = |code: u64| {
            its.write64(register, baser & !PAGE_SIZE_FIELD | code << PAGE_SIZE_AT);
            (its.read64(register) & PAGE_SIZE_FIELD) >> PAGE_SIZE_AT == code
        };
        let Some(code) = (0..3).find(|&code| taken(code)) else {
            return Err("the ITS takes none of the sizes of pages there are");
        };
        let page = [1 << 12, 1 << 14, 1 << 16][code as usize];
        let pages = (entry * entries).div_ceil(page).min(256);
        let bytes = pages * page;
        let frame = phys::alloc_contiguous(bytes / 4096, 1 << 16, u64::MAX).ok_or("no memory for its tables")?;
        // SAFETY: the frames just allocated, direct-mapped.
        unsafe { core::ptr::write_bytes(phys_to_virt(frame) as *mut u8, 0, bytes as usize) };
        flush_cache_range(phys_to_virt(frame), bytes);
        let value =
            BASER_VALID | ATTRIBUTES | kind << 56 | (entry - 1) << 48 | frame | code << PAGE_SIZE_AT | (pages - 1);
        its.write64(register, value);
        return Ok(());
    }
    Ok(())
}

/// Sets the first ITS up, if the firmware lists one, with the boot
/// processor's redistributor. Without one devices have no messages.
pub(in crate::architecture) fn init() {
    let platform = super::acpi::PLATFORM.expect();
    let Some(&phys_base) = platform.its.first() else {
        crate::kinfo!("gic: no ITS, so no device has message-signalled interrupts");
        return;
    };
    match setup(phys_base) {
        Ok(its) => {
            ITS.set(its);
            crate::kinfo!("gic: ITS at {:#x}", phys_base);
        }
        Err(why) => crate::kwarn!("gic: the ITS at {:#x} is not used: {}", phys_base, why),
    }
}

fn setup(phys_base: u64) -> Result<Its, &'static str> {
    let no_memory = "no memory for its tables";
    let base = kvirt::map_mmio(phys_base, 0x2_0000, Cache::Uncached);
    // SAFETY: the ITS's registers, mapped as a device's.
    let typer = unsafe { core::ptr::read_volatile((base + GITS_TYPER) as *const u64) };
    let event_bytes = ((typer >> 4) & 0xF) + 1;
    let device_bits = ((typer >> 13) & 0x1F) + 1;
    // Where the ITS names redistributors by physical address.
    let physical = typer & (1 << 19) != 0;
    let properties_size = 1u64 << gic::MESSAGE_ID_BITS;
    let properties = phys::alloc_contiguous(properties_size.div_ceil(4096), 4096, u64::MAX).ok_or(no_memory)?;
    let pending = phys::alloc_contiguous(16, 1 << 16, u64::MAX).ok_or(no_memory)?;
    // SAFETY: the frames just allocated, direct-mapped.
    unsafe {
        core::ptr::write_bytes(phys_to_virt(properties) as *mut u8, 0, properties_size as usize);
        core::ptr::write_bytes(phys_to_virt(pending) as *mut u8, 0, 16 * 4096);
    }
    flush_cache_range(phys_to_virt(properties), properties_size);
    flush_cache_range(phys_to_virt(pending), 16 * 4096);
    gic::enable_messages(properties, pending);
    let queue = phys::alloc_contiguous(QUEUE_PAGES, 1 << 16, u64::MAX).ok_or(no_memory)?;
    // SAFETY: the frames just allocated, direct-mapped.
    unsafe { core::ptr::write_bytes(phys_to_virt(queue) as *mut u8, 0, QUEUE_BYTES as usize) };
    flush_cache_range(phys_to_virt(queue), QUEUE_BYTES);
    let its = Its {
        base,
        translater: phys_base + GITS_TRANSLATER,
        event_bytes,
        target: gic::redistributor_target(physical),
        properties,
        queue: SpinLock::new(Queue { base: queue, write: 0 }),
        state: SpinLock::new(State::default()),
    };
    allocate_table(&its, BASER_DEVICES, 1 << device_bits.min(16))?;
    allocate_table(&its, BASER_COLLECTIONS, 1)?;
    its.write64(GITS_CBASER, VALID | ATTRIBUTES | queue | (QUEUE_PAGES - 1));
    its.write64(GITS_CWRITER, 0);
    // SAFETY: the ITS's registers, mapped as a device's.
    unsafe { core::ptr::write_volatile((base + GITS_CTLR) as *mut u32, ENABLED) };
    its.command(&[[CMD_MAPC, 0, VALID | its.target << 16 | COLLECTION, 0]]);
    Ok(its)
}

/// The address and data that make a message of device `device` raise
/// `vector` on the boot processor; a device's first has the ITS learn it.
/// `None` without an ITS, or memory for the device's table of events.
pub(in crate::architecture) fn message(vector: u8, device: u16) -> Option<(u64, u32)> {
    let its = ITS.get()?;
    let device = device as u32;
    let mut state = its.state.lock();
    let mut commands = Vec::new();
    if !state.devices.iter().any(|&(d, _)| d == device) {
        let bytes = (its.event_bytes << EVENT_BITS).max(256);
        let table = phys::alloc_contiguous(bytes.div_ceil(4096), 4096, u64::MAX)?;
        // SAFETY: the frames just allocated, direct-mapped.
        unsafe { core::ptr::write_bytes(phys_to_virt(table) as *mut u8, 0, bytes as usize) };
        flush_cache_range(phys_to_virt(table), bytes);
        state.devices.push((device, table));
        commands.push([CMD_MAPD | (device as u64) << 32, EVENT_BITS - 1, VALID | table, 0]);
    }
    let intid = gic::message_intid(vector);
    let property = phys_to_virt(its.properties + vector as u64);
    // SAFETY: the LPI's byte of the table, given over to the GIC.
    unsafe { core::ptr::write_volatile(property as *mut u8, LPI_ENABLED) };
    flush_cache_range(property, 1);
    state.owners.push((vector, device));
    let event = (device as u64) << 32;
    commands.push([CMD_MAPTI | event, vector as u64 | (intid as u64) << 32, COLLECTION, 0]);
    commands.push([CMD_INV | event, vector as u64, 0, 0]);
    its.command(&commands);
    // A device in a guest's domain reaches it where that domain has it.
    let address = if super::iommu::in_guest(device as u16) { super::iommu::DOORBELL } else { its.translater };
    Some((address, vector as u32))
}

/// The page of `GITS_TRANSLATER`, for guests' domains.
pub(in crate::architecture) fn doorbell_page() -> Option<u64> {
    ITS.get().map(|its| its.translater & !0xFFF)
}

/// No device's message raises `vector` any more.
pub(in crate::architecture) fn release(vector: u8) {
    let Some(its) = ITS.get() else { return };
    let mut state = its.state.lock();
    let Some(at) = state.owners.iter().position(|&(v, _)| v == vector) else { return };
    let (_, device) = state.owners.swap_remove(at);
    its.command(&[[CMD_DISCARD | (device as u64) << 32, vector as u64, 0, 0]]);
}
