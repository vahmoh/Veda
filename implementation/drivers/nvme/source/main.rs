// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `nvme` — the driver for NVM Express disks: the SSDs of most PCs since
//! about 2016, and QEMU's `nvme`.
//!
//! Every namespace is served as the `block` protocol under `block/<serial>`
//! (`proto::block::serve`; the controller's serial number, to which a
//! namespace other than the first adds `-n<id>`), so the file system finds
//! the home disk by its serial number whatever the disk.
//!
//! The controller is reset and given an admin queue pair and one I/O queue
//! pair, and the driver runs a request at a time (the file system issues
//! one at a time anyway): READ, WRITE and FLUSH, the data in one buffer as
//! large as the largest transfer, which PRP entries describe (a list past
//! two pages); a request larger than the controller takes is split.
//! IDENTIFY gives the serial number and model, the namespaces, and their
//! sizes and block sizes (512 or 4096 bytes; formats with metadata are not
//! used). It waits on an MSI-X (or MSI) interrupt when the controller has
//! one, and polls otherwise. A command that does not finish (or a
//! controller that reports a fatal error) fails, and the controller is
//! reset, which drops every command it has, and started again: a command
//! left to it might otherwise still write into the buffer later ones use.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{Ordering, fence};

use proto::block::{self, BlockError, BlockInfo, MAX_TRANSFER, Served};
use proto::pci::{DmaBuffer, Registers, pcidev};
use runtime::object::{Channel, Interrupt};
use runtime::println;

runtime::entry!(main);

/// The BAR of the controller's registers.
const REGISTERS: u8 = 0;
const PAGE: usize = 4096;

/// Registers (NVM Express 2.0, section 3.1).
mod reg {
    pub const CAP: usize = 0x00;
    pub const CC: usize = 0x14;
    pub const CSTS: usize = 0x1C;
    pub const AQA: usize = 0x24;
    pub const ASQ: usize = 0x28;
    pub const ACQ: usize = 0x30;
    pub const DOORBELLS: usize = 0x1000;
}

const CC_EN: u32 = 1 << 0;
/// 64-byte submission and 16-byte completion queue entries; 4 KiB pages;
/// the NVM command set.
const CC_IO_ENTRIES: u32 = 6 << 16 | 4 << 20;
const CSTS_RDY: u32 = 1 << 0;
const CSTS_CFS: u32 = 1 << 1;

/// Admin commands.
const ADMIN_CREATE_SQ: u8 = 0x01;
const ADMIN_CREATE_CQ: u8 = 0x05;
const ADMIN_IDENTIFY: u8 = 0x06;
const ADMIN_SET_FEATURES: u8 = 0x09;
const FEATURE_QUEUES: u32 = 0x07;
const IDENTIFY_NAMESPACE: u32 = 0;
const IDENTIFY_CONTROLLER: u32 = 1;
const IDENTIFY_NAMESPACES: u32 = 2;
/// NVM commands.
const IO_FLUSH: u8 = 0x00;
const IO_WRITE: u8 = 0x01;
const IO_READ: u8 = 0x02;

/// Entries of each queue.
const ENTRIES: u16 = 32;
/// Layout of the DMA memory: the admin queues, the I/O queues, the
/// identify buffer, the PRP list, the data buffer.
const ADMIN_SQ: usize = 0;
const ADMIN_CQ: usize = PAGE;
const IO_SQ: usize = 2 * PAGE;
const IO_CQ: usize = 3 * PAGE;
const IDENTIFY: usize = 4 * PAGE;
const PRP_LIST: usize = 5 * PAGE;
const DATA: usize = 6 * PAGE;
const MEMORY: usize = DATA + MAX_TRANSFER as usize;

/// How long a command may take.
const COMMAND_TIMEOUT_NS: u64 = 30_000_000_000;

