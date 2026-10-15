// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `virtio-blk` — the driver for virtio block devices (disks).
//!
//! Each disk is served as the `block` protocol under the name
//! `block/<serial>` (`vproto::block::serve`), so that the file system finds
//! a particular disk, such as the user's home disk, by its serial number.
//! Requests are processed one at a time: the only client, the file system,
//! issues one request at a time anyway. A request the device does not
//! finish within 30 seconds fails, and the device is reset and given up: it
//! might otherwise still write into memory later requests use.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use vproto::block::{self, BlockError, BlockInfo, MAX_TRANSFER, Served};
use vproto::pci::{DmaBuffer, pcidev};
use vrt::object::{Channel, Interrupt};
use vrt::println;
use vrt::time::Duration;
use vvirtio::{Device, Segment, Virtqueue};

vrt::entry!(main);

const SECTOR: usize = 512;
const QUEUE_SIZE: u16 = 16;

// Feature bits (virtio 1.2, section 5.2.3).
const F_RO: u64 = 1 << 5;
const F_FLUSH: u64 = 1 << 9;

// Request types.
const T_IN: u32 = 0;
const T_OUT: u32 = 1;
const T_FLUSH: u32 = 4;
const T_GET_ID: u32 = 8;
const S_OK: u8 = 0;

/// Layout of the DMA buffer: request header, status byte, data.
const HEADER: usize = 0;
const STATUS: usize = 16;
const DATA: usize = 64;
/// Bytes of the serial number `T_GET_ID` gives.
const ID_LEN: usize = 20;

/// How long to wait for an interrupt before looking at the queue again.
const POLL_NS: u64 = 50_000_000;
/// How long a request may take.
const REQUEST_TIMEOUT_NS: u64 = 30_000_000_000;

struct Disk {
    dev: Device,
    queue: Virtqueue,
    irq: Option<Interrupt>,
    buf: DmaBuffer,
    can_flush: bool,
    /// A request did not finish: the device has been reset, and is used no
    /// more.
    failed: bool,
}

impl Disk {
    /// Submits one request and waits for it. `len` bytes at `DATA` are sent
    /// to the device, or received if `device_writes`.
    fn request(&mut self, kind: u32, sector: u64, len: usize, device_writes: bool) -> Result<(), BlockError> {
        if self.failed {
            return Err(BlockError::Io);
        }
        let mut header = [0u8; 16];
        header[0..4].copy_from_slice(&kind.to_le_bytes());
        header[8..16].copy_from_slice(&sector.to_le_bytes());
        self.buf.write(HEADER, &header).map_err(|_| BlockError::Invalid)?;
        self.buf.write(STATUS, &[0xFF]).map_err(|_| BlockError::Invalid)?;
        let base = self.buf.phys();
        let head = Segment { phys: base + HEADER as u64, len: 16, device_writes: false };
        let data = Segment { phys: base + DATA as u64, len: len as u32, device_writes };
        let status = Segment { phys: base + STATUS as u64, len: 1, device_writes: true };
        let pushed = if len > 0 { self.queue.push(&[head, data, status]) } else { self.queue.push(&[head, status]) };
        pushed.ok_or(BlockError::Io)?;
        self.dev.notify(&self.queue);
        let end = vrt::time::now_ns() + REQUEST_TIMEOUT_NS;
        let mut spins = 0u32;
        while self.queue.pop_used().is_none() {
            if vrt::time::now_ns() > end {
                // The request is still the device's, which may yet write
                // into the buffer: a reset takes it back.
                let reset = self.dev.reset();
                println!(
                    "the disk did not finish a request in {} s: {}",
                    REQUEST_TIMEOUT_NS / 1_000_000_000,
                    if reset.is_ok() { "it is reset and given up" } else { "given up (it does not reset either)" }
                );
                self.failed = true;
                return Err(BlockError::Io);
            }
            spins += 1;
            match &self.irq {
                // Interrupts are edge-like: re-arm, then look at the queue.
                Some(irq) => {
                    let _ = irq.wait_irq((vrt::time::now_ns() + POLL_NS).min(end));
                    let _ = irq.ack();
                }
                None if spins > 1000 => vrt::time::sleep(Duration::from_millis(1)),
                None => core::hint::spin_loop(),
            }
        }
        let mut st = [0u8];
        self.buf.read(STATUS, &mut st).map_err(|_| BlockError::Io)?;
        if st[0] == S_OK { Ok(()) } else { Err(BlockError::Io) }
    }
}

