// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The GICv3 interrupt controller: its distributor, the redistributor of
//! each CPU, the CPU interface in system registers, and what comes in
//! through them.
//!
//! An interrupt arrives as an INTID: software-generated (SGIs, 0 to 15),
//! private to a CPU (PPIs, 16 to 31), or a device's, a line (SPIs, 32 up)
//! or a message (LPIs, 8192 up, which come through the ITS, `super::its`).
//! The kernel's own IPIs are SGIs, and its timer a PPI. Devices' interrupts
//! go to the boot processor, each with a vector of its own from one pool
//! (`super::irq`): a line's by a table here, a message's as its INTID's
//! distance from the first LPI's.

use core::arch::asm;
use core::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, Ordering};

use super::cpu::{self, read_register, write_register};
use super::trap::TrapFrame;
use super::virtualization::{MAINTENANCE_PPI, VIRTUAL_TIMER_PPI};
use super::{its, percpu, timer};
use crate::memory::kvirt;
use crate::memory::paging::Cache;
use crate::scheduler::percpu::MAX_CPUS;
use crate::synchronization::bkl;

/// The lines of the IOMMU's faults (0: none).
static FAULT_LINES: [AtomicU32; 2] = [AtomicU32::new(0), AtomicU32::new(0)];

/// Turns the IOMMU's lines on, to the boot processor: true if it has
/// any.
pub(in crate::architecture) fn route_fault_lines(event: u32, error: u32) -> bool {
    let lines = [event, error];
    for (slot, line) in FAULT_LINES.iter().zip(lines) {
        if line != 0 && route_line(line, FAULT_VECTOR, false) {
            slot.store(line, Ordering::Relaxed);
        }
    }
    FAULT_LINES.iter().any(|l| l.load(Ordering::Relaxed) != 0)
}

/// The kernel's IPIs. They never take the BKL but the first: the others
/// must work while another CPU holds it.
pub(in crate::architecture) const SGI_RESCHEDULE: u32 = 0;
pub(in crate::architecture) const SGI_BARRIER: u32 = 1;
pub(in crate::architecture) const SGI_HALT: u32 = 2;
/// Makes a CPU that runs a guest leave it (to see an interrupt for the
/// guest, or a request to its scheduler); it does nothing else.
pub(in crate::architecture) const SGI_KICK: u32 = 3;

/// What [`LINE_VECTORS`] says of the IOMMU's own lines: no device's
/// interrupt has it (`super::irq::DEVICE_VECTORS`).
const FAULT_VECTOR: u8 = 0xFF;

/// The first of the private interrupts, of the lines, and of the messages.
const FIRST_PPI: u32 = 16;
const FIRST_SPI: u32 = 32;
const FIRST_LPI: u32 = 8192;
/// INTIDs at and above this are no interrupt, or a spurious one, up to the
/// LPIs'.
const SPECIAL: u32 = 1020;

/// The distributor's registers.
const GICD_CTLR: u64 = 0x0000;
const GICD_TYPER: u64 = 0x0004;
const GICD_IGROUPR: u64 = 0x0080;
const GICD_ISENABLER: u64 = 0x0100;
const GICD_ICENABLER: u64 = 0x0180;
const GICD_IPRIORITYR: u64 = 0x0400;
const GICD_ICFGR: u64 = 0x0C00;
const GICD_IROUTER: u64 = 0x6000;
/// `GICD_CTLR`: affinity routing (`ARE_NS`), and group 1 interrupts on.
const GICD_CTLR_ARE: u32 = 1 << 4;
const GICD_CTLR_GROUP1: u32 = 1 << 1;
/// `GICD_CTLR`: a write has not taken effect yet.
const GICD_CTLR_RWP: u32 = 1 << 31;

