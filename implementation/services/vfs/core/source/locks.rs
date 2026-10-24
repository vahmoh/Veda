// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Advisory locks on files, which the VFS arbitrates between processes
//! (`file::lock`, `file::test_lock`), as Linux keeps them (its
//! `fs/locks.c`):
//!
//! * **`flock`'s**, on a whole file, held by an open file (an open file
//!   description), shared or exclusive. An open file holds one at most, and
//!   changing its kind takes the old one away first, as on Linux: a change
//!   refused leaves none.
//! * **Locks on bytes**, held by a process (POSIX's record locks,
//!   `fcntl`'s) or by an open file (Linux's OFD locks), which are in each
//!   other's way but not in `flock`'s. One owner's locks never overlap: a
//!   new one replaces what the owner held of its bytes, and merges with the
//!   owner's locks of its kind that it touches.
//!
//! A shared lock is in the way of other owners' exclusive ones, and an
//! exclusive one of all theirs. A request that a lock is in the way of
//! fails, or waits: waiting requests are granted, in the order they came,
//! once nothing is in their way, and a process's request that would wait
//! for ever fails with `Deadlock`, as Linux finds that
//! ([`MAX_DEADLOCK_HOPS`]). What becomes of waiting requests is told as
//! [`Notice`]s, which the VFS writes on their channels.
//!
//! A process owns locks by a secret only it has ([`Owners`]), drawn for the
//! connection that speaks for it (`vfs::lock_owner`): its locks go as that
//! connection closes ([`Locks::release`]), as a process's do as it ends.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use proto::fs::{FsError, LOCK_END, LOCK_OWNER_SIZE};

use crate::same;

/// A file, by its file system's number and its inode (`Stat::device` and
/// `Stat::inode`), which no other file has while the system runs.
pub type FileId = (u64, u64);

/// Who holds a lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Owner {
    /// An open file (an open file description), by the VFS's number for
    /// it: `flock`'s locks, and OFD locks.
    Open(u64),
    /// A process, by the VFS's number for the connection that speaks for
    /// it ([`Owners`]): POSIX's record locks.
    Process(u64),
}

/// A lock's kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Others may hold shared locks there too.
    Shared,
    /// No one else may hold a lock there.
    Exclusive,
}

impl Kind {
    /// Whether a lock of this kind and one of `other`'s, of two owners, are
    /// in each other's way.
    fn conflicts(self, other: Kind) -> bool {
        self == Kind::Exclusive || other == Kind::Exclusive
    }
}

/// A lock on bytes of a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lock {
    pub owner: Owner,
    pub kind: Kind,
    /// Its first byte and its last, both held ([`LOCK_END`] at most).
    pub start: u64,
    pub end: u64,
}

impl Lock {
    /// Whether it holds any of the bytes `start..=end`.
    fn overlaps(&self, start: u64, end: u64) -> bool {
        self.start <= end && start <= self.end
    }
}

/// What a request asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Request {
    /// `flock`'s lock on the whole file, rather than one on bytes (`start`
    /// and `end` then count for nothing).
    pub whole: bool,
    /// The lock to hold; none to take away what the owner holds there.
    pub kind: Option<Kind>,
    /// The first byte and the last ([`LOCK_END`] at most).
    pub start: u64,
    pub end: u64,
}

/// What became of a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Done: the lock asked for is held, or what was asked to go is gone.
    Done,
    /// It waits, as the waiter of this number, until it is granted or fails
    /// ([`Notice`]).
    Waiting(u64),
}

/// What became of a waiting request: granted (saying whether its owner
/// holds locks on the file's bytes now), or failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Notice {
    pub waiter: u64,
    pub result: Result<bool, FsError>,
}

/// The most locks the VFS holds, on all files together, which keeps what
/// they take of its memory small...
pub const MAX_LOCKS: usize = 1 << 16;
/// ... and on one file.
pub const MAX_FILE_LOCKS: usize = 4096;

/// How many waits deep a process's request is followed, looking for one for
/// a lock of the process asking: eleven, as far as Linux follows them (its
/// `MAX_DEADLK_ITERATIONS`, and one more).
pub const MAX_DEADLOCK_HOPS: usize = 11;

/// One file's locks.
#[derive(Default)]
struct FileLocks {
    /// `flock`'s: the open files that hold one, and its kind.
    whole: Vec<(u64, Kind)>,
    /// Locks on bytes: each owner's together, by their first bytes, owners
    /// in the order they came, as Linux lists them (which `F_GETLK` shows).
    bytes: Vec<Lock>,
}

impl FileLocks {
    fn len(&self) -> usize {
        self.whole.len() + self.bytes.len()
    }
}

/// A request that waits.
#[derive(Debug, Clone, Copy)]
struct Waiter {
    id: u64,
    file: FileId,
    owner: Owner,
    request: Request,
    /// The owner of the lock in its way, when it was last looked at.
    blocked_by: Owner,
}

impl Waiter {
    /// Whether its wait is followed in looking for deadlocks: a process's,
    /// for a lock on bytes (as Linux follows waits).
    fn followed(&self) -> bool {
        matches!(self.owner, Owner::Process(_)) && !self.request.whole
    }
}

/// The locks the VFS holds, and the requests that wait for one.
pub struct Locks {
    files: BTreeMap<FileId, FileLocks>,
    /// In the order they came.
    waiters: Vec<Waiter>,
    /// The number of the next waiter.
    next: u64,
    /// The locks held, on every file.
    count: usize,
    /// What became of waiting requests, not yet told.
    notices: Vec<Notice>,
}

