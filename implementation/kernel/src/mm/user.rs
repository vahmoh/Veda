// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Copying data between the kernel and the current user address space.
//!
//! User memory can be changed concurrently by other threads of the process
//! running on other CPUs, so the kernel never keeps references into it: data
//! is validated (and faulted in) and then copied. Because mappings can only
//! change under the BKL, which the caller holds, a validated range cannot
//! disappear during the copy.
//!
//! A system call that moves data validates the caller's range once, as a
//! [`UserRange`], and copies straight between it and where the data is kept
//! (a socket's buffer, a channel's message, which [`read_vec`] fills): no
//! kernel buffer in between, and no second look at the page tables. The
//! range stays as it was validated until the call returns, as long as it
//! does not block in between.

use alloc::vec::Vec;

use vabi::Error;

use crate::arch::mm::{user_access_begin, user_access_end};
use crate::sched;

/// A range of the current address space's user memory, validated (and
/// faulted in) for reading, or for writing too, by the system call in
/// progress, which copies within it until it returns or blocks.
pub struct UserRange {
    addr: u64,
    len: usize,
    writable: bool,
}

impl UserRange {
    /// The `len` bytes at `addr`, which the kernel reads.
    pub fn readable(addr: u64, len: usize) -> Result<UserRange, Error> {
        validate(addr, len, false)?;
        Ok(UserRange { addr, len, writable: false })
    }

    /// The `len` bytes at `addr`, which the kernel writes.
    pub fn writable(addr: u64, len: usize) -> Result<UserRange, Error> {
        validate(addr, len, true)?;
        Ok(UserRange { addr, len, writable: true })
    }

    /// Copies the bytes at `offset` into `out`.
    pub fn read(&self, offset: usize, out: &mut [u8]) -> Result<(), Error> {
        let src = self.at(offset, out.len())?;
        if out.is_empty() {
            return Ok(());
        }
        user_access_begin();
        // SAFETY: within the range, validated and mapped readable.
        unsafe { core::ptr::copy_nonoverlapping(src as *const u8, out.as_mut_ptr(), out.len()) };
        user_access_end();
        Ok(())
    }

    /// Copies `data` to `offset`.
    pub fn write(&self, offset: usize, data: &[u8]) -> Result<(), Error> {
        if !self.writable {
            return Err(Error::Fault);
        }
        let dst = self.at(offset, data.len())?;
        if data.is_empty() {
            return Ok(());
        }
        user_access_begin();
        // SAFETY: within the range, validated and mapped writable.
        unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), dst as *mut u8, data.len()) };
        user_access_end();
        Ok(())
    }

    /// The address of the `len` bytes at `offset`, if the range holds them.
    fn at(&self, offset: usize, len: usize) -> Result<u64, Error> {
        match offset.checked_add(len) {
            Some(end) if end <= self.len => Ok(self.addr + offset as u64),
            _ => Err(Error::Fault),
        }
    }
}

fn validate(addr: u64, len: usize, write: bool) -> Result<(), Error> {
    if len == 0 {
        return Ok(());
    }
    let thread = sched::current();
    let aspace = thread.aspace.as_ref().ok_or(Error::Fault)?;
    if aspace.ensure_range(addr, len as u64, write) { Ok(()) } else { Err(Error::Fault) }
}

/// Copies `out.len()` bytes from user address `src`.
pub fn copy_from_user(out: &mut [u8], src: u64) -> Result<(), Error> {
    validate(src, out.len(), false)?;
    user_access_begin();
    // SAFETY: the range was validated and is mapped readable.
    unsafe { core::ptr::copy_nonoverlapping(src as *const u8, out.as_mut_ptr(), out.len()) };
    user_access_end();
    Ok(())
}

/// Copies `data` to user address `dst`.
pub fn copy_to_user(dst: u64, data: &[u8]) -> Result<(), Error> {
    validate(dst, data.len(), true)?;
    user_access_begin();
    // SAFETY: the range was validated and is mapped writable.
    unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), dst as *mut u8, data.len()) };
    user_access_end();
    Ok(())
}

/// Reads a plain-data value from user memory.
pub fn read<T: Copy>(src: u64) -> Result<T, Error> {
    let mut v = core::mem::MaybeUninit::<T>::uninit();
    // SAFETY: we fill every byte of `v` before assuming it initialised; the
    // ABI types read this way are valid for any bit pattern.
    let bytes = unsafe { core::slice::from_raw_parts_mut(v.as_mut_ptr() as *mut u8, core::mem::size_of::<T>()) };
    copy_from_user(bytes, src)?;
    // SAFETY: fully initialised above.
    Ok(unsafe { v.assume_init() })
}

/// Writes a plain-data value to user memory.
pub fn write<T: Copy>(dst: u64, value: &T) -> Result<(), Error> {
    // SAFETY: viewing a Copy value's bytes.
    let bytes = unsafe { core::slice::from_raw_parts(value as *const T as *const u8, core::mem::size_of::<T>()) };
    copy_to_user(dst, bytes)
}

/// A zeroed kernel buffer for data on its way to or from user memory:
/// `NoMemory`, not a panic, when the kernel heap cannot provide it, or when
/// it would reach into the memory the kernel keeps for itself
/// (`phys::user_may_use`).
pub fn buffer(len: usize) -> Result<Vec<u8>, Error> {
    if !crate::mm::phys::user_may_use(len as u64) {
        return Err(Error::NoMemory);
    }
    let mut v = Vec::new();
    v.try_reserve_exact(len).map_err(|_| Error::NoMemory)?;
    v.resize(len, 0);
    Ok(v)
}

/// Copies a user buffer into a new kernel vector (bounded by `max`), which
/// it fills straight from user memory (nothing is zeroed first).
pub fn read_vec(src: u64, len: usize, max: usize) -> Result<Vec<u8>, Error> {
    if len > max {
        return Err(Error::InvalidArgs);
    }
    if len == 0 {
        return Ok(Vec::new());
    }
    let from = UserRange::readable(src, len)?.at(0, len)?;
    if !crate::mm::phys::user_may_use(len as u64) {
        return Err(Error::NoMemory);
    }
    let mut v = Vec::new();
    v.try_reserve_exact(len).map_err(|_| Error::NoMemory)?;
    user_access_begin();
    // SAFETY: the range is validated and mapped readable, and `v` has room
    // for the `len` bytes copied, which are then its contents.
    unsafe {
        core::ptr::copy_nonoverlapping(from as *const u8, v.as_mut_ptr(), len);
        v.set_len(len);
    }
    user_access_end();
    Ok(v)
}

/// Reads an array of `u32` values (e.g. handle lists).
pub fn read_u32s(src: u64, count: usize, max: usize) -> Result<Vec<u32>, Error> {
    let bytes = read_vec(src, count.checked_mul(4).ok_or(Error::InvalidArgs)?, max * 4)?;
    Ok(bytes.as_chunks::<4>().0.iter().map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect())
}
