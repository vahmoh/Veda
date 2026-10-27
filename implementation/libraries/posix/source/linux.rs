// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The Linux ABI that musl expects: flags and the layouts of the structures
//! passed through system calls. What is particular to the architecture (the
//! system call numbers, `struct stat`) is `architecture::linux`'s,
//! re-exported here.
//!
//! The values are Linux's own, from its headers as `linux-raw-sys` generates
//! them for each architecture, under the names and types the layer uses;
//! the few written here say why.

pub use crate::architecture::linux::{EpollEvent, Stat, nr, ucontext};

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
    pub const EFBIG: isize = linux_raw_sys::errno::EFBIG as isize;
    pub const EBUSY: isize = linux_raw_sys::errno::EBUSY as isize;
    pub const EEXIST: isize = linux_raw_sys::errno::EEXIST as isize;
    pub const EXDEV: isize = linux_raw_sys::errno::EXDEV as isize;
    pub const ENODEV: isize = linux_raw_sys::errno::ENODEV as isize;
    pub const ENOTDIR: isize = linux_raw_sys::errno::ENOTDIR as isize;
    pub const EISDIR: isize = linux_raw_sys::errno::EISDIR as isize;
    pub const EINVAL: isize = linux_raw_sys::errno::EINVAL as isize;
    pub const EMFILE: isize = linux_raw_sys::errno::EMFILE as isize;
    pub const ENOTTY: isize = linux_raw_sys::errno::ENOTTY as isize;
    pub const ENOSPC: isize = linux_raw_sys::errno::ENOSPC as isize;
    pub const ESPIPE: isize = linux_raw_sys::errno::ESPIPE as isize;
    pub const EROFS: isize = linux_raw_sys::errno::EROFS as isize;
    pub const EPIPE: isize = linux_raw_sys::errno::EPIPE as isize;
    pub const ERANGE: isize = linux_raw_sys::errno::ERANGE as isize;
    pub const EDEADLK: isize = linux_raw_sys::errno::EDEADLK as isize;
    pub const ENAMETOOLONG: isize = linux_raw_sys::errno::ENAMETOOLONG as isize;
    pub const ENOLCK: isize = linux_raw_sys::errno::ENOLCK as isize;
    pub const ENOSYS: isize = linux_raw_sys::errno::ENOSYS as isize;
    pub const ENOTEMPTY: isize = linux_raw_sys::errno::ENOTEMPTY as isize;
    pub const ELOOP: isize = linux_raw_sys::errno::ELOOP as isize;
    pub const EOVERFLOW: isize = linux_raw_sys::errno::EOVERFLOW as isize;
    pub const EILSEQ: isize = linux_raw_sys::errno::EILSEQ as isize;
    pub const ENOTSOCK: isize = linux_raw_sys::errno::ENOTSOCK as isize;
    pub const EOPNOTSUPP: isize = linux_raw_sys::errno::EOPNOTSUPP as isize;
    pub const EAFNOSUPPORT: isize = linux_raw_sys::errno::EAFNOSUPPORT as isize;
    pub const EISCONN: isize = linux_raw_sys::errno::EISCONN as isize;
    pub const ETIMEDOUT: isize = linux_raw_sys::errno::ETIMEDOUT as isize;
    pub const EDOM: isize = linux_raw_sys::errno::EDOM as isize;
    pub const EDESTADDRREQ: isize = linux_raw_sys::errno::EDESTADDRREQ as isize;
    pub const EMSGSIZE: isize = linux_raw_sys::errno::EMSGSIZE as isize;
    pub const ENOPROTOOPT: isize = linux_raw_sys::errno::ENOPROTOOPT as isize;
    pub const EPROTONOSUPPORT: isize = linux_raw_sys::errno::EPROTONOSUPPORT as isize;
    pub const EPROTO: isize = linux_raw_sys::errno::EPROTO as isize;
    pub const EADDRINUSE: isize = linux_raw_sys::errno::EADDRINUSE as isize;
    pub const EADDRNOTAVAIL: isize = linux_raw_sys::errno::EADDRNOTAVAIL as isize;
    pub const ENETDOWN: isize = linux_raw_sys::errno::ENETDOWN as isize;
    pub const ENETUNREACH: isize = linux_raw_sys::errno::ENETUNREACH as isize;
    pub const ECONNABORTED: isize = linux_raw_sys::errno::ECONNABORTED as isize;
    pub const ECONNRESET: isize = linux_raw_sys::errno::ECONNRESET as isize;
    pub const ENOBUFS: isize = linux_raw_sys::errno::ENOBUFS as isize;
    pub const ENOTCONN: isize = linux_raw_sys::errno::ENOTCONN as isize;
    pub const ECONNREFUSED: isize = linux_raw_sys::errno::ECONNREFUSED as isize;
    pub const EHOSTUNREACH: isize = linux_raw_sys::errno::EHOSTUNREACH as isize;
    pub const EALREADY: isize = linux_raw_sys::errno::EALREADY as isize;
    pub const EINPROGRESS: isize = linux_raw_sys::errno::EINPROGRESS as isize;
    pub const EPROTOTYPE: isize = linux_raw_sys::errno::EPROTOTYPE as isize;
    pub const ESOCKTNOSUPPORT: isize = linux_raw_sys::errno::ESOCKTNOSUPPORT as isize;
    pub const ETOOMANYREFS: isize = linux_raw_sys::errno::ETOOMANYREFS as isize;
    /// Not `errno` but Linux's own word inside the kernel, which no program
    /// sees: a blocking call a signal interrupted, which ends with `EINTR`
    /// or starts again once the handlers have run, as their `SA_RESTART`
    /// says (`crate::signal`).
    pub const ERESTARTSYS: isize = 512;
}

