// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Where an ARM64 guest has what is not its RAM, which starts at 0: its
//! interrupt controller's registers, above the room below 4 GiB that BARs
//! have (`driver_virtual_machine`'s `PCI_LOW`).

/// The distributor's registers, 64 KiB.
pub const GIC_DISTRIBUTOR: u64 = 0xFF00_0000;
pub const GIC_DISTRIBUTOR_SIZE: u64 = 0x1_0000;
/// The redistributors': a run of [`GIC_REDISTRIBUTOR_SIZE`] for each
/// processor, in the order of their numbers.
pub const GIC_REDISTRIBUTORS: u64 = 0xFF10_0000;
pub const GIC_REDISTRIBUTOR_SIZE: u64 = 0x2_0000;
/// Room for the most processors a guest has.
pub const GIC_REDISTRIBUTORS_SIZE: u64 = 64 * GIC_REDISTRIBUTOR_SIZE;

// The redistributors end below 4 GiB, and after the distributor.
const _: () = assert!(GIC_DISTRIBUTOR + GIC_DISTRIBUTOR_SIZE <= GIC_REDISTRIBUTORS);
const _: () = assert!(GIC_REDISTRIBUTORS + GIC_REDISTRIBUTORS_SIZE <= 1 << 32);

/// Where a guest's devices reach the machine's ITS to raise their MSIs:
/// nothing of the guest's is here, so that the page can lead there in their
/// domain, and below 4 GiB, for the functions whose MSIs have 32 bits of
/// address. Its offset in the page is the `GITS_TRANSLATER`'s, which is 64
/// bytes into its own page.
pub const MSI_DOORBELL: u64 = 0xFFFF_0040;

/// Where the interrupt controller's registers are, none of which is memory.
pub fn is_gic(address: u64) -> bool {
    (GIC_DISTRIBUTOR..GIC_DISTRIBUTOR + GIC_DISTRIBUTOR_SIZE).contains(&address)
        || (GIC_REDISTRIBUTORS..GIC_REDISTRIBUTORS + GIC_REDISTRIBUTORS_SIZE).contains(&address)
}
