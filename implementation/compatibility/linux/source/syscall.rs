// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The Linux program's system calls: those of a thread's own (its signal
//! frames, its alternate stack, its thread pointer, its robust list),
//! those that make threads or replace the program (`clone`, `execve`),
//! the program's memory (whose areas Linux compatibility keeps,
//! `crate::memory`), the files only Linux has (`/proc`, `crate::proc`),
//! what says the system is Linux (`uname`, `personality`), and the calls
//! for what a program here may not do (mounting, loading modules, tracing
//! another: refused as Linux refuses an unprivileged program). Every other
//! is the POSIX layer's, as it carries out a C program's.

use crate::architecture;
use crate::linux::errno::*;
use crate::linux::{nr, pr};
use crate::task::{self, Task};
use crate::{exec, memory, proc, signal};

/// What carrying a call out came to.
pub enum Outcome {
    /// The call returns this (a value, or `-errno`).
    Return(isize),
    /// The registers are set already (`rt_sigreturn`, a successful
    /// `execve`).
    Done,
}

fn ret(r: Result<usize, isize>) -> Outcome {
    Outcome::Return(posix::host::encode(r))
}

/// Carries out the system call thread `t` made, its result in its
/// registers. One a signal ended keeps the code that says so, to be
/// started again or ended as `crate::signal::deliver` decides.
pub fn handle(t: &mut Task) {
    let (n, a) = architecture::syscall(&t.state);
    match dispatch(t, n, a) {
        Outcome::Done => {}
        Outcome::Return(v) => {
            if matches!(-v, ERESTARTSYS | ERESTARTNOINTR | ERESTARTNOHAND) {
                t.restart = Some(task::Restart { call: n, first: a[0], code: -v });
            }
            architecture::set_result(&mut t.state, v);
        }
    }
}

fn dispatch(t: &mut Task, n: usize, a: [usize; 6]) -> Outcome {
    let a64 = a.map(|v| v as u64);
    match n {
        nr::RT_SIGRETURN => {
            signal::sigreturn(t);
            Outcome::Done
        }
        nr::SIGALTSTACK => ret(signal::sigaltstack(t, a[0], a[1])),
        // SAFETY: the program's arguments, as the POSIX layer's mmap takes them.
        nr::MMAP => {
            ret(unsafe { memory::mmap(a64[0], a64[1], a[2] as u32, a[3] as u32, a[4], a64[5]) }.map(|v| v as usize))
        }
        nr::MUNMAP => ret(memory::munmap(a64[0], a64[1]).map(|v| v as usize)),
        nr::MPROTECT => ret(memory::mprotect(a64[0], a64[1], a[2] as u32).map(|v| v as usize)),
        nr::MREMAP => ret(memory::mremap(a64[0], a64[1], a64[2], a[3] as u32, a64[4]).map(|v| v as usize)),
        nr::MADVISE => ret(memory::madvise(a64[0], a64[1], a[2] as u32)),
        nr::BRK => Outcome::Return(memory::brk(a64[0])),
        nr::CLONE => ret(clone(t, a64)),
        nr::CLONE3 => ret(clone3(t, a[0], a[1])),
        nr::EXECVE => exec::execve(t, at_cwd(), a[0], a[1], a[2], 0),
        nr::EXECVEAT => exec::execve(t, a[0] as i32, a[1], a[2], a[3], a[4] as u32),
        nr::EXIT => task::exit(t, a[0] as i32),
        nr::EXIT_GROUP => task::exit_group(t, a[0] as i32),
        nr::SET_ROBUST_LIST => ret(task::set_robust_list(t, a64[0], a64[1])),
        nr::GET_ROBUST_LIST => ret(task::get_robust_list(t, a[0] as i32, a[1], a[2])),
        nr::UNAME => ret(uname(a[0])),
        nr::PERSONALITY => ret(personality(a[0] as u32)),
        nr::PRCTL => prctl(a),
        nr::CAPGET => ret(capget(a[0], a[1])),
        nr::CAPSET => ret(capset(a[1])),
        // Tracing another program, mounting, changing roots and
        // namespaces, the machine's own state: what Linux refuses a
        // program without privileges.
        nr::PTRACE
        | nr::MOUNT
        | nr::UMOUNT2
        | nr::CHROOT
        | nr::PIVOT_ROOT
        | nr::SETNS
        | nr::REBOOT
        | nr::INIT_MODULE
        | nr::DELETE_MODULE
        | nr::SWAPON
        | nr::SWAPOFF
        | nr::SETTIMEOFDAY
        | nr::CLOCK_SETTIME => Outcome::Return(-EPERM),
        // A namespace of its own is a privilege too (an unprivileged one
        // is not had here).
        nr::UNSHARE => Outcome::Return(-EPERM),
        nr::SECCOMP => Outcome::Return(-EINVAL),
        nr::SENDFILE => ret(sendfile(a[0] as i32, a[1] as i32, a[2], a[3])),
        nr::COPY_FILE_RANGE => ret(copy_file_range(a[0] as i32, a[1], a[2] as i32, a[3], a[4], a[5] as u32)),
        _ => {
            if let Some(r) = architecture_call(t, n, a) {
                return r;
            }
            if let Some(r) = proc::intercept(n, a) {
                return ret(r);
            }
            // SAFETY: the program's arguments, which it passed for this
            // call.
            ret(unsafe { posix::host::syscall(n, a) })
        }
    }
}