/// `open` flags.
pub mod o {
    pub const ACCMODE: u32 = linux_raw_sys::general::O_ACCMODE;
    pub const RDONLY: u32 = linux_raw_sys::general::O_RDONLY;
    pub const WRONLY: u32 = linux_raw_sys::general::O_WRONLY;
    pub const RDWR: u32 = linux_raw_sys::general::O_RDWR;
    pub const CREAT: u32 = linux_raw_sys::general::O_CREAT;
    pub const EXCL: u32 = linux_raw_sys::general::O_EXCL;
    pub const TRUNC: u32 = linux_raw_sys::general::O_TRUNC;
    pub const APPEND: u32 = linux_raw_sys::general::O_APPEND;
    pub const NONBLOCK: u32 = linux_raw_sys::general::O_NONBLOCK;
    /// Writes reach persistent storage before they return (`O_SYNC`
    /// includes `O_DSYNC`'s bit).
    pub const DSYNC: u32 = linux_raw_sys::general::O_DSYNC;
    pub const SYNC: u32 = linux_raw_sys::general::O_SYNC;
    pub const DIRECTORY: u32 = linux_raw_sys::general::O_DIRECTORY;
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
    pub const APPEND: u32 = linux_raw_sys::general::RWF_APPEND;
    /// Linux 6.9's, newer than `linux-raw-sys`'s headers.
    pub const NOAPPEND: u32 = 0x20;
}

/// `*at` flags.
pub mod at {
    /// `dirfd` meaning the working directory.
    pub const FDCWD: i32 = linux_raw_sys::general::AT_FDCWD;
    pub const SYMLINK_NOFOLLOW: u32 = linux_raw_sys::general::AT_SYMLINK_NOFOLLOW;
    pub const SYMLINK_FOLLOW: u32 = linux_raw_sys::general::AT_SYMLINK_FOLLOW;
    pub const REMOVEDIR: u32 = linux_raw_sys::general::AT_REMOVEDIR;
    pub const EMPTY_PATH: u32 = linux_raw_sys::general::AT_EMPTY_PATH;
    pub const NO_AUTOMOUNT: u32 = linux_raw_sys::general::AT_NO_AUTOMOUNT;
    /// `statx`'s say in how caches are treated (`AT_STATX_SYNC_AS_STAT`,
    /// `AT_STATX_FORCE_SYNC`, `AT_STATX_DONT_SYNC`).
    pub const STATX_SYNC_TYPE: u32 = linux_raw_sys::general::AT_STATX_SYNC_TYPE;
}

