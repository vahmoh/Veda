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
//! | `/dev/shm` | shared memory objects (POSIX's `shm_open`, `sem_open`) |
//!
//! A directory is listed a page at a time ([`vfs::Client::read_dir_page`]),
//! as a listing can be larger than a message carries;
//! [`vfs::Client::read_dir`] asks for every page.
//!
//! **Unix domain sockets.** A POSIX program's socket bound to a path is a
//! node of the file system ([`vfs::Client::bind_socket`]), which `stat`
//! shows and `remove` takes away, as a file; one bound to an abstract name
//! (Linux's) is no file, and its name goes with it. The VFS keeps the
//! socket's inbox, a channel whose other end the program holds, and writes
//! on it what comes for the socket ([`socket_event`]): the connections to
//! it, the datagrams sent to it. Programs connect and send by address
//! ([`SocketAddress`]); the bytes of a connection then go between them,
//! over the ends the connection carries, without the VFS.
//!
//! **Shared memory objects.** `/dev/shm` holds memory with names, which
//! programs open as files: what a file connection to one reads and writes
//! is the memory itself, which [`file::Client::memory`] hands out to map,
//! so that every process that maps it shares it (as POSIX programs share
//! memory with `shm_open`, and their semaphores with `sem_open`). Its
//! objects alone are there, each with any number of names
//! ([`vfs::Client::link`]); none is kept on the home disk.
//!
//! **Confined connections.** A connection can give up, for good, all of the
//! file system but a few places ([`vfs::Client::confine`]): a program that
//! handles what it cannot trust (the browser's page processes) keeps only
//! what it reads. A connection that is not confined can lend one a place
//! to read ([`vfs::Client::lend`], [`vfs::Client::borrow`]), and take it
//! back ([`vfs::Client::take_back`]): the browser lends its network
//! process the folder of a page the user opens, while a page it has open
//! needs it.
//!
//! **Locks.** The VFS arbitrates advisory locks between processes, as
//! Linux does ([`file::Client::lock`]): `flock`'s, on a whole file, held by
//! an open file; and locks on bytes, held by a process (POSIX's record
//! locks, `fcntl`'s) or by an open file (Linux's OFD locks), which are in
//! one another's way but not in `flock`'s. A process names itself as an
//! owner by a secret only it has ([`vfs::Client::lock_owner`]); its locks
//! go as it ends, an open file's as its last connection closes.

use alloc::string::String;
use alloc::vec::Vec;
use ipc::{Bytes, IpcError, enumeration, message, protocol};
use rt::object::{Channel, Handle, Vmo};

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
        /// A Unix domain socket is there: it is connected to, not opened.
        IsSocket = 13,
        /// No socket takes connections or datagrams there: nothing is
        /// bound, or what is bound does not listen, or its program closed
        /// it.
        Refused = 14,
        /// The socket there is of another type (a stream socket's
        /// connection to a datagram socket).
        WrongType = 15,
        /// The socket's queue is full, or a lock is in the way of the one
        /// asked for: try again later.
        Busy = 16,
        /// Waiting for the lock asked for would wait for ever: its holder
        /// waits, directly or through others, for a lock the one asking
        /// holds ([`file::Client::lock`]).
        Deadlock = 17,
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
            FsError::IsSocket => "is a socket",
            FsError::Refused => "connection refused",
            FsError::WrongType => "wrong type of socket",
            FsError::Busy => "busy, try again",
            FsError::Deadlock => "waiting for the lock would wait for ever",
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
        /// Modification time: nanoseconds since 1970-01-01 in local time,
        /// as `rt::time::unix_time_ns` gives it and Veda shows times (the
        /// POSIX layer gives programs UTC); 0 = unknown.
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
        /// A Unix domain socket ([`vfs::Client::bind_socket`]).
        pub socket: bool,
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
    /// `/dev/shm`, the shared memory objects.
    pub const SHM: u64 = 4;
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
        /// As [`Stat::socket`].
        pub socket: bool,
    }
}

