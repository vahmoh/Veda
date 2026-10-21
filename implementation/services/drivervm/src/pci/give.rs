// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Giving a function to the guest, whole or not at all: the memory the
//! firmware keeps for it, and its memory BARs, mapped into the guest's
//! memory where the guest has them, its errors to the host turned off,
//! then its DMA (the guest's IOMMU domain). If any of it cannot be, none of
//! it is: what was mapped is unmapped, its registers are as they were, and
//! the function is the host's as it was (its channel closes). And what an
//! Intel integrated GPU's driver reads of the PC's firmware, for the guest.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;

use abi::map_flags;
use drivervm_core::functions::{Windows, messages};
use hv::pci::{Bar, HostBridge};
use proto::pci::{DeviceInfo, cap, find_capability, pcidev};
use rt::object::Guest;
use rt::println;

const PAGE: u64 = 4096;

/// Where a function given is in the guest: its BARs, its PCI Express
/// capability, the memory the firmware keeps for it, and how many MSIs it
/// can send.
pub struct Placed {
    pub bars: Vec<Bar>,
    pub pci_express: Option<u16>,
    pub kept: Vec<Range<u64>>,
    pub msis: u32,
}

/// The guest's memory mapped for a function being given: unmapped when
/// this goes, unless the function was given.
struct Mapped<'a> {
    guest: &'a Guest,
    ranges: Vec<Range<u64>>,
}

impl Drop for Mapped<'_> {
    fn drop(&mut self) {
        for r in &self.ranges {
            let _ = self.guest.unmap(r.start, (r.end - r.start) as usize);
        }
    }
}

/// Gives the guest function `info` (`pci`'s), whole or not at all: the
/// memory the firmware keeps for it (but what `reserved` has, which a
/// function given before brought) and its memory BARs (placed in
/// `windows`, which keeps the places only if it is given) mapped into the
/// guest's memory, its errors to the host turned off, then its DMA.
pub fn give(
    guest: &Guest,
    pci: &pcidev::Client,
    info: &DeviceInfo,
    windows: &mut Windows,
    reserved: &[Range<u64>],
) -> Result<Placed, String> {
    let resource = pci.device_resource().map_err(|e| format!("devmgr: {e}"))?.map_err(|e| format!("{e:?}"))?;
    // The memory the firmware keeps for it, where the PC has it: in the
    // guest's address space, so in its IOMMU domain, before it joins (a
    // function given before may have had the same).
    let kept = pci.reserved_memory().map_err(|e| format!("devmgr: {e}"))?.map_err(|e| format!("{e:?}"))?;
    let mut ranges = Vec::new();
    let mut mapped = Mapped { guest, ranges: Vec::new() };
    for k in &kept {
        let Some(end) = k.base.checked_add(k.size).filter(|_| k.size > 0) else {
            return Err(format!("the memory the firmware keeps for it ({:#x}, {} bytes) is none", k.base, k.size));
        };
        let range = k.base..end;
        ranges.push(range.clone());
        if reserved.contains(&range) {
            continue;
        }
        let at = format!("the memory the firmware keeps for it ({:#x}-{:#x})", range.start, range.end - 1);
        let free = windows.free.iter().any(|f| f.start <= range.start && range.end <= f.end)
            && !reserved.iter().chain(&mapped.ranges).any(|r| r.start < range.end && range.start < r.end);
        if !free {
            return Err(format!("{at} is where the guest has something else"));
        }
        guest
            .map(&k.memory, 0, k.size as usize, k.base, map_flags::READ | map_flags::WRITE)
            .map_err(|e| format!("{at} cannot be the guest's: {e}"))?;
        mapped.ranges.push(range);
    }
    let mut placing = windows.clone();
    let bars = map_bars(guest, pci, info, &mut placing, &mut mapped)?;
    // No errors to the host, before its DMA is the guest's.
    let pci_express = find_capability(pci, hv::pci::CAP_PCI_EXPRESS);
    let quieted = quiet_errors(pci, pci_express);
    // Its DMA reaches the guest's memory from now on: once all the rest is
    // the guest's, before the guest runs (it never does DMA the guest
    // programmed with host addresses).
    let sid = (info.bus as u16) << 8 | (info.slot as u16) << 3 | info.function as u16;
    if let Err(e) = guest.attach_device(&resource, sid) {
        quieted.restore(pci);
        return Err(format!("{e}"));
    }
    mapped.ranges.clear();
    *windows = placing;
    Ok(Placed { bars, pci_express, kept: ranges, msis: msi_messages(pci) })
}

