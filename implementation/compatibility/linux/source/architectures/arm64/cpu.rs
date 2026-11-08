// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What Linux says of an AArch64 processor in `/proc/cpuinfo`: its number,
//! its speed as Linux reckons it from the generic timer's frequency
//! (`BogoMIPS`), and its features by Linux's names for the bits of
//! `AT_HWCAP` and `AT_HWCAP2`.
//!
//! It is plain data, so the host's tests check it whatever the host is.

use alloc::string::String;
use alloc::vec::Vec;

/// Linux's names for the features of `AT_HWCAP`'s bits, in their order,
/// and of `AT_HWCAP2`'s, as far as Veda tells any.
const NAMES: [&str; 2] = [
    concat!(
        "fp asimd evtstrm aes pmull sha1 sha2 crc32 atomics fphp asimdhp cpuid asimdrdm jscvt fcma lrcpc dcpop ",
        "sha3 sm3 sm4 asimddp sha512 sve asimdfhm dit uscat ilrcpc flagm ssbs sb paca pacg",
    ),
    concat!(
        "dcpodp sve2 sveaes svepmull svebitperm svesha3 svesm4 flagm2 frint svei8mm svef32mm svef64mm svebf16 ",
        "i8mm bf16 dgh rng",
    ),
];

/// The entry of processor `cpu`, its generic timer counting at `hz`, with
/// the features `hwcap` (`AT_HWCAP`, `AT_HWCAP2`) tells.
pub fn entry(cpu: u32, hz: u64, hwcap: [u64; 2]) -> String {
    alloc::format!(
        "processor\t: {cpu}\nBogoMIPS\t: {}.{:02}\nFeatures\t: {}\n\n",
        hz / 500_000,
        hz / 5_000 % 100,
        features(hwcap)
    )
}

/// The features `hwcap` tells, as `Features` names them.
fn features(hwcap: [u64; 2]) -> String {
    let mut named = Vec::new();
    for (word, names) in hwcap.into_iter().zip(NAMES) {
        named.extend(names.split(' ').enumerate().filter(|&(bit, _)| word & 1 << bit != 0).map(|(_, name)| name));
    }
    named.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn features_are_named_as_linux_names_them() {
        assert_eq!(features([0b11, 0]), "fp asimd");
        assert_eq!(features([1 | 1 << 3 | 1 << 31, 1 << 16]), "fp aes pacg rng");
        assert_eq!(NAMES.map(|names| names.split(' ').count()), [32, 17]);
    }

    #[test]
    fn an_entry_is_linux_s() {
        // QEMU's 62.5 MHz timer.
        assert_eq!(entry(1, 62_500_000, [0b11, 0]), "processor\t: 1\nBogoMIPS\t: 125.00\nFeatures\t: fp asimd\n\n");
        assert!(entry(0, 24_000_000, [1, 0]).contains("BogoMIPS\t: 48.00\n"));
    }
}
