// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The Linux ABI that musl expects: flags and the layouts of the structures
//! passed through system calls. What is particular to the architecture (the
//! system call numbers, `struct stat`) is `arch::linux`'s, re-exported here.
//!
//! The values are Linux's own, from its headers as `linux-raw-sys` generates
//! them for each architecture, under the names and types the layer uses;
//! the few written here say why.

#![allow(dead_code)]

pub use crate::arch::linux::{Stat, nr};

/// `errno` values.
pub mod errno {
    pub const EPERM: isize = linux_raw_sys::errno::EPERM as isize;
    pub const ENOENT: isize = linux_raw_sys::errno::ENOENT as isize;
    pub const ESRCH: isize = linux_raw_sys::errno::ESRCH as isize;
    pub const EINTR: isize = linux_raw_sys::errno::EINTR as isize;
    pub const EIO: isize = linux_raw_sys::errno::EIO as isize;
    pub const ENXIO: isize = linux_raw_sys::errno::ENXIO as isize;
    pub const E2BIG: isize = linux_raw_sys::errno::E2BIG as isize;
    pub const ENOEXEC: isize = linux_raw_sys::errno::ENOEXEC as isize;
    pub const EBADF: isize = linux_raw_sys::errno::EBADF as isize;
    pub const ECHILD: isize = linux_raw_sys::errno::ECHILD as isize;
    pub const EAGAIN: isize = linux_raw_sys::errno::EAGAIN as isize;
    pub const ENOMEM: isize = linux_raw_sys::errno::ENOMEM as isize;
    pub const EACCES: isize = linux_raw_sys::errno::EACCES as isize;
    pub const EFAULT: isize = linux_raw_sys::errno::EFAULT as isize;
    pub const EBUSY: isize = linux_raw_sys::errno::EBUSY as isize;
    pub const EEXIST: isize = linux_raw_sys::errno::EEXIST as isize;
    pub const EXDEV: isize = linux_raw_sys::errno::EXDEV as isize;
    pub const ENODEV: isize = linux_raw_sys::errno::ENODEV as isize;
    pub const ENOTDIR: isize = linux_raw_sys::errno::ENOTDIR as isize;
    pub const EISDIR: isize = linux_raw_sys::errno::EISDIR as isize;
    pub const EINVAL: isize = linux_raw_sys::errno::EINVAL as isize;
    pub const ENFILE: isize = linux_raw_sys::errno::ENFILE as isize;
    pub const EMFILE: isize = linux_raw_sys::errno::EMFILE as isize;
    pub const ENOTTY: isize = linux_raw_sys::errno::ENOTTY as isize;
    pub const EFBIG: isize = linux_raw_sys::errno::EFBIG as isize;
    pub const ENOSPC: isize = linux_raw_sys::errno::ENOSPC as isize;
    pub const ESPIPE: isize = linux_raw_sys::errno::ESPIPE as isize;
    pub const EROFS: isize = linux_raw_sys::errno::EROFS as isize;
    pub const EMLINK: isize = linux_raw_sys::errno::EMLINK as isize;
    pub const EPIPE: isize = linux_raw_sys::errno::EPIPE as isize;
    pub const ERANGE: isize = linux_raw_sys::errno::ERANGE as isize;
    pub const ENAMETOOLONG: isize = linux_raw_sys::errno::ENAMETOOLONG as isize;
    pub const ENOSYS: isize = linux_raw_sys::errno::ENOSYS as isize;
    pub const ENOTEMPTY: isize = linux_raw_sys::errno::ENOTEMPTY as isize;
    pub const ELOOP: isize = linux_raw_sys::errno::ELOOP as isize;
    pub const ETIME: isize = linux_raw_sys::errno::ETIME as isize;
    pub const EILSEQ: isize = linux_raw_sys::errno::EILSEQ as isize;
    pub const ENOTSOCK: isize = linux_raw_sys::errno::ENOTSOCK as isize;
    pub const EOPNOTSUPP: isize = linux_raw_sys::errno::EOPNOTSUPP as isize;
    pub const EAFNOSUPPORT: isize = linux_raw_sys::errno::EAFNOSUPPORT as isize;
    pub const EISCONN: isize = linux_raw_sys::errno::EISCONN as isize;
    pub const ETIMEDOUT: isize = linux_raw_sys::errno::ETIMEDOUT as isize;
}

