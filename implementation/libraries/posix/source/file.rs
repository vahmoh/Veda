// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Files and directories of the VFS behind descriptors.

use alloc::string::String;
use alloc::vec::Vec;

use abi::signals::{PEER_CLOSED, READABLE};
use ipc::Bytes;
use proto::fs::{FsError, LockHolder, LockRange, MAX_IO, Stat, file, lock_event, open_flags, seek};
use rt::object::{Channel, Vmo};
use rt::sync::Mutex;

use crate::error;
use crate::linux::errno::{EINVAL, EIO, ENOLCK, ENOMEM, ERESTARTSYS};
use crate::linux::{self, dt};
use crate::path;
use crate::vfs;

/// An open file: a connection of the VFS's `file` protocol. The VFS keeps
/// the offset, shared with every process the file was passed to.
pub struct VfsFile {
    client: Mutex<file::Client>,
    /// The path it was opened by, if this process opened it (it may have
    /// been renamed since): which file system it is on.
    path: Option<String>,
    /// The file it is, by its file system's number and its inode
    /// (`Stat::device`, `Stat::inode`): as it was opened, or as the VFS
    /// says once asked.
    identity: Mutex<Option<(u64, u64)>>,
}

/// What a request for a lock did ([`VfsFile::request_lock`]).
pub enum Requested {
    /// Done: whether its owner, a process, holds locks on the file's bytes
    /// now.
    Done(bool),
    /// It waits: the VFS says on this channel what becomes of it
    /// ([`await_lock`]).
    Waiting(Channel),
}

impl VfsFile {
    /// A file this process was given (its path is not known).
    pub fn new(channel: Channel) -> VfsFile {
        VfsFile { client: Mutex::new(file::Client::new(channel)), path: None, identity: Mutex::new(None) }
    }

    /// A file this process opened as `path`, which the VFS says `stat` of.
    pub fn opened(channel: Channel, path: String, stat: &Stat) -> VfsFile {
        let identity = Mutex::new(Some((stat.device, stat.inode)));
        VfsFile { client: Mutex::new(file::Client::new(channel)), path: Some(path), identity }
    }

    /// The file it is: its file system's number and its inode, which no
    /// other file has while the system runs.
    pub fn identity(&self) -> Result<(u64, u64), isize> {
        if let Some(identity) = *self.identity.lock() {
            return Ok(identity);
        }
        let st = self.stat()?;
        *self.identity.lock() = Some((st.device, st.inode));
        Ok((st.device, st.inode))
    }

    /// Asks the VFS for a lock on the file, or to take one away
    /// (`file::lock`, which says what `owner`, `whole` and `lock` mean):
    /// with `wait`, a lock in the way has the request wait, which
    /// [`await_lock`] waits for without holding the connection, which other
    /// threads go on using meanwhile. `ENOLCK` when the VFS holds as many
    /// locks, or waits, as it can.
    pub fn request_lock(&self, owner: &[u8], whole: bool, lock: LockRange, wait: bool) -> Result<Requested, isize> {
        let outcome = match self.client.lock().lock(Bytes(owner.to_vec()), whole, lock, wait) {
            Ok(Err(FsError::TooMany)) => return Err(ENOLCK),
            r => error::ipc(r)?,
        };
        Ok(match outcome.waiting {
            Some(channel) => Requested::Waiting(channel),
            None => Requested::Done(outcome.held),
        })
    }

    /// The first lock in the way of `lock` for `owner` (`file::test_lock`).
    pub fn test_lock(&self, owner: &[u8], lock: LockRange) -> Result<Option<LockHolder>, isize> {
        error::ipc(self.client.lock().test_lock(Bytes(owner.to_vec()), lock))
    }

    /// The path the file was opened by, if this process opened it.
    pub fn path(&self) -> Option<&str> {
        self.path.as_deref()
    }

    /// Reads at the offset until `buf` is full or the file ends.
    pub fn read(&self, buf: &mut [u8]) -> Result<usize, isize> {
        let c = self.client.lock();
        read_chunks(buf, |_, want| error::ipc(c.read(want)).map(|Bytes(data)| data))
    }

    /// Writes all of `data` at the offset (at the end in append mode), or
    /// as much as the file system takes.
    pub fn write(&self, data: &[u8]) -> Result<usize, isize> {
        let c = self.client.lock();
        write_chunks(data, |_, chunk| error::ipc(c.write(Bytes(chunk.to_vec()))))
    }

