// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Memory objects and address-space management.

use abi::{Error, RawHandle, Rights, cache_policy, dma_flags, map_flags, resource_kind, vmo_flags};

use super::{SysResult, get_resource, get_vmo, handle, insert, ok, target_process};
use crate::memory::aspace::Perms;
use crate::memory::paging::Cache;
use crate::memory::user;
use crate::memory::vmo::Vmo;
use crate::object::KObject;
use crate::synchronization::bkl;

const VMO_RIGHTS: Rights =
    Rights(Rights::BASIC.0 | Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0 | Rights::EXECUTE.0 | Rights::GET_INFO.0);

/// Bytes a long operation (committing, reading or writing a VMO) handles
/// between two moments it lets the other CPUs into the kernel
/// ([`bkl::relax`]).
const STEP: usize = 256 * 1024;

pub fn vmo_create(size: usize, flags: usize) -> SysResult {
    if flags & !(vmo_flags::COMMIT | vmo_flags::WRITE_COMBINING) != 0 {
        return Err(Error::InvalidArgs);
    }
    let cache = if flags & vmo_flags::WRITE_COMBINING != 0 { Cache::WriteCombining } else { Cache::WriteBack };
    let vmo = Vmo::new_anonymous(size as u64, cache).ok_or(Error::InvalidArgs)?;
    if flags & vmo_flags::COMMIT != 0 {
        // No other CPU can reach the new VMO: the lock may be let go between
        // steps.
        for off in (0..vmo.size()).step_by(4096) {
            vmo.page(off, true).ok_or(Error::NoMemory)?;
            if (off + 4096).is_multiple_of(STEP as u64) {
                bkl::relax();
            }
        }
    }
    ok(insert(KObject::Vmo(vmo), VMO_RIGHTS)? as usize)
}

pub fn vmo_read(raw: RawHandle, offset: usize, buf: usize, len: usize) -> SysResult {
    let vmo = get_vmo(raw, Rights::READ)?;
    let mut tmp = user::buffer(len.min(STEP))?;
    let mut done = 0;
    while done < len {
        let n = (len - done).min(tmp.len());
        if !vmo.read(offset.checked_add(done).ok_or(Error::OutOfRange)? as u64, &mut tmp[..n]) {
            return Err(Error::OutOfRange);
        }
        user::copy_to_user((buf + done) as u64, &tmp[..n])?;
        done += n;
        // The VMO stays referenced, and each step's user memory is
        // checked as it is copied.
        if done < len {
            bkl::relax();
        }
    }
    ok(len)
}

pub fn vmo_write(raw: RawHandle, offset: usize, buf: usize, len: usize) -> SysResult {
    let vmo = get_vmo(raw, Rights::WRITE)?;
    let mut tmp = user::buffer(len.min(STEP))?;
    let mut done = 0;
    while done < len {
        let n = (len - done).min(tmp.len());
        user::copy_from_user(&mut tmp[..n], (buf + done) as u64)?;
        if !vmo.write(offset.checked_add(done).ok_or(Error::OutOfRange)? as u64, &tmp[..n]) {
            return Err(Error::OutOfRange);
        }
        done += n;
        if done < len {
            bkl::relax();
        }
    }
    ok(len)
}

pub fn vmo_get_size(raw: RawHandle) -> SysResult {
    ok(get_vmo(raw, Rights::NONE)?.size() as usize)
}

pub fn vmo_create_slice(raw: RawHandle, offset: usize, size: usize) -> SysResult {
    let h = handle(raw, Rights::DUPLICATE)?;
    let KObject::Vmo(vmo) = &h.object else { return Err(Error::WrongType) };
    let slice = vmo.slice(offset as u64, size as u64)?;
    ok(insert(KObject::Vmo(slice), h.rights)? as usize)
}

pub fn vmo_set_size(raw: RawHandle, size: usize) -> SysResult {
    if !get_vmo(raw, Rights::WRITE)?.grow(size as u64) {
        return Err(Error::InvalidArgs);
    }
    ok(0)
}

pub fn vmo_create_physical(res: RawHandle, paddr: usize, size: usize, cache: usize) -> SysResult {
    let r = get_resource(res, Rights::NONE)?;
    if size == 0 || !r.permits(resource_kind::MMIO, paddr as u64, size as u64) {
        return Err(Error::AccessDenied);
    }
    // Devices' and the firmware's memory, never the kernel's RAM, nor the
    // registers the kernel keeps for itself.
    if crate::memory::phys::overlaps_ram(paddr as u64, size as u64) {
        crate::kwarn!("vmo: refused a physical VMO over RAM ({:#x}, {} bytes)", paddr, size);
        return Err(Error::AccessDenied);
    }
    if crate::memory::kvirt::is_kernels(paddr as u64, size as u64) {
        crate::kwarn!("vmo: refused a physical VMO over the kernel's registers ({:#x}, {} bytes)", paddr, size);
        return Err(Error::AccessDenied);
    }
    let cache = match cache {
        cache_policy::WRITE_BACK => Cache::WriteBack,
        cache_policy::WRITE_COMBINING => Cache::WriteCombining,
        cache_policy::UNCACHED => Cache::Uncached,
        _ => return Err(Error::InvalidArgs),
    };
    // The pages the range touches (the root resource permits any range).
    let span = (size as u64)
        .checked_add(paddr as u64 & 0xFFF)
        .and_then(crate::memory::checked_page_align_up)
        .filter(|&span| span <= crate::memory::vmo::MAX_VMO_SIZE)
        .ok_or(Error::InvalidArgs)?;
    let vmo = Vmo::new_physical(paddr as u64, span, cache);
    let rights = Rights(Rights::BASIC.0 | Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0 | Rights::GET_INFO.0);
    ok(insert(KObject::Vmo(vmo), rights)? as usize)
}

