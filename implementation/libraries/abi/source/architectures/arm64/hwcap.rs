// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What programs are told of the processor, as they cannot read its ID
//! registers: Linux's `AT_HWCAP` and `AT_HWCAP2` on AArch64, whose bits
//! programs and libraries already know.
//!
//! Only what a program can use by itself is told. What needs the kernel's
//! part too (SVE, SME, MTE, pointer authentication, BTI) is not, nor is
//! `HWCAP_CPUID`, as the kernel does not answer for the ID registers.

/// The ID registers features are read from.
#[derive(Debug, Clone, Copy, Default)]
pub struct IdRegisters {
    pub pfr0: u64,
    pub pfr1: u64,
    pub isar0: u64,
    pub isar1: u64,
    pub mmfr2: u64,
}

/// A feature's field: the bit that tells it, the register and where the
/// field is in it, and the least value that shows it.
type Field = (u32, u64, u32, u64);

/// `AT_HWCAP` and `AT_HWCAP2` of a processor with these registers.
pub fn of(id: &IdRegisters) -> [u64; 2] {
    let field = |register: u64, at: u32| (register >> at) & 0xF;
    let told = |fields: &[Field]| {
        fields.iter().filter(|&&(_, register, at, least)| field(register, at) >= least).fold(0, |all, f| all | 1 << f.0)
    };
    // Floating point and SIMD are signed, 0xF when there are none.
    let (fp, simd) = (field(id.pfr0, 16), field(id.pfr0, 20));
    let mut first = told(&[
        (3, id.isar0, 4, 1),   // AES
        (4, id.isar0, 4, 2),   // PMULL
        (5, id.isar0, 8, 1),   // SHA1
        (6, id.isar0, 12, 1),  // SHA2
        (7, id.isar0, 16, 1),  // CRC32
        (8, id.isar0, 20, 2),  // ATOMICS
        (12, id.isar0, 28, 1), // ASIMDRDM
        (13, id.isar1, 12, 1), // JSCVT
        (14, id.isar1, 16, 1), // FCMA
        (15, id.isar1, 20, 1), // LRCPC
        (16, id.isar1, 0, 1),  // DCPOP
        (17, id.isar0, 32, 1), // SHA3
        (18, id.isar0, 36, 1), // SM3
        (19, id.isar0, 40, 1), // SM4
        (20, id.isar0, 44, 1), // ASIMDDP
        (21, id.isar0, 12, 2), // SHA512
        (23, id.isar0, 48, 1), // ASIMDFHM
        (24, id.pfr0, 48, 1),  // DIT
        (25, id.mmfr2, 32, 1), // USCAT
        (26, id.isar1, 20, 2), // ILRCPC
        (27, id.isar0, 52, 1), // FLAGM
        (28, id.pfr1, 4, 2),   // SSBS
        (29, id.isar1, 36, 1), // SB
    ]);
    for (present, half_precision, bits) in [(fp != 0xF, fp == 1, [0, 9]), (simd != 0xF, simd == 1, [1, 10])] {
        first |= (present as u64) << bits[0] | (half_precision as u64) << bits[1];
    }
    let second = told(&[
        (0, id.isar1, 0, 2),   // DCPODP
        (7, id.isar0, 52, 2),  // FLAGM2
        (8, id.isar1, 32, 1),  // FRINT
        (13, id.isar1, 52, 1), // I8MM
        (14, id.isar1, 44, 1), // BF16
        (15, id.isar1, 48, 1), // DGH
        (16, id.isar0, 60, 1), // RNG
    ]);
    [first, second]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_least_processor_has_floating_point_and_simd() {
        assert_eq!(of(&IdRegisters::default()), [0b11, 0]);
    }

    #[test]
    fn half_precision_goes_with_its_unit() {
        let [first, _] = of(&IdRegisters { pfr0: 1 << 16 | 1 << 20, ..IdRegisters::default() });
        assert_eq!(first, 1 | 1 << 1 | 1 << 9 | 1 << 10);
        assert_eq!(of(&IdRegisters { pfr0: 0xF << 16 | 0xF << 20, ..IdRegisters::default() }), [0, 0]);
    }

    #[test]
    fn a_field_shows_more_as_it_grows() {
        // AES alone, then with PMULL; SHA-256, then with SHA-512.
        let first = |isar0| of(&IdRegisters { isar0, ..IdRegisters::default() })[0];
        assert_eq!(first(1 << 4), 0b11 | 1 << 3);
        assert_eq!(first(2 << 4), 0b11 | 1 << 3 | 1 << 4);
        assert_eq!(first(1 << 12), 0b11 | 1 << 6);
        assert_eq!(first(2 << 12), 0b11 | 1 << 6 | 1 << 21);
    }

    #[test]
    fn what_needs_the_kernel_is_not_told() {
        // All of it there, as far as the registers read.
        let everything = IdRegisters { pfr0: 0x1111_1111_1111_1111, pfr1: !0, isar0: !0, isar1: !0, mmfr2: !0 };
        let [first, second] = of(&everything);
        for bit in [11, 22, 30, 31] {
            assert_eq!(first & 1 << bit, 0, "HWCAP bit {bit}");
        }
        for bit in [1, 2, 3, 4, 5, 6, 9, 10, 11, 12, 17] {
            assert_eq!(second & 1 << bit, 0, "HWCAP2 bit {bit}");
        }
    }

    #[test]
    fn features_of_the_second_word_are_there() {
        let id = IdRegisters { isar0: 1 << 60, isar1: 1 << 52, ..IdRegisters::default() };
        assert_eq!(of(&id)[1], 1 << 13 | 1 << 16);
    }
}