/// A queue pair: where its entries are, the next submission and the next
/// completion, whose phase tag tells a new one.
struct Queue {
    id: u16,
    sq: usize,
    cq: usize,
    tail: u16,
    head: u16,
    phase: bool,
}

impl Queue {
    fn new(id: u16, sq: usize, cq: usize) -> Queue {
        Queue { id, sq, cq, tail: 0, head: 0, phase: true }
    }
}

/// Why a command failed: the controller's status (its type and code), no
/// answer (or a fatal error of the controller's), a queue entry outside the
/// driver's memory, or a controller given up.
enum Failure {
    Status(u16),
    Timeout,
    Memory,
    Gone,
}

impl core::fmt::Display for Failure {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Failure::Status(s) => write!(f, "status type {}, code {:#04x}", s >> 8, s & 0xFF),
            Failure::Timeout => f.write_str("no answer"),
            Failure::Memory => f.write_str("a queue entry outside the driver's memory"),
            Failure::Gone => f.write_str("the controller is given up"),
        }
    }
}

struct Controller {
    regs: Registers,
    /// Its capabilities (CAP).
    cap: u64,
    mem: DmaBuffer,
    irq: Option<Interrupt>,
    /// Bytes between doorbells.
    stride: usize,
    /// The largest transfer of one command, in bytes.
    max_transfer: usize,
    admin: Queue,
    io: Queue,
    next_id: u16,
    /// It did not start again after a command it did not finish: it is
    /// used no more.
    gone: bool,
}

/// A namespace being served.
struct Namespace {
    id: u32,
    blocks: u64,
    block_size: usize,
}

impl Controller {
    fn r32(&self, off: usize) -> u32 {
        self.regs.read32(off)
    }

    fn w32(&self, off: usize, v: u32) {
        self.regs.write32(off, v)
    }

    fn r64(&self, off: usize) -> u64 {
        self.r32(off) as u64 | (self.r32(off + 4) as u64) << 32
    }

    fn w64(&self, off: usize, v: u64) {
        self.w32(off, v as u32);
        self.w32(off + 4, (v >> 32) as u32);
    }

    /// Waits until `done` holds, up to `timeout_ns`.
    fn wait(&self, timeout_ns: u64, mut done: impl FnMut(&Controller) -> bool) -> bool {
        let end = runtime::time::now_ns() + timeout_ns;
        while !done(self) {
            if runtime::time::now_ns() > end {
                return false;
            }
            runtime::time::sleep(runtime::time::Duration::from_millis(1));
        }
        true
    }

    /// Runs command `cmd` (its sixteen dwords; the identifier is filled in)
    /// on queue pair `io` (else the admin one) and waits for it: dword 0 of
    /// its completion.
    fn run(&mut self, io: bool, mut cmd: [u32; 16]) -> Result<u32, Failure> {
        if self.gone {
            return Err(Failure::Gone);
        }
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        cmd[0] |= (id as u32) << 16;
        let stride = self.stride;
        let q = if io { &mut self.io } else { &mut self.admin };
        let bytes: Vec<u8> = cmd.iter().flat_map(|d| d.to_le_bytes()).collect();
        self.mem.write(q.sq + q.tail as usize * 64, &bytes).map_err(|_| Failure::Memory)?;
        q.tail = (q.tail + 1) % ENTRIES;
        let (qid, tail) = (q.id as usize, q.tail);
        fence(Ordering::SeqCst);
        self.w32(reg::DOORBELLS + 2 * qid * stride, tail as u32);
        let end = runtime::time::now_ns() + COMMAND_TIMEOUT_NS;
        let mut spins = 0u32;
        loop {
            let q = if io { &mut self.io } else { &mut self.admin };
            let at = q.cq + q.head as usize * 16;
            // The completion queue's entry, which the controller writes whole
            // before it flips the phase tag (in dword 3, with its status and
            // the command's identifier): read as it is now.
            let last = self.mem.read32(at + 12).map_err(|_| Failure::Memory)?;
            let status = last >> 16;
            if (status & 1 == 1) == q.phase {
                fence(Ordering::SeqCst);
                let result = self.mem.read32(at).map_err(|_| Failure::Memory)?;
                let done_id = last as u16;
                q.head = (q.head + 1) % ENTRIES;
                if q.head == 0 {
                    q.phase = !q.phase;
                }
                let head = q.head;
                self.w32(reg::DOORBELLS + (2 * qid + 1) * stride, head as u32);
                if done_id != id {
                    // An answer to a command that timed out before.
                    continue;
                }
                return match (status >> 1) & 0x7FF {
                    0 => Ok(result),
                    code => Err(Failure::Status(code as u16)),
                };
            }
            if runtime::time::now_ns() > end || self.r32(reg::CSTS) & CSTS_CFS != 0 {
                return Err(Failure::Timeout);
            }
            spins += 1;
            match &self.irq {
                Some(irq) if spins > 50 => {
                    let _ = irq.wait_irq(runtime::time::now_ns() + 1_000_000);
                    let _ = irq.ack();
                }
                _ if spins > 500 => runtime::time::sleep(runtime::time::Duration::from_millis(1)),
                _ => core::hint::spin_loop(),
            }
        }
    }

