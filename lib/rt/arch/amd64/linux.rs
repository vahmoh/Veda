//! Linux's system calls on x86-64, for programs of the driver VM's guest
//! (`crate::guest`): the numbers of those the runtime makes, and the
//! `syscall` instruction.

use core::arch::asm;

/// System call numbers (`arch/x86/entry/syscalls/syscall_64.tbl`).
pub mod nr {
    pub const WRITE: usize = 1;
    pub const CLOSE: usize = 3;
    pub const MMAP: usize = 9;
    pub const MUNMAP: usize = 11;
    pub const IOCTL: usize = 16;
    pub const SCHED_YIELD: usize = 24;
    pub const NANOSLEEP: usize = 35;
    pub const FUTEX: usize = 202;
    pub const EXIT_GROUP: usize = 231;
    pub const OPENAT: usize = 257;
    pub const GETRANDOM: usize = 318;
}

/// Linux system call `nr` with arguments `a`: its result, or `-errno`.
///
/// # Safety
/// The arguments must be what the call takes (memory it reads or writes
/// included).
pub unsafe fn syscall(nr: usize, a: [usize; 6]) -> isize {
    let ret: isize;
    // SAFETY: forwarded to the caller; the kernel clobbers rcx and r11.
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") nr as isize => ret,
            in("rdi") a[0],
            in("rsi") a[1],
            in("rdx") a[2],
            in("r10") a[3],
            in("r8") a[4],
            in("r9") a[5],
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    ret
}
