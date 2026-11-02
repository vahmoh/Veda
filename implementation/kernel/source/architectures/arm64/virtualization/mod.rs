// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Virtual machines on ARM64: the Virtualization Host Extensions, with a
//! GICv3's virtual interface.
//!
//! The kernel runs at EL2 where firmware lets it, and there guests are a
//! matter of entering EL1. A guest's physical address space is translated
//! by stage 2 tables (`stage2`); its processors run at EL1 under `HCR_EL2`
//! (`vcpu`), and take interrupts from the list registers.

mod stage2;
mod vcpu;

use core::arch::asm;
use core::sync::atomic::{AtomicU32, Ordering};

use hv::arm64::gic::INTIDS;

use super::cpu::{self, read_register, write_register};

/// The tables that translate a guest's physical addresses.
pub use stage2::Stage2 as GuestTables;
pub use vcpu::Vcpu;

/// The PPIs of the virtual timer, which is the guest's, and of the virtual
/// interface's maintenance interrupt. As Arm's Base System Architecture
/// has them, for lack of tables that say otherwise (the MADT's for the
/// latter is per CPU).
pub(in crate::architecture) const VIRTUAL_TIMER_PPI: u32 = 27;
pub(in crate::architecture) const MAINTENANCE_PPI: u32 = 25;

/// `CNTHCTL_EL2`, as `E2H` has it: guests' EL1 reads the physical counter,
/// and has no use for the physical timer, whose interrupt is nobody's.
const CNTHCTL_GUESTS: u64 = 1 << 10;
/// `CPACR_EL1`, which is `CPTR_EL2` as `E2H` has it: guests reach no trace
/// registers (`TTA`).
const CPTR_GUESTS: u64 = 1 << 28;
/// `MDCR_EL2`: guests reach no performance counters (`TPMCR`, `TPM`), debug
/// registers (`TDA`, `TDOSA`, `TDRA`), statistical profiling (`TPMS`) or
/// trace filters (`TTRF`), which all stay as the host left them whichever
/// guest runs. What is left of its reset value says how many counters EL2
/// has (`HPMN`). Exceptions stay where `HCR_EL2` sends them.
const MDCR_GUESTS: u64 = 1 << 5 | 1 << 6 | 1 << 9 | 1 << 10 | 1 << 11 | 1 << 14 | 1 << 19;
const MDCR_HPMN: u64 = 0x1F;
/// `VTCR_EL2`: 48-bit guest-physical addresses, four levels from level 0,
/// write-back tables shared with the other processors, 4 KiB granules. The
/// output's size is the processor's, in `PS`. Bit 31 is reserved, as 1.
const VTCR: u64 = 16 | 2 << 6 | 1 << 8 | 1 << 10 | 3 << 12 | 1 << 31;
const VTCR_PS: u32 = 16;

/// The list registers there are.
static LIST_REGISTERS: AtomicU32 = AtomicU32::new(0);

/// The active priority registers of each group there are: one for 32
/// priorities of preemption, as most processors have, two for 64, four for
/// 128.
static ACTIVE_PRIORITY_REGISTERS: AtomicU32 = AtomicU32::new(0);

/// How wide the processor's physical addresses are, as `PARange` says (at
/// most what 4 KiB granules reach without LPA2).
fn physical_range() -> u64 {
    (read_register!("id_aa64mmfr0_el1") & 0xF).min(5)
}

/// Sets up what this CPU needs to run guests.
pub(in crate::architecture) fn init_this_cpu() {
    if !cpu::at_el2() {
        return;
    }
    let vtr = read_register!("s3_4_c12_c11_1"); // ICH_VTR_EL2
    LIST_REGISTERS.store(((vtr & 0x1F) + 1) as u32, Ordering::Relaxed);
    let preemption_bits = ((vtr >> 26) & 7) as u32 + 1;
    ACTIVE_PRIORITY_REGISTERS.store(1 << preemption_bits.saturating_sub(5), Ordering::Relaxed);
    // SAFETY: registers that only say how guests run, and none runs yet.
    unsafe {
        // Guests count as the kernel does, from the physical counter: their
        // timers' deadlines are compared with its clock. Firmware may leave
        // an offset, which the architecture leaves unknown at reset.
        write_register!("cntvoff_el2", 0u64);
        write_register!("cntkctl_el1", read_register!("cntkctl_el1") | CNTHCTL_GUESTS);
        write_register!("cpacr_el1", read_register!("cpacr_el1") | CPTR_GUESTS);
        write_register!("mdcr_el2", read_register!("mdcr_el2") & MDCR_HPMN | MDCR_GUESTS);
        write_register!("vtcr_el2", VTCR | physical_range() << VTCR_PS);
        // Traps that firmware may leave as it finds them, on the processors
        // that have them.
        if (read_register!("id_aa64mmfr0_el1") >> 56) & 0xF != 0 {
            write_register!("s3_4_c1_c1_4", 0u64); // HFGRTR_EL2
            write_register!("s3_4_c1_c1_5", 0u64); // HFGWTR_EL2
            write_register!("s3_4_c1_c1_6", 0u64); // HFGITR_EL2
            write_register!("s3_4_c3_c1_4", 0u64); // HDFGRTR_EL2
            write_register!("s3_4_c3_c1_5", 0u64); // HDFGWTR_EL2
        }
        if (read_register!("id_aa64mmfr1_el1") >> 40) & 0xF != 0 {
            write_register!("s3_4_c1_c2_2", 0u64); // HCRX_EL2
        }
        asm!("isb", options(nostack, preserves_flags));
    }
}

/// The list registers of this machine.
fn list_registers() -> usize {
    LIST_REGISTERS.load(Ordering::Relaxed) as usize
}

/// The active priority registers of this machine.
fn active_priority_registers() -> usize {
    ACTIVE_PRIORITY_REGISTERS.load(Ordering::Relaxed) as usize
}

/// Whether this machine can run guests, and why not.
pub fn available() -> Result<(), &'static str> {
    if !cpu::at_el2() {
        return Err("the firmware left EL2 to itself, or the processor has no VHE");
    }
    if physical_range() < 4 {
        return Err("the processor's physical addresses are under 44 bits");
    }
    Ok(())
}

/// The vector of an interrupt a virtual processor takes, if `vector` is
/// one: an SPI of its GIC.
pub fn interrupt_vector(vector: usize) -> Option<u8> {
    (hv::arm64::gic::PRIVATE..INTIDS).contains(&vector).then_some(vector as u8)
}

/// Logs whether this machine can run guests, once at boot.
pub fn report() {
    match available() {
        Ok(()) => crate::kinfo!("virtualization: EL2 with VHE; {} list registers", list_registers()),
        Err(why) => crate::kinfo!("virtualization: no virtual machines ({})", why),
    }
}

/// The virtual timer of the guest this CPU runs fired (`gic`).
pub(in crate::architecture) fn timer_fired() {
    vcpu::timer_fired();
}
