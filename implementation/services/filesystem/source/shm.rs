// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The memory of `/dev/shm`'s shared memory objects
//! (`filesystem::shared_memory`): a VMO each, which the VFS reads and
//! writes for the files that have the object open, and hands out to map
//! ([`Memory::handle`]): every process that maps it shares the memory, and
//! keeps it while its mapping lasts, whatever becomes of the object.

use abi::Rights;
use proto::fs::FsError;
use runtime::object::Vmo;

/// An object's memory.
pub struct Memory(Vmo);

impl Memory {
    /// New memory, which holds nothing yet (a VMO has a page at least).
    pub fn new() -> Result<Memory, FsError> {
        Vmo::create(abi::PAGE_SIZE).map(Memory).map_err(|_| FsError::NoSpace)
    }

    /// A handle to the memory, to map it: one that cannot write it unless
    /// `writable`.
    pub fn handle(&self, writable: bool) -> Result<Vmo, FsError> {
        let rights = self.0.0.basic_info().map_err(|_| FsError::Io)?.rights;
        let rights = if writable { rights } else { rights & !Rights::WRITE.0 };
        self.0.0.duplicate(Some(Rights(rights))).map(Vmo::from_handle).map_err(|_| FsError::NoSpace)
    }
}

impl filesystem::shared_memory::Memory for Memory {
    fn reserve(&mut self, len: u64) -> Result<(), FsError> {
        let held = self.0.size().map_err(|_| FsError::Io)? as u64;
        if len > held {
            self.0.set_size(len as usize).map_err(|_| FsError::NoSpace)?;
        }
        Ok(())
    }

    fn read(&self, offset: u64, buf: &mut [u8]) -> Result<(), FsError> {
        self.0.read(offset as usize, buf).map_err(|_| FsError::Io)
    }

    fn write(&mut self, offset: u64, data: &[u8]) -> Result<(), FsError> {
        // Writing commits memory, which may have run out.
        self.0.write(offset as usize, data).map_err(|_| FsError::NoSpace)
    }
}
