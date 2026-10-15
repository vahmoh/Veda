// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The block device protocol: sector-level access to a disk, and how disk
//! drivers serve it ([`serve`]).
//!
//! A disk driver registers one service per disk, named after the disk's
//! serial number (see [`service_name`] and [`disk_id`]), so that clients can
//! find a particular disk (such as the user's home disk) without probing.
//! Only the file system may use a disk ([`may_use`]): it decides who reads
//! and writes what of the files on it (`/system`, `/home/.private`), which
//! anyone else reading or writing its sectors would bypass.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use vabi::signals;
use vipc::{Bytes, WaitSet, enumeration, message, protocol};
use vrt::object::Channel;
use vrt::println;

use crate::init::ClientIdentity;

enumeration! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum BlockError {
        /// The device reported an error.
        Io = 1,
        /// The request goes past the end of the disk.
        OutOfRange = 2,
        ReadOnly = 3,
        /// Bad arguments (unaligned data, too large a transfer).
        Invalid = 4,
    }
}

impl core::fmt::Display for BlockError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            BlockError::Io => "I/O error",
            BlockError::OutOfRange => "beyond the end of the disk",
            BlockError::ReadOnly => "the disk is read-only",
            BlockError::Invalid => "invalid request",
        })
    }
}

message! {
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct BlockInfo {
        /// Capacity in sectors.
        pub sectors: u64,
        /// Bytes per sector (512 for virtio).
        pub sector_size: u32,
        pub read_only: bool,
        /// The disk's serial number ("" if it has none).
        pub serial: String,
    }
}

impl BlockInfo {
    /// The disk's capacity in MiB (for the log; as large as a `u64` holds,
    /// whatever its device says).
    pub fn mib(&self) -> u64 {
        self.sectors.saturating_mul(self.sector_size as u64) >> 20
    }
}

/// Largest transfer of one `read` or `write`, in bytes.
pub const MAX_TRANSFER: u32 = 32 * 1024;

/// The service name of the disk with serial number `serial`.
pub fn service_name(serial: &str) -> String {
    format!("block/{serial}")
}

protocol! {
    /// A disk.
    pub mod block = "block" {
        1 => fn info() -> BlockInfo;
        /// Reads `count` sectors starting at sector `lba` (at most
        /// `MAX_TRANSFER` bytes).
        2 => fn read(lba: u64, count: u32) -> Result<Bytes, BlockError>;
        /// Writes whole sectors starting at sector `lba`.
        3 => fn write(lba: u64, data: Bytes) -> Result<(), BlockError>;
        /// Makes previous writes durable.
        4 => fn flush() -> Result<(), BlockError>;
    }
}

/// The program that may use a disk: the file system, a system service of
/// this name.
pub const CLIENT: &str = "vfs";

/// Whether `who` may use a disk: the file system alone, which decides who
/// reads and writes what of the files on it.
pub fn may_use(who: &ClientIdentity) -> bool {
    who.service && who.name == CLIENT
}

/// What a disk tells of itself (its serial number, its model) as text fit
/// for its name and the log: of `raw` (ASCII, padded with spaces or ended
/// by a NUL), its printable characters but `/` (which would put its name
/// below another's), without the padding.
pub fn device_text(raw: &[u8]) -> String {
    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    let text: String = raw[..end]
        .iter()
        .filter(|&&b| b == b' ' || (b.is_ascii_graphic() && b != b'/'))
        .map(|&b| char::from(b))
        .collect();
    String::from(text.trim())
}

/// What a disk is called (it is served as `block/<id>`): its serial number,
/// or, if it has none, where it is (`location`, which its driver words:
/// `pci-00:04.0`).
pub fn disk_id(serial: &str, location: &str) -> String {
    String::from(if serial.is_empty() { location } else { serial })
}

/// A disk a driver serves (see [`serve`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Served {
    /// What it is called: it is served as `block/<id>` (see [`disk_id`]).
    pub id: String,
    /// What `info` answers.
    pub info: BlockInfo,
    /// Where it is and what it is, for the log (`00:04.0: 64 MiB`).
    pub description: String,
}

/// What a driver does for the disks it serves: the reads, writes and
/// flushes of disk `disk` (its index in what it gave [`serve`]). Each has
/// been checked against the disk first: whole sectors, at least one and at
/// most [`MAX_TRANSFER`] bytes of them, all on the disk; no writes to a
/// read-only one.
pub trait Disks {
    /// Reads `count` sectors from sector `lba` on: `count` times the disk's
    /// sector size bytes.
    fn read(&mut self, disk: usize, lba: u64, count: u32) -> Result<Vec<u8>, BlockError>;
    /// Writes `data`, whole sectors, from sector `lba` on.
    fn write(&mut self, disk: usize, lba: u64, data: &[u8]) -> Result<(), BlockError>;
    /// Makes the disk's earlier writes durable.
    fn flush(&mut self, disk: usize) -> Result<(), BlockError>;
}