/// The major and minor numbers of device number `dev`, as glibc's
/// `major` and `minor` split it.
pub fn dev_parts(dev: u64) -> (u32, u32) {
    let major = ((dev >> 8) & 0xfff) | ((dev >> 32) & !0xfff);
    let minor = (dev & 0xff) | ((dev >> 12) & !0xff);
    (major as u32, minor as u32)
}

/// `access` modes.
pub mod access {
    pub const X_OK: u32 = linux_raw_sys::general::X_OK;
    pub const W_OK: u32 = linux_raw_sys::general::W_OK;
}

/// `st_mode` file types.
pub mod mode {
    pub const IFIFO: u32 = linux_raw_sys::general::S_IFIFO;
    pub const IFCHR: u32 = linux_raw_sys::general::S_IFCHR;
    pub const IFDIR: u32 = linux_raw_sys::general::S_IFDIR;
    pub const IFREG: u32 = linux_raw_sys::general::S_IFREG;
    pub const IFSOCK: u32 = linux_raw_sys::general::S_IFSOCK;
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
    /// Record locks held by the open file description (OFD locks).
    pub const OFD_GETLK: u32 = linux_raw_sys::general::F_OFD_GETLK;
    pub const OFD_SETLK: u32 = linux_raw_sys::general::F_OFD_SETLK;
    pub const OFD_SETLKW: u32 = linux_raw_sys::general::F_OFD_SETLKW;
    pub const SETOWN: u32 = linux_raw_sys::general::F_SETOWN;
    pub const GETOWN: u32 = linux_raw_sys::general::F_GETOWN;
    pub const SETOWN_EX: u32 = linux_raw_sys::general::F_SETOWN_EX;
    pub const GETOWN_EX: u32 = linux_raw_sys::general::F_GETOWN_EX;
    /// Kinds of owner (`struct f_owner_ex`'s `type`): a thread, a
    /// process, a process group.
    pub const F_OWNER_TID: i32 = linux_raw_sys::general::F_OWNER_TID as i32;
    pub const F_OWNER_PID: i32 = linux_raw_sys::general::F_OWNER_PID as i32;
    pub const F_OWNER_PGRP: i32 = linux_raw_sys::general::F_OWNER_PGRP as i32;
    pub const SETPIPE_SZ: u32 = linux_raw_sys::general::F_SETPIPE_SZ;
    pub const GETPIPE_SZ: u32 = linux_raw_sys::general::F_GETPIPE_SZ;
    pub const DUPFD_QUERY: u32 = linux_raw_sys::general::F_DUPFD_QUERY;
    pub const DUPFD_CLOEXEC: u32 = linux_raw_sys::general::F_DUPFD_CLOEXEC;
    pub const ADD_SEALS: u32 = linux_raw_sys::general::F_ADD_SEALS;
    pub const GET_SEALS: u32 = linux_raw_sys::general::F_GET_SEALS;
    pub const FD_CLOEXEC: u32 = linux_raw_sys::general::FD_CLOEXEC;
    /// `l_type`s of a lock: shared, exclusive, not held.
    pub const F_RDLCK: i16 = linux_raw_sys::general::F_RDLCK as i16;
    pub const F_WRLCK: i16 = linux_raw_sys::general::F_WRLCK as i16;
    pub const F_UNLCK: i16 = linux_raw_sys::general::F_UNLCK as i16;
}

