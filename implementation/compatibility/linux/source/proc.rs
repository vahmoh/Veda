// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The files only Linux has, which Linux compatibility makes as they are
//! opened: `/proc` (the process itself: its program, its memory's areas,
//! its arguments; the machine: its processors, memory, uptime; the
//! kernel's names), what `/sys` says of the processors, and
//! `/etc/resolv.conf` (the network service's DNS servers, whatever the
//! Linux root's says). Programs read them for how many processors there
//! are, where their own stack and executable are, and the like.
//!
//! A file is made whole as it is opened, as a memory file of its bytes,
//! which the descriptor then reads (and `fstat` describes); a link
//! (`/proc/self/exe`) is what `readlink` reads, and what opening it opens.
//! A descriptor's link (`/proc/self/fd/N`) reads as Linux's says what the
//! descriptor is open on (`posix::host::fd_link`); opened or looked at, it
//! is the descriptor's file, as the POSIX layer's `/dev/fd/N` is.
//! The calls that name paths (`open`, `stat`, `access`, `readlink` and
//! their `at` forms) look here first ([`intercept`]); every other path is
//! the POSIX layer's. Only the process's own `/proc/<pid>` is there:
//! another's is not this process's to see.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::linux::errno::*;
use crate::linux::{at, mode, nr, o};
use crate::memory::{self, Kind};

/// What a path of Linux compatibility's is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    /// A file, its bytes.
    File(Vec<u8>),
    /// A symbolic link, to the program's path.
    Link(String),
    /// A descriptor of the program's, a link that reads as what it is open
    /// on.
    Fd(i32, String),
    /// A directory.
    Dir,
}

/// What the process was started with, for `/proc/self`: its arguments,
/// environment and auxiliary vector (as `execve` last gave them), and its
/// program's path.
pub struct Started {
    pub args: Vec<Vec<u8>>,
    pub env: Vec<Vec<u8>>,
    pub auxv: Vec<u8>,
    pub exe: String,
}

static STARTED: runtime::sync::Mutex<Option<Started>> = runtime::sync::Mutex::new(None);

/// What the process was started with, held still ([`hold`]).
pub struct Held {
    _started: runtime::sync::MutexGuard<'static, Option<Started>>,
}

/// Holds what the process was started with still while a copy of it is
/// made (`fork`), which has it as it is.
pub fn hold() -> Held {
    Held { _started: STARTED.lock() }
}

/// Notes what the program was started with (at the start, and as
/// `execve` replaces it).
pub fn started(s: Started) {
    *STARTED.lock() = Some(s);
}

/// The program's path, as `/proc/self/exe` names it.
pub fn exe() -> String {
    STARTED.lock().as_ref().map(|s| s.exe.clone()).unwrap_or_default()
}

/// Linux compatibility's node at the program's path `path`, if it has one
/// there.
pub fn node(path: &str) -> Option<Node> {
    if path == "/etc/resolv.conf" {
        return Some(Node::File(resolv_conf().into_bytes()));
    }
    if let Some(rest) = strip(path, "/sys/devices/system/cpu") {
        let cpus = system().cpu_count.max(1);
        return match rest {
            "" => Some(Node::Dir),
            "/online" | "/possible" | "/present" => Some(Node::File(format!("0-{}\n", cpus - 1).into_bytes())),
            _ => None,
        };
    }
    let rest = strip(path, "/proc")?;
    let pid = posix::host::pid();
    if rest.is_empty() {
        return Some(Node::Dir);
    }
    // The process itself, by whichever name.
    let mine = format!("/{pid}");
    for name in ["/self", "/thread-self", mine.as_str()] {
        if let Some(own) = strip(rest, name) {
            return own_node(own, pid);
        }
    }
    let text = match rest {
        "/cpuinfo" => cpuinfo(),
        "/meminfo" => meminfo(),
        "/stat" => stat(),
        "/uptime" => uptime(),
        "/loadavg" => format!("0.00 0.00 0.00 1/{} {pid}\n", system().thread_count.max(1)),
        "/version" => format!("Linux version {} (veda) #1 SMP PREEMPT_DYNAMIC Veda\n", crate::RELEASE),
        "/filesystems" => String::from("nodev\tproc\nnodev\tsysfs\nnodev\ttmpfs\n"),
        "/mounts" => mounts(),
        "/sys/kernel/osrelease" => format!("{}\n", crate::RELEASE),
        "/sys/kernel/ostype" => String::from("Linux\n"),
        "/sys/kernel/hostname" => format!("{}\n", hostname()),
        "/sys/kernel/pid_max" => String::from("1073741824\n"),
        "/sys/kernel/threads-max" => String::from("65536\n"),
        "/sys/kernel/random/uuid" => uuid(),
        "/sys/kernel/random/boot_id" => boot_id(),
        "/sys/vm/overcommit_memory" => String::from("0\n"),
        "/sys/fs/file-max" => String::from("1048576\n"),
        "/sys" | "/sys/kernel" | "/sys/kernel/random" | "/sys/vm" | "/sys/fs" => return Some(Node::Dir),
        _ => return None,
    };
    Some(Node::File(text.into_bytes()))
}

