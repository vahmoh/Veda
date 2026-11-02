// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The IOMMU (Arm's SMMUv3): what devices reach of memory.
//!
//! At boot, before any device is given to a guest, the SMMU the firmware's
//! IORT table names for the PCI root is set up with a stream table that lets
//! every stream through untranslated, so Veda's own drivers reach memory as
//! they would without one. A device given to a guest ([`Domain::attach`])
//! has its stream translated by the guest's domain, whose stage 2 tables
//! mirror the guest's memory; when the domain goes, the stream reaches
//! nothing at all.
//!
//! Which interrupts a device may raise is the ITS's business
//! (`super::its`), which the device's stream id, not anything it says,
//! selects. A device in a domain still has to reach the ITS's doorbell,
//! at an address of the domain's that no guest has (`DOORBELL`).
//!
//! The kernel uses the SMMU only if it does stage 2 with 4 KiB granules.
//! Otherwise it leaves it alone: devices work as they would without one,
//! and none can be given to a guest.

mod domain;
mod unit;

use alloc::vec::Vec;

use iommu::arm64::iort::{self, Iort};
use iommu::arm64::smmu::{self, Event};

pub use domain::Domain;
use unit::Unit;

use crate::firmware::acpi;
use crate::memory::phys_to_virt;
use crate::synchronization::{Once, SpinLock};

/// Where a guest's domain has the ITS's doorbell, for the MSIs of its
/// devices: an address of the guest-physical space that nothing has.
pub(in crate::architecture) const DOORBELL: u64 = hv::arm64::memory::MSI_DOORBELL;

/// Faults past this many go unlogged, but for every 1024th.
const FAULTS_LOGGED: u32 = 32;

struct Iommu {
    unit: Unit,
    /// Where its registers are, as the log names it (as it does a PC's
    /// units).
    address: u64,
    iort: Iort,
    /// Its walks see the processors' caches.
    coherent: bool,
    vmids: SpinLock<Vmids>,
    /// Streams that left a guest's domain (they reach nothing until one
    /// takes them again), and whether they were refused since.
    left: SpinLock<Vec<(u32, bool)>>,
    /// Faults reported so far (only the first ones are logged).
    faults: SpinLock<u32>,
}

/// VMIDs of guests' domains: from 1 up, those of ended domains first.
struct Vmids {
    next: u32,
    limit: u32,
    free: Vec<u16>,
}

impl Vmids {
    fn take(&mut self) -> Option<u16> {
        self.free.pop().or_else(|| {
            (self.next < self.limit).then(|| {
                self.next += 1;
                (self.next - 1) as u16
            })
        })
    }
}

static IOMMU: Once<Iommu> = Once::new();

fn iommu() -> Option<&'static Iommu> {
    IOMMU.get()
}

/// Whether the IOMMU is in use: functions can be given to guests.
pub fn active() -> bool {
    IOMMU.get().is_some()
}

/// Makes what the processor wrote to `[phys, phys + len)` reach the SMMU
/// before what follows, written back if it reads memory around the caches.
fn publish(iommu: &Iommu, phys: u64, len: u64) {
    if iommu.coherent {
        abi::barrier::device_write();
    } else {
        super::memory::flush_cache_range(phys_to_virt(phys), len);
    }
}

/// Sets up the IOMMU, if the firmware describes one (after the interrupt
/// controller).
pub fn init() {
    let Some(table) = acpi::find_all(b"IORT").filter(|t| t.checksum_ok()).last() else { return };
    let parsed = match iort::parse(table.bytes) {
        Ok(iort) => iort,
        Err(e) => {
            crate::kwarn!("iommu: the IORT table: {}", e);
            return;
        }
    };
    let Some(&described) = parsed.smmu() else { return };
    match Unit::new(described.base, described.coherent) {
        Ok(unit) => {
            let coherent = unit.coherent();
            let limit = if unit.capabilities.vmid16 { 1 << 16 } else { 1 << 8 };
            let iommu = Iommu {
                unit,
                address: described.base,
                iort: parsed,
                coherent,
                vmids: SpinLock::new(Vmids { next: 1, limit, free: Vec::new() }),
                left: SpinLock::new(Vec::new()),
                faults: SpinLock::new(0),
            };
            IOMMU.set(iommu);
            if super::gic::route_fault_lines(described.event_interrupt, described.error_interrupt)
                && let Some(iommu) = IOMMU.get()
            {
                iommu.unit.enable_interrupts();
            }
            crate::kinfo!(
                "iommu: SMMUv3 at {:#x}: devices passed through until given to a guest{}",
                described.base,
                if coherent { "" } else { ", flushed for it" },
            );
        }
        Err(why) => crate::kinfo!("iommu: not used: {}", why),
    }
}

/// The SMMU's interrupts: logs what it refused.
pub(in crate::architecture) fn fault_interrupt() {
    let Some(iommu) = iommu() else { return };
    iommu.unit.take_events(|e| log_fault(iommu, e));
}

/// Stream `stream` left a guest's domain.
fn left_guest(iommu: &Iommu, stream: u32) {
    let mut left = iommu.left.lock();
    if !left.iter().any(|&(s, _)| s == stream) {
        left.push((stream, false));
    }
}

/// Stream `stream` is in a guest's domain again.
fn joined_guest(iommu: &Iommu, stream: u32) {
    iommu.left.lock().retain(|&(s, _)| s != stream);
}

fn log_fault(iommu: &Iommu, e: Event) {
    // What a device does after its guest ended reaches nothing, as it
    // should: said once.
    if let Some(entry) = iommu.left.lock().iter_mut().find(|(s, _)| *s == e.stream) {
        if !entry.1 {
            entry.1 = true;
            crate::kinfo!("iommu: stream {:#x} left its guest and goes on with its DMA; it reaches nothing", e.stream);
        }
        return;
    }
    let n = {
        let mut count = iommu.faults.lock();
        *count = count.wrapping_add(1);
        *count
    };
    if n > FAULTS_LOGGED && !n.is_multiple_of(1024) {
        return;
    }
    crate::kwarn!(
        "iommu: unit at {:#x} refused a {} at {:#x} from stream {:#x}: {}{}",
        iommu.address,
        if e.write { "write" } else { "read" },
        e.address,
        e.stream,
        smmu::event_name(e.kind),
        if n == FAULTS_LOGGED { "; the next faults go unlogged" } else { "" }
    );
}

/// Whether requester `device` is in a guest's domain, whose address of the
/// ITS's doorbell is its messages'.
pub(in crate::architecture) fn in_guest(device: u16) -> bool {
    iommu().is_some_and(|i| i.iort.stream_of(device).is_some_and(|stream| i.unit.is_translated(stream)))
}