/// `flock` operations.
pub mod flock {
    pub const SH: u32 = linux_raw_sys::general::LOCK_SH;
    pub const EX: u32 = linux_raw_sys::general::LOCK_EX;
    pub const NB: u32 = linux_raw_sys::general::LOCK_NB;
    pub const UN: u32 = linux_raw_sys::general::LOCK_UN;
    /// Mandatory locks, which Linux no longer has, and ignores.
    pub const MAND: u32 = linux_raw_sys::general::LOCK_MAND;
}

/// `sync_file_range` flags.
pub mod sync_range {
    pub const WAIT_BEFORE: u32 = linux_raw_sys::general::SYNC_FILE_RANGE_WAIT_BEFORE;
    pub const WRITE: u32 = linux_raw_sys::general::SYNC_FILE_RANGE_WRITE;
    pub const WAIT_AFTER: u32 = linux_raw_sys::general::SYNC_FILE_RANGE_WAIT_AFTER;
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

/// `membarrier`'s commands.
pub mod membarrier {
    use linux_raw_sys::general::membarrier_cmd;
    pub const QUERY: u32 = membarrier_cmd::MEMBARRIER_CMD_QUERY as u32;
    pub const PRIVATE_EXPEDITED: u32 = membarrier_cmd::MEMBARRIER_CMD_PRIVATE_EXPEDITED as u32;
    pub const REGISTER_PRIVATE_EXPEDITED: u32 = membarrier_cmd::MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED as u32;
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
    /// The peer finished sending (or this end stopped reading).
    pub const RDHUP: i16 = linux_raw_sys::general::POLLRDHUP as i16;
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
    pub const WAIT_BITSET: u32 = linux_raw_sys::general::FUTEX_WAIT_BITSET;
    pub const WAKE_BITSET: u32 = linux_raw_sys::general::FUTEX_WAKE_BITSET;
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
    pub const STACK: u32 = linux_raw_sys::general::RLIMIT_STACK;
    pub const CORE: u32 = linux_raw_sys::general::RLIMIT_CORE;
    pub const NOFILE: u32 = linux_raw_sys::general::RLIMIT_NOFILE;
    /// musl's `RLIM_INFINITY`: every bit of the 64-bit limit.
    pub const INFINITY: u64 = u64::MAX;
}

/// Scheduling policies (`sched_setscheduler`).
pub mod policy {
    pub const NORMAL: u32 = linux_raw_sys::general::SCHED_NORMAL;
    pub const FIFO: u32 = linux_raw_sys::general::SCHED_FIFO;
    pub const RR: u32 = linux_raw_sys::general::SCHED_RR;
    pub const BATCH: u32 = linux_raw_sys::general::SCHED_BATCH;
    pub const IDLE: u32 = linux_raw_sys::general::SCHED_IDLE;
    pub const DEADLINE: u32 = linux_raw_sys::general::SCHED_DEADLINE;
    pub const RESET_ON_FORK: u32 = linux_raw_sys::general::SCHED_RESET_ON_FORK;
}

/// What `getpriority` and `setpriority` name.
pub mod prio {
    pub const PROCESS: u32 = linux_raw_sys::general::PRIO_PROCESS;
    pub const PGRP: u32 = linux_raw_sys::general::PRIO_PGRP;
    pub const USER: u32 = linux_raw_sys::general::PRIO_USER;
}

/// Signals.
pub mod sig {
    /// The layer's panic handler's, which only Veda's build has.
    #[cfg_attr(not(target_os = "none"), allow(dead_code))]
    pub const ABRT: u32 = linux_raw_sys::general::SIGABRT;
    pub const KILL: u32 = linux_raw_sys::general::SIGKILL;
    pub const PIPE: u32 = linux_raw_sys::general::SIGPIPE;
    pub const ALRM: u32 = linux_raw_sys::general::SIGALRM;
    pub const VTALRM: u32 = linux_raw_sys::general::SIGVTALRM;
    pub const PROF: u32 = linux_raw_sys::general::SIGPROF;
    pub const CHLD: u32 = linux_raw_sys::general::SIGCHLD;
    pub const STOP: u32 = linux_raw_sys::general::SIGSTOP;
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
    pub const SA_RESTART: u64 = linux_raw_sys::general::SA_RESTART as u64;
    pub const BLOCK: u32 = linux_raw_sys::general::SIG_BLOCK;
    pub const UNBLOCK: u32 = linux_raw_sys::general::SIG_UNBLOCK;
    pub const SETMASK: u32 = linux_raw_sys::general::SIG_SETMASK;
    /// `si_code`s: sent by `kill` (`SI_USER`, 0), by the kernel
    /// (`SI_KERNEL`: an interval timer's), by `sigqueue`, by a POSIX timer,
    /// by `tkill` or `tgkill`.
    pub const SI_USER: i32 = linux_raw_sys::general::SI_USER as i32;
    pub const SI_KERNEL: i32 = linux_raw_sys::general::SI_KERNEL as i32;
    pub const SI_QUEUE: i32 = linux_raw_sys::general::SI_QUEUE;
    pub const SI_TIMER: i32 = linux_raw_sys::general::SI_TIMER;
    pub const SI_TKILL: i32 = linux_raw_sys::general::SI_TKILL;
    /// `ss_flags` of a signal stack there is not (`sigaltstack`).
    pub const SS_DISABLE: i32 = linux_raw_sys::general::SS_DISABLE as i32;
}

/// Interval and POSIX timers: `setitimer`'s timers, `sigevent`'s ways of
/// telling of an expiration, `timer_settime`'s flag.
pub mod timer {
    pub const ITIMER_REAL: u32 = linux_raw_sys::general::ITIMER_REAL;
    pub const ITIMER_VIRTUAL: u32 = linux_raw_sys::general::ITIMER_VIRTUAL;
    pub const ITIMER_PROF: u32 = linux_raw_sys::general::ITIMER_PROF;
    pub const SIGEV_SIGNAL: i32 = linux_raw_sys::general::SIGEV_SIGNAL as i32;
    pub const SIGEV_NONE: i32 = linux_raw_sys::general::SIGEV_NONE as i32;
    pub const SIGEV_THREAD_ID: i32 = linux_raw_sys::general::SIGEV_THREAD_ID as i32;
    pub const TIMER_ABSTIME: u32 = linux_raw_sys::general::TIMER_ABSTIME;
}

/// `struct itimerval`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Itimerval {
    pub interval: Timeval,
    pub value: Timeval,
}