/// How many bytes `count` sectors from sector `lba` of the disk `info`
/// describes are, if one request may have them: at least one sector and at
/// most [`MAX_TRANSFER`] bytes (else `Invalid`), all of them on the disk
/// (else `OutOfRange`).
fn transfer(info: &BlockInfo, lba: u64, count: u64) -> Result<usize, BlockError> {
    let bytes = count
        .checked_mul(info.sector_size as u64)
        .filter(|&bytes| bytes > 0 && bytes <= MAX_TRANSFER as u64)
        .ok_or(BlockError::Invalid)?;
    match lba.checked_add(count) {
        Some(end) if end <= info.sectors => Ok(bytes as usize),
        _ => Err(BlockError::OutOfRange),
    }
}

/// The requests of a connection to disk `disk`, which reach its driver
/// checked.
struct Session<'a, D: ?Sized> {
    disks: &'a mut D,
    disk: usize,
    info: &'a BlockInfo,
}

impl<D: Disks + ?Sized> block::Server for Session<'_, D> {
    fn info(&mut self) -> BlockInfo {
        self.info.clone()
    }

    fn read(&mut self, lba: u64, count: u32) -> Result<Bytes, BlockError> {
        let len = transfer(self.info, lba, count as u64)?;
        let data = self.disks.read(self.disk, lba, count)?;
        // All that was asked for, or the driver failed.
        if data.len() != len {
            return Err(BlockError::Io);
        }
        Ok(Bytes(data))
    }

    fn write(&mut self, lba: u64, data: Bytes) -> Result<(), BlockError> {
        if self.info.read_only {
            return Err(BlockError::ReadOnly);
        }
        let sector = self.info.sector_size as usize;
        if sector == 0 || !data.0.len().is_multiple_of(sector) {
            return Err(BlockError::Invalid);
        }
        transfer(self.info, lba, (data.0.len() / sector) as u64)?;
        self.disks.write(self.disk, lba, &data.0)
    }

    fn flush(&mut self) -> Result<(), BlockError> {
        self.disks.flush(self.disk)
    }
}

/// The keys of connections to disks, which follow their disks' listeners'
/// (the disks' indices).
const CONNECTIONS: u64 = 1 << 32;

