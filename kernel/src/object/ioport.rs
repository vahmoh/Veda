// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! I/O port ranges granted to user-space drivers. Port access goes through
//! the `ioport_read/write` system calls, which check the range.

use alloc::sync::Arc;

use vabi::Error;

use crate::arch;

pub struct IoPorts {
    pub koid: u64,
    pub base: u16,
    pub count: u16,
}

impl IoPorts {
    pub fn new(base: u16, count: u16) -> Arc<IoPorts> {
        Arc::new(IoPorts { koid: super::new_koid(), base, count })
    }

    fn check(&self, p: u16, width: usize) -> Result<(), Error> {
        let end = p as u32 + width as u32;
        if p < self.base || end > self.base as u32 + self.count as u32 {
            return Err(Error::AccessDenied);
        }
        Ok(())
    }

    pub fn read(&self, p: u16, width: usize) -> Result<u32, Error> {
        self.check(p, width)?;
        // SAFETY: the driver was granted this port range.
        unsafe { arch::ioport::read(p, width) }.ok_or(Error::InvalidArgs)
    }

    pub fn write(&self, p: u16, width: usize, value: u32) -> Result<(), Error> {
        self.check(p, width)?;
        // SAFETY: the driver was granted this port range.
        if unsafe { arch::ioport::write(p, width, value) } { Ok(()) } else { Err(Error::InvalidArgs) }
    }
}