/// The kernel's `struct sigevent`: how a POSIX timer tells of its
/// expirations (`notify`): with signal `signo` and `value`, to the process
/// or (`SIGEV_THREAD_ID`) thread `tid`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Sigevent {
    pub value: u64,
    pub signo: i32,
    pub notify: i32,
    pub tid: i32,
    pub _pad: [i32; 11],
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

/// `struct itimerspec`: a timer's period and the time to its next
/// expiration.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Itimerspec {
    pub interval: Timespec,
    pub value: Timespec,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
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
#[derive(Clone, Copy)]
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
    /// A datagram was longer than the buffer (and, asked for, its length).
    pub const TRUNC: u32 = linux_raw_sys::net::MSG_TRUNC;
    /// Ancillary data was cut short: it did not fit.
    pub const CTRUNC: u32 = linux_raw_sys::net::MSG_CTRUNC;
    /// Descriptors received (`SCM_RIGHTS`) are closed on `exec`.
    pub const CMSG_CLOEXEC: u32 = linux_raw_sys::net::MSG_CMSG_CLOEXEC;
    /// `recvmmsg`: wait for the first message alone (Linux's, which
    /// `linux-raw-sys`'s headers do not have).
    pub const WAITFORONE: u32 = 0x10000;
}

/// `epoll`'s events, flags and operations. The events are `poll`'s.
pub mod epoll {
    pub const IN: u32 = linux_raw_sys::general::EPOLLIN;
    pub const OUT: u32 = linux_raw_sys::general::EPOLLOUT;
    pub const ERR: u32 = linux_raw_sys::general::EPOLLERR;
    pub const HUP: u32 = linux_raw_sys::general::EPOLLHUP;
    pub const RDNORM: u32 = linux_raw_sys::general::EPOLLRDNORM;
    pub const WRNORM: u32 = linux_raw_sys::general::EPOLLWRNORM;
    pub const RDHUP: u32 = linux_raw_sys::general::EPOLLRDHUP;
    pub const EXCLUSIVE: u32 = linux_raw_sys::general::EPOLLEXCLUSIVE;
    pub const ONESHOT: u32 = linux_raw_sys::general::EPOLLONESHOT;
    pub const ET: u32 = linux_raw_sys::general::EPOLLET;
    pub const CTL_ADD: u32 = linux_raw_sys::general::EPOLL_CTL_ADD;
    pub const CTL_DEL: u32 = linux_raw_sys::general::EPOLL_CTL_DEL;
    pub const CTL_MOD: u32 = linux_raw_sys::general::EPOLL_CTL_MOD;
    pub const CLOEXEC: u32 = linux_raw_sys::general::EPOLL_CLOEXEC;
}