/// `open` flags.
pub mod o {
    pub const ACCMODE: u32 = linux_raw_sys::general::O_ACCMODE;
    pub const RDONLY: u32 = linux_raw_sys::general::O_RDONLY;
    pub const WRONLY: u32 = linux_raw_sys::general::O_WRONLY;
    pub const RDWR: u32 = linux_raw_sys::general::O_RDWR;
    pub const CREAT: u32 = linux_raw_sys::general::O_CREAT;
    pub const EXCL: u32 = linux_raw_sys::general::O_EXCL;
    pub const NOCTTY: u32 = linux_raw_sys::general::O_NOCTTY;
    pub const TRUNC: u32 = linux_raw_sys::general::O_TRUNC;
    pub const APPEND: u32 = linux_raw_sys::general::O_APPEND;
    pub const NONBLOCK: u32 = linux_raw_sys::general::O_NONBLOCK;
    pub const DIRECTORY: u32 = linux_raw_sys::general::O_DIRECTORY;
    pub const NOFOLLOW: u32 = linux_raw_sys::general::O_NOFOLLOW;
    pub const CLOEXEC: u32 = linux_raw_sys::general::O_CLOEXEC;
    pub const PATH: u32 = linux_raw_sys::general::O_PATH;
    /// The kernel's `__O_TMPFILE` bit alone: `O_TMPFILE` adds `O_DIRECTORY`,
    /// which a test of the bit must not take for it.
    pub const TMPFILE: u32 = linux_raw_sys::general::__O_TMPFILE;
}

/// The flags of `preadv2` and `pwritev2`.
pub mod rwf {
    pub const HIPRI: u32 = linux_raw_sys::general::RWF_HIPRI;
    pub const DSYNC: u32 = linux_raw_sys::general::RWF_DSYNC;
    pub const SYNC: u32 = linux_raw_sys::general::RWF_SYNC;
    pub const NOWAIT: u32 = linux_raw_sys::general::RWF_NOWAIT;
    pub const APPEND: u32 = linux_raw_sys::general::RWF_APPEND;
    /// Linux 6.9's, newer than `linux-raw-sys`'s headers.
    pub const NOAPPEND: u32 = 0x20;
}

/// `*at` flags.
pub mod at {
    /// `dirfd` meaning the working directory.
    pub const FDCWD: i32 = linux_raw_sys::general::AT_FDCWD;
    pub const SYMLINK_NOFOLLOW: u32 = linux_raw_sys::general::AT_SYMLINK_NOFOLLOW;
    pub const REMOVEDIR: u32 = linux_raw_sys::general::AT_REMOVEDIR;
    pub const EACCESS: u32 = linux_raw_sys::general::AT_EACCESS;
    pub const SYMLINK_FOLLOW: u32 = linux_raw_sys::general::AT_SYMLINK_FOLLOW;
    pub const EMPTY_PATH: u32 = linux_raw_sys::general::AT_EMPTY_PATH;
}

/// `access` modes.
pub mod access {
    pub const F_OK: u32 = linux_raw_sys::general::F_OK;
    pub const X_OK: u32 = linux_raw_sys::general::X_OK;
    pub const W_OK: u32 = linux_raw_sys::general::W_OK;
    pub const R_OK: u32 = linux_raw_sys::general::R_OK;
}

/// `st_mode` file types.
pub mod mode {
    pub const IFMT: u32 = linux_raw_sys::general::S_IFMT;
    pub const IFIFO: u32 = linux_raw_sys::general::S_IFIFO;
    pub const IFCHR: u32 = linux_raw_sys::general::S_IFCHR;
    pub const IFDIR: u32 = linux_raw_sys::general::S_IFDIR;
    pub const IFREG: u32 = linux_raw_sys::general::S_IFREG;
}

