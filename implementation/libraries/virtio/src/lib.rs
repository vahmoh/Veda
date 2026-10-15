// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! virtio 1.x over PCI ("modern" interface) for user-space drivers.
//!
//! * [`Device`] locates the virtio structures through the `pcidev`
//!   protocol, maps the BARs they are in, performs the status/feature
//!   handshake and sets up MSI-X vectors.
//! * [`Virtqueue`] is a split virtqueue living in physically contiguous DMA
//!   memory (`vproto::pci::DmaBuffer`, which drivers use for their payloads
//!   too).
//!
//! Where the device says its structures are, and how long they are (its
//! capabilities, its queues' notification offsets, its MSI-X table), is
//! checked against the BARs they are in before anything is read or written
//! there: a device, or a guest that plays one, steers no access outside
//! them, and one that describes itself wrongly is refused (`BadLayout`)
//! rather than crashing its driver. So is every read of its device-specific
//! configuration.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

use alloc::vec::Vec;
use core::sync::atomic::{Ordering, fence};

use vproto::pci::{DmaBuffer, MSIX_ENTRY, MapError, Registers, cap, capabilities, pcidev};
use vrt::object::{Interrupt, Resource};
use vrt::time::Duration;

/// Device status bits.
pub mod status {
    pub const ACKNOWLEDGE: u8 = 1;
    pub const DRIVER: u8 = 2;
    pub const DRIVER_OK: u8 = 4;
    pub const FEATURES_OK: u8 = 8;
    pub const FAILED: u8 = 128;
}

/// Feature bit every virtio 1.x driver negotiates.
pub const F_VERSION_1: u64 = 1 << 32;
/// The device's memory accesses go through the platform's IOMMU: taken
/// whenever offered (a device that offers it may refuse a driver that does
/// not), since the addresses Veda gives devices are what its IOMMU
/// translates.
pub const F_ACCESS_PLATFORM: u64 = 1 << 33;

/// The registers of the common configuration structure (virtio 1.2,
/// 4.1.4.3), by offset.
mod common {
    pub const DEVICE_FEATURE_SELECT: usize = 0;
    pub const DEVICE_FEATURE: usize = 4;
    pub const DRIVER_FEATURE_SELECT: usize = 8;
    pub const DRIVER_FEATURE: usize = 12;
    pub const CONFIG_MSIX_VECTOR: usize = 16;
    pub const DEVICE_STATUS: usize = 20;
    pub const QUEUE_SELECT: usize = 22;
    pub const QUEUE_SIZE: usize = 24;
    pub const QUEUE_MSIX_VECTOR: usize = 26;
    pub const QUEUE_ENABLE: usize = 28;
    pub const QUEUE_NOTIFY_OFF: usize = 30;
    pub const QUEUE_DESC: usize = 32;
    pub const QUEUE_DRIVER: usize = 40;
    pub const QUEUE_DEVICE: usize = 48;
    /// Its bytes up to the end of the last register used.
    pub const LEN: usize = 56;
}

/// The kinds of structure a virtio capability describes (its `cfg_type`).
mod kind {
    pub const COMMON: u8 = 1;
    pub const NOTIFY: u8 = 2;
    pub const ISR: u8 = 3;
    pub const DEVICE: u8 = 4;
}

/// Bytes of a virtio capability (`struct virtio_pci_cap`), and of the
/// notification structure's, which adds its multiplier.
const CAP_LEN: u16 = 16;
const NOTIFY_CAP_LEN: u16 = 20;
/// The highest BAR number.
const LAST_BAR: u8 = 5;
/// Bytes of configuration space capabilities are in.
const CONFIG_SPACE: u16 = 256;

/// How long a device may take to reset.
const RESET_NS: u64 = 1_000_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtioError {
    /// Talking to devmgr failed.
    Ipc,
    /// A required virtio capability is missing.
    MissingCapability,
    /// A structure the device describes is not inside its BAR, or is too
    /// short or misaligned for what is in it.
    BadLayout,
    /// The device does not finish resetting.
    NoReset,
    /// The device rejected our feature set.
    FeaturesRejected,
    /// The requested queue does not exist or is too small.
    BadQueue,
    /// Out of memory or mapping failed.
    NoMemory,
    /// No MSI-X support.
    NoMsix,
    /// The device-specific configuration has no such field (it has none,
    /// or a shorter one).
    NoSuchField,
}

