// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Memory files (`memfd_create`): files with no name, whose contents are a
//! VMO. Their descriptors read and write it, map it (`mmap`; shared
//! mappings are the memory itself), and go to other processes over Unix
//! domain sockets and to the programs a program starts, which then share
//! the memory: how programs such as WebKit's processes share buffers.
//!
//! * **Size.** The VMO grows as the file does (`ftruncate`, writes past its
//!   end); VMOs never shrink, so a file made shorter has its tail cleared
//!   and reads as shorter. A mapping may reach past the end, into memory
//!   that reads as zeros (where Linux raises `SIGBUS`).
//! * **Access** is the descriptor's, and the kernel enforces it: the handle
//!   behind a descriptor that may not write has no `WRITE` right, so
//!   neither it nor a process it is sent to can write the memory or map it
//!   writable. Opening `/dev/fd/N` read-only makes such a descriptor of a
//!   memory file ([`Memfd::reopened`]), as `/proc/self/fd/N` does on Linux.
//! * **Seals** (`MFD_ALLOW_SEALING`, `F_ADD_SEALS`): those that fix the
//!   size (`F_SEAL_SHRINK`, `F_SEAL_GROW`) and the seals themselves
//!   (`F_SEAL_SEAL`), and with `MFD_NOEXEC_SEAL` `F_SEAL_EXEC` (no
//!   executable mapping: its handles lose the `EXECUTE` right). The write
//!   seals would need every writable mapping in every process known, which
//!   no one here knows: they are refused (`EINVAL`), so that no program
//!   counts on memory that others may still write.
//! * **Sharing.** The descriptions of a memory file in one process (`dup`s,
//!   reopenings) share its size and seals; another process it goes to gets
//!   them as they are then, and the memory itself.

use alloc::sync::Arc;
use alloc::vec;

use abi::Rights;
use runtime::object::Vmo;
use runtime::sync::Mutex;
use runtime::vm;

use crate::error::{self, SysResult};
use crate::linux::errno::*;
use crate::linux::{self, mman::*};

/// `memfd_create`'s flags.
mod mfd {
    pub const CLOEXEC: u32 = linux_raw_sys::general::MFD_CLOEXEC;
    pub const ALLOW_SEALING: u32 = linux_raw_sys::general::MFD_ALLOW_SEALING;
    pub const HUGETLB: u32 = linux_raw_sys::general::MFD_HUGETLB;
    pub const NOEXEC_SEAL: u32 = linux_raw_sys::general::MFD_NOEXEC_SEAL;
    pub const EXEC: u32 = linux_raw_sys::general::MFD_EXEC;
}

/// The seals (`F_ADD_SEALS`, `F_GET_SEALS`).
pub mod seal {
    pub const SEAL: u32 = linux_raw_sys::general::F_SEAL_SEAL;
    pub const SHRINK: u32 = linux_raw_sys::general::F_SEAL_SHRINK;
    pub const GROW: u32 = linux_raw_sys::general::F_SEAL_GROW;
    pub const WRITE: u32 = linux_raw_sys::general::F_SEAL_WRITE;
    pub const FUTURE_WRITE: u32 = linux_raw_sys::general::F_SEAL_FUTURE_WRITE;
    pub const EXEC: u32 = linux_raw_sys::general::F_SEAL_EXEC;
}

/// The longest name a memory file takes (Linux's: 249 bytes, without the
/// NUL). Names are for debugging alone; Veda keeps none.
const MAX_NAME: usize = 249;

/// The largest a memory file grows: 64 GiB, far beyond any buffer shared
/// through one, and within what one VMO holds (1 TiB).
const MAX_SIZE: u64 = 64 << 30;

const PAGE: u64 = abi::PAGE_SIZE as u64;

/// What the descriptions of a memory file in this process share.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shared {
    /// Its size, in bytes: its VMO holds at least as much.
    pub size: u64,
    /// Its seals.
    pub seals: u32,
}