impl Default for Locks {
    fn default() -> Locks {
        Locks::new()
    }
}

impl Locks {
    pub const fn new() -> Locks {
        Locks { files: BTreeMap::new(), waiters: Vec::new(), next: 1, count: 0, notices: Vec::new() }
    }

    /// Carries out `owner`'s `request` on `file`: holds the lock asked for,
    /// or takes away what the owner holds there. A lock in the way fails it
    /// with `Busy`, or, with `wait`, has it wait; a process's request that
    /// would wait for ever fails with `Deadlock` ([`Locks::deadlocked`]).
    /// `Invalid` for bytes that end before they start or past
    /// [`LOCK_END`], or for `flock`'s lock asked for by a process;
    /// `TooMany` for more locks than [`MAX_LOCKS`] in all or
    /// [`MAX_FILE_LOCKS`] on the file.
    pub fn lock(&mut self, file: FileId, owner: Owner, request: Request, wait: bool) -> Result<Outcome, FsError> {
        if request.whole {
            return self.lock_whole(file, owner, request, wait);
        }
        if request.start > request.end || request.end > LOCK_END {
            return Err(FsError::Invalid);
        }
        let Some(kind) = request.kind else {
            self.set_bytes(file, owner, None, request.start, request.end)?;
            self.settle(file);
            return Ok(Outcome::Done);
        };
        if let Some(holder) = self.bytes_in_the_way(file, owner, kind, request.start, request.end) {
            if !wait {
                return Err(FsError::Busy);
            }
            if matches!(owner, Owner::Process(_)) && self.deadlocked(owner, holder.owner) {
                return Err(FsError::Deadlock);
            }
            return Ok(Outcome::Waiting(self.wait(file, owner, request, holder.owner)));
        }
        self.set_bytes(file, owner, Some(kind), request.start, request.end)?;
        self.settle(file);
        Ok(Outcome::Done)
    }

    /// [`Locks::lock`] for `flock`'s lock.
    fn lock_whole(&mut self, file: FileId, owner: Owner, request: Request, wait: bool) -> Result<Outcome, FsError> {
        let Owner::Open(open) = owner else { return Err(FsError::Invalid) };
        // One of the kind asked for stays as it is; one of the other kind
        // goes first, whether the new one is granted or not, as on Linux.
        let mut changed = false;
        if let Some(locks) = self.files.get_mut(&file)
            && let Some(i) = locks.whole.iter().position(|&(o, _)| o == open)
        {
            if Some(locks.whole[i].1) == request.kind {
                return Ok(Outcome::Done);
            }
            locks.whole.remove(i);
            self.count -= 1;
            changed = true;
        }
        let outcome = match request.kind {
            None => Ok(Outcome::Done),
            Some(kind) => match self.whole_in_the_way(file, owner, kind) {
                Some(holder) if wait => Ok(Outcome::Waiting(self.wait(file, owner, request, holder))),
                Some(_) => Err(FsError::Busy),
                None => self.put_whole(file, open, kind).map(|()| {
                    changed = true;
                    Outcome::Done
                }),
            },
        };
        if changed {
            self.settle(file);
        }
        outcome
    }

    /// The first lock on `file`'s bytes in the way of one of `kind`'s on
    /// `start..=end` for `owner`, in the order Linux keeps them (each
    /// owner's together, by their first bytes, owners in the order they
    /// came): `F_GETLK`'s answer. For no kind (Linux's `F_OFD_GETLK` of
    /// `F_UNLCK`), the first of `owner`'s own among those bytes.
    pub fn test(&self, file: FileId, owner: Owner, kind: Option<Kind>, start: u64, end: u64) -> Option<Lock> {
        match kind {
            Some(kind) => self.bytes_in_the_way(file, owner, kind, start, end),
            None => self.files.get(&file)?.bytes.iter().find(|l| l.owner == owner && l.overlaps(start, end)).copied(),
        }
    }

    /// Whether `owner` holds locks on `file`'s bytes.
    pub fn holds(&self, file: FileId, owner: Owner) -> bool {
        self.files.get(&file).is_some_and(|locks| locks.bytes.iter().any(|l| l.owner == owner))
    }

    /// Waiter `waiter` gave up (its channel closed): it waits no more.
    pub fn cancel(&mut self, waiter: u64) {
        self.waiters.retain(|w| w.id != waiter);
    }

    /// `owner` is no more (an open file's last connection closed, or the
    /// connection that speaks for a process): its locks go from every file,
    /// and its waiting requests fail (`BadFd`).
    pub fn release(&mut self, owner: Owner) {
        let mut changed = Vec::new();
        for (file, locks) in self.files.iter_mut() {
            let before = locks.len();
            locks.bytes.retain(|l| l.owner != owner);
            locks.whole.retain(|&(o, _)| Owner::Open(o) != owner);
            if locks.len() < before {
                self.count -= before - locks.len();
                changed.push(*file);
            }
        }
        self.fail_waiters(|w| w.owner == owner);
        for file in changed {
            self.settle(file);
        }
    }

    /// `file` is open no more: its locks go, and its waiting requests fail
    /// (`BadFd`).
    pub fn forget(&mut self, file: FileId) {
        if let Some(locks) = self.files.remove(&file) {
            self.count -= locks.len();
        }
        self.fail_waiters(|w| w.file == file);
    }

