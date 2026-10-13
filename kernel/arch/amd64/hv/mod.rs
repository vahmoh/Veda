// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Virtual machines on AMD64: Intel VMX with EPT.
//!
//! A guest's physical address space is translated by extended page tables
//! (`ept`); its processors run in VMX non-root operation (`vcpu`), with
//! what the processor offers for it (`vmx`).

mod ept;
mod vcpu;
pub(in crate::arch) mod vmx;

/// The tables that translate a guest's physical addresses.
pub use ept::Ept as GuestTables;
pub use vcpu::Vcpu;

/// Whether this machine can run guests, and why not.
pub fn available() -> Result<(), &'static str> {
    vmx::caps().map(|_| ())
}

/// The vector of an interrupt a virtual processor takes, if `vector` is
/// one: 32 to 255 (those below are the processor's exceptions).
pub fn interrupt_vector(vector: usize) -> Option<u8> {
    u8::try_from(vector).ok().filter(|&v| v >= 32)
}

/// Logs whether this machine can run guests, once at boot.
pub fn report() {
    match vmx::caps() {
        Ok(c) => crate::kinfo!(
            "hv: VMX with EPT{}; preemption timer at TSC/{}",
            if c.vpid { " and VPIDs" } else { "" },
            1u32 << c.preemption_rate
        ),
        Err(why) => crate::kinfo!("hv: no virtual machines ({})", why),
    }
}