/// `timerfd`'s flags.
pub mod tfd {
    pub const CLOEXEC: u32 = linux_raw_sys::general::TFD_CLOEXEC;
    pub const NONBLOCK: u32 = linux_raw_sys::general::TFD_NONBLOCK;
    pub const TIMER_ABSTIME: u32 = linux_raw_sys::general::TFD_TIMER_ABSTIME;
    pub const TIMER_CANCEL_ON_SET: u32 = linux_raw_sys::general::TFD_TIMER_CANCEL_ON_SET;
}

/// `eventfd`'s flags.
pub mod efd {
    pub const SEMAPHORE: u32 = linux_raw_sys::general::EFD_SEMAPHORE;
    pub const CLOEXEC: u32 = linux_raw_sys::general::EFD_CLOEXEC;
    pub const NONBLOCK: u32 = linux_raw_sys::general::EFD_NONBLOCK;
}

/// Sockets: address families, types, levels and options.
pub mod sock {
    pub const AF_UNSPEC: u16 = linux_raw_sys::net::AF_UNSPEC as u16;
    pub const AF_UNIX: u16 = linux_raw_sys::net::AF_UNIX as u16;
    pub const AF_INET: u16 = linux_raw_sys::net::AF_INET as u16;
    pub const AF_INET6: u16 = linux_raw_sys::net::AF_INET6 as u16;
    pub const STREAM: u32 = linux_raw_sys::net::SOCK_STREAM;
    pub const DGRAM: u32 = linux_raw_sys::net::SOCK_DGRAM;
    pub const SEQPACKET: u32 = linux_raw_sys::net::SOCK_SEQPACKET;
    /// The flags `socket`'s type may carry beside the type itself.
    pub const NONBLOCK: u32 = linux_raw_sys::general::O_NONBLOCK;
    pub const CLOEXEC: u32 = linux_raw_sys::general::O_CLOEXEC;
    pub const IPPROTO_IP: u32 = linux_raw_sys::net::IPPROTO_IP as u32;
    pub const IPPROTO_TCP: u32 = linux_raw_sys::net::IPPROTO_TCP as u32;
    pub const IPPROTO_UDP: u32 = linux_raw_sys::net::IPPROTO_UDP as u32;
    pub const IPPROTO_IPV6: u32 = linux_raw_sys::net::IPPROTO_IPV6 as u32;
    pub const SOL_SOCKET: u32 = linux_raw_sys::net::SOL_SOCKET;
    pub const SO_REUSEADDR: u32 = linux_raw_sys::net::SO_REUSEADDR;
    pub const SO_TYPE: u32 = linux_raw_sys::net::SO_TYPE;
    pub const SO_ERROR: u32 = linux_raw_sys::net::SO_ERROR;
    pub const SO_BROADCAST: u32 = linux_raw_sys::net::SO_BROADCAST;
    pub const SO_SNDBUF: u32 = linux_raw_sys::net::SO_SNDBUF;
    pub const SO_RCVBUF: u32 = linux_raw_sys::net::SO_RCVBUF;
    pub const SO_KEEPALIVE: u32 = linux_raw_sys::net::SO_KEEPALIVE;
    pub const SO_OOBINLINE: u32 = linux_raw_sys::net::SO_OOBINLINE;
    pub const SO_LINGER: u32 = linux_raw_sys::net::SO_LINGER;
    pub const SO_REUSEPORT: u32 = linux_raw_sys::net::SO_REUSEPORT;
    pub const SO_RCVLOWAT: u32 = linux_raw_sys::net::SO_RCVLOWAT;
    pub const SO_SNDLOWAT: u32 = linux_raw_sys::net::SO_SNDLOWAT;
    pub const SO_RCVTIMEO: u32 = linux_raw_sys::net::SO_RCVTIMEO;
    pub const SO_SNDTIMEO: u32 = linux_raw_sys::net::SO_SNDTIMEO;
    pub const SO_ACCEPTCONN: u32 = linux_raw_sys::net::SO_ACCEPTCONN;
    pub const SO_PROTOCOL: u32 = linux_raw_sys::net::SO_PROTOCOL;
    pub const SO_DOMAIN: u32 = linux_raw_sys::net::SO_DOMAIN;
    pub const SO_PASSCRED: u32 = linux_raw_sys::net::SO_PASSCRED;
    pub const SO_PEERCRED: u32 = linux_raw_sys::net::SO_PEERCRED;
    /// Ancillary data: descriptors, and who sent a message.
    pub const SCM_RIGHTS: u32 = linux_raw_sys::net::SCM_RIGHTS;
    pub const SCM_CREDENTIALS: u32 = linux_raw_sys::net::SCM_CREDENTIALS;
    pub const TCP_NODELAY: u32 = linux_raw_sys::net::TCP_NODELAY;
    pub const TCP_MAXSEG: u32 = linux_raw_sys::net::TCP_MAXSEG;
    pub const TCP_KEEPIDLE: u32 = linux_raw_sys::net::TCP_KEEPIDLE;
    pub const TCP_KEEPINTVL: u32 = linux_raw_sys::net::TCP_KEEPINTVL;
    pub const TCP_KEEPCNT: u32 = linux_raw_sys::net::TCP_KEEPCNT;
    pub const IP_TOS: u32 = linux_raw_sys::net::IP_TOS;
    pub const IP_TTL: u32 = linux_raw_sys::net::IP_TTL;
    pub const IPV6_UNICAST_HOPS: u32 = linux_raw_sys::net::IPV6_UNICAST_HOPS;
    pub const IPV6_V6ONLY: u32 = linux_raw_sys::net::IPV6_V6ONLY;
    pub const SHUT_RD: u32 = linux_raw_sys::net::SHUT_RD;
    pub const SHUT_WR: u32 = linux_raw_sys::net::SHUT_WR;
    pub const SHUT_RDWR: u32 = linux_raw_sys::net::SHUT_RDWR;
}

