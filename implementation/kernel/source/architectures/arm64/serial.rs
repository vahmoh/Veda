// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The serial console the kernel log goes to: the PL011 UART the firmware's
//! SPCR table keeps for an operating system (QEMU's console, which tests
//! read; a cable's), as the firmware set it up.
//!
//! It is given the log as fast as it takes it, never waited for: the kernel
//! writes its log holding its lock, which would stop every processor's
//! kernel meanwhile. When its transmitter is empty it takes its FIFO's
//! worth (16 bytes in every PL011); `log` gives it the rest later, from the
//! timer.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::memory::kvirt;
use crate::memory::paging::Cache;

/// The data register, and the flag register.
const DR: u64 = 0x00;
const FR: u64 = 0x18;
/// The transmit FIFO is full; it is empty.
const FR_TXFF: u32 = 1 << 5;
const FR_TXFE: u32 = 1 << 7;
/// Bytes the transmitter takes when it is empty.
const FIFO: usize = 16;

/// [`BASE`] before it is known whether there is a UART, which is until the
/// firmware's tables are read: the log waits for it meanwhile.
const NOT_KNOWN: u64 = 0;
/// [`BASE`] where there is none.
const NONE: u64 = 1;

/// Where the UART's registers are mapped.
static BASE: AtomicU64 = AtomicU64::new(NOT_KNOWN);

/// Maps the UART whose registers are at `phys`, if there is one.
pub(in crate::architecture) fn init(phys: Option<u64>) {
    let base = phys.map_or(NONE, |phys| kvirt::map_mmio(phys, 4096, Cache::Uncached));
    BASE.store(base, Ordering::Release);
}

fn flags(base: u64) -> u32 {
    // SAFETY: the UART's flag register, mapped as a device's.
    unsafe { core::ptr::read_volatile((base + FR) as *const u32) }
}

/// Whether there is a UART, as far as is known.
pub fn present() -> bool {
    BASE.load(Ordering::Acquire) != NONE
}

/// The UART's registers, once they are mapped.
fn mapped() -> Option<u64> {
    Some(BASE.load(Ordering::Acquire)).filter(|&base| base > NONE)
}

/// How many bytes the transmitter takes now, without waiting.
pub fn room() -> usize {
    mapped().map_or(0, |base| if flags(base) & FR_TXFE != 0 { FIFO } else { 0 })
}

/// Sends `b`, which [`room`] made room for.
pub fn put(b: u8) {
    let base = BASE.load(Ordering::Acquire);
    // SAFETY: the UART's data register, mapped as a device's.
    unsafe { core::ptr::write_volatile((base + DR) as *mut u32, b as u32) };
}

/// Sends `bytes` (a carriage return before each line feed), waiting for
/// the transmitter: when nothing else is to run again (a panic).
pub fn write_blocking(bytes: &[u8]) {
    for &b in bytes {
        if b == b'\n' {
            put_blocking(b'\r');
        }
        put_blocking(b);
    }
}

/// Sends `b`, waiting for the transmitter.
pub fn put_blocking(b: u8) {
    let Some(base) = mapped() else { return };
    let mut spins = 0u32;
    while flags(base) & FR_TXFF != 0 && spins < 1_000_000 {
        spins += 1;
        core::hint::spin_loop();
    }
    put(b);
}
