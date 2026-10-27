// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Persistence of the user's home directory on the home disk.
//!
//! The disk with serial number [`HOME_DISK_SERIAL`] holds snapshots of
//! `/home` ([`filesystem_core::snapshot`]) in two slots, each a header
//! sector followed by the snapshot. A save writes the slot not holding the
//! newest snapshot, its data first and its header last, each made durable
//! before the next; loading takes the newest slot whose snapshot reads back
//! whole and matches its checksum, else the other. So a crash or power loss
//! in the middle of a save loses at most that save, and a snapshot that
//! cannot be read is not overwritten before one that can be has taken its
//! place.
//!
//! Snapshots stream to and from the disk a transfer at a time: neither
//! saving nor loading holds a second copy of the home directory in memory.
//! Every request to the disk has a deadline, so that a disk that stops
//! answering fails the save rather than stops the VFS.

use alloc::vec::Vec;

use ipc::Bytes;
use proto::block::{self as blk, block};
use proto::fs::FsError;
use rt::println;

use filesystem_core::snapshot::{self, Samples, Sink, Source};
use filesystem_core::tree::Tree;

/// Serial number of the disk that holds the home directory.
pub const HOME_DISK_SERIAL: &str = "veda-home";
const MAGIC: [u8; 8] = *b"VHOMEFS1";
const SECTOR: usize = 512;
/// First sector of slot 0 (the start of the disk is left alone).
const SLOT0: u64 = 8;
/// How long to wait for the home disk's driver at boot.
const ATTACH_TIMEOUT_NS: u64 = 3_000_000_000;
/// How long the disk may take over one request (at most `MAX_TRANSFER`
/// bytes, or a flush), even under emulation: a disk that takes longer has
/// failed, and the VFS, which waits for it, must go on serving files.
const DISK_TIMEOUT_NS: u64 = 30_000_000_000;
/// Bytes read or written at a time.
const TRANSFER: usize = blk::MAX_TRANSFER as usize;

/// The home disk.
pub struct Store {
    disk: block::Client,
    sectors: u64,
    /// The newest snapshot's generation.
    generation: u64,
    /// The slot the next save goes to.
    next_slot: u64,
}

fn io(e: impl core::fmt::Display) -> FsError {
    println!("home disk: {}", e);
    FsError::Io
}

/// The header of a slot that holds a snapshot.
struct Header {
    generation: u64,
    len: u64,
    crc: u32,
}

impl Store {
    /// Connects to the home disk, waiting a few seconds for its driver.
    /// Returns `None` if there is no usable home disk.
    pub fn connect() -> Option<Store> {
        let name = blk::service_name(HOME_DISK_SERIAL);
        let start = rt::time::now_ns();
        loop {
            let names = proto::with_registry(|r| r.list()).ok().and_then(|r| r.ok()).unwrap_or_default();
            if names.contains(&name) {
                break;
            }
            if rt::time::now_ns() - start > ATTACH_TIMEOUT_NS {
                return None;
            }
            rt::time::sleep(rt::time::Duration::from_millis(20));
        }
        let disk = block::Client::new(proto::connect(&name).ok()?);
        disk.set_timeout(DISK_TIMEOUT_NS);
        let info = disk.info().ok()?;
        if info.read_only || info.sector_size != SECTOR as u32 || info.sectors < 2 * (SLOT0 + 64) {
            println!(
                "home disk unusable ({} sectors of {} bytes{})",
                info.sectors,
                info.sector_size,
                if info.read_only { ", read-only" } else { "" }
            );
            return None;
        }
        Some(Store { disk, sectors: info.sectors, generation: 0, next_slot: 0 })
    }

    fn slot_start(&self, slot: u64) -> u64 {
        if slot == 0 { SLOT0 } else { self.sectors / 2 }
    }

    /// Bytes of snapshot data a slot can hold.
    pub fn capacity(&self) -> usize {
        ((self.sectors / 2 - SLOT0 - 1) as usize) * SECTOR
    }

    /// Reads `count` sectors (at most a transfer's worth) from `lba`.
    fn read(&self, lba: u64, count: usize) -> Result<Vec<u8>, FsError> {
        let Bytes(data) = self.disk.read(lba, count as u32).map_err(io)?.map_err(io)?;
        if data.len() != count * SECTOR {
            return Err(FsError::Io);
        }
        Ok(data)
    }

    /// Writes `data` (at most a transfer's worth), padded to whole sectors,
    /// at `lba`.
    fn write(&self, lba: u64, data: &[u8]) -> Result<(), FsError> {
        let mut buf = data.to_vec();
        buf.resize(data.len().next_multiple_of(SECTOR), 0);
        self.disk.write(lba, Bytes(buf)).map_err(io)?.map_err(io)
    }

    fn flush(&self) -> Result<(), FsError> {
        self.disk.flush().map_err(io)?.map_err(io)
    }

    /// The header of `slot`, if it holds a snapshot that fits in it.
    fn header(&self, slot: u64) -> Option<Header> {
        let h = self.read(self.slot_start(slot), 1).ok()?;
        if h[0..8] != MAGIC {
            return None;
        }
        let word = |at: usize| u64::from_le_bytes(h[at..at + 8].try_into().unwrap_or_default());
        let header = Header {
            generation: word(8),
            len: word(16),
            crc: u32::from_le_bytes(h[24..28].try_into().unwrap_or_default()),
        };
        if header.len > self.capacity() as u64 {
            println!("home snapshot in slot {} is damaged; ignoring it", slot);
            return None;
        }
        Some(header)
    }

