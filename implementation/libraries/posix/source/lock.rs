// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Advisory locks on files, which the VFS arbitrates between processes as
//! Linux does (`file::lock`): `flock`'s, `fcntl`'s, and `lockf`'s, which the
//! C library makes of `fcntl`'s.
//!
//! | Locks | Held by | Go |
//! |-------|---------|----|
//! | `flock`'s, on the whole file | the open file description | as it closes |
//! | record locks on bytes (`F_SETLK`, `F_SETLKW`, `F_GETLK`) | the process | as it ends, or closes any descriptor of the file |
//! | OFD locks on bytes (`F_OFD_SETLK`, `F_OFD_SETLKW`, `F_OFD_GETLK`) | the open file description | as it closes |
//!
//! Locks on bytes, a process's and an open file's, are in each other's
//! way, but not in `flock`'s. A process owns its record locks by a secret
//! the VFS drew for its connection (`vfs::lock_owner`), which lasts as long
//! as the process; and as the VFS cannot tell which process closes a
//! connection, the process takes its record locks on a file away itself as
//! it closes a descriptor of the file ([`closed`]): it notes the files it
//! holds some on, so that closing others costs nothing more.
//!
//! Only the VFS's files take locks: on other descriptors (directories,
//! pipes, sockets, memory files) every lock is refused with `ENOLCK`, and
//! unlocking succeeds. A wait for a lock (`F_SETLKW`, `F_OFD_SETLKW`,
//! `flock` without `LOCK_NB`) reports `EDEADLK` where Linux does, and a
//! signal ends it as it ends other blocking calls ([`crate::interrupt`]):
//! `EINTR`, or the call made again after a handler with `SA_RESTART`.

use alloc::collections::BTreeMap;

use proto::fs::{LOCK_END, LockRange, lock_kind};
use rt::sync::Mutex;

use crate::error::SysResult;
use crate::fd::{self, Description, Object};
use crate::file::{Requested, VfsFile, await_lock};
use crate::linux::errno::{EBADF, EINVAL, ENOLCK, EOVERFLOW};
use crate::linux::{Flock, fcntl, o, seek};
use crate::{process, user, vfs};

/// The files this process holds record locks on, or waits for one on, by
/// their identities ([`VfsFile::identity`]): those whose closing takes
/// locks away. Requests for record locks take turns under its lock, so that
/// what the VFS answered last is what it did last; waits for them do not.
static RECORDS: Mutex<BTreeMap<(u64, u64), Records>> = Mutex::new(BTreeMap::new());

/// This process's record locks on a file.
#[derive(Default)]
struct Records {
    /// It held some after what the VFS last did with them.
    held: bool,
    /// Its requests that wait for one.
    waiting: u32,
}

/// Notes whether this process holds record locks on file `identity` now.
fn note(records: &mut BTreeMap<(u64, u64), Records>, identity: (u64, u64), held: bool) {
    let r = records.entry(identity).or_default();
    r.held = held;
    if !r.held && r.waiting == 0 {
        records.remove(&identity);
    }
}

/// `flock(fd, operation)`: `LOCK_SH`, `LOCK_EX` or `LOCK_UN`, with
/// `LOCK_NB` not to wait.
pub fn flock(fd: i32, operation: u32) -> SysResult {
    use crate::linux::flock::{EX, MAND, NB, SH, UN};
    // Linux has dropped mandatory locks, and says nothing of them.
    if operation & MAND != 0 {
        return Ok(0);
    }
    let kind = match operation & !NB {
        SH => lock_kind::SHARED,
        EX => lock_kind::EXCLUSIVE,
        UN => lock_kind::NONE,
        _ => return Err(EINVAL),
    };
    let desc = fd::get(fd)?;
    if desc.flags() & o::PATH != 0 {
        return Err(EBADF);
    }
    let Object::File(f) = &desc.object else { return if kind == lock_kind::NONE { Ok(0) } else { Err(ENOLCK) } };
    let whole = LockRange { kind, start: 0, end: LOCK_END };
    match f.request_lock(&[], true, whole, operation & NB == 0)? {
        Requested::Done(_) => Ok(0),
        Requested::Waiting(waiting) => await_lock(waiting).map(|_| 0),
    }
}

