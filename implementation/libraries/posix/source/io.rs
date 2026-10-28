// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Reading, writing and controlling descriptors.

use alloc::sync::Arc;
use alloc::vec::Vec;

use abi::signals::{PEER_CLOSED, PEER_WRITE_DISABLED, READABLE, TERMINATED, WRITABLE};
use abi::{RawHandle, WaitItem, startup};
use runtime::object::{Channel, Socket};

use crate::error::{self, SysResult};
use crate::fd::{self, Description, Object};
use crate::inet::Recv;
use crate::inherit::{Inherited, Pseudo};
use crate::linux::errno::*;
use crate::linux::{self, Iovec, Pollfd, fcntl, ioctl, o, poll, rwf};
use crate::log::Log;
use crate::{time, tty, user, vfs};

/// Reads into `buf` from `desc` (at `offset` for `pread`).
fn read_desc(desc: &Description, buf: &mut [u8], offset: Option<u64>) -> SysResult {
    if !desc.readable() {
        return Err(EBADF);
    }
    match (&desc.object, offset) {
        (Object::File(f), None) => f.read(buf),
        (Object::File(f), Some(at)) => f.pread(buf, at),
        (Object::Memfd(m), at) => m.read(buf, at),
        (
            Object::Stream(_)
            | Object::Inet(_)
            | Object::Unix(_)
            | Object::EventFd(_)
            | Object::TimerFd(_)
            | Object::Null
            | Object::Log(_),
            Some(_),
        ) => Err(ESPIPE),
        (Object::Stream(s), None) => s.read(buf, desc.nonblocking()),
        (Object::EventFd(e), None) => e.read(buf, desc.nonblocking()),
        (Object::TimerFd(t), None) => t.read(buf, desc.nonblocking()),
        (Object::Epoll(_) | Object::Pidfd(_), _) => Err(EINVAL),
        (Object::Inet(s), None) => {
            s.recv(&mut [buf], Recv { nonblock: desc.nonblocking(), ..Recv::default() }).map(|r| r.len)
        }
        (Object::Unix(s), None) => {
            s.recv(&mut [buf], Recv { nonblock: desc.nonblocking(), ..Recv::default() }).map(|r| r.len)
        }
        (Object::Dir(_), _) => Err(EISDIR),
        (Object::Null | Object::Log(_), None) => Ok(0),
    }
}

/// Writes `data` to `desc` (at `offset` for `pwrite`).
fn write_desc(desc: &Description, data: &[u8], offset: Option<u64>) -> SysResult {
    if !desc.writable() {
        return Err(EBADF);
    }
    match (&desc.object, offset) {
        (Object::File(f), None) => f.write(data),
        (Object::File(f), Some(at)) => f.pwrite(data, at),
        (Object::Memfd(m), at) => m.write(data, at, desc.flags() & o::APPEND != 0),
        (
            Object::Stream(_)
            | Object::Inet(_)
            | Object::Unix(_)
            | Object::EventFd(_)
            | Object::TimerFd(_)
            | Object::Null
            | Object::Log(_),
            Some(_),
        ) => Err(ESPIPE),
        (Object::Stream(s), None) => s.write(data, desc.nonblocking()),
        (Object::EventFd(e), None) => {
            let n = e.write(data, desc.nonblocking())?;
            // Each write is an event to edge-triggered epoll.
            desc.changed();
            Ok(n)
        }
        // A timer is only read (as Linux's).
        (Object::TimerFd(_) | Object::Epoll(_) | Object::Pidfd(_), _) => Err(EINVAL),
        (Object::Inet(s), None) => s.send(data, None, desc.nonblocking(), false),
        (Object::Unix(s), None) => s.send(data, None, &[], desc.nonblocking(), false),
        (Object::Dir(_), _) => Err(EISDIR),
        (Object::Null, None) => Ok(data.len()),
        (Object::Log(log), None) => {
            log.write(data);
            Ok(data.len())
        }
    }
}

pub unsafe fn read(fd: i32, buf: usize, len: usize) -> SysResult {
    let desc = fd::get(fd)?;
    // SAFETY: the program passed a buffer of `len` bytes.
    read_desc(&desc, unsafe { user::slice_mut(buf, len)? }, None)
}

pub unsafe fn write(fd: i32, buf: usize, len: usize) -> SysResult {
    let desc = fd::get(fd)?;
    // SAFETY: as above.
    let n = write_desc(&desc, unsafe { user::slice(buf, len)? }, None)?;
    settle(&desc, n, false)
}

/// After `n` bytes were written to `desc`: brings them to persistent
/// storage before the call returns if the descriptor asks for that
/// (`O_SYNC`, `O_DSYNC`) or the call does (`synced`: `RWF_SYNC`,
/// `RWF_DSYNC`).
fn settle(desc: &Description, n: usize, synced: bool) -> SysResult {
    if n > 0 && (synced || desc.synchronous()) && matches!(desc.object, Object::File(_)) {
        sync_all()?;
    }
    Ok(n)
}

/// Has the VFS bring every change to persistent storage now (it otherwise
/// does so within seconds of the last change).
fn sync_all() -> SysResult {
    error::ipc(vfs::with(|c| c.sync())?).map(|()| 0)
}

