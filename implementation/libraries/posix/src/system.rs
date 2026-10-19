// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The system: name, memory, limits, processors, randomness.

use core::sync::atomic::{AtomicU32, Ordering};

use crate::error::{self, SysResult};
use crate::linux::errno::{EFAULT, EINVAL, EPERM};
use crate::linux::{Rlimit, Sysinfo, Utsname, prctl, rlimit};
use crate::{fd, user};

/// The release `uname` reports.
const RELEASE: &str = env!("CARGO_PKG_VERSION");

fn field(s: &str) -> [u8; 65] {
    let mut f = [0u8; 65];
    let n = s.len().min(64);
    f[..n].copy_from_slice(&s.as_bytes()[..n]);
    f
}

pub unsafe fn uname(buf: usize) -> SysResult {
    let mut version = alloc::string::String::from("Veda ");
    version.push_str(RELEASE);
    let u = Utsname {
        sysname: field("Veda"),
        nodename: field("veda"),
        release: field(RELEASE),
        version: field(&version),
        machine: field(rt::arch::MACHINE),
        domainname: field("(none)"),
    };
    // SAFETY: the program passed a struct utsname.
    unsafe { user::write(buf, u)? };
    Ok(0)
}

/// `veda_process_memory()` (`<veda/memory.h>`): the bytes of memory this
/// process holds, as the kernel counts them: the pages committed to its
/// private memory and to the memory objects it maps, each once.
#[unsafe(no_mangle)]
pub extern "C" fn veda_process_memory() -> usize {
    rt::object::process_self_info().map_or(0, |i| i.memory_bytes as usize)
}

/// `veda_memory_file(fd, vmo)` (`<veda/memory.h>`): a handle to the memory
/// of the memory file on `fd`, with the descriptor's rights, written to
/// `vmo`. EBADF for no descriptor, EINVAL for one of anything else.
///
/// # Safety
/// `vmo` has room for a handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn veda_memory_file(fd: core::ffi::c_int, vmo: *mut abi::RawHandle) -> core::ffi::c_int {
    let handle = fd::get(fd).and_then(|desc| match &desc.object {
        fd::Object::Memfd(m) => m.memory(),
        _ => Err(EINVAL),
    });
    match handle {
        Ok(_) if vmo.is_null() => EFAULT as core::ffi::c_int,
        Ok(h) => {
            // SAFETY: as the caller promises; the handle is the caller's now.
            unsafe { vmo.write(h.0.into_raw()) };
            0
        }
        Err(e) => e as core::ffi::c_int,
    }
}

pub unsafe fn sysinfo(buf: usize) -> SysResult {
    let i = rt::object::system_info().unwrap_or_default();
    let s = Sysinfo {
        uptime: (i.uptime_ns / 1_000_000_000) as i64,
        totalram: i.total_memory,
        freeram: i.free_memory,
        procs: i.process_count.min(u16::MAX as u32) as u16,
        mem_unit: 1,
        ..Default::default()
    };
    // SAFETY: the program passed a struct sysinfo.
    unsafe { user::write(buf, s)? };
    Ok(0)
}

/// The limits: what Linux gives a process by default, the stack being the
/// size Veda gives the main thread.
fn limit(resource: u32) -> Result<Rlimit, isize> {
    let unlimited = Rlimit { cur: rlimit::INFINITY, max: rlimit::INFINITY };
    Ok(match resource {
        rlimit::STACK => {
            let s = rt::process::POSIX_STACK as u64;
            Rlimit { cur: s, max: s }
        }
        rlimit::NOFILE => Rlimit { cur: fd::MAX_FDS as u64, max: fd::MAX_FDS as u64 },
        rlimit::CORE => Rlimit { cur: 0, max: 0 },
        0..16 => unlimited,
        _ => return Err(EINVAL),
    })
}

/// `prlimit64` (and `getrlimit`, `setrlimit`) on this process. Limits can
/// be lowered in name only; they are not enforced.
pub unsafe fn prlimit(pid: i32, resource: u32, new: usize, old: usize) -> SysResult {
    if pid != 0 && pid != crate::process::pid() {
        return Err(EPERM);
    }
    let current = limit(resource)?;
    if new != 0 {
        // SAFETY: the program passed a struct rlimit.
        let n: Rlimit = unsafe { user::read(new)? };
        if n.cur > n.max || n.max > current.max {
            return Err(if n.cur > n.max { EINVAL } else { EPERM });
        }
    }
    if old != 0 {
        // SAFETY: as above.
        unsafe { user::write(old, current)? };
    }
    Ok(0)
}

/// `getrandom`: the kernel's random bytes, all of them or an error (a
/// buffer is never left part random, part not). The kernel's generator is
/// seeded before any program runs, so the flags (`GRND_NONBLOCK`,
/// `GRND_RANDOM`, `GRND_INSECURE`) change nothing.
pub unsafe fn getrandom(buf: usize, len: usize, flags: u32) -> SysResult {
    const KNOWN: u32 = 1 | 2 | 4;
    if flags & !KNOWN != 0 {
        return Err(EINVAL);
    }
    // SAFETY: the program passed `len` bytes.
    let out = unsafe { user::slice_mut(buf, len)? };
    rt::object::try_random_bytes(out).map_err(error::kernel)?;
    Ok(len)
}

/// `sched_getaffinity`: every processor.
pub unsafe fn sched_getaffinity(size: usize, mask: usize) -> SysResult {
    let cpus = rt::object::system_info().map_or(1, |i| i.cpu_count.max(1)) as usize;
    let bytes = cpus.div_ceil(64) * 8;
    if size < bytes {
        return Err(EINVAL);
    }
    // SAFETY: the program passed `size` bytes.
    let out = unsafe { user::slice_mut(mask, bytes)? };
    out.fill(0);
    for c in 0..cpus {
        out[c / 8] |= 1 << (c % 8);
    }
    Ok(bytes)
}

static UMASK: AtomicU32 = AtomicU32::new(0o022);

/// `umask`: kept for the program to read back (there are no permission
/// bits to apply it to).
pub fn umask(mask: u32) -> SysResult {
    Ok(UMASK.swap(mask & 0o777, Ordering::Relaxed) as usize)
}

pub unsafe fn prctl(option: u32, arg: usize) -> SysResult {
    match option {
        prctl::SET_NAME => Ok(0),
        prctl::GET_NAME => {
            let info = rt::object::process_self_info().unwrap_or_default();
            let name = info.name();
            let mut out = [0u8; 16];
            let n = name.len().min(15);
            out[..n].copy_from_slice(&name.as_bytes()[..n]);
            // SAFETY: the program passed 16 bytes.
            unsafe { user::write(arg, out)? };
            Ok(0)
        }
        _ => Err(EINVAL),
    }
}
