// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `/dev/shm`: shared memory objects, the memory POSIX programs share by
//! name (`shm_open`; `sem_open` keeps a semaphore in one).
//!
//! An object is memory with a size, which a program opens as a file of the
//! VFS: what the file reads and writes is the memory, and a mapping of the
//! file maps the memory itself ([`Shm::memory`]), so that every process
//! that maps it shares it. The memory is the service's ([`Memory`], a VMO
//! there); here the objects are named, sized and counted.
//!
//! * **Names.** `/dev/shm` is one directory, of objects alone. An object
//!   may have several names ([`Shm::link`]), as a file on Linux has hard
//!   links: the C library's `sem_open` makes a semaphore under a name of
//!   its own, then links it to the one it is opened by, so that no program
//!   sees it half made.
//! * **Lifetime.** An object lives while it has a name or something has it
//!   open ([`Shm::hold`]), as a file does; the processes that map its
//!   memory keep the memory itself.
//! * **Size.** In bytes, as a file's (`ftruncate`, writes past the end):
//!   its memory grows with it, and never shrinks, so what an object no
//!   longer holds is cleared, and reads as zeros if it grows again. A
//!   mapping may reach past the end, into memory that reads as zeros, as a
//!   memory file's does.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::ops::Bound;

use proto::fs::{DirEntry, FsError, Stat, device, open_flags};

/// Names an object, never again another while the system runs: its inode
/// number.
pub type Id = u64;

/// The directory's inode number; the objects' follow it.
pub const DIR: Id = 1;

/// The most objects at once, and the most names: each object holds memory,
/// and a handle of the service's. Past them, as past its size, there is no
/// room (`NoSpace`), as on a file system out of inodes.
pub const MAX_OBJECTS: usize = 8192;

/// The largest an object grows, in bytes: as large as a file.
pub const MAX_SIZE: u64 = 1 << 31;

/// The most bytes cleared in one write to the memory.
const CLEAR_STEP: usize = 64 * 1024;

/// The memory of an object, which the service provides.
pub trait Memory {
    /// Makes it hold at least `len` bytes (it never shrinks).
    fn reserve(&mut self, len: u64) -> Result<(), FsError>;
    /// Reads `buf.len()` bytes at `offset`, within what it holds.
    fn read(&self, offset: u64, buf: &mut [u8]) -> Result<(), FsError>;
    /// Writes `data` at `offset`, within what it holds.
    fn write(&mut self, offset: u64, data: &[u8]) -> Result<(), FsError>;
}

/// A shared memory object.
struct Object<M> {
    memory: M,
    /// Its size, in bytes: its memory holds at least as much.
    size: u64,
    /// Nanoseconds since the Unix epoch.
    modified: u64,
    /// How many names it has in the directory.
    names: usize,
    /// How many open files have it.
    open: usize,
}

/// The directory of shared memory objects.
pub struct Shm<M> {
    names: BTreeMap<String, Id>,
    objects: BTreeMap<Id, Object<M>>,
    next: Id,
    /// The directory's modification time.
    modified: u64,
    /// The time now, in nanoseconds since the Unix epoch.
    clock: fn() -> u64,
}

impl<M: Memory> Shm<M> {
    /// An empty directory.
    pub fn new(clock: fn() -> u64) -> Shm<M> {
        Shm { names: BTreeMap::new(), objects: BTreeMap::new(), next: DIR + 1, modified: clock(), clock }
    }

    /// What a path inside the directory (its components after `/dev/shm`)
    /// names: the directory itself (`None`), or an object.
    pub fn find(&self, comps: &[&str]) -> Result<Option<Id>, FsError> {
        match comps {
            [] => Ok(None),
            [name] => self.names.get(*name).map(|&id| Some(id)).ok_or(FsError::NotFound),
            [name, ..] if self.names.contains_key(*name) => Err(FsError::NotDir),
            _ => Err(FsError::NotFound),
        }
    }