    /// An admin or NVM command: its opcode, namespace, data pointers and
    /// command dwords 10 to 15.
    fn command(opcode: u8, namespace: u32, prp: [u64; 2], cdw: [u32; 6]) -> [u32; 16] {
        let mut c = [0u32; 16];
        c[0] = opcode as u32;
        c[1] = namespace;
        c[6] = prp[0] as u32;
        c[7] = (prp[0] >> 32) as u32;
        c[8] = prp[1] as u32;
        c[9] = (prp[1] >> 32) as u32;
        c[10..16].copy_from_slice(&cdw);
        c
    }

    /// IDENTIFY: the 4 KiB structure `cns` names (of `namespace`).
    fn identify(&mut self, cns: u32, namespace: u32) -> Result<Vec<u8>, Failure> {
        let buffer = self.mem.phys() + IDENTIFY as u64;
        self.run(false, Self::command(ADMIN_IDENTIFY, namespace, [buffer, 0], [cns, 0, 0, 0, 0, 0]))?;
        self.mem.to_vec(IDENTIFY, PAGE).map_err(|_| Failure::Memory)
    }

    /// The data buffer's `len` bytes from page `offset` on as PRP entries:
    /// the first page, and the second or the list of the others (the
    /// buffer's list from that page on).
    fn prp(&self, offset: usize, len: usize) -> [u64; 2] {
        let first = self.mem.phys() + (DATA + offset) as u64;
        match len {
            0..=PAGE => [first, 0],
            _ if len <= 2 * PAGE => [first, first + PAGE as u64],
            _ => [first, self.mem.phys() + (PRP_LIST + offset / PAGE * 8) as u64],
        }
    }

    /// Reads (or writes) `count` blocks of `ns` at `lba`, the data in the
    /// data buffer, in as many commands as the controller needs (each a
    /// whole number of pages of the buffer).
    fn transfer(&mut self, ns: &Namespace, lba: u64, count: usize, write: bool) -> Result<(), BlockError> {
        let per_command = (self.max_transfer / ns.block_size).max(1);
        let mut done = 0;
        while done < count {
            let n = (count - done).min(per_command);
            let offset = done * ns.block_size;
            let opcode = if write { IO_WRITE } else { IO_READ };
            let start = lba + done as u64;
            let prp = self.prp(offset, n * ns.block_size);
            let cmd = Self::command(opcode, ns.id, prp, [start as u32, (start >> 32) as u32, n as u32 - 1, 0, 0, 0]);
            if let Err(e) = self.run(true, cmd) {
                let what = if write { "write" } else { "read" };
                println!("namespace {}: {} of {} blocks at {} failed: {}", ns.id, what, n, start, e);
                return Err(self.failed(e));
            }
            done += n;
        }
        Ok(())
    }

