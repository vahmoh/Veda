// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The virtual file system protocol.
//!
//! Paths are absolute, `/`-separated UTF-8, at most [`MAX_PATH`] bytes
//! long. The standard namespace:
//!
//! | Path | Contents |
//! |------|----------|
//! | `/system` | the read-only system image (programs, fonts, assets) |
//! | `/home/user` | the user's documents, pictures and music |
//! | `/tmp` | scratch space |
//! | `/dev` | devices: `null`, `zero`, `full`, `random`, `urandom` |
//!
//! A directory is listed a page at a time ([`vfs::Client::read_dir_page`]),
//! as a listing can be larger than a message carries;
//! [`vfs::Client::read_dir`] asks for every page.

use alloc::string::String;
use alloc::vec::Vec;
use vipc::{Bytes, IpcError, enumeration, message, protocol};
use vrt::object::{Channel, Vmo};

enumeration! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum FsError {
        NotFound = 1,
        NotDir = 2,
        IsDir = 3,
        Exists = 4,
        ReadOnly = 5,
        Invalid = 6,
        NoSpace = 7,
        Io = 8,
        BadFd = 9,
        TooMany = 10,
        NotEmpty = 11,
        /// The path is in another program's private directory.
        Denied = 12,
    }
}

impl core::fmt::Display for FsError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            FsError::NotFound => "no such file or directory",
            FsError::NotDir => "not a directory",
            FsError::IsDir => "is a directory",
            FsError::Exists => "already exists",
            FsError::ReadOnly => "read-only file system",
            FsError::Invalid => "invalid path or argument",
            FsError::NoSpace => "no space left",
            FsError::Io => "I/O error",
            FsError::BadFd => "bad file descriptor",
            FsError::TooMany => "too many open files",
            FsError::NotEmpty => "directory not empty",
            FsError::Denied => "permission denied",
        })
    }
}

message! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct Stat {
        /// Bytes of a file; entries of a directory (as many as listing
        /// it gives).
        pub size: u64,
        pub is_dir: bool,
        pub read_only: bool,
        /// Modification time, nanoseconds since the Unix epoch (0 = unknown).
        pub modified: u64,
        /// Identifies the file within its file system (never reused while
        /// the system runs): the inode number of POSIX programs.
        pub inode: u64,
        /// Identifies the file system the file is in (see [`device`]).
        pub device: u64,
        /// A program: an ELF executable, or a `#!` script. (Veda keeps no
        /// permission bits; this is what makes a file executable.)
        pub executable: bool,
        /// A device such as `/dev/null` rather than a regular file.
        pub char_device: bool,
    }
}

/// The file systems of [`Stat::device`].
pub mod device {
    /// The writable file system: `/home`, `/tmp` and the root.
    pub const RAM: u64 = 1;
    /// `/system`, the read-only system image.
    pub const SYSTEM: u64 = 2;
    /// `/dev`, the devices.
    pub const DEV: u64 = 3;
}

message! {
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct DirEntry {
        pub name: String,
        pub is_dir: bool,
        /// As [`Stat::size`].
        pub size: u64,
        pub modified: u64,
        /// As [`Stat::inode`].
        pub inode: u64,
        /// As [`Stat::executable`].
        pub executable: bool,
    }
}

impl DirEntry {
    /// The bytes the entry takes in a message.
    pub fn encoded_len(&self) -> usize {
        // The name (its length and bytes), `is_dir`, `size`, `modified`,
        // `inode` and `executable`.
        4 + self.name.len() + 1 + 8 + 8 + 8 + 1
    }
}

message! {
    /// A page of a directory listing (see [`vfs::Client::read_dir_page`]).
    #[derive(Debug, Clone, PartialEq, Eq, Default)]
    pub struct DirPage {
        /// Entries in the order of their names' bytes.
        pub entries: Vec<DirEntry>,
        /// More entries follow the last of these.
        pub more: bool,
    }
}

/// The most bytes of entries ([`DirEntry::encoded_len`]) in one page of a
/// listing: half of what a message carries, so that a page always fits,
/// with room for the reply around it. A page holds at least one entry,
/// however long its name.
pub const DIR_PAGE_BYTES: usize = 32 * 1024;

/// The longest path the VFS takes, in bytes (as Linux's `PATH_MAX`): no
/// file or folder is ever further from the root, so that every one can be
/// named, and walked to, by its path.
pub const MAX_PATH: usize = 4096;

message! {
    /// How full the file system holding a path is (see `vfs::space`).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct Space {
        /// Bytes it can hold.
        pub total: u64,
        /// Bytes in use.
        pub used: u64,
        /// Its contents survive a restart (the system image, or a home
        /// directory kept on the home disk).
        pub persistent: bool,
    }
}

/// Flags for `open`.
pub mod open_flags {
    pub const READ: u32 = 1;
    pub const WRITE: u32 = 2;
    pub const CREATE: u32 = 4;
    pub const TRUNCATE: u32 = 8;
    /// Writes go to the end of the file. A modifier: writing takes `WRITE`.
    pub const APPEND: u32 = 16;
    /// With `CREATE`: fail with `Exists` if the file is already there.
    pub const EXCLUSIVE: u32 = 32;
}