    /// The file's pages from `offset`, `len` bytes, if they are the system
    /// image's (`file::pages`): what a mapping that never writes them can
    /// be, shared with everyone who maps them.
    pub fn pages(&self, offset: u64, len: usize) -> Option<Vmo> {
        self.client.lock().pages(offset, len as u64).ok()?.ok()
    }

    /// The memory of a shared memory object (`/dev/shm`), made to hold
    /// `len` bytes, to map as it is (`file::memory`), with the access the
    /// file was opened with; `None` for another file, whose bytes are the
    /// file system's own.
    pub fn memory(&self, len: u64) -> Result<Option<Vmo>, isize> {
        match self.client.lock().memory(len) {
            Ok(Ok(vmo)) => Ok(Some(vmo)),
            Ok(Err(FsError::Invalid)) => Ok(None),
            // The memory could not grow: as for any mapping.
            Ok(Err(FsError::NoSpace)) => Err(ENOMEM),
            Ok(Err(e)) => Err(error::fs(e)),
            Err(_) => Err(EIO),
        }
    }

    /// Reads at `offset` until `buf` is full or the file ends, leaving the
    /// offset alone.
    pub fn pread(&self, buf: &mut [u8], offset: u64) -> Result<usize, isize> {
        let c = self.client.lock();
        read_chunks(buf, |done, want| error::ipc(c.read_at(offset + done as u64, want)).map(|Bytes(data)| data))
    }

    /// Writes `data` at `offset`, leaving the offset alone.
    pub fn pwrite(&self, data: &[u8], offset: u64) -> Result<usize, isize> {
        let c = self.client.lock();
        write_chunks(data, |done, chunk| error::ipc(c.write_at(offset + done as u64, Bytes(chunk.to_vec()))))
    }

    pub fn seek(&self, offset: i64, whence: u32) -> Result<u64, isize> {
        let whence = match whence {
            linux::seek::SET => seek::SET,
            linux::seek::CUR => seek::CURRENT,
            linux::seek::END => seek::END,
            _ => return Err(EINVAL),
        };
        error::ipc(self.client.lock().seek(offset, whence))
    }

    pub fn stat(&self) -> Result<Stat, isize> {
        error::ipc(self.client.lock().stat())
    }

    pub fn truncate(&self, len: u64) -> Result<(), isize> {
        error::ipc(self.client.lock().truncate(len))
    }

    pub fn set_append(&self, on: bool) -> Result<(), isize> {
        error::ipc(self.client.lock().set_append(on))
    }

    /// Sets the modification time (nanoseconds since the Unix epoch; 0:
    /// now).
    pub fn set_modified(&self, modified: u64) -> Result<(), isize> {
        error::ipc(self.client.lock().set_modified(modified))
    }

    /// The `open_flags` the file was opened with.
    pub fn open_flags(&self) -> Result<u32, isize> {
        self.client.lock().flags().map_err(|_| EIO)
    }

    /// Another connection to this open file (for another process).
    pub fn duplicate(&self) -> Result<Channel, isize> {
        error::ipc(self.client.lock().duplicate())
    }
}

/// Waits for a request for a lock that waits ([`Requested::Waiting`]) to be
/// granted (whether its owner, a process, holds locks on the file's bytes
/// then), or to fail: with `EDEADLK`, `EBADF` if the file closed first, or
/// `ENOLCK` if the VFS had no room for the lock. A signal for the thread
/// ends the wait as it ends other blocking calls (`ERESTARTSYS`: the call
/// is made again after a handler with `SA_RESTART`, as Linux makes
/// `flock` and `F_SETLKW` again): the channel goes, and the VFS forgets
/// the request with it, unless it had granted it already.
pub fn await_lock(waiting: Channel) -> Result<bool, isize> {
    let message = match crate::interrupt::wait_one(&waiting.0, READABLE | PEER_CLOSED, abi::DEADLINE_INFINITE) {
        Ok(_) | Err(abi::Error::Interrupted) => match waiting.read() {
            Ok(message) => message,
            Err(abi::Error::ShouldWait) => return Err(ERESTARTSYS),
            Err(_) => return Err(EIO),
        },
        Err(_) => return Err(EIO),
    };
    match ipc::decode_event::<Result<bool, FsError>>(message) {
        Ok((lock_event::DONE, Err(FsError::TooMany))) => Err(ENOLCK),
        Ok((lock_event::DONE, result)) => result.map_err(error::fs),
        _ => Err(EIO),
    }
}

