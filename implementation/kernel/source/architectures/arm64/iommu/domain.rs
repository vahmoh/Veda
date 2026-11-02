// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! A guest's domain: the stage 2 tables that translate its devices'
//! requests (the guest's memory, as its processors see it, and the ITS's
//! doorbell), and the streams in it.

use alloc::vec::Vec;

use abi::Error;
use iommu::arm64::smmu::command;
use iommu::arm64::smmu::stage2::{
    ACCESSED, ADDRESS, DEVICE, INNER_SHAREABLE, READ, TABLE_OR_PAGE, VALID, WRITE, WRITE_BACK,
};
use iommu::arm64::smmu::{ste_abort, ste_stage2};

use super::{DOORBELL, Iommu, iommu, publish};
use crate::architecture::arm64::its;
use crate::memory::{phys, phys_to_virt};

pub struct Domain {
    vmid: u16,
    /// The top table.
    root: u64,
    /// The devices in it (their requester ids) and their streams.
    streams: Vec<(u16, u32)>,
}

/// A requester id as `bus:device.function`, as the log names devices.
fn bdf(device: u16) -> alloc::string::String {
    alloc::format!("{:02x}:{:02x}.{}", device >> 8, (device >> 3) & 0x1F, device & 7)
}

fn table(frame: u64) -> &'static mut [u64; 512] {
    // SAFETY: the domain's tables are frames it owns, direct-mapped.
    unsafe { &mut *(phys_to_virt(frame) as *mut [u64; 512]) }
}

fn index(address: u64, level: u32) -> usize {
    ((address >> (12 + 9 * level)) & 0x1FF) as usize
}

impl Domain {
    /// An empty domain, with the ITS's doorbell (`NotSupported` without an
    /// IOMMU).
    pub fn new() -> Result<Domain, Error> {
        let iommu = iommu().ok_or(Error::NotSupported)?;
        let vmid = iommu.vmids.lock().take().ok_or(Error::LimitReached)?;
        let Some(root) = phys::alloc_zeroed() else {
            iommu.vmids.lock().free.push(vmid);
            return Err(Error::NoMemory);
        };
        publish(iommu, root, 4096);
        let mut domain = Domain { vmid, root, streams: Vec::new() };
        if let Some(doorbell) = its::doorbell_page() {
            domain.leaf(DOORBELL & !0xFFF, doorbell, DEVICE | WRITE)?;
        }
        Ok(domain)
    }

    /// Requests reach addresses below this.
    pub fn limit() -> u64 {
        iommu().map_or(0, |iommu| 1 << iommu.unit.capabilities.address_bits())
    }