/// `fsync` and `fdatasync`: the changes of the file `fd` is (with every
/// other) reach persistent storage before it returns. Pipes, the terminal
/// and the like have none: `EINVAL`, as on Linux.
pub fn fsync(fd: i32) -> SysResult {
    match fd::get(fd)?.object {
        Object::File(_) | Object::Dir(_) => sync_all(),
        // Memory has nothing to bring to storage.
        Object::Memfd(_) => Ok(0),
        Object::Stream(_)
        | Object::Inet(_)
        | Object::Unix(_)
        | Object::EventFd(_)
        | Object::TimerFd(_)
        | Object::Epoll(_)
        | Object::Pidfd(_)
        | Object::Null
        | Object::Log(_) => Err(EINVAL),
    }
}

/// `syncfs`: the changes of the file system `fd` is on (with every other)
/// reach persistent storage before it returns.
pub fn syncfs(fd: i32) -> SysResult {
    fd::get(fd)?;
    sync_all()
}

/// `sync`: every change reaches persistent storage before it returns. It
/// has no way to fail.
pub fn sync() -> SysResult {
    let _ = sync_all();
    Ok(0)
}

/// `sync_file_range`: writing a range out (`SYNC_FILE_RANGE_WRITE`) brings
/// the file's changes (with every other) to persistent storage; waiting for
/// writes in progress (the other flags) has nothing to wait for.
pub fn sync_file_range(fd: i32, offset: i64, len: i64, flags: u32) -> SysResult {
    use crate::linux::sync_range::{WAIT_AFTER, WAIT_BEFORE, WRITE};
    if flags & !(WAIT_BEFORE | WRITE | WAIT_AFTER) != 0 || offset < 0 || len < 0 || offset.checked_add(len).is_none() {
        return Err(EINVAL);
    }
    match fd::get(fd)?.object {
        Object::File(_) if flags & WRITE != 0 => sync_all(),
        Object::File(_) | Object::Dir(_) | Object::Memfd(_) => Ok(0),
        Object::Stream(_)
        | Object::Inet(_)
        | Object::Unix(_)
        | Object::EventFd(_)
        | Object::TimerFd(_)
        | Object::Epoll(_)
        | Object::Pidfd(_)
        | Object::Null
        | Object::Log(_) => Err(ESPIPE),
    }
}

pub unsafe fn pread(fd: i32, buf: usize, len: usize, offset: i64) -> SysResult {
    let desc = fd::get(fd)?;
    let at = u64::try_from(offset).map_err(|_| EINVAL)?;
    // SAFETY: as above.
    read_desc(&desc, unsafe { user::slice_mut(buf, len)? }, Some(at))
}

pub unsafe fn pwrite(fd: i32, buf: usize, len: usize, offset: i64) -> SysResult {
    let desc = fd::get(fd)?;
    let at = u64::try_from(offset).map_err(|_| EINVAL)?;
    // SAFETY: as above.
    let n = write_desc(&desc, unsafe { user::slice(buf, len)? }, Some(at))?;
    settle(&desc, n, false)
}

/// The buffers of an `iovec` array.
///
/// # Safety
/// `iov` must be where the program keeps `count` iovecs.
pub(crate) unsafe fn iovecs(iov: usize, count: usize) -> Result<Vec<Iovec>, isize> {
    if count > 1024 {
        return Err(EINVAL);
    }
    // SAFETY: per the caller.
    (0..count).map(|i| unsafe { user::read::<Iovec>(iov + i * size_of::<Iovec>()) }).collect()
}

/// The bytes of the program's buffers `vecs`, one after another.
///
/// # Safety
/// `vecs` must describe buffers of the program's.
pub(crate) unsafe fn gather(vecs: &[Iovec]) -> Result<Vec<u8>, isize> {
    let mut all = Vec::new();
    for v in vecs {
        // SAFETY: per the caller.
        all.extend_from_slice(unsafe { user::slice(v.base as usize, v.len)? });
    }
    Ok(all)
}

/// Reads from `desc` into the program's buffers `vecs` in turn (`readv`,
/// `recvmsg`). From a stream as Linux reads a pipe, waiting (unless
/// `nonblock`) only while nothing has come, and from the terminal in
/// canonical mode one line at most; from a network socket what it has (one
/// datagram from a datagram socket). An error after some bytes came ends it
/// with those.
///
/// # Safety
/// `vecs` must describe buffers of the program's.
pub(crate) unsafe fn read_vectored(
    desc: &Description,
    vecs: &[Iovec],
    mut at: Option<u64>,
    nonblock: bool,
) -> SysResult {
    if !desc.readable() {
        return Err(EBADF);
    }
    if matches!(desc.object, Object::Inet(_) | Object::Unix(_)) {
        if at.is_some() {
            return Err(ESPIPE);
        }
        // SAFETY: per the caller.
        let mut bufs: Vec<&mut [u8]> =
            vecs.iter().map(|v| unsafe { user::slice_mut(v.base as usize, v.len) }).collect::<Result<_, _>>()?;
        let how = Recv { nonblock, ..Recv::default() };
        return match &desc.object {
            Object::Unix(s) => s.recv(&mut bufs, how).map(|r| r.len),
            Object::Inet(s) => s.recv(&mut bufs, how).map(|r| r.len),
            _ => unreachable!(),
        };
    }
    let lines = matches!(&desc.object, Object::Stream(s) if s.reads_lines());
    let mut total = 0;
    for v in vecs {
        if v.len == 0 {
            continue;
        }
        // SAFETY: per the caller.
        let buf = unsafe { user::slice_mut(v.base as usize, v.len)? };
        let read = match &desc.object {
            // After the first buffer, only what is there already.
            Object::Stream(s) if at.is_none() => s.read(buf, nonblock || total > 0),
            _ => read_desc(desc, buf, at),
        };
        let n = match read {
            Ok(n) => n,
            Err(_) if total > 0 => break,
            Err(e) => return Err(e),
        };
        total += n;
        at = at.map(|a| a + n as u64);
        if n < v.len || (lines && buf[n - 1] == b'\n') {
            break;
        }
    }
    Ok(total)
}