/// `fcntl` commands and descriptor flags.
pub mod fcntl {
    pub const DUPFD: u32 = linux_raw_sys::general::F_DUPFD;
    pub const GETFD: u32 = linux_raw_sys::general::F_GETFD;
    pub const SETFD: u32 = linux_raw_sys::general::F_SETFD;
    pub const GETFL: u32 = linux_raw_sys::general::F_GETFL;
    pub const SETFL: u32 = linux_raw_sys::general::F_SETFL;
    pub const GETLK: u32 = linux_raw_sys::general::F_GETLK;
    pub const SETLK: u32 = linux_raw_sys::general::F_SETLK;
    pub const SETLKW: u32 = linux_raw_sys::general::F_SETLKW;
    pub const SETOWN: u32 = linux_raw_sys::general::F_SETOWN;
    pub const GETOWN: u32 = linux_raw_sys::general::F_GETOWN;
    pub const DUPFD_QUERY: u32 = linux_raw_sys::general::F_DUPFD_QUERY;
    pub const DUPFD_CLOEXEC: u32 = linux_raw_sys::general::F_DUPFD_CLOEXEC;
    pub const FD_CLOEXEC: u32 = linux_raw_sys::general::FD_CLOEXEC;
    /// `l_type` of a lock that is not held.
    pub const F_UNLCK: i16 = linux_raw_sys::general::F_UNLCK as i16;
}

/// `lseek` origins.
pub mod seek {
    pub const SET: u32 = linux_raw_sys::general::SEEK_SET;
    pub const CUR: u32 = linux_raw_sys::general::SEEK_CUR;
    pub const END: u32 = linux_raw_sys::general::SEEK_END;
}

/// `mmap` protections and flags.
pub mod mman {
    pub const PROT_READ: u32 = linux_raw_sys::general::PROT_READ;
    pub const PROT_WRITE: u32 = linux_raw_sys::general::PROT_WRITE;
    pub const PROT_EXEC: u32 = linux_raw_sys::general::PROT_EXEC;
    pub const MAP_SHARED: u32 = linux_raw_sys::general::MAP_SHARED;
    pub const MAP_PRIVATE: u32 = linux_raw_sys::general::MAP_PRIVATE;
    pub const MAP_SHARED_VALIDATE: u32 = linux_raw_sys::general::MAP_SHARED_VALIDATE;
    pub const MAP_TYPE: u32 = linux_raw_sys::general::MAP_TYPE;
    pub const MAP_FIXED: u32 = linux_raw_sys::general::MAP_FIXED;
    pub const MAP_ANONYMOUS: u32 = linux_raw_sys::general::MAP_ANONYMOUS;
    pub const MAP_POPULATE: u32 = linux_raw_sys::general::MAP_POPULATE;
    pub const MAP_FIXED_NOREPLACE: u32 = linux_raw_sys::general::MAP_FIXED_NOREPLACE;
    pub const MADV_DONTNEED: u32 = linux_raw_sys::general::MADV_DONTNEED;
    pub const MADV_FREE: u32 = linux_raw_sys::general::MADV_FREE;
    pub const MADV_REMOVE: u32 = linux_raw_sys::general::MADV_REMOVE;
}

/// Terminal and descriptor `ioctl` requests.
pub mod ioctl {
    pub const TCGETS: u32 = linux_raw_sys::ioctl::TCGETS;
    pub const TCSETS: u32 = linux_raw_sys::ioctl::TCSETS;
    pub const TCSETSW: u32 = linux_raw_sys::ioctl::TCSETSW;
    pub const TCSETSF: u32 = linux_raw_sys::ioctl::TCSETSF;
    pub const TCSBRK: u32 = linux_raw_sys::ioctl::TCSBRK;
    pub const TCXONC: u32 = linux_raw_sys::ioctl::TCXONC;
    pub const TCFLSH: u32 = linux_raw_sys::ioctl::TCFLSH;
    pub const TIOCSCTTY: u32 = linux_raw_sys::ioctl::TIOCSCTTY;
    pub const TIOCGPGRP: u32 = linux_raw_sys::ioctl::TIOCGPGRP;
    pub const TIOCSPGRP: u32 = linux_raw_sys::ioctl::TIOCSPGRP;
    pub const TIOCGWINSZ: u32 = linux_raw_sys::ioctl::TIOCGWINSZ;
    pub const TIOCSWINSZ: u32 = linux_raw_sys::ioctl::TIOCSWINSZ;
    pub const FIONREAD: u32 = linux_raw_sys::ioctl::FIONREAD;
    pub const FIONBIO: u32 = linux_raw_sys::ioctl::FIONBIO;
    pub const TIOCNOTTY: u32 = linux_raw_sys::ioctl::TIOCNOTTY;
    pub const FIONCLEX: u32 = linux_raw_sys::ioctl::FIONCLEX;
    pub const FIOCLEX: u32 = linux_raw_sys::ioctl::FIOCLEX;
}