/// A redistributor's registers, then those of its SGIs and PPIs, a frame
/// on.
const GICR_CTLR: u64 = 0x0000;
const GICR_TYPER: u64 = 0x0008;
const GICR_WAKER: u64 = 0x0014;
const GICR_PROPBASER: u64 = 0x0070;
const GICR_PENDBASER: u64 = 0x0078;
const GICR_SGI: u64 = 0x1_0000;
const GICR_IGROUPR0: u64 = GICR_SGI + 0x0080;
const GICR_ISENABLER0: u64 = GICR_SGI + 0x0100;
const GICR_ICENABLER0: u64 = GICR_SGI + 0x0180;
const GICR_IPRIORITYR: u64 = GICR_SGI + 0x0400;
/// `GICR_WAKER`: the redistributor is asleep; and it has gone to sleep.
const GICR_WAKER_SLEEP: u32 = 1 << 1;
const GICR_WAKER_ASLEEP: u32 = 1 << 2;
/// `GICR_TYPER`: this is the last redistributor of the range; it has
/// virtual LPIs, and so two frames more.
const GICR_TYPER_LAST: u64 = 1 << 4;
const GICR_TYPER_VLPIS: u64 = 1 << 1;
/// The redistributor's frames.
const GICR_FRAMES: u64 = 0x2_0000;
const GICR_FRAMES_VLPIS: u64 = 0x4_0000;

/// Every interrupt runs at the same priority, so none preempts another
/// (and they run with interrupts masked anyway).
const PRIORITY: u8 = 0xA0;

/// The distributor's registers, mapped.
static DISTRIBUTOR: AtomicU64 = AtomicU64::new(0);
/// How many lines the distributor has (INTIDs below this).
static LINES: AtomicU32 = AtomicU32::new(0);
/// The timer's interrupt, the kernel's.
static TIMER: AtomicU32 = AtomicU32::new(0);
/// Each CPU's redistributor's registers, mapped.
static REDISTRIBUTORS: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
/// Where the boot processor's redistributor really is.
static BOOT_REDISTRIBUTOR: AtomicU64 = AtomicU64::new(0);
/// The vector of each line (0: none).
static LINE_VECTORS: [AtomicU8; SPECIAL as usize] = [const { AtomicU8::new(0) }; SPECIAL as usize];

fn distributor(offset: u64) -> *mut u32 {
    (DISTRIBUTOR.load(Ordering::Relaxed) + offset) as *mut u32
}

fn read_distributor(offset: u64) -> u32 {
    // SAFETY: the distributor's registers, mapped as a device's.
    unsafe { core::ptr::read_volatile(distributor(offset)) }
}

fn write_distributor(offset: u64, value: u32) {
    // SAFETY: the distributor's registers, mapped as a device's.
    unsafe { core::ptr::write_volatile(distributor(offset), value) };
}

/// Waits until what was written to the distributor has taken effect.
fn wait_for_distributor() {
    while read_distributor(GICD_CTLR) & GICD_CTLR_RWP != 0 {
        core::hint::spin_loop();
    }
}

/// The redistributor of the current CPU.
fn redistributor() -> u64 {
    REDISTRIBUTORS[percpu::current_id()].load(Ordering::Relaxed)
}

fn read32(at: u64) -> u32 {
    // SAFETY: a register of a redistributor, mapped as a device's.
    unsafe { core::ptr::read_volatile(at as *const u32) }
}

fn write32(at: u64, value: u32) {
    // SAFETY: a register of a redistributor, mapped as a device's.
    unsafe { core::ptr::write_volatile(at as *mut u32, value) };
}

fn write64(at: u64, value: u64) {
    // SAFETY: a register of a redistributor, mapped as a device's.
    unsafe { core::ptr::write_volatile(at as *mut u64, value) };
}

fn read64(at: u64) -> u64 {
    // SAFETY: a register of a redistributor, mapped as a device's.
    unsafe { core::ptr::read_volatile(at as *const u64) }
}