/// `AT_FDCWD`, which `execve`'s path is relative to.
fn at_cwd() -> i32 {
    crate::linux::at::FDCWD
}

/// The calls of the architecture's own table.
#[cfg(target_arch = "x86_64")]
fn architecture_call(t: &mut Task, n: usize, a: [usize; 6]) -> Option<Outcome> {
    use architecture::arch_prctl::*;
    const ENODEV: isize = linux_raw_sys::errno::ENODEV as isize;
    Some(match n {
        nr::ARCH_PRCTL => {
            let addr = a[1] as u64;
            match a[0] as u32 {
                SET_FS if addr == 0 || (abi::USER_SPACE_START as u64..abi::USER_SPACE_END as u64).contains(&addr) => {
                    architecture::set_tls(&mut t.state, addr);
                    Outcome::Return(0)
                }
                SET_FS => Outcome::Return(-EPERM),
                // SAFETY: the program's memory, which a fault writing it
                // ends.
                GET_FS => match unsafe { crate::user::write(a[1], architecture::tls(&t.state)) } {
                    Some(()) => Outcome::Return(0),
                    None => Outcome::Return(-EFAULT),
                },
                // No GS of a thread's own is kept.
                // SAFETY: as above.
                GET_GS => match unsafe { crate::user::write(a[1], 0u64) } {
                    Some(()) => Outcome::Return(0),
                    None => Outcome::Return(-EFAULT),
                },
                SET_GS => Outcome::Return(-EINVAL),
                // `cpuid` runs, and faults for no program.
                GET_CPUID => Outcome::Return(1),
                SET_CPUID => Outcome::Return(-ENODEV),
                _ => Outcome::Return(-EINVAL),
            }
        }
        // A child in this process's memory, which it waits for.
        nr::VFORK => {
            use crate::linux::clone::{VFORK, VM};
            let sigchld = u64::from(crate::linux::sig::CHLD);
            ret(task::clone(t, VM | VFORK | sigchld, 0, 0, 0, 0))
        }
        nr::FORK => Outcome::Return(-ENOSYS),
        _ => return None,
    })
}

#[cfg(target_arch = "aarch64")]
fn architecture_call(_t: &mut Task, _n: usize, _a: [usize; 6]) -> Option<Outcome> {
    None
}

/// `clone`, in the architecture's argument order (x86-64: flags, stack,
/// parent's id word, child's id word, thread pointer).
fn clone(t: &mut Task, a: [u64; 6]) -> Result<usize, isize> {
    #[cfg(target_arch = "x86_64")]
    let (flags, stack, ptid, ctid, tls) = (a[0], a[1], a[2], a[3], a[4]);
    #[cfg(target_arch = "aarch64")]
    let (flags, stack, ptid, tls, ctid) = (a[0], a[1], a[2], a[3], a[4]);
    task::clone(t, flags, stack, ptid, ctid, tls)
}