/// A memory file, as one description has it.
pub struct Memfd {
    /// Its memory, with the rights the description's access gives.
    vmo: Vmo,
    /// The memory's kernel object id: the file's inode number.
    pub koid: u64,
    shared: Arc<Mutex<Shared>>,
    /// This description's file offset.
    offset: Mutex<u64>,
}

impl Memfd {
    fn with(vmo: Vmo, koid: u64, shared: Arc<Mutex<Shared>>) -> Memfd {
        Memfd { vmo, koid, shared, offset: Mutex::new(0) }
    }

    /// A new, empty memory file (`memfd_create` with `flags`, whose
    /// `MFD_CLOEXEC` is the caller's).
    pub fn create(name: &[u8], flags: u32) -> Result<Memfd, isize> {
        if flags & !(mfd::CLOEXEC | mfd::ALLOW_SEALING | mfd::NOEXEC_SEAL | mfd::EXEC) != 0
            || flags & mfd::HUGETLB != 0
            || flags & (mfd::NOEXEC_SEAL | mfd::EXEC) == mfd::NOEXEC_SEAL | mfd::EXEC
            || name.len() > MAX_NAME
        {
            return Err(EINVAL);
        }
        // A VMO has at least a page; the file is empty all the same.
        let vmo = Vmo::create(PAGE as usize).map_err(error::kernel)?;
        let koid = vmo.0.koid();
        let mut seals = if flags & (mfd::ALLOW_SEALING | mfd::NOEXEC_SEAL) != 0 { 0 } else { seal::SEAL };
        let vmo = if flags & mfd::NOEXEC_SEAL != 0 {
            seals |= seal::EXEC;
            without(&vmo, Rights::EXECUTE)?
        } else {
            vmo
        };
        Ok(Memfd::with(vmo, koid, Arc::new(Mutex::new(Shared { size: 0, seals }))))
    }

    /// Another description of the same file in this process, with the
    /// access `writable` asks for (opening `/dev/fd/N`): its handle has no
    /// `WRITE` right unless it is to write, which this one's must allow.
    pub fn reopened(&self, writable: bool) -> Result<Memfd, isize> {
        let vmo = if writable {
            if !self.rights()?.contains(Rights::WRITE) {
                return Err(EACCES);
            }
            Vmo::from_handle(self.vmo.0.duplicate(None).map_err(error::kernel)?)
        } else {
            without(&self.vmo, Rights::WRITE)?
        };
        Ok(Memfd::with(vmo, self.koid, self.shared.clone()))
    }

    /// The file another process sent or passed on: its memory, its size
    /// and seals as they were then.
    pub fn arrived(vmo: Vmo, shared: Shared) -> Option<Memfd> {
        let koid = vmo.0.koid();
        let held = vmo.size().ok()? as u64;
        (shared.size <= held && shared.size <= MAX_SIZE).then(|| Memfd::with(vmo, koid, Arc::new(Mutex::new(shared))))
    }

    /// A handle to its memory, with this description's rights
    /// (`veda_memory_file`).
    pub fn memory(&self) -> Result<Vmo, isize> {
        self.vmo.0.duplicate(None).map(Vmo::from_handle).map_err(error::kernel)
    }

    /// What goes with the file to another process: a handle to its memory,
    /// without the right to write if `writable` is not set, and its size
    /// and seals.
    pub fn travelling(&self, writable: bool) -> Result<(Vmo, Shared), isize> {
        let vmo = if writable {
            Vmo::from_handle(self.vmo.0.duplicate(None).map_err(error::kernel)?)
        } else {
            without(&self.vmo, Rights::WRITE)?
        };
        Ok((vmo, *self.shared.lock()))
    }

    fn rights(&self) -> Result<Rights, isize> {
        self.vmo.0.basic_info().map(|i| Rights(i.rights)).map_err(error::kernel)
    }

    pub fn size(&self) -> u64 {
        self.shared.lock().size
    }