/// `path` with `prefix` taken off, if it is in it, or is it.
fn strip<'a>(path: &'a str, prefix: &str) -> Option<&'a str> {
    let rest = path.strip_prefix(prefix)?;
    (rest.is_empty() || rest.starts_with('/')).then_some(rest)
}

/// `/proc/self`'s nodes.
fn own_node(rest: &str, pid: i32) -> Option<Node> {
    let started = STARTED.lock();
    let s = started.as_ref()?;
    let text: Vec<u8> = match rest {
        "" | "/fd" | "/task" => return Some(Node::Dir),
        _ if rest.starts_with("/fd/") => {
            let fd = rest["/fd/".len()..].parse::<i32>().ok().filter(|fd| *fd >= 0)?;
            return posix::host::fd_link(fd).ok().map(|target| Node::Fd(fd, target));
        }
        "/exe" => return Some(Node::Link(s.exe.clone())),
        "/cwd" => return Some(Node::Link(posix::host::cwd())),
        "/root" => return Some(Node::Link(String::from("/"))),
        "/cmdline" => s.args.iter().flat_map(|a| a.iter().copied().chain([0])).collect(),
        "/environ" => s.env.iter().flat_map(|a| a.iter().copied().chain([0])).collect(),
        "/auxv" => s.auxv.clone(),
        "/comm" => format!("{}\n", comm(&s.exe)).into_bytes(),
        "/maps" => maps().into_bytes(),
        "/stat" => own_stat(pid, &s.exe).into_bytes(),
        "/statm" => {
            let pages = resident() / abi::PAGE_SIZE as u64;
            format!("{pages} {pages} 0 0 0 {pages} 0\n").into_bytes()
        }
        "/status" => status(pid, &s.exe).into_bytes(),
        "/mounts" => mounts().into_bytes(),
        "/mountinfo" => mountinfo().into_bytes(),
        "/limits" => limits().into_bytes(),
        "/oom_score_adj" | "/oom_adj" | "/oom_score" => b"0\n".to_vec(),
        "/loginuid" => b"1000\n".to_vec(),
        "/cgroup" => b"0::/\n".to_vec(),
        _ => return None,
    };
    Some(Node::File(text))
}

fn system() -> abi::SystemInfo {
    runtime::object::system_info().unwrap_or_default()
}

/// The program's name, as the kernel cuts it (`TASK_COMM_LEN`).
fn comm(exe: &str) -> &str {
    let name = exe.rsplit('/').next().unwrap_or(exe);
    &name[..name.floor_char_boundary(15)]
}

/// The bytes of memory the process holds.
fn resident() -> u64 {
    runtime::object::process_self_info().map_or(0, |i| i.memory_bytes)
}

fn hostname() -> String {
    let mut uts = [0u8; 6 * 65];
    // SAFETY: a `utsname` of our own.
    let _ = unsafe {
        posix::host::syscall(linux_raw_sys::general::__NR_uname as usize, [uts.as_mut_ptr() as usize, 0, 0, 0, 0, 0])
    };
    let node = &uts[65..130];
    String::from_utf8_lossy(&node[..node.iter().position(|&b| b == 0).unwrap_or(65)]).into_owned()
}