/// `poll` events.
pub mod poll {
    pub const IN: i16 = linux_raw_sys::general::POLLIN as i16;
    pub const PRI: i16 = linux_raw_sys::general::POLLPRI as i16;
    pub const OUT: i16 = linux_raw_sys::general::POLLOUT as i16;
    pub const ERR: i16 = linux_raw_sys::general::POLLERR as i16;
    pub const HUP: i16 = linux_raw_sys::general::POLLHUP as i16;
    pub const NVAL: i16 = linux_raw_sys::general::POLLNVAL as i16;
    pub const RDNORM: i16 = linux_raw_sys::general::POLLRDNORM as i16;
    pub const WRNORM: i16 = linux_raw_sys::general::POLLWRNORM as i16;
}

/// Clocks of `clock_gettime`.
pub mod clock {
    pub const REALTIME: u32 = linux_raw_sys::general::CLOCK_REALTIME;
    pub const MONOTONIC: u32 = linux_raw_sys::general::CLOCK_MONOTONIC;
    pub const PROCESS_CPUTIME_ID: u32 = linux_raw_sys::general::CLOCK_PROCESS_CPUTIME_ID;
    pub const THREAD_CPUTIME_ID: u32 = linux_raw_sys::general::CLOCK_THREAD_CPUTIME_ID;
    pub const MONOTONIC_RAW: u32 = linux_raw_sys::general::CLOCK_MONOTONIC_RAW;
    pub const REALTIME_COARSE: u32 = linux_raw_sys::general::CLOCK_REALTIME_COARSE;
    pub const MONOTONIC_COARSE: u32 = linux_raw_sys::general::CLOCK_MONOTONIC_COARSE;
    pub const BOOTTIME: u32 = linux_raw_sys::general::CLOCK_BOOTTIME;
    /// `clock_nanosleep` flag: the time is absolute.
    pub const TIMER_ABSTIME: u32 = linux_raw_sys::general::TIMER_ABSTIME;
}

/// `clone` flags.
pub mod clone {
    pub const VM: u32 = linux_raw_sys::general::CLONE_VM;
    pub const FS: u32 = linux_raw_sys::general::CLONE_FS;
    pub const FILES: u32 = linux_raw_sys::general::CLONE_FILES;
    pub const SIGHAND: u32 = linux_raw_sys::general::CLONE_SIGHAND;
    pub const VFORK: u32 = linux_raw_sys::general::CLONE_VFORK;
    pub const THREAD: u32 = linux_raw_sys::general::CLONE_THREAD;
    pub const SETTLS: u32 = linux_raw_sys::general::CLONE_SETTLS;
    pub const PARENT_SETTID: u32 = linux_raw_sys::general::CLONE_PARENT_SETTID;
    pub const CHILD_CLEARTID: u32 = linux_raw_sys::general::CLONE_CHILD_CLEARTID;
    pub const CHILD_SETTID: u32 = linux_raw_sys::general::CLONE_CHILD_SETTID;
}