pub unsafe fn readv(fd: i32, iov: usize, count: usize, offset: Option<i64>) -> SysResult {
    let desc = fd::get(fd)?;
    let at = offset.map(|o| u64::try_from(o).map_err(|_| EINVAL)).transpose()?;
    // SAFETY: the program passed `count` iovecs, each a buffer of its own.
    unsafe { read_vectored(&desc, &iovecs(iov, count)?, at, desc.nonblocking()) }
}

pub unsafe fn writev(fd: i32, iov: usize, count: usize, offset: Option<i64>) -> SysResult {
    let desc = fd::get(fd)?;
    // SAFETY: per the caller.
    let n = unsafe { write_vectored(&desc, iov, count, offset)? };
    settle(&desc, n, false)
}

/// Writes the program's `count` buffers at `iov` to `desc` in turn (at
/// `offset`, or the file's own), as `writev` does.
///
/// # Safety
/// `iov` must be where the program keeps `count` iovecs, each a buffer of
/// its own.
unsafe fn write_vectored(desc: &Description, iov: usize, count: usize, offset: Option<i64>) -> SysResult {
    let mut at = offset.map(|o| u64::try_from(o).map_err(|_| EINVAL)).transpose()?;
    // SAFETY: as above.
    let vecs = unsafe { iovecs(iov, count)? };
    // One write for streams, so that the pieces stay together (a datagram
    // socket's are one datagram).
    if matches!(
        desc.object,
        Object::Stream(_) | Object::Inet(_) | Object::Unix(_) | Object::EventFd(_) | Object::Log(_)
    ) && vecs.len() > 1
    {
        // SAFETY: as above.
        return write_desc(desc, &unsafe { gather(&vecs)? }, at);
    }
    let mut total = 0;
    for v in vecs {
        // SAFETY: as above.
        let buf = unsafe { user::slice(v.base as usize, v.len)? };
        let n = match write_desc(desc, buf, at) {
            Ok(n) => n,
            // What was written counts.
            Err(_) if total > 0 => break,
            Err(e) => return Err(e),
        };
        total += n;
        at = at.map(|a| a + n as u64);
        if n < v.len {
            break;
        }
    }
    Ok(total)
}

/// Whether a call of `preadv2` (`write` false) or `pwritev2` on a
/// descriptor (`appending` or not) can be made as its flags (`RWF_*`) ask.
/// It can with `RWF_HIPRI` (a hint), `RWF_DSYNC` and `RWF_SYNC` (a write
/// then reaches persistent storage before the call returns; a read has
/// nothing to do for them), and for a read the append flags, which do not
/// concern it. A write can also be made with `RWF_NOAPPEND`, except in
/// append mode, where the VFS writes at the end. Not with `RWF_APPEND` for
/// a write, or with `RWF_NOWAIT`.
fn rwf_possible(flags: u32, write: bool, appending: bool) -> bool {
    let always = rwf::HIPRI | rwf::DSYNC | rwf::SYNC;
    if write {
        flags & !(always | rwf::NOAPPEND) == 0 && !(appending && flags & rwf::NOAPPEND != 0)
    } else {
        flags & !(always | rwf::APPEND | rwf::NOAPPEND) == 0
    }
}

/// `preadv2`: [`readv`] at `offset`, or at the file's own offset when it is
/// -1. A call whose flags ask for what cannot be done fails with
/// `EOPNOTSUPP` ([`rwf_possible`]), which callers take as "without them".
///
/// # Safety
/// As [`readv`].
pub unsafe fn preadv2(fd: i32, iov: usize, count: usize, offset: i64, flags: u32) -> SysResult {
    if !rwf_possible(flags, false, false) {
        return Err(EOPNOTSUPP);
    }
    // SAFETY: per the caller.
    unsafe { readv(fd, iov, count, (offset != -1).then_some(offset)) }
}

/// `pwritev2`: [`writev`] at `offset`, or at the file's own offset when it
/// is -1. A call whose flags ask for what cannot be done fails with
/// `EOPNOTSUPP` ([`rwf_possible`]), which callers take as "without them":
/// the C library's `pwrite` asks for `RWF_NOAPPEND` first, then writes
/// without it.
///
/// # Safety
/// As [`writev`].
pub unsafe fn pwritev2(fd: i32, iov: usize, count: usize, offset: i64, flags: u32) -> SysResult {
    let desc = fd::get(fd)?;
    if !rwf_possible(flags, true, desc.flags() & o::APPEND != 0) {
        return Err(EOPNOTSUPP);
    }
    // SAFETY: per the caller.
    let n = unsafe { write_vectored(&desc, iov, count, (offset != -1).then_some(offset))? };
    settle(&desc, n, flags & (rwf::SYNC | rwf::DSYNC) != 0)
}