fn resolv_conf() -> String {
    let mut text = String::from("# The network service's DNS servers (Veda's Linux compatibility).\n");
    for server in posix::host::dns_servers() {
        text.push_str(&format!("nameserver {server}\n"));
    }
    text
}

fn cpuinfo() -> String {
    let info = system();
    let brand = String::from_utf8_lossy(&info.cpu_brand);
    let brand = brand.trim_end_matches('\0').trim();
    let cpus = info.cpu_count.max(1);
    let mut text = String::new();
    for cpu in 0..cpus {
        text.push_str(&format!(
            "processor\t: {cpu}\nmodel name\t: {brand}\nphysical id\t: 0\nsiblings\t: {cpus}\ncore id\t\t: {cpu}\n\
             cpu cores\t: {cpus}\napicid\t\t: {cpu}\nflags\t\t: {}\n\n",
            crate::architecture::cpu_flags()
        ));
    }
    text
}

fn meminfo() -> String {
    let info = system();
    let (total, free) = (info.total_memory / 1024, info.free_memory / 1024);
    format!(
        "MemTotal:       {total:>8} kB\nMemFree:        {free:>8} kB\nMemAvailable:   {free:>8} kB\n\
         Buffers:               0 kB\nCached:                0 kB\nSwapCached:            0 kB\n\
         SwapTotal:             0 kB\nSwapFree:              0 kB\n"
    )
}

/// `/proc/stat`: the processors' time, in hundredths of a second, as
/// busy (user) and idle.
fn stat() -> String {
    let info = system();
    let cpus = u64::from(info.cpu_count.max(1));
    let ticks = |ns: u64| ns / 10_000_000;
    let (all, idle) = (ticks(info.uptime_ns) * cpus, ticks(info.idle_time_ns));
    let busy = all.saturating_sub(idle);
    let mut text = format!("cpu  {busy} 0 0 {idle} 0 0 0 0 0 0\n");
    for cpu in 0..cpus {
        text.push_str(&format!("cpu{cpu} {} 0 0 {} 0 0 0 0 0 0\n", busy / cpus, idle / cpus));
    }
    let boot = (runtime::time::utc_time_ns().saturating_sub(info.uptime_ns)) / 1_000_000_000;
    text.push_str(&format!("btime {boot}\nprocesses {}\nprocs_running 1\nprocs_blocked 0\n", info.process_count));
    text
}

fn uptime() -> String {
    let info = system();
    let (up, idle) = (info.uptime_ns / 10_000_000, info.idle_time_ns / 10_000_000);
    format!("{}.{:02} {}.{:02}\n", up / 100, up % 100, idle / 100, idle % 100)
}

fn mounts() -> String {
    String::from(
        "veda / veda rw 0 0\nproc /proc proc rw,nosuid,nodev,noexec 0 0\n\
         sysfs /sys sysfs ro,nosuid,nodev,noexec 0 0\ntmpfs /tmp tmpfs rw 0 0\n",
    )
}

fn mountinfo() -> String {
    String::from(
        "1 0 0:1 / / rw - veda veda rw\n2 1 0:2 / /proc rw,nosuid,nodev,noexec - proc proc rw\n\
         3 1 0:3 / /sys ro,nosuid,nodev,noexec - sysfs sysfs ro\n4 1 0:4 / /tmp rw - tmpfs tmpfs rw\n",
    )
}

fn limits() -> String {
    String::from(
        "Limit                     Soft Limit           Hard Limit           Units     \n\
         Max stack size            8388608              unlimited            bytes     \n\
         Max open files            1024                 1024                 files     \n",
    )
}