/// `struct sockaddr_in`: the port and address in network byte order.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SockaddrIn {
    pub family: u16,
    pub port: [u8; 2],
    pub addr: [u8; 4],
    pub zero: [u8; 8],
}

/// `struct sockaddr_in6`: the port, flow label and address in network byte
/// order.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SockaddrIn6 {
    pub family: u16,
    pub port: [u8; 2],
    pub flowinfo: [u8; 4],
    pub addr: [u8; 16],
    pub scope_id: u32,
}

/// `struct sockaddr_un`: a path (`sun_path`), not always ended by a NUL.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SockaddrUn {
    pub family: u16,
    pub path: [u8; 108],
}

/// `struct ucred` (`SO_PEERCRED`, `SCM_CREDENTIALS`).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Ucred {
    pub pid: i32,
    pub uid: u32,
    pub gid: u32,
}

/// `struct cmsghdr`: the header of an ancillary message, which its data
/// follows (aligned as the header is).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Cmsghdr {
    /// The header's and the data's bytes.
    pub len: usize,
    pub level: i32,
    pub kind: i32,
}

/// `struct mmsghdr` (`sendmmsg`, `recvmmsg`): a message, and how many bytes
/// went or came.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Mmsghdr {
    pub hdr: Msghdr,
    pub len: u32,
}