impl core::fmt::Display for VirtioError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            VirtioError::Ipc => "the device manager does not answer",
            VirtioError::MissingCapability => "a virtio structure is missing",
            VirtioError::BadLayout => "a structure is not inside its BAR",
            VirtioError::NoReset => "the device does not reset",
            VirtioError::FeaturesRejected => "the device rejects the features",
            VirtioError::BadQueue => "no such queue",
            VirtioError::NoMemory => "out of memory",
            VirtioError::NoMsix => "no MSI-X vector",
            VirtioError::NoSuchField => "no such configuration field",
        })
    }
}

/// A descriptor of a split virtqueue (virtio 1.2, 2.7.5): its buffer's
/// address (8 bytes), length (4), flags (2) and next descriptor (2).
const DESC: usize = 16;
const DESC_F_NEXT: u16 = 1;
const DESC_F_WRITE: u16 = 2;

/// One element of a descriptor chain: (physical address, length, device
/// writes it).
#[derive(Debug, Clone, Copy)]
pub struct Segment {
    pub phys: u64,
    pub len: u32,
    pub device_writes: bool,
}

/// A split virtqueue.
pub struct Virtqueue {
    pub index: u16,
    size: u16,
    mem: DmaBuffer,
    avail_off: usize,
    used_off: usize,
    free: Vec<u16>,
    /// Length of the chain starting at each head (0: not an outstanding
    /// head).
    chain_len: Vec<u16>,
    /// Our own copy of each descriptor's `next` link, so freeing a chain
    /// never trusts memory the device can write.
    next: Vec<u16>,
    last_used: u16,
    /// Where its notification register is in the device's notification
    /// structure.
    notify: usize,
}

impl Virtqueue {
    fn new(index: u16, size: u16, dma: &Resource, notify: usize) -> Result<Virtqueue, VirtioError> {
        let desc_bytes = DESC * size as usize;
        let avail_off = desc_bytes;
        let avail_bytes = 6 + 2 * size as usize;
        let used_off = (avail_off + avail_bytes).next_multiple_of(4096);
        let used_bytes = 6 + 8 * size as usize;
        let mem = DmaBuffer::new(dma, used_off + used_bytes).map_err(|_| VirtioError::NoMemory)?;
        Ok(Virtqueue {
            index,
            size,
            mem,
            avail_off,
            used_off,
            free: (0..size).rev().collect(),
            chain_len: alloc::vec![0; size as usize],
            next: alloc::vec![0; size as usize],
            last_used: 0,
            notify,
        })
    }

    pub fn size(&self) -> u16 {
        self.size
    }

    /// Number of free descriptors.
    pub fn free_descriptors(&self) -> usize {
        self.free.len()
    }

    /// Posts a descriptor chain. Returns its head index (the token reported
    /// back by [`Virtqueue::pop_used`]).
    pub fn push(&mut self, chain: &[Segment]) -> Option<u16> {
        if chain.is_empty() || chain.len() > self.free.len() {
            return None;
        }
        let mut ids = self.free.split_off(self.free.len() - chain.len());
        ids.reverse();
        match self.post(chain, &ids) {
            Ok(head) => Some(head),
            Err(_) => {
                // The device never saw them.
                self.free.extend(ids.iter().rev());
                None
            }
        }
    }

    /// Writes `chain` into descriptors `ids`, and makes its head available
    /// to the device.
    fn post(&mut self, chain: &[Segment], ids: &[u16]) -> Result<u16, vabi::Error> {
        let head = *ids.first().ok_or(vabi::Error::InvalidArgs)?;
        for (i, (seg, &id)) in chain.iter().zip(ids).enumerate() {
            let mut flags = if seg.device_writes { DESC_F_WRITE } else { 0 };
            let next = match ids.get(i + 1) {
                Some(&next) => {
                    flags |= DESC_F_NEXT;
                    next
                }
                None => 0,
            };
            self.next[id as usize] = next;
            let at = DESC * id as usize;
            self.mem.write64(at, seg.phys)?;
            self.mem.write32(at + 8, seg.len)?;
            self.mem.write16(at + 12, flags)?;
            self.mem.write16(at + 14, next)?;
        }
        // The available ring's index, then its entries; the ring is the
        // driver's.
        let idx = self.mem.read16(self.avail_off + 2)?;
        self.mem.write16(self.avail_off + 4 + 2 * (idx % self.size) as usize, head)?;
        fence(Ordering::SeqCst);
        self.mem.write16(self.avail_off + 2, idx.wrapping_add(1))?;
        self.chain_len[head as usize] = chain.len() as u16;
        Ok(head)
    }