/// `/proc/self/maps`: the program's areas, as Linux lists them.
fn maps() -> String {
    let mut text = String::new();
    for (start, a) in memory::areas() {
        let p = |bit: u32, c: char| if a.prot & bit != 0 { c } else { '-' };
        use crate::linux::mman::{PROT_EXEC, PROT_READ, PROT_WRITE};
        let name = match &a.kind {
            Kind::Image(path) => path.as_str(),
            Kind::Stack => "[stack]",
            Kind::Heap => "[heap]",
            Kind::Vdso => "[vdso]",
            Kind::Vvar => "[vvar]",
            Kind::Mapped { .. } => "",
        };
        let shared = matches!(a.kind, Kind::Mapped { shared: true, .. });
        text.push_str(&format!(
            "{start:08x}-{:08x} {}{}{}{} 00000000 00:00 0{}{name}\n",
            a.end,
            p(PROT_READ, 'r'),
            p(PROT_WRITE, 'w'),
            p(PROT_EXEC, 'x'),
            if shared { 's' } else { 'p' },
            if name.is_empty() { "" } else { "                          " },
        ));
    }
    text
}

fn own_stat(pid: i32, exe: &str) -> String {
    let ppid = runtime::object::process_self_info().map_or(0, |i| (i.parent_koid & 0x3fff_ffff) as i32);
    let threads = crate::task::live();
    let rss = resident() / abi::PAGE_SIZE as u64;
    let stack = memory::areas().into_iter().find(|(_, a)| a.kind == Kind::Stack).map_or(0, |(_, a)| a.end);
    format!(
        "{pid} ({}) R {ppid} {pid} {pid} 0 -1 4194304 0 0 0 0 0 0 0 0 20 0 {threads} 0 0 0 {rss} \
         18446744073709551615 0 0 {stack} 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n",
        comm(exe)
    )
}

fn status(pid: i32, exe: &str) -> String {
    let ppid = runtime::object::process_self_info().map_or(0, |i| (i.parent_koid & 0x3fff_ffff) as i32);
    let kb = resident() / 1024;
    format!(
        "Name:\t{}\nUmask:\t0022\nState:\tR (running)\nTgid:\t{pid}\nPid:\t{pid}\nPPid:\t{ppid}\n\
         Uid:\t1000\t1000\t1000\t1000\nGid:\t1000\t1000\t1000\t1000\nFDSize:\t1024\nVmRSS:\t{kb:>8} kB\n\
         Threads:\t{}\nSigQ:\t0/65536\nCapInh:\t0000000000000000\nCapPrm:\t0000000000000000\n\
         CapEff:\t0000000000000000\nCapBnd:\t000001ffffffffff\nNoNewPrivs:\t1\nSeccomp:\t0\n\
         Cpus_allowed_list:\t0-{}\n",
        comm(exe),
        crate::task::live(),
        system().cpu_count.max(1) - 1
    )
}

