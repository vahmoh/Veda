// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Memory: `mmap` and friends.
//!
//! Memory files (`memfd_create`) map as [`crate::memfd`] says, and so do
//! shared memory objects (`/dev/shm`, files of the VFS's whose bytes are
//! memory): a shared mapping of one is its memory itself, which the VFS
//! hands out (`file::memory`), shared with every process that maps it.
//!
//! Private anonymous memory is the kernel's private memory (`vm_allocate`):
//! its pages are freed as soon as they are unmapped, parts of mappings
//! included, or dropped with `MADV_DONTNEED`. Other mappings get a VMO of
//! their own, which the mapping keeps alive (the handle is closed at once).
//! Either way `munmap` and `mprotect` are plain kernel calls on address
//! ranges. A file mapping is a private copy of the file's bytes: changes
//! are never written back, so shared writable file mappings are refused,
//! and a shared one is mapped through a handle without the right to write,
//! so that `mprotect` cannot make it writable either (`EACCES`). A mapping
//! that does not write a file of the system image is the image's own pages
//! instead (shared by everyone who maps them, as programs run from them),
//! which `mprotect` cannot make writable either, private or not.

use abi::{Error, Rights, map_flags};
use rt::object::Vmo;
use rt::vm;

use crate::error::{self, SysResult};
use crate::fd::{self, Object};
use crate::file::VfsFile;
use crate::linux::errno::*;
use crate::linux::mman::*;

const PAGE: usize = abi::PAGE_SIZE;

/// The kernel's mapping flags for POSIX protections.
pub(crate) fn map_prot(prot: u32) -> usize {
    let mut f = 0;
    if prot & PROT_READ != 0 {
        f |= map_flags::READ;
    }
    // Pages that can be written or executed can be read, as on Linux.
    if prot & PROT_WRITE != 0 {
        f |= map_flags::READ | map_flags::WRITE;
    }
    if prot & PROT_EXEC != 0 {
        f |= map_flags::READ | map_flags::EXECUTE;
    }
    f
}

pub unsafe fn mmap(addr: usize, len: usize, prot: u32, flags: u32, fd: i32, offset: i64) -> SysResult {
    if len == 0
        || offset < 0
        || !(offset as usize).is_multiple_of(PAGE)
        || prot & !(PROT_READ | PROT_WRITE | PROT_EXEC) != 0
    {
        return Err(EINVAL);
    }
    let len = len.checked_next_multiple_of(PAGE).ok_or(ENOMEM)?;
    let kind = flags & MAP_TYPE;
    if !matches!(kind, MAP_SHARED | MAP_PRIVATE | MAP_SHARED_VALIDATE) {
        return Err(EINVAL);
    }
    let fixed = flags & (MAP_FIXED | MAP_FIXED_NOREPLACE) != 0;
    if fixed && !addr.is_multiple_of(PAGE) {
        return Err(EINVAL);
    }
    // The memory of a shared memory object that a shared mapping maps.
    let mut memory = None;
    let desc = if flags & MAP_ANONYMOUS != 0 {
        None
    } else {
        let desc = fd::get(fd)?;
        if !desc.readable() {
            return Err(EACCES);
        }
        if !matches!(desc.object, Object::File(_) | Object::Memfd(_)) {
            return Err(ENODEV);
        }
        if let Object::File(f) = &desc.object
            && kind != MAP_PRIVATE
        {
            let end = (offset as u64).checked_add(len as u64).ok_or(ENOMEM)?;
            memory = f.memory(end)?;
            if memory.is_some() && prot & PROT_WRITE != 0 && !desc.writable() {
                return Err(EACCES);
            }
        }
        // Other files' changes are never written back: a shared mapping of
        // one can only be read. (A memory file's is its memory.)
        if matches!(desc.object, Object::File(_)) && memory.is_none() && kind != MAP_PRIVATE && prot & PROT_WRITE != 0 {
            return Err(ENODEV);
        }
        Some(desc)
    };
    // Where the mapping goes, and whether its pages are provided now.
    let mut place = 0;
    if flags & MAP_POPULATE != 0 {
        place |= map_flags::COMMIT;
    }
    if fixed {
        if flags & MAP_FIXED != 0 && flags & MAP_FIXED_NOREPLACE == 0 {
            // MAP_FIXED replaces whatever was there.
            match vm::unmap(None, addr, len) {
                Ok(()) | Err(Error::NotFound) => {}
                Err(e) => return Err(error::kernel(e)),
            }
        }
        place |= map_flags::FIXED;
    }
    let hint = addr & !(PAGE - 1);
    if let Some(vmo) = memory {
        return vm::map(None, &vmo, offset as usize, len, hint, map_prot(prot) | place).map_err(map_error);
    }
    if let Some(d) = desc.as_deref()
        && let Object::Memfd(m) = &d.object
    {
        return m.map(offset as u64, len, hint, place, prot, kind != MAP_PRIVATE, d.writable());
    }
    let file = desc.as_deref().and_then(|d| match &d.object {
        Object::File(f) => Some(f),
        _ => None,
    });
    match (file, kind == MAP_PRIVATE) {
        (Some(f), private) => map_file(f, offset as u64, len, hint, place, prot, private),
        (None, true) => vm::allocate(None, len, hint, map_prot(prot) | place).map_err(map_error),
        (None, false) => Vmo::create(len)
            .and_then(|vmo| vm::map(None, &vmo, 0, len, hint, map_prot(prot) | place))
            .map_err(map_error),
    }
}