    /// Takes the next completed chain: (head, bytes written by the device).
    /// The chain's descriptors are returned to the free list. Entries naming
    /// a descriptor that is not the head of an outstanding chain (a device
    /// bug) are skipped.
    pub fn pop_used(&mut self) -> Option<(u16, u32)> {
        loop {
            fence(Ordering::SeqCst);
            if self.last_used == self.mem.read16(self.used_off + 2).ok()? {
                return None;
            }
            // The entry is read after the index that announced it (virtio
            // 1.2, 2.7.13: the driver's read barrier).
            fence(Ordering::Acquire);
            let at = self.used_off + 4 + 8 * (self.last_used % self.size) as usize;
            let (id, len) = (self.mem.read32(at).ok()?, self.mem.read32(at + 4).ok()?);
            self.last_used = self.last_used.wrapping_add(1);
            if id >= self.size as u32 || self.chain_len[id as usize] == 0 {
                continue;
            }
            return Some(self.release(id as u16, len));
        }
    }

    fn release(&mut self, head: u16, len: u32) -> (u16, u32) {
        let count = core::mem::take(&mut self.chain_len[head as usize]);
        let mut cur = head;
        for i in 0..count {
            self.free.push(cur);
            if i + 1 < count {
                cur = self.next[cur as usize];
            }
        }
        (head, len)
    }
}

/// A queue's size: the largest power of two (as split virtqueues' sizes
/// are) that neither the device's largest (`device`; 0: no such queue) nor
/// `wanted` is below.
fn queue_size(device: u16, wanted: u16) -> Option<u16> {
    let most = device.min(wanted);
    (most > 0).then(|| 1 << most.ilog2())
}

/// Where a queue's notification register is in the notification structure
/// (`len` bytes long): its notification offset times the device's
/// multiplier, with room for the 16-bit register there, aligned (virtio
/// 1.2, 4.1.4.4).
fn notify_offset(queue_notify_off: u16, multiplier: u32, len: usize) -> Result<usize, VirtioError> {
    let at = usize::try_from(queue_notify_off as u64 * multiplier as u64).map_err(|_| VirtioError::BadLayout)?;
    match at.checked_add(2) {
        Some(end) if end <= len && at.is_multiple_of(2) => Ok(at),
        _ => Err(VirtioError::BadLayout),
    }
}

/// The structure a capability says is `length` bytes from `offset` on in a
/// BAR of `bar_len` bytes, if it lies inside it whole, is at least `least`
/// bytes long and starts aligned to `align`: its offset and length.
fn place(offset: u32, length: u32, bar_len: usize, least: usize, align: usize) -> Result<(usize, usize), VirtioError> {
    let (offset, len) = (offset as usize, length as usize);
    let inside = offset.checked_add(len).is_some_and(|end| end <= bar_len);
    if inside && len >= least && offset.is_multiple_of(align) { Ok((offset, len)) } else { Err(VirtioError::BadLayout) }
}

/// Where a structure is: `len` bytes from `offset` on in the BAR at `bar`
/// among the device's mapped ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Window {
    bar: usize,
    offset: usize,
    len: usize,
}

/// What a virtio capability says of the structure it describes: the BAR
/// it is in, where in it, how long, and (the notification structure's) the
/// multiplier of its queues' notification offsets.
#[derive(Debug, Clone, Copy)]
struct Described {
    bar: u8,
    offset: u32,
    length: u32,
    multiplier: u32,
}

/// The MSI-X capability (where it is in configuration space), the table it
/// describes, and how many entries that has.
#[derive(Debug, Clone, Copy)]
struct Msix {
    cap: u16,
    table: Window,
    vectors: u16,
}

/// A virtio PCI device.
pub struct Device {
    pub pci: pcidev::Client,
    /// The BARs its structures are in, mapped, with their numbers.
    bars: Vec<(u8, Registers)>,
    common: Window,
    notify: Window,
    notify_multiplier: u32,
    isr: Window,
    device_cfg: Option<Window>,
    dma: Resource,
    msix: Option<Msix>,
    vectors_used: u16,
}

