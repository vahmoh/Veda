// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Where the guest's PCI functions go, and how many interrupts each may
//! have.
//!
//! * On the guest's bus 0 ([`places`]): a function on the host's bus 0
//!   keeps its device and function numbers if its device's function 0
//!   comes too (drivers may look for siblings where they are on the PC);
//!   the others take a free device number, as its function 0. Device 0 is
//!   the host bridge's, as on a PC.
//! * Its memory BARs ([`Windows`]): below 4 GiB those that must be (32-bit
//!   ones) or fit, the rest above, each naturally aligned; and the memory
//!   the firmware keeps for functions may be where nothing else of the
//!   guest's is.
//! * Its MSIs ([`messages`]): as many as its MSI or MSI-X capability says it
//!   can send, each by its index (MSI's message number, MSI-X's entry).

use alloc::vec::Vec;
use core::ops::Range;

/// Where in the guest's address space BARs go: below 4 GiB the ones that
/// must (32-bit) or fit, the rest above; and where the memory the firmware
/// keeps for functions may be (where nothing else of the guest's is).
#[derive(Debug, Clone)]
pub struct Windows {
    pub low: Range<u64>,
    pub high: Range<u64>,
    pub free: Vec<Range<u64>>,
}

impl Windows {
    /// A naturally aligned place for `size` bytes (a power of two): in the
    /// low window if `low_only` or it fits there, else the high one.
    pub fn place(&mut self, size: u64, low_only: bool) -> Option<u64> {
        let take = |w: &mut Range<u64>| {
            let at = w.start.checked_next_multiple_of(size)?;
            (at.checked_add(size)? <= w.end).then(|| {
                w.start = at + size;
                at
            })
        };
        take(&mut self.low).or_else(|| if low_only { None } else { take(&mut self.high) })
    }
}

/// Where each function of `functions` (bus, device, function on the host)
/// goes on the guest's bus 0: device << 3 | function, and whether its
/// device has other functions there; `None` once the bus is full. Device 0
/// stays the host bridge's.
pub fn places(functions: &[(u8, u8, u8)]) -> Vec<Option<(u8, bool)>> {
    let keeps = |&(bus, dev, func): &(u8, u8, u8)| {
        bus == 0 && dev < 32 && (func == 0 || functions.iter().any(|&(b, d, f)| (b, d, f) == (0, dev, 0)))
    };
    let mut taken = [false; 32];
    for f in functions.iter().filter(|f| keeps(f)) {
        taken[f.1 as usize] = true;
    }
    // Device 0 is the host bridge's.
    let mut free = (1..32u8).filter(|&d| !taken[d as usize]).collect::<Vec<_>>().into_iter();
    functions
        .iter()
        .map(|f| {
            if keeps(f) {
                let siblings = functions.iter().filter(|g| keeps(g) && g.1 == f.1).count();
                Some(((f.1 << 3) | (f.2 & 7), siblings > 1))
            } else {
                free.next().map(|d| (d << 3, false))
            }
        })
        .collect()
}

/// How many MSIs a function can send, by its capabilities' message
/// control registers: its MSI capability's (`msi`: as many as Multiple
/// Message Capable says, one to 32), its MSI-X capability's (`msix`: as
/// many as its table has entries, up to 2048); the more of the two, which
/// the guest may use one at a time. None without either.
pub fn messages(msi: Option<u16>, msix: Option<u16>) -> u32 {
    // Multiple Message Capable, bits 3:1: a power of two, 32 at most (its
    // larger values are reserved).
    let msi = msi.map_or(0, |control| 1u32 << ((control >> 1) & 7).min(5));
    // Table Size, bits 10:0: the entries less one.
    let msix = msix.map_or(0, |control| u32::from(control & 0x7FF) + 1);
    msi.max(msix)
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;

    #[test]
    fn bars_are_aligned_below_4_gib_first() {
        let mut w = Windows { low: 0xC000_0000..0xC010_0000, high: (128 << 30)..(256 << 30), free: Vec::new() };
        assert_eq!(w.place(0x1000, true), Some(0xC000_0000));
        // Naturally aligned: past the first page.
        assert_eq!(w.place(0x10000, false), Some(0xC001_0000));
        // A 64-bit BAR that does not fit below goes above.
        assert_eq!(w.place(0x100000, false), Some(128 << 30));
        // A 32-bit one does not go anywhere then.
        assert_eq!(w.place(0x100000, true), None);
        assert_eq!(w.place(1 << 63, false), None);
    }

    #[test]
    fn functions_keep_their_place_on_bus_0_with_their_siblings() {
        // 00:02.0 (a GPU), 00:1f.3 (audio) with its device's function 0,
        // 00:1f.0, and 03:00.0 (behind a bridge).
        let places = places(&[(0, 2, 0), (0, 0x1f, 3), (0, 0x1f, 0), (3, 0, 0)]);
        assert_eq!(
            places,
            vec![Some((2 << 3, false)), Some((0x1f << 3 | 3, true)), Some((0x1f << 3, true)), Some((1 << 3, false))]
        );
        // Without its function 0 a function goes where there is room.
        assert_eq!(super::places(&[(0, 0x1f, 3)]), vec![Some((1 << 3, false))]);
    }

    #[test]
    fn the_bus_holds_31_devices() {
        let many: Vec<(u8, u8, u8)> = (0..40).map(|b| (b + 1, 0, 0)).collect();
        let places = places(&many);
        assert_eq!(places.iter().filter(|p| p.is_some()).count(), 31);
        assert_eq!(places[30], Some((31 << 3, false)));
        assert_eq!(places[31], None);
    }

    #[test]
    fn a_function_has_as_many_msis_as_its_capabilities_say() {
        assert_eq!(messages(None, None), 0);
        // MSI: one, then 2^MMC, at most 32.
        assert_eq!(messages(Some(0x0080), None), 1);
        assert_eq!(messages(Some(0x0086), None), 8);
        assert_eq!(messages(Some(0x000E), None), 32);
        // MSI-X: its table's entries.
        assert_eq!(messages(None, Some(0x0000)), 1);
        assert_eq!(messages(Some(0x0080), Some(0x8007)), 8);
        assert_eq!(messages(None, Some(0xFFFF)), 2048);
    }
}
