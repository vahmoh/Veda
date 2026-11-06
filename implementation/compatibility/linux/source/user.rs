// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The program's memory, as Linux compatibility reads and writes it for
//! the program (signal frames, `execve`'s arguments, the robust futex
//! list): only where the program's areas allow the access, and while they
//! do ([`crate::memory::while_accessible`]), as the kernel copies to and
//! from user memory, so that a bad address is a fault of the program's,
//! not this process's crash.

use alloc::vec::Vec;

use crate::memory::while_accessible;

/// Reads a `T` at `at`.
///
/// # Safety
/// `T` must be plain data, any bytes of which are a value.
pub unsafe fn read<T: Copy>(at: usize) -> Option<T> {
    // SAFETY: the program's memory, readable, held meanwhile.
    while_accessible(at, size_of::<T>(), false, || unsafe { core::ptr::read_unaligned(at as *const T) })
}

/// Writes `v` at `at`.
///
/// # Safety
/// `at` must be the program's memory, which nothing of this process uses.
pub unsafe fn write<T: Copy>(at: usize, v: T) -> Option<()> {
    // SAFETY: the program's memory, writable, held meanwhile.
    while_accessible(at, size_of::<T>(), true, || unsafe { core::ptr::write_unaligned(at as *mut T, v) })
}

/// Reads `out.len()` bytes at `at`.
///
/// # Safety
/// As [`read`].
pub unsafe fn read_bytes(at: usize, out: &mut [u8]) -> Option<()> {
    // SAFETY: the program's memory, readable, held meanwhile.
    while_accessible(at, out.len(), false, || unsafe {
        core::ptr::copy_nonoverlapping(at as *const u8, out.as_mut_ptr(), out.len())
    })
}

/// Writes `bytes` at `at`.
///
/// # Safety
/// As [`write`].
pub unsafe fn write_bytes(at: usize, bytes: &[u8]) -> Option<()> {
    // SAFETY: the program's memory, writable, held meanwhile.
    while_accessible(at, bytes.len(), true, || unsafe {
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), at as *mut u8, bytes.len())
    })
}

/// Zeroes `len` bytes at `at`.
///
/// # Safety
/// As [`write`].
pub unsafe fn zero(at: usize, len: usize) -> Option<()> {
    // SAFETY: the program's memory, writable, held meanwhile.
    while_accessible(at, len, true, || unsafe { core::ptr::write_bytes(at as *mut u8, 0, len) })
}

/// The NUL-terminated string at `at`, of at most `max` bytes before its
/// NUL.
///
/// # Safety
/// As [`read`].
pub unsafe fn string(at: usize, max: usize) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        // A page at a time, as far as the program's memory goes (its areas
        // are whole pages).
        let from = at.checked_add(out.len())?;
        let chunk = abi::PAGE_SIZE - from % abi::PAGE_SIZE;
        let ended = while_accessible(from, chunk, false, || {
            // SAFETY: the program's memory, readable, held meanwhile.
            let bytes = unsafe { core::slice::from_raw_parts(from as *const u8, chunk) };
            match bytes.iter().position(|&b| b == 0) {
                Some(n) => {
                    out.extend_from_slice(&bytes[..n]);
                    true
                }
                None => {
                    out.extend_from_slice(bytes);
                    false
                }
            }
        })?;
        if ended {
            return (out.len() <= max).then_some(out);
        }
        if out.len() > max {
            return None;
        }
    }
}

/// The NULL-terminated array of strings at `at` (`argv`, `envp`): at most
/// `max` bytes in all, strings and pointers.
///
/// # Safety
/// As [`read`].
pub unsafe fn strings(at: usize, max: usize) -> Option<Vec<Vec<u8>>> {
    let mut out = Vec::new();
    if at == 0 {
        return Some(out);
    }
    let mut total = 0usize;
    loop {
        // SAFETY: per the caller.
        let p: usize = unsafe { read(at.checked_add(8 * out.len())?)? };
        if p == 0 {
            return Some(out);
        }
        // SAFETY: per the caller.
        let s = unsafe { string(p, max.saturating_sub(total))? };
        total += s.len() + 1 + 8;
        if total > max {
            return None;
        }
        out.push(s);
    }
}