/// Serves the disks `served` describes, which `disks` reads and writes:
/// registers each as `block/<id>`, takes connections from the file system
/// alone (anyone else's are closed, and logged), and answers each request
/// on a connection, checked, for the disk it was made to. Returns once it
/// has neither a disk registered nor a connection: at once if no disk
/// could be registered.
pub fn serve<D: Disks + ?Sized>(disks: &mut D, served: &[Served]) {
    let mut listeners: BTreeMap<u64, (Channel, usize)> = BTreeMap::new();
    for (disk, s) in served.iter().enumerate() {
        match crate::register(&service_name(&s.id)) {
            Ok(listener) => {
                println!("disk {} at {}", s.id, s.description);
                listeners.insert(disk as u64, (listener, disk));
            }
            Err(e) => println!("cannot register {}: {:?}", service_name(&s.id), e),
        }
    }
    let mut connections: BTreeMap<u64, (Channel, usize)> = BTreeMap::new();
    let mut next = CONNECTIONS;
    while !listeners.is_empty() || !connections.is_empty() {
        let mut ws = WaitSet::new();
        for (&key, (channel, _)) in listeners.iter().chain(&connections) {
            ws.add(channel.raw(), signals::READABLE | signals::PEER_CLOSED, key);
        }
        let Ok(ready) = ws.wait(vabi::DEADLINE_INFINITE) else { continue };
        for (key, observed) in ready {
            if let Some((listener, disk)) = listeners.get(&key) {
                let disk = *disk;
                while let Some((channel, who)) = crate::accept_with_identity(listener) {
                    if may_use(&who) {
                        connections.insert(next, (channel, disk));
                        next += 1;
                    } else {
                        println!(
                            "{} (process {}) may not use disk {}: only the file system does",
                            who.name, who.koid, served[disk].id
                        );
                    }
                }
                // The registry is gone: no more connections come.
                if observed & signals::PEER_CLOSED != 0 {
                    listeners.remove(&key);
                }
                continue;
            }
            let Some((channel, disk)) = connections.get(&key) else { continue };
            if observed & signals::READABLE != 0 {
                let disk = *disk;
                while let Ok(msg) = channel.read() {
                    let mut session = Session { disks: &mut *disks, disk, info: &served[disk].info };
                    match block::dispatch(&mut session, msg) {
                        Ok(reply) => {
                            let _ = reply.send(channel);
                        }
                        Err(e) => println!("bad request: {}", e),
                    }
                }
            } else if observed & signals::PEER_CLOSED != 0 {
                connections.remove(&key);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::block::Server;
    use super::*;
    use alloc::vec;

    /// Disks that answer every request, and remember what reached them.
    #[derive(Default)]
    struct Recorded {
        /// The disk, the sector and the bytes of each request.
        requests: Vec<(usize, u64, usize)>,
        /// Reads give a sector less than asked for.
        short: bool,
    }

    impl Disks for Recorded {
        fn read(&mut self, disk: usize, lba: u64, count: u32) -> Result<Vec<u8>, BlockError> {
            let len = count as usize * 512 - if self.short { 512 } else { 0 };
            self.requests.push((disk, lba, len));
            Ok(vec![7; len])
        }

        fn write(&mut self, disk: usize, lba: u64, data: &[u8]) -> Result<(), BlockError> {
            self.requests.push((disk, lba, data.len()));
            Ok(())
        }

        fn flush(&mut self, disk: usize) -> Result<(), BlockError> {
            self.requests.push((disk, 0, 0));
            Ok(())
        }
    }

    fn disk(sectors: u64, sector_size: u32, read_only: bool) -> BlockInfo {
        BlockInfo { sectors, sector_size, read_only, serial: String::from("veda-home") }
    }

    fn who(name: &str, service: bool) -> ClientIdentity {
        ClientIdentity { koid: 9, name: String::from(name), service, app: !service, requested: false }
    }

    #[test]
    fn only_the_file_system_may_use_a_disk() {
        assert!(may_use(&who("vfs", true)));
        // An application may take the file system's name, but is none.
        assert!(!may_use(&who("vfs", false)));
        assert!(!may_use(&who("shell", true)));
        assert!(!may_use(&who("terminal", false)));
        assert!(!may_use(&ClientIdentity::default()));
    }

    #[test]
    fn reads_reach_the_driver_checked() {
        let info = disk(100, 512, false);
        let mut disks = Recorded::default();
        let mut s = Session { disks: &mut disks, disk: 1, info: &info };
        assert_eq!(s.read(10, 4).map(|b| b.0.len()), Ok(2048));
        assert_eq!(s.read(0, 0), Err(BlockError::Invalid));
        assert_eq!(s.read(0, MAX_TRANSFER / 512 + 1), Err(BlockError::Invalid));
        assert_eq!(s.read(0, u32::MAX), Err(BlockError::Invalid));
        assert_eq!(s.read(97, 4), Err(BlockError::OutOfRange));
        assert_eq!(s.read(u64::MAX - 1, 4), Err(BlockError::OutOfRange));
        assert_eq!(s.read(96, 4).map(|b| b.0.len()), Ok(2048));
        assert_eq!(disks.requests, [(1, 10, 2048), (1, 96, 2048)]);
    }

    #[test]
    fn writes_reach_the_driver_checked() {
        let info = disk(100, 4096, false);
        let mut disks = Recorded::default();
        let mut s = Session { disks: &mut disks, disk: 0, info: &info };
        assert_eq!(s.write(3, Bytes(vec![1; 8192])), Ok(()));
        assert_eq!(s.write(3, Bytes(Vec::new())), Err(BlockError::Invalid));
        assert_eq!(s.write(3, Bytes(vec![1; 512])), Err(BlockError::Invalid));
        assert_eq!(s.write(0, Bytes(vec![1; MAX_TRANSFER as usize + 4096])), Err(BlockError::Invalid));
        assert_eq!(s.write(99, Bytes(vec![1; 8192])), Err(BlockError::OutOfRange));
        assert_eq!(s.flush(), Ok(()));
        assert_eq!(disks.requests, [(0, 3, 8192), (0, 0, 0)]);

        let info = disk(100, 512, true);
        let mut s = Session { disks: &mut disks, disk: 0, info: &info };
        assert_eq!(s.write(0, Bytes(vec![1; 512])), Err(BlockError::ReadOnly));
        let info = disk(100, 0, false);
        let mut s = Session { disks: &mut disks, disk: 0, info: &info };
        assert_eq!(s.write(0, Bytes(vec![1; 512])), Err(BlockError::Invalid));
        assert_eq!(s.read(0, 1), Err(BlockError::Invalid));
        assert_eq!(disks.requests.len(), 2);
    }

    #[test]
    fn a_driver_that_reads_too_little_fails_the_read() {
        let info = disk(100, 512, false);
        let mut disks = Recorded { short: true, ..Recorded::default() };
        let mut s = Session { disks: &mut disks, disk: 0, info: &info };
        assert_eq!(s.read(0, 2), Err(BlockError::Io));
    }

    #[test]
    fn disks_are_named_by_what_they_say_fit_for_a_name() {
        assert_eq!(device_text(b"veda-home\0\0\0\0\0\0\0\0\0\0\0"), "veda-home");
        assert_eq!(device_text(b"  S4EVNF0M123456     "), "S4EVNF0M123456");
        // Nothing of a line of its own in the log, nor a name below
        // another's.
        assert_eq!(device_text(b"veda\nhome/\x1b[2J\xff"), "vedahome[2J");
        assert_eq!(device_text(b"\0veda-home"), "");
        assert_eq!(device_text(b"    "), "");
        assert_eq!(disk_id("veda-home", "pci-00:04.0"), "veda-home");
        assert_eq!(disk_id("", "pci-00:04.0"), "pci-00:04.0");
        assert_eq!(service_name(&disk_id("", "ahci-00:1f.2-0")), "block/ahci-00:1f.2-0");
    }

    #[test]
    fn a_disk_of_any_size_has_a_size_in_mib() {
        assert_eq!(disk(2048, 512, false).mib(), 1);
        assert_eq!(disk(u64::MAX, 4096, false).mib(), u64::MAX >> 20);
    }
}