    /// A command failed for `why`: if the controller did not answer, it is
    /// reset (which drops every command it has, so that none writes into
    /// the buffer when later ones use it) and started again, or given up if
    /// it does not start. The request fails either way.
    fn failed(&mut self, why: Failure) -> BlockError {
        if matches!(why, Failure::Timeout)
            && let Err(e) = self.restart()
        {
            println!("the controller does not start again ({}): its disks are given up", e);
            self.gone = true;
        }
        BlockError::Io
    }

    /// Resets the controller and starts it again with fresh queues.
    fn restart(&mut self) -> Result<(), String> {
        if !self.disable() {
            return Err(String::from("does not reset"));
        }
        // The queues start again at their first entries, and the
        // completions there from before must not look new.
        self.admin = Queue::new(0, ADMIN_SQ, ADMIN_CQ);
        self.io = Queue::new(1, IO_SQ, IO_CQ);
        let queues = alloc::vec![0; IDENTIFY - ADMIN_SQ];
        self.mem.write(ADMIN_SQ, &queues).map_err(|e| format!("its queues: {e}"))?;
        start(self)
    }

    /// How long the controller may take to get ready, or to reset: CAP.TO,
    /// in 500 ms.
    fn ready_timeout(&self) -> u64 {
        ((self.cap >> 24) & 0xFF).max(1) * 500_000_000
    }

    /// Resets the controller: it stops, and drops its queues and every
    /// command in them. False if it does not.
    fn disable(&self) -> bool {
        if self.r32(reg::CC) & CC_EN != 0 {
            self.w32(reg::CC, self.r32(reg::CC) & !CC_EN);
        }
        self.wait(self.ready_timeout(), |c| c.r32(reg::CSTS) & CSTS_RDY == 0)
    }
}

/// Resets the controller and starts it with its admin queues, then its
/// I/O queue pair.
fn start(c: &mut Controller) -> Result<(), String> {
    if !c.disable() {
        return Err(String::from("does not reset"));
    }
    let timeout = c.ready_timeout();
    let base = c.mem.phys();
    let entries = (ENTRIES - 1) as u32;
    c.w32(reg::AQA, entries << 16 | entries);
    c.w64(reg::ASQ, base + ADMIN_SQ as u64);
    c.w64(reg::ACQ, base + ADMIN_CQ as u64);
    c.w32(reg::CC, CC_IO_ENTRIES | CC_EN);
    if !c.wait(timeout, |c| c.r32(reg::CSTS) & (CSTS_RDY | CSTS_CFS) != 0) || c.r32(reg::CSTS) & CSTS_CFS != 0 {
        return Err(String::from("does not start"));
    }
    // One I/O queue pair, its completions on interrupt entry 0.
    let fail = |what: &str, e: Failure| format!("{what}: {e}");
    c.run(false, Controller::command(ADMIN_SET_FEATURES, 0, [0, 0], [FEATURE_QUEUES, 0, 0, 0, 0, 0]))
        .map_err(|e| fail("cannot have an I/O queue", e))?;
    let interrupts = if c.irq.is_some() { 1 << 1 } else { 0 };
    let cq = Controller::command(
        ADMIN_CREATE_CQ,
        0,
        [base + IO_CQ as u64, 0],
        [entries << 16 | 1, interrupts | 1, 0, 0, 0, 0],
    );
    c.run(false, cq).map_err(|e| fail("cannot make the I/O completion queue", e))?;
    let sq =
        Controller::command(ADMIN_CREATE_SQ, 0, [base + IO_SQ as u64, 0], [entries << 16 | 1, 1 << 16 | 1, 0, 0, 0, 0]);
    c.run(false, sq).map_err(|e| fail("cannot make the I/O submission queue", e))?;
    Ok(())
}

