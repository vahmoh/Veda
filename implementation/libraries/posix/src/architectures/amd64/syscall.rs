// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The system calls of Linux's x86-64 ABI that its generic one (which newer
//! architectures have) does without: older forms of calls it has (`open`
//! for `openat`, `pipe` for `pipe2`, ...), and `arch_prctl`.

use abi::nr as veda;
use rt::sys::call;

use super::linux::{arch_prctl as prctl, nr};
use crate::error::{self, SysResult};
use crate::linux::errno::*;
use crate::linux::{Timeval, at, o};
use crate::{epoll, eventfd, fs, io, process, signal, time, user};

/// Runs Linux system call `n` if it is one of x86-64's own; `None` for the
/// others.
///
/// # Safety
/// The arguments are the program's: pointers must be valid for the call.
pub unsafe fn dispatch(n: usize, a: [usize; 6]) -> Option<SysResult> {
    let fd0 = a[0] as i32;
    // SAFETY (whole match): the arguments are passed on as the program gave
    // them, each call documenting what it requires of them.
    Some(unsafe {
        match n {
            nr::OPEN => fs::openat(at::FDCWD, a[0], a[1] as u32),
            nr::CREAT => fs::openat(at::FDCWD, a[0], o::CREAT | o::WRONLY | o::TRUNC),
            nr::STAT => fs::fstatat(at::FDCWD, a[0], a[1], 0),
            nr::LSTAT => fs::fstatat(at::FDCWD, a[0], a[1], at::SYMLINK_NOFOLLOW),
            nr::POLL => {
                let ms = a[2] as i32;
                let deadline = (ms >= 0).then(|| time::monotonic_ns().saturating_add(ms as u64 * 1_000_000));
                io::poll(a[0], a[1], deadline)
            }
            nr::SELECT => match deadline_tv(a[4]) {
                Ok(deadline) => io::select(fd0, a[1], a[2], a[3], deadline),
                Err(e) => Err(e),
            },
            nr::ACCESS => fs::faccessat(at::FDCWD, a[0], a[1] as u32),
            nr::PIPE => io::pipe2(a[0], 0),
            nr::DUP2 => io::dup3(fd0, a[1] as i32, 0, true),
            nr::PAUSE => signal::pause(),
            nr::GETPGRP => Ok(process::pid() as usize),
            // Processes are made from program images (posix_spawn), not
            // by copying this one.
            nr::FORK | nr::VFORK => Err(ENOSYS),
            nr::RENAME => fs::renameat(at::FDCWD, a[0], at::FDCWD, a[1], 0),
            nr::MKDIR => fs::mkdirat(at::FDCWD, a[0]),
            nr::RMDIR => fs::unlinkat(at::FDCWD, a[0], at::REMOVEDIR),
            nr::UNLINK => fs::unlinkat(at::FDCWD, a[0], 0),
            // Veda has neither hard nor symbolic links.
            nr::LINK | nr::SYMLINK | nr::MKNOD => Err(EPERM),
            nr::READLINK => fs::readlinkat(at::FDCWD, a[0]),
            nr::CHMOD | nr::CHOWN | nr::LCHOWN => fs::chmodat(at::FDCWD, a[0]),
            nr::TIME => time::time(a[0]),
            nr::ARCH_PRCTL => arch_prctl(a[0] as u32, a[1]),
            // The size is a hint, which only has to be positive.
            nr::EPOLL_CREATE if (a[0] as i32) > 0 => epoll::epoll_create1(0),
            nr::EPOLL_CREATE => Err(EINVAL),
            nr::EPOLL_WAIT => epoll::epoll_wait(fd0, a[1], a[2] as i32, epoll::deadline_ms(a[3] as i32)),
            nr::EVENTFD => eventfd::eventfd2(a[0] as u32, 0),
            _ => return None,
        }
    })
}

/// `arch_prctl`: only the thread pointer (the FS base) can be set.
fn arch_prctl(code: u32, addr: usize) -> SysResult {
    match code {
        prctl::SET_FS => call(veda::THREAD_SET_TLS, [addr, 0, 0, 0, 0, 0]).map(|_| 0).map_err(error::kernel),
        _ => Err(EINVAL),
    }
}

/// Deadline (monotonic ns) of a relative timeout given as a `timeval`,
/// `None` for none.
unsafe fn deadline_tv(tv: usize) -> Result<Option<u64>, isize> {
    if tv == 0 {
        return Ok(None);
    }
    // SAFETY: the program passed a timeval.
    let t: Timeval = unsafe { user::read(tv)? };
    if t.sec < 0 || !(0..1_000_000).contains(&t.usec) {
        return Err(EINVAL);
    }
    let ns = (t.sec as u64).saturating_mul(1_000_000_000).saturating_add(t.usec as u64 * 1000);
    Ok(Some(time::monotonic_ns().saturating_add(ns)))
}