pub fn lseek(fd: i32, offset: i64, whence: u32) -> SysResult {
    let desc = fd::get(fd)?;
    match &desc.object {
        Object::File(f) => f.seek(offset, whence).map(|o| o as usize),
        Object::Memfd(m) => m.seek(offset, whence).map(|o| o as usize),
        Object::Dir(d) => d.seek(offset, whence).map(|o| o as usize),
        Object::Stream(_)
        | Object::Inet(_)
        | Object::Unix(_)
        | Object::EventFd(_)
        | Object::TimerFd(_)
        | Object::Epoll(_)
        | Object::Pidfd(_) => Err(ESPIPE),
        Object::Null | Object::Log(_) => Ok(0),
    }
}

pub fn close(fd: i32) -> SysResult {
    fd::remove(fd).map(|_| 0)
}

pub fn dup(fd: i32) -> SysResult {
    let desc = fd::get(fd)?;
    fd::insert(desc, false, 0).map(|n| n as usize)
}

pub fn dup3(old: i32, new: i32, flags: u32, dup2: bool) -> SysResult {
    if flags & !o::CLOEXEC != 0 {
        return Err(EINVAL);
    }
    let desc = fd::get(old)?;
    if old == new {
        return if dup2 { Ok(new as usize) } else { Err(EINVAL) };
    }
    fd::replace(new, desc, flags & o::CLOEXEC != 0)?;
    Ok(new as usize)
}

/// `close_range`. `CLOSE_RANGE_UNSHARE` changes nothing: a process's
/// threads always share its one table.
pub fn close_range(first: u32, last: u32, flags: u32) -> SysResult {
    const UNSHARE: u32 = 2;
    const CLOEXEC: u32 = 4;
    if first > last || flags & !(UNSHARE | CLOEXEC) != 0 {
        return Err(EINVAL);
    }
    if flags & CLOEXEC != 0 {
        fd::set_cloexec_range(first as usize, last as usize);
    } else {
        fd::remove_range(first as usize, last as usize);
    }
    Ok(0)
}

/// Installs `a` and `b` at the lowest free descriptors and stores those at
/// `out`, the program's `int[2]`; nothing stays installed if that fails.
///
/// # Safety
/// `out` must be null or the program's `int[2]`.
pub(crate) unsafe fn install_pair(
    a: Arc<Description>,
    b: Arc<Description>,
    cloexec: bool,
    out: usize,
) -> Result<(), isize> {
    let x = fd::insert(a, cloexec, 0)?;
    let y = match fd::insert(b, cloexec, 0) {
        Ok(y) => y,
        Err(e) => {
            let _ = fd::remove(x);
            return Err(e);
        }
    };
    // SAFETY: per the caller.
    if let Err(e) = unsafe { user::write::<[i32; 2]>(out, [x, y]) } {
        let _ = fd::remove(x);
        let _ = fd::remove(y);
        return Err(e);
    }
    Ok(())
}

/// `pipe2`.
///
/// # Safety
/// `fds` must be null or the program's `int[2]`.
pub unsafe fn pipe2(fds: usize, flags: u32) -> SysResult {
    if flags & !(o::CLOEXEC | o::NONBLOCK) != 0 {
        return Err(EINVAL);
    }
    if fds == 0 {
        return Err(EFAULT);
    }
    let (a, b) = Socket::create().map_err(error::kernel)?;
    let status = flags & o::NONBLOCK;
    // A pipe: one end only reads, the other only writes.
    let reader = Description::new(Object::Stream(crate::stream::Stream::new(a)), o::RDONLY | status);
    let writer = Description::new(Object::Stream(crate::stream::Stream::new(b)), o::WRONLY | status);
    // SAFETY: per the caller.
    unsafe { install_pair(reader, writer, flags & o::CLOEXEC != 0, fds)? };
    Ok(0)
}