/// `fcntl`'s lock commands: `F_GETLK`, `F_SETLK` and `F_SETLKW`, of record
/// locks, and `F_OFD_GETLK`, `F_OFD_SETLK` and `F_OFD_SETLKW`, of OFD
/// locks, on the bytes the `struct flock` at `arg` names, which a test
/// fills in with the first lock in the way (`F_UNLCK` for none).
///
/// # Safety
/// `arg` must be the program's `struct flock`.
pub unsafe fn fcntl(fd: i32, cmd: u32, arg: usize) -> SysResult {
    let desc = fd::get(fd)?;
    if desc.flags() & o::PATH != 0 {
        return Err(EBADF);
    }
    // SAFETY: per the caller.
    let mut l: Flock = unsafe { user::read(arg)? };
    let ofd = matches!(cmd, fcntl::OFD_GETLK | fcntl::OFD_SETLK | fcntl::OFD_SETLKW);
    let test = matches!(cmd, fcntl::GETLK | fcntl::OFD_GETLK);
    // A test is of a lock; an OFD test of none finds the description's own.
    if test && !ofd && !matches!(l.kind, fcntl::F_RDLCK | fcntl::F_WRLCK) {
        return Err(EINVAL);
    }
    let (start, end) = span(origin(&desc, l.whence)?, l.start, l.len)?;
    let kind = match l.kind {
        fcntl::F_RDLCK => lock_kind::SHARED,
        fcntl::F_WRLCK => lock_kind::EXCLUSIVE,
        fcntl::F_UNLCK => lock_kind::NONE,
        _ => return Err(EINVAL),
    };
    // A lock needs the access its kind does: to read for a shared one, to
    // write for an exclusive one.
    let allowed = match kind {
        lock_kind::SHARED => desc.readable(),
        lock_kind::EXCLUSIVE => desc.writable(),
        _ => true,
    };
    if !test && !allowed {
        return Err(EBADF);
    }
    if ofd && l.pid != 0 {
        return Err(EINVAL);
    }
    let lock = LockRange { kind, start, end };
    let Object::File(f) = &desc.object else {
        // Locks the VFS does not arbitrate: none is held, none can be.
        if test {
            l.kind = fcntl::F_UNLCK;
            // SAFETY: per the caller.
            unsafe { user::write(arg, l)? };
            return Ok(0);
        }
        return if kind == lock_kind::NONE { Ok(0) } else { Err(ENOLCK) };
    };
    let wait = matches!(cmd, fcntl::SETLKW | fcntl::OFD_SETLKW);
    match (ofd, test) {
        (true, false) => match f.request_lock(&[], false, lock, wait)? {
            Requested::Done(_) => Ok(0),
            Requested::Waiting(waiting) => await_lock(waiting).map(|_| 0),
        },
        (false, false) => record(f, lock, wait).map(|()| 0),
        (_, true) => {
            let owner = if ofd { alloc::vec::Vec::new() } else { vfs::lock_owner()? };
            match f.test_lock(&owner, lock)? {
                None => l.kind = fcntl::F_UNLCK,
                Some(found) => {
                    let kind = if found.lock.kind == lock_kind::SHARED { fcntl::F_RDLCK } else { fcntl::F_WRLCK };
                    l = Flock {
                        kind,
                        whence: seek::SET as i16,
                        start: found.lock.start as i64,
                        len: if found.lock.end == LOCK_END {
                            0
                        } else {
                            (found.lock.end - found.lock.start + 1) as i64
                        },
                        // An open file's lock has no process, Linux's -1.
                        pid: found.process.map_or(-1, process::pid_of),
                    };
                }
            }
            // SAFETY: per the caller.
            unsafe { user::write(arg, l)? };
            Ok(0)
        }
    }
}

/// Where a `struct flock`'s `l_whence` counts from: the start of the file,
/// the description's offset or the file's end (0 for what is no file).
fn origin(desc: &Description, whence: i16) -> Result<i64, isize> {
    let file = match &desc.object {
        Object::File(f) => Some(f),
        _ => None,
    };
    match u32::try_from(whence) {
        Ok(seek::SET) => Ok(0),
        Ok(seek::CUR) => file.map_or(Ok(0), |f| f.seek(0, seek::CUR).map(|o| o as i64)),
        Ok(seek::END) => file.map_or(Ok(0), |f| f.stat().map(|st| st.size as i64)),
        _ => Err(EINVAL),
    }
}