/// The namespace `id`, if it can be served: its size and block size.
fn namespace(c: &mut Controller, id: u32) -> Option<Namespace> {
    let ns = c.identify(IDENTIFY_NAMESPACE, id).ok()?;
    let qword = |at: usize| u64::from_le_bytes(ns[at..at + 8].try_into().unwrap_or_default());
    let blocks = qword(0);
    let format = (ns[26] & 0xF) as usize | ((ns[26] >> 5) as usize & 3) << 4;
    let lbaf = u32::from_le_bytes(ns[128 + 4 * format..132 + 4 * format].try_into().unwrap_or_default());
    let (metadata, shift) = (lbaf & 0xFFFF, (lbaf >> 16) & 0xFF);
    if blocks == 0 {
        return None;
    }
    if metadata != 0 || !matches!(shift, 9 | 12) {
        // A block size of 2^shift bytes, which may be past what a u64 holds.
        let size = 1u64.checked_shl(shift).map_or_else(|| format!("2^{shift}"), |size| format!("{size}"));
        println!("namespace {}: blocks of {} bytes with {} of metadata are not supported", id, size, metadata);
        return None;
    }
    Some(Namespace { id, blocks, block_size: 1 << shift })
}

/// The controller and the namespaces it serves.
struct Nvme {
    c: Controller,
    namespaces: Vec<Namespace>,
}

impl block::Disks for Nvme {
    fn read(&mut self, disk: usize, lba: u64, count: u32) -> Result<Vec<u8>, BlockError> {
        let ns = self.namespaces.get(disk).ok_or(BlockError::Invalid)?;
        self.c.transfer(ns, lba, count as usize, false)?;
        self.c.mem.to_vec(DATA, count as usize * ns.block_size).map_err(|_| BlockError::Invalid)
    }

    fn write(&mut self, disk: usize, lba: u64, data: &[u8]) -> Result<(), BlockError> {
        let ns = self.namespaces.get(disk).ok_or(BlockError::Invalid)?;
        self.c.mem.write(DATA, data).map_err(|_| BlockError::Invalid)?;
        self.c.transfer(ns, lba, data.len() / ns.block_size, true)
    }

    fn flush(&mut self, disk: usize) -> Result<(), BlockError> {
        let ns = self.namespaces.get(disk).ok_or(BlockError::Invalid)?;
        let cmd = Controller::command(IO_FLUSH, ns.id, [0, 0], [0; 6]);
        match self.c.run(true, cmd) {
            Ok(_) => Ok(()),
            Err(e) => {
                println!("namespace {}: flush failed: {}", ns.id, e);
                Err(self.c.failed(e))
            }
        }
    }
}