    /// Restores `/home` from the newest snapshot that reads back whole,
    /// each read into a file system that `fresh` makes. `None` if no
    /// snapshot can be read.
    pub fn load(&mut self, fresh: impl Fn() -> Tree, samples: &Samples) -> Option<Tree> {
        let mut found: Vec<(u64, Header)> = (0..2).filter_map(|slot| Some((slot, self.header(slot)?))).collect();
        // Newest first. Until one is restored, the next save goes where the
        // older is (or to slot 0), keeping the newest.
        found.sort_by_key(|(_, h)| core::cmp::Reverse(h.generation));
        self.generation = found.first().map_or(0, |(_, h)| h.generation);
        self.next_slot = match found.as_slice() {
            [_, (older, _)] => *older,
            [(only, _)] => 1 - only,
            _ => 0,
        };
        for (slot, h) in &found {
            let mut ram = fresh();
            let mut src = DiskSource::new(self, self.slot_start(*slot) + 1, h.len);
            match snapshot::read(&mut src, h.len, &mut ram, samples) {
                Ok(()) if src.hasher.clone().finalize() == h.crc => {
                    self.next_slot = 1 - slot;
                    return Some(ram);
                }
                Ok(()) => println!("home snapshot in slot {} is damaged; ignoring it", slot),
                Err(e) => println!("home snapshot in slot {} cannot be read ({}); ignoring it", slot, e),
            }
        }
        None
    }

    /// Writes a snapshot of `/home` in `ram`, `len` bytes long
    /// (`snapshot::len`), into the slot that does not hold the newest: data
    /// first, then its header.
    pub fn save(&mut self, ram: &Tree, samples: &Samples, len: usize) -> Result<(), FsError> {
        if len > self.capacity() {
            return Err(FsError::NoSpace);
        }
        let start = self.slot_start(self.next_slot);
        let mut sink = DiskSink::new(self, start + 1);
        snapshot::write(ram, samples, &mut sink);
        let (written, crc) = sink.finish()?;
        if written != len {
            // Measured and written the same way: not the tree it measured.
            return Err(FsError::Io);
        }
        self.flush()?;
        let mut header = [0u8; SECTOR];
        header[0..8].copy_from_slice(&MAGIC);
        header[8..16].copy_from_slice(&(self.generation + 1).to_le_bytes());
        header[16..24].copy_from_slice(&(len as u64).to_le_bytes());
        header[24..28].copy_from_slice(&crc.to_le_bytes());
        self.write(start, &header)?;
        self.flush()?;
        self.generation += 1;
        self.next_slot = 1 - self.next_slot;
        Ok(())
    }
}

/// A snapshot's bytes read from the disk a transfer at a time, and their
/// checksum.
struct DiskSource<'a> {
    store: &'a Store,
    /// The next sector to read, and the snapshot's bytes still on the disk.
    lba: u64,
    left: u64,
    buf: Vec<u8>,
    pos: usize,
    hasher: crc32fast::Hasher,
}

impl DiskSource<'_> {
    fn new(store: &Store, lba: u64, len: u64) -> DiskSource<'_> {
        DiskSource { store, lba, left: len, buf: Vec::new(), pos: 0, hasher: crc32fast::Hasher::new() }
    }
}

impl Source for DiskSource<'_> {
    fn read(&mut self, out: &mut [u8]) -> bool {
        let mut done = 0;
        while done < out.len() {
            if self.pos == self.buf.len() {
                if self.left == 0 {
                    return false;
                }
                let bytes = self.left.min(TRANSFER as u64) as usize;
                let sectors = bytes.div_ceil(SECTOR);
                let Ok(mut data) = self.store.read(self.lba, sectors) else { return false };
                data.truncate(bytes);
                self.hasher.update(&data);
                self.buf = data;
                self.pos = 0;
                self.lba += sectors as u64;
                self.left -= bytes as u64;
            }
            let n = (out.len() - done).min(self.buf.len() - self.pos);
            out[done..done + n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
            done += n;
            self.pos += n;
        }
        true
    }
}

/// Writes a snapshot to the disk a transfer at a time, and its checksum.
/// The first failure stops it.
struct DiskSink<'a> {
    store: &'a Store,
    /// The next sector to write.
    lba: u64,
    buf: Vec<u8>,
    written: usize,
    hasher: crc32fast::Hasher,
    failed: Option<FsError>,
}

impl DiskSink<'_> {
    fn new(store: &Store, lba: u64) -> DiskSink<'_> {
        DiskSink {
            store,
            lba,
            buf: Vec::with_capacity(TRANSFER),
            written: 0,
            hasher: crc32fast::Hasher::new(),
            failed: None,
        }
    }

    /// Writes what is buffered.
    fn drain(&mut self) {
        if self.failed.is_none()
            && !self.buf.is_empty()
            && let Err(e) = self.store.write(self.lba, &self.buf)
        {
            self.failed = Some(e);
        }
        self.lba += self.buf.len().div_ceil(SECTOR) as u64;
        self.buf.clear();
    }

    /// Writes the rest; returns how many bytes there were and their
    /// checksum.
    fn finish(mut self) -> Result<(usize, u32), FsError> {
        self.drain();
        match self.failed {
            Some(e) => Err(e),
            None => Ok((self.written, self.hasher.finalize())),
        }
    }
}

impl Sink for DiskSink<'_> {
    fn bytes(&mut self, mut b: &[u8]) {
        if self.failed.is_some() {
            return;
        }
        self.written += b.len();
        self.hasher.update(b);
        while !b.is_empty() {
            let n = b.len().min(TRANSFER - self.buf.len());
            self.buf.extend_from_slice(&b[..n]);
            b = &b[n..];
            if self.buf.len() == TRANSFER {
                self.drain();
            }
        }
    }
}
