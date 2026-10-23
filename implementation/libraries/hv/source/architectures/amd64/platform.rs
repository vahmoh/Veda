// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The paravirtual platform (`crate::platform`) as x86-64 guests have it.
//!
//! A guest's processors have an x2APIC whose timer runs on the TSC
//! (`TSC-deadline` mode included); a processor's number is its local APIC
//! id. The guest finds the platform through `cpuid`:
//!
//! * leaf [`CPUID_SIGNATURE`]: the highest platform leaf in `eax`, and
//!   [`SIGNATURE`] in `ebx`, `ecx` and `edx`;
//! * leaf [`CPUID_PLATFORM`]: the platform's features in `eax` (none yet)
//!   and the number of processors in `ebx`, whose local APIC ids are 0 to
//!   that number less one;
//! * leaf [`CPUID_TIMING`]: the TSC's frequency in `eax` and the local APIC
//!   timer's in `ebx`, in kHz.
//!
//! The TSC is the host's own (no offset, no scaling): invariant, and the
//! same on every processor.
//!
//! A hypercall is `vmcall` with the call's number in `rax` and its
//! arguments in `rbx`, `rcx`, `rdx`, `rsi` and `rdi`, in that order; the
//! result comes back in `rax`. A processor started with
//! `start_cpu(cpu, entry)` starts at `entry` in 64-bit mode with interrupts
//! off, on page tables that map the first 4 GiB of guest-physical memory as
//! they are. The ACPI tables' RSDP is where the boot parameters say
//! (`acpi_rsdp_addr`, see `linux`). The Linux guest's side of all this is
//! `arch/x86/kernel/cpu/veda.c`, `arch/x86/pci/veda.c` and
//! `arch/x86/include/asm/veda_para.h` in the Linux port
//! (`implementation/ports/linux`).

/// The leaf of the platform's signature.
pub const CPUID_SIGNATURE: u32 = 0x4000_0000;
/// The leaf of the platform's features and processors.
pub const CPUID_PLATFORM: u32 = 0x4000_0001;
/// The leaf of the clocks' frequencies.
pub const CPUID_TIMING: u32 = 0x4000_0010;
/// The highest platform leaf.
pub const CPUID_MAX: u32 = CPUID_TIMING;

/// What `ebx`, `ecx` and `edx` of [`CPUID_SIGNATURE`] spell.
pub const SIGNATURE: [u8; 12] = *b"VedaVedaVeda";

/// The registers of [`SIGNATURE`]: `ebx`, `ecx`, `edx`.
pub fn signature_registers() -> [u32; 3] {
    let w = |i: usize| u32::from_le_bytes([SIGNATURE[i], SIGNATURE[i + 1], SIGNATURE[i + 2], SIGNATURE[i + 3]]);
    [w(0), w(4), w(8)]
}

/// The message of a `pci_msi` hypercall, as its result holds it: the
/// address (its low 32 bits; an x86 MSI's high ones are zero) in the low
/// half, the data in the high half.
pub fn msi_result(address: u64, data: u32) -> u64 {
    (address & 0xFFFF_FFFF) | ((data as u64) << 32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn msi_results_hold_address_and_data() {
        assert_eq!(msi_result(0xFEE0_00B0, 0), 0xFEE0_00B0);
        assert_eq!(msi_result(0xFEE0_1000, 0x41), 0x41_FEE0_1000);
    }

    #[test]
    fn signature_spells_veda() {
        let [b, c, d] = signature_registers();
        let mut s = [0u8; 12];
        s[..4].copy_from_slice(&b.to_le_bytes());
        s[4..8].copy_from_slice(&c.to_le_bytes());
        s[8..].copy_from_slice(&d.to_le_bytes());
        assert_eq!(&s, b"VedaVedaVeda");
    }
}