pub unsafe fn fcntl(fd: i32, cmd: u32, arg: usize) -> SysResult {
    match cmd {
        fcntl::DUPFD | fcntl::DUPFD_CLOEXEC => {
            let desc = fd::get(fd)?;
            // Beyond the limit on descriptors (or negative), as on Linux.
            if arg >= fd::MAX_FDS {
                return Err(EINVAL);
            }
            fd::insert(desc, cmd == fcntl::DUPFD_CLOEXEC, arg).map(|n| n as usize)
        }
        // Whether `arg` refers to the same open file description.
        fcntl::DUPFD_QUERY => Ok(Arc::ptr_eq(&fd::get(fd)?, &fd::get(arg as i32)?) as usize),
        fcntl::GETFD => Ok(if fd::cloexec(fd)? { fcntl::FD_CLOEXEC as usize } else { 0 }),
        fcntl::SETFD => fd::set_cloexec(fd, arg as u32 & fcntl::FD_CLOEXEC != 0).map(|_| 0),
        fcntl::GETFL => Ok(fd::get(fd)?.flags() as usize),
        fcntl::SETFL => {
            let desc = fd::get(fd)?;
            let append = arg as u32 & o::APPEND != 0;
            if let Object::File(f) = &desc.object
                && append != (desc.flags() & o::APPEND != 0)
            {
                f.set_append(append)?;
            }
            desc.set_status(arg as u32);
            Ok(0)
        }
        // Advisory locks, which the VFS arbitrates.
        fcntl::GETLK | fcntl::SETLK | fcntl::SETLKW | fcntl::OFD_GETLK | fcntl::OFD_SETLK | fcntl::OFD_SETLKW => {
            // SAFETY: the program passed a `struct flock`.
            unsafe { crate::lock::fcntl(fd, cmd, arg) }
        }
        // The owner is kept, as a program may ask who it is; none is sent
        // SIGIO or SIGURG, as Veda's signals are synchronous.
        fcntl::SETOWN => {
            let id = arg as i32;
            let kind = if id < 0 { fcntl::F_OWNER_PGRP } else { fcntl::F_OWNER_PID };
            fd::get(fd)?.set_owner(kind, id.checked_abs().ok_or(EINVAL)?);
            Ok(0)
        }
        fcntl::GETOWN => match fd::get(fd)?.owner() {
            (fcntl::F_OWNER_PGRP, id) => Ok(-id as isize as usize),
            (_, id) => Ok(id as usize),
        },
        fcntl::SETOWN_EX => {
            let desc = fd::get(fd)?;
            // SAFETY: the program passed a `struct f_owner_ex`.
            let [kind, id]: [i32; 2] = unsafe { user::read(arg)? };
            if !matches!(kind, fcntl::F_OWNER_TID | fcntl::F_OWNER_PID | fcntl::F_OWNER_PGRP) || id < 0 {
                return Err(EINVAL);
            }
            desc.set_owner(kind, id);
            Ok(0)
        }
        fcntl::GETOWN_EX => {
            let (kind, id) = fd::get(fd)?.owner();
            // SAFETY: as above.
            unsafe { user::write(arg, [kind, id])? };
            Ok(0)
        }
        // A pipe holds what its kernel socket does, which does not change:
        // as much as asked for, unless more.
        fcntl::GETPIPE_SZ | fcntl::SETPIPE_SZ => {
            let desc = fd::get(fd)?;
            if !matches!(&desc.object, Object::Stream(s) if !s.is_tty()) {
                return Err(EBADF);
            }
            if cmd == fcntl::SETPIPE_SZ && arg > abi::SOCKET_CAPACITY {
                return Err(EPERM);
            }
            Ok(abi::SOCKET_CAPACITY)
        }
        // Only memory files have seals ([`crate::memfd`]).
        fcntl::ADD_SEALS => {
            let desc = fd::get(fd)?;
            let Object::Memfd(m) = &desc.object else { return Err(EINVAL) };
            m.add_seals(arg as u32, desc.writable()).map(|()| 0)
        }
        fcntl::GET_SEALS => match &fd::get(fd)?.object {
            Object::Memfd(m) => Ok(m.seals() as usize),
            _ => Err(EINVAL),
        },
        _ => Err(EINVAL),
    }
}

pub unsafe fn ioctl(fd: i32, request: u32, arg: usize) -> SysResult {
    let desc = fd::get(fd)?;
    match request {
        ioctl::FIONBIO => {
            // SAFETY: the program passed an `int`.
            let on: i32 = unsafe { user::read(arg)? };
            let flags = if on != 0 { desc.flags() | o::NONBLOCK } else { desc.flags() & !o::NONBLOCK };
            desc.set_status(flags);
            Ok(0)
        }
        ioctl::FIONREAD => {
            let n = match &desc.object {
                Object::Stream(s) => s.available(),
                Object::Inet(s) => s.available(),
                Object::Unix(s) => s.available(),
                Object::File(f) => {
                    let size = f.stat()?.size;
                    let at = f.seek(0, linux::seek::CUR)?;
                    size.saturating_sub(at) as usize
                }
                Object::Memfd(m) => {
                    let at = m.seek(0, linux::seek::CUR)?;
                    m.size().saturating_sub(at) as usize
                }
                _ => 0,
            };
            // SAFETY: the program passed an `int`.
            unsafe { user::write(arg, n.min(i32::MAX as usize) as i32)? };
            Ok(0)
        }
        ioctl::FIOCLEX => fd::set_cloexec(fd, true).map(|_| 0),
        ioctl::FIONCLEX => fd::set_cloexec(fd, false).map(|_| 0),
        // SAFETY: per the request.
        _ => unsafe { tty::ioctl(fd, request, arg) },
    }
}

pub fn ftruncate(fd: i32, len: i64) -> SysResult {
    let desc = fd::get(fd)?;
    let len = u64::try_from(len).map_err(|_| EINVAL)?;
    match &desc.object {
        Object::File(f) if desc.writable() => f.truncate(len).map(|_| 0),
        Object::Memfd(m) if desc.writable() => m.truncate(len).map(|_| 0),
        Object::Dir(_) => Err(EISDIR),
        _ => Err(EINVAL),
    }
}

/// What `poll` and `epoll` wait on: an object, kept open while they do.
pub(crate) enum Waitable {
    /// An object its description keeps: a stream's socket endpoint, an
    /// eventfd's event.
    Desc(Arc<Description>),
    /// A network socket's channel, a Unix domain socket's inbox or
    /// messages.
    Channel(Arc<Channel>),
    /// A Unix domain stream socket's connection.
    Stream(Arc<crate::unix::StreamConn>),
    /// A TCP connection's bytes: a kernel socket's endpoint.
    Bytes(Arc<crate::stream::Stream>),
}

