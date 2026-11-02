// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What guests see of the processor.
//!
//! An operating system believes the processor's ID registers, and turns on
//! what they show. Much of that needs a hypervisor's part too, such as
//! registers to swap between virtual processors, or traps to turn off, and
//! each new processor shows more. So guests' reads of these registers trap,
//! and they see what is named here and nothing else: a field that is not
//! kept reads 0, as on a processor without it, whatever comes to be there.

/// The field of an ID register at bit `at`, 4 bits wide as they all are.
const fn field(at: u32) -> u64 {
    0xF << at
}

/// Several fields of an ID register.
const fn fields(at: &[u32]) -> u64 {
    let mut mask = 0;
    let mut i = 0;
    while i < at.len() {
        mask |= field(at[i]);
        i += 1;
    }
    mask
}

/// `ID_AA64PFR0_EL1`: floating point and SIMD, `DIT`, `CSV2` and `CSV3`;
/// 64-bit EL0 and EL1 only, and the GIC's system registers.
const PFR0_KEPT: u64 = fields(&[16, 20, 48, 56, 60]);
const PFR0_GUEST: u64 = 1 | 1 << 4 | 1 << 24;
/// `ID_AA64PFR1_EL1`: `BT`, `SSBS` and `CSV2_frac`.
const PFR1_KEPT: u64 = fields(&[0, 4, 32]);
/// `ID_AA64DFR0_EL1`: Armv8.0's debug, two breakpoints and two watchpoints
/// as the least there is (the registers are nothing: see the kernel's
/// `system_register`), no `PMU`, and double lock not implemented.
const DFR0_GUEST: u64 = 6 | 1 << 12 | 1 << 20 | 0xF << 36;
/// `ID_AA64ISAR0_EL1`: all instructions but `TME`.
const ISAR0_KEPT: u64 = !field(24);
/// `ID_AA64ISAR1_EL1`: `DPB`, `JSCVT`, `FCMA`, `LRCPC`, `FRINTTS`, `SB`,
/// `BF16`, `DGH` and `I8MM`, without pointer authentication.
const ISAR1_KEPT: u64 = fields(&[0, 12, 16, 20, 32, 36, 44, 48, 52]);
/// `ID_AA64ISAR2_EL1`: `RPRES`, `BC` and `CLRBHB`.
const ISAR2_KEPT: u64 = fields(&[4, 20, 28]);
/// `ID_AA64MMFR0_EL1`: the sizes of addresses and granules, what endianness
/// there is, `SNSMem` and `ExS`.
const MMFR0_KEPT: u64 = fields(&[0, 4, 8, 12, 16, 20, 24, 28, 44]);
/// `PARange`, of `ID_AA64MMFR0_EL1`, at most: 48-bit addresses, what stage 2
/// translates.
const PARANGE_MOST: u64 = 5;
/// `ID_AA64MMFR1_EL1`: `HAFDBS`, `HPDS`, `PAN`, `ETS`, `nTLBPA` and
/// `ECBHB`.
const MMFR1_KEPT: u64 = fields(&[0, 12, 20, 36, 48, 60]);
/// `ID_AA64MMFR2_EL1`: `UAO`, `CCIDX`, `AT`, `TTL`, `BBM` and `E0PD`.
const MMFR2_KEPT: u64 = fields(&[4, 20, 32, 48, 52, 60]);

/// What a guest reads in the ID register `S3_0_C0_C<crm>_<op2>`, where the
/// processor has `value`.
pub fn id_register(crm: u8, op2: u8, value: u64) -> u64 {
    match (crm, op2) {
        (4, 0) => value & PFR0_KEPT | PFR0_GUEST,
        (4, 1) => value & PFR1_KEPT,
        (5, 0) => DFR0_GUEST,
        (6, 0) => value & ISAR0_KEPT,
        (6, 1) => value & ISAR1_KEPT,
        (6, 2) => value & ISAR2_KEPT,
        (7, 0) => value & MMFR0_KEPT & !field(0) | (value & field(0)).min(PARANGE_MOST),
        (7, 1) => value & MMFR1_KEPT,
        (7, 2) => value & MMFR2_KEPT,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What QEMU's `max` processor shows, a processor with much that
    /// guests are not to see.
    const PFR0: u64 = 0x1101_0013_1111_2222;
    const MMFR0: u64 = 0x1100_1021_0010_5146;
    const MMFR1: u64 = 0x0011_1110_2120_2122;

    #[test]
    fn a_guest_has_only_el0_and_el1_and_no_more_than_it_is_told() {
        let seen = id_register(4, 0, PFR0);
        assert_eq!(seen & 0xF, 1, "64-bit EL0");
        assert_eq!(seen >> 4 & 0xF, 1, "64-bit EL1");
        assert_eq!(seen >> 8 & 0xFF, 0, "no EL2 or EL3");
        assert_eq!(seen >> 24 & 0xF, 1, "the GIC's system registers");
        assert_eq!(seen >> 28 & 0xF, 0, "no RAS");
        assert_eq!(seen >> 32 & 0xF, 0, "no SVE");
    }

    #[test]
    fn what_needs_a_hypervisor_is_hidden() {
        assert_eq!(id_register(4, 1, 0x1234_5678_9ABC_DEF0) >> 8 & 0xF, 0, "no MTE");
        assert_eq!(id_register(6, 1, !0) >> 4 & 0xF, 0, "no pointer authentication");
        assert_eq!(id_register(6, 2, !0) & 0xF, 0, "no WFET");
        assert_eq!(id_register(6, 2, !0) >> 16 & 0xF, 0, "no MOPS");
        let mmfr1 = id_register(7, 1, MMFR1);
        assert_eq!(mmfr1 >> 8 & 0xF, 0, "no VHE");
        assert_eq!(mmfr1 >> 40 & 0xF, 0, "no HCRX_EL2");
        assert_eq!(id_register(7, 0, MMFR0) >> 56, 0, "no fine-grained traps or ECV");
    }

    #[test]
    fn registers_of_what_guests_cannot_have_read_zero() {
        // ID_AA64ZFR0_EL1 and ID_AA64SMFR0_EL1, ID_AA64MMFR3_EL1, and one
        // of AArch32's.
        for (crm, op2) in [(4, 4), (4, 5), (7, 3), (1, 0)] {
            assert_eq!(id_register(crm, op2, !0), 0);
        }
    }

    #[test]
    fn physical_addresses_stop_where_stage_2_does() {
        assert_eq!(id_register(7, 0, MMFR0) & 0xF, 5, "52-bit addresses read as 48");
        assert_eq!(id_register(7, 0, 2) & 0xF, 2, "a narrower processor's stay");
    }

    #[test]
    fn the_debug_architecture_is_the_least_there_is() {
        let seen = id_register(5, 0, !0);
        assert_eq!(seen & 0xF, 6);
        assert_eq!(seen >> 8 & 0xF, 0, "no PMU");
        assert_eq!(seen >> 36 & 0xF, 0xF, "no double lock");
    }
}
