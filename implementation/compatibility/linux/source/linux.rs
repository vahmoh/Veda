// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What Linux's ABI says that Linux compatibility uses of its own: the
//! numbers its system calls carry (beyond those the POSIX layer carries
//! out), its signals and their `si_code`s, and the flags of the calls it
//! carries out itself. The numbers are Linux's headers', through
//! `linux_raw_sys`.

use linux_raw_sys::general as g;

/// `errno` values, as system calls return them (negated).
pub mod errno {
    use linux_raw_sys::errno as e;

    pub const EPERM: isize = e::EPERM as isize;
    pub const ENOENT: isize = e::ENOENT as isize;
    pub const ESRCH: isize = e::ESRCH as isize;
    pub const EINTR: isize = e::EINTR as isize;
    pub const ENOEXEC: isize = e::ENOEXEC as isize;
    pub const EBADF: isize = e::EBADF as isize;
    pub const EAGAIN: isize = e::EAGAIN as isize;
    pub const ENOMEM: isize = e::ENOMEM as isize;
    pub const EACCES: isize = e::EACCES as isize;
    pub const EFAULT: isize = e::EFAULT as isize;
    pub const EEXIST: isize = e::EEXIST as isize;
    pub const ENOTDIR: isize = e::ENOTDIR as isize;
    pub const EISDIR: isize = e::EISDIR as isize;
    pub const EINVAL: isize = e::EINVAL as isize;
    pub const ENOSYS: isize = e::ENOSYS as isize;
    pub const ERANGE: isize = e::ERANGE as isize;
    pub const ENAMETOOLONG: isize = e::ENAMETOOLONG as isize;
    pub const EROFS: isize = e::EROFS as isize;
    pub const EOPNOTSUPP: isize = e::EOPNOTSUPP as isize;
    /// The kernel's own: a blocking call a signal ended, which starts again
    /// after its handler if that had `SA_RESTART` (`ERESTARTSYS`), always
    /// (`ERESTARTNOINTR`), or only if no handler runs (`ERESTARTNOHAND`).
    pub const ERESTARTSYS: isize = 512;
    pub const ERESTARTNOINTR: isize = 513;
    pub const ERESTARTNOHAND: isize = 514;
}

/// Signals, their handlers' flags and the signal stack's.
pub mod sig {
    use linux_raw_sys::general as g;

    pub const HUP: u32 = g::SIGHUP;
    pub const INT: u32 = g::SIGINT;
    pub const QUIT: u32 = g::SIGQUIT;
    pub const ILL: u32 = g::SIGILL;
    pub const TRAP: u32 = g::SIGTRAP;
    pub const ABRT: u32 = g::SIGABRT;
    pub const BUS: u32 = g::SIGBUS;
    pub const FPE: u32 = g::SIGFPE;
    pub const KILL: u32 = g::SIGKILL;
    pub const SEGV: u32 = g::SIGSEGV;
    pub const PIPE: u32 = g::SIGPIPE;
    pub const TERM: u32 = g::SIGTERM;
    pub const CHLD: u32 = g::SIGCHLD;
    pub const STOP: u32 = g::SIGSTOP;
    pub const SYS: u32 = g::SIGSYS;
    pub const XCPU: u32 = g::SIGXCPU;
    pub const XFSZ: u32 = g::SIGXFSZ;
    /// Signals are 1 to 64.
    pub const MAX: u32 = 64;

    pub const DFL: usize = 0;
    pub const IGN: usize = 1;

    pub const SA_SIGINFO: u64 = g::SA_SIGINFO as u64;
    pub const SA_ONSTACK: u64 = g::SA_ONSTACK as u64;
    pub const SA_RESTART: u64 = g::SA_RESTART as u64;
    pub const SA_RESTORER: u64 = g::SA_RESTORER as u64;

    pub const SS_ONSTACK: i32 = g::SS_ONSTACK as i32;
    pub const SS_DISABLE: i32 = g::SS_DISABLE as i32;
    pub const SS_AUTODISARM: i32 = g::SS_AUTODISARM as i32;
    /// The smallest signal stack Linux takes (`MINSIGSTKSZ`).
    pub const MINSIGSTKSZ: u64 = g::MINSIGSTKSZ as u64;