/// `futex` operations.
pub mod futex {
    pub const WAIT: u32 = linux_raw_sys::general::FUTEX_WAIT;
    pub const WAKE: u32 = linux_raw_sys::general::FUTEX_WAKE;
    pub const REQUEUE: u32 = linux_raw_sys::general::FUTEX_REQUEUE;
    pub const CMP_REQUEUE: u32 = linux_raw_sys::general::FUTEX_CMP_REQUEUE;
    pub const WAKE_OP: u32 = linux_raw_sys::general::FUTEX_WAKE_OP;
    pub const WAIT_BITSET: u32 = linux_raw_sys::general::FUTEX_WAIT_BITSET;
    pub const WAKE_BITSET: u32 = linux_raw_sys::general::FUTEX_WAKE_BITSET;
    pub const PRIVATE_FLAG: u32 = linux_raw_sys::general::FUTEX_PRIVATE_FLAG;
    pub const CLOCK_REALTIME: u32 = linux_raw_sys::general::FUTEX_CLOCK_REALTIME;
    pub const CMD_MASK: u32 = linux_raw_sys::general::FUTEX_CMD_MASK as u32;
}

/// `wait4`/`waitid` options and kinds.
pub mod wait {
    pub const NOHANG: u32 = linux_raw_sys::general::WNOHANG;
    pub const UNTRACED: u32 = linux_raw_sys::general::WUNTRACED;
    pub const EXITED: u32 = linux_raw_sys::general::WEXITED;
    pub const NOWAIT: u32 = linux_raw_sys::general::WNOWAIT;
    pub const P_ALL: u32 = linux_raw_sys::general::P_ALL;
    pub const P_PID: u32 = linux_raw_sys::general::P_PID;
    pub const P_PGID: u32 = linux_raw_sys::general::P_PGID;
    /// `si_code` of a child that exited / was killed.
    pub const CLD_EXITED: i32 = linux_raw_sys::general::CLD_EXITED as i32;
    pub const CLD_KILLED: i32 = linux_raw_sys::general::CLD_KILLED as i32;
}

/// Resource limits.
pub mod rlimit {
    pub const CPU: u32 = linux_raw_sys::general::RLIMIT_CPU;
    pub const FSIZE: u32 = linux_raw_sys::general::RLIMIT_FSIZE;
    pub const DATA: u32 = linux_raw_sys::general::RLIMIT_DATA;
    pub const STACK: u32 = linux_raw_sys::general::RLIMIT_STACK;
    pub const CORE: u32 = linux_raw_sys::general::RLIMIT_CORE;
    pub const NOFILE: u32 = linux_raw_sys::general::RLIMIT_NOFILE;
    pub const AS: u32 = linux_raw_sys::general::RLIMIT_AS;
    /// musl's `RLIM_INFINITY`: every bit of the 64-bit limit.
    pub const INFINITY: u64 = u64::MAX;
}

/// Signals.
pub mod sig {
    pub const HUP: u32 = linux_raw_sys::general::SIGHUP;
    pub const INT: u32 = linux_raw_sys::general::SIGINT;
    pub const QUIT: u32 = linux_raw_sys::general::SIGQUIT;
    pub const ILL: u32 = linux_raw_sys::general::SIGILL;
    pub const TRAP: u32 = linux_raw_sys::general::SIGTRAP;
    pub const ABRT: u32 = linux_raw_sys::general::SIGABRT;
    pub const BUS: u32 = linux_raw_sys::general::SIGBUS;
    pub const FPE: u32 = linux_raw_sys::general::SIGFPE;
    pub const KILL: u32 = linux_raw_sys::general::SIGKILL;
    pub const USR1: u32 = linux_raw_sys::general::SIGUSR1;
    pub const SEGV: u32 = linux_raw_sys::general::SIGSEGV;
    pub const USR2: u32 = linux_raw_sys::general::SIGUSR2;
    pub const PIPE: u32 = linux_raw_sys::general::SIGPIPE;
    pub const ALRM: u32 = linux_raw_sys::general::SIGALRM;
    pub const TERM: u32 = linux_raw_sys::general::SIGTERM;
    pub const CHLD: u32 = linux_raw_sys::general::SIGCHLD;
    pub const CONT: u32 = linux_raw_sys::general::SIGCONT;
    pub const STOP: u32 = linux_raw_sys::general::SIGSTOP;
    pub const TSTP: u32 = linux_raw_sys::general::SIGTSTP;
    pub const TTIN: u32 = linux_raw_sys::general::SIGTTIN;
    pub const TTOU: u32 = linux_raw_sys::general::SIGTTOU;
    pub const URG: u32 = linux_raw_sys::general::SIGURG;
    pub const WINCH: u32 = linux_raw_sys::general::SIGWINCH;
    /// Signals are numbered 1 to `NSIG - 1` (musl's `NSIG`: one past the
    /// kernel's `_NSIG` signals).
    pub const NSIG: u32 = linux_raw_sys::general::_NSIG + 1;
    /// The handlers `SIG_DFL` and `SIG_IGN` (the headers define them as
    /// pointer casts, not numbers).
    pub const DFL: usize = 0;
    pub const IGN: usize = 1;
    pub const SA_SIGINFO: u64 = linux_raw_sys::general::SA_SIGINFO as u64;
    pub const SA_NODEFER: u64 = linux_raw_sys::general::SA_NODEFER as u64;
    pub const SA_RESETHAND: u64 = linux_raw_sys::general::SA_RESETHAND as u64;
    pub const BLOCK: u32 = linux_raw_sys::general::SIG_BLOCK;
    pub const UNBLOCK: u32 = linux_raw_sys::general::SIG_UNBLOCK;
    pub const SETMASK: u32 = linux_raw_sys::general::SIG_SETMASK;
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Timespec {
    pub sec: i64,
    pub nsec: i64,
}

impl Timespec {
    pub fn from_ns(ns: u64) -> Timespec {
        Timespec { sec: (ns / 1_000_000_000) as i64, nsec: (ns % 1_000_000_000) as i64 }
    }