fn main() -> i32 {
    let Some(h) = runtime::env::take_handle(proto::startup::driver::PCIDEV) else {
        println!("no pcidev channel");
        return 1;
    };
    let pci = pcidev::Client::new(Channel::from_handle(h));
    let location = pci.info().map(|i| format!("{:02x}:{:02x}.{}", i.bus, i.slot, i.function)).unwrap_or_default();
    if !matches!(pci.enable(true), Ok(Ok(()))) {
        println!("cannot enable the controller");
        return 1;
    }
    let Ok(Ok(dma)) = pci.dma_resource() else {
        println!("no DMA resource");
        return 1;
    };
    let regs = match Registers::map(&pci, REGISTERS) {
        Ok(regs) => regs,
        Err(e) => {
            println!("cannot map the registers (BAR {}): {}", REGISTERS, e);
            return 1;
        }
    };
    let Ok(mem) = DmaBuffer::new(&dma, MEMORY) else {
        println!("no DMA memory");
        return 1;
    };
    let mut c = Controller {
        regs,
        cap: 0,
        mem,
        irq: None,
        stride: 4,
        max_transfer: MAX_TRANSFER as usize,
        admin: Queue::new(0, ADMIN_SQ, ADMIN_CQ),
        io: Queue::new(1, IO_SQ, IO_CQ),
        next_id: 0,
        gone: false,
    };
    c.cap = c.r64(reg::CAP);
    let cap = c.cap;
    c.stride = 4 << ((cap >> 32) & 0xF);
    // The doorbells of both queue pairs, as far apart as CAP.DSTRD says,
    // are inside the registers.
    if c.regs.len() < reg::DOORBELLS + 4 * c.stride {
        println!("register window too small ({} bytes) for doorbells {} bytes apart", c.regs.len(), c.stride);
        return 1;
    }
    // Queues of `ENTRIES` (CAP.MQES is the most, less one), the NVM
    // command set (CAP.CSS), 4 KiB pages (CAP.MPSMIN).
    let mpsmin = (cap >> 48) & 0xF;
    if (cap & 0xFFFF) + 1 < ENTRIES as u64 || cap & (1 << 37) == 0 || mpsmin != 0 {
        println!("controller at {}: not one this driver takes (capabilities {:#x})", location, cap);
        return 1;
    }
    // The PRP list: the data buffer's pages after the first.
    let data = c.mem.phys() + DATA as u64;
    let list: Vec<u8> =
        (1..MAX_TRANSFER as usize / PAGE).flat_map(|i| (data + (i * PAGE) as u64).to_le_bytes()).collect();
    if c.mem.write(PRP_LIST, &list).is_err() {
        println!("no room for the PRP list");
        return 1;
    }
    c.irq = proto::pci::enable_msix(&pci).or_else(|| proto::pci::enable_msi(&pci));
    if let Err(why) = start(&mut c) {
        println!("controller at {}: {}", location, why);
        return 1;
    }
    let Ok(controller) = c.identify(IDENTIFY_CONTROLLER, 0) else {
        println!("controller at {}: IDENTIFY CONTROLLER failed", location);
        return 1;
    };
    // Identify strings: ASCII, padded with spaces.
    let serial = block::device_text(&controller[4..24]);
    let model = block::device_text(&controller[24..64]);
    // MDTS: the largest transfer, in pages, as a power of two (0: no limit;
    // nor is one larger than memory).
    let mdts = controller[77] as u32;
    if mdts != 0
        && let Some(most) = 1usize.checked_shl(mdts).and_then(|pages| pages.checked_mul(PAGE))
    {
        c.max_transfer = c.max_transfer.min(most);
    }
    let Ok(list) = c.identify(IDENTIFY_NAMESPACES, 0) else {
        println!("controller at {}: no namespace list", location);
        return 1;
    };
    let ids: Vec<u32> =
        list.as_chunks::<4>().0.iter().map(|b| u32::from_le_bytes(*b)).take_while(|&id| id != 0).collect();
    let namespaces: Vec<Namespace> = ids.iter().filter_map(|&id| namespace(&mut c, id)).collect();
    if namespaces.is_empty() {
        println!("controller at {} ({}): no namespaces", location, model);
        return 0;
    }

    // One disk per namespace: the first named by the controller's serial
    // number, the others by it and their ids.
    let base = block::disk_id(&serial, &format!("nvme-{location}"));
    let polling = if c.irq.is_some() { "" } else { ", polling" };
    let served: Vec<Served> = namespaces
        .iter()
        .enumerate()
        .map(|(i, ns)| {
            let id = if i == 0 { base.clone() } else { format!("{}-n{}", base, ns.id) };
            let info = BlockInfo {
                sectors: ns.blocks,
                sector_size: ns.block_size as u32,
                read_only: false,
                serial: if serial.is_empty() { String::new() } else { id.clone() },
            };
            let description = format!(
                "{} namespace {}: {} MiB in {}-byte blocks ({}){}",
                location,
                ns.id,
                info.mib(),
                ns.block_size,
                model,
                polling
            );
            Served { id, info, description }
        })
        .collect();
    block::serve(&mut Nvme { c, namespaces }, &served);
    1
}