/// The redistributor for each CPU `id` of the machine's affinities, by the
/// affinity it reports: the firmware's list of each CPU's own, or else the
/// ranges it gives.
fn map_redistributors() {
    let platform = super::acpi::PLATFORM.expect();
    let mut found = 0;
    let mut note = |frame: u64, phys: u64| {
        let affinity = (read64(frame + GICR_TYPER) >> 32) & 0xFFFF_FFFF;
        let want = |c: u64| (c & 0xFF_FFFF) | ((c >> 8) & 0xFF00_0000);
        if let Some(i) = platform.cpus.iter().position(|c| want(c.affinity) == affinity)
            && i < MAX_CPUS
        {
            REDISTRIBUTORS[i].store(frame, Ordering::Relaxed);
            if i == 0 {
                BOOT_REDISTRIBUTOR.store(phys, Ordering::Relaxed);
            }
            found += 1;
        }
    };
    if platform.redistributors.is_empty() {
        for &phys in &platform.per_cpu_redistributors {
            note(kvirt::map_mmio(phys, GICR_FRAMES_VLPIS, Cache::Uncached), phys);
        }
    }
    for &(phys, length) in &platform.redistributors {
        let base = kvirt::map_mmio(phys, length as u64, Cache::Uncached);
        let mut at = base;
        while at < base + length as u64 {
            let typer = read64(at + GICR_TYPER);
            note(at, phys + (at - base));
            if typer & GICR_TYPER_LAST != 0 {
                break;
            }
            at += if typer & GICR_TYPER_VLPIS != 0 { GICR_FRAMES_VLPIS } else { GICR_FRAMES };
        }
    }
    crate::kinfo!("gic: {} redistributor(s) of {} CPU(s)", found, platform.cpus.len());
}

/// Sets the interrupt controller up: the distributor, every line off and
/// each going to the boot processor when it is turned on, and the
/// redistributors. Then the boot processor's part ([`init_cpu`]), and the
/// ITS.
pub(in crate::architecture) fn init() {
    let platform = super::acpi::PLATFORM.expect();
    let base = kvirt::map_mmio(platform.distributor, 0x1_0000, Cache::Uncached);
    DISTRIBUTOR.store(base, Ordering::Relaxed);
    let lines = ((read_distributor(GICD_TYPER) & 0x1F) + 1) * 32;
    let lines = lines.min(SPECIAL);
    LINES.store(lines, Ordering::Relaxed);
    write_distributor(GICD_CTLR, 0);
    wait_for_distributor();
    for word in 1..lines / 32 {
        write_distributor(GICD_ICENABLER + word as u64 * 4, !0);
        write_distributor(GICD_IGROUPR + word as u64 * 4, !0);
    }
    wait_for_distributor();
    write_distributor(GICD_CTLR, GICD_CTLR_ARE | GICD_CTLR_GROUP1);
    wait_for_distributor();
    TIMER.store(if cpu::at_el2() { platform.timer_el2 } else { platform.timer_el1 }, Ordering::Relaxed);
    map_redistributors();
    init_cpu();
    its::init();
}

/// Sets up the current CPU's part: wakes its redistributor, turns its IPIs
/// and timer on, and turns its interface on.
pub(in crate::architecture) fn init_cpu() {
    let gicr = redistributor();
    write32(gicr + GICR_WAKER, read32(gicr + GICR_WAKER) & !GICR_WAKER_SLEEP);
    while read32(gicr + GICR_WAKER) & GICR_WAKER_ASLEEP != 0 {
        core::hint::spin_loop();
    }
    let timer = TIMER.load(Ordering::Relaxed);
    write32(gicr + GICR_ICENABLER0, !0);
    write32(gicr + GICR_IGROUPR0, !0);
    for intid in (0..FIRST_SPI as u64).step_by(4) {
        write32(gicr + GICR_IPRIORITYR + intid, u32::from_ne_bytes([PRIORITY; 4]));
    }
    // At EL2, the guests' timer and virtual interface too.
    let guests = if cpu::at_el2() { 1 << VIRTUAL_TIMER_PPI | 1 << MAINTENANCE_PPI } else { 0 };
    write32(gicr + GICR_ISENABLER0, 0xFFFF | 1 << timer | guests);
    // SAFETY: this CPU's interface, whose interrupts the kernel handles. At
    // EL2 the loader has opened its system registers (`ICC_SRE_EL2`).
    unsafe {
        if !cpu::at_el2() {
            write_register!("s3_0_c12_c12_5", read_register!("s3_0_c12_c12_5") | 1); // ICC_SRE_EL1
            asm!("isb", options(nostack, preserves_flags));
        }
        write_register!("s3_0_c4_c6_0", 0xFFu64); // ICC_PMR_EL1: all priorities
        write_register!("s3_0_c12_c12_3", 0u64); // ICC_BPR1_EL1
        write_register!("s3_0_c12_c12_4", 0u64); // ICC_CTLR_EL1: EOI also deactivates
        write_register!("s3_0_c12_c12_7", 1u64); // ICC_IGRPEN1_EL1
        asm!("isb", options(nostack, preserves_flags));
    }
}