fn pci_read(pci: &pcidev::Client, off: u16, width: u8) -> Result<u32, VirtioError> {
    match pci.config_read(off, width) {
        Ok(Ok(v)) => Ok(v),
        _ => Err(VirtioError::Ipc),
    }
}

/// The kind of structure the virtio capability at `at` describes, and
/// what it says of it; `None` if it is not one this driver can use (it is
/// shorter than a virtio capability is, runs past configuration space, or
/// names no BAR).
fn described(pci: &pcidev::Client, at: u16) -> Result<Option<(u8, Described)>, VirtioError> {
    let len = pci_read(pci, at + 2, 1)? as u16;
    let cfg_type = pci_read(pci, at + 3, 1)? as u8;
    let bar = pci_read(pci, at + 4, 1)? as u8;
    let least = if cfg_type == kind::NOTIFY { NOTIFY_CAP_LEN } else { CAP_LEN };
    if len < least || at + least > CONFIG_SPACE || bar > LAST_BAR {
        return Ok(None);
    }
    let multiplier = if cfg_type == kind::NOTIFY { pci_read(pci, at + 16, 4)? } else { 0 };
    let (offset, length) = (pci_read(pci, at + 8, 4)?, pci_read(pci, at + 12, 4)?);
    Ok(Some((cfg_type, Described { bar, offset, length, multiplier })))
}

/// The index among `bars` of BAR `bar`, mapped now if it was not yet.
fn mapped(pci: &pcidev::Client, bars: &mut Vec<(u8, Registers)>, bar: u8) -> Result<usize, VirtioError> {
    if let Some(i) = bars.iter().position(|(b, _)| *b == bar) {
        return Ok(i);
    }
    let registers = Registers::map(pci, bar).map_err(|e| match e {
        MapError::Ipc(_) => VirtioError::Ipc,
        MapError::Pci(_) => VirtioError::MissingCapability,
        MapError::Kernel(_) => VirtioError::NoMemory,
    })?;
    bars.push((bar, registers));
    Ok(bars.len() - 1)
}

/// The window of the structure `d` describes, its BAR mapped: inside the
/// BAR, at least `least` bytes long, aligned to `align`.
fn window(
    pci: &pcidev::Client,
    bars: &mut Vec<(u8, Registers)>,
    d: Described,
    least: usize,
    align: usize,
) -> Result<Window, VirtioError> {
    let bar = mapped(pci, bars, d.bar)?;
    let bar_len = bars.get(bar).map_or(0, |(_, r)| r.len());
    let (offset, len) = place(d.offset, d.length, bar_len, least, align)?;
    Ok(Window { bar, offset, len })
}

/// The MSI-X table the capability at `at` describes, inside the BAR it
/// names whole.
fn msix(pci: &pcidev::Client, bars: &mut Vec<(u8, Registers)>, at: u16) -> Result<Msix, VirtioError> {
    let control = pci_read(pci, at + 2, 2)?;
    let table = pci_read(pci, at + 4, 4)?;
    let vectors = (control & 0x7FF) as u16 + 1;
    let length = vectors as u32 * MSIX_ENTRY as u32;
    let d = Described { bar: (table & 7) as u8, offset: table & !7, length, multiplier: 0 };
    Ok(Msix { cap: at, table: window(pci, bars, d, MSIX_ENTRY, 8)?, vectors })
}

impl Device {
    /// Locates the structures, maps the BARs they are in and enables the
    /// device.
    pub fn new(pci: pcidev::Client) -> Result<Device, VirtioError> {
        match pci.enable(true) {
            Ok(Ok(())) => {}
            _ => return Err(VirtioError::Ipc),
        }
        let dma = match pci.dma_resource() {
            Ok(Ok(r)) => r,
            _ => return Err(VirtioError::Ipc),
        };
        // The first of each structure the device describes, and its MSI-X
        // capability.
        let (mut common_cfg, mut notify_cfg, mut isr_cfg, mut device_cfg, mut msix_cap) =
            (None, None, None, None, None);
        for c in capabilities(&pci) {
            if c.id == cap::MSI_X {
                msix_cap = msix_cap.or(Some(c.offset));
                continue;
            }
            if c.id != cap::VENDOR {
                continue;
            }
            let Some((cfg_type, d)) = described(&pci, c.offset)? else { continue };
            let first = match cfg_type {
                kind::COMMON => &mut common_cfg,
                kind::NOTIFY => &mut notify_cfg,
                kind::ISR => &mut isr_cfg,
                kind::DEVICE => &mut device_cfg,
                _ => continue,
            };
            first.get_or_insert(d);
        }
        let (Some(common_cfg), Some(notify_cfg), Some(isr_cfg)) = (common_cfg, notify_cfg, isr_cfg) else {
            return Err(VirtioError::MissingCapability);
        };
        let mut bars = Vec::new();
        let common = window(&pci, &mut bars, common_cfg, common::LEN, 4)?;
        let notify = window(&pci, &mut bars, notify_cfg, 2, 2)?;
        let isr = window(&pci, &mut bars, isr_cfg, 1, 1)?;
        let device_cfg = device_cfg.map(|d| window(&pci, &mut bars, d, 0, 4)).transpose()?;
        let msix = msix_cap.map(|at| msix(&pci, &mut bars, at)).transpose()?;
        Ok(Device {
            pci,
            bars,
            common,
            notify,
            notify_multiplier: notify_cfg.multiplier,
            isr,
            device_cfg,
            dma,
            msix,
            vectors_used: 0,
        })
    }