/// `clone3(args, size)`: `struct clone_args`, whose fields after the first
/// eight (`set_tid`, `cgroup`) must be zero.
fn clone3(t: &mut Task, args: usize, size: usize) -> Result<usize, isize> {
    const FIRST: usize = 64;
    if !(FIRST..=4096).contains(&size) {
        return Err(EINVAL);
    }
    let mut bytes = alloc::vec![0u8; size];
    // SAFETY: the program's memory, which a fault reading it ends.
    unsafe { crate::user::read_bytes(args, &mut bytes).ok_or(EFAULT)? };
    if bytes[FIRST..].iter().any(|&b| b != 0) {
        return Err(EINVAL);
    }
    let w = |i: usize| u64::from_le_bytes(bytes[8 * i..8 * i + 8].try_into().unwrap());
    let (flags, _pidfd, child_tid, parent_tid, exit_signal, stack, stack_size, tls) =
        (w(0), w(1), w(2), w(3), w(4), w(5), w(6), w(7));
    // The signal a child's end sends (`SIGCHLD`, for a process) is in its
    // own field here, not in the flags' low byte, as `clone` has it.
    if exit_signal > u64::from(crate::linux::sig::MAX) || flags & crate::linux::clone::SIGNAL != 0 {
        return Err(EINVAL);
    }
    let top = if stack == 0 { 0 } else { stack.checked_add(stack_size).ok_or(EINVAL)? };
    task::clone(t, flags | exit_signal, top, parent_tid, child_tid, tls)
}

/// `uname`: Linux, of a release recent enough for what programs check
/// (the C libraries' oldest kernels, the system calls they try), on this
/// machine, with the system's name for it.
fn uname(at: usize) -> Result<usize, isize> {
    // `struct utsname`: six fields of 65 bytes.
    let mut bytes = [0u8; 6 * 65];
    // The node's and domain's names, as the POSIX layer has them.
    // SAFETY: a `utsname` of our own.
    unsafe {
        posix::host::syscall(linux_raw_sys::general::__NR_uname as usize, [bytes.as_mut_ptr() as usize, 0, 0, 0, 0, 0])?
    };
    let fields: [(usize, &str); 4] =
        [(0, "Linux"), (2, crate::RELEASE), (3, "#1 SMP PREEMPT_DYNAMIC Veda"), (4, architecture::MACHINE)];
    for (i, text) in fields {
        let field = &mut bytes[65 * i..65 * (i + 1)];
        field.fill(0);
        field[..text.len()].copy_from_slice(text.as_bytes());
    }
    // SAFETY: the program's memory, which a fault writing it ends.
    unsafe { crate::user::write_bytes(at, &bytes).ok_or(EFAULT)? };
    Ok(0)
}

/// `personality`: Linux's own, whatever is asked but another system's.
fn personality(p: u32) -> Result<usize, isize> {
    if p == crate::linux::PERSONALITY_QUERY || p & 0xff == crate::linux::PER_LINUX {
        Ok(crate::linux::PER_LINUX as usize)
    } else {
        Err(EINVAL)
    }
}

/// `prctl`: the options a program without privileges has of Linux, which
/// change nothing here (a dying parent's signal, being dumpable, no new
/// privileges, transparent huge pages, who may trace it), and the POSIX
/// layer's (a thread's name).
fn prctl(a: [usize; 6]) -> Outcome {
    let (option, arg) = (a[0] as u32, a[1]);
    let write_int = |at: usize, v: i32| {
        // SAFETY: the program's memory, which a fault writing it ends.
        match unsafe { crate::user::write(at, v) } {
            Some(()) => Outcome::Return(0),
            None => Outcome::Return(-EFAULT),
        }
    };
    match option {
        pr::SET_PDEATHSIG if arg <= crate::linux::sig::MAX as usize => Outcome::Return(0),
        pr::SET_PDEATHSIG => Outcome::Return(-EINVAL),
        pr::GET_PDEATHSIG => write_int(arg, 0),
        pr::GET_DUMPABLE => Outcome::Return(1),
        pr::SET_DUMPABLE if arg <= 1 => Outcome::Return(0),
        pr::SET_DUMPABLE => Outcome::Return(-EINVAL),
        pr::SET_NO_NEW_PRIVS if arg == 1 => Outcome::Return(0),
        pr::SET_NO_NEW_PRIVS => Outcome::Return(-EINVAL),
        // No program here gains privileges: it has none.
        pr::GET_NO_NEW_PRIVS => Outcome::Return(1),
        pr::GET_SECCOMP => Outcome::Return(0),
        pr::SET_SECCOMP => Outcome::Return(-EINVAL),
        pr::CAPBSET_READ if arg <= 40 => Outcome::Return(0),
        pr::CAPBSET_READ => Outcome::Return(-EINVAL),
        pr::SET_CHILD_SUBREAPER | pr::SET_THP_DISABLE | pr::SET_PTRACER => Outcome::Return(0),
        pr::GET_CHILD_SUBREAPER => write_int(arg, 0),
        pr::GET_THP_DISABLE => Outcome::Return(0),
        // SAFETY: the program's arguments.
        _ => ret(unsafe { posix::host::syscall(nr::PRCTL, a) }),
    }
}