pub fn vmo_create_contiguous(res: RawHandle, size: usize, flags: usize) -> SysResult {
    if flags & !(dma_flags::BELOW_4G | dma_flags::WRITE_COMBINING) != 0 {
        return Err(Error::InvalidArgs);
    }
    let r = get_resource(res, Rights::NONE)?;
    if !r.permits(resource_kind::DMA, 0, 0) && !r.permits(resource_kind::DMA, r.base, 0) {
        return Err(Error::AccessDenied);
    }
    let limit = if flags & dma_flags::BELOW_4G != 0 { 1 << 32 } else { u64::MAX };
    let cache = if flags & dma_flags::WRITE_COMBINING != 0 { Cache::WriteCombining } else { Cache::WriteBack };
    let vmo = Vmo::new_contiguous(size as u64, limit, cache).ok_or(Error::NoMemory)?;
    ok(insert(KObject::Vmo(vmo), VMO_RIGHTS)? as usize)
}

pub fn vmo_phys_addr(raw: RawHandle, offset: usize) -> SysResult {
    let vmo = get_vmo(raw, Rights::NONE)?;
    ok(vmo.phys_addr(offset as u64).ok_or(Error::NotSupported)? as usize)
}

/// The most pages one `vmo_pages` call reports.
const MAX_PAGES: usize = 1 << 20;

pub fn vmo_pages(res: RawHandle, raw: RawHandle, offset: usize, count: usize, out: usize) -> SysResult {
    let r = get_resource(res, Rights::NONE)?;
    if !r.permits(resource_kind::DMA, 0, 0) && !r.permits(resource_kind::DMA, r.base, 0) {
        return Err(Error::AccessDenied);
    }
    // A device reads and writes them.
    let vmo = get_vmo(raw, Rights(Rights::READ.0 | Rights::WRITE.0))?;
    if !offset.is_multiple_of(4096) || count > MAX_PAGES {
        return Err(Error::InvalidArgs);
    }
    let mut chunk = alloc::vec::Vec::with_capacity(count.min(512));
    let mut done = 0;
    while done < count {
        chunk.clear();
        for i in done..count.min(done + 512) {
            let at = (offset as u64).checked_add(i as u64 * 4096).ok_or(Error::OutOfRange)?;
            if at >= vmo.size() {
                return Err(Error::OutOfRange);
            }
            chunk.push(vmo.page(at, true).ok_or(Error::NoMemory)?);
        }
        let bytes: alloc::vec::Vec<u8> = chunk.iter().flat_map(|a| a.to_le_bytes()).collect();
        user::copy_to_user((out + done * 8) as u64, &bytes)?;
        done += chunk.len();
        if done < count {
            bkl::relax();
        }
    }
    ok(count)
}

/// Refuses new executable memory to a process that gave it up.
fn executable_allowed(perms: Perms) -> Result<(), Error> {
    if perms.exec { super::task::allowed(abi::restrict::NO_EXECUTE) } else { Ok(()) }
}

pub fn vm_map(proc: RawHandle, vmo: RawHandle, offset: usize, len: usize, addr: usize, flags: usize) -> SysResult {
    let p = target_process(proc)?;
    let perms = Perms::from_map_flags(flags);
    executable_allowed(perms)?;
    // Where the page tables cannot give an access without reading (no
    // write-only or execute-only pages), that access takes the READ right.
    let mut needed = Rights::MAP;
    if perms.effective().read {
        needed = needed | Rights::READ;
    }
    if perms.write {
        needed = needed | Rights::WRITE;
    }
    if perms.exec {
        needed = needed | Rights::EXECUTE;
    }
    let h = handle(vmo, needed)?;
    let KObject::Vmo(vmo) = h.object else { return Err(Error::WrongType) };
    // What vm_protect may later grant: no more than the handle allows.
    let readable = h.rights.contains(Rights::READ);
    let max = Perms {
        read: readable,
        write: readable && h.rights.contains(Rights::WRITE),
        exec: readable && h.rights.contains(Rights::EXECUTE),
    };
    let aspace = p.aspace().ok_or(Error::BadState)?;
    let at = aspace.map(
        vmo,
        offset as u64,
        len as u64,
        addr as u64,
        flags & map_flags::FIXED != 0,
        perms,
        max,
        flags & map_flags::COMMIT != 0,
    )?;
    ok(at as usize)
}

pub fn vm_allocate(proc: RawHandle, len: usize, addr: usize, flags: usize) -> SysResult {
    let p = target_process(proc)?;
    executable_allowed(Perms::from_map_flags(flags))?;
    let aspace = p.aspace().ok_or(Error::BadState)?;
    let at = aspace.allocate(
        len as u64,
        addr as u64,
        flags & map_flags::FIXED != 0,
        Perms::from_map_flags(flags),
        flags & map_flags::COMMIT != 0,
    )?;
    ok(at as usize)
}

pub fn vm_decommit(proc: RawHandle, addr: usize, len: usize) -> SysResult {
    let p = target_process(proc)?;
    p.aspace().ok_or(Error::BadState)?.decommit(addr as u64, len as u64)?;
    ok(0)
}

pub fn vm_unmap(proc: RawHandle, addr: usize, len: usize) -> SysResult {
    let p = target_process(proc)?;
    p.aspace().ok_or(Error::BadState)?.unmap(addr as u64, len as u64)?;
    ok(0)
}

pub fn vm_protect(proc: RawHandle, addr: usize, len: usize, flags: usize) -> SysResult {
    let p = target_process(proc)?;
    executable_allowed(Perms::from_map_flags(flags))?;
    p.aspace().ok_or(Error::BadState)?.protect(addr as u64, len as u64, Perms::from_map_flags(flags))?;
    ok(0)
}