    /// The name of the object a path names, or would: `IsDir` for the
    /// directory, and what [`Shm::find`] says of a path below an object.
    fn name<'p>(&self, comps: &[&'p str]) -> Result<&'p str, FsError> {
        match comps {
            [] => Err(FsError::IsDir),
            [name] => Ok(name),
            _ => Err(self.find(comps).err().unwrap_or(FsError::NotFound)),
        }
    }

    fn object(&self, id: Id) -> Result<&Object<M>, FsError> {
        self.objects.get(&id).ok_or(FsError::BadFd)
    }

    fn object_mut(&mut self, id: Id) -> Result<&mut Object<M>, FsError> {
        self.objects.get_mut(&id).ok_or(FsError::BadFd)
    }

    /// What there is to know of the directory (`None`) or an object.
    pub fn stat(&self, id: Option<Id>) -> Result<Stat, FsError> {
        let (size, modified, inode) = match id {
            None => (self.names.len() as u64, self.modified, DIR),
            Some(id) => {
                let o = self.object(id)?;
                (o.size, o.modified, id)
            }
        };
        Ok(Stat { size, is_dir: id.is_none(), modified, inode, device: device::SHM, ..Stat::default() })
    }

    /// The directory's entries whose names come after `after` in the order
    /// of their bytes, in that order.
    pub fn entries_after<'a>(&'a self, after: &str) -> impl Iterator<Item = DirEntry> + 'a {
        let names = self.names.range::<str, _>((Bound::Excluded(after), Bound::Unbounded));
        names.filter_map(|(name, &id)| {
            let o = self.objects.get(&id)?;
            Some(DirEntry {
                name: name.clone(),
                is_dir: false,
                size: o.size,
                modified: o.modified,
                inode: id,
                executable: false,
                socket: false,
            })
        })
    }

    /// Finds or makes (`open_flags::CREATE`, with memory from `memory`) the
    /// object at `comps` for opening with `flags`, emptying it if they ask
    /// (`TRUNCATE`). What opens it then [holds](Shm::hold) it.
    pub fn open(
        &mut self,
        comps: &[&str],
        flags: u32,
        memory: impl FnOnce() -> Result<M, FsError>,
    ) -> Result<Id, FsError> {
        let name = self.name(comps)?;
        let id = match self.names.get(name) {
            Some(_) if flags & open_flags::CREATE != 0 && flags & open_flags::EXCLUSIVE != 0 => {
                return Err(FsError::Exists);
            }
            Some(&id) => id,
            None if flags & open_flags::CREATE != 0 => self.create(name, memory()?)?,
            None => return Err(FsError::NotFound),
        };
        if flags & open_flags::TRUNCATE != 0 {
            self.truncate(id, 0)?;
        }
        Ok(id)
    }

    /// Makes an object named `name`, empty, of `memory`.
    fn create(&mut self, name: &str, memory: M) -> Result<Id, FsError> {
        if self.objects.len() >= MAX_OBJECTS || self.names.len() >= MAX_OBJECTS {
            return Err(FsError::NoSpace);
        }
        let id = self.next;
        self.next += 1;
        let now = (self.clock)();
        self.objects.insert(id, Object { memory, size: 0, modified: now, names: 1, open: 0 });
        self.names.insert(name.to_string(), id);
        self.modified = now;
        Ok(id)
    }

    /// Something opened object `id`.
    pub fn hold(&mut self, id: Id) {
        if let Some(o) = self.objects.get_mut(&id) {
            o.open += 1;
        }
    }

    /// Something that had object `id` open closed it: an object with no
    /// name goes with the last.
    pub fn release(&mut self, id: Id) {
        if let Some(o) = self.objects.get_mut(&id) {
            o.open = o.open.saturating_sub(1);
            self.forget_if_unused(id);
        }
    }

    /// Lets object `id` go if it has no name and nothing has it open.
    fn forget_if_unused(&mut self, id: Id) {
        if self.objects.get(&id).is_some_and(|o| o.names == 0 && o.open == 0) {
            self.objects.remove(&id);
        }
    }

    /// Takes away the name `comps`: the object goes with its last name,
    /// once nothing has it open. The directory itself stays (`Denied`).
    pub fn unlink(&mut self, comps: &[&str]) -> Result<(), FsError> {
        let name = match self.name(comps) {
            Err(FsError::IsDir) => return Err(FsError::Denied),
            name => name?,
        };
        let id = self.names.remove(name).ok_or(FsError::NotFound)?;
        if let Some(o) = self.objects.get_mut(&id) {
            o.names -= 1;
        }
        self.forget_if_unused(id);
        self.modified = (self.clock)();
        Ok(())
    }

    /// Gives the object at `from` the name `to` as well (`Exists` if `to`
    /// is taken).
    pub fn link(&mut self, from: &[&str], to: &[&str]) -> Result<(), FsError> {
        let id = self.find(from)?.ok_or(FsError::IsDir)?;
        let name = self.name(to)?;
        if self.names.contains_key(name) {
            return Err(FsError::Exists);
        }
        if self.names.len() >= MAX_OBJECTS {
            return Err(FsError::NoSpace);
        }
        self.names.insert(name.to_string(), id);
        self.object_mut(id)?.names += 1;
        self.modified = (self.clock)();
        Ok(())
    }

    /// Moves the name `from` to `to`. If `to` is taken, the move fails with
    /// `Exists` unless `replace` is set: then `to` names the object `from`
    /// did, and the one it named loses that name. Two names of one object
    /// stay as they are, as POSIX's `rename` has it.
    pub fn rename(&mut self, from: &[&str], to: &[&str], replace: bool) -> Result<(), FsError> {
        // The directory itself stays where it is.
        let id = self.find(from)?.ok_or(FsError::Denied)?;
        let from = self.name(from)?;
        let to = match self.name(to) {
            Err(FsError::IsDir) => return Err(FsError::Denied),
            to => to?,
        };
        match self.names.get(to).copied() {
            Some(old) if old == id => return Ok(()),
            Some(_) if !replace => return Err(FsError::Exists),
            Some(old) => {
                self.names.remove(to);
                if let Some(o) = self.objects.get_mut(&old) {
                    o.names -= 1;
                }
                self.forget_if_unused(old);
            }
            None => {}
        }
        self.names.remove(from);
        self.names.insert(to.to_string(), id);
        self.modified = (self.clock)();
        Ok(())
    }

    /// Reads up to `len` bytes of object `id` at `offset` (none past its
    /// end).
    pub fn read(&self, id: Id, offset: u64, len: usize) -> Result<Vec<u8>, FsError> {
        let o = self.object(id)?;
        let n = o.size.saturating_sub(offset).min(len as u64) as usize;
        let mut buf = vec![0; n];
        if n > 0 {
            o.memory.read(offset, &mut buf)?;
        }
        Ok(buf)
    }

    /// Writes `data` into object `id` at `offset` (at its end with
    /// `append`), growing it as needed; returns where the data went.
    pub fn write(&mut self, id: Id, offset: u64, append: bool, data: &[u8]) -> Result<u64, FsError> {
        let now = (self.clock)();
        let o = self.object_mut(id)?;
        let at = if append { o.size } else { offset };
        let end = at.checked_add(data.len() as u64).filter(|&end| end <= MAX_SIZE).ok_or(FsError::NoSpace)?;
        if !data.is_empty() {
            o.memory.reserve(end)?;
            o.memory.write(at, data)?;
            o.size = o.size.max(end);
        }
        o.modified = now;
        Ok(at)
    }

    /// Makes object `id` `len` bytes long: what it loses is cleared, what
    /// it gains reads as zeros.
    pub fn truncate(&mut self, id: Id, len: u64) -> Result<(), FsError> {
        if len > MAX_SIZE {
            return Err(FsError::NoSpace);
        }
        let now = (self.clock)();
        let o = self.object_mut(id)?;
        if len < o.size {
            let zeros = vec![0u8; (o.size - len).min(CLEAR_STEP as u64) as usize];
            let mut at = len;
            while at < o.size {
                let n = (o.size - at).min(zeros.len() as u64) as usize;
                o.memory.write(at, &zeros[..n])?;
                at += n as u64;
            }
        } else {
            o.memory.reserve(len)?;
        }
        o.size = len;
        o.modified = now;
        Ok(())
    }

    /// The size of object `id`, in bytes.
    pub fn size(&self, id: Id) -> Result<u64, FsError> {
        Ok(self.object(id)?.size)
    }

    /// The memory of object `id`, made to hold at least `len` bytes (past
    /// its size, which stays, it reads as zeros): to map.
    pub fn memory(&mut self, id: Id, len: u64) -> Result<&M, FsError> {
        if len > MAX_SIZE {
            return Err(FsError::NoSpace);
        }
        let o = self.object_mut(id)?;
        o.memory.reserve(len)?;
        Ok(&o.memory)
    }

    /// Sets the modification time of object `id` (nanoseconds since the
    /// Unix epoch; 0: now).
    pub fn set_modified(&mut self, id: Id, modified: u64) -> Result<(), FsError> {
        let now = (self.clock)();
        self.object_mut(id)?.modified = if modified == 0 { now } else { modified };
        Ok(())
    }

    /// The bytes of every object.
    pub fn bytes(&self) -> u64 {
        self.objects.values().map(|o| o.size).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;
    use core::sync::atomic::{AtomicU64, Ordering};

    /// A clock that moves on a nanosecond every time it is read.
    fn clock() -> u64 {
        static NOW: AtomicU64 = AtomicU64::new(1_000);
        NOW.fetch_add(1, Ordering::Relaxed)
    }

    /// Memory in pages, as a VMO holds it, which counts what was written
    /// to it.
    #[derive(Default)]
    struct Pages {
        bytes: Vec<u8>,
        written: usize,
    }

    impl Memory for Pages {
        fn reserve(&mut self, len: u64) -> Result<(), FsError> {
            let len = (len as usize).next_multiple_of(4096);
            if len > self.bytes.len() {
                self.bytes.resize(len, 0);
            }
            Ok(())
        }

        fn read(&self, offset: u64, buf: &mut [u8]) -> Result<(), FsError> {
            let at = offset as usize;
            buf.copy_from_slice(self.bytes.get(at..at + buf.len()).ok_or(FsError::Io)?);
            Ok(())
        }

        fn write(&mut self, offset: u64, data: &[u8]) -> Result<(), FsError> {
            let at = offset as usize;
            self.bytes.get_mut(at..at + data.len()).ok_or(FsError::Io)?.copy_from_slice(data);
            self.written += data.len();
            Ok(())
        }
    }

    const RW: u32 = open_flags::READ | open_flags::WRITE;
    const CREATE: u32 = RW | open_flags::CREATE;

    fn memory() -> Result<Pages, FsError> {
        Ok(Pages::default())
    }

    /// Opens (or makes) `/dev/shm/<name>`, held as an open file holds it.
    fn open(shm: &mut Shm<Pages>, name: &str, flags: u32) -> Result<Id, FsError> {
        let id = shm.open(&[name], flags, memory)?;
        shm.hold(id);
        Ok(id)
    }

    fn names(shm: &Shm<Pages>) -> Vec<String> {
        shm.entries_after("").map(|e| e.name).collect()
    }

    #[test]
    fn objects_are_made_written_and_read() {
        let mut shm = Shm::new(clock);
        assert_eq!(shm.stat(None).unwrap().size, 0);
        let id = open(&mut shm, "data", CREATE).unwrap();
        let st = shm.stat(Some(id)).unwrap();
        assert_eq!((st.size, st.is_dir, st.inode, st.device), (0, false, id, device::SHM));
        assert_eq!(shm.write(id, 0, false, b"hello").unwrap(), 0);
        assert_eq!(shm.write(id, 5, false, b" world").unwrap(), 5);
        assert_eq!(shm.read(id, 0, 100).unwrap(), b"hello world");
        assert_eq!(shm.read(id, 6, 3).unwrap(), b"wor");
        assert_eq!(shm.read(id, 50, 3).unwrap(), b"");
        // Appending writes at the end, whatever the offset.
        assert_eq!(shm.write(id, 0, true, b"!").unwrap(), 11);
        assert_eq!(shm.size(id).unwrap(), 12);
        // The directory lists it, with its size and inode.
        let dir = shm.stat(None).unwrap();
        assert!(dir.is_dir && dir.size == 1 && dir.inode == DIR);
        let e = shm.entries_after("").next().unwrap();
        assert_eq!((e.name.as_str(), e.size, e.inode, e.is_dir), ("data", 12, id, false));
        // Opened again by its name, it is the same object.
        assert_eq!(open(&mut shm, "data", RW), Ok(id));
    }

    #[test]
    fn paths_and_flags_are_as_for_files() {
        let mut shm = Shm::new(clock);
        assert_eq!(shm.open(&["none"], RW, memory), Err(FsError::NotFound));
        let id = open(&mut shm, "x", CREATE | open_flags::EXCLUSIVE).unwrap();
        assert_eq!(shm.open(&["x"], CREATE | open_flags::EXCLUSIVE, memory), Err(FsError::Exists));
        assert_eq!(shm.open(&[], RW, memory), Err(FsError::IsDir));
        // There are no directories in it.
        assert_eq!(shm.open(&["x", "y"], CREATE, memory), Err(FsError::NotDir));
        assert_eq!(shm.open(&["none", "y"], CREATE, memory), Err(FsError::NotFound));
        assert_eq!(shm.find(&["x"]), Ok(Some(id)));
        assert_eq!(shm.find(&[]), Ok(None));
        assert_eq!(shm.find(&["x", "y"]), Err(FsError::NotDir));
        // Truncating on opening empties it.
        shm.write(id, 0, false, b"contents").unwrap();
        open(&mut shm, "x", RW | open_flags::TRUNCATE).unwrap();
        assert_eq!(shm.size(id).unwrap(), 0);
    }

    #[test]
    fn what_an_object_loses_reads_as_zeros() {
        let mut shm = Shm::new(clock);
        let id = open(&mut shm, "x", CREATE).unwrap();
        shm.write(id, 0, false, b"0123456789").unwrap();
        shm.truncate(id, 4).unwrap();
        assert_eq!(shm.read(id, 0, 100).unwrap(), b"0123");
        shm.truncate(id, 8).unwrap();
        assert_eq!(shm.read(id, 0, 100).unwrap(), b"0123\0\0\0\0");
        // Growing it writes nothing.
        let written = shm.objects[&id].memory.written;
        shm.truncate(id, 1 << 20).unwrap();
        assert_eq!(shm.objects[&id].memory.written, written);
        assert_eq!(shm.size(id).unwrap(), 1 << 20);
        assert!(shm.read(id, 8, 1 << 20).unwrap().iter().all(|&b| b == 0));
    }

    #[test]
    fn objects_grow_to_the_largest_file_alone() {
        let mut shm = Shm::new(clock);
        let id = open(&mut shm, "x", CREATE).unwrap();
        assert_eq!(shm.truncate(id, MAX_SIZE + 1), Err(FsError::NoSpace));
        assert_eq!(shm.write(id, MAX_SIZE, false, b"x"), Err(FsError::NoSpace));
        assert_eq!(shm.write(id, u64::MAX, false, b"x"), Err(FsError::NoSpace));
        assert_eq!(shm.size(id).unwrap(), 0);
    }

    #[test]
    fn mapping_reaches_past_the_size() {
        let mut shm = Shm::new(clock);
        let id = open(&mut shm, "sem", CREATE).unwrap();
        shm.write(id, 0, false, &[7; 32]).unwrap();
        // A page mapped, to hold 32 bytes.
        assert_eq!(shm.memory(id, 4096).unwrap().bytes.len(), 4096);
        assert_eq!(shm.size(id).unwrap(), 32);
        assert_eq!(shm.memory(id, 4 << 30).err(), Some(FsError::NoSpace));
    }

    #[test]
    fn objects_live_while_named_or_open() {
        let mut shm = Shm::new(clock);
        let id = open(&mut shm, "x", CREATE).unwrap();
        shm.write(id, 0, false, b"still here").unwrap();
        shm.unlink(&["x"]).unwrap();
        assert_eq!(shm.find(&["x"]), Err(FsError::NotFound));
        assert!(names(&shm).is_empty());
        // Open, it is there for what has it.
        assert_eq!(shm.read(id, 0, 100).unwrap(), b"still here");
        shm.release(id);
        assert_eq!(shm.read(id, 0, 100), Err(FsError::BadFd));
        // A name keeps a closed object.
        let id = open(&mut shm, "y", CREATE).unwrap();
        shm.write(id, 0, false, b"kept").unwrap();
        shm.release(id);
        let again = open(&mut shm, "y", RW).unwrap();
        assert_eq!((again, shm.read(id, 0, 100).unwrap()), (id, b"kept".to_vec()));
        assert_eq!(shm.unlink(&[]), Err(FsError::Denied));
        assert_eq!(shm.unlink(&["none"]), Err(FsError::NotFound));
    }

    #[test]
    fn links_are_names_of_one_object() {
        let mut shm = Shm::new(clock);
        // As sem_open publishes a semaphore: made under a name of its own,
        // linked to its name, which it then keeps alone.
        let id = open(&mut shm, "tmp-1", CREATE | open_flags::EXCLUSIVE).unwrap();
        shm.write(id, 0, false, b"semaphore").unwrap();
        shm.link(&["tmp-1"], &["sem.lock"]).unwrap();
        assert_eq!(shm.find(&["sem.lock"]), Ok(Some(id)));
        assert_eq!(shm.stat(Some(id)).unwrap().inode, id);
        shm.unlink(&["tmp-1"]).unwrap();
        shm.release(id);
        assert_eq!(names(&shm), ["sem.lock"]);
        assert_eq!(shm.read(id, 0, 100).unwrap(), b"semaphore");
        // A name taken is not given again.
        let other = open(&mut shm, "tmp-2", CREATE).unwrap();
        assert_eq!(shm.link(&["tmp-2"], &["sem.lock"]), Err(FsError::Exists));
        assert_eq!(shm.link(&["none"], &["z"]), Err(FsError::NotFound));
        assert_eq!(shm.link(&[], &["z"]), Err(FsError::IsDir));
        assert_eq!(shm.link(&["tmp-2"], &[]), Err(FsError::IsDir));
        // The last name goes, and the object with it.
        shm.unlink(&["sem.lock"]).unwrap();
        assert_eq!(shm.read(id, 0, 1), Err(FsError::BadFd));
        assert_eq!(shm.read(other, 0, 1), Ok(Vec::new()));
    }

    #[test]
    fn renames_move_and_replace_names() {
        let mut shm = Shm::new(clock);
        let a = open(&mut shm, "a", CREATE).unwrap();
        let b = open(&mut shm, "b", CREATE).unwrap();
        shm.release(b);
        assert_eq!(shm.rename(&["a"], &["b"], false), Err(FsError::Exists));
        shm.rename(&["a"], &["c"], false).unwrap();
        assert_eq!(names(&shm), ["b", "c"]);
        // Replacing: `b` names `a`'s object, and its own goes, as nothing
        // has it open.
        shm.rename(&["c"], &["b"], true).unwrap();
        assert_eq!(names(&shm), ["b"]);
        assert_eq!(shm.find(&["b"]), Ok(Some(a)));
        assert_eq!(shm.size(b), Err(FsError::BadFd));
        // Two names of one object stay.
        shm.link(&["b"], &["d"]).unwrap();
        shm.rename(&["b"], &["d"], true).unwrap();
        assert_eq!(names(&shm), ["b", "d"]);
        assert_eq!(shm.rename(&["none"], &["e"], false), Err(FsError::NotFound));
        assert_eq!(shm.rename(&[], &["e"], false), Err(FsError::Denied));
        assert_eq!(shm.rename(&["b"], &[], true), Err(FsError::Denied));
    }

    #[test]
    fn inodes_are_never_given_again() {
        let mut shm = Shm::new(clock);
        let first = open(&mut shm, "x", CREATE).unwrap();
        shm.unlink(&["x"]).unwrap();
        shm.release(first);
        let second = open(&mut shm, "x", CREATE).unwrap();
        assert!(second > first && first > DIR);
    }

    #[test]
    fn objects_are_limited() {
        let mut shm = Shm::new(clock);
        for i in 0..MAX_OBJECTS {
            shm.open(&[format!("o{i}").as_str()], CREATE, memory).unwrap();
        }
        assert_eq!(shm.open(&["one more"], CREATE, memory), Err(FsError::NoSpace));
        assert_eq!(shm.link(&["o0"], &["another name"]), Err(FsError::NoSpace));
        shm.unlink(&["o0"]).unwrap();
        assert!(shm.open(&["one more"], CREATE, memory).is_ok());
    }

    #[test]
    fn times_follow_changes() {
        let mut shm = Shm::new(clock);
        let before = shm.stat(None).unwrap().modified;
        let id = open(&mut shm, "x", CREATE).unwrap();
        let made = shm.stat(None).unwrap().modified;
        assert!(made > before);
        let t = shm.stat(Some(id)).unwrap().modified;
        shm.write(id, 0, false, b"x").unwrap();
        assert!(shm.stat(Some(id)).unwrap().modified > t);
        shm.set_modified(id, 5).unwrap();
        assert_eq!(shm.stat(Some(id)).unwrap().modified, 5);
        shm.unlink(&["x"]).unwrap();
        assert!(shm.stat(None).unwrap().modified > made);
        assert_eq!(shm.bytes(), 1);
    }
}