    /// The registers of the `width` bytes at `offset` of window `w`, if
    /// they are inside it: its BAR's, and where in the BAR they are.
    fn at(&self, w: Window, offset: usize, width: usize) -> Option<(&Registers, usize)> {
        if offset.checked_add(width)? > w.len {
            return None;
        }
        self.bars.get(w.bar).map(|(_, r)| (r, w.offset + offset))
    }

    fn read8(&self, w: Window, offset: usize) -> u8 {
        self.at(w, offset, 1).map_or(u8::MAX, |(r, at)| r.read8(at))
    }

    fn write8(&self, w: Window, offset: usize, v: u8) {
        if let Some((r, at)) = self.at(w, offset, 1) {
            r.write8(at, v)
        }
    }

    fn read16(&self, w: Window, offset: usize) -> u16 {
        self.at(w, offset, 2).map_or(u16::MAX, |(r, at)| r.read16(at))
    }

    fn write16(&self, w: Window, offset: usize, v: u16) {
        if let Some((r, at)) = self.at(w, offset, 2) {
            r.write16(at, v)
        }
    }

    fn read32(&self, w: Window, offset: usize) -> u32 {
        self.at(w, offset, 4).map_or(u32::MAX, |(r, at)| r.read32(at))
    }

    fn write32(&self, w: Window, offset: usize, v: u32) {
        if let Some((r, at)) = self.at(w, offset, 4) {
            r.write32(at, v)
        }
    }

    fn write64(&self, w: Window, offset: usize, v: u64) {
        self.write32(w, offset, v as u32);
        self.write32(w, offset + 4, (v >> 32) as u32);
    }

    pub fn status(&self) -> u8 {
        self.read8(self.common, common::DEVICE_STATUS)
    }

    pub fn set_status(&self, s: u8) {
        self.write8(self.common, common::DEVICE_STATUS, s);
    }

    /// Resets the device: it lets go of its queues, and forgets its
    /// features and status. Fails if it has not within a second.
    pub fn reset(&self) -> Result<(), VirtioError> {
        self.set_status(0);
        let end = vrt::time::now_ns() + RESET_NS;
        let mut spins = 0u32;
        while self.status() != 0 {
            if vrt::time::now_ns() > end {
                return Err(VirtioError::NoReset);
            }
            spins += 1;
            if spins > 100 {
                vrt::time::sleep(Duration::from_millis(1));
            } else {
                core::hint::spin_loop();
            }
        }
        Ok(())
    }