    /// Whether signal `s` dumps core as it ends a process by default (what
    /// a parent's wait status says of it).
    pub fn dumps_core(s: u32) -> bool {
        matches!(s, QUIT | ILL | TRAP | ABRT | BUS | FPE | SEGV | SYS | XCPU | XFSZ)
    }
}

/// `si_code`s of the signals faults raise, and the kernel's own.
pub mod si {
    use linux_raw_sys::general as g;

    pub const KERNEL: i32 = g::SI_KERNEL as i32;
    pub const ILL_ILLOPC: i32 = g::ILL_ILLOPC as i32;
    pub const ILL_ILLOPN: i32 = g::ILL_ILLOPN as i32;
    pub const FPE_INTDIV: i32 = g::FPE_INTDIV as i32;
    pub const FPE_FLTDIV: i32 = g::FPE_FLTDIV as i32;
    pub const FPE_FLTOVF: i32 = g::FPE_FLTOVF as i32;
    pub const FPE_FLTUND: i32 = g::FPE_FLTUND as i32;
    pub const FPE_FLTRES: i32 = g::FPE_FLTRES as i32;
    pub const FPE_FLTINV: i32 = g::FPE_FLTINV as i32;
    pub const FPE_FLTUNK: i32 = g::FPE_FLTUNK as i32;
    pub const SEGV_MAPERR: i32 = g::SEGV_MAPERR as i32;
    pub const SEGV_ACCERR: i32 = g::SEGV_ACCERR as i32;
    pub const SEGV_CPERR: i32 = g::SEGV_CPERR as i32;
    pub const SEGV_MTESERR: i32 = g::SEGV_MTESERR as i32;
    pub const BUS_ADRALN: i32 = g::BUS_ADRALN as i32;
    pub const BUS_OBJERR: i32 = g::BUS_OBJERR as i32;
    pub const TRAP_BRKPT: i32 = g::TRAP_BRKPT as i32;
    pub const TRAP_TRACE: i32 = g::TRAP_TRACE as i32;
    pub const TRAP_HWBKPT: i32 = g::TRAP_HWBKPT as i32;
}

/// `clone`'s flags.
pub mod clone {
    use linux_raw_sys::general as g;

    pub const VM: u64 = g::CLONE_VM as u64;
    pub const FS: u64 = g::CLONE_FS as u64;
    pub const FILES: u64 = g::CLONE_FILES as u64;
    pub const SIGHAND: u64 = g::CLONE_SIGHAND as u64;
    pub const PIDFD: u64 = g::CLONE_PIDFD as u64;
    pub const VFORK: u64 = g::CLONE_VFORK as u64;
    pub const THREAD: u64 = g::CLONE_THREAD as u64;
    pub const SYSVSEM: u64 = g::CLONE_SYSVSEM as u64;
    pub const SETTLS: u64 = g::CLONE_SETTLS as u64;
    pub const PARENT_SETTID: u64 = g::CLONE_PARENT_SETTID as u64;
    pub const CHILD_CLEARTID: u64 = g::CLONE_CHILD_CLEARTID as u64;
    pub const DETACHED: u64 = g::CLONE_DETACHED as u64;
    pub const CHILD_SETTID: u64 = g::CLONE_CHILD_SETTID as u64;
    /// The signal a child's end sends its parent: the low byte.
    pub const SIGNAL: u64 = 0xff;
}

/// `mmap`'s and `mprotect`'s flags, and `madvise`'s advice.
pub mod mman {
    use linux_raw_sys::general as g;

