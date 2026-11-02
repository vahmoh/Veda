// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The loader's log on ARM64: polled output on the PL011 UART the firmware's
//! ACPI tables name (SPCR, the console they keep for an operating system),
//! which the firmware has already set up and maps. Usable both before and
//! after `ExitBootServices`, until the kernel's address space is entered,
//! after the loader's last line. No such UART: no log.

use core::fmt;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::uefi::{ACPI_20_TABLE, SystemTable};

/// The UART's data register, and its flag register.
const DR: u64 = 0x00;
const FR: u64 = 0x18;
/// The transmit FIFO is full.
const FR_TXFF: u32 = 1 << 5;

/// SPCR's interface types: a PL011, and the subset of it Arm's server
/// standards ask for.
const PL011: u8 = 3;
const SBSA_GENERIC: u8 = 0x0E;

/// The UART's registers (0: there is none).
static BASE: AtomicU64 = AtomicU64::new(0);

/// The little-endian 64-bit word at `address`.
///
/// # Safety
/// `address` must be readable.
unsafe fn u64_at(address: u64) -> u64 {
    // SAFETY: per the caller.
    unsafe { core::ptr::read_unaligned(address as *const u64) }
}

/// The address of a PL011's registers, as the SPCR among the tables of RSDP
/// `rsdp` gives them.
///
/// # Safety
/// `rsdp` must be the firmware's, with its tables mapped where they are.
unsafe fn spcr_uart(rsdp: u64) -> Option<u64> {
    // SAFETY: the tables are where the firmware says, each its length long.
    unsafe {
        let xsdt = u64_at(rsdp + 24);
        let length = core::ptr::read_unaligned((xsdt + 4) as *const u32) as u64;
        for entry in (xsdt + 36..xsdt + length).step_by(8) {
            let table = u64_at(entry);
            if core::ptr::read_unaligned(table as *const [u8; 4]) != *b"SPCR" {
                continue;
            }
            let interface = *((table + 36) as *const u8);
            return matches!(interface, PL011 | SBSA_GENERIC).then(|| u64_at(table + 44));
        }
    }
    None
}

/// Finds the UART among the firmware's tables.
pub fn init(st: &SystemTable) {
    // SAFETY: the configuration table has `number_of_table_entries` entries.
    let tables = unsafe { core::slice::from_raw_parts(st.configuration_table, st.number_of_table_entries) };
    let Some(acpi) = tables.iter().find(|t| t.vendor_guid == ACPI_20_TABLE) else { return };
    // SAFETY: the firmware's own table.
    if let Some(base) = unsafe { spcr_uart(acpi.vendor_table as u64) } {
        BASE.store(base, Ordering::Relaxed);
    }
}

fn write_byte(base: u64, b: u8) {
    // SAFETY: polling the flag register and writing the data register of
    // the UART the firmware named, which it maps.
    unsafe {
        let mut spins = 0u32;
        while core::ptr::read_volatile((base + FR) as *const u32) & FR_TXFF != 0 && spins < 100_000 {
            spins += 1;
            core::hint::spin_loop();
        }
        core::ptr::write_volatile((base + DR) as *mut u32, b as u32);
    }
}

pub struct Serial;

impl fmt::Write for Serial {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let base = BASE.load(Ordering::Relaxed);
        if base == 0 {
            return Ok(());
        }
        for b in s.bytes() {
            if b == b'\n' {
                write_byte(base, b'\r');
            }
            write_byte(base, b);
        }
        Ok(())
    }
}