/// `capget(header, data)`: version 3, and no capabilities.
fn capget(header: usize, data: usize) -> Result<usize, isize> {
    // SAFETY: the program's memory, which a fault reading or writing it
    // ends.
    unsafe {
        let version: u32 = crate::user::read(header).ok_or(EFAULT)?;
        if version != crate::linux::CAPABILITY_VERSION_3 {
            crate::user::write(header, crate::linux::CAPABILITY_VERSION_3).ok_or(EFAULT)?;
            return if data == 0 { Ok(0) } else { Err(EINVAL) };
        }
        if data != 0 {
            crate::user::zero(data, 24).ok_or(EFAULT)?;
        }
    }
    Ok(0)
}

/// `capset`: none can be had; giving all up is nothing.
fn capset(data: usize) -> Result<usize, isize> {
    let mut caps = [0u8; 24];
    // SAFETY: the program's memory, which a fault reading it ends.
    unsafe { crate::user::read_bytes(data, &mut caps).ok_or(EFAULT)? };
    if caps.iter().all(|&b| b == 0) { Ok(0) } else { Err(EPERM) }
}

/// How much `sendfile` and `copy_file_range` move at a time.
const COPY_CHUNK: usize = 64 << 10;

/// Copies up to `len` bytes from `input` (at the offset at `in_off`, which
/// moves, if given; else its own) to `output` (likewise): what Linux's
/// in-kernel copies do, a read and a write at a time here. Returns how
/// many bytes went; a failure after some went is that number, as Linux
/// returns it. What was read but not written goes back to `input`, where
/// it can: its offset moves by what went, as on Linux.
fn copy(input: i32, in_off: usize, output: i32, out_off: usize, len: usize) -> Result<usize, isize> {
    let (mut from, mut to) = (offset_at(in_off)?, offset_at(out_off)?);
    let mut buf = alloc::vec![0u8; COPY_CHUNK.min(len.max(1))];
    let mut done = 0;
    while done < len {
        let want = buf.len().min(len - done);
        let read = match from {
            // SAFETY: our own buffer.
            Some(o) => unsafe {
                posix::host::syscall(nr::PREAD64, [input as usize, buf.as_mut_ptr() as usize, want, o as usize, 0, 0])
            },
            // SAFETY: as above.
            None => unsafe {
                posix::host::syscall(nr::READ, [input as usize, buf.as_mut_ptr() as usize, want, 0, 0, 0])
            },
        };
        let got = match read {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if done == 0 => return Err(e),
            Err(_) => break,
        };
        let (mut written, mut failed) = (0, None);
        while written < got {
            let part = &buf[written..got];
            let wrote = match to {
                // SAFETY: our own buffer.
                Some(o) => unsafe {
                    posix::host::syscall(
                        nr::PWRITE64,
                        [output as usize, part.as_ptr() as usize, part.len(), (o as usize) + written, 0, 0],
                    )
                },
                // SAFETY: as above.
                None => unsafe {
                    posix::host::syscall(nr::WRITE, [output as usize, part.as_ptr() as usize, part.len(), 0, 0, 0])
                },
            };
            match wrote {
                Ok(n) if n > 0 => written += n,
                Ok(_) => break,
                Err(e) => {
                    failed = Some(e);
                    break;
                }
            }
        }
        if written < got && from.is_none() {
            let back = -((got - written) as i64) as usize;
            // SAFETY: no memory of anyone's.
            let _ = unsafe { posix::host::syscall(nr::LSEEK, [input as usize, back, SEEK_CUR, 0, 0, 0]) };
        }
        done += written;
        from = from.map(|o| o + written as i64);
        to = to.map(|o| o + written as i64);
        if written < got {
            if let Some(e) = failed.filter(|_| done == 0) {
                return Err(e);
            }
            break;
        }
    }
    // The offsets given move by what went.
    // SAFETY: the program's memory, which a fault writing it ends.
    unsafe {
        if let Some(o) = from {
            crate::user::write(in_off, o).ok_or(EFAULT)?;
        }
        if let Some(o) = to {
            crate::user::write(out_off, o).ok_or(EFAULT)?;
        }
    }
    Ok(done)
}

