//! Where a guest has what is below 1 MiB, as a PC has it: in the legacy
//! area, which the guest's memory map reserves, the copy of an Intel GPU's
//! OpRegion (`crate::igd`), then the ACPI tables (`crate::acpi`), the RSDP
//! first.

/// Where the guest's copy of the OpRegion is (with its raw VBT right after
/// it, in at most `igd::OPREGION_ROOM` bytes).
pub const OPREGION_AT: u64 = 0xC_0000;
/// Where the ACPI tables are, the RSDP first, and the room they have.
pub const ACPI_AT: u64 = 0xE_0000;
pub const ACPI_ROOM: usize = 0x2_0000;

// The copy ends where the ACPI tables begin.
const _: () = assert!(OPREGION_AT + crate::igd::OPREGION_ROOM as u64 <= ACPI_AT);