impl Waitable {
    pub(crate) fn handle(&self) -> Option<RawHandle> {
        match self {
            Waitable::Desc(d) => match &d.object {
                Object::Stream(s) => Some(s.socket.raw()),
                Object::EventFd(e) => Some(e.event.raw()),
                Object::TimerFd(t) => Some(t.event().raw()),
                Object::Pidfd(p) => Some(p.raw()),
                _ => None,
            },
            Waitable::Channel(c) => Some(c.raw()),
            Waitable::Stream(c) => Some(c.stream.socket.raw()),
            Waitable::Bytes(s) => Some(s.socket.raw()),
        }
    }
}

/// What a description is ready for now.
pub(crate) struct Ready {
    /// `epoll`'s events, which are `poll`'s too: `EPOLLIN` and `EPOLLRDNORM`,
    /// `EPOLLOUT` and `EPOLLWRNORM`, `EPOLLRDHUP` (the peer writes no
    /// more), `EPOLLHUP`, `EPOLLERR`.
    pub events: u32,
    /// What to wait on for more, and for which of its signals: those that
    /// make it ready for what is asked and is not, and those that change it
    /// otherwise (a hang-up, a message on a network socket's channel).
    pub wait: Option<(Waitable, u32)>,
    /// How often it changed in ways its readiness may not show, for
    /// edge-triggered `epoll`.
    pub seq: u64,
}

/// What `desc` is ready for now, asked whether it can be read (`want_in`)
/// and written (`want_out`) without waiting.
pub(crate) fn readiness(desc: &Arc<Description>, want_in: bool, want_out: bool) -> Ready {
    use crate::linux::epoll::{ERR, HUP, IN, OUT, RDHUP, RDNORM, WRNORM};
    let seq = desc.seq();
    match &desc.object {
        Object::Stream(s) => {
            let (readable, hup, writable) = s.readiness();
            let mut events = 0;
            if readable {
                events |= IN | RDNORM;
            }
            if writable && !hup {
                events |= OUT | WRNORM;
            }
            if hup {
                // The read end of a pipe whose writers are gone has the end
                // of its input; the write end, nobody to write to.
                events |= if desc.writable() && !desc.readable() { ERR } else { HUP | RDHUP };
            }
            let mut signals = 0;
            if !hup {
                signals |= PEER_CLOSED | PEER_WRITE_DISABLED;
            }
            if want_in && !readable {
                signals |= READABLE;
            }
            if want_out && !writable && !hup {
                signals |= WRITABLE;
            }
            let wait = (signals != 0).then(|| (Waitable::Desc(desc.clone()), signals));
            Ready { events, wait, seq }
        }
        Object::Inet(s) => {
            let now = s.readiness(want_in, want_out);
            let mut events = 0;
            if now.input {
                events |= IN | RDNORM;
            }
            if now.output {
                events |= OUT | WRNORM;
            }
            if now.read_hangup {
                events |= RDHUP;
            }
            if now.hangup {
                events |= HUP;
            }
            if now.error {
                events |= ERR;
            }
            Ready { events, wait: now.wait, seq: seq.wrapping_add(now.seq) }
        }
        Object::Unix(s) => {
            let now = s.readiness(want_in, want_out);
            let mut events = 0;
            if now.input {
                events |= IN | RDNORM;
            }
            if now.output {
                events |= OUT | WRNORM;
            }
            if now.read_hangup {
                events |= RDHUP;
            }
            if now.hangup {
                events |= HUP;
            }
            Ready { events, wait: now.wait, seq }
        }
        Object::EventFd(e) => {
            let (readable, writable) = e.readiness();
            let mut events = 0;
            if readable {
                events |= IN | RDNORM;
            }
            if writable {
                events |= OUT | WRNORM;
            }
            let mut signals = 0;
            if want_in && !readable {
                signals |= crate::eventfd::READABLE;
            }
            if want_out && !writable {
                signals |= crate::eventfd::WRITABLE;
            }
            let wait = (signals != 0).then(|| (Waitable::Desc(desc.clone()), signals));
            Ready { events, wait, seq }
        }
        // A timer: readable once it has expired.
        Object::TimerFd(t) => {
            let readable = t.readable();
            let events = if readable { IN | RDNORM } else { 0 };
            let wait = (want_in && !readable).then(|| (Waitable::Desc(desc.clone()), crate::timerfd::READABLE));
            Ready { events, wait, seq }
        }
        // A child's descriptor: readable once it has ended.
        Object::Pidfd(p) => {
            let ended = p.wait(TERMINATED, 0).is_ok();
            let events = if ended { IN | RDNORM } else { 0 };
            let wait = (want_in && !ended).then(|| (Waitable::Desc(desc.clone()), TERMINATED));
            Ready { events, wait, seq }
        }
        // Another epoll instance is never ready here: they do not nest.
        Object::Epoll(_) => Ready { events: 0, wait: None, seq },
        // Files and the rest never make a reader or writer wait.
        Object::File(_) | Object::Memfd(_) | Object::Dir(_) | Object::Null | Object::Log(_) => {
            Ready { events: IN | RDNORM | OUT | WRNORM, wait: None, seq }
        }
    }
}

/// What `poll` reports for one descriptor right now, and how to wait for
/// more.
fn poll_one(p: &Pollfd) -> (i16, Option<(Waitable, u32)>) {
    let Ok(desc) = fd::get(p.fd) else { return (poll::NVAL, None) };
    let asked = p.events as u16 as u32;
    let want_in = p.events & (poll::IN | poll::RDNORM | poll::RDHUP) != 0;
    let want_out = p.events & (poll::OUT | poll::WRNORM) != 0;
    let now = readiness(&desc, want_in, want_out);
    let always = crate::linux::epoll::ERR | crate::linux::epoll::HUP;
    ((now.events & (asked | always)) as i16, now.wait)
}

