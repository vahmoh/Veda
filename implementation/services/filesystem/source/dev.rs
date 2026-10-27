// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `/dev`: the devices every POSIX system has.
//!
//! | Name | Reading gives | Writing |
//! |------|---------------|---------|
//! | `null` | nothing (the end of the file) | discards the data |
//! | `zero` | zero bytes | discards the data |
//! | `full` | zero bytes | fails: no space left |
//! | `random`, `urandom` | the kernel's random bytes | discards the data |
//!
//! The terminal (`/dev/tty`) and a program's own descriptors (`/dev/stdin`,
//! `/dev/fd/N`) differ from process to process; the POSIX layer provides
//! them. `/dev/shm`, which [`Dev::list`]'s caller lists with the devices,
//! is a file system of its own: the shared memory objects
//! (`filesystem::shared_memory`).

use alloc::vec::Vec;

use proto::fs::{DirEntry, FsError, Stat, device};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dev {
    Null,
    Zero,
    Full,
    Random,
}

/// The directory of the shared memory objects, in `/dev`.
pub const SHM: &str = "shm";

/// Every device with its name.
const ALL: [(&str, Dev); 5] =
    [("full", Dev::Full), ("null", Dev::Null), ("random", Dev::Random), ("urandom", Dev::Random), ("zero", Dev::Zero)];

/// Inode of `/dev` itself; the devices follow.
const DIR_INODE: u64 = 1;

impl Dev {
    /// The device a path inside `/dev` names.
    pub fn find(comps: &[&str]) -> Result<Option<Dev>, FsError> {
        match comps {
            [] => Ok(None),
            [name] => ALL.iter().find(|(n, _)| n == name).map(|&(_, d)| Some(d)).ok_or(FsError::NotFound),
            [name, ..] if ALL.iter().any(|(n, _)| n == name) => Err(FsError::NotDir),
            _ => Err(FsError::NotFound),
        }
    }

    fn inode(self) -> u64 {
        DIR_INODE + 1 + self as u64
    }

    pub fn stat(dev: Option<Dev>) -> Stat {
        Stat {
            // Its devices, and the shared memory objects' directory.
            size: if dev.is_none() { ALL.len() as u64 + 1 } else { 0 },
            is_dir: dev.is_none(),
            read_only: false,
            modified: 0,
            inode: dev.map_or(DIR_INODE, Dev::inode),
            device: device::DEV,
            executable: false,
            char_device: dev.is_some(),
            socket: false,
        }
    }

    pub fn list() -> Vec<DirEntry> {
        ALL.iter()
            .map(|&(name, d)| DirEntry {
                name: name.into(),
                is_dir: false,
                size: 0,
                modified: 0,
                inode: d.inode(),
                executable: false,
                socket: false,
            })
            .collect()
    }

    /// Reads up to `len` bytes. Random bytes the kernel cannot give (it is
    /// short of memory) are an I/O error for the reader, never bytes that
    /// are not random, nor the end of the file systems' service.
    pub fn read(self, len: usize) -> Result<Vec<u8>, FsError> {
        match self {
            Dev::Null => Ok(Vec::new()),
            Dev::Zero | Dev::Full => Ok(alloc::vec![0; len]),
            Dev::Random => {
                let mut v = alloc::vec![0; len];
                rt::object::try_random_bytes(&mut v).map_err(|_| FsError::Io)?;
                Ok(v)
            }
        }
    }

    /// Writes `len` bytes; returns how many were taken.
    pub fn write(self, len: usize) -> Result<u32, FsError> {
        match self {
            Dev::Full => Err(FsError::NoSpace),
            _ => Ok(len as u32),
        }
    }
}