impl DirEntry {
    /// The bytes the entry takes in a message.
    pub fn encoded_len(&self) -> usize {
        // The name (its length and bytes), `is_dir`, `size`, `modified`,
        // `inode`, `executable` and `socket`.
        4 + self.name.len() + 1 + 8 + 8 + 8 + 1 + 1
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

/// The types of Unix domain sockets (`socket`'s `SOCK_STREAM`,
/// `SOCK_DGRAM`, `SOCK_SEQPACKET`).
pub mod socket_kind {
    /// A connection's byte stream.
    pub const STREAM: u32 = 1;
    /// Datagrams, to and from any socket of the type.
    pub const DATAGRAM: u32 = 2;
    /// A connection's messages.
    pub const SEQPACKET: u32 = 5;
}

/// The largest datagram a Unix domain socket sends or receives, in bytes:
/// with the path it is sent to, and who sent it, it fits a message.
pub const MAX_DATAGRAM: usize = 56 * 1024;

/// The longest name a Unix domain socket is bound to, in bytes: as much as
/// Linux's `sun_path` holds.
pub const MAX_SOCKET_NAME: usize = 108;

message! {
    /// Where a Unix domain socket is bound: a path of the file system, or a
    /// name of the abstract namespace, Linux's (what a `sun_path` that
    /// starts with a NUL names), which is no file, is apart for each type
    /// of socket, and is free again once its socket is closed.
    #[derive(Debug, Clone, PartialEq, Eq, Default)]
    pub struct SocketAddress {
        /// An absolute path; empty for an abstract name.
        pub path: String,
        /// The abstract name, without the NUL that starts it (any bytes;
        /// it may be empty).
        pub abstract_name: Bytes,
    }
}

impl SocketAddress {
    /// The socket bound at `path`.
    pub fn path(path: String) -> SocketAddress {
        SocketAddress { path, abstract_name: Bytes(Vec::new()) }
    }

    /// The socket bound to abstract name `name`.
    pub fn abstract_name(name: Vec<u8>) -> SocketAddress {
        SocketAddress { path: String::new(), abstract_name: Bytes(name) }
    }

    /// Whether it is an abstract name.
    pub fn is_abstract(&self) -> bool {
        self.path.is_empty()
    }
}

message! {
    /// Who is at the other end of a Unix domain socket: what `SO_PEERCRED`
    /// and `getpeername` say of it.
    #[derive(Debug, Clone, PartialEq, Eq, Default)]
    pub struct SocketPeer {
        /// The process that connected, or bound, the socket, or sent the
        /// datagram: its kernel object id, as it says (0: unknown). (The
        /// registry knows a POSIX program by the program that started it,
        /// for which it speaks.)
        pub koid: u64,
        /// The name its socket is bound to, as its program gave it (empty:
        /// none).
        pub name: Bytes,
    }
}

message! {
    /// A place of the file system that a confined connection reaches
    /// ([`vfs::Client::confine`]): what is at or below `path`, to read, or
    /// to change too.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Place {
        /// An absolute path.
        pub path: String,
        /// What is there may be changed too.
        pub write: bool,
    }
}

/// The most places a confined connection's view holds: the VFS looks at
/// each of them for every request of the connection.
pub const MAX_PLACES: usize = 64;

/// The bytes of a loan ([`vfs::Client::lend`]): a secret, which whoever
/// has it may take the loan with, and its lender take it back with.
pub const LOAN_SIZE: usize = 16;

/// The events the VFS writes on a socket's inbox ([`vfs::Client::bind_socket`]),
/// by their ordinals.
pub mod socket_event {
    /// A connection to a listening stream or seqpacket socket
    /// ([`super::SocketConnection`]).
    pub const CONNECTION: u32 = 1;
    /// A datagram to a datagram socket ([`super::SocketDatagram`]).
    pub const DATAGRAM: u32 = 2;
    /// On a seqpacket connection's channel, which the POSIX layer's two
    /// ends speak these events on: the other end sends no more
    /// (`shutdown`); no payload.
    pub const END: u32 = 3;
}

message! {
    /// A connection to a listening socket.
    #[derive(Debug)]
    pub struct SocketConnection {
        /// Who connects.
        pub from: SocketPeer,
        /// The connection's ends, as the program that connects made them
        /// (the POSIX layer's: the other ends are its).
        pub ends: Vec<Handle>,
    }
}

message! {
    /// A datagram, as a socket receives it.
    #[derive(Debug)]
    pub struct SocketDatagram {
        /// Who sent it.
        pub from: SocketPeer,
        pub data: Bytes,
        /// The descriptors sent with it (`SCM_RIGHTS`), as the POSIX layer
        /// says them; the handles they take are `handles`.
        pub rights: Bytes,
        pub handles: Vec<Handle>,
    }
}

/// What a lock is ([`LockRange::kind`]).
pub mod lock_kind {
    /// None: what unlocking asks for.
    pub const NONE: u32 = 0;
    /// Shared (`flock`'s `LOCK_SH`, `fcntl`'s `F_RDLCK`): others may hold
    /// shared locks there too.
    pub const SHARED: u32 = 1;
    /// Exclusive (`LOCK_EX`, `F_WRLCK`): no one else may hold a lock there.
    pub const EXCLUSIVE: u32 = 2;
}

/// The last byte a lock holds at most, the largest offset POSIX has: a
/// lock to it goes on to the end of the file, wherever that comes (POSIX's
/// `l_len` of 0).
pub const LOCK_END: u64 = i64::MAX as u64;

/// The bytes of the secret that names a process as the owner of locks
/// ([`vfs::Client::lock_owner`]).
pub const LOCK_OWNER_SIZE: usize = 16;

message! {
    /// A lock's kind and bytes ([`file::Client::lock`]).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct LockRange {
        /// [`lock_kind`].
        pub kind: u32,
        /// Its first byte and its last, both held: 0 and [`LOCK_END`] for
        /// the whole file, however long it grows.
        pub start: u64,
        pub end: u64,
    }
}