/// `poll`: waits until a descriptor is ready or `deadline` (monotonic ns,
/// `None` = forever) passes.
pub unsafe fn poll(fds: usize, count: usize, deadline: Option<u64>) -> SysResult {
    if count > fd::MAX_FDS {
        return Err(EINVAL);
    }
    let read_all = || -> Result<Vec<Pollfd>, isize> {
        // SAFETY: the program passed `count` pollfds.
        (0..count).map(|i| unsafe { user::read::<Pollfd>(fds + i * size_of::<Pollfd>()) }).collect()
    };
    let mut pfds = read_all()?;
    loop {
        let mut ready = 0;
        let mut waits: Vec<(Waitable, u32)> = Vec::new();
        for p in pfds.iter_mut() {
            if p.fd < 0 {
                p.revents = 0;
                continue;
            }
            let (r, wait) = poll_one(p);
            p.revents = r;
            if r != 0 {
                ready += 1;
            }
            waits.extend(wait);
        }
        let now = time::monotonic_ns();
        if ready > 0 || deadline.is_some_and(|d| now >= d) {
            for (i, p) in pfds.iter().enumerate() {
                // SAFETY: as above.
                unsafe { user::write(fds + i * size_of::<Pollfd>(), *p)? };
            }
            return Ok(ready);
        }
        if waits.is_empty() {
            // Nothing can become ready: wait out the time, unless a signal
            // is to be acted on first.
            if crate::signal::sleep_until(deadline.unwrap_or(abi::DEADLINE_INFINITE)) {
                return Err(EINTR);
            }
            continue;
        }
        let mut items: Vec<WaitItem> = waits
            .iter()
            .filter_map(|(w, signals)| {
                Some(WaitItem { handle: w.handle()?, signals: *signals, observed: 0, _reserved: 0 })
            })
            .collect();
        match crate::interrupt::wait_many(&mut items, deadline.unwrap_or(abi::DEADLINE_INFINITE)) {
            Ok(_) | Err(abi::Error::TimedOut) => {}
            // A signal ends it, never to start again (as on Linux).
            Err(abi::Error::Interrupted) => return Err(EINTR),
            Err(e) => return Err(error::kernel(e)),
        }
        pfds = read_all()?;
    }
}

/// `select`/`pselect6` through `poll`.
pub unsafe fn select(n: i32, readfds: usize, writefds: usize, exceptfds: usize, deadline: Option<u64>) -> SysResult {
    let n = usize::try_from(n).map_err(|_| EINVAL)?.min(fd::MAX_FDS);
    let words = n.div_ceil(64);
    // SAFETY: the program passed fd_sets of at least `n` bits (or nulls).
    let load = |p: usize| -> Result<Vec<u64>, isize> {
        if p == 0 {
            return Ok(alloc::vec![0; words]);
        }
        (0..words).map(|i| unsafe { user::read::<u64>(p + i * 8) }).collect()
    };
    let (r, w, e) = (load(readfds)?, load(writefds)?, load(exceptfds)?);
    let has = |set: &[u64], i: usize| set[i / 64] & (1 << (i % 64)) != 0;
    let mut pfds = Vec::new();
    for i in 0..n {
        let mut events = 0;
        if has(&r, i) {
            events |= poll::IN;
        }
        if has(&w, i) {
            events |= poll::OUT;
        }
        if has(&e, i) {
            events |= poll::PRI;
        }
        if events != 0 {
            pfds.push(Pollfd { fd: i as i32, events, revents: 0 });
        }
    }
    // Poll on a copy in our own memory.
    // SAFETY: `pfds` is a live array of pollfds.
    let ready = unsafe { poll(pfds.as_mut_ptr() as usize, pfds.len(), deadline)? };
    let _ = ready;
    if pfds.iter().any(|p| p.revents & poll::NVAL != 0) {
        return Err(EBADF);
    }
    let mut out = (alloc::vec![0u64; words], alloc::vec![0u64; words], alloc::vec![0u64; words]);
    let mut count = 0;
    for p in &pfds {
        let i = p.fd as usize;
        let bit = 1u64 << (i % 64);
        if p.revents & (poll::IN | poll::HUP | poll::ERR) != 0 && has(&r, i) {
            out.0[i / 64] |= bit;
            count += 1;
        }
        if p.revents & (poll::OUT | poll::ERR) != 0 && has(&w, i) {
            out.1[i / 64] |= bit;
            count += 1;
        }
        if p.revents & poll::PRI != 0 && has(&e, i) {
            out.2[i / 64] |= bit;
            count += 1;
        }
    }
    for (p, set) in [(readfds, &out.0), (writefds, &out.1), (exceptfds, &out.2)] {
        if p != 0 {
            for (i, word) in set.iter().enumerate() {
                // SAFETY: as above.
                unsafe { user::write(p + i * 8, *word)? };
            }
        }
    }
    Ok(count)
}