/// Fills `buf` in reads of at most `MAX_IO` bytes, `read(done, want)`
/// giving those after the first `done`, until it is full or a read comes
/// short (the file ended). A reply longer than asked is an I/O error: the
/// file system cannot be trusted with the rest. An error after some bytes
/// came ends the read with those, as POSIX has it (the next read reports
/// it).
fn read_chunks(buf: &mut [u8], mut read: impl FnMut(usize, u32) -> Result<Vec<u8>, isize>) -> Result<usize, isize> {
    let mut done = 0;
    while done < buf.len() {
        let want = (buf.len() - done).min(MAX_IO as usize);
        let data = match read(done, want as u32) {
            Ok(data) if data.len() <= want => data,
            Ok(_) | Err(_) if done > 0 => break,
            Ok(_) => return Err(EIO),
            Err(e) => return Err(e),
        };
        buf[done..done + data.len()].copy_from_slice(&data);
        done += data.len();
        if data.len() < want {
            break;
        }
    }
    Ok(done)
}

/// Writes `data` in pieces of at most `MAX_IO` bytes, `write(done, piece)`
/// writing those after the first `done` and saying how many it took, until
/// all are written or a write takes fewer than given (the file system is
/// full). A count larger than the piece is an I/O error. An error after
/// some bytes were written ends the write with those, as POSIX has it.
fn write_chunks(data: &[u8], mut write: impl FnMut(usize, &[u8]) -> Result<u32, isize>) -> Result<usize, isize> {
    let mut done = 0;
    for piece in data.chunks(MAX_IO as usize) {
        let n = match write(done, piece) {
            Ok(n) if n as usize <= piece.len() => n as usize,
            Ok(_) | Err(_) if done > 0 => break,
            Ok(_) => return Err(EIO),
            Err(e) => return Err(e),
        };
        done += n;
        if n < piece.len() {
            break;
        }
    }
    Ok(done)
}

/// The POSIX access mode for VFS open flags.
pub fn access_mode(flags: u32) -> u32 {
    match (flags & open_flags::READ != 0, flags & open_flags::WRITE != 0) {
        (true, true) => linux::o::RDWR,
        (false, true) => linux::o::WRONLY,
        _ => linux::o::RDONLY,
    }
}

/// An entry of a directory listing.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    inode: u64,
    kind: u8,
    name: String,
}

/// An open directory, read with `getdents64`. The VFS lists it a page at a
/// time as it is read. Its entries are numbered from 0 (`.`, `..`, then the
/// VFS's, in the order of their names' bytes); a position, the `d_off` of
/// the entry before it, is the number of the next entry to read.
pub struct Directory {
    pub path: String,
    state: Mutex<Listing>,
}

/// Where the reading of a directory has got.
#[derive(Default)]
struct Listing {
    /// The entries of the page read now; the first is number `first`.
    entries: Vec<Entry>,
    first: usize,
    /// The number of the next entry to read.
    pos: usize,
    /// The name the next page starts after, if there is a next page.
    next: Option<String>,
    /// The listing has started: `.` and `..` were the first page.
    started: bool,
}

impl Listing {
    /// Makes entry number `pos` one of `entries`, `start` giving `.` and
    /// `..` and `fetch` the page after a name (its entries, and whether
    /// more follow): pages are fetched on from the one read now, or from
    /// the start to go back. False past the last entry.
    fn reach(
        &mut self,
        start: impl FnOnce() -> Result<[Entry; 2], isize>,
        mut fetch: impl FnMut(&str) -> Result<(Vec<Entry>, bool), isize>,
    ) -> Result<bool, isize> {
        if !self.started || self.pos < self.first {
            self.entries = Vec::from(start()?);
            self.first = 0;
            self.next = Some(String::new());
            self.started = true;
        }
        while self.pos >= self.first + self.entries.len() {
            let Some(after) = &self.next else { return Ok(false) };
            let (page, more) = fetch(after)?;
            // A page that does not move on ends the listing.
            self.next = page.last().map(|e| e.name.clone()).filter(|last| more && last > after);
            self.first += self.entries.len();
            self.entries = page;
        }
        Ok(true)
    }
}

impl Directory {
    pub fn new(path: String) -> Directory {
        Directory { path, state: Mutex::new(Listing::default()) }
    }

    /// `.` and `..`.
    fn dots(&self) -> Result<[Entry; 2], isize> {
        let here = vfs::stat(&self.path)?.inode;
        let up = vfs::stat(path::parent(&self.path)).map(|s| s.inode).unwrap_or(here);
        Ok([
            Entry { inode: here, kind: dt::DIR, name: ".".into() },
            Entry { inode: up, kind: dt::DIR, name: "..".into() },
        ])
    }