/// Sends IPI `sgi` to the CPU of `affinity`. What was stored before is seen
/// by its target's handler.
pub(in crate::architecture) fn send_sgi(affinity: u64, sgi: u32) {
    let (aff0, aff1, aff2, aff3) =
        (affinity & 0xFF, (affinity >> 8) & 0xFF, (affinity >> 16) & 0xFF, (affinity >> 32) & 0xFF);
    let value = aff3 << 48 | aff2 << 32 | (sgi as u64) << 24 | aff1 << 16 | (aff0 >> 4) << 44 | 1 << (aff0 & 0xF);
    // SAFETY: the barrier only orders; then an SGI of the kernel's.
    unsafe {
        asm!("dsb ishst", options(nostack, preserves_flags));
        write_register!("s3_0_c12_c11_5", value); // ICC_SGI1R_EL1
        asm!("isb", options(nostack, preserves_flags));
    }
}

/// Which `Enable` bit of 32 is `line`'s, and its word.
fn enable_bit(line: u32) -> (u64, u32) {
    ((line / 32) as u64 * 4, 1 << (line % 32))
}

/// Routes line `gsi` to `vector` of the boot processor, on. False if the
/// distributor has no such line.
pub(in crate::architecture) fn route_line(gsi: u32, vector: u8, level: bool) -> bool {
    if !(FIRST_SPI..LINES.load(Ordering::Relaxed)).contains(&gsi) {
        return false;
    }
    LINE_VECTORS[gsi as usize].store(vector, Ordering::Release);
    let (word, bit) = enable_bit(gsi);
    write_distributor(GICD_ICENABLER + word, bit);
    wait_for_distributor();
    let config = GICD_ICFGR + (gsi / 16) as u64 * 4;
    let shift = (gsi % 16) * 2 + 1;
    write_distributor(config, read_distributor(config) & !(1 << shift) | ((!level as u32) << shift));
    let priority = GICD_IPRIORITYR + (gsi & !3) as u64;
    let shift = (gsi % 4) * 8;
    write_distributor(priority, read_distributor(priority) & !(0xFF << shift) | (PRIORITY as u32) << shift);
    let route = DISTRIBUTOR.load(Ordering::Relaxed) + GICD_IROUTER + gsi as u64 * 8;
    write64(route, percpu::get(0).affinity);
    write_distributor(GICD_ISENABLER + word, bit);
    true
}

/// Masks or unmasks line `gsi`.
pub(in crate::architecture) fn set_line_masked(gsi: u32, masked: bool) {
    if !(FIRST_SPI..LINES.load(Ordering::Relaxed)).contains(&gsi) {
        return;
    }
    let (word, bit) = enable_bit(gsi);
    write_distributor(if masked { GICD_ICENABLER } else { GICD_ISENABLER } + word, bit);
    if masked {
        wait_for_distributor();
    }
}

/// No line raises `vector` any more.
pub(in crate::architecture) fn release_lines(vector: u8) {
    for slot in &LINE_VECTORS {
        let _ = slot.compare_exchange(vector, 0, Ordering::AcqRel, Ordering::Relaxed);
    }
}

/// The INTID of the message that raises `vector`.
pub(in crate::architecture) fn message_intid(vector: u8) -> u32 {
    FIRST_LPI + vector as u32
}

/// The most message INTIDs need: 14 bits.
pub(in crate::architecture) const MESSAGE_ID_BITS: u32 = 14;