/// Installs the descriptors a process was started with: role `FD + n`
/// handles are kernel sockets (pipes, the terminal), `file` connections, or
/// what carries the sockets and memory files `inherited` says; `inherited`
/// also says the descriptors that are no kernel object.
pub fn adopt_startup_fds(inherited: &Inherited) {
    for (n, carried) in &inherited.carried {
        let Some(n) = u32::try_from(*n).ok().filter(|&n| n < startup::role::MAX_FDS) else { continue };
        let roles = [startup::role::FD + n, startup::role::FD_EXTRA + n];
        let handles = roles.into_iter().map_while(runtime::env::take_handle).collect();
        if let Some(desc) = crate::carry::from_parent(carried, handles) {
            let _ = fd::replace(n as i32, desc, false);
        }
    }
    for role in runtime::env::handle_roles() {
        let Some(n) = startup::role::fd(role) else { continue };
        let Some(h) = runtime::env::take_handle(role) else { continue };
        let Ok(info) = h.basic_info() else { continue };
        let desc = if info.object_type == abi::ObjectType::Socket as u32 {
            // Its access is what the handle allows.
            let mode = match (info.rights & abi::Rights::READ.0 != 0, info.rights & abi::Rights::WRITE.0 != 0) {
                (true, true) => o::RDWR,
                (false, true) => o::WRONLY,
                _ => o::RDONLY,
            };
            Description::new(Object::Stream(crate::stream::Stream::new(Socket::from_handle(h))), mode)
        } else if info.object_type == abi::ObjectType::Channel as u32 {
            let f = crate::file::VfsFile::new(runtime::object::Channel::from_handle(h));
            let mode = f.open_flags().map(crate::file::access_mode).unwrap_or(o::RDWR);
            let append = f.open_flags().is_ok_and(|fl| fl & proto::fs::open_flags::APPEND != 0);
            Description::new(Object::File(f), mode | if append { o::APPEND } else { 0 })
        } else {
            continue;
        };
        let _ = fd::replace(n as i32, desc, false);
    }
    for (n, fd) in &inherited.fds {
        let desc = match fd {
            Pseudo::Dir(path) => Description::new(Object::Dir(crate::file::Directory::new(path.clone())), o::RDONLY),
            Pseudo::Null => Description::new(Object::Null, o::RDWR),
            Pseudo::Log => Description::new(Object::Log(Log::new()), o::RDWR),
        };
        let _ = fd::replace(*n, desc, false);
    }
    // Standard streams nobody gave, but a POSIX program that left them
    // closed: input is empty, output goes to the kernel log.
    for n in 0..=2 {
        if fd::get(n).is_err() && !inherited.closed.contains(&n) {
            let object = if n == 0 { Object::Null } else { Object::Log(Log::new()) };
            let _ = fd::replace(n, Description::new(object, o::RDWR), false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_of_vectored_calls_that_can_be_done() {
        // The C library's pwrite and pwritev, but in append mode.
        assert!(rwf_possible(rwf::NOAPPEND, true, false));
        assert!(!rwf_possible(rwf::NOAPPEND, true, true));
        assert!(rwf_possible(0, true, true) && rwf_possible(0, false, false));
        assert!(rwf_possible(rwf::HIPRI | rwf::DSYNC | rwf::SYNC, true, true));
        // Appending, or reading only what is there, is not done.
        assert!(!rwf_possible(rwf::APPEND, true, false));
        let nowait = linux_raw_sys::general::RWF_NOWAIT;
        assert!(!rwf_possible(nowait, true, false) && !rwf_possible(nowait, false, false));
        // The append flags do not concern a read.
        assert!(rwf_possible(rwf::APPEND | rwf::NOAPPEND, false, false));
        assert!(!rwf_possible(0x100, false, false) && !rwf_possible(0x100, true, false));
    }

    /// A descriptor of the empty input, in the table (which the tests share).
    fn null(mode: u32) -> (i32, Arc<Description>) {
        let desc = Description::new(Object::Null, mode);
        (fd::insert(desc.clone(), false, 0).unwrap(), desc)
    }

    #[test]
    fn only_files_are_synced() {
        let (fd, _) = null(o::RDWR);
        assert_eq!(fsync(fd), Err(EINVAL));
        assert_eq!(sync_file_range(fd, 0, 10, crate::linux::sync_range::WRITE), Err(ESPIPE));
        assert_eq!(sync_file_range(fd, 0, 10, 8), Err(EINVAL));
        assert_eq!(sync_file_range(fd, -1, 10, 0), Err(EINVAL));
        assert_eq!(sync_file_range(fd, i64::MAX, 10, 0), Err(EINVAL));
        let _ = fd::remove(fd);
        assert_eq!(fsync(fd), Err(EBADF));
    }

    #[test]
    fn a_pair_is_not_left_installed_when_its_numbers_cannot_be_stored() {
        let a = Description::new(Object::Null, o::RDWR);
        let b = Description::new(Object::Null, o::RDWR);
        // SAFETY: a null pointer, which is refused.
        assert_eq!(unsafe { install_pair(a.clone(), b.clone(), false, 0) }, Err(EFAULT));
        assert!(fd::snapshot().iter().all(|(_, d, _)| !Arc::ptr_eq(d, &a) && !Arc::ptr_eq(d, &b)));
        let mut out = [0i32; 2];
        // SAFETY: an `int[2]` of the test's.
        unsafe { install_pair(a.clone(), b, true, out.as_mut_ptr() as usize).unwrap() };
        assert!(Arc::ptr_eq(&fd::get(out[0]).unwrap(), &a) && fd::cloexec(out[1]) == Ok(true));
        let _ = (fd::remove(out[0]), fd::remove(out[1]));
    }
}