/// The `errno` for a mapping that cannot be made.
fn map_error(e: Error) -> isize {
    match e {
        Error::AlreadyExists => EEXIST,
        Error::NoMemory | Error::OutOfRange => ENOMEM,
        e => error::kernel(e),
    }
}

/// Maps `len` bytes of `file` from `offset`: the system image's pages, if
/// they are its and the mapping does not write them; otherwise a copy of
/// them, in a VMO of the mapping's own. A private mapping is filled in place
/// and then given the protections asked for. A shared one is filled through
/// a mapping of its own, then mapped through a handle that cannot write:
/// the kernel then refuses to make it writable later (`mprotect`), as its
/// changes would never reach the file.
fn map_file(file: &VfsFile, offset: u64, len: usize, hint: usize, place: usize, prot: u32, private: bool) -> SysResult {
    if prot & PROT_WRITE == 0
        && let Some(pages) = file.pages(offset, len)
    {
        return vm::map(None, &pages, 0, len, hint, map_prot(prot) | place).map_err(map_error);
    }
    let vmo = Vmo::create(len).map_err(map_error)?;
    let writable = map_flags::READ | map_flags::WRITE;
    // Where the file's bytes go in, writable.
    let fill = if private {
        vm::map(None, &vmo, 0, len, hint, writable | place)
    } else {
        vm::map(None, &vmo, 0, len, 0, writable)
    }
    .map_err(map_error)?;
    // SAFETY: the mapping was just made, `len` bytes, writable, and is
    // nobody else's.
    let buf = unsafe { core::slice::from_raw_parts_mut(fill as *mut u8, len) };
    let filled = file.pread(buf, offset);
    if private {
        let protected = filled.and_then(|_| {
            if map_prot(prot) == writable {
                return Ok(());
            }
            vm::protect(None, fill, len, map_prot(prot)).map_err(error::kernel)
        });
        return match protected {
            Ok(()) => Ok(fill),
            Err(e) => {
                let _ = vm::unmap(None, fill, len);
                Err(e)
            }
        };
    }
    let _ = vm::unmap(None, fill, len);
    filled?;
    let rights = vmo.0.basic_info().map_err(error::kernel)?.rights & !Rights::WRITE.0;
    let read_only = Vmo::from_handle(vmo.0.duplicate(Some(Rights(rights))).map_err(error::kernel)?);
    vm::map(None, &read_only, 0, len, hint, map_prot(prot) | place).map_err(map_error)
}

pub fn munmap(addr: usize, len: usize) -> SysResult {
    if !addr.is_multiple_of(PAGE) || len == 0 {
        return Err(EINVAL);
    }
    match vm::unmap(None, addr, len) {
        // Nothing mapped there is fine.
        Ok(()) | Err(Error::NotFound) => Ok(0),
        Err(e) => Err(error::kernel(e)),
    }
}

pub fn mprotect(addr: usize, len: usize, prot: u32) -> SysResult {
    if !addr.is_multiple_of(PAGE) || prot & !(PROT_READ | PROT_WRITE | PROT_EXEC) != 0 {
        return Err(EINVAL);
    }
    if len == 0 {
        return Ok(0);
    }
    vm::protect(None, addr, len, map_prot(prot)).map(|_| 0).map_err(|e| match e {
        Error::NotFound | Error::OutOfRange => ENOMEM,
        Error::AccessDenied => EACCES,
        e => error::kernel(e),
    })
}

/// `madvise`. Dropping pages (`MADV_DONTNEED`, `MADV_FREE`) frees them at
/// once in private anonymous memory, which reads as zeros afterwards; other
/// memory has nowhere to take its contents back from, so the advice is
/// refused there (`EINVAL`, as on Linux where it cannot apply). Other
/// advice is taken as given.
pub fn madvise(addr: usize, len: usize, advice: u32) -> SysResult {
    if !addr.is_multiple_of(PAGE) {
        return Err(EINVAL);
    }
    let len = len.checked_next_multiple_of(PAGE).ok_or(EINVAL)?;
    if len == 0 {
        return Ok(0);
    }
    match advice {
        MADV_DONTNEED | MADV_FREE => vm::decommit(None, addr, len).map(|()| 0).map_err(|e| match e {
            Error::NotFound => ENOMEM,
            Error::NotSupported => EINVAL,
            e => error::kernel(e),
        }),
        MADV_REMOVE => Err(EINVAL),
        _ => Ok(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protections() {
        assert_eq!(map_prot(0), 0);
        assert_eq!(map_prot(PROT_READ), map_flags::READ);
        assert_eq!(map_prot(PROT_WRITE), map_flags::READ | map_flags::WRITE);
        assert_eq!(map_prot(PROT_EXEC), map_flags::READ | map_flags::EXECUTE);
    }
}