    /// The VFS's page of entries after `after`, and whether more follow.
    fn page(&self, after: &str) -> Result<(Vec<Entry>, bool), isize> {
        let page = error::ipc(vfs::with(|c| c.read_dir_page(self.path.clone(), after.into(), 0))?)?;
        let entries = page
            .entries
            .into_iter()
            .map(|e| {
                let kind = if e.is_dir {
                    dt::DIR
                } else if e.socket {
                    dt::SOCK
                } else if self.path == "/dev" {
                    dt::CHR
                } else {
                    dt::REG
                };
                Entry { inode: e.inode.max(1), kind, name: e.name }
            })
            .collect();
        Ok((entries, page.more))
    }

    /// Fills `buf` with `struct linux_dirent64` records; 0 at the end.
    pub fn getdents64(&self, buf: &mut [u8]) -> Result<usize, isize> {
        let mut st = self.state.lock();
        let mut used = 0;
        loop {
            match st.reach(|| self.dots(), |after| self.page(after)) {
                Ok(true) => {}
                Ok(false) => break,
                // What was read comes first; the failure comes again with
                // the next call.
                Err(_) if used > 0 => break,
                Err(e) => return Err(e),
            }
            let Some(e) = st.entries.get(st.pos - st.first) else { break };
            let len = record_len(e.name.len());
            if used + len > buf.len() {
                if used == 0 {
                    return Err(EINVAL);
                }
                break;
            }
            let r = &mut buf[used..used + len];
            r[..8].copy_from_slice(&e.inode.to_le_bytes());
            r[8..16].copy_from_slice(&((st.pos + 1) as i64).to_le_bytes());
            r[16..18].copy_from_slice(&(len as u16).to_le_bytes());
            r[18] = e.kind;
            r[19..19 + e.name.len()].copy_from_slice(e.name.as_bytes());
            r[19 + e.name.len()..].fill(0);
            used += len;
            st.pos += 1;
        }
        Ok(used)
    }

    /// `lseek` on a directory: positions are the `d_off` cookies; going
    /// back to the start lists the directory afresh.
    pub fn seek(&self, offset: i64, whence: u32) -> Result<u64, isize> {
        let mut st = self.state.lock();
        let new = match whence {
            linux::seek::SET => offset,
            linux::seek::CUR => st.pos as i64 + offset,
            _ => return Err(EINVAL),
        };
        let new = usize::try_from(new).map_err(|_| EINVAL)?;
        if new == 0 {
            st.started = false;
        }
        st.pos = new;
        Ok(new as u64)
    }
}