    /// What became of waiting requests since this was last asked.
    pub fn notices(&mut self) -> Vec<Notice> {
        core::mem::take(&mut self.notices)
    }

    /// Fails the waiting requests `gone` picks (`BadFd`).
    fn fail_waiters(&mut self, gone: impl Fn(&Waiter) -> bool) {
        let (failed, kept): (Vec<Waiter>, Vec<Waiter>) = core::mem::take(&mut self.waiters).into_iter().partition(gone);
        self.waiters = kept;
        self.notices.extend(failed.into_iter().map(|w| Notice { waiter: w.id, result: Err(FsError::BadFd) }));
    }

    /// Has `owner`'s `request` on `file` wait, its way barred by a lock of
    /// `blocked_by`'s; returns its waiter's number.
    fn wait(&mut self, file: FileId, owner: Owner, request: Request, blocked_by: Owner) -> u64 {
        let id = self.next;
        self.next += 1;
        self.waiters.push(Waiter { id, file, owner, request, blocked_by });
        id
    }

    /// The first lock on `file`'s bytes that is in the way of one of
    /// `kind`'s on `start..=end` for `owner`.
    fn bytes_in_the_way(&self, file: FileId, owner: Owner, kind: Kind, start: u64, end: u64) -> Option<Lock> {
        let locks = self.files.get(&file)?;
        locks.bytes.iter().find(|l| l.owner != owner && l.overlaps(start, end) && l.kind.conflicts(kind)).copied()
    }

    /// The open file whose `flock` lock on `file` is in the way of one of
    /// `kind`'s for `owner`.
    fn whole_in_the_way(&self, file: FileId, owner: Owner, kind: Kind) -> Option<Owner> {
        let locks = self.files.get(&file)?;
        locks.whole.iter().find(|&&(o, k)| Owner::Open(o) != owner && k.conflicts(kind)).map(|&(o, _)| Owner::Open(o))
    }

    /// Gives open file `open` `flock`'s lock of `kind` on `file`, in place
    /// of the one it holds.
    fn put_whole(&mut self, file: FileId, open: u64, kind: Kind) -> Result<(), FsError> {
        let full = self.count >= MAX_LOCKS;
        let locks = self.files.entry(file).or_default();
        if let Some(mine) = locks.whole.iter_mut().find(|(o, _)| *o == open) {
            mine.1 = kind;
            return Ok(());
        }
        if full || locks.len() >= MAX_FILE_LOCKS {
            self.tidy(file);
            return Err(FsError::TooMany);
        }
        locks.whole.push((open, kind));
        self.count += 1;
        Ok(())
    }

    /// Makes `owner`'s locks on `file`'s bytes `start..=end` one of
    /// `kind`'s, or none: what it held of them goes, what it held beside
    /// them stays, and its locks of one kind that touch merge.
    fn set_bytes(
        &mut self,
        file: FileId,
        owner: Owner,
        kind: Option<Kind>,
        start: u64,
        end: u64,
    ) -> Result<(), FsError> {
        let empty = FileLocks::default();
        let locks = self.files.get(&file).unwrap_or(&empty);
        // The owner's locks, together: where, and how many.
        let at = locks.bytes.iter().position(|l| l.owner == owner).unwrap_or(locks.bytes.len());
        let len = locks.bytes[at..].iter().take_while(|l| l.owner == owner).count();
        let mut mine = Vec::with_capacity(len + 2);
        for l in &locks.bytes[at..at + len] {
            if !l.overlaps(start, end) {
                mine.push(*l);
                continue;
            }
            if l.start < start {
                mine.push(Lock { end: start - 1, ..*l });
            }
            if l.end > end {
                mine.push(Lock { start: end + 1, ..*l });
            }
        }
        if let Some(kind) = kind {
            mine.push(Lock { owner, kind, start, end });
        }
        mine.sort_by_key(|l| l.start);
        let mut merged: Vec<Lock> = Vec::with_capacity(mine.len());
        for l in mine {
            match merged.last_mut() {
                // No lock ends past LOCK_END: one more is still a number.
                Some(last) if last.kind == l.kind && last.end + 1 >= l.start => last.end = last.end.max(l.end),
                _ => merged.push(l),
            }
        }
        let grows = merged.len() > len;
        if grows && (self.count + merged.len() - len > MAX_LOCKS || locks.len() + merged.len() - len > MAX_FILE_LOCKS) {
            return Err(FsError::TooMany);
        }
        if len == 0 && merged.is_empty() {
            return Ok(());
        }
        self.count = self.count + merged.len() - len;
        let locks = self.files.entry(file).or_default();
        locks.bytes.splice(at..at + len, merged);
        Ok(())
    }

    /// Grants the requests that wait on `file` that nothing is in the way
    /// of now, in the order they came, and fails a process's that would
    /// wait for ever, as Linux's waiters, woken, try again.
    fn settle(&mut self, file: FileId) {
        let mut i = 0;
        while let Some(&w) = self.waiters.get(i) {
            if w.file != file {
                i += 1;
                continue;
            }
            let holder = match w.request {
                Request { kind: None, .. } => None,
                Request { whole: true, kind: Some(kind), .. } => self.whole_in_the_way(file, w.owner, kind),
                Request { kind: Some(kind), start, end, .. } => {
                    self.bytes_in_the_way(file, w.owner, kind, start, end).map(|l| l.owner)
                }
            };
            match holder {
                None => {
                    self.waiters.remove(i);
                    let result = self.grant(&w);
                    self.notices.push(Notice { waiter: w.id, result });
                    // What its owner holds now may let those before it go.
                    i = 0;
                }
                Some(holder) => {
                    self.waiters[i].blocked_by = holder;
                    if w.followed() && self.deadlocked(w.owner, holder) {
                        self.waiters.remove(i);
                        self.notices.push(Notice { waiter: w.id, result: Err(FsError::Deadlock) });
                    } else {
                        i += 1;
                    }
                }
            }
        }
        self.tidy(file);
    }