/// The first and last bytes of `len` bytes from `start` past `origin`, as
/// Linux reads a `struct flock` (`flock_to_posix_lock`): back from it for a
/// negative length, on to the end of the file and past it (`LOCK_END`) for
/// 0. `EINVAL` before the file's start, `EOVERFLOW` past the largest
/// offset.
fn span(origin: i64, start: i64, len: i64) -> Result<(u64, u64), isize> {
    if start > i64::MAX - origin {
        return Err(EOVERFLOW);
    }
    let first = origin + start;
    if first < 0 {
        return Err(EINVAL);
    }
    match len {
        0 => Ok((first as u64, LOCK_END)),
        len if len > 0 => {
            if len - 1 > i64::MAX - first {
                return Err(EOVERFLOW);
            }
            Ok((first as u64, (first + (len - 1)) as u64))
        }
        len => {
            if first + len < 0 {
                return Err(EINVAL);
            }
            Ok(((first + len) as u64, (first - 1) as u64))
        }
    }
}

/// Asks for this process's record lock `lock` on `file` (or takes its locks
/// there away, for `lock_kind::NONE`), waiting while another is in the way
/// if `wait`, and notes whether it holds some on the file then.
fn record(file: &VfsFile, lock: LockRange, wait: bool) -> Result<(), isize> {
    let owner = vfs::lock_owner()?;
    let identity = file.identity()?;
    let waiting = {
        let mut records = RECORDS.lock();
        match file.request_lock(&owner, false, lock, wait)? {
            Requested::Done(held) => {
                note(&mut records, identity, held);
                return Ok(());
            }
            Requested::Waiting(waiting) => {
                records.entry(identity).or_default().waiting += 1;
                waiting
            }
        }
    };
    let granted = await_lock(waiting);
    let mut records = RECORDS.lock();
    let r = records.entry(identity).or_default();
    r.waiting = r.waiting.saturating_sub(1);
    let held = r.held || granted.is_ok();
    note(&mut records, identity, held);
    granted.map(|_| ())
}