    fn iommu(&self) -> &'static Iommu {
        // A domain exists only with the IOMMU.
        iommu().expect("a domain without an IOMMU")
    }

    /// Makes the page at `address` lead to `frame`, with `bits`.
    fn leaf(&mut self, address: u64, frame: u64, bits: u64) -> Result<(), Error> {
        let iommu = self.iommu();
        let mut t = self.root;
        for level in (1..=3).rev() {
            let at = t + index(address, level) as u64 * 8;
            let e = &mut table(t)[index(address, level)];
            if *e & VALID == 0 {
                let next = phys::alloc_zeroed().ok_or(Error::NoMemory)?;
                publish(iommu, next, 4096);
                *e = next | VALID | TABLE_OR_PAGE;
                publish(iommu, at, 8);
            }
            t = *e & ADDRESS;
        }
        table(t)[index(address, 0)] = (frame & ADDRESS) | VALID | TABLE_OR_PAGE | ACCESSED | INNER_SHAREABLE | bits;
        publish(iommu, t + index(address, 0) as u64 * 8, 8);
        Ok(())
    }

    /// Maps the page at `address` to `frame`. The devices see it after
    /// [`Domain::commit`]. Nothing of a guest's is where the doorbell is
    /// (`hv::arm64::memory::MSI_DOORBELL`).
    pub fn map(&mut self, address: u64, frame: u64, read: bool, write: bool) -> Result<(), Error> {
        if address >= Self::limit() || address & !0xFFF == DOORBELL & !0xFFF {
            return Err(Error::OutOfRange);
        }
        let access = if read { READ } else { 0 } | if write { WRITE } else { 0 };
        self.leaf(address, frame, WRITE_BACK | access)
    }

    /// Removes the mapping of the page at `address`, if any. The page may
    /// be reused only after [`Domain::commit`].
    pub fn unmap(&mut self, address: u64) {
        let iommu = self.iommu();
        let mut t = self.root;
        for level in (1..=3).rev() {
            let e = table(t)[index(address, level)];
            if e & VALID == 0 {
                return;
            }
            t = e & ADDRESS;
        }
        table(t)[index(address, 0)] = 0;
        publish(iommu, t + index(address, 0) as u64 * 8, 8);
    }

    /// Makes the devices see the domain as it is now: the SMMU forgets
    /// what it cached of it. Needed after unmapping, before the pages are
    /// reused; the SMMU caches no absent entries.
    pub fn commit(&self, unmapped: bool) {
        if !unmapped {
            return;
        }
        let iommu = self.iommu();
        if let Err(e) = iommu.unit.command(&[command::forget_vmid(self.vmid)]) {
            crate::kwarn!("iommu: unit at {:#x}: {}", iommu.address, e);
        }
    }

    /// Puts device `device` (a requester id on segment 0) in the domain. It
    /// must be in the host's domain, or in none (left by a guest that
    /// ended); not in another guest's.
    pub fn attach(&mut self, device: u16) -> Result<(), Error> {
        let iommu = self.iommu();
        let stream = iommu.iort.stream_of(device).ok_or(Error::NotSupported)?;
        if iommu.unit.is_translated(stream) {
            return Err(Error::Busy);
        }
        let entry = ste_stage2(self.root, self.vmid, iommu.unit.capabilities.output_size);
        iommu.unit.set_stream(stream, &entry).map_err(|e| {
            crate::kwarn!("iommu: unit at {:#x}: {}", iommu.address, e);
            Error::Internal
        })?;
        iommu.unit.set_translated(stream, true);
        self.streams.push((device, stream));
        super::joined_guest(iommu, stream);
        crate::kinfo!("iommu: {} is in domain {} now (stream {:#x})", bdf(device), self.vmid, stream);
        Ok(())
    }
}

impl Drop for Domain {
    fn drop(&mut self) {
        let iommu = self.iommu();
        // The devices reach nothing from now on, and the SMMU forgets the
        // domain's translations: then its tables can go. (If it could not be
        // told, they stay, lest a device walk freed memory.)
        let mut forgotten = true;
        for &(device, stream) in &self.streams {
            match iommu.unit.set_stream(stream, &ste_abort()) {
                Ok(()) => {
                    iommu.unit.set_translated(stream, false);
                    super::left_guest(iommu, stream);
                    crate::kinfo!("iommu: {} left domain {}: it reaches nothing", bdf(device), self.vmid);
                }
                Err(e) => {
                    crate::kwarn!("iommu: unit at {:#x}: {}; domain {} is kept", iommu.address, e, self.vmid);
                    forgotten = false;
                }
            }
        }
        if !forgotten || iommu.unit.command(&[command::forget_vmid(self.vmid)]).is_err() {
            return;
        }
        let mut tables: Vec<(u64, u32)> = alloc::vec![(self.root, 3)];
        while let Some((t, level)) = tables.pop() {
            if level > 0 {
                tables.extend(table(t).iter().filter(|&&e| e & VALID != 0).map(|&e| (e & ADDRESS, level - 1)));
            }
            phys::free(t);
        }
        iommu.vmids.lock().free.push(self.vmid);
    }
}