    /// Holds the lock waiter `w` asked for, nothing being in its way;
    /// whether its owner holds locks on the file's bytes now.
    fn grant(&mut self, w: &Waiter) -> Result<bool, FsError> {
        match (w.request.kind, w.owner) {
            (Some(kind), Owner::Open(open)) if w.request.whole => self.put_whole(w.file, open, kind)?,
            (Some(kind), _) if !w.request.whole => {
                self.set_bytes(w.file, w.owner, Some(kind), w.request.start, w.request.end)?
            }
            _ => return Err(FsError::Invalid),
        }
        Ok(self.holds(w.file, w.owner))
    }

    /// Whether `caller`, a process, waiting for a lock of `holder`'s would
    /// wait for ever, as Linux finds that: `holder` waits for a lock the
    /// caller holds, or for one of an owner that waits for one the caller
    /// holds, and so on, [`MAX_DEADLOCK_HOPS`] deep. Of each process, its
    /// last request to wait (for a lock on bytes) is followed; open files'
    /// waits are not, as they belong to no process.
    fn deadlocked(&self, caller: Owner, holder: Owner) -> bool {
        let mut holder = holder;
        for _ in 0..MAX_DEADLOCK_HOPS {
            let Some(w) = self.waiters.iter().rev().find(|w| w.owner == holder && w.followed()) else { return false };
            holder = w.blocked_by;
            if holder == caller {
                return true;
            }
        }
        false
    }

    /// Forgets `file`'s locks if it has none left.
    fn tidy(&mut self, file: FileId) {
        if self.files.get(&file).is_some_and(|locks| locks.len() == 0) {
            self.files.remove(&file);
        }
    }
}

/// The processes that own locks: each by the connection that speaks for
/// it, the secret that names it (`vfs::lock_owner`), drawn from the
/// kernel's random generator, and the kernel object id it gave. Secrets are
/// compared in constant time: no answer tells how near a guess came.
#[derive(Default)]
pub struct Owners {
    owners: Vec<OwnerEntry>,
}

struct OwnerEntry {
    conn: u64,
    secret: [u8; LOCK_OWNER_SIZE],
    koid: u64,
}

impl Owners {
    pub const fn new() -> Owners {
        Owners { owners: Vec::new() }
    }

    /// The secret of connection `conn`'s process, if it has one.
    pub fn secret(&self, conn: u64) -> Option<[u8; LOCK_OWNER_SIZE]> {
        self.owners.iter().find(|o| o.conn == conn).map(|o| o.secret)
    }

    /// Names connection `conn`'s process, of kernel object id `koid`, by
    /// `secret` (random bytes the caller drew), unless it is named already.
    pub fn add(&mut self, conn: u64, koid: u64, secret: [u8; LOCK_OWNER_SIZE]) {
        if self.secret(conn).is_none() {
            self.owners.push(OwnerEntry { conn, secret, koid });
        }
    }

    /// The connection of the process whose secret is `secret`.
    pub fn find(&self, secret: &[u8]) -> Option<u64> {
        self.owners.iter().find(|o| same(&o.secret, secret)).map(|o| o.conn)
    }

    /// The kernel object id connection `conn`'s process gave.
    pub fn koid(&self, conn: u64) -> Option<u64> {
        self.owners.iter().find(|o| o.conn == conn).map(|o| o.koid)
    }