const SEEK_CUR: usize = linux_raw_sys::general::SEEK_CUR as usize;

/// The offset at `at` in the program's memory, if it gives one (`at` is
/// not 0): one of `copy`'s.
fn offset_at(at: usize) -> Result<Option<i64>, isize> {
    if at == 0 {
        return Ok(None);
    }
    // SAFETY: the program's memory, which a fault reading it ends.
    match unsafe { crate::user::read::<i64>(at) } {
        Some(o) if o >= 0 => Ok(Some(o)),
        Some(_) => Err(EINVAL),
        None => Err(EFAULT),
    }
}

/// What `fstat` says of `fd`.
fn fstat(fd: i32) -> Result<linux_raw_sys::general::stat, isize> {
    // SAFETY: a C structure of numbers, for which all bits are valid.
    let mut st: linux_raw_sys::general::stat = unsafe { core::mem::zeroed() };
    // SAFETY: our own structure.
    unsafe { posix::host::syscall(nr::FSTAT, [fd as usize, &raw mut st as usize, 0, 0, 0, 0])? };
    Ok(st)
}

/// Whether `fd` appends what is written to it (`O_APPEND`), into which
/// neither copy writes, as on Linux.
fn appends(fd: i32) -> Result<bool, isize> {
    use linux_raw_sys::general::{F_GETFL, O_APPEND};
    // SAFETY: no memory.
    let flags = unsafe { posix::host::syscall(nr::FCNTL, [fd as usize, F_GETFL as usize, 0, 0, 0, 0])? };
    Ok(flags as u32 & O_APPEND != 0)
}

/// `sendfile(out, in, offset, count)`.
fn sendfile(output: i32, input: i32, offset: usize, count: usize) -> Result<usize, isize> {
    if appends(output)? {
        return Err(EINVAL);
    }
    copy(input, offset, output, 0, count)
}

/// `copy_file_range(in, in_off, out, out_off, len, flags)`, which no flags
/// are for. As on Linux, between regular files only, and not from a part
/// of a file to a part of it that overlaps it.
fn copy_file_range(
    input: i32,
    in_off: usize,
    output: i32,
    out_off: usize,
    len: usize,
    flags: u32,
) -> Result<usize, isize> {
    use linux_raw_sys::general::{S_IFDIR, S_IFMT, S_IFREG};
    if flags != 0 {
        return Err(EINVAL);
    }
    let (a, b) = (fstat(input)?, fstat(output)?);
    let kind = |st: &linux_raw_sys::general::stat| st.st_mode & S_IFMT;
    if kind(&a) == S_IFDIR || kind(&b) == S_IFDIR {
        return Err(EISDIR);
    }
    if kind(&a) != S_IFREG || kind(&b) != S_IFREG {
        return Err(EINVAL);
    }
    if appends(output)? {
        return Err(EBADF);
    }
    if a.st_ino != 0 && (a.st_dev, a.st_ino) == (b.st_dev, b.st_ino) {
        // Where each starts: as given, or its file's offset.
        let at = |fd: i32, given: usize| -> Result<i64, isize> {
            match offset_at(given)? {
                Some(o) => Ok(o),
                // SAFETY: no memory.
                None => {
                    unsafe { posix::host::syscall(nr::LSEEK, [fd as usize, 0, SEEK_CUR, 0, 0, 0]) }.map(|o| o as i64)
                }
            }
        };
        let (from, to) = (at(input, in_off)?, at(output, out_off)?);
        // What would go: no more than is after `from`.
        let size = a.st_size;
        let count = if from >= size { 0 } else { (len.min(i64::MAX as usize) as i64).min(size - from) };
        if to < from.saturating_add(count) && from < to.saturating_add(count) {
            return Err(EINVAL);
        }
    }
    copy(input, in_off, output, out_off, len)
}
