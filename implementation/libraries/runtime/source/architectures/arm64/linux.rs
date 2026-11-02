// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Linux's system calls on AArch64, for programs of the driver VM's guest
//! (`crate::guest`): the numbers of those the runtime makes, and the
//! `svc` instruction.

use core::arch::asm;

/// System call numbers (`include/uapi/asm-generic/unistd.h`).
pub mod nr {
    pub const IOCTL: usize = 29;
    pub const OPENAT: usize = 56;
    pub const CLOSE: usize = 57;
    pub const WRITE: usize = 64;
    pub const EXIT_GROUP: usize = 94;
    pub const FUTEX: usize = 98;
    pub const NANOSLEEP: usize = 101;
    pub const SCHED_YIELD: usize = 124;
    pub const MUNMAP: usize = 215;
    pub const MMAP: usize = 222;
    pub const GETRANDOM: usize = 278;
}

/// Linux system call `nr` with arguments `a`: its result, or `-errno`.
///
/// # Safety
/// The arguments must be what the call takes (memory it reads or writes
/// included).
pub unsafe fn syscall(nr: usize, a: [usize; 6]) -> isize {
    let ret: isize;
    // SAFETY: forwarded to the caller; the kernel clobbers only x0.
    unsafe {
        asm!(
            "svc #0",
            in("x8") nr,
            inlateout("x0") a[0] as isize => ret,
            in("x1") a[1],
            in("x2") a[2],
            in("x3") a[3],
            in("x4") a[4],
            in("x5") a[5],
            options(nostack),
        );
    }
    ret
}