    /// Resets the device and performs the feature handshake, accepting the
    /// intersection of the device's features and `wanted` (plus VERSION_1
    /// and ACCESS_PLATFORM).
    pub fn initialize(&self, wanted: u64) -> Result<u64, VirtioError> {
        // A device whose status reads 0 is in its reset state already (the
        // firmware resets its devices when it hands over). Resetting it
        // again is not only redundant: QEMU's 3D virtio-gpu resets on its
        // main loop and holds the vCPU that asked until it has, which now
        // and then deadlocks the whole machine.
        if self.status() != 0 {
            self.reset()?;
        }
        self.set_status(status::ACKNOWLEDGE);
        self.set_status(status::ACKNOWLEDGE | status::DRIVER);
        let c = self.common;
        self.write32(c, common::DEVICE_FEATURE_SELECT, 0);
        let lo = self.read32(c, common::DEVICE_FEATURE) as u64;
        self.write32(c, common::DEVICE_FEATURE_SELECT, 1);
        let hi = self.read32(c, common::DEVICE_FEATURE) as u64;
        let offered = lo | hi << 32;
        let accepted = offered & (wanted | F_VERSION_1 | F_ACCESS_PLATFORM);
        if accepted & F_VERSION_1 == 0 {
            self.set_status(status::FAILED);
            return Err(VirtioError::FeaturesRejected);
        }
        self.write32(c, common::DRIVER_FEATURE_SELECT, 0);
        self.write32(c, common::DRIVER_FEATURE, accepted as u32);
        self.write32(c, common::DRIVER_FEATURE_SELECT, 1);
        self.write32(c, common::DRIVER_FEATURE, (accepted >> 32) as u32);
        self.set_status(status::ACKNOWLEDGE | status::DRIVER | status::FEATURES_OK);
        if self.status() & status::FEATURES_OK == 0 {
            self.set_status(status::FAILED);
            return Err(VirtioError::FeaturesRejected);
        }
        Ok(accepted)
    }

    /// Creates queue `index` with up to `max_size` entries.
    pub fn setup_queue(&self, index: u16, max_size: u16) -> Result<Virtqueue, VirtioError> {
        let c = self.common;
        self.write16(c, common::QUEUE_SELECT, index);
        let size = queue_size(self.read16(c, common::QUEUE_SIZE), max_size).ok_or(VirtioError::BadQueue)?;
        self.write16(c, common::QUEUE_SIZE, size);
        let notify = notify_offset(self.read16(c, common::QUEUE_NOTIFY_OFF), self.notify_multiplier, self.notify.len)?;
        let q = Virtqueue::new(index, size, &self.dma, notify)?;
        let base = q.mem.phys();
        self.write64(c, common::QUEUE_DESC, base);
        self.write64(c, common::QUEUE_DRIVER, base + q.avail_off as u64);
        self.write64(c, common::QUEUE_DEVICE, base + q.used_off as u64);
        self.write16(c, common::QUEUE_ENABLE, 1);
        Ok(q)
    }

    /// Tells the device `queue` has new buffers.
    pub fn notify(&self, queue: &Virtqueue) {
        fence(Ordering::SeqCst);
        self.write16(self.notify, queue.notify, queue.index);
    }

    /// Allocates an MSI-X vector, binds it to `queue` (or to configuration
    /// changes when `None`), and returns the interrupt to wait on.
    pub fn msix_vector(&mut self, queue: Option<u16>) -> Result<Interrupt, VirtioError> {
        let msix = self.msix.ok_or(VirtioError::NoMsix)?;
        if self.vectors_used >= msix.vectors {
            return Err(VirtioError::NoMsix);
        }
        let (irq, msi) = match self.pci.alloc_msi() {
            Ok(Ok(v)) => v,
            _ => return Err(VirtioError::NoMsix),
        };
        let v = self.vectors_used;
        self.vectors_used += 1;
        // Entry `v` of the table, which lies inside its BAR whole.
        let entry = v as usize * MSIX_ENTRY;
        self.write32(msix.table, entry, msi.address as u32);
        self.write32(msix.table, entry + 4, (msi.address >> 32) as u32);
        self.write32(msix.table, entry + 8, msi.data);
        // Unmasked.
        self.write32(msix.table, entry + 12, 0);
        // Enable MSI-X (clear the function mask).
        let control = pci_read(&self.pci, msix.cap + 2, 2)?;
        let _ = self.pci.config_write(msix.cap + 2, 2, (control | 0x8000) & !0x4000);
        let c = self.common;
        match queue {
            Some(q) => {
                self.write16(c, common::QUEUE_SELECT, q);
                self.write16(c, common::QUEUE_MSIX_VECTOR, v);
                if self.read16(c, common::QUEUE_MSIX_VECTOR) != v {
                    return Err(VirtioError::NoMsix);
                }
            }
            None => self.write16(c, common::CONFIG_MSIX_VECTOR, v),
        }
        Ok(irq)
    }

    /// Final step of initialisation.
    pub fn driver_ok(&self) {
        self.set_status(status::ACKNOWLEDGE | status::DRIVER | status::FEATURES_OK | status::DRIVER_OK);
    }

