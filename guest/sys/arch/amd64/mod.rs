//! Linux's system calls on x86-64: the numbers of those the driver VM's
//! programs make (`arch/x86/entry/syscalls/syscall_64.tbl`), and the
//! `syscall` instruction.

/// System call numbers.
pub mod nr {
    pub const READ: usize = 0;
    pub const WRITE: usize = 1;
    pub const MMAP: usize = 9;
    pub const MUNMAP: usize = 11;
    pub const IOCTL: usize = 16;
    pub const SOCKET: usize = 41;
    pub const BIND: usize = 49;
    pub const SETSOCKOPT: usize = 54;
    pub const KILL: usize = 62;
    pub const STATFS: usize = 137;
    pub const MOUNT: usize = 165;
    pub const UMOUNT2: usize = 166;
    pub const REBOOT: usize = 169;
    pub const CLOCK_GETTIME: usize = 228;
    pub const PPOLL: usize = 271;
}

/// System call `nr` with arguments `a`: its result, or `-errno`.
///
/// # Safety
/// The arguments must be what the system call takes (pointers to memory
/// it may read or write as it does).
pub unsafe fn syscall(nr: usize, a: [usize; 6]) -> isize {
    let ret: isize;
    // SAFETY: the caller passes what the system call takes; the kernel
    // clobbers rcx and r11.
    unsafe {
        std::arch::asm!(
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