/// Maps the function's memory BARs into the guest's memory (`mapped`
/// has them): where it sees them, placed in `windows`. A BAR the guest
/// cannot have whole pages of (smaller than a page, or not on one) stays
/// out.
fn map_bars(
    guest: &Guest,
    pci: &pcidev::Client,
    info: &DeviceInfo,
    windows: &mut Windows,
    mapped: &mut Mapped,
) -> Result<Vec<Bar>, String> {
    let mut bars = Vec::new();
    for b in info.bars.iter().filter(|b| !b.io) {
        if b.size < PAGE || !b.address.is_multiple_of(PAGE) || !b.size.is_power_of_two() {
            println!(
                "BAR {} ({} bytes at {:#x}) is not whole pages; the guest does not get it",
                b.index, b.size, b.address
            );
            continue;
        }
        let is64 =
            pci.config_read(0x10 + 4 * b.index as u16, 4).ok().and_then(|r| r.ok()).is_some_and(|v| v & 0b110 == 0b100);
        let address = windows
            .place(b.size, !is64)
            .ok_or_else(|| format!("no room for BAR {} ({} KiB)", b.index, b.size / 1024))?;
        let vmo =
            pci.map_bar(b.index).map_err(|e| format!("devmgr: {e}"))?.map_err(|e| format!("BAR {}: {e:?}", b.index))?;
        guest
            .map(&vmo, 0, b.size as usize, address, map_flags::READ | map_flags::WRITE)
            .map_err(|e| format!("BAR {} cannot be mapped: {e}", b.index))?;
        mapped.ranges.push(address..address + b.size);
        bars.push(Bar { index: b.index, address, size: b.size, is64, prefetchable: b.prefetchable });
    }
    Ok(bars)
}

/// What a function's registers that report errors held before
/// [`quiet_errors`]: (offset, value).
struct Quieted([Option<(u16, u32)>; 2]);

impl Quieted {
    /// Puts them back.
    fn restore(self, pci: &pcidev::Client) {
        for (at, value) in self.0.into_iter().flatten() {
            let _ = pci.config_write(at, 2, value);
        }
    }
}

/// Turns off the errors the function reports to the host (its SERR# and
/// PCI Express error reporting; see `hv::pci`), before the guest runs:
/// what the registers held.
fn quiet_errors(pci: &pcidev::Client, pci_express: Option<u16>) -> Quieted {
    let read = |at| pci.config_read(at, 2).ok().and_then(|r| r.ok());
    let mut was = [None, None];
    if let Some(command) = read(0x04) {
        let _ = pci.config_write(0x04, 2, command & !(hv::pci::COMMAND_SERR as u32));
        was[0] = Some((0x04, command));
    }
    if let Some(at) = pci_express.map(|p| p + hv::pci::DEVCTL)
        && let Some(control) = read(at)
    {
        let _ = pci.config_write(at, 2, control & !(hv::pci::DEVCTL_ERROR_REPORTING as u32));
        was[1] = Some((at, control));
    }
    Quieted(was)
}

/// How many MSIs the function can send, by its MSI and MSI-X
/// capabilities.
fn msi_messages(pci: &pcidev::Client) -> u32 {
    let control = |id| {
        let at = find_capability(pci, id)?;
        pci.config_read(at + 2, 2).ok()?.ok().map(|v| v as u16)
    };
    messages(control(cap::MSI), control(cap::MSI_X))
}

/// Whether the function is an Intel integrated GPU (a display controller of
/// Intel's on the root bus).
pub fn is_intel_gpu(info: &DeviceInfo) -> bool {
    info.vendor == 0x8086 && info.class == 0x03 && info.bus == 0
}

/// What the driver of Intel GPU `pci` (`name`) reads of the PC's firmware:
/// the guest's copy of its OpRegion (none if the firmware has none), and
/// the stand-in for the PC's host bridge.
pub fn intel_gpu(pci: &pcidev::Client, name: &str) -> (Option<Vec<u8>>, Option<HostBridge>) {
    let copy = match pci.opregion() {
        Ok(Ok(o)) => {
            let vbt = (!o.vbt.is_empty()).then_some(&o.vbt[..]);
            let copy = hv::igd::for_guest(&o.opregion, vbt, crate::architecture::OPREGION_AT);
            let vbt = match vbt {
                Some(v) => format!("a VBT of {} bytes after it", v.len()),
                None => String::from("its VBT in it"),
            };
            match &copy {
                Some(_) => {
                    println!("{name}: its OpRegion is the guest's at {:#x}, {vbt}", crate::architecture::OPREGION_AT)
                }
                None => println!("{name}: its OpRegion is not one the guest can have"),
            }
            copy
        }
        Ok(Err(e)) => {
            println!("{name}: no OpRegion ({e:?})");
            None
        }
        Err(e) => {
            println!("{name}: devmgr: {e}");
            None
        }
    };
    let bridge = match pci.host_bridge() {
        Ok(Ok(b)) => {
            println!(
                "{name}: the PC's host bridge ({:04x}:{:04x}) is at the guest's 00:00.0",
                b.ids & 0xFFFF,
                b.ids >> 16
            );
            Some(HostBridge {
                ids: b.ids,
                class: b.class,
                subsystem: b.subsystem,
                mchbar: b.mchbar,
                graphics_control: b.graphics_control,
            })
        }
        _ => None,
    };
    (copy, bridge)
}