/// `whence` of [`file::Client::seek`].
pub mod seek {
    /// From the start of the file.
    pub const SET: u32 = 0;
    /// From the current offset.
    pub const CURRENT: u32 = 1;
    /// From the end of the file.
    pub const END: u32 = 2;
}

/// Largest payload of a single `read`/`write` call.
pub const MAX_IO: u32 = 48 * 1024;

protocol! {
    /// File system service.
    pub mod vfs = "vfs" {
        /// Opens a file; returns a descriptor local to this connection.
        1 => fn open(path: String, flags: u32) -> Result<u32, FsError>;
        2 => fn close(fd: u32) -> Result<(), FsError>;
        /// Reads up to `len` (at most `MAX_IO`) bytes at `offset`.
        3 => fn read(fd: u32, offset: u64, len: u32) -> Result<Bytes, FsError>;
        /// Writes at `offset`; returns the number of bytes written.
        4 => fn write(fd: u32, offset: u64, data: Bytes) -> Result<u32, FsError>;
        5 => fn stat(path: String) -> Result<Stat, FsError>;
        /// A page of the listing of directory `path`: its entries whose
        /// names come after `after` in the order of their bytes (from the
        /// first for `""`), at most `max` of them (0: as many as fit), and
        /// no more than [`DIR_PAGE_BYTES`] of them. The next page is
        /// asked for after the last name of this one.
        6 => fn read_dir_page(path: String, after: String, max: u32) -> Result<DirPage, FsError>;
        7 => fn mkdir(path: String) -> Result<(), FsError>;
        8 => fn remove(path: String) -> Result<(), FsError>;
        9 => fn rename(from: String, to: String) -> Result<(), FsError>;
        /// Returns the whole file in a VMO plus its exact size (fast path
        /// for loading programs, images and music).
        10 => fn read_file(path: String) -> Result<(Vmo, u64), FsError>;
        /// Replaces (or creates) a file with the first `len` bytes of `data`.
        11 => fn write_file(path: String, data: Vmo, len: u64) -> Result<(), FsError>;
        12 => fn truncate(fd: u32, len: u64) -> Result<(), FsError>;
        /// Writes all changes to persistent storage now (before power-off).
        13 => fn sync() -> Result<(), FsError>;
        /// How full the file system holding `path` is. Writes that would
        /// not fit fail with `NoSpace`.
        14 => fn space(path: String) -> Result<Space, FsError>;
        /// Opens a file (`open_flags`) as a connection of its own, which
        /// speaks [`file`] and can be passed to another process. Closing
        /// the channel closes the file. Directories cannot be opened so.
        15 => fn open_file(path: String, flags: u32) -> Result<(Channel, Stat), FsError>;
        /// Renames `from` to `to`, replacing `to` if it exists (a file, or
        /// an empty directory if `from` is a directory), in one step.
        16 => fn replace(from: String, to: String) -> Result<(), FsError>;
        /// Sets the modification time (nanoseconds since the Unix epoch; 0:
        /// now).
        17 => fn set_modified(path: String, modified: u64) -> Result<(), FsError>;
        /// Copies the file `from` to `to`, which must not exist yet, in one
        /// step. The copy shares its contents with the original until
        /// either changes, so it takes neither time nor memory, however
        /// large the file.
        18 => fn copy_file(from: String, to: String) -> Result<(), FsError>;
        /// Gives up, for good, this connection's access to the system
        /// services' private directories (`/home/.private`): from now on
        /// the VFS treats it as it treats an application's. A service
        /// that acts on the user's files for others (the agent) works
        /// through such a connection, so that no file planted by someone
        /// else can lead it into its own private data. What it has open
        /// stays open.
        19 => fn renounce_private() -> Result<(), FsError>;
    }
}

/// Collects a whole listing from `fetch`, which gives the page after a name
/// (as [`vfs::Client::read_dir_page`] does): what [`vfs::Client::read_dir`]
/// does, apart from the connection.
pub fn read_all_pages<E>(
    mut fetch: impl FnMut(&str) -> Result<Result<DirPage, FsError>, E>,
) -> Result<Result<Vec<DirEntry>, FsError>, E> {
    let mut all = Vec::new();
    let mut after = String::new();
    loop {
        let page = match fetch(&after)? {
            Ok(page) => page,
            Err(e) => return Ok(Err(e)),
        };
        // A page that says more follows always holds an entry, the last
        // after the name it was asked for; one that does not ends the
        // listing rather than asks for itself again.
        let Some(last) = page.entries.last().filter(|last| last.name > after) else { return Ok(Ok(all)) };
        after.clone_from(&last.name);
        all.extend(page.entries);
        if !page.more {
            return Ok(Ok(all));
        }
    }
}

impl vfs::Client {
    /// The whole listing of directory `path`, its entries in the order of
    /// their names' bytes, asked for a page at a time
    /// ([`read_dir_page`](vfs::Client::read_dir_page)). Each entry that is
    /// there all along comes exactly once; one added or removed meanwhile
    /// may or may not.
    pub fn read_dir(&self, path: String) -> Result<Result<Vec<DirEntry>, FsError>, IpcError> {
        read_all_pages(|after| self.read_dir_page(path.clone(), after.into(), 0))
    }
}