message! {
    /// A lock in another's way ([`file::Client::test_lock`]).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct LockHolder {
        pub lock: LockRange,
        /// The process that holds it, by the kernel object id it gave
        /// ([`vfs::Client::lock_owner`]); none for an open file's.
        pub process: Option<u64>,
    }
}

message! {
    /// What a lock request did ([`file::Client::lock`]).
    #[derive(Debug)]
    pub struct LockOutcome {
        /// The lock waits to be granted: the VFS writes on this channel
        /// once it is, or once it never will be ([`lock_event`]), and
        /// forgets the request as the channel closes. None: done.
        pub waiting: Option<Channel>,
        /// Its owner, a process, holds locks on the file's bytes now: what
        /// it takes away as it closes the file ([`file::Client::lock`]).
        pub held: bool,
    }
}

/// The event the VFS writes on a lock request's channel
/// ([`LockOutcome::waiting`]).
pub mod lock_event {
    /// The lock was granted (`Ok`, saying as [`super::LockOutcome::held`]
    /// does whether its owner holds locks on the file's bytes) or never
    /// will be (`Err`: `Deadlock`; `BadFd` when its open file, or the file
    /// itself, closed first). Payload: `Result<bool, FsError>`.
    pub const DONE: u32 = 1;
}

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
        /// Sets the modification time (as [`Stat::modified`] has it; 0:
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
        /// Binds a Unix domain socket of type `kind` ([`socket_kind`]) to
        /// `at`: makes a socket node at its path, which must not exist yet,
        /// or takes its abstract name, which no open socket of the type may
        /// have (`Exists`). `name` is the name the program gave (what
        /// peers are told of the socket), `koid` the process that binds it
        /// (as [`SocketPeer`] says), `inbox` the channel the VFS writes what
        /// comes for the socket on ([`socket_event`]): datagrams from now
        /// on, and connections once it listens. When the program closes
        /// the other end, nothing takes connections there any more: an
        /// abstract name is free again, a node stays until it is removed,
        /// as on Linux. Returns the socket's id, which only this program's
        /// calls name it by.
        20 => fn bind_socket(at: SocketAddress, kind: u32, name: Bytes, koid: u64, inbox: Channel) -> Result<u64, FsError>;
        /// Socket `id`, this program's stream or seqpacket socket, takes
        /// connections from now on (`listen`).
        21 => fn listen_socket(id: u64) -> Result<(), FsError>;
        /// Connects to the listening socket bound at `to`, of type `kind`:
        /// hands it `ends`, the connection's, and who connects (process
        /// `koid` of this program, from its socket `from`, or 0 for one not
        /// bound). A datagram socket's connection hands nothing: `to`
        /// becomes where it sends. `Busy` while the socket has as many
        /// connections waiting as its inbox holds; `Refused` for an
        /// abstract name no open socket of the type has. Returns who is at
        /// the other end.
        22 => fn connect_socket(to: SocketAddress, kind: u32, from: u64, koid: u64, ends: Vec<Handle>) -> Result<SocketPeer, FsError>;
        /// Sends a datagram to the datagram socket bound at `to`, from
        /// process `koid` of this program, and its socket `from` (0: one
        /// not bound): `data`, and descriptors as [`SocketDatagram`] has
        /// them. `Busy` while its inbox is full.
        23 => fn send_to_socket(to: SocketAddress, from: u64, koid: u64, data: Bytes, rights: Bytes, handles: Vec<Handle>) -> Result<(), FsError>;
        /// Confines this connection, for good, to `view`: from now on it
        /// reaches only what is at or below its places (at most
        /// [`MAX_PLACES`]), each to read or to change too, the innermost
        /// place that holds a path saying which (`/dev` to read and
        /// `/dev/null` to change; `/dev/shm`'s shared memory objects, by
        /// places at or inside it alone); and the directories on the way
        /// to them, which it can look at and list, but which show it only
        /// what leads on to a place. Nothing else is there for it
        /// (`NotFound`), what it may only read it cannot change
        /// (`Denied`), it has no abstract socket names, which are in no
        /// place, and its `sync` returns at once (what it changes is saved
        /// as anything is). As
        /// OpenBSD's `unveil` does, a program gives up the rest before it
        /// handles what it cannot trust (the browser's page processes).
        /// Confining it again narrows the view: one that would reach what
        /// this one's own places do not, or more wholly, is refused
        /// (`Denied`); the places it borrowed ([`vfs::Client::borrow`])
        /// stay, borrowed, and count towards the [`MAX_PLACES`]. What it
        /// has open stays open.
        24 => fn confine(view: Vec<Place>) -> Result<(), FsError>;
        /// Lends the place `path`, a file or a directory there now, to
        /// read: returns the loan, a secret of [`LOAN_SIZE`] random bytes,
        /// which a confined connection takes with
        /// [`vfs::Client::borrow`]. The lender hands it to the one it lends
        /// to (the browser to its network process, over their own channel):
        /// whoever has it may take it, once. Only a connection that is not
        /// confined lends (`Denied`), and only what it reaches itself (not
        /// another service's private directory). A loan not taken lapses
        /// after a minute, or as this connection closes; a connection holds
        /// at most 16 at once (`TooMany`). One taken lasts until this
        /// connection takes it back ([`vfs::Client::take_back`]), or
        /// closes.
        25 => fn lend(path: String) -> Result<Bytes, FsError>;
        /// Takes the loan `loan` of the place `path` (named as it was
        /// lent): from now on this connection reaches what is at or below
        /// `path`, to read, as a place of its view; it reaches nothing less
        /// than before, and changes nothing more. (A connection that is not
        /// confined, or whose own places hold it, reaches it already: the
        /// loan is only taken, with nothing for its lender to take back.)
        /// `Denied` if there is no such loan: another secret or another
        /// place, or one taken or lapsed; `TooMany` if the view holds
        /// [`MAX_PLACES`] places already, the loan left untaken. As a
        /// sandbox extension does on Apple's systems, a loan lets a
        /// sandboxed program reach what the user opened with it, and
        /// nothing else, until its lender takes it back.
        26 => fn borrow(path: String, loan: Bytes) -> Result<(), FsError>;
        /// Takes back the loan `loan` this connection made
        /// ([`vfs::Client::lend`]): one not taken yet can be taken no more,
        /// and the place leaves the view of the confined connection that
        /// took it, which reaches nothing else less, and nothing more; what
        /// it has open there stays open. The loans a connection made are
        /// taken back so as it closes too. That nothing is left to take
        /// back (the loan lapsed, or was taken back already, or the one
        /// that took it reached its place anyway, or closed) is no error;
        /// nor does another's loan, or a guess, take anything back. As the
        /// browser takes back the folder it lent its network process once
        /// no page it has open needs it.
        27 => fn take_back(loan: Bytes) -> Result<(), FsError>;
        /// The secret ([`LOCK_OWNER_SIZE`] random bytes) by which this
        /// connection's process owns locks on files' bytes, as POSIX's
        /// record locks belong to a process ([`file::Client::lock`]): the
        /// same each time. Only those it gives it to have it; its locks
        /// go, and its requests waiting for one with them, as this
        /// connection closes, as a process's do as it ends: a process asks
        /// on the connection that lasts as long as it does. `koid` is its
        /// kernel object id, as it says, which [`file::Client::test_lock`]
        /// tells others of its locks (the first given stays).
        28 => fn lock_owner(koid: u64) -> Result<Bytes, FsError>;
        /// Gives the shared memory object at `from` the name `to` as well,
        /// both in `/dev/shm` (as `link` does on Linux): `Exists` if `to`
        /// is taken. Nothing else has links (`Invalid`). A program
        /// publishes an object so that no other sees it before it is ready,
        /// as the C library's `sem_open` does with a semaphore: made under
        /// a name of its own, then linked to the one it is opened by.
        29 => fn link(from: String, to: String) -> Result<(), FsError>;
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
        /// Sets the modification time (as [`Stat::modified`] has it; 0:
        /// now).
        11 => fn set_modified(modified: u64) -> Result<(), FsError>;
        /// The file's pages from `offset` (page-aligned), `len` bytes
        /// (rounded up to pages), if they are the system image's: to map as
        /// they are, read-only and shared with everyone who maps them, past
        /// the file's end zeros to the end of its last page. `Invalid` for
        /// a file whose bytes are the file system's own, which a mapping
        /// copies, and for a range past its last page.
        12 => fn pages(offset: u64, len: u64) -> Result<Vmo, FsError>;
        /// Locks bytes of the file, or unlocks them (`lock.kind` of
        /// `lock_kind::NONE`), as Linux's advisory locks are, for one of
        /// three owners:
        ///
        /// * `whole`: this open file, with `flock`'s lock on the whole file
        ///   (`lock`'s bytes count for nothing), which locks on bytes are
        ///   not in the way of. One of the other kind that it holds goes
        ///   first, as on Linux, even when the new one is not granted;
        /// * `owner` empty: this open file, with a lock on `lock`'s bytes
        ///   (Linux's OFD locks);
        /// * `owner` a process's secret ([`vfs::Client::lock_owner`]): that
        ///   process, with a lock on `lock`'s bytes, as POSIX's record
        ///   locks are. POSIX has a process's locks on a file go when it
        ///   closes any descriptor of the file: the process unlocks the
        ///   whole file itself as it closes one, as the VFS cannot tell
        ///   which process holds a connection.
        ///
        /// One owner's locks on bytes replace each other where they meet,
        /// and merge with its locks of the same kind beside them. A shared
        /// lock (on a file open to read) is in the way of other owners'
        /// exclusive ones, an exclusive lock (on a file open to write) of
        /// all theirs, `flock`'s in the same way (on a file open either
        /// way). A lock in the way fails the request with `Busy`, or with
        /// `wait` has it wait to be granted ([`LockOutcome::waiting`]). A
        /// process's request that would wait for ever fails with
        /// `Deadlock`, where Linux finds that: its lock's holder waits for
        /// a lock the process holds, or for one of a process that does,
        /// and so on, a few processes deep (an open file's waits are not
        /// followed). `TooMany` when the VFS holds as many locks as it can,
        /// `BadFd` for a lock that the open file's access does not allow.
        13 => fn lock(owner: Bytes, whole: bool, lock: LockRange, wait: bool) -> Result<LockOutcome, FsError>;
        /// The first lock on the file's bytes that is in the way of `lock`,
        /// shared or exclusive, for `owner` (as [`file::Client::lock`] has
        /// it), as POSIX's `F_GETLK` finds it: locks are kept each owner's
        /// together, by their first bytes, owners in the order they first
        /// locked the file. For `lock_kind::NONE` (Linux's `F_OFD_GETLK`
        /// of `F_UNLCK`), the first of `owner`'s own among `lock`'s bytes.
        /// None: no such lock.
        14 => fn test_lock(owner: Bytes, lock: LockRange) -> Result<Option<LockHolder>, FsError>;
        /// The memory of a shared memory object (`/dev/shm`), to map as it
        /// is, shared with every process that maps it: a handle that can
        /// write it if the file was opened to write, its memory made to
        /// hold at least `len` bytes (past the object's size, which stays,
        /// memory that reads as zeros). `Invalid` for other files, whose
        /// bytes are the file system's own.
        15 => fn memory(len: u64) -> Result<Vmo, FsError>;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;
    use alloc::string::ToString;
    use alloc::vec;
    use ipc::{Encode, Encoder};

    fn entry(name: &str) -> DirEntry {
        DirEntry { name: name.into(), is_dir: false, size: 1, modified: 2, inode: 3, executable: false, socket: false }
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
        let mut e = Encoder::with_header(6, 1, ipc::FLAG_RESPONSE);
        Ok::<_, FsError>(DirPage { entries, more: true }).encode(&mut e);
        assert!(e.bytes.len() <= abi::CHANNEL_MAX_BYTES);
    }

    #[test]
    fn the_largest_datagram_fits_a_message() {
        // On its way to the VFS: the longest path, and descriptors.
        let mut e = Encoder::with_header(23, 1, 0);
        SocketAddress::path("/".to_string() + &"p".repeat(MAX_PATH - 1)).encode(&mut e);
        (1u64, 2u64).encode(&mut e);
        (Bytes(vec![0; MAX_DATAGRAM]), Bytes(vec![0; 2048]), Vec::<rt::object::Handle>::new()).encode(&mut e);
        assert!(e.bytes.len() <= abi::CHANNEL_MAX_BYTES, "{}", e.bytes.len());
        // In the socket's inbox: who sent it, with the longest name.
        let mut e = Encoder::with_header(socket_event::DATAGRAM, 0, ipc::FLAG_EVENT);
        let from = SocketPeer { koid: 1, name: Bytes(vec![b'n'; MAX_SOCKET_NAME]) };
        SocketDatagram { from, data: Bytes(vec![0; MAX_DATAGRAM]), rights: Bytes(vec![0; 2048]), handles: vec![] }
            .encode(&mut e);
        assert!(e.bytes.len() <= abi::CHANNEL_MAX_BYTES, "{}", e.bytes.len());
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