/// The size of a `linux_dirent64` record with a name of `len` bytes.
fn record_len(len: usize) -> usize {
    (19 + len + 1).next_multiple_of(8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_sizes() {
        assert_eq!(record_len(1), 24);
        assert_eq!(record_len(4), 24);
        assert_eq!(record_len(5), 32);
        assert_eq!(record_len(255), 280);
    }

    fn entry(name: &str) -> Entry {
        Entry { inode: 2, kind: dt::REG, name: name.into() }
    }

    /// Reads entries from `l` until the end; the names, and how many pages
    /// were fetched.
    fn read_all(l: &mut Listing, names: &[String], pages: &mut usize) -> Vec<String> {
        let mut read = Vec::new();
        while l.reach(dots, |after| fetch(names, after, pages)).unwrap() {
            read.push(l.entries[l.pos - l.first].name.clone());
            l.pos += 1;
        }
        read
    }

    fn dots() -> Result<[Entry; 2], isize> {
        Ok([entry("."), entry("..")])
    }

    /// Pages of three of `names`.
    fn fetch(names: &[String], after: &str, pages: &mut usize) -> Result<(Vec<Entry>, bool), isize> {
        *pages += 1;
        let rest: Vec<&String> = names.iter().filter(|n| n.as_str() > after).collect();
        Ok((rest.iter().take(3).map(|n| entry(n)).collect(), rest.len() > 3))
    }

    #[test]
    fn directories_are_read_a_page_at_a_time() {
        let names: Vec<String> = (0..10).map(|i| alloc::format!("f{i}")).collect();
        let mut l = Listing::default();
        let mut pages = 0;
        let read = read_all(&mut l, &names, &mut pages);
        let mut expected = alloc::vec![String::from("."), String::from("..")];
        expected.extend(names.iter().cloned());
        assert_eq!(read, expected);
        assert_eq!(pages, 4);
        // A page at a time is held.
        assert!(l.entries.len() <= 3);
        // Going back lists afresh up to where it goes; going on skips.
        for (pos, name) in [(4, "f2"), (11, "f9"), (0, "."), (7, "f5")] {
            l.pos = pos;
            assert!(l.reach(dots, |after| fetch(&names, after, &mut pages)).unwrap());
            assert_eq!(l.entries[l.pos - l.first].name, name, "{pos}");
        }
        l.pos = 12;
        assert!(!l.reach(dots, |after| fetch(&names, after, &mut pages)).unwrap());
        // An empty directory has `.` and `..`.
        let mut l = Listing::default();
        assert_eq!(read_all(&mut l, &[], &mut pages), [".", ".."]);
    }

    #[test]
    fn failures_leave_the_listing_where_it_was() {
        let names: Vec<String> = (0..5).map(|i| alloc::format!("f{i}")).collect();
        let mut l = Listing::default();
        let mut pages = 0;
        l.pos = 3;
        assert!(l.reach(dots, |after| fetch(&names, after, &mut pages)).unwrap());
        l.pos = 5;
        assert_eq!(l.reach(dots, |_| Err(EINVAL)), Err(EINVAL));
        assert!(l.reach(dots, |after| fetch(&names, after, &mut pages)).unwrap());
        assert_eq!(l.entries[l.pos - l.first].name, "f3");
    }

    #[test]
    fn access_modes() {
        assert_eq!(access_mode(open_flags::READ), linux::o::RDONLY);
        assert_eq!(access_mode(open_flags::WRITE | open_flags::CREATE), linux::o::WRONLY);
        assert_eq!(access_mode(open_flags::WRITE | open_flags::APPEND), linux::o::WRONLY);
        // Appending is not writing: O_RDONLY | O_APPEND reads only.
        assert_eq!(access_mode(open_flags::READ | open_flags::APPEND), linux::o::RDONLY);
    }

    #[test]
    fn reads_trust_no_reply_length() {
        let file: Vec<u8> = (0..200_000u32).map(|i| i as u8).collect();
        // As a file system that keeps to the protocol.
        let mut buf = alloc::vec![0u8; 150_000];
        let n = read_chunks(&mut buf, |done, want| Ok(file[done..done + want as usize].to_vec())).unwrap();
        assert_eq!((n, &buf[..]), (150_000, &file[..150_000]));
        // The end of the file comes in the middle of a read.
        let n = read_chunks(&mut buf, |done, want| {
            Ok(file[done + 100_000..][..(want as usize).min(100_000 - done)].to_vec())
        });
        assert_eq!(n, Ok(100_000));
        // More than asked: refused, or the end of what came before.
        assert_eq!(read_chunks(&mut buf, |_, want| Ok(alloc::vec![1; want as usize + 1])), Err(EIO));
        let mut calls = 0;
        let n = read_chunks(&mut buf, |_, want| {
            calls += 1;
            Ok(alloc::vec![2; want as usize + if calls == 2 { 7 } else { 0 }])
        });
        assert_eq!(n, Ok(MAX_IO as usize));
        // An error after some bytes ends the read with them.
        let mut calls = 0;
        let n = read_chunks(&mut buf, |_, want| {
            calls += 1;
            if calls == 1 { Ok(alloc::vec![3; want as usize]) } else { Err(EIO) }
        });
        assert_eq!(n, Ok(MAX_IO as usize));
        assert_eq!(read_chunks(&mut buf, |_, _| Err(EINVAL)), Err(EINVAL));
    }

    #[test]
    fn writes_count_what_was_written() {
        let data = alloc::vec![5u8; 3 * MAX_IO as usize + 10];
        let mut written = Vec::new();
        let n = write_chunks(&data, |done, piece| {
            assert_eq!(done, written.len());
            written.extend_from_slice(piece);
            Ok(piece.len() as u32)
        });
        assert_eq!((n, written.len()), (Ok(data.len()), data.len()));
        // The file system fills up in the second piece.
        let n = write_chunks(&data, |done, piece| Ok(if done == 0 { piece.len() as u32 } else { 100 }));
        assert_eq!(n, Ok(MAX_IO as usize + 100));
        // It fails in the third: what went before counts.
        let n = write_chunks(
            &data,
            |done, piece| if done < 2 * MAX_IO as usize { Ok(piece.len() as u32) } else { Err(28) },
        );
        assert_eq!(n, Ok(2 * MAX_IO as usize));
        assert_eq!(write_chunks(&data, |_, _| Err(28)), Err(28));
        // A count larger than the piece.
        assert_eq!(write_chunks(&data, |_, piece| Ok(piece.len() as u32 + 1)), Err(EIO));
        // Nothing to write makes no call.
        assert_eq!(write_chunks(&[], |_, _| panic!("a call for nothing")), Ok(0));
    }
}