    /// Nanoseconds, or `None` for a negative or malformed time.
    pub fn to_ns(self) -> Option<u64> {
        if self.sec < 0 || !(0..1_000_000_000).contains(&self.nsec) {
            return None;
        }
        Some((self.sec as u64).saturating_mul(1_000_000_000).saturating_add(self.nsec as u64))
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Timeval {
    pub sec: i64,
    pub usec: i64,
}

/// `struct winsize`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Winsize {
    pub row: u16,
    pub col: u16,
    pub xpixel: u16,
    pub ypixel: u16,
}

/// The kernel's `struct termios` (what `TCGETS` and `TCSETS` carry).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Termios {
    pub iflag: u32,
    pub oflag: u32,
    pub cflag: u32,
    pub lflag: u32,
    pub line: u8,
    pub cc: [u8; 19],
}

/// `struct utsname`.
#[repr(C)]
pub struct Utsname {
    pub sysname: [u8; 65],
    pub nodename: [u8; 65],
    pub release: [u8; 65],
    pub version: [u8; 65],
    pub machine: [u8; 65],
    pub domainname: [u8; 65],
}

/// `struct rusage`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Rusage {
    pub utime: Timeval,
    pub stime: Timeval,
    pub rest: [i64; 14],
}

/// `struct tms`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Tms {
    pub utime: i64,
    pub stime: i64,
    pub cutime: i64,
    pub cstime: i64,
}

/// The kernel's `struct sysinfo`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Sysinfo {
    pub uptime: i64,
    pub loads: [u64; 3],
    pub totalram: u64,
    pub freeram: u64,
    pub sharedram: u64,
    pub bufferram: u64,
    pub totalswap: u64,
    pub freeswap: u64,
    pub procs: u16,
    pub _pad: u16,
    pub totalhigh: u64,
    pub freehigh: u64,
    pub mem_unit: u32,
}

/// `struct rlimit`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Rlimit {
    pub cur: u64,
    pub max: u64,
}

/// `struct iovec`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Iovec {
    pub base: *mut u8,
    pub len: usize,
}

/// The kernel's `struct msghdr` (`sendmsg`, `recvmsg`).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Msghdr {
    pub name: usize,
    pub namelen: u32,
    pub iov: usize,
    pub iovlen: usize,
    pub control: usize,
    pub controllen: usize,
    pub flags: i32,
}