/// Turns the boot processor's messages on: `properties` (a byte for each
/// LPI) and `pending` (a bit for each INTID) are the tables of its
/// redistributor, in memory given over to it.
pub(in crate::architecture) fn enable_messages(properties: u64, pending: u64) {
    /// The tables are write-back, shared with the processors
    /// (`GICR_PROPBASER`, `GICR_PENDBASER`).
    const ATTRIBUTES: u64 = 7 << 7 | 1 << 10;
    /// `GICR_PENDBASER`: the table is zeroes.
    const PENDING_ZEROED: u64 = 1 << 62;
    /// `GICR_CTLR`: LPIs are on.
    const ENABLE_LPIS: u32 = 1;
    let gicr = redistributor();
    write64(gicr + GICR_PROPBASER, properties | ATTRIBUTES | (MESSAGE_ID_BITS - 1) as u64);
    write64(gicr + GICR_PENDBASER, pending | ATTRIBUTES | PENDING_ZEROED);
    // SAFETY: a barrier only orders.
    unsafe { asm!("dsb sy", options(nostack, preserves_flags)) };
    write32(gicr + GICR_CTLR, read32(gicr + GICR_CTLR) | ENABLE_LPIS);
}

/// What the ITS names the boot processor's redistributor by, with
/// `physical` set: its physical address, in 64 KiB units; otherwise its
/// processor number.
pub(in crate::architecture) fn redistributor_target(physical: bool) -> u64 {
    if physical {
        BOOT_REDISTRIBUTOR.load(Ordering::Relaxed) >> 16
    } else {
        (read64(redistributor() + GICR_TYPER) >> 8) & 0xFFFF
    }
}

#[inline]
fn acknowledge() -> u32 {
    read_register!("s3_0_c12_c12_0") as u32 // ICC_IAR1_EL1
}

#[inline]
fn end_of_interrupt(intid: u32) {
    // SAFETY: the interrupt [`acknowledge`] gave.
    unsafe { write_register!("s3_0_c12_c12_1", intid) }; // ICC_EOIR1_EL1
}

/// Takes the interrupt that brought the processor here.
pub(in crate::architecture) fn interrupt(frame: &mut TrapFrame) {
    let intid = acknowledge();
    match intid {
        // These never take the BKL: they must work while another CPU holds it.
        SGI_BARRIER => {
            super::tlb::handle_ipi();
            end_of_interrupt(intid);
            return;
        }
        SGI_HALT => cpu::halt_forever(),
        SGI_KICK | MAINTENANCE_PPI => {
            end_of_interrupt(intid);
            return;
        }
        // The kernel's own timer, at EL1.
        VIRTUAL_TIMER_PPI if cpu::at_el2() => {
            super::virtualization::timer_fired();
            end_of_interrupt(intid);
            return;
        }
        SPECIAL..FIRST_LPI => return,
        _ => {}
    }

    bkl::acquire();
    if frame.is_user() {
        crate::scheduler::entered_from_user();
    }
    match intid {
        SGI_RESCHEDULE => {
            end_of_interrupt(intid);
            crate::scheduler::request_resched();
        }
        i if i == TIMER.load(Ordering::Relaxed) => {
            timer::fired();
            end_of_interrupt(intid);
            crate::scheduler::timer_interrupt();
        }
        i if i >= FIRST_LPI => {
            crate::interrupt::dispatch((i - FIRST_LPI) as u8);
            end_of_interrupt(intid);
        }
        i if (FIRST_SPI..SPECIAL).contains(&i) => {
            match LINE_VECTORS[i as usize].load(Ordering::Acquire) {
                0 => crate::kwarn!("unexpected interrupt line {}", i),
                FAULT_VECTOR => super::iommu::fault_interrupt(),
                vector => crate::interrupt::dispatch(vector),
            }
            end_of_interrupt(intid);
        }
        _ => {
            debug_assert!(intid >= FIRST_PPI || intid < FIRST_SPI);
            crate::kwarn!("unexpected interrupt {}", intid);
            end_of_interrupt(intid);
        }
    }
    if frame.is_user() {
        crate::scheduler::return_to_user_hook(frame);
    }
    bkl::release();
}