    pub const PROT_READ: u32 = g::PROT_READ;
    pub const PROT_WRITE: u32 = g::PROT_WRITE;
    pub const PROT_EXEC: u32 = g::PROT_EXEC;
    pub const PROT_GROWSDOWN: u32 = g::PROT_GROWSDOWN;
    pub const PROT_GROWSUP: u32 = g::PROT_GROWSUP;
    pub const MAP_SHARED: u32 = g::MAP_SHARED;
    pub const MAP_PRIVATE: u32 = g::MAP_PRIVATE;
    pub const MAP_SHARED_VALIDATE: u32 = g::MAP_SHARED_VALIDATE;
    pub const MAP_TYPE: u32 = 0x0f;
    pub const MAP_FIXED: u32 = g::MAP_FIXED;
    pub const MAP_ANONYMOUS: u32 = g::MAP_ANONYMOUS;
    pub const MAP_32BIT: u32 = 0x40;
    pub const MAP_FIXED_NOREPLACE: u32 = g::MAP_FIXED_NOREPLACE;
    pub const MAP_HUGETLB: u32 = g::MAP_HUGETLB;
    pub const MREMAP_MAYMOVE: u32 = g::MREMAP_MAYMOVE;
    pub const MREMAP_FIXED: u32 = g::MREMAP_FIXED;
    pub const MREMAP_DONTUNMAP: u32 = g::MREMAP_DONTUNMAP;
    pub const MADV_DONTFORK: u32 = g::MADV_DONTFORK;
    pub const MADV_DOFORK: u32 = g::MADV_DOFORK;
    pub const MADV_WIPEONFORK: u32 = g::MADV_WIPEONFORK;
    pub const MADV_KEEPONFORK: u32 = g::MADV_KEEPONFORK;
}

/// Call numbers of Linux's system calls that Linux compatibility carries
/// out itself, of the architecture's table.
pub mod nr {
    use linux_raw_sys::general as g;

    pub const MMAP: usize = g::__NR_mmap as usize;
    pub const MPROTECT: usize = g::__NR_mprotect as usize;
    pub const MUNMAP: usize = g::__NR_munmap as usize;
    pub const MREMAP: usize = g::__NR_mremap as usize;
    pub const MADVISE: usize = g::__NR_madvise as usize;
    pub const BRK: usize = g::__NR_brk as usize;
    pub const RT_SIGRETURN: usize = g::__NR_rt_sigreturn as usize;
    pub const SIGALTSTACK: usize = g::__NR_sigaltstack as usize;
    pub const CLONE: usize = g::__NR_clone as usize;
    pub const CLONE3: usize = g::__NR_clone3 as usize;
    pub const EXECVE: usize = g::__NR_execve as usize;
    pub const EXECVEAT: usize = g::__NR_execveat as usize;
    pub const EXIT: usize = g::__NR_exit as usize;
    pub const EXIT_GROUP: usize = g::__NR_exit_group as usize;
    pub const UNAME: usize = g::__NR_uname as usize;
    pub const SET_ROBUST_LIST: usize = g::__NR_set_robust_list as usize;
    pub const GET_ROBUST_LIST: usize = g::__NR_get_robust_list as usize;
    pub const PERSONALITY: usize = g::__NR_personality as usize;
    pub const PTRACE: usize = g::__NR_ptrace as usize;
    pub const CAPGET: usize = g::__NR_capget as usize;
    pub const CAPSET: usize = g::__NR_capset as usize;
    pub const OPENAT: usize = g::__NR_openat as usize;
    pub const NEWFSTATAT: usize = g::__NR_newfstatat as usize;
    pub const STATX: usize = g::__NR_statx as usize;
    pub const FACCESSAT: usize = g::__NR_faccessat as usize;
    pub const FACCESSAT2: usize = g::__NR_faccessat2 as usize;
    pub const READLINKAT: usize = g::__NR_readlinkat as usize;
    pub const PRCTL: usize = g::__NR_prctl as usize;
    pub const SECCOMP: usize = g::__NR_seccomp as usize;
    pub const MOUNT: usize = g::__NR_mount as usize;
    pub const UMOUNT2: usize = g::__NR_umount2 as usize;
    pub const CHROOT: usize = g::__NR_chroot as usize;
    pub const PIVOT_ROOT: usize = g::__NR_pivot_root as usize;
    pub const UNSHARE: usize = g::__NR_unshare as usize;
    pub const SETNS: usize = g::__NR_setns as usize;
    pub const REBOOT: usize = g::__NR_reboot as usize;
    pub const INIT_MODULE: usize = g::__NR_init_module as usize;
    pub const DELETE_MODULE: usize = g::__NR_delete_module as usize;
    pub const SWAPON: usize = g::__NR_swapon as usize;
    pub const SWAPOFF: usize = g::__NR_swapoff as usize;
    pub const SETTIMEOFDAY: usize = g::__NR_settimeofday as usize;
    pub const CLOCK_SETTIME: usize = g::__NR_clock_settime as usize;
    pub const IO_URING_SETUP: usize = g::__NR_io_uring_setup as usize;
    pub const SENDFILE: usize = g::__NR_sendfile as usize;
    pub const COPY_FILE_RANGE: usize = g::__NR_copy_file_range as usize;
    pub const READ: usize = g::__NR_read as usize;
    pub const WRITE: usize = g::__NR_write as usize;
    pub const PREAD64: usize = g::__NR_pread64 as usize;
    pub const PWRITE64: usize = g::__NR_pwrite64 as usize;
    pub const LSEEK: usize = g::__NR_lseek as usize;
    pub const FSTAT: usize = g::__NR_fstat as usize;
    pub const FCNTL: usize = g::__NR_fcntl as usize;