protocol! {
    /// An open file: a connection made by [`vfs::Client::open_file`]. The
    /// offset and the flags belong to the open file and are shared by every
    /// connection [duplicated](file::Client::duplicate) from it, as POSIX shares
    /// an open file description between processes.
    pub mod file = "file" {
        /// Reads up to `len` (at most `MAX_IO`) bytes at the offset and
        /// moves it past them. Fewer (none at the end) when the file ends.
        1 => fn read(len: u32) -> Result<Bytes, FsError>;
        /// Writes at the offset (at the end of the file in append mode) and
        /// moves it past the data.
        2 => fn write(data: Bytes) -> Result<u32, FsError>;
        /// Reads at `offset`, leaving the offset alone.
        3 => fn read_at(offset: u64, len: u32) -> Result<Bytes, FsError>;
        /// Writes at `offset`, leaving the offset alone.
        4 => fn write_at(offset: u64, data: Bytes) -> Result<u32, FsError>;
        /// Moves the offset (`seek::SET`, `CURRENT` or `END`); returns it.
        5 => fn seek(offset: i64, whence: u32) -> Result<u64, FsError>;
        6 => fn stat() -> Result<Stat, FsError>;
        7 => fn truncate(len: u64) -> Result<(), FsError>;
        /// Another connection to this open file.
        8 => fn duplicate() -> Result<Channel, FsError>;
        /// The `open_flags` it was opened with.
        9 => fn flags() -> u32;
        /// Turns append mode on or off (the one flag that can change).
        10 => fn set_append(append: bool) -> Result<(), FsError>;
        /// Sets the modification time (nanoseconds since the Unix epoch;
        /// 0: now).
        11 => fn set_modified(modified: u64) -> Result<(), FsError>;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;
    use alloc::vec;
    use vipc::{Encode, Encoder};

    fn entry(name: &str) -> DirEntry {
        DirEntry { name: name.into(), is_dir: false, size: 1, modified: 2, inode: 3, executable: false }
    }

    #[test]
    fn entries_know_their_size_in_a_message() {
        for name in ["a", "Notes for Monday.txt", "日本語"] {
            let mut e = Encoder::new();
            entry(name).encode(&mut e);
            assert_eq!(e.bytes.len(), entry(name).encoded_len(), "{name}");
        }
    }

    #[test]
    fn a_full_page_fits_in_a_message() {
        // As many entries as a page holds, each with the longest name, in
        // the reply to a request.
        let long = "x".repeat(255);
        let mut entries = Vec::new();
        let mut bytes = 0;
        while bytes + entry(&long).encoded_len() <= DIR_PAGE_BYTES {
            bytes += entry(&long).encoded_len();
            entries.push(entry(&long));
        }
        let mut e = Encoder::with_header(6, 1, vipc::FLAG_RESPONSE);
        Ok::<_, FsError>(DirPage { entries, more: true }).encode(&mut e);
        assert!(e.bytes.len() <= vabi::CHANNEL_MAX_BYTES);
    }

    /// Pages of `names` (sorted) after a name, `per_page` at a time.
    fn pages(names: &[String], per_page: usize) -> impl FnMut(&str) -> Result<Result<DirPage, FsError>, ()> + '_ {
        move |after| {
            let rest: Vec<&String> = names.iter().filter(|n| n.as_str() > after).collect();
            let entries = rest.iter().take(per_page).map(|n| entry(n)).collect();
            Ok(Ok(DirPage { entries, more: rest.len() > per_page }))
        }
    }

    #[test]
    fn a_listing_is_collected_page_by_page() {
        let mut names: Vec<String> = (0..1000).map(|i| format!("file {i}")).collect();
        names.sort();
        for per_page in [1, 7, 999, 1000, 5000] {
            let all = read_all_pages(pages(&names, per_page)).unwrap().unwrap();
            assert_eq!(all.iter().map(|e| &e.name).collect::<Vec<_>>(), names.iter().collect::<Vec<_>>());
        }
        assert_eq!(read_all_pages(pages(&[], 3)).unwrap().unwrap(), vec![]);
        // Errors of either layer end it.
        assert_eq!(read_all_pages(|_| Ok::<_, ()>(Err(FsError::NotDir))), Ok(Err(FsError::NotDir)));
        assert_eq!(read_all_pages(|_| Err::<Result<DirPage, FsError>, _>(7)), Err(7));
    }

    #[test]
    fn a_listing_that_does_not_move_on_ends() {
        // A page saying more follows, without entries, or with the same
        // ones again, is not asked for for ever.
        let empty = read_all_pages(|_| Ok::<_, ()>(Ok(DirPage { entries: vec![], more: true })));
        assert_eq!(empty, Ok(Ok(vec![])));
        let same = read_all_pages(|_| Ok::<_, ()>(Ok(DirPage { entries: vec![entry("a")], more: true })));
        assert_eq!(same, Ok(Ok(vec![entry("a")])));
    }
}