/// Flags of `send` and `recv`.
pub mod msg {
    pub const OOB: u32 = linux_raw_sys::net::MSG_OOB;
    pub const PEEK: u32 = linux_raw_sys::net::MSG_PEEK;
    pub const DONTWAIT: u32 = linux_raw_sys::net::MSG_DONTWAIT;
    pub const WAITALL: u32 = linux_raw_sys::net::MSG_WAITALL;
    pub const NOSIGNAL: u32 = linux_raw_sys::net::MSG_NOSIGNAL;
}

/// `struct pollfd`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Pollfd {
    pub fd: i32,
    pub events: i16,
    pub revents: i16,
}

/// The kernel's `struct sigaction`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Sigaction {
    pub handler: usize,
    pub flags: u64,
    pub restorer: usize,
    pub mask: u64,
}

/// `struct statfs`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Statfs {
    pub kind: i64,
    pub bsize: i64,
    pub blocks: u64,
    pub bfree: u64,
    pub bavail: u64,
    pub files: u64,
    pub ffree: u64,
    pub fsid: [i32; 2],
    pub namelen: i64,
    pub frsize: i64,
    pub flags: i64,
    pub spare: [i64; 4],
}

/// `struct flock`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Flock {
    pub kind: i16,
    pub whence: i16,
    pub start: i64,
    pub len: i64,
    pub pid: i32,
}

/// `siginfo_t`, as `waitid` fills it in for a child and signal handlers
/// get it.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Siginfo {
    pub signo: i32,
    pub errno: i32,
    pub code: i32,
    pub _pad: i32,
    pub pid: i32,
    pub uid: u32,
    pub status: i32,
    pub _rest: [u8; 100],
}

impl Default for Siginfo {
    fn default() -> Siginfo {
        Siginfo { signo: 0, errno: 0, code: 0, _pad: 0, pid: 0, uid: 0, status: 0, _rest: [0; 100] }
    }
}

/// Directory entry types of `getdents64`.
pub mod dt {
    pub const FIFO: u8 = linux_raw_sys::general::DT_FIFO as u8;
    pub const CHR: u8 = linux_raw_sys::general::DT_CHR as u8;
    pub const DIR: u8 = linux_raw_sys::general::DT_DIR as u8;
    pub const REG: u8 = linux_raw_sys::general::DT_REG as u8;
}

/// `prctl` options.
pub mod prctl {
    pub const SET_NAME: u32 = linux_raw_sys::prctl::PR_SET_NAME;
    pub const GET_NAME: u32 = linux_raw_sys::prctl::PR_GET_NAME;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_match_the_kernel() {
        use core::mem::{offset_of, size_of};
        assert_eq!(size_of::<Stat>(), 144);
        assert_eq!(offset_of!(Stat, mode), 24);
        assert_eq!(offset_of!(Stat, size), 48);
        assert_eq!(offset_of!(Stat, mtime), 88);
        assert_eq!(size_of::<Termios>(), 36);
        assert_eq!(size_of::<Utsname>(), 390);
        assert_eq!(size_of::<Rusage>(), 144);
        assert_eq!(offset_of!(Sysinfo, procs), 80);
        assert_eq!(offset_of!(Sysinfo, totalhigh), 88);
        assert_eq!(offset_of!(Sysinfo, mem_unit), 104);
        assert_eq!(size_of::<Sigaction>(), 32);
        assert_eq!(size_of::<Statfs>(), 120);
        assert_eq!(size_of::<Flock>(), 32);
        assert_eq!(size_of::<Siginfo>(), 128);
        assert_eq!(size_of::<Pollfd>(), 8);
        assert_eq!(size_of::<Msghdr>(), 56);
        assert_eq!(offset_of!(Msghdr, iov), 16);
        assert_eq!(offset_of!(Msghdr, control), 32);
        assert_eq!(offset_of!(Msghdr, flags), 48);
    }

    #[test]
    fn timespecs() {
        assert_eq!(Timespec::from_ns(1_500_000_001), Timespec { sec: 1, nsec: 500_000_001 });
        assert_eq!(Timespec { sec: 2, nsec: 5 }.to_ns(), Some(2_000_000_005));
        assert_eq!(Timespec { sec: -1, nsec: 0 }.to_ns(), None);
        assert_eq!(Timespec { sec: 0, nsec: 1_000_000_000 }.to_ns(), None);
    }
}