    // The x86-64 table's older forms of calls the generic one has.
    #[cfg(target_arch = "x86_64")]
    pub use x86_64::*;
    #[cfg(target_arch = "x86_64")]
    mod x86_64 {
        use linux_raw_sys::general as g;

        pub const ARCH_PRCTL: usize = g::__NR_arch_prctl as usize;
        pub const OPEN: usize = g::__NR_open as usize;
        pub const STAT: usize = g::__NR_stat as usize;
        pub const LSTAT: usize = g::__NR_lstat as usize;
        pub const ACCESS: usize = g::__NR_access as usize;
        pub const READLINK: usize = g::__NR_readlink as usize;
        pub const FORK: usize = g::__NR_fork as usize;
        pub const VFORK: usize = g::__NR_vfork as usize;
    }
}

/// `prctl`'s options that Linux compatibility answers itself.
pub mod pr {
    pub const SET_PDEATHSIG: u32 = 1;
    pub const GET_PDEATHSIG: u32 = 2;
    pub const GET_DUMPABLE: u32 = 3;
    pub const SET_DUMPABLE: u32 = 4;
    pub const GET_SECCOMP: u32 = 21;
    pub const SET_SECCOMP: u32 = 22;
    pub const CAPBSET_READ: u32 = 23;
    pub const SET_CHILD_SUBREAPER: u32 = 36;
    pub const GET_CHILD_SUBREAPER: u32 = 37;
    pub const SET_NO_NEW_PRIVS: u32 = 38;
    pub const GET_NO_NEW_PRIVS: u32 = 39;
    pub const SET_THP_DISABLE: u32 = 41;
    pub const GET_THP_DISABLE: u32 = 42;
    pub const SET_PTRACER: u32 = 0x5961_6d61;
}

/// `AT_` flags of the calls that take a directory and a name.
pub mod at {
    use linux_raw_sys::general as g;

    pub const FDCWD: i32 = g::AT_FDCWD;
    pub const SYMLINK_NOFOLLOW: u32 = g::AT_SYMLINK_NOFOLLOW;
    pub const EMPTY_PATH: u32 = g::AT_EMPTY_PATH;
}

/// File types and permissions of `st_mode`.
pub mod mode {
    use linux_raw_sys::general as g;

    pub const S_IFREG: u32 = g::S_IFREG;
    pub const S_IFDIR: u32 = g::S_IFDIR;
    pub const S_IFLNK: u32 = g::S_IFLNK;
}

/// `open`'s flags.
pub mod o {
    use linux_raw_sys::general as g;

    pub const ACCMODE: u32 = g::O_ACCMODE;
    pub const RDONLY: u32 = g::O_RDONLY;
    pub const CLOEXEC: u32 = g::O_CLOEXEC;
    pub const DIRECTORY: u32 = g::O_DIRECTORY;
    pub const NOFOLLOW: u32 = g::O_NOFOLLOW;
    pub const PATH: u32 = g::O_PATH;
}

/// The personality of a Linux process: Linux's own (`PER_LINUX`), with no
/// flags (`personality(0xffffffff)` asks for it).
pub const PER_LINUX: u32 = 0;
/// `personality`'s argument that asks without changing.
pub const PERSONALITY_QUERY: u32 = 0xffff_ffff;

/// The capabilities' version Linux compatibility answers `capget` with
/// (`_LINUX_CAPABILITY_VERSION_3`).
pub const CAPABILITY_VERSION_3: u32 = g::_LINUX_CAPABILITY_VERSION_3;
