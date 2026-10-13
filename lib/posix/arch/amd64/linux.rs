// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What Linux's ABI has particular to x86-64, as musl uses it: the system
//! call numbers, `struct stat`, and `arch_prctl`'s codes.

// The ABI's whole tables, not only what the layer uses.
#![allow(dead_code)]

use crate::linux::Timespec;

/// System call numbers (musl's `arch/x86_64/bits/syscall.h`).
pub mod nr {
    pub const READ: usize = 0;
    pub const WRITE: usize = 1;
    pub const OPEN: usize = 2;
    pub const CLOSE: usize = 3;
    pub const STAT: usize = 4;
    pub const FSTAT: usize = 5;
    pub const LSTAT: usize = 6;
    pub const POLL: usize = 7;
    pub const LSEEK: usize = 8;
    pub const MMAP: usize = 9;
    pub const MPROTECT: usize = 10;
    pub const MUNMAP: usize = 11;
    pub const BRK: usize = 12;
    pub const RT_SIGACTION: usize = 13;
    pub const RT_SIGPROCMASK: usize = 14;
    pub const IOCTL: usize = 16;
    pub const PREAD64: usize = 17;
    pub const PWRITE64: usize = 18;
    pub const READV: usize = 19;
    pub const WRITEV: usize = 20;
    pub const ACCESS: usize = 21;
    pub const PIPE: usize = 22;
    pub const SELECT: usize = 23;
    pub const SCHED_YIELD: usize = 24;
    pub const MREMAP: usize = 25;
    pub const MSYNC: usize = 26;
    pub const MINCORE: usize = 27;
    pub const MADVISE: usize = 28;
    pub const DUP: usize = 32;
    pub const DUP2: usize = 33;
    pub const PAUSE: usize = 34;
    pub const NANOSLEEP: usize = 35;
    pub const GETITIMER: usize = 36;
    pub const ALARM: usize = 37;
    pub const SETITIMER: usize = 38;
    pub const GETPID: usize = 39;
    pub const SOCKET: usize = 41;
    pub const SENDTO: usize = 44;
    pub const RECVFROM: usize = 45;
    pub const SENDMSG: usize = 46;
    pub const RECVMSG: usize = 47;
    pub const SHUTDOWN: usize = 48;
    pub const SOCKETPAIR: usize = 53;
    pub const CLONE: usize = 56;
    pub const FORK: usize = 57;
    pub const VFORK: usize = 58;
    pub const EXECVE: usize = 59;
    pub const EXIT: usize = 60;
    pub const WAIT4: usize = 61;
    pub const KILL: usize = 62;
    pub const UNAME: usize = 63;
    pub const FCNTL: usize = 72;
    pub const FLOCK: usize = 73;
    pub const FSYNC: usize = 74;
    pub const FDATASYNC: usize = 75;
    pub const TRUNCATE: usize = 76;
    pub const FTRUNCATE: usize = 77;
    pub const GETDENTS: usize = 78;
    pub const GETCWD: usize = 79;
    pub const CHDIR: usize = 80;
    pub const FCHDIR: usize = 81;
    pub const RENAME: usize = 82;
    pub const MKDIR: usize = 83;
    pub const RMDIR: usize = 84;
    pub const CREAT: usize = 85;
    pub const LINK: usize = 86;
    pub const UNLINK: usize = 87;
    pub const SYMLINK: usize = 88;
    pub const READLINK: usize = 89;
    pub const CHMOD: usize = 90;
    pub const FCHMOD: usize = 91;
    pub const CHOWN: usize = 92;
    pub const FCHOWN: usize = 93;
    pub const LCHOWN: usize = 94;
    pub const UMASK: usize = 95;
    pub const GETTIMEOFDAY: usize = 96;
    pub const GETRLIMIT: usize = 97;
    pub const GETRUSAGE: usize = 98;
    pub const SYSINFO: usize = 99;
    pub const TIMES: usize = 100;
    pub const GETUID: usize = 102;
    pub const GETGID: usize = 104;
    pub const SETUID: usize = 105;
    pub const SETGID: usize = 106;
    pub const GETEUID: usize = 107;
    pub const GETEGID: usize = 108;
    pub const SETPGID: usize = 109;
    pub const GETPPID: usize = 110;
    pub const GETPGRP: usize = 111;
    pub const SETSID: usize = 112;
    pub const SETREUID: usize = 113;
    pub const SETREGID: usize = 114;
    pub const GETGROUPS: usize = 115;
    pub const SETGROUPS: usize = 116;
    pub const SETRESUID: usize = 117;
    pub const GETRESUID: usize = 118;
    pub const SETRESGID: usize = 119;
    pub const GETRESGID: usize = 120;
    pub const GETPGID: usize = 121;
    pub const GETSID: usize = 124;
    pub const RT_SIGPENDING: usize = 127;
    pub const RT_SIGTIMEDWAIT: usize = 128;
    pub const RT_SIGSUSPEND: usize = 130;
    pub const SIGALTSTACK: usize = 131;
    pub const UTIME: usize = 132;
    pub const MKNOD: usize = 133;
    pub const STATFS: usize = 137;
    pub const FSTATFS: usize = 138;
    pub const GETPRIORITY: usize = 140;
    pub const SETPRIORITY: usize = 141;
    pub const SCHED_GETPARAM: usize = 143;
    pub const SCHED_SETSCHEDULER: usize = 144;
    pub const SCHED_GETSCHEDULER: usize = 145;
    pub const SCHED_GET_PRIORITY_MAX: usize = 146;
    pub const SCHED_GET_PRIORITY_MIN: usize = 147;
    pub const MLOCK: usize = 149;
    pub const MUNLOCK: usize = 150;
    pub const MLOCKALL: usize = 151;
    pub const MUNLOCKALL: usize = 152;
    pub const PRCTL: usize = 157;
    pub const ARCH_PRCTL: usize = 158;
    pub const SETRLIMIT: usize = 160;
    pub const SYNC: usize = 162;
    pub const GETTID: usize = 186;
    pub const READAHEAD: usize = 187;
    pub const TKILL: usize = 200;
    pub const TIME: usize = 201;
    pub const FUTEX: usize = 202;
    pub const SCHED_SETAFFINITY: usize = 203;
    pub const SCHED_GETAFFINITY: usize = 204;
    pub const GETDENTS64: usize = 217;
    pub const SET_TID_ADDRESS: usize = 218;
    pub const FADVISE64: usize = 221;
    pub const CLOCK_GETTIME: usize = 228;
    pub const CLOCK_GETRES: usize = 229;
    pub const CLOCK_NANOSLEEP: usize = 230;
    pub const EXIT_GROUP: usize = 231;
    pub const TGKILL: usize = 234;
    pub const UTIMES: usize = 235;
    pub const WAITID: usize = 247;
    pub const OPENAT: usize = 257;
    pub const MKDIRAT: usize = 258;
    pub const MKNODAT: usize = 259;
    pub const FCHOWNAT: usize = 260;
    pub const FUTIMESAT: usize = 261;
    pub const NEWFSTATAT: usize = 262;
    pub const UNLINKAT: usize = 263;
    pub const RENAMEAT: usize = 264;
    pub const LINKAT: usize = 265;
    pub const SYMLINKAT: usize = 266;
    pub const READLINKAT: usize = 267;
    pub const FCHMODAT: usize = 268;
    pub const FACCESSAT: usize = 269;
    pub const PSELECT6: usize = 270;
    pub const PPOLL: usize = 271;
    pub const SET_ROBUST_LIST: usize = 273;
    pub const SYNC_FILE_RANGE: usize = 277;
    pub const UTIMENSAT: usize = 280;
    pub const FALLOCATE: usize = 285;
    pub const DUP3: usize = 292;
    pub const PIPE2: usize = 293;
    pub const PREADV: usize = 295;
    pub const PWRITEV: usize = 296;
    pub const PRLIMIT64: usize = 302;
    pub const SYNCFS: usize = 306;
    pub const GETCPU: usize = 309;
    pub const RENAMEAT2: usize = 316;
    pub const GETRANDOM: usize = 318;
    pub const MEMBARRIER: usize = 324;
    pub const STATX: usize = 332;
    pub const RSEQ: usize = 334;
    pub const CLOSE_RANGE: usize = 436;
    pub const FACCESSAT2: usize = 439;
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

/// `arch_prctl` codes.
pub mod arch_prctl {
    pub const SET_FS: u32 = 0x1002;
    pub const GET_FS: u32 = 0x1003;
}
