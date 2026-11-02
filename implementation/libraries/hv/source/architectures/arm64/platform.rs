// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The paravirtual platform (`crate::platform`) as ARM64 guests have it.
//!
//! A guest's processors have a GICv3, whose CPU interface is the processor's
//! own virtual interface and whose distributor and redistributors the
//! kernel answers for ([`super::gic`]), and the architecture's generic
//! timer, which is the host's counter, unchanged; a processor's number is
//! its affinity (`MPIDR_EL1`'s `Aff0` and `Aff1`, 16 to each).
//!
//! The guest learns of the platform from the device tree it boots with
//! ([`super::fdt`]), whose `/hypervisor` node is compatible with
//! [`COMPATIBLE`]. It has no firmware, and no ACPI tables.
//!
//! A hypercall is `hvc #0` with the call's number in `x0` and its arguments
//! in `x1` to `x5`, in that order; the result comes back in `x0`. A
//! processor starts through PSCI, as the device tree says, or with
//! `start_cpu(cpu, entry)`; it starts at `entry` in EL1 with its MMU off and
//! interrupts masked, as Linux's boot protocol has it. PSCI's calls go the
//! way hypercalls do, by their own numbers ([`psci`]), which the monitor
//! answers. The Linux guest's side of all this is
//! `arch/arm64/kernel/veda.c`, `drivers/pci/controller/pci-veda.c` and
//! `arch/arm64/include/asm/veda_para.h` in the Linux port
//! (`implementation/ports/linux`).

/// What the device tree's `/hypervisor` node is compatible with.
pub const COMPATIBLE: &str = "veda,platform";

/// The interrupt that says the bridge has news ([`crate::bridge`]), an SPI
/// of the guest's GIC, by its INTID: the device tree gives it to the
/// `/hypervisor` node.
pub const CALLBACK_INTID: u32 = 32;

/// The lines a function's INTx and the rest are, from here: GSI `n` is the
/// SPI of INTID `FIRST_LINE + n`.
pub const FIRST_LINE: u32 = 33;

/// PSCI, which the guest's Linux calls through `hvc`, as the device tree's
/// `/psci` node says.
pub mod psci {
    pub const VERSION: u64 = 0x8400_0000;
    pub const CPU_SUSPEND: u64 = 0xC400_0001;
    pub const CPU_OFF: u64 = 0x8400_0002;
    pub const CPU_ON: u64 = 0xC400_0003;
    pub const AFFINITY_INFO: u64 = 0xC400_0004;
    pub const MIGRATE_INFO_TYPE: u64 = 0x8400_0006;
    pub const SYSTEM_OFF: u64 = 0x8400_0008;
    pub const SYSTEM_RESET: u64 = 0x8400_0009;
    pub const FEATURES: u64 = 0x8400_000A;

    /// PSCI 1.0.
    pub const VERSION_1_0: u64 = 1 << 16;
    /// What `MIGRATE_INFO_TYPE` says: nothing has to migrate.
    pub const NO_MIGRATION: u64 = 2;

    pub const SUCCESS: u64 = 0;
    pub const NOT_SUPPORTED: u64 = -1i64 as u64;
    pub const INVALID_PARAMETERS: u64 = -2i64 as u64;
    pub const ALREADY_ON: u64 = -4i64 as u64;
}

/// Whether a processor may start in `pstate`: its flags, what masks its
/// exceptions, and EL0, or EL1 on either stack. It reaches the processor
/// as it stands: at EL2 it would run the monitor's choice of code as the
/// kernel, and any other mode is an illegal return, which stays at EL2.
pub fn is_guest_pstate(pstate: u64) -> bool {
    const FLAGS_AND_MASKS: u64 = 0xF000_03C0;
    const EL0T: u64 = 0;
    const EL1T: u64 = 4;
    const EL1H: u64 = 5;
    matches!(pstate & !FLAGS_AND_MASKS, EL0T | EL1T | EL1H)
}

/// Whether `call` (a hypercall's `x0`) is one of PSCI's rather than one of
/// the platform's, whose numbers are small.
pub fn is_psci(call: u64) -> bool {
    call >> 24 == 0x84 || call >> 24 == 0xC4
}

/// The number of the processor of `affinity` (`Aff0` and `Aff1`).
pub fn cpu_of(affinity: u64) -> u64 {
    (affinity & 0xF) | ((affinity >> 8) & 0xFF) << 4
}

/// The affinity of processor `cpu`, as `MPIDR_EL1` and the device tree have
/// it.
pub fn affinity_of(cpu: u64) -> u64 {
    (cpu & 0xF) | ((cpu >> 4) & 0xFF) << 8
}

/// The message of a `pci_msi` hypercall, as its result holds it: the data
/// in the top 16 bits, the address (48 bits, as physical addresses are
/// here) below them. An ITS's data is an event, a vector, which fits.
pub fn msi_result(address: u64, data: u32) -> u64 {
    (address & 0xFFFF_FFFF_FFFF) | ((data as u64) << 48)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_processor_starts_in_el0_or_el1() {
        for pstate in [0, 4, 5, 0x3C5, 0xF000_03C5] {
            assert!(is_guest_pstate(pstate), "{pstate:#x}");
        }
        // EL2 and EL3, on either stack; AArch32; reserved modes; a bit that
        // has no business being set, such as `IL`.
        for pstate in [8, 9, 0x3C9, 0x3C8, 12, 13, 1, 2, 3, 6, 7, 0x10, 0x13, 1 << 20, 1 << 32] {
            assert!(!is_guest_pstate(pstate), "{pstate:#x}");
        }
    }

    #[test]
    fn msi_results_hold_address_and_data() {
        assert_eq!(msi_result(0x0808_0040, 0), 0x0808_0040);
        let r = msi_result(0x80_0808_0040, 0x41);
        assert_eq!((r & 0xFFFF_FFFF_FFFF, r >> 48), (0x80_0808_0040, 0x41));
    }

    #[test]
    fn psci_calls_are_told_from_the_platforms() {
        assert!(is_psci(psci::CPU_ON) && is_psci(psci::VERSION) && is_psci(psci::SYSTEM_OFF));
        assert!(!is_psci(crate::platform::hypercall::CONSOLE_WRITE));
        assert!(!is_psci(crate::platform::hypercall::FIRMWARE_VARIABLE));
    }

    #[test]
    fn processors_are_numbered_by_their_affinity() {
        for cpu in [0, 1, 15, 16, 17, 63] {
            assert_eq!(cpu_of(affinity_of(cpu)), cpu);
        }
        assert_eq!(affinity_of(17), 0x101);
    }
}