    /// Makes the memory hold at least `size` bytes.
    fn reserve(&self, size: u64) -> Result<(), isize> {
        if size > MAX_SIZE {
            return Err(EFBIG);
        }
        let held = self.vmo.size().map_err(error::kernel)? as u64;
        if size > held {
            self.vmo.set_size(size as usize).map_err(error::kernel)?;
        }
        Ok(())
    }

    /// `ftruncate`: the file becomes `len` bytes long, as its seals allow;
    /// what it loses reads as zeros if it grows again.
    pub fn truncate(&self, len: u64) -> Result<(), isize> {
        let mut shared = self.shared.lock();
        if (len < shared.size && shared.seals & seal::SHRINK != 0)
            || (len > shared.size && shared.seals & seal::GROW != 0)
        {
            return Err(EPERM);
        }
        if len < shared.size {
            self.clear(len, shared.size)?;
        } else {
            self.reserve(len)?;
        }
        shared.size = len;
        Ok(())
    }

    /// Zeroes `[from, to)` of the memory.
    fn clear(&self, from: u64, to: u64) -> Result<(), isize> {
        let zeros = vec![0u8; (to - from).min(64 * 1024) as usize];
        let mut at = from;
        while at < to {
            let n = (to - at).min(zeros.len() as u64) as usize;
            self.vmo.write(at as usize, &zeros[..n]).map_err(error::kernel)?;
            at += n as u64;
        }
        Ok(())
    }

    /// Reads at `at` (the description's offset, which moves, if `None`).
    pub fn read(&self, buf: &mut [u8], at: Option<u64>) -> SysResult {
        let mut offset = self.offset.lock();
        let from = at.unwrap_or(*offset);
        let size = self.size();
        let n = size.saturating_sub(from).min(buf.len() as u64) as usize;
        if n > 0 {
            self.vmo.read(from as usize, &mut buf[..n]).map_err(error::kernel)?;
        }
        if at.is_none() {
            *offset = from + n as u64;
        }
        Ok(n)
    }

    /// Writes at `at` (the description's offset, which moves, if `None`;
    /// the end with `append`), growing the file as its seals allow.
    pub fn write(&self, data: &[u8], at: Option<u64>, append: bool) -> SysResult {
        let mut offset = self.offset.lock();
        let mut shared = self.shared.lock();
        if shared.seals & (seal::WRITE | seal::FUTURE_WRITE) != 0 {
            return Err(EPERM);
        }
        let from = if append && at.is_none() { shared.size } else { at.unwrap_or(*offset) };
        let end = from.checked_add(data.len() as u64).ok_or(EFBIG)?;
        if end > shared.size && shared.seals & seal::GROW != 0 {
            return Err(EPERM);
        }
        self.reserve(end)?;
        self.vmo.write(from as usize, data).map_err(error::kernel)?;
        shared.size = shared.size.max(end);
        if at.is_none() {
            *offset = end;
        }
        Ok(data.len())
    }

    /// `lseek`.
    pub fn seek(&self, offset: i64, whence: u32) -> Result<u64, isize> {
        let mut at = self.offset.lock();
        let base = match whence {
            linux::seek::SET => 0,
            linux::seek::CUR => *at as i64,
            linux::seek::END => self.size() as i64,
            _ => return Err(EINVAL),
        };
        let to = base.checked_add(offset).filter(|&o| o >= 0).ok_or(EINVAL)?;
        *at = to as u64;
        Ok(*at)
    }

    /// `F_ADD_SEALS`: the seals the file can take (see the module's
    /// documentation), from a description that may write.
    pub fn add_seals(&self, seals: u32, writable: bool) -> Result<(), isize> {
        if seals & !(seal::SEAL | seal::SHRINK | seal::GROW | seal::EXEC) != 0 {
            return Err(EINVAL);
        }
        if !writable {
            return Err(EPERM);
        }
        let mut shared = self.shared.lock();
        if shared.seals & seal::SEAL != 0 {
            return Err(EPERM);
        }
        shared.seals |= seals;
        Ok(())
    }

    pub fn seals(&self) -> u32 {
        self.shared.lock().seals
    }