    /// Connection `conn` closed: its secret names no one any more.
    pub fn remove(&mut self, conn: u64) {
        self.owners.retain(|o| o.conn != conn);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    const FILE: FileId = (1, 10);
    const OTHER_FILE: FileId = (1, 11);
    const P1: Owner = Owner::Process(1);
    const P2: Owner = Owner::Process(2);
    const P3: Owner = Owner::Process(3);
    const O1: Owner = Owner::Open(1);
    const O2: Owner = Owner::Open(2);
    const O3: Owner = Owner::Open(3);

    fn bytes(kind: Option<Kind>, start: u64, end: u64) -> Request {
        Request { whole: false, kind, start, end }
    }

    fn whole(kind: Option<Kind>) -> Request {
        Request { whole: true, kind, start: 0, end: LOCK_END }
    }

    fn shared(start: u64, end: u64) -> Request {
        bytes(Some(Kind::Shared), start, end)
    }

    fn exclusive(start: u64, end: u64) -> Request {
        bytes(Some(Kind::Exclusive), start, end)
    }

    fn unlock(start: u64, end: u64) -> Request {
        bytes(None, start, end)
    }

    /// `owner`'s locks on `file`'s bytes: kind, first and last byte.
    fn held(locks: &Locks, file: FileId, owner: Owner) -> Vec<(Kind, u64, u64)> {
        locks
            .files
            .get(&file)
            .map_or(vec![], |f| f.bytes.iter().filter(|l| l.owner == owner).map(|l| (l.kind, l.start, l.end)).collect())
    }

    /// The number of the request that `outcome` says waits.
    fn waiter(outcome: Result<Outcome, FsError>) -> u64 {
        match outcome {
            Ok(Outcome::Waiting(id)) => id,
            other => panic!("not waiting: {other:?}"),
        }
    }

    #[test]
    fn flock_locks_are_shared_or_exclusive() {
        let mut locks = Locks::new();
        assert_eq!(locks.lock(FILE, O1, whole(Some(Kind::Shared)), false), Ok(Outcome::Done));
        assert_eq!(locks.lock(FILE, O2, whole(Some(Kind::Shared)), false), Ok(Outcome::Done));
        assert_eq!(locks.lock(FILE, O3, whole(Some(Kind::Exclusive)), false), Err(FsError::Busy));
        // The same again changes nothing; another file is apart.
        assert_eq!(locks.lock(FILE, O1, whole(Some(Kind::Shared)), false), Ok(Outcome::Done));
        assert_eq!(locks.lock(OTHER_FILE, O3, whole(Some(Kind::Exclusive)), false), Ok(Outcome::Done));
        assert_eq!(locks.lock(FILE, O1, whole(None), false), Ok(Outcome::Done));
        assert_eq!(locks.lock(FILE, O2, whole(None), false), Ok(Outcome::Done));
        assert_eq!(locks.lock(FILE, O3, whole(Some(Kind::Exclusive)), false), Ok(Outcome::Done));
        assert_eq!(locks.lock(FILE, O1, whole(Some(Kind::Shared)), false), Err(FsError::Busy));
        // Unlocking what is not held is no error.
        assert_eq!(locks.lock(FILE, O2, whole(None), false), Ok(Outcome::Done));
        // Only open files hold them.
        assert_eq!(locks.lock(FILE, P1, whole(Some(Kind::Shared)), false), Err(FsError::Invalid));
        assert_eq!(locks.count, 2);
    }

    #[test]
    fn a_flock_lock_changing_kind_goes_first() {
        let mut locks = Locks::new();
        locks.lock(FILE, O1, whole(Some(Kind::Shared)), false).unwrap();
        locks.lock(FILE, O2, whole(Some(Kind::Shared)), false).unwrap();
        // Refused: the shared lock it held is gone, as on Linux.
        assert_eq!(locks.lock(FILE, O1, whole(Some(Kind::Exclusive)), false), Err(FsError::Busy));
        assert_eq!(locks.lock(FILE, O2, whole(Some(Kind::Exclusive)), false), Ok(Outcome::Done));
        // To shared, alone: granted at once.
        assert_eq!(locks.lock(FILE, O2, whole(Some(Kind::Shared)), false), Ok(Outcome::Done));
        assert_eq!(locks.lock(FILE, O1, whole(Some(Kind::Shared)), false), Ok(Outcome::Done));
        assert_eq!(locks.count, 2);
    }

    #[test]
    fn flock_locks_and_locks_on_bytes_are_apart() {
        let mut locks = Locks::new();
        locks.lock(FILE, O1, whole(Some(Kind::Exclusive)), false).unwrap();
        assert_eq!(locks.lock(FILE, P1, exclusive(0, LOCK_END), false), Ok(Outcome::Done));
        assert_eq!(locks.lock(FILE, O2, exclusive(5, 5), false), Err(FsError::Busy));
        assert_eq!(locks.test(FILE, O2, Some(Kind::Shared), 0, 0).map(|l| l.owner), Some(P1));
        // flock's are not in the way of a test of bytes.
        locks.lock(FILE, P1, unlock(0, LOCK_END), false).unwrap();
        assert_eq!(locks.test(FILE, P2, Some(Kind::Exclusive), 0, LOCK_END), None);
    }

    #[test]
    fn one_owners_locks_on_bytes_replace_and_merge() {
        let mut locks = Locks::new();
        locks.lock(FILE, P1, exclusive(0, 9), false).unwrap();
        locks.lock(FILE, P1, shared(10, 19), false).unwrap();
        locks.lock(FILE, P1, shared(20, 29), false).unwrap();
        // Of one kind and touching: merged.
        assert_eq!(held(&locks, FILE, P1), [(Kind::Exclusive, 0, 9), (Kind::Shared, 10, 29)]);
        // Replaced in the middle: split.
        locks.lock(FILE, P1, exclusive(15, 16), false).unwrap();
        assert_eq!(
            held(&locks, FILE, P1),
            [(Kind::Exclusive, 0, 9), (Kind::Shared, 10, 14), (Kind::Exclusive, 15, 16), (Kind::Shared, 17, 29)]
        );
        // An unlock in the middle leaves both sides.
        locks.lock(FILE, P1, unlock(5, 12), false).unwrap();
        assert_eq!(
            held(&locks, FILE, P1),
            [(Kind::Exclusive, 0, 4), (Kind::Shared, 13, 14), (Kind::Exclusive, 15, 16), (Kind::Shared, 17, 29)]
        );
        // One lock over all of them, and past them, makes one.
        locks.lock(FILE, P1, shared(0, LOCK_END), false).unwrap();
        assert_eq!(held(&locks, FILE, P1), [(Kind::Shared, 0, LOCK_END)]);
        // A lock touching another of its kind joins it, at either end.
        locks.lock(FILE, P2, exclusive(100, 199), false).unwrap_err();
        locks.lock(FILE, P1, unlock(0, LOCK_END), false).unwrap();
        locks.lock(FILE, P2, exclusive(100, 199), false).unwrap();
        locks.lock(FILE, P2, exclusive(200, 299), false).unwrap();
        locks.lock(FILE, P2, exclusive(50, 99), false).unwrap();
        assert_eq!(held(&locks, FILE, P2), [(Kind::Exclusive, 50, 299)]);
        assert_eq!(locks.count, 1);
        locks.lock(FILE, P2, unlock(0, LOCK_END), false).unwrap();
        assert_eq!(locks.count, 0);
        assert!(locks.files.is_empty());
    }

    #[test]
    fn locks_on_bytes_are_in_each_others_way_but_for_one_owners() {
        let mut locks = Locks::new();
        locks.lock(FILE, P1, shared(0, 9), false).unwrap();
        // Shared and shared go together; exclusive does not.
        assert_eq!(locks.lock(FILE, P2, shared(5, 14), false), Ok(Outcome::Done));
        assert_eq!(locks.lock(FILE, P3, exclusive(9, 9), false), Err(FsError::Busy));
        assert_eq!(locks.lock(FILE, P3, exclusive(15, 20), false), Ok(Outcome::Done));
        assert_eq!(locks.lock(FILE, P1, shared(20, 20), false), Err(FsError::Busy));
        // An owner's own locks are never in its way.
        assert_eq!(locks.lock(FILE, P1, exclusive(0, 4), false), Ok(Outcome::Done));
        // An open file's locks and a process's are in each other's way.
        assert_eq!(locks.lock(FILE, O1, shared(0, 0), false), Err(FsError::Busy));
        assert_eq!(locks.lock(FILE, O1, shared(5, 5), false), Ok(Outcome::Done));
        assert_eq!(locks.lock(FILE, P2, exclusive(5, 5), false), Err(FsError::Busy));
        // Bytes that end before they start, or past the largest offset.
        assert_eq!(locks.lock(FILE, P1, shared(5, 4), false), Err(FsError::Invalid));
        assert_eq!(locks.lock(FILE, P1, shared(0, LOCK_END + 1), false), Err(FsError::Invalid));
    }

    #[test]
    fn a_test_finds_the_first_lock_in_the_way_as_linux_lists_them() {
        let mut locks = Locks::new();
        // P2 came first: its locks come first, whatever their bytes.
        locks.lock(FILE, P2, shared(50, 59), false).unwrap();
        locks.lock(FILE, P1, exclusive(0, 9), false).unwrap();
        locks.lock(FILE, P2, shared(10, 19), false).unwrap();
        let found = |kind, start, end| locks.test(FILE, P3, Some(kind), start, end);
        assert_eq!(
            found(Kind::Exclusive, 0, LOCK_END),
            Some(Lock { owner: P2, kind: Kind::Shared, start: 10, end: 19 })
        );
        assert_eq!(found(Kind::Shared, 0, LOCK_END), Some(Lock { owner: P1, kind: Kind::Exclusive, start: 0, end: 9 }));
        assert_eq!(found(Kind::Shared, 10, 100), None);
        // Its own are no answer; for no kind, they are.
        assert_eq!(locks.test(FILE, P1, Some(Kind::Exclusive), 0, 9), None);
        assert_eq!(locks.test(FILE, P2, None, 0, LOCK_END).map(|l| l.start), Some(10));
        assert_eq!(locks.test(FILE, P3, None, 0, LOCK_END), None);
        assert_eq!(locks.test(OTHER_FILE, P3, Some(Kind::Exclusive), 0, LOCK_END), None);
        // A process that unlocks all and locks again comes last.
        locks.lock(FILE, P2, unlock(0, LOCK_END), false).unwrap();
        locks.lock(FILE, P2, shared(10, 19), false).unwrap();
        assert_eq!(locks.test(FILE, P3, Some(Kind::Exclusive), 0, LOCK_END).map(|l| l.owner), Some(P1));
    }

    #[test]
    fn waiting_requests_are_granted_in_the_order_they_came() {
        let mut locks = Locks::new();
        locks.lock(FILE, P1, exclusive(0, 9), false).unwrap();
        let a = waiter(locks.lock(FILE, P2, exclusive(0, 0), true));
        let b = waiter(locks.lock(FILE, P3, shared(5, 5), true));
        let c = waiter(locks.lock(FILE, O1, exclusive(0, 9), true));
        assert!(locks.notices().is_empty());
        // Half of it goes: the request for those bytes alone is granted.
        locks.lock(FILE, P1, unlock(0, 4), false).unwrap();
        assert_eq!(locks.notices(), [Notice { waiter: a, result: Ok(true) }]);
        // The rest: P3 is granted, the open file waits for both.
        locks.lock(FILE, P1, unlock(0, LOCK_END), false).unwrap();
        assert_eq!(locks.notices(), [Notice { waiter: b, result: Ok(true) }]);
        locks.lock(FILE, P2, unlock(0, LOCK_END), false).unwrap();
        assert!(locks.notices().is_empty());
        locks.lock(FILE, P3, unlock(0, LOCK_END), false).unwrap();
        assert_eq!(locks.notices(), [Notice { waiter: c, result: Ok(true) }]);
        assert_eq!(held(&locks, FILE, O1), [(Kind::Exclusive, 0, 9)]);
    }

    #[test]
    fn a_grant_that_frees_bytes_lets_those_before_it_go() {
        let mut locks = Locks::new();
        // P2 holds 0..=9 exclusively and waits to share it all with P3.
        locks.lock(FILE, P2, exclusive(0, 9), false).unwrap();
        locks.lock(FILE, P3, exclusive(20, 29), false).unwrap();
        let first = waiter(locks.lock(FILE, P1, shared(5, 5), true));
        let second = waiter(locks.lock(FILE, P2, shared(0, 29), true));
        // P3's lock goes: P2's request is granted, and its exclusive lock
        // becoming shared lets P1's go too.
        locks.lock(FILE, P3, unlock(0, LOCK_END), false).unwrap();
        assert_eq!(
            locks.notices(),
            [Notice { waiter: second, result: Ok(true) }, Notice { waiter: first, result: Ok(true) }]
        );
    }

    #[test]
    fn flock_requests_wait_too() {
        let mut locks = Locks::new();
        locks.lock(FILE, O1, whole(Some(Kind::Shared)), false).unwrap();
        locks.lock(FILE, O2, whole(Some(Kind::Shared)), false).unwrap();
        // A change to exclusive gives up the shared lock, then waits.
        let w = waiter(locks.lock(FILE, O1, whole(Some(Kind::Exclusive)), true));
        assert_eq!(locks.lock(FILE, O3, whole(Some(Kind::Exclusive)), false), Err(FsError::Busy));
        locks.lock(FILE, O2, whole(None), false).unwrap();
        assert_eq!(locks.notices(), [Notice { waiter: w, result: Ok(false) }]);
        assert_eq!(locks.files[&FILE].whole, [(1, Kind::Exclusive)]);
    }

    #[test]
    fn a_waiter_that_gives_up_waits_no_more() {
        let mut locks = Locks::new();
        locks.lock(FILE, P1, exclusive(0, 0), false).unwrap();
        let w = waiter(locks.lock(FILE, P2, exclusive(0, 0), true));
        locks.cancel(w);
        locks.lock(FILE, P1, unlock(0, 0), false).unwrap();
        assert!(locks.notices().is_empty());
        assert!(!locks.holds(FILE, P2));
    }

    #[test]
    fn two_processes_waiting_for_each_other_deadlock() {
        let mut locks = Locks::new();
        locks.lock(FILE, P1, exclusive(0, 0), false).unwrap();
        locks.lock(FILE, P2, exclusive(1, 1), false).unwrap();
        let w = waiter(locks.lock(FILE, P1, exclusive(1, 1), true));
        // P2 would wait for P1, which waits for P2.
        assert_eq!(locks.lock(FILE, P2, exclusive(0, 0), true), Err(FsError::Deadlock));
        // Without waiting it is only busy; P1's request still waits.
        assert_eq!(locks.lock(FILE, P2, exclusive(0, 0), false), Err(FsError::Busy));
        locks.lock(FILE, P2, unlock(1, 1), false).unwrap();
        assert_eq!(locks.notices(), [Notice { waiter: w, result: Ok(true) }]);
    }

    #[test]
    fn deadlocks_are_found_through_other_processes() {
        let mut locks = Locks::new();
        for (n, owner) in [P1, P2, P3].into_iter().enumerate() {
            locks.lock(FILE, owner, exclusive(n as u64, n as u64), false).unwrap();
        }
        // P1 waits for P2, P2 for P3 (on another file's lock too).
        waiter(locks.lock(FILE, P1, exclusive(1, 1), true));
        locks.lock(OTHER_FILE, P3, exclusive(0, 0), false).unwrap();
        waiter(locks.lock(OTHER_FILE, P2, shared(0, 0), true));
        assert_eq!(locks.lock(FILE, P3, exclusive(0, 0), true), Err(FsError::Deadlock));
        // An open file's waits are not followed, nor is a wait on one.
        assert!(locks.lock(FILE, O1, exclusive(2, 2), true).is_ok());
        locks.lock(FILE, O2, exclusive(10, 10), false).unwrap();
        assert!(locks.lock(FILE, P3, exclusive(10, 10), true).is_ok());
    }

    #[test]
    fn deadlocks_are_followed_as_deep_as_linux_follows_them() {
        // Processes 1..=n each hold byte n, and wait for byte n + 1: a
        // request of process n + 1 for byte 1 closes the circle.
        for n in [MAX_DEADLOCK_HOPS, MAX_DEADLOCK_HOPS + 1] {
            let mut locks = Locks::new();
            for p in 1..=n as u64 + 1 {
                locks.lock(FILE, Owner::Process(p), exclusive(p, p), false).unwrap();
            }
            for p in 1..=n as u64 {
                waiter(locks.lock(FILE, Owner::Process(p), exclusive(p + 1, p + 1), true));
            }
            let last = Owner::Process(n as u64 + 1);
            let r = locks.lock(FILE, last, exclusive(1, 1), true);
            if n == MAX_DEADLOCK_HOPS {
                assert_eq!(r, Err(FsError::Deadlock), "{n} deep");
            } else {
                assert!(matches!(r, Ok(Outcome::Waiting(_))), "{n} deep: {r:?}");
            }
        }
    }

    #[test]
    fn a_waiter_finds_a_deadlock_once_its_way_changes() {
        let mut locks = Locks::new();
        // P1 waits to write byte 5, which P2 and P3 share.
        locks.lock(FILE, P2, shared(5, 5), false).unwrap();
        locks.lock(FILE, P3, shared(5, 5), false).unwrap();
        locks.lock(FILE, P1, exclusive(9, 9), false).unwrap();
        let w = waiter(locks.lock(FILE, P1, exclusive(5, 5), true));
        // P3 waits for P1, which waits for P2 (the first in its way).
        let p3 = waiter(locks.lock(FILE, P3, exclusive(9, 9), true));
        // P2's lock goes: P1's request, tried again, would wait for P3,
        // which waits for P1, as Linux finds when its waiter wakes.
        locks.lock(FILE, P2, unlock(5, 5), false).unwrap();
        assert_eq!(locks.notices(), [Notice { waiter: w, result: Err(FsError::Deadlock) }]);
        locks.lock(FILE, P1, unlock(9, 9), false).unwrap();
        assert_eq!(locks.notices(), [Notice { waiter: p3, result: Ok(true) }]);
    }

    #[test]
    fn an_owner_that_goes_takes_its_locks_and_waits_with_it() {
        let mut locks = Locks::new();
        locks.lock(FILE, P1, exclusive(0, 9), false).unwrap();
        locks.lock(OTHER_FILE, P1, shared(0, 0), false).unwrap();
        locks.lock(FILE, O1, whole(Some(Kind::Exclusive)), false).unwrap();
        let a = waiter(locks.lock(FILE, P2, shared(0, 0), true));
        let b = waiter(locks.lock(FILE, O2, whole(Some(Kind::Shared)), true));
        let c = waiter(locks.lock(FILE, O1, exclusive(0, 0), true));
        // P1 goes, from both files: P2 is granted, and is in the way of the
        // open file's request now.
        locks.release(P1);
        assert_eq!(locks.notices(), [Notice { waiter: a, result: Ok(true) }]);
        assert!(!locks.holds(FILE, P1) && !locks.holds(OTHER_FILE, P1));
        // The open file goes: its request fails, and its flock lock lets
        // the other's go.
        locks.release(O1);
        assert_eq!(
            locks.notices(),
            [Notice { waiter: c, result: Err(FsError::BadFd) }, Notice { waiter: b, result: Ok(false) }]
        );
        // The file closes: all of its locks go.
        locks.forget(FILE);
        assert!(!locks.holds(FILE, P2));
        assert_eq!(locks.count, 0);
        assert!(locks.files.is_empty());
    }

    #[test]
    fn a_closed_file_fails_its_waiters() {
        let mut locks = Locks::new();
        locks.lock(FILE, P1, exclusive(0, 0), false).unwrap();
        let w = waiter(locks.lock(FILE, P2, exclusive(0, 0), true));
        locks.forget(FILE);
        assert_eq!(locks.notices(), [Notice { waiter: w, result: Err(FsError::BadFd) }]);
        locks.forget(FILE);
        assert!(locks.notices().is_empty());
    }

    #[test]
    fn locks_are_limited() {
        let mut locks = Locks::new();
        // Kinds by turns, which do not merge.
        let kind = |i: u64| if i.is_multiple_of(2) { Kind::Shared } else { Kind::Exclusive };
        for i in 0..MAX_FILE_LOCKS as u64 {
            locks.lock(FILE, P1, bytes(Some(kind(i)), i, i), false).unwrap();
        }
        let next = MAX_FILE_LOCKS as u64;
        assert_eq!(locks.lock(FILE, P1, bytes(Some(kind(next)), next, next), false), Err(FsError::TooMany));
        assert_eq!(locks.lock(FILE, O1, whole(Some(Kind::Shared)), false), Err(FsError::TooMany));
        // What merges or replaces still goes; splitting one would grow.
        assert_eq!(locks.lock(FILE, P1, bytes(Some(kind(next - 1)), next, next), false), Ok(Outcome::Done));
        assert_eq!(locks.lock(FILE, P1, bytes(Some(kind(0)), 1, 1), false), Ok(Outcome::Done));
        assert_eq!(locks.lock(FILE, P1, exclusive(1000, 1000), false).map(|_| ()), Ok(()));
        assert_eq!(locks.lock(FILE, P1, unlock(next, next), false), Ok(Outcome::Done));
        assert_eq!(locks.count, held(&locks, FILE, P1).len());
        // Other files have room.
        assert_eq!(locks.lock(OTHER_FILE, P1, exclusive(0, 0), false), Ok(Outcome::Done));
    }

    #[test]
    fn owners_are_known_by_their_secrets() {
        let mut owners = Owners::new();
        owners.add(7, 1007, [1; LOCK_OWNER_SIZE]);
        owners.add(8, 1008, [2; LOCK_OWNER_SIZE]);
        // Named once: the first secret and id stay.
        owners.add(7, 9999, [3; LOCK_OWNER_SIZE]);
        assert_eq!(owners.secret(7), Some([1; LOCK_OWNER_SIZE]));
        assert_eq!((owners.find(&[1; LOCK_OWNER_SIZE]), owners.koid(7)), (Some(7), Some(1007)));
        assert_eq!(owners.find(&[2; LOCK_OWNER_SIZE]), Some(8));
        // Another secret, a shorter one, or none: no one.
        let mut near = [1; LOCK_OWNER_SIZE];
        near[LOCK_OWNER_SIZE - 1] = 0;
        assert_eq!(owners.find(&near), None);
        assert_eq!(owners.find(&[1; LOCK_OWNER_SIZE - 1]), None);
        assert_eq!(owners.find(&[]), None);
        owners.remove(7);
        assert_eq!((owners.find(&[1; LOCK_OWNER_SIZE]), owners.secret(7), owners.koid(7)), (None, None, None));
    }
}