/// This process closes a descriptor of `file`: as POSIX has it, its record
/// locks on the file go, whichever descriptor took them. Only a file it
/// holds some on (or waits for one on, which may have been granted) costs
/// a request.
pub fn closed(file: &VfsFile) {
    let mut records = RECORDS.lock();
    if records.is_empty() {
        return;
    }
    let Ok(identity) = file.identity() else { return };
    if !records.contains_key(&identity) {
        return;
    }
    let Ok(owner) = vfs::lock_owner() else { return };
    let everything = LockRange { kind: lock_kind::NONE, start: 0, end: LOCK_END };
    if let Ok(Requested::Done(held)) = file.request_lock(&owner, false, everything, false) {
        note(&mut records, identity, held);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_are_read_as_linux_reads_them() {
        // From the start: a length, to the end and past it, or back.
        assert_eq!(span(0, 10, 5), Ok((10, 14)));
        assert_eq!(span(0, 10, 0), Ok((10, LOCK_END)));
        assert_eq!(span(0, 10, -4), Ok((6, 9)));
        assert_eq!(span(0, 10, -10), Ok((0, 9)));
        // From an offset or an end, which a negative start goes back from.
        assert_eq!(span(100, -10, 1), Ok((90, 90)));
        assert_eq!(span(100, 0, 0), Ok((100, LOCK_END)));
        // Before the file's start.
        assert_eq!(span(0, -1, 1), Err(EINVAL));
        assert_eq!(span(100, -101, 0), Err(EINVAL));
        assert_eq!(span(0, 10, -11), Err(EINVAL));
        assert_eq!(span(0, 0, i64::MIN), Err(EINVAL));
        // Past the largest offset; up to it is a span.
        assert_eq!(span(1, i64::MAX, 1), Err(EOVERFLOW));
        assert_eq!(span(0, i64::MAX, 2), Err(EOVERFLOW));
        assert_eq!(span(0, 2, i64::MAX), Err(EOVERFLOW));
        assert_eq!(span(0, 1, i64::MAX), Ok((1, LOCK_END)));
        assert_eq!(span(0, i64::MAX, 1), Ok((LOCK_END, LOCK_END)));
        assert_eq!(span(0, 0, i64::MAX), Ok((0, LOCK_END - 1)));
        // Far from either end: what overflows nothing is a span.
        assert_eq!(span(i64::MAX, -i64::MAX, 0), Ok((0, LOCK_END)));
        assert_eq!(span(i64::MAX, i64::MIN, 0), Err(EINVAL));
    }

    /// A descriptor of the empty input, in the table (which the tests share).
    fn null(mode: u32) -> i32 {
        fd::insert(Description::new(Object::Null, mode), false, 0).unwrap()
    }

    #[test]
    fn what_is_no_file_takes_no_locks() {
        use crate::linux::flock::{EX, MAND, NB, SH, UN};
        let (rw, ro) = (null(o::RDWR), null(o::RDONLY));
        assert_eq!(flock(rw, EX), Err(ENOLCK));
        assert_eq!(flock(rw, SH | NB), Err(ENOLCK));
        assert_eq!(flock(rw, UN), Ok(0));
        assert_eq!(flock(rw, 0), Err(EINVAL));
        assert_eq!(flock(rw, SH | EX), Err(EINVAL));
        // Mandatory locks are ignored, as on Linux, even for no descriptor.
        assert_eq!(flock(-1, MAND | SH), Ok(0));
        assert_eq!(flock(-1, SH), Err(EBADF));
        let lock_with = |fd: i32, cmd: u32, l: Flock| {
            let mut l = l;
            // SAFETY: a `struct flock` of the test's.
            let r = unsafe { fcntl(fd, cmd, &mut l as *mut Flock as usize) };
            (r, l.kind)
        };
        let of = |kind: i16| Flock { kind, ..Default::default() };
        assert_eq!(lock_with(rw, fcntl::SETLK, of(fcntl::F_WRLCK)).0, Err(ENOLCK));
        assert_eq!(lock_with(rw, fcntl::SETLKW, of(fcntl::F_RDLCK)).0, Err(ENOLCK));
        assert_eq!(lock_with(rw, fcntl::OFD_SETLK, of(fcntl::F_RDLCK)).0, Err(ENOLCK));
        assert_eq!(lock_with(rw, fcntl::SETLK, of(fcntl::F_UNLCK)).0, Ok(0));
        // None is held, so none is in the way.
        assert_eq!(lock_with(rw, fcntl::GETLK, of(fcntl::F_WRLCK)), (Ok(0), fcntl::F_UNLCK));
        assert_eq!(lock_with(ro, fcntl::GETLK, of(fcntl::F_WRLCK)), (Ok(0), fcntl::F_UNLCK));
        assert_eq!(lock_with(rw, fcntl::OFD_GETLK, of(fcntl::F_UNLCK)), (Ok(0), fcntl::F_UNLCK));
        // A lock the descriptor's access does not allow, no lock at all, a
        // test of none, an OFD lock naming a process, bytes before the
        // start or an origin that is none.
        assert_eq!(lock_with(ro, fcntl::SETLK, of(fcntl::F_WRLCK)).0, Err(EBADF));
        assert_eq!(lock_with(rw, fcntl::SETLK, of(7)).0, Err(EINVAL));
        assert_eq!(lock_with(rw, fcntl::GETLK, of(fcntl::F_UNLCK)).0, Err(EINVAL));
        assert_eq!(lock_with(rw, fcntl::OFD_SETLK, Flock { pid: 1, ..of(fcntl::F_RDLCK) }).0, Err(EINVAL));
        assert_eq!(lock_with(rw, fcntl::SETLK, Flock { start: -1, ..of(fcntl::F_RDLCK) }).0, Err(EINVAL));
        assert_eq!(lock_with(rw, fcntl::SETLK, Flock { whence: 3, ..of(fcntl::F_RDLCK) }).0, Err(EINVAL));
        let _ = (fd::remove(rw), fd::remove(ro));
    }
}