    /// Reads and clears the ISR status (legacy interrupt acknowledgement).
    pub fn isr(&self) -> u8 {
        self.read8(self.isr, 0)
    }

    pub fn dma(&self) -> &Resource {
        &self.dma
    }

    /// The registers of the device-specific configuration's field of
    /// `width` bytes at `offset`, if it has one there, aligned as the
    /// specification has fields read (virtio 1.2, 4.1.3.1: one of up to 32
    /// bits to its width, a wider one to 32 bits).
    fn field(&self, offset: u32, width: usize) -> Result<(&Registers, usize), VirtioError> {
        let w = self.device_cfg.ok_or(VirtioError::NoSuchField)?;
        let offset = offset as usize;
        if !offset.is_multiple_of(width.min(4)) {
            return Err(VirtioError::NoSuchField);
        }
        self.at(w, offset, width).ok_or(VirtioError::NoSuchField)
    }

    /// The 8-bit field at `offset` of the device-specific configuration.
    pub fn cfg_read8(&self, offset: u32) -> Result<u8, VirtioError> {
        self.field(offset, 1).map(|(r, at)| r.read8(at))
    }

    /// Writes the 8-bit field at `offset` of the device-specific
    /// configuration.
    pub fn cfg_write8(&self, offset: u32, v: u8) -> Result<(), VirtioError> {
        self.field(offset, 1).map(|(r, at)| r.write8(at, v))
    }

    /// The 16-bit field at `offset` of the device-specific configuration.
    pub fn cfg_read16(&self, offset: u32) -> Result<u16, VirtioError> {
        self.field(offset, 2).map(|(r, at)| r.read16(at))
    }

    /// The 32-bit field at `offset` of the device-specific configuration.
    pub fn cfg_read32(&self, offset: u32) -> Result<u32, VirtioError> {
        self.field(offset, 4).map(|(r, at)| r.read32(at))
    }

    /// The 64-bit field at `offset` of the device-specific configuration,
    /// read as two 32-bit halves, the low one first.
    pub fn cfg_read64(&self, offset: u32) -> Result<u64, VirtioError> {
        self.field(offset, 8).map(|(r, at)| r.read32(at) as u64 | (r.read32(at + 4) as u64) << 32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structures_lie_inside_their_bars() {
        assert_eq!(place(0x1000, 56, 0x4000, common::LEN, 4), Ok((0x1000, 56)));
        assert_eq!(place(0x3FC8, 56, 0x4000, common::LEN, 4), Ok((0x3FC8, 56)));
        // Past the BAR's end, however far.
        assert_eq!(place(0x3FCC, 56, 0x4000, common::LEN, 4), Err(VirtioError::BadLayout));
        assert_eq!(place(u32::MAX, u32::MAX, 0x4000, 0, 1), Err(VirtioError::BadLayout));
        // Too short for the registers used, or misaligned for them.
        assert_eq!(place(0x1000, 52, 0x4000, common::LEN, 4), Err(VirtioError::BadLayout));
        assert_eq!(place(0x1002, 56, 0x4000, common::LEN, 4), Err(VirtioError::BadLayout));
    }

    #[test]
    fn queue_notifications_lie_inside_their_structure() {
        assert_eq!(notify_offset(3, 4, 0x1000), Ok(12));
        assert_eq!(notify_offset(0x3FF, 4, 0x1000), Ok(0xFFC));
        assert_eq!(notify_offset(0x400, 4, 0x1000), Err(VirtioError::BadLayout));
        assert_eq!(notify_offset(u16::MAX, u32::MAX, 0x1000), Err(VirtioError::BadLayout));
        // A 16-bit register is aligned.
        assert_eq!(notify_offset(1, 1, 0x1000), Err(VirtioError::BadLayout));
        // Every queue's at the start, with a multiplier of 0.
        assert_eq!(notify_offset(7, 0, 2), Ok(0));
        assert_eq!(notify_offset(0, 4, 1), Err(VirtioError::BadLayout));
    }

    #[test]
    fn queue_sizes_are_powers_of_two_neither_side_exceeds() {
        assert_eq!(queue_size(256, 16), Some(16));
        assert_eq!(queue_size(12, 16), Some(8));
        assert_eq!(queue_size(1, 16), Some(1));
        assert_eq!(queue_size(u16::MAX, u16::MAX), Some(32768));
        assert_eq!(queue_size(0, 16), None);
        assert_eq!(queue_size(16, 0), None);
    }
}
