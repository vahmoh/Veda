// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What Linux's ABI has particular to x86-64, as musl uses it: the system
//! call numbers, `struct stat`, `struct epoll_event`, and `arch_prctl`'s
//! codes.

use crate::linux::Timespec;

/// System call numbers (x86-64's, as musl's `arch/x86_64/bits/syscall.h`
/// has them).
pub mod nr {
    pub const READ: usize = linux_raw_sys::general::__NR_read as usize;
    pub const WRITE: usize = linux_raw_sys::general::__NR_write as usize;
    pub const OPEN: usize = linux_raw_sys::general::__NR_open as usize;
    pub const CLOSE: usize = linux_raw_sys::general::__NR_close as usize;
    pub const STAT: usize = linux_raw_sys::general::__NR_stat as usize;
    pub const FSTAT: usize = linux_raw_sys::general::__NR_fstat as usize;
    pub const LSTAT: usize = linux_raw_sys::general::__NR_lstat as usize;
    pub const POLL: usize = linux_raw_sys::general::__NR_poll as usize;
    pub const LSEEK: usize = linux_raw_sys::general::__NR_lseek as usize;
    pub const MMAP: usize = linux_raw_sys::general::__NR_mmap as usize;
    pub const MPROTECT: usize = linux_raw_sys::general::__NR_mprotect as usize;
    pub const MUNMAP: usize = linux_raw_sys::general::__NR_munmap as usize;
    pub const BRK: usize = linux_raw_sys::general::__NR_brk as usize;
    pub const RT_SIGACTION: usize = linux_raw_sys::general::__NR_rt_sigaction as usize;
    pub const RT_SIGPROCMASK: usize = linux_raw_sys::general::__NR_rt_sigprocmask as usize;
    pub const IOCTL: usize = linux_raw_sys::general::__NR_ioctl as usize;
    pub const PREAD64: usize = linux_raw_sys::general::__NR_pread64 as usize;
    pub const PWRITE64: usize = linux_raw_sys::general::__NR_pwrite64 as usize;
    pub const READV: usize = linux_raw_sys::general::__NR_readv as usize;
    pub const WRITEV: usize = linux_raw_sys::general::__NR_writev as usize;
    pub const ACCESS: usize = linux_raw_sys::general::__NR_access as usize;
    pub const PIPE: usize = linux_raw_sys::general::__NR_pipe as usize;
    pub const SELECT: usize = linux_raw_sys::general::__NR_select as usize;
    pub const SCHED_YIELD: usize = linux_raw_sys::general::__NR_sched_yield as usize;
    pub const MREMAP: usize = linux_raw_sys::general::__NR_mremap as usize;
    pub const MSYNC: usize = linux_raw_sys::general::__NR_msync as usize;
    pub const MINCORE: usize = linux_raw_sys::general::__NR_mincore as usize;
    pub const MADVISE: usize = linux_raw_sys::general::__NR_madvise as usize;
    pub const DUP: usize = linux_raw_sys::general::__NR_dup as usize;
    pub const DUP2: usize = linux_raw_sys::general::__NR_dup2 as usize;
    pub const PAUSE: usize = linux_raw_sys::general::__NR_pause as usize;
    pub const NANOSLEEP: usize = linux_raw_sys::general::__NR_nanosleep as usize;
    pub const GETPID: usize = linux_raw_sys::general::__NR_getpid as usize;
    pub const SOCKET: usize = linux_raw_sys::general::__NR_socket as usize;
    pub const CONNECT: usize = linux_raw_sys::general::__NR_connect as usize;
    pub const ACCEPT: usize = linux_raw_sys::general::__NR_accept as usize;
    pub const SENDTO: usize = linux_raw_sys::general::__NR_sendto as usize;
    pub const RECVFROM: usize = linux_raw_sys::general::__NR_recvfrom as usize;
    pub const SENDMSG: usize = linux_raw_sys::general::__NR_sendmsg as usize;
    pub const RECVMSG: usize = linux_raw_sys::general::__NR_recvmsg as usize;
    pub const SHUTDOWN: usize = linux_raw_sys::general::__NR_shutdown as usize;
    pub const BIND: usize = linux_raw_sys::general::__NR_bind as usize;
    pub const LISTEN: usize = linux_raw_sys::general::__NR_listen as usize;
    pub const GETSOCKNAME: usize = linux_raw_sys::general::__NR_getsockname as usize;
    pub const GETPEERNAME: usize = linux_raw_sys::general::__NR_getpeername as usize;
    pub const SOCKETPAIR: usize = linux_raw_sys::general::__NR_socketpair as usize;
    pub const SETSOCKOPT: usize = linux_raw_sys::general::__NR_setsockopt as usize;
    pub const GETSOCKOPT: usize = linux_raw_sys::general::__NR_getsockopt as usize;
    pub const CLONE: usize = linux_raw_sys::general::__NR_clone as usize;
    pub const FORK: usize = linux_raw_sys::general::__NR_fork as usize;
    pub const VFORK: usize = linux_raw_sys::general::__NR_vfork as usize;
    pub const EXECVE: usize = linux_raw_sys::general::__NR_execve as usize;
    pub const EXIT: usize = linux_raw_sys::general::__NR_exit as usize;
    pub const WAIT4: usize = linux_raw_sys::general::__NR_wait4 as usize;
    pub const KILL: usize = linux_raw_sys::general::__NR_kill as usize;
    pub const UNAME: usize = linux_raw_sys::general::__NR_uname as usize;
    pub const FCNTL: usize = linux_raw_sys::general::__NR_fcntl as usize;
    pub const FLOCK: usize = linux_raw_sys::general::__NR_flock as usize;
    pub const FSYNC: usize = linux_raw_sys::general::__NR_fsync as usize;
    pub const FDATASYNC: usize = linux_raw_sys::general::__NR_fdatasync as usize;
    pub const TRUNCATE: usize = linux_raw_sys::general::__NR_truncate as usize;
    pub const FTRUNCATE: usize = linux_raw_sys::general::__NR_ftruncate as usize;
    pub const GETCWD: usize = linux_raw_sys::general::__NR_getcwd as usize;
    pub const CHDIR: usize = linux_raw_sys::general::__NR_chdir as usize;
    pub const FCHDIR: usize = linux_raw_sys::general::__NR_fchdir as usize;
    pub const RENAME: usize = linux_raw_sys::general::__NR_rename as usize;
    pub const MKDIR: usize = linux_raw_sys::general::__NR_mkdir as usize;
    pub const RMDIR: usize = linux_raw_sys::general::__NR_rmdir as usize;
    pub const CREAT: usize = linux_raw_sys::general::__NR_creat as usize;
    pub const LINK: usize = linux_raw_sys::general::__NR_link as usize;
    pub const UNLINK: usize = linux_raw_sys::general::__NR_unlink as usize;
    pub const SYMLINK: usize = linux_raw_sys::general::__NR_symlink as usize;
    pub const READLINK: usize = linux_raw_sys::general::__NR_readlink as usize;
    pub const CHMOD: usize = linux_raw_sys::general::__NR_chmod as usize;
    pub const FCHMOD: usize = linux_raw_sys::general::__NR_fchmod as usize;
    pub const CHOWN: usize = linux_raw_sys::general::__NR_chown as usize;
    pub const FCHOWN: usize = linux_raw_sys::general::__NR_fchown as usize;
    pub const LCHOWN: usize = linux_raw_sys::general::__NR_lchown as usize;
    pub const UMASK: usize = linux_raw_sys::general::__NR_umask as usize;
    pub const GETTIMEOFDAY: usize = linux_raw_sys::general::__NR_gettimeofday as usize;
    pub const GETRLIMIT: usize = linux_raw_sys::general::__NR_getrlimit as usize;
    pub const GETRUSAGE: usize = linux_raw_sys::general::__NR_getrusage as usize;
    pub const SYSINFO: usize = linux_raw_sys::general::__NR_sysinfo as usize;
    pub const TIMES: usize = linux_raw_sys::general::__NR_times as usize;
    pub const GETUID: usize = linux_raw_sys::general::__NR_getuid as usize;
    pub const GETGID: usize = linux_raw_sys::general::__NR_getgid as usize;
    pub const SETUID: usize = linux_raw_sys::general::__NR_setuid as usize;
    pub const SETGID: usize = linux_raw_sys::general::__NR_setgid as usize;
    pub const GETEUID: usize = linux_raw_sys::general::__NR_geteuid as usize;
    pub const GETEGID: usize = linux_raw_sys::general::__NR_getegid as usize;
    pub const SETPGID: usize = linux_raw_sys::general::__NR_setpgid as usize;
    pub const GETPPID: usize = linux_raw_sys::general::__NR_getppid as usize;
    pub const GETPGRP: usize = linux_raw_sys::general::__NR_getpgrp as usize;
    pub const SETSID: usize = linux_raw_sys::general::__NR_setsid as usize;
    pub const SETREUID: usize = linux_raw_sys::general::__NR_setreuid as usize;
    pub const SETREGID: usize = linux_raw_sys::general::__NR_setregid as usize;
    pub const GETGROUPS: usize = linux_raw_sys::general::__NR_getgroups as usize;
    pub const SETGROUPS: usize = linux_raw_sys::general::__NR_setgroups as usize;
    pub const SETRESUID: usize = linux_raw_sys::general::__NR_setresuid as usize;
    pub const GETRESUID: usize = linux_raw_sys::general::__NR_getresuid as usize;
    pub const SETRESGID: usize = linux_raw_sys::general::__NR_setresgid as usize;
    pub const GETRESGID: usize = linux_raw_sys::general::__NR_getresgid as usize;
    pub const GETPGID: usize = linux_raw_sys::general::__NR_getpgid as usize;
    pub const GETSID: usize = linux_raw_sys::general::__NR_getsid as usize;
    pub const RT_SIGPENDING: usize = linux_raw_sys::general::__NR_rt_sigpending as usize;
    pub const RT_SIGTIMEDWAIT: usize = linux_raw_sys::general::__NR_rt_sigtimedwait as usize;
    pub const RT_SIGSUSPEND: usize = linux_raw_sys::general::__NR_rt_sigsuspend as usize;
    pub const SIGALTSTACK: usize = linux_raw_sys::general::__NR_sigaltstack as usize;
    pub const MKNOD: usize = linux_raw_sys::general::__NR_mknod as usize;
    pub const STATFS: usize = linux_raw_sys::general::__NR_statfs as usize;
    pub const FSTATFS: usize = linux_raw_sys::general::__NR_fstatfs as usize;
    pub const GETPRIORITY: usize = linux_raw_sys::general::__NR_getpriority as usize;
    pub const SETPRIORITY: usize = linux_raw_sys::general::__NR_setpriority as usize;
    pub const SCHED_GETPARAM: usize = linux_raw_sys::general::__NR_sched_getparam as usize;
    pub const SCHED_SETSCHEDULER: usize = linux_raw_sys::general::__NR_sched_setscheduler as usize;
    pub const SCHED_GETSCHEDULER: usize = linux_raw_sys::general::__NR_sched_getscheduler as usize;
    pub const SCHED_GET_PRIORITY_MAX: usize = linux_raw_sys::general::__NR_sched_get_priority_max as usize;
    pub const SCHED_GET_PRIORITY_MIN: usize = linux_raw_sys::general::__NR_sched_get_priority_min as usize;
    pub const MLOCK: usize = linux_raw_sys::general::__NR_mlock as usize;
    pub const MUNLOCK: usize = linux_raw_sys::general::__NR_munlock as usize;
    pub const MLOCKALL: usize = linux_raw_sys::general::__NR_mlockall as usize;
    pub const MUNLOCKALL: usize = linux_raw_sys::general::__NR_munlockall as usize;
    pub const PRCTL: usize = linux_raw_sys::general::__NR_prctl as usize;
    pub const ARCH_PRCTL: usize = linux_raw_sys::general::__NR_arch_prctl as usize;
    pub const SETRLIMIT: usize = linux_raw_sys::general::__NR_setrlimit as usize;
    pub const SYNC: usize = linux_raw_sys::general::__NR_sync as usize;
    pub const GETTID: usize = linux_raw_sys::general::__NR_gettid as usize;
    pub const READAHEAD: usize = linux_raw_sys::general::__NR_readahead as usize;
    pub const TKILL: usize = linux_raw_sys::general::__NR_tkill as usize;
    pub const TIME: usize = linux_raw_sys::general::__NR_time as usize;
    pub const FUTEX: usize = linux_raw_sys::general::__NR_futex as usize;
    pub const SCHED_SETAFFINITY: usize = linux_raw_sys::general::__NR_sched_setaffinity as usize;
    pub const SCHED_GETAFFINITY: usize = linux_raw_sys::general::__NR_sched_getaffinity as usize;
    pub const EPOLL_CREATE: usize = linux_raw_sys::general::__NR_epoll_create as usize;
    pub const GETDENTS64: usize = linux_raw_sys::general::__NR_getdents64 as usize;
    pub const SET_TID_ADDRESS: usize = linux_raw_sys::general::__NR_set_tid_address as usize;
    pub const FADVISE64: usize = linux_raw_sys::general::__NR_fadvise64 as usize;
    pub const CLOCK_GETTIME: usize = linux_raw_sys::general::__NR_clock_gettime as usize;
    pub const CLOCK_GETRES: usize = linux_raw_sys::general::__NR_clock_getres as usize;
    pub const CLOCK_NANOSLEEP: usize = linux_raw_sys::general::__NR_clock_nanosleep as usize;
    pub const EXIT_GROUP: usize = linux_raw_sys::general::__NR_exit_group as usize;
    pub const EPOLL_WAIT: usize = linux_raw_sys::general::__NR_epoll_wait as usize;
    pub const EPOLL_CTL: usize = linux_raw_sys::general::__NR_epoll_ctl as usize;
    pub const TGKILL: usize = linux_raw_sys::general::__NR_tgkill as usize;
    pub const WAITID: usize = linux_raw_sys::general::__NR_waitid as usize;
    pub const OPENAT: usize = linux_raw_sys::general::__NR_openat as usize;
    pub const MKDIRAT: usize = linux_raw_sys::general::__NR_mkdirat as usize;
    pub const MKNODAT: usize = linux_raw_sys::general::__NR_mknodat as usize;
    pub const FCHOWNAT: usize = linux_raw_sys::general::__NR_fchownat as usize;
    pub const NEWFSTATAT: usize = linux_raw_sys::general::__NR_newfstatat as usize;
    pub const UNLINKAT: usize = linux_raw_sys::general::__NR_unlinkat as usize;
    pub const RENAMEAT: usize = linux_raw_sys::general::__NR_renameat as usize;
    pub const LINKAT: usize = linux_raw_sys::general::__NR_linkat as usize;
    pub const SYMLINKAT: usize = linux_raw_sys::general::__NR_symlinkat as usize;
    pub const READLINKAT: usize = linux_raw_sys::general::__NR_readlinkat as usize;
    pub const FCHMODAT: usize = linux_raw_sys::general::__NR_fchmodat as usize;
    pub const FACCESSAT: usize = linux_raw_sys::general::__NR_faccessat as usize;
    pub const PSELECT6: usize = linux_raw_sys::general::__NR_pselect6 as usize;
    pub const PPOLL: usize = linux_raw_sys::general::__NR_ppoll as usize;
    pub const SET_ROBUST_LIST: usize = linux_raw_sys::general::__NR_set_robust_list as usize;
    pub const SYNC_FILE_RANGE: usize = linux_raw_sys::general::__NR_sync_file_range as usize;
    pub const UTIMENSAT: usize = linux_raw_sys::general::__NR_utimensat as usize;
    pub const EPOLL_PWAIT: usize = linux_raw_sys::general::__NR_epoll_pwait as usize;
    pub const EVENTFD: usize = linux_raw_sys::general::__NR_eventfd as usize;
    pub const FALLOCATE: usize = linux_raw_sys::general::__NR_fallocate as usize;
    pub const ACCEPT4: usize = linux_raw_sys::general::__NR_accept4 as usize;
    pub const EVENTFD2: usize = linux_raw_sys::general::__NR_eventfd2 as usize;
    pub const EPOLL_CREATE1: usize = linux_raw_sys::general::__NR_epoll_create1 as usize;
    pub const DUP3: usize = linux_raw_sys::general::__NR_dup3 as usize;
    pub const PIPE2: usize = linux_raw_sys::general::__NR_pipe2 as usize;
    pub const PREADV: usize = linux_raw_sys::general::__NR_preadv as usize;
    pub const PWRITEV: usize = linux_raw_sys::general::__NR_pwritev as usize;
    pub const PREADV2: usize = linux_raw_sys::general::__NR_preadv2 as usize;
    pub const PWRITEV2: usize = linux_raw_sys::general::__NR_pwritev2 as usize;
    pub const RECVMMSG: usize = linux_raw_sys::general::__NR_recvmmsg as usize;
    pub const PRLIMIT64: usize = linux_raw_sys::general::__NR_prlimit64 as usize;
    pub const SYNCFS: usize = linux_raw_sys::general::__NR_syncfs as usize;
    pub const SENDMMSG: usize = linux_raw_sys::general::__NR_sendmmsg as usize;
    pub const GETCPU: usize = linux_raw_sys::general::__NR_getcpu as usize;
    pub const RENAMEAT2: usize = linux_raw_sys::general::__NR_renameat2 as usize;
    pub const GETRANDOM: usize = linux_raw_sys::general::__NR_getrandom as usize;
    pub const MEMBARRIER: usize = linux_raw_sys::general::__NR_membarrier as usize;
    pub const STATX: usize = linux_raw_sys::general::__NR_statx as usize;
    pub const RSEQ: usize = linux_raw_sys::general::__NR_rseq as usize;
    pub const CLOSE_RANGE: usize = linux_raw_sys::general::__NR_close_range as usize;
    pub const FACCESSAT2: usize = linux_raw_sys::general::__NR_faccessat2 as usize;
    pub const PIDFD_OPEN: usize = linux_raw_sys::general::__NR_pidfd_open as usize;
    pub const EPOLL_PWAIT2: usize = linux_raw_sys::general::__NR_epoll_pwait2 as usize;
}

/// `struct epoll_event`, which x86-64 packs: the data right after the
/// events.
#[repr(C, packed)]
#[derive(Debug, Clone, Copy, Default)]
pub struct EpollEvent {
    pub events: u32,
    pub data: u64,
}

/// `struct stat` (the kernel's, which is also musl's on x86-64).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Stat {
    pub dev: u64,
    pub ino: u64,
    pub nlink: u64,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub _pad0: u32,
    pub rdev: u64,
    pub size: i64,
    pub blksize: i64,
    pub blocks: i64,
    pub atime: Timespec,
    pub mtime: Timespec,
    pub ctime: Timespec,
    pub _unused: [i64; 3],
}

/// `ucontext_t`, the context a signal handler gets (musl's on x86-64): its
/// size, and where its signal mask is (`uc_sigmask`, after `uc_flags`,
/// `uc_link`, `uc_stack` and the 256 bytes of `uc_mcontext`).
pub mod ucontext {
    pub const SIZE: usize = 936;
    pub const SIGMASK: usize = 296;
}

/// `arch_prctl` codes.
pub mod arch_prctl {
    pub const SET_FS: u32 = linux_raw_sys::general::ARCH_SET_FS;
}