fn uuid() -> String {
    let mut b = [0u8; 16];
    let _ = runtime::object::try_random_bytes(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}\n", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

/// One boot's id, the same for every process of it: drawn from what stays
/// the same until the system starts again (its start in UTC).
fn boot_id() -> String {
    let info = system();
    let boot = runtime::time::utc_time_ns().saturating_sub(info.uptime_ns) / 1_000_000_000;
    format!("{boot:08x}-0000-4000-8000-{:012x}\n", boot & 0xffff_ffff_ffff)
}

/// The calls that name paths, if the path is one of Linux compatibility's:
/// what the call returns. `None` for the others, which the POSIX layer
/// carries out.
pub fn intercept(n: usize, a: [usize; 6]) -> Option<Result<usize, isize>> {
    // Which argument is the directory and which the name, and what the
    // call is.
    let (dirfd, name, call) = match n {
        nr::OPENAT => (a[0] as i32, a[1], Call::Open(a[2] as u32)),
        nr::NEWFSTATAT => (a[0] as i32, a[1], Call::Stat(a[2], a[3] as u32)),
        nr::STATX => (a[0] as i32, a[1], Call::Statx(a[2] as u32, a[4])),
        nr::FACCESSAT | nr::FACCESSAT2 => (a[0] as i32, a[1], Call::Access),
        nr::READLINKAT => (a[0] as i32, a[1], Call::Readlink(a[2], a[3])),
        _ => old_form(n, a)?,
    };
    // SAFETY: the program's memory, which a fault reading it ends.
    let name = unsafe { crate::user::string(name, 4095) }?;
    if name.is_empty() {
        return None;
    }
    let path = posix::host::program_path_at(dirfd, &name).ok()?;
    let found = node(&path)?;
    Some(carry_out(call, &path, found))
}

/// What a call that names a path does.
#[derive(Debug, Clone, Copy)]
enum Call {
    /// `open`'s flags.
    Open(u32),
    /// `stat` (where its buffer is, and `AT_SYMLINK_NOFOLLOW`).
    Stat(usize, u32),
    /// `statx` (its flags, where its buffer is).
    Statx(u32, usize),
    Access,
    /// `readlink` (where its buffer is, and its size).
    Readlink(usize, usize),
}

/// The architecture's older forms of these calls (no directory: the
/// working directory's).
#[cfg(target_arch = "x86_64")]
fn old_form(n: usize, a: [usize; 6]) -> Option<(i32, usize, Call)> {
    Some(match n {
        nr::OPEN => (at::FDCWD, a[0], Call::Open(a[1] as u32)),
        nr::STAT => (at::FDCWD, a[0], Call::Stat(a[1], 0)),
        nr::LSTAT => (at::FDCWD, a[0], Call::Stat(a[1], at::SYMLINK_NOFOLLOW)),
        nr::ACCESS => (at::FDCWD, a[0], Call::Access),
        nr::READLINK => (at::FDCWD, a[0], Call::Readlink(a[1], a[2])),
        _ => return None,
    })
}

#[cfg(not(target_arch = "x86_64"))]
fn old_form(_n: usize, _a: [usize; 6]) -> Option<(i32, usize, Call)> {
    None
}

fn carry_out(call: Call, path: &str, found: Node) -> Result<usize, isize> {
    // A descriptor followed is its file, as `/dev/fd/N` is.
    let found = match (call, found) {
        (Call::Open(flags), Node::Fd(fd, _)) if flags & o::NOFOLLOW == 0 => Node::Link(format!("/dev/fd/{fd}")),
        (Call::Stat(_, flags) | Call::Statx(flags, _), Node::Fd(fd, _)) if flags & at::SYMLINK_NOFOLLOW == 0 => {
            Node::Link(format!("/dev/fd/{fd}"))
        }
        (_, Node::Fd(_, target)) => Node::Link(target),
        (_, found) => found,
    };
    match (call, found) {
        (Call::Access, _) => Ok(0),
        (Call::Readlink(buf, size), Node::Link(target)) => {
            let n = target.len().min(size);
            // SAFETY: the program's memory, which a fault writing it ends.
            unsafe { crate::user::write_bytes(buf, &target.as_bytes()[..n]).ok_or(EFAULT)? };
            Ok(n)
        }
        (Call::Readlink(..), _) => Err(EINVAL),
        // A link opened or looked at is what it names (but `lstat`).
        (Call::Open(flags), Node::Link(target)) if flags & o::NOFOLLOW == 0 => {
            let target = alloc::ffi::CString::new(target).map_err(|_| ENOENT)?;
            // SAFETY: a path of our own.
            unsafe {
                posix::host::syscall(
                    nr::OPENAT,
                    [at::FDCWD as usize, target.as_ptr() as usize, flags as usize, 0, 0, 0],
                )
            }
        }
        (Call::Stat(buf, flags), Node::Link(target)) if flags & at::SYMLINK_NOFOLLOW == 0 => {
            let target = alloc::ffi::CString::new(target).map_err(|_| ENOENT)?;
            // SAFETY: a path of our own; the program's buffer.
            unsafe {
                posix::host::syscall(nr::NEWFSTATAT, [at::FDCWD as usize, target.as_ptr() as usize, buf, 0, 0, 0])
            }
        }
        (Call::Statx(flags, buf), Node::Link(target)) if flags & at::SYMLINK_NOFOLLOW == 0 => {
            let target = alloc::ffi::CString::new(target).map_err(|_| ENOENT)?;
            // SAFETY: as above.
            unsafe { posix::host::syscall(nr::STATX, [at::FDCWD as usize, target.as_ptr() as usize, 0, 0x7ff, buf, 0]) }
        }
        (Call::Open(flags), Node::File(bytes)) => {
            if flags & o::DIRECTORY != 0 {
                return Err(ENOTDIR);
            }
            open_bytes(path, &bytes, flags)
        }
        (Call::Open(_), Node::Dir) => Err(ENOENT),
        (Call::Open(_), Node::Link(_) | Node::Fd(..)) => Err(linux_raw_sys::errno::ELOOP as isize),
        (Call::Stat(buf, _), node) => {
            let (kind, size) = described(&node);
            // SAFETY: the program's memory, which a fault writing it ends.
            unsafe { crate::user::write_bytes(buf, &crate::architecture::stat_bytes(kind, size)).ok_or(EFAULT)? };
            Ok(0)
        }
        (Call::Statx(_, buf), node) => {
            let (kind, size) = described(&node);
            // SAFETY: as above.
            unsafe { crate::user::write_bytes(buf, &statx_bytes(kind, size)).ok_or(EFAULT)? };
            Ok(0)
        }
    }
}

/// What `stat` says of a node: its type and permissions, and its size.
fn described(node: &Node) -> (u32, u64) {
    match node {
        Node::File(bytes) => (mode::S_IFREG | 0o444, bytes.len() as u64),
        Node::Link(target) => (mode::S_IFLNK | 0o777, target.len() as u64),
        Node::Fd(_, target) => (mode::S_IFLNK | 0o700, target.len() as u64),
        Node::Dir => (mode::S_IFDIR | 0o555, 0),
    }
}

/// `struct statx` for a node.
fn statx_bytes(kind: u32, size: u64) -> [u8; 256] {
    let mut b = [0u8; 256];
    // What is given: type, mode, links, owner, size (`STATX_BASIC_STATS`
    // but times and blocks).
    b[0..4].copy_from_slice(&0x7ffu32.to_le_bytes());
    b[4..8].copy_from_slice(&4096u32.to_le_bytes());
    b[16..20].copy_from_slice(&1u32.to_le_bytes());
    b[20..24].copy_from_slice(&1000u32.to_le_bytes());
    b[24..28].copy_from_slice(&1000u32.to_le_bytes());
    b[28..30].copy_from_slice(&(kind as u16).to_le_bytes());
    b[40..48].copy_from_slice(&size.to_le_bytes());
    b
}

/// A descriptor that reads `bytes`: a memory file of them, read from its
/// start, close-on-exec if `flags` say.
fn open_bytes(path: &str, bytes: &[u8], flags: u32) -> Result<usize, isize> {
    let name = alloc::ffi::CString::new(path.rsplit('/').next().unwrap_or("proc")).map_err(|_| EINVAL)?;
    let cloexec = if flags & o::CLOEXEC != 0 { linux_raw_sys::general::MFD_CLOEXEC as usize } else { 0 };
    let memfd = linux_raw_sys::general::__NR_memfd_create as usize;
    // SAFETY: our own name.
    let fd = unsafe { posix::host::syscall(memfd, [name.as_ptr() as usize, cloexec, 0, 0, 0, 0])? };
    let write = linux_raw_sys::general::__NR_pwrite64 as usize;
    let mut done = 0;
    while done < bytes.len() {
        let left = &bytes[done..];
        // SAFETY: our own buffer.
        match unsafe { posix::host::syscall(write, [fd, left.as_ptr() as usize, left.len(), done, 0, 0]) } {
            Ok(n) if n > 0 => done += n,
            _ => {
                let _ =
                    unsafe { posix::host::syscall(linux_raw_sys::general::__NR_close as usize, [fd, 0, 0, 0, 0, 0]) };
                return Err(EIO);
            }
        }
    }
    Ok(fd)
}

const EIO: isize = linux_raw_sys::errno::EIO as isize;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_whole_names() {
        assert_eq!(strip("/proc/self/maps", "/proc"), Some("/self/maps"));
        assert_eq!(strip("/proc", "/proc"), Some(""));
        assert_eq!(strip("/processes", "/proc"), None);
    }

    #[test]
    fn short_names_as_the_kernel_cuts_them() {
        assert_eq!(comm("/usr/bin/a-very-long-program-name"), "a-very-long-pro");
        assert_eq!(comm("/bin/ls"), "ls");
    }
}