    /// Maps `len` bytes from `offset` (`mmap`) with `prot`: the memory
    /// itself if `shared` (the file grows to cover it, as its seals allow,
    /// past its size if need be), a copy of it otherwise. `place` holds the
    /// kernel's placement flags; `writable` whether the description may
    /// write.
    pub fn map(
        &self,
        offset: u64,
        len: usize,
        hint: usize,
        place: usize,
        prot: u32,
        shared: bool,
        writable: bool,
    ) -> SysResult {
        let end = offset.checked_add(len as u64).ok_or(ENOMEM)?;
        let flags = map_prot(prot) | place;
        if shared {
            if prot & PROT_WRITE != 0 && !writable {
                return Err(EACCES);
            }
            if prot & PROT_EXEC != 0 && self.seals() & seal::EXEC != 0 {
                return Err(EPERM);
            }
            self.reserve(end.next_multiple_of(PAGE))?;
            return vm::map(None, &self.vmo, offset as usize, len, hint, flags).map_err(map_error);
        }
        // A private copy of what the file holds there; past its end, zeros.
        let copy = Vmo::create(len).map_err(map_error)?;
        let mut chunk = vec![0u8; len.min(64 * 1024)];
        let size = self.size();
        let mut done = 0u64;
        while offset + done < size.min(end) {
            let n = (size.min(end) - offset - done).min(chunk.len() as u64) as usize;
            self.vmo.read((offset + done) as usize, &mut chunk[..n]).map_err(error::kernel)?;
            copy.write(done as usize, &chunk[..n]).map_err(error::kernel)?;
            done += n as u64;
        }
        vm::map(None, &copy, 0, len, hint, flags).map_err(map_error)
    }

    /// What `fstat` says of it.
    pub fn stat(&self) -> linux::Stat {
        let size = self.size();
        linux::Stat {
            ino: self.koid,
            nlink: 1,
            mode: linux::mode::IFREG | 0o600,
            uid: crate::vfs::UID,
            gid: crate::vfs::UID,
            size: size as i64,
            blksize: PAGE as i64,
            blocks: size.div_ceil(512) as i64,
            ..Default::default()
        }
    }
}

/// `memfd_create(name, flags)`.
///
/// # Safety
/// `name` must be the program's string.
pub unsafe fn memfd_create(name: usize, flags: u32) -> SysResult {
    // SAFETY: per the caller.
    let name = unsafe { crate::user::cstr(name)? };
    let file = Memfd::create(name, flags)?;
    let desc = crate::fd::Description::new(crate::fd::Object::Memfd(file), crate::linux::o::RDWR);
    crate::fd::insert(desc, flags & mfd::CLOEXEC != 0, 0).map(|n| n as usize)
}

/// A handle to `vmo` without `rights`.
fn without(vmo: &Vmo, rights: Rights) -> Result<Vmo, isize> {
    let have = vmo.0.basic_info().map_err(error::kernel)?.rights;
    vmo.0.duplicate(Some(Rights(have & !rights.0))).map(Vmo::from_handle).map_err(error::kernel)
}

/// The kernel's mapping flags for POSIX protections, as for every mapping.
fn map_prot(prot: u32) -> usize {
    crate::mem::map_prot(prot)
}

fn map_error(e: abi::Error) -> isize {
    match e {
        abi::Error::AccessDenied => EACCES,
        abi::Error::NoMemory | abi::Error::OutOfRange => ENOMEM,
        abi::Error::AlreadyExists => EEXIST,
        e => error::kernel(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_are_checked() {
        // HUGETLB, both exec choices, and unknown bits are refused.
        for flags in [mfd::HUGETLB, mfd::NOEXEC_SEAL | mfd::EXEC, 1 << 20] {
            assert_eq!(Memfd::create(b"x", flags).err(), Some(EINVAL), "{flags:#x}");
        }
        assert_eq!(Memfd::create(&[b'n'; MAX_NAME + 1], 0).err(), Some(EINVAL));
    }
}