/// The disk is the driver's only one: `disk` is 0.
impl block::Disks for Disk {
    fn read(&mut self, _disk: usize, lba: u64, count: u32) -> Result<Vec<u8>, BlockError> {
        let len = count as usize * SECTOR;
        self.request(T_IN, lba, len, true)?;
        self.buf.to_vec(DATA, len).map_err(|_| BlockError::Invalid)
    }

    fn write(&mut self, _disk: usize, lba: u64, data: &[u8]) -> Result<(), BlockError> {
        self.buf.write(DATA, data).map_err(|_| BlockError::Invalid)?;
        self.request(T_OUT, lba, data.len(), false)
    }

    fn flush(&mut self, _disk: usize) -> Result<(), BlockError> {
        if self.can_flush { self.request(T_FLUSH, 0, 0, false) } else { Ok(()) }
    }
}

/// Reads the disk's serial number (up to 20 bytes, NUL padded).
fn serial_number(disk: &mut Disk) -> String {
    if disk.request(T_GET_ID, 0, ID_LEN, true).is_err() {
        return String::new();
    }
    disk.buf.to_vec(DATA, ID_LEN).map(|id| block::device_text(&id)).unwrap_or_default()
}

fn main() -> i32 {
    let Some(h) = vrt::env::take_handle(vproto::startup::driver::PCIDEV) else {
        println!("no pcidev channel");
        return 1;
    };
    let pci = pcidev::Client::new(Channel::from_handle(h));
    let location = pci.info().map(|i| format!("{:02x}:{:02x}.{}", i.bus, i.slot, i.function)).unwrap_or_default();
    let mut dev = match Device::new(pci) {
        Ok(d) => d,
        Err(e) => {
            println!("device setup failed: {}", e);
            return 1;
        }
    };
    let features = match dev.initialize(F_RO | F_FLUSH) {
        Ok(f) => f,
        Err(e) => {
            println!("feature negotiation failed: {}", e);
            return 1;
        }
    };
    // The capacity, in 512-byte sectors.
    let sectors = match dev.cfg_read64(0) {
        Ok(sectors) => sectors,
        Err(e) => {
            println!("no capacity: {}", e);
            return 1;
        }
    };
    let queue = match dev.setup_queue(0, QUEUE_SIZE) {
        Ok(q) => q,
        Err(e) => {
            println!("no request queue: {}", e);
            return 1;
        }
    };
    let Ok(buf) = DmaBuffer::new(dev.dma(), DATA + MAX_TRANSFER as usize) else {
        println!("out of DMA memory");
        return 1;
    };
    let irq = dev.msix_vector(Some(0)).ok();
    dev.driver_ok();
    let read_only = features & F_RO != 0;
    let mut disk = Disk { dev, queue, irq, buf, can_flush: features & F_FLUSH != 0, failed: false };
    let serial = serial_number(&mut disk);
    if disk.failed {
        return 1;
    }
    let info = BlockInfo { sectors, sector_size: SECTOR as u32, read_only, serial };
    let served = [Served {
        id: block::disk_id(&info.serial, &format!("pci-{location}")),
        description: format!("{}: {} MiB{}", location, info.mib(), if read_only { ", read-only" } else { "" }),
        info,
    }];
    block::serve(&mut disk, &served);
    1
}