/// `struct linger`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Linger {
    pub onoff: i32,
    pub linger: i32,
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
/// get it. A POSIX timer's signal has the timer's id where `pid` is, its
/// overruns where `uid` is, and its value ([`Siginfo::set_value`]) where
/// `status` is.
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

impl Siginfo {
    /// Sets `si_value` (a `union sigval`: 8 bytes, where `status` begins).
    pub fn set_value(&mut self, value: u64) {
        let bytes = value.to_ne_bytes();
        self.status = i32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        self._rest[..4].copy_from_slice(&bytes[4..]);
    }

    /// `si_value` ([`Siginfo::set_value`]).
    pub fn value(&self) -> u64 {
        let low = self.status.to_ne_bytes();
        let high = &self._rest[..4];
        u64::from_ne_bytes([low[0], low[1], low[2], low[3], high[0], high[1], high[2], high[3]])
    }
}

/// Directory entry types of `getdents64`.
pub mod dt {
    pub const CHR: u8 = linux_raw_sys::general::DT_CHR as u8;
    pub const DIR: u8 = linux_raw_sys::general::DT_DIR as u8;
    pub const REG: u8 = linux_raw_sys::general::DT_REG as u8;
    pub const SOCK: u8 = linux_raw_sys::general::DT_SOCK as u8;
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
        assert_eq!(size_of::<SockaddrIn>(), 16);
        assert_eq!(size_of::<SockaddrIn6>(), 28);
        assert_eq!(offset_of!(SockaddrIn6, scope_id), 24);
        assert_eq!(size_of::<Linger>(), 8);
        assert_eq!(size_of::<SockaddrUn>(), 110);
        assert_eq!(size_of::<Ucred>(), 12);
        assert_eq!(size_of::<Cmsghdr>(), 16);
        assert_eq!(offset_of!(Cmsghdr, kind), 12);
        assert_eq!(size_of::<Mmsghdr>(), 64);
        assert_eq!(size_of::<EpollEvent>(), 12);
        assert_eq!(size_of::<Itimerval>(), 32);
        assert_eq!(size_of::<Sigevent>(), 64);
        assert_eq!(offset_of!(Sigevent, tid), 16);
    }

    #[test]
    fn a_timers_value_is_where_siginfo_keeps_sigval() {
        let mut info = Siginfo::default();
        info.set_value(0x1122_3344_5566_7788);
        assert_eq!(info.value(), 0x1122_3344_5566_7788);
        // `si_value` is the 8 bytes from offset 24.
        let bytes = unsafe { core::slice::from_raw_parts(&info as *const Siginfo as *const u8, 128) };
        assert_eq!(bytes[24..32], 0x1122_3344_5566_7788u64.to_ne_bytes());
    }

    #[test]
    fn timespecs() {
        assert_eq!(Timespec::from_ns(1_500_000_001), Timespec { sec: 1, nsec: 500_000_001 });
        assert_eq!(Timespec { sec: 2, nsec: 5 }.to_ns(), Some(2_000_000_005));
        assert_eq!(Timespec { sec: -1, nsec: 0 }.to_ns(), None);
        assert_eq!(Timespec { sec: 0, nsec: 1_000_000_000 }.to_ns(), None);
    }
}
