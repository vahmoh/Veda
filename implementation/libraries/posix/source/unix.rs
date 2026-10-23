// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Unix domain sockets (`AF_UNIX`): stream, datagram and seqpacket
//! sockets, named by paths of the file system, and socket pairs.
//!
//! A socket bound to a path is a node of the VFS (`vfs::bind_socket`); one
//! bound to an abstract name (Linux's: a `sun_path` that starts with a NUL)
//! has the name in the VFS's abstract namespace while it is open. The VFS
//! keeps the socket's inbox, a channel whose other end the socket holds,
//! and writes on it the connections to a listening socket and the
//! datagrams sent to a datagram socket (`proto::fs::socket_event`).
//! A connection is made by the socket that connects: a kernel socket for a
//! stream's bytes (as a pipe's), or a channel for a seqpacket connection's
//! messages. It keeps one end and hands the other to the listener through
//! the VFS; from then on the two programs talk directly, the VFS out of the
//! way. A socket pair is made the same way, without the VFS.
//!
//! **Descriptors** (`SCM_RIGHTS`, see [`crate::carry`]) go with a datagram,
//! or a seqpacket message, in its channel message; on a stream, as handles
//! with its bytes, which the kernel's sockets carry as Linux's do: with the
//! first byte sent with them, a read taking one write's at most and ending
//! before the next's. Whoever holds the socket, in this process or another,
//! sends and receives them.
//!
//! **Sockets travel** to the processes a program starts and over sockets
//! ([`Unix::travelling`]): a socket's channels are made shared
//! (`rt::object::Channel::create_shared`), so that a copy of the handle
//! that carries it — its connection's kernel socket or channel, else its
//! inbox — goes, and what the handle does not say (its names, its peer, its
//! options) with it. Several processes then hold one socket, as on Linux:
//! a listening socket's connections, and a datagram socket's datagrams, go
//! to whichever of them takes them first (`accept`, a read), and the socket
//! is closed once all of them have closed it.
//!
//! A datagram or seqpacket message looked at without being taken
//! (`MSG_PEEK`, `FIONREAD`) is held here, in front of what the channel
//! still has; a stream's bytes are looked at in the kernel's socket.
//!
//! Compared with Linux:
//!
//! * abstract names are Linux's: apart for each type of socket, free again
//!   once their socket is closed; binding the family alone, or connecting
//!   or sending datagrams from a socket not bound that says who sends
//!   (`SO_PASSCRED`), takes a free one of a NUL and five hex digits;
//! * a datagram is at most `proto::fs::MAX_DATAGRAM` bytes (`EMSGSIZE`),
//!   and a datagram socket connected to another also receives from others;
//! * a connection waiting to be accepted, and a datagram, wait in the
//!   listener's or receiver's inbox, which holds a few hundred: the
//!   backlog `listen` asks for is not counted;
//! * `SO_PEERCRED` and `SCM_CREDENTIALS` say the process that connected,
//!   or bound, the other end, or sent a message, as that process says (the
//!   registry vouches only for the program that started it; every POSIX
//!   program is one user);
//! * a socket's options, and its being shut down, go with it as they are
//!   when it goes: changed afterwards, they change in one process only. A
//!   message looked at (`MSG_PEEK`) stays with the process that looked, and
//!   a connected socket's copy does not keep the name it is bound to (what
//!   accepts connections or receives datagrams there).

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use abi::signals::{PEER_CLOSED, PEER_WRITE_DISABLED, READABLE, WRITABLE};
use abi::{Error as KError, ObjectType};
use ipc::{Bytes, Decode, IpcError};
use proto::fs::{
    FsError, MAX_DATAGRAM, MAX_SOCKET_NAME, SocketAddress, SocketConnection, SocketDatagram, SocketPeer, socket_event,
    socket_kind,
};
use rt::object::{Channel, Handle, Socket};
use rt::sync::{Mutex, MutexGuard};

use crate::carry::Rights;
use crate::error::{self, SysResult};
use crate::fd::Description;
use crate::inet::Recv;
use crate::io::Waitable;
use crate::linux::errno::*;
use crate::linux::sock::{AF_UNIX, AF_UNSPEC, DGRAM, SEQPACKET, STREAM};
use crate::linux::{SockaddrUn, Ucred, at};
use crate::stream::Stream;
use crate::{process, signal, time, user, vfs};

/// The longest a blocked send waits before it asks again, while the socket
/// it sends to has a full inbox.
const BUSY_WAIT_MAX_NS: u64 = 16_000_000;

/// What kind of socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A connection's byte stream (`SOCK_STREAM`).
    Stream,
    /// Datagrams (`SOCK_DGRAM`).
    Datagram,
    /// A connection's messages (`SOCK_SEQPACKET`).
    SeqPacket,
}

impl Kind {
    /// The kind of a `socket` type.
    pub fn of(sock_type: u32) -> Option<Kind> {
        match sock_type {
            STREAM => Some(Kind::Stream),
            DGRAM => Some(Kind::Datagram),
            SEQPACKET => Some(Kind::SeqPacket),
            _ => None,
        }
    }

    /// Its `socket` type.
    pub fn sock_type(self) -> u32 {
        match self {
            Kind::Stream => STREAM,
            Kind::Datagram => DGRAM,
            Kind::SeqPacket => SEQPACKET,
        }
    }

    /// The VFS's name for it.
    fn vfs(self) -> u32 {
        match self {
            Kind::Stream => socket_kind::STREAM,
            Kind::Datagram => socket_kind::DATAGRAM,
            Kind::SeqPacket => socket_kind::SEQPACKET,
        }
    }
}

/// What a program names with a `struct sockaddr_un`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Addr {
    /// A path: its bytes, without the NUL.
    Path(Vec<u8>),
    /// A name of the abstract namespace: the bytes after the NUL that
    /// starts `sun_path`, as many as the address's length covers (NULs
    /// among them).
    Abstract(Vec<u8>),
    /// The family alone: no name, which `bind` takes as any free abstract
    /// name (autobind).
    Unnamed,
    /// `AF_UNSPEC`: no address (which dissolves a datagram socket's
    /// association with its peer).
    Unspec,
}

impl Addr {
    /// The name a socket bound to it has, as `getsockname` says it: a
    /// path's bytes, or an abstract name after its NUL.
    pub fn name(&self) -> Vec<u8> {
        match self {
            Addr::Path(path) => path.clone(),
            Addr::Abstract(name) => core::iter::once(0).chain(name.iter().copied()).collect(),
            Addr::Unnamed | Addr::Unspec => Vec::new(),
        }
    }
}

/// Reads the address a program passed (`len` bytes at `addr`).
///
/// # Safety
/// `addr` must be null or point at `len` readable bytes.
pub unsafe fn read_addr(addr: usize, len: usize) -> Result<Addr, isize> {
    if addr == 0 {
        return Err(EFAULT);
    }
    if len < size_of::<u16>() || len > size_of::<SockaddrUn>() {
        return Err(EINVAL);
    }
    // SAFETY: per the caller, at least the family is there.
    match unsafe { user::read::<u16>(addr)? } {
        AF_UNIX => {}
        AF_UNSPEC => return Ok(Addr::Unspec),
        _ => return Err(EINVAL),
    }
    if len == size_of::<u16>() {
        return Ok(Addr::Unnamed);
    }
    // SAFETY: per the caller.
    let sun_path = unsafe { user::slice(addr + size_of::<u16>(), len - size_of::<u16>())? };
    if sun_path[0] == 0 {
        return Ok(Addr::Abstract(sun_path[1..].to_vec()));
    }
    let path = &sun_path[..sun_path.iter().position(|&b| b == 0).unwrap_or(sun_path.len())];
    Ok(Addr::Path(path.to_vec()))
}

/// Stores the name of a socket for the program (as [`Addr::name`] has it),
/// as `getsockname`, `accept` and `recvfrom` do: at `addr`, which has room
/// for as many bytes as the `socklen_t` at `addrlen` says, as much of the
/// `struct sockaddr_un` as fits; then its whole length at `addrlen` (a
/// path's with its NUL, an abstract name's with nothing after it, just the
/// family for a socket without a name). Nothing when `addr` is null.
///
/// # Safety
/// `addrlen` must be null or the program's `socklen_t`, and `addr` null or
/// a buffer of that many bytes.
pub unsafe fn store_name(name: &[u8], addr: usize, addrlen: usize) -> Result<(), isize> {
    if addr == 0 {
        return Ok(());
    }
    let mut raw = SockaddrUn { family: AF_UNIX, path: [0; 108] };
    let n = name.len().min(raw.path.len());
    raw.path[..n].copy_from_slice(&name[..n]);
    let len = match name.first() {
        None => size_of::<u16>(),
        Some(0) => size_of::<u16>() + n,
        Some(_) => (size_of::<u16>() + n + 1).min(size_of::<SockaddrUn>()),
    };
    // SAFETY: per the caller.
    let room: u32 = unsafe { user::read(addrlen)? };
    // SAFETY: `raw` is plain old data of its size.
    let bytes = unsafe { core::slice::from_raw_parts(&raw as *const SockaddrUn as *const u8, size_of::<SockaddrUn>()) };
    let k = len.min(room as usize);
    // SAFETY: per the caller, `room` bytes are there.
    unsafe { user::slice_mut(addr, k)? }.copy_from_slice(&bytes[..k]);
    // SAFETY: per the caller.
    unsafe { user::write(addrlen, len as u32) }
}

/// Where the VFS has the socket an address names: an absolute path
/// (relative names are the working directory's), or an abstract name.
fn address_of(addr: &Addr) -> Result<SocketAddress, isize> {
    match addr {
        Addr::Path(name) => Ok(SocketAddress::path(vfs::resolve_at(at::FDCWD, name)?.path)),
        Addr::Abstract(name) => Ok(SocketAddress::abstract_name(name.clone())),
        Addr::Unnamed | Addr::Unspec => Err(EINVAL),
    }
}

/// The names autobinding takes from: Linux's, a NUL and five hex digits.
const AUTOBIND_NAMES: u32 = 1 << 20;

/// A deadline `timeout` (ns) from now; none for none.
fn deadline(timeout: Option<u64>) -> u64 {
    timeout.map_or(abi::DEADLINE_INFINITE, |t| time::monotonic_ns().saturating_add(t))
}

/// Waits on `handle` for `signals` until `deadline`: `EAGAIN` when it
/// passes (what a socket's time-outs make a call fail with, as on Linux).
fn wait(handle: &Handle, signals: u32, deadline: u64) -> Result<u32, isize> {
    match handle.wait(signals, deadline) {
        Ok(s) => Ok(s),
        Err(KError::TimedOut) => Err(EAGAIN),
        Err(e) => Err(error::kernel(e)),
    }
}

/// The `errno` for a send of descriptors the kernel refused.
fn rights_errno(e: KError) -> isize {
    match e {
        KError::PeerClosed | KError::BadState => EPIPE,
        // The other end has not taken those sent before, or a descriptor
        // would reach the socket or channel it goes on, through what is on
        // its way (a socket sent over itself).
        KError::LimitReached | KError::NotSupported => ETOOMANYREFS,
        e => error::kernel(e),
    }
}

/// Who is at the other end of a connection or a socket pair.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Peer {
    /// The name its socket is bound to (empty: none).
    pub name: Vec<u8>,
    /// The process that connected, or bound, it (`SO_PEERCRED`).
    pub pid: i32,
}

impl Peer {
    fn of(p: &SocketPeer) -> Peer {
        Peer { name: p.name.0.clone(), pid: pid_of(p.koid) }
    }

    /// This process, from a socket bound to `name`.
    fn me(name: &[u8]) -> SocketPeer {
        SocketPeer { koid: process::koid(), name: Bytes(name.to_vec()) }
    }
}

/// The process id of a process's kernel object id (0 if unknown).
fn pid_of(koid: u64) -> i32 {
    if koid == 0 { 0 } else { process::pid_of(koid) }
}

/// A connected stream socket's connection: a kernel socket's endpoint,
/// whose bytes go as a pipe's, and descriptors with them.
pub struct StreamConn {
    pub stream: Stream,
    pub peer: Peer,
}

impl StreamConn {
    fn new(socket: Socket, peer: Peer) -> StreamConn {
        StreamConn { stream: Stream::new(socket), peer }
    }

    /// The endpoint's signals now.
    fn signals(&self) -> u32 {
        self.stream.socket.wait(READABLE | WRITABLE | PEER_CLOSED | PEER_WRITE_DISABLED, 0).unwrap_or(0)
    }

    /// Receives into `buf` what the endpoint has (waiting for something
    /// unless `nonblock`; for all of it with `waitall`) until `deadline`,
    /// and the descriptors that came with those bytes: a read ends with
    /// them. With `peek`, what it returns stays to be read again, and its
    /// descriptors with it. 0 at the end of the stream, or once this end
    /// reads no more (`shut_read`) and nothing is left.
    fn recv(
        &self,
        buf: &mut [u8],
        how: Recv,
        deadline: u64,
        shut_read: bool,
    ) -> Result<(usize, Option<Rights>), isize> {
        if buf.is_empty() {
            return Ok((0, None));
        }
        let socket = &self.stream.socket;
        let mut got = 0;
        loop {
            let mut ended = false;
            let read = if how.peek {
                socket.peek(&mut buf[got..]).map(|n| (n, Vec::new()))
            } else {
                socket.read_with(&mut buf[got..], abi::CHANNEL_MAX_HANDLES).map(|(n, handles, _)| (n, handles))
            };
            match read {
                Ok((n, handles)) => {
                    got += n;
                    if !handles.is_empty() {
                        return Ok((got, Rights::from_handles(handles)));
                    }
                }
                Err(KError::PeerClosed) => ended = true,
                Err(KError::ShouldWait) => {}
                Err(_) if got > 0 => return Ok((got, None)),
                Err(e) => return Err(error::kernel(e)),
            }
            if got == buf.len() || (got > 0 && (!how.waitall || how.peek)) || ended || shut_read {
                return Ok((got, None));
            }
            if how.nonblock {
                return if got > 0 { Ok((got, None)) } else { Err(EAGAIN) };
            }
            match wait(&socket.0, READABLE | PEER_CLOSED | PEER_WRITE_DISABLED, deadline) {
                Ok(_) => {}
                Err(_) if got > 0 => return Ok((got, None)),
                Err(e) => return Err(e),
            }
        }
    }

    /// Sends `data`, all of it (waiting for room until `deadline` unless
    /// `nonblock`: then what fits), and the descriptors `fds` with its
    /// first byte. Descriptors go with bytes: without, they do not go (as
    /// on Linux).
    fn send(&self, data: &[u8], fds: &[Arc<Description>], nonblock: bool, deadline: u64) -> SysResult {
        if fds.is_empty() || data.is_empty() {
            return self.stream.send_until(data, nonblock, false, deadline);
        }
        let socket = &self.stream.socket;
        // The first piece, with the descriptors, carried afresh each time:
        // a write with no room closed what carried them.
        let first = loop {
            match socket.write_with(data, Rights::of(fds)?.into_handles()?) {
                Ok(n) => break n,
                Err(KError::ShouldWait) if nonblock => return Err(EAGAIN),
                Err(KError::ShouldWait) => {
                    wait(&socket.0, WRITABLE | PEER_CLOSED, deadline)?;
                }
                Err(e) => return Err(rights_errno(e)),
            }
        };
        if first == data.len() {
            return Ok(first);
        }
        // The rest as any write, what went counted whatever comes.
        Ok(first + self.stream.send_until(&data[first..], nonblock, false, deadline).unwrap_or(0))
    }
}

/// A datagram or seqpacket message, as it came.
struct Packet {
    from: SocketPeer,
    data: Vec<u8>,
    rights: Rights,
}

/// Messages that come on a channel: datagrams on a datagram socket's
/// inbox, or a socket pair's or seqpacket connection's messages, which go
/// out on the same channel; or a listener's connections.
pub struct Mailbox {
    pub channel: Arc<Channel>,
    /// A message taken off the channel and not read yet: looked at
    /// (`MSG_PEEK`), or measured (`FIONREAD`).
    held: Mutex<Option<Packet>>,
    /// A connection's: its messages end when the other end says so, or is
    /// gone (a datagram socket's peer may be gone with nothing ending).
    connection: bool,
    /// The other end sends no more.
    ended: AtomicBool,
}

impl Mailbox {
    fn new(channel: Channel, connection: bool) -> Mailbox {
        Mailbox { channel: Arc::new(channel), held: Mutex::new(None), connection, ended: AtomicBool::new(false) }
    }

    fn ended(&self) -> bool {
        self.ended.load(Ordering::Relaxed)
    }

    /// Fills `held` with the next message off the channel, if one waits.
    fn pull(&self, held: &mut Option<Packet>) -> Result<(), isize> {
        while held.is_none() {
            match self.channel.read() {
                Ok(mut msg) => {
                    let Ok((header, mut d)) = ipc::open(&mut msg) else { continue };
                    match header.ordinal {
                        socket_event::DATAGRAM => {
                            if let Ok(dg) = SocketDatagram::decode(&mut d) {
                                let rights = Rights { bytes: dg.rights.0, handles: dg.handles };
                                *held = Some(Packet { from: dg.from, data: dg.data.0, rights });
                            }
                        }
                        socket_event::END if self.connection => self.ended.store(true, Ordering::Relaxed),
                        // Not what the other end sends: passed over.
                        _ => {}
                    }
                }
                Err(KError::ShouldWait) => break,
                Err(KError::PeerClosed) => {
                    if self.connection {
                        self.ended.store(true, Ordering::Relaxed);
                    }
                    break;
                }
                Err(e) => return Err(error::kernel(e)),
            }
        }
        Ok(())
    }

    /// Whether a read would not wait: a message waits, or none ever will.
    fn readable(&self) -> bool {
        let gone = if self.connection { PEER_CLOSED } else { 0 };
        self.held.lock().is_some() || self.ended() || self.channel.wait(READABLE | gone, 0).is_ok()
    }

    /// The next message's length (`FIONREAD`).
    fn next_len(&self) -> usize {
        let mut held = self.held.lock();
        let _ = self.pull(&mut held);
        held.as_ref().map_or(0, |p| p.data.len())
    }

    /// Receives one message into `bufs` in turn, cut to fit, waiting for
    /// one (unless `nonblock`) until `deadline`; with `peek` it stays to be
    /// read again (its descriptors with it). Once the other end sends no
    /// more, or this end reads no more (`shut_read`), 0 bytes from nobody.
    fn recv(&self, bufs: &mut [&mut [u8]], how: Recv, deadline: u64, shut_read: bool) -> Result<Received, isize> {
        loop {
            {
                let mut held = self.held.lock();
                self.pull(&mut held)?;
                if let Some(p) = held.as_ref() {
                    let mut n = 0;
                    for buf in bufs.iter_mut() {
                        let k = buf.len().min(p.data.len() - n);
                        buf[..k].copy_from_slice(&p.data[n..n + k]);
                        n += k;
                    }
                    let len = p.data.len();
                    let (from, pid) = (p.from.name.0.clone(), pid_of(p.from.koid));
                    let rights = match how.peek {
                        true => None,
                        false => held.take().map(|p| p.rights).filter(|r| !r.bytes.is_empty()),
                    };
                    let reported = if how.trunc { len } else { n };
                    return Ok(Received { len: reported, from: Some(from), truncated: n < len, rights, pid });
                }
            }
            if self.ended() || shut_read {
                return Ok(Received::nothing());
            }
            if how.nonblock {
                return Err(EAGAIN);
            }
            let gone = if self.connection { PEER_CLOSED } else { 0 };
            wait(&self.channel.0, READABLE | gone, deadline)?;
        }
    }

    /// Sends a message, and the descriptors `fds` with it, on a pair's or
    /// a connection's channel, waiting for room until `deadline` unless
    /// `nonblock` (`EAGAIN`); `EPIPE` once the other end is gone.
    fn send(&self, from: SocketPeer, data: &[u8], fds: &[Arc<Description>], nonblock: bool, end: u64) -> SysResult {
        let errno = |e: IpcError| match e {
            IpcError::Kernel(k) => rights_errno(k),
            IpcError::PeerClosed => EPIPE,
            IpcError::TooLarge => EMSGSIZE,
            _ => EIO,
        };
        loop {
            // What carries descriptors goes only where there is room.
            let s = self.channel.wait(WRITABLE | PEER_CLOSED, 0).unwrap_or(0);
            if s & PEER_CLOSED != 0 {
                return Err(EPIPE);
            }
            if s & WRITABLE == 0 {
                if nonblock {
                    return Err(EAGAIN);
                }
                wait(&self.channel.0, WRITABLE | PEER_CLOSED, end)?;
                continue;
            }
            let rights = Rights::of(fds)?;
            let d = SocketDatagram {
                from: from.clone(),
                data: Bytes(data.to_vec()),
                rights: Bytes(rights.bytes),
                handles: rights.handles,
            };
            return match ipc::send_event(&self.channel, socket_event::DATAGRAM, d) {
                Ok(()) => Ok(data.len()),
                // Another writer took the room: carried afresh next time.
                Err(IpcError::Kernel(KError::ShouldWait)) if !nonblock => continue,
                Err(IpcError::Kernel(KError::ShouldWait)) => Err(EAGAIN),
                Err(e) => Err(errno(e)),
            };
        }
    }

    /// The next connection on a listener's inbox, without waiting.
    fn connection(&self) -> Result<Option<SocketConnection>, isize> {
        loop {
            match self.channel.read() {
                Ok(msg) => {
                    // What is not a connection is not what the VFS sends:
                    // passed over.
                    if let Ok((socket_event::CONNECTION, c)) = ipc::decode_event::<SocketConnection>(msg) {
                        return Ok(Some(c));
                    }
                }
                // The VFS keeps its end while the socket is bound.
                Err(KError::ShouldWait | KError::PeerClosed) => return Ok(None),
                Err(e) => return Err(error::kernel(e)),
            }
        }
    }
}

/// The name a socket is bound to.
struct Bound {
    /// As its program gave it.
    name: Vec<u8>,
    /// The VFS's id for it.
    id: u64,
    /// Its inbox, whose other end the VFS writes on; closing it closes the
    /// socket for the VFS.
    inbox: Arc<Mailbox>,
}

/// Where a socket is.
enum Conn {
    /// A stream or seqpacket socket neither connected nor listening; a
    /// datagram socket other than a pair's end.
    Idle,
    /// A stream or seqpacket socket that takes connections (on its inbox).
    Listening,
    /// A connected stream socket.
    Stream(Arc<StreamConn>),
    /// A connected seqpacket socket, or a datagram socket pair's end: its
    /// messages come on the channel and go out on it.
    Paired(Arc<Mailbox>, Peer),
}

/// Where a connected datagram socket sends.
#[derive(Clone)]
struct Dest {
    /// The address it was connected to.
    address: SocketAddress,
    /// Who is there.
    peer: Peer,
}

ipc::message! {
    /// The options a program set (`setsockopt`): those that change what a
    /// socket does, and those that do not, kept to be read back.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct Options {
        /// How long reads and writes wait (ns; `None`: for ever).
        pub recv_timeout: Option<u64>,
        pub send_timeout: Option<u64>,
        /// Received messages say who sent them (`SO_PASSCRED`).
        pub passcred: bool,
        pub reuse_addr: bool,
        pub keepalive: bool,
        pub broadcast: bool,
        pub oob_inline: bool,
        /// `SO_LINGER`'s time, when it is on.
        pub linger: Option<u32>,
    }
}

/// What a socket is, as it travels.
mod shape {
    /// Neither connected nor listening; bound (its inbox goes) or not.
    pub const IDLE: u32 = 0;
    /// Listening (its inbox goes).
    pub const LISTENING: u32 = 1;
    /// A connected stream socket (its kernel socket goes).
    pub const STREAM: u32 = 2;
    /// A connected seqpacket socket, or a socket pair's datagram end (its
    /// channel goes).
    pub const PAIRED: u32 = 3;
}

ipc::message! {
    /// A socket on its way to another process, beside the handle that
    /// carries it: its connection's kernel socket or channel, else its
    /// inbox if it is bound, else none (see [`Unix::travelling`]).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Travelling {
        /// Its type (`SOCK_STREAM`, ...).
        kind: u32,
        shape: u32,
        /// The name `getsockname` says.
        local: Bytes,
        /// The VFS's id for the socket and the name it is bound to, when
        /// its inbox goes.
        bound: Option<(u64, Bytes)>,
        /// Who is at the other end of its connection: the name, the
        /// process.
        peer: (Bytes, i32),
        /// A seqpacket connection's other end sends no more.
        ended: bool,
        /// Where a connected datagram socket sends, and who is there.
        dest: Option<(SocketAddress, Bytes, i32)>,
        options: Options,
        /// It reads no more, writes no more (`shutdown`).
        shut: (bool, bool),
    }
}

struct State {
    conn: Conn,
    /// The name `getsockname` says: the one it is bound to, or the
    /// listener's it was accepted on (empty: none).
    local: Vec<u8>,
    /// The VFS's socket, once it is bound.
    bound: Option<Bound>,
    /// Where a datagram socket connected to a path sends.
    dest: Option<Dest>,
    options: Options,
    /// The program stopped reading or writing (`shutdown`).
    shut_read: bool,
    shut_write: bool,
}

/// A Unix domain socket (`Object::Unix`).
pub struct Unix {
    pub kind: Kind,
    state: Mutex<State>,
}

/// What a receive got.
#[derive(Debug, Default)]
pub struct Received {
    pub len: usize,
    /// Who sent it: a datagram's (or seqpacket message's) sender's name,
    /// empty for a socket without one; `None` for a stream's bytes.
    pub from: Option<Vec<u8>>,
    /// A message was longer than the buffers.
    pub truncated: bool,
    /// The descriptors that came with it.
    pub rights: Option<Rights>,
    /// The process that sent it (`SCM_CREDENTIALS`).
    pub pid: i32,
}

impl Received {
    /// The end: nothing, from nobody.
    fn nothing() -> Received {
        Received::default()
    }
}

/// What `poll` makes of a socket.
pub struct Readiness {
    pub input: bool,
    pub output: bool,
    pub hangup: bool,
    /// The peer writes no more, or the program stopped reading.
    pub read_hangup: bool,
    /// What to wait on for more, and for which of its signals.
    pub wait: Option<(Waitable, u32)>,
}

impl Unix {
    pub fn new(kind: Kind) -> Unix {
        Unix::with(kind, Conn::Idle)
    }

    fn with(kind: Kind, conn: Conn) -> Unix {
        let state = State {
            conn,
            local: Vec::new(),
            bound: None,
            dest: None,
            options: Options::default(),
            shut_read: false,
            shut_write: false,
        };
        Unix { kind, state: Mutex::new(state) }
    }

    /// A connected pair (`socketpair`), each end's peer this process.
    pub fn pair(kind: Kind) -> Result<(Unix, Unix), isize> {
        let me = Peer { name: Vec::new(), pid: process::pid() };
        let conn = |c: Conn| Unix::with(kind, c);
        match kind {
            Kind::Stream => {
                let (a, b) = Socket::create().map_err(error::kernel)?;
                Ok((
                    conn(Conn::Stream(Arc::new(StreamConn::new(a, me.clone())))),
                    conn(Conn::Stream(Arc::new(StreamConn::new(b, me)))),
                ))
            }
            Kind::Datagram | Kind::SeqPacket => {
                let (a, b) = Channel::create_shared().map_err(error::kernel)?;
                let connection = kind == Kind::SeqPacket;
                Ok((
                    conn(Conn::Paired(Arc::new(Mailbox::new(a, connection)), me.clone())),
                    conn(Conn::Paired(Arc::new(Mailbox::new(b, connection)), me)),
                ))
            }
        }
    }

    /// The socket as it travels to another process (sent over a socket, or
    /// given to a process this one starts), and the handle that carries it:
    /// a copy, as the socket stays here too, which reads only or writes
    /// only, for a stream, as `readable` and `writable` say.
    pub fn travelling(&self, readable: bool, writable: bool) -> Result<(Travelling, Option<Handle>), isize> {
        let st = self.state();
        let duplicate = |c: &Channel| c.0.duplicate(None).map_err(error::kernel);
        let (mut peer, mut ended, mut bound) = ((Bytes(Vec::new()), 0), false, None);
        let (shape, handle) = match &st.conn {
            Conn::Stream(c) => {
                peer = (Bytes(c.peer.name.clone()), c.peer.pid);
                (shape::STREAM, Some(c.stream.shared_handle(readable, writable)?))
            }
            Conn::Paired(m, p) => {
                (peer, ended) = ((Bytes(p.name.clone()), p.pid), m.ended());
                (shape::PAIRED, Some(duplicate(&m.channel)?))
            }
            Conn::Idle | Conn::Listening => {
                let shape = if matches!(st.conn, Conn::Listening) { shape::LISTENING } else { shape::IDLE };
                let inbox = match &st.bound {
                    Some(b) => {
                        bound = Some((b.id, Bytes(b.name.clone())));
                        Some(duplicate(&b.inbox.channel)?)
                    }
                    None => None,
                };
                (shape, inbox)
            }
        };
        let travelling = Travelling {
            kind: self.kind.sock_type(),
            shape,
            local: Bytes(st.local.clone()),
            bound,
            peer,
            ended,
            dest: st.dest.as_ref().map(|d| (d.address.clone(), Bytes(d.peer.name.clone()), d.peer.pid)),
            options: st.options,
            shut: (st.shut_read, st.shut_write),
        };
        Ok((travelling, handle))
    }

    /// The socket that came from another process ([`Unix::travelling`]),
    /// with the handle that carries it; `None` if the handle is not what it
    /// says.
    pub fn arrived(t: Travelling, handle: Option<Handle>) -> Option<Unix> {
        let kind = Kind::of(t.kind)?;
        let is = |h: &Handle, ty: ObjectType| h.basic_info().is_ok_and(|i| i.object_type == ty as u32);
        let peer = Peer { name: t.peer.0.0, pid: t.peer.1 };
        let mut bound = None;
        let conn = match (t.shape, handle) {
            (shape::STREAM, Some(h)) if kind == Kind::Stream && is(&h, ObjectType::Socket) => {
                Conn::Stream(Arc::new(StreamConn::new(Socket::from_handle(h), peer)))
            }
            (shape::PAIRED, Some(h)) if kind != Kind::Stream && is(&h, ObjectType::Channel) => {
                let mailbox = Mailbox::new(Channel::from_handle(h), kind == Kind::SeqPacket);
                mailbox.ended.store(t.ended, Ordering::Relaxed);
                Conn::Paired(Arc::new(mailbox), peer)
            }
            (shape::IDLE | shape::LISTENING, inbox) => {
                match (t.bound, inbox) {
                    (Some((id, name)), Some(h)) if is(&h, ObjectType::Channel) => {
                        let inbox = Arc::new(Mailbox::new(Channel::from_handle(h), false));
                        bound = Some(Bound { name: name.0, id, inbox });
                    }
                    (None, None) => {}
                    _ => return None,
                }
                match t.shape {
                    shape::LISTENING if bound.is_none() || kind == Kind::Datagram => return None,
                    shape::LISTENING => Conn::Listening,
                    _ => Conn::Idle,
                }
            }
            _ => return None,
        };
        let u = Unix::with(kind, conn);
        {
            let mut st = u.state();
            st.local = t.local.0;
            st.bound = bound;
            st.dest = t.dest.map(|(address, name, pid)| Dest { address, peer: Peer { name: name.0, pid } });
            st.options = t.options;
            (st.shut_read, st.shut_write) = t.shut;
        }
        Some(u)
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock()
    }

    pub fn options(&self) -> Options {
        self.state().options
    }

    pub fn set_options(&self, f: impl FnOnce(&mut Options)) -> SysResult {
        f(&mut self.state().options);
        Ok(0)
    }

    /// Whether it listens (`SO_ACCEPTCONN`).
    pub fn listening(&self) -> bool {
        matches!(self.state().conn, Conn::Listening)
    }

    /// `getsockname`: the name it is bound to (empty: none).
    pub fn local(&self) -> Vec<u8> {
        self.state().local.clone()
    }

    /// `getpeername`: the name its peer's socket is bound to.
    pub fn peer_name(&self) -> Result<Vec<u8>, isize> {
        let st = self.state();
        match (&st.conn, &st.dest) {
            (Conn::Stream(c), _) => Ok(c.peer.name.clone()),
            (Conn::Paired(_, p), _) => Ok(p.name.clone()),
            (_, Some(d)) => Ok(d.peer.name.clone()),
            _ => Err(ENOTCONN),
        }
    }

    /// `SO_PEERCRED`: who connected the other end, or made the pair (none
    /// before a connection).
    pub fn peer_cred(&self) -> Ucred {
        let pid = match &self.state().conn {
            Conn::Stream(c) => c.peer.pid,
            Conn::Paired(_, p) => p.pid,
            _ => 0,
        };
        Ucred { pid, uid: vfs::UID, gid: vfs::UID }
    }

    /// `bind`: the socket takes `name` in the file system.
    pub fn bind(&self, addr: Addr) -> SysResult {
        match addr {
            Addr::Unnamed => self.autobind(),
            Addr::Unspec => Err(EINVAL),
            addr => self.bind_to(&addr),
        }
    }

    /// Binds the socket to `addr`: `EADDRINUSE` while its name is taken.
    fn bind_to(&self, addr: &Addr) -> SysResult {
        let name = addr.name();
        if name.len() > MAX_SOCKET_NAME {
            return Err(EINVAL);
        }
        let at = address_of(addr)?;
        let mut st = self.state();
        if st.bound.is_some() || !st.local.is_empty() || !matches!(st.conn, Conn::Idle) {
            return Err(EINVAL);
        }
        // Shared, so that the socket can travel to other processes.
        let (ours, theirs) = Channel::create_shared().map_err(error::kernel)?;
        let kind = self.kind.vfs();
        let id = match vfs::with(|c| c.bind_socket(at, kind, Bytes(name.clone()), process::koid(), theirs))? {
            Ok(Ok(id)) => id,
            Ok(Err(FsError::Exists)) => return Err(EADDRINUSE),
            Ok(Err(e)) => return Err(error::fs(e)),
            Err(_) => return Err(EIO),
        };
        st.local.clone_from(&name);
        st.bound = Some(Bound { name, id, inbox: Arc::new(Mailbox::new(ours, false)) });
        Ok(0)
    }

    /// Binds the socket to a free abstract name of its own choosing, as
    /// Linux does (the first free one from a random place); nothing for a
    /// socket that has a name.
    fn autobind(&self) -> SysResult {
        if !self.state().local.is_empty() {
            return Ok(0);
        }
        let mut random = [0u8; 4];
        rt::object::random_bytes(&mut random);
        let start = u32::from_le_bytes(random);
        for k in 0..AUTOBIND_NAMES {
            let n = start.wrapping_add(k) % AUTOBIND_NAMES;
            match self.bind_to(&Addr::Abstract(alloc::format!("{n:05x}").into_bytes())) {
                Err(EADDRINUSE) => {}
                other => return other,
            }
        }
        Err(ENOSPC)
    }

    /// A socket not bound that says who sends (`SO_PASSCRED`) takes a name
    /// before it connects or sends, so that its peers can say who it is,
    /// as on Linux.
    fn autobind_to_send(&self) -> Result<(), isize> {
        let st = self.state();
        if st.options.passcred && st.local.is_empty() && matches!(st.conn, Conn::Idle) {
            drop(st);
            self.autobind()?;
        }
        Ok(())
    }

    /// `listen`: a bound stream or seqpacket socket takes connections. The
    /// backlog is the inbox's.
    pub fn listen(&self) -> SysResult {
        if self.kind == Kind::Datagram {
            return Err(EOPNOTSUPP);
        }
        let mut st = self.state();
        match st.conn {
            Conn::Listening => return Ok(0),
            Conn::Idle => {}
            _ => return Err(EINVAL),
        }
        let id = st.bound.as_ref().ok_or(EINVAL)?.id;
        error::ipc(vfs::with(|c| c.listen_socket(id))?)?;
        st.conn = Conn::Listening;
        Ok(0)
    }

    /// `accept`: the next connection, waiting for one unless `nonblock`
    /// (until the receive time-out).
    pub fn accept(&self, nonblock: bool) -> Result<Unix, isize> {
        if self.kind == Kind::Datagram {
            return Err(EOPNOTSUPP);
        }
        let (inbox, name, end) = {
            let st = self.state();
            let (Conn::Listening, Some(b)) = (&st.conn, &st.bound) else { return Err(EINVAL) };
            (b.inbox.clone(), b.name.clone(), deadline(st.options.recv_timeout))
        };
        loop {
            if let Some(c) = inbox.connection()? {
                let peer = Peer::of(&c.from);
                let end = c.ends.into_iter().next().filter(|h| {
                    let wanted = if self.kind == Kind::Stream { ObjectType::Socket } else { ObjectType::Channel };
                    h.basic_info().is_ok_and(|i| i.object_type == wanted as u32)
                });
                let conn = match end {
                    Some(end) if self.kind == Kind::Stream => {
                        Conn::Stream(Arc::new(StreamConn::new(Socket::from_handle(end), peer)))
                    }
                    Some(end) => Conn::Paired(Arc::new(Mailbox::new(Channel::from_handle(end), true)), peer),
                    // A connection without its end, or with another
                    // object: passed over.
                    None => continue,
                };
                let u = Unix::with(self.kind, conn);
                u.state().local = name;
                return Ok(u);
            }
            if nonblock {
                return Err(EAGAIN);
            }
            wait(&inbox.channel.0, READABLE, end)?;
        }
    }

    /// `connect`: a stream or seqpacket socket connects to the listening
    /// socket at `name` (as soon as the listener's inbox has room: until
    /// the send time-out, or not at all with `nonblock`); a datagram socket
    /// sends there from now on (`None`: nowhere).
    pub fn connect(&self, to: Option<Addr>, nonblock: bool) -> SysResult {
        {
            let st = self.state();
            match (&st.conn, self.kind) {
                (Conn::Idle, _) => {}
                (Conn::Paired(..), Kind::Datagram) => return Err(EISCONN),
                (Conn::Listening, _) => return Err(EINVAL),
                _ => return Err(EISCONN),
            }
        }
        let Some(to) = to else {
            // A datagram socket's association dissolves.
            if self.kind != Kind::Datagram {
                return Err(EINVAL);
            }
            self.state().dest = None;
            return Ok(0);
        };
        let address = address_of(&to)?;
        self.autobind_to_send()?;
        let (from, end) = {
            let st = self.state();
            (st.bound.as_ref().map_or(0, |b| b.id), deadline(st.options.send_timeout))
        };
        let kind = self.kind.vfs();
        if self.kind == Kind::Datagram {
            let (me, to) = (process::koid(), address.clone());
            let peer = error::ipc(vfs::with(|c| c.connect_socket(to, kind, from, me, Vec::new()))?)?;
            self.state().dest = Some(Dest { address, peer: Peer::of(&peer) });
            return Ok(0);
        }
        let mut backoff = 1_000_000;
        loop {
            // The connection's two ends: this socket keeps one, the listener
            // gets the other.
            let (conn, theirs) = match self.kind {
                Kind::Stream => {
                    let (a, b) = Socket::create().map_err(error::kernel)?;
                    (End::Stream(a), alloc::vec![b.into_handle()])
                }
                _ => {
                    let (a, b) = Channel::create_shared().map_err(error::kernel)?;
                    (End::Packets(a), alloc::vec![b.into_handle()])
                }
            };
            match vfs::with(|c| c.connect_socket(address.clone(), kind, from, process::koid(), theirs))? {
                Ok(Ok(peer)) => {
                    let peer = Peer::of(&peer);
                    let mut st = self.state();
                    if !matches!(st.conn, Conn::Idle) {
                        // Another thread connected it meanwhile.
                        return Err(EISCONN);
                    }
                    st.conn = match conn {
                        End::Stream(socket) => Conn::Stream(Arc::new(StreamConn::new(socket, peer))),
                        End::Packets(channel) => Conn::Paired(Arc::new(Mailbox::new(channel, true)), peer),
                    };
                    st.shut_read = false;
                    st.shut_write = false;
                    return Ok(0);
                }
                Ok(Err(FsError::Busy)) if !nonblock => {
                    // The listener's backlog is full: try again soon.
                    if busy_wait(&mut backoff, end) {
                        return Err(EAGAIN);
                    }
                }
                Ok(Err(e)) => return Err(error::fs(e)),
                Err(_) => return Err(EIO),
            }
        }
    }

    /// `shutdown`: this end stops reading, writing, or both. A stream's peer
    /// reads to the end of what was sent; a seqpacket connection's, after
    /// the messages sent.
    pub fn shutdown(&self, read: bool, write: bool) -> SysResult {
        let mut st = self.state();
        let conn = match &st.conn {
            Conn::Stream(c) => Ok(c.clone()),
            Conn::Paired(m, _) => Err(m.clone()),
            _ if st.dest.is_some() => {
                st.shut_read |= read;
                st.shut_write |= write;
                return Ok(0);
            }
            _ => return Err(ENOTCONN),
        };
        st.shut_read |= read;
        if write && !st.shut_write {
            st.shut_write = true;
            match conn {
                Ok(c) => c.stream.socket.shutdown().map_err(error::kernel)?,
                Err(m) if self.kind == Kind::SeqPacket => {
                    // The peer reads to the end of what came before it.
                    let _ = ipc::send_event(&m.channel, socket_event::END, ());
                }
                Err(_) => {}
            }
        }
        Ok(0)
    }

    /// Bytes that can be read without waiting (`FIONREAD`): a datagram or
    /// seqpacket socket's next message's.
    pub fn available(&self) -> usize {
        let st = self.state();
        match (&st.conn, &st.bound) {
            (Conn::Stream(c), _) => c.stream.available(),
            (Conn::Paired(m, _), _) => m.next_len(),
            (Conn::Idle, Some(b)) if self.kind == Kind::Datagram => b.inbox.next_len(),
            _ => 0,
        }
    }

    /// Receives into `bufs` in turn: from a stream what is there (waiting
    /// for something unless `nonblock`; all of it with `waitall`), else one
    /// message, cut to fit; and the descriptors that came with it. Leaves
    /// it to be read again with `peek`.
    pub fn recv(&self, bufs: &mut [&mut [u8]], how: Recv) -> Result<Received, isize> {
        let (end, shut_read, source) = {
            let st = self.state();
            let (end, shut_read) = (deadline(st.options.recv_timeout), st.shut_read);
            let source = match (&st.conn, &st.bound) {
                (Conn::Stream(c), _) => Ok(c.clone()),
                (Conn::Paired(m, _), _) => Err(m.clone()),
                // A datagram socket receives on its inbox once bound...
                (Conn::Idle, Some(b)) if self.kind == Kind::Datagram => Err(b.inbox.clone()),
                // ... and nothing before.
                (Conn::Idle, None) if self.kind == Kind::Datagram => {
                    if shut_read {
                        return Ok(Received::nothing());
                    }
                    if how.nonblock {
                        return Err(EAGAIN);
                    }
                    drop(st);
                    return Err(if signal::sleep_until(end) { EINTR } else { EAGAIN });
                }
                // Linux's answers to reading a socket that is not connected.
                _ if self.kind == Kind::Stream => return Err(EINVAL),
                _ => return Err(ENOTCONN),
            };
            (end, shut_read, source)
        };
        match source {
            Ok(c) => {
                let room: usize = bufs.iter().map(|b| b.len()).sum();
                let (len, rights) = if bufs.len() == 1 {
                    c.recv(bufs[0], how, end, shut_read)?
                } else {
                    // Read whole, then laid out in the buffers.
                    let mut all = alloc::vec![0u8; room.min(256 * 1024)];
                    let (n, rights) = c.recv(&mut all, how, end, shut_read)?;
                    let mut at = 0;
                    for buf in bufs.iter_mut() {
                        let k = buf.len().min(n - at);
                        buf[..k].copy_from_slice(&all[at..at + k]);
                        at += k;
                    }
                    (n, rights)
                };
                Ok(Received { len, from: None, truncated: false, rights, pid: c.peer.pid })
            }
            Err(m) => m.recv(bufs, how, end, shut_read),
        }
    }

    /// Sends `data`, and the descriptors `fds` with it: on a stream all of
    /// it (waiting for room unless `nonblock`: then what fits), else as one
    /// message, to `dest` (a datagram socket's) or the peer. A stream or
    /// seqpacket socket that writes no more raises `SIGPIPE` (unless
    /// `nosignal`).
    pub fn send(
        &self,
        data: &[u8],
        dest: Option<Addr>,
        fds: &[Arc<Description>],
        nonblock: bool,
        nosignal: bool,
    ) -> SysResult {
        let r = self.send_inner(data, dest, fds, nonblock);
        if r == Err(EPIPE) && !nosignal && self.kind != Kind::Datagram {
            signal::raise(crate::linux::sig::PIPE);
        }
        r
    }

    fn send_inner(&self, data: &[u8], dest: Option<Addr>, fds: &[Arc<Description>], nonblock: bool) -> SysResult {
        if self.kind == Kind::Datagram {
            self.autobind_to_send()?;
        }
        let (end, shut_write, target, me, from) = {
            let st = self.state();
            // Its name: the one it is bound to, or its listener's.
            let me = Peer::me(&st.local);
            let target = match (&st.conn, &dest) {
                (Conn::Stream(c), None) => Target::Stream(c.clone()),
                (Conn::Stream(_), Some(_)) => return Err(EISCONN),
                (Conn::Paired(m, _), _) if self.kind == Kind::SeqPacket || dest.is_none() => Target::Channel(m.clone()),
                (Conn::Paired(..), Some(_)) => return Err(EISCONN),
                (Conn::Idle, Some(_)) if self.kind == Kind::Stream => return Err(EOPNOTSUPP),
                (Conn::Idle, Some(to)) if self.kind == Kind::Datagram => Target::Address(address_of(to)?),
                (Conn::Idle, None) if self.kind == Kind::Datagram => match &st.dest {
                    Some(d) => Target::Address(d.address.clone()),
                    None => return Err(ENOTCONN),
                },
                _ => return Err(ENOTCONN),
            };
            let from = st.bound.as_ref().map_or(0, |b| b.id);
            (deadline(st.options.send_timeout), st.shut_write, target, me, from)
        };
        if shut_write {
            return Err(EPIPE);
        }
        if self.kind != Kind::Stream && data.len() > MAX_DATAGRAM {
            return Err(EMSGSIZE);
        }
        match target {
            Target::Stream(c) => c.send(data, fds, nonblock, end),
            Target::Channel(m) => match m.send(me, data, fds, nonblock, end) {
                // A datagram socket pair's peer is gone: nobody takes what
                // it sends.
                Err(EPIPE) if self.kind == Kind::Datagram => Err(ECONNREFUSED),
                other => other,
            },
            Target::Address(to) => {
                let mut backoff = 1_000_000;
                loop {
                    // Carried afresh each time: a call refused took them.
                    let rights = Rights::of(fds)?;
                    let (bytes, handles) = (Bytes(rights.bytes), rights.handles);
                    let sent = vfs::with(|c| {
                        c.send_to_socket(to.clone(), from, process::koid(), Bytes(data.to_vec()), bytes, handles)
                    })?;
                    match sent {
                        Ok(Ok(())) => return Ok(data.len()),
                        Ok(Err(FsError::Busy)) if !nonblock => {
                            if busy_wait(&mut backoff, end) {
                                return Err(EAGAIN);
                            }
                        }
                        Ok(Err(e)) => return Err(error::fs(e)),
                        Err(IpcError::TooLarge) => return Err(EMSGSIZE),
                        Err(_) => return Err(EIO),
                    }
                }
            }
        }
    }

    /// What `poll` makes of the socket, for reading (`want_in`) and writing
    /// (`want_out`).
    pub fn readiness(&self, want_in: bool, want_out: bool) -> Readiness {
        let st = self.state();
        let none = |input, output, hangup| Readiness { input, output, hangup, read_hangup: false, wait: None };
        match (&st.conn, &st.bound) {
            (Conn::Stream(c), _) => {
                let s = c.signals();
                let closed = s & PEER_CLOSED != 0;
                let read_hangup = closed || s & PEER_WRITE_DISABLED != 0 || st.shut_read;
                let input = s & READABLE != 0 || read_hangup;
                // A write to a peer that is gone does not wait: it fails.
                let output = s & WRITABLE != 0 || closed;
                // What is not raised yet (raised, it would be waited for
                // without waiting).
                let mut signals = 0;
                if !closed {
                    signals |= PEER_CLOSED;
                    if s & PEER_WRITE_DISABLED == 0 {
                        signals |= PEER_WRITE_DISABLED;
                    }
                }
                if want_in && !input {
                    signals |= READABLE;
                }
                if want_out && !output {
                    signals |= WRITABLE;
                }
                let wait = (signals != 0).then(|| (Waitable::Stream(c.clone()), signals));
                Readiness { input, output, hangup: closed || (read_hangup && st.shut_write), read_hangup, wait }
            }
            (Conn::Paired(m, _), _) => {
                let s = m.channel.wait(READABLE | WRITABLE | PEER_CLOSED, 0).unwrap_or(0);
                let closed = s & PEER_CLOSED != 0;
                let input = m.readable() || st.shut_read;
                let output = s & WRITABLE != 0 || closed;
                let read_hangup = m.connection && (m.ended() || closed || st.shut_read);
                let mut signals = if closed { 0 } else { PEER_CLOSED };
                if want_in && !input {
                    signals |= READABLE;
                }
                if want_out && !output {
                    signals |= WRITABLE;
                }
                let wait = (signals != 0).then(|| (Waitable::Channel(m.channel.clone()), signals));
                Readiness { input, output, hangup: m.connection && closed, read_hangup, wait }
            }
            (Conn::Listening, Some(b)) => {
                let input = b.inbox.channel.wait(READABLE, 0).is_ok();
                let wait = (want_in && !input).then(|| (Waitable::Channel(b.inbox.channel.clone()), READABLE));
                Readiness { input, output: false, hangup: false, read_hangup: false, wait }
            }
            (Conn::Idle, bound) if self.kind == Kind::Datagram => {
                // Sends to a path wait for room in the VFS's calls.
                let input = bound.as_ref().is_some_and(|b| b.inbox.readable()) || st.shut_read;
                let wait = match bound {
                    Some(b) if want_in && !input => Some((Waitable::Channel(b.inbox.channel.clone()), READABLE)),
                    _ => None,
                };
                Readiness { input, output: true, hangup: false, read_hangup: st.shut_read, wait }
            }
            // A stream or seqpacket socket that is not connected is hung
            // up, as on Linux; only its own calls change that.
            _ => none(false, true, true),
        }
    }

    /// How many bytes it holds for reading, or for its peer
    /// (`SO_RCVBUF`, `SO_SNDBUF`): a stream's endpoint's room, a message's
    /// largest.
    pub fn buffer_size(&self) -> usize {
        match &self.state().conn {
            Conn::Stream(c) => c.stream.socket.info().map_or(0, |i| (i.readable + i.writable) as usize),
            _ => MAX_DATAGRAM,
        }
    }

    /// The object id that identifies the socket (`fstat`).
    pub fn id(&self) -> u64 {
        let st = self.state();
        match (&st.conn, &st.bound) {
            (Conn::Stream(c), _) => c.stream.koid,
            (Conn::Paired(m, _), _) => m.channel.0.koid(),
            (_, Some(b)) => b.inbox.channel.0.koid(),
            _ => 0,
        }
    }
}

/// A connection's end, as the socket that connects keeps it.
enum End {
    Stream(Socket),
    Packets(Channel),
}

/// Where a send goes.
enum Target {
    Stream(Arc<StreamConn>),
    Channel(Arc<Mailbox>),
    Address(SocketAddress),
}

/// Waits a while before a call that found a socket's inbox full asks again,
/// longer each time; whether `deadline` passed.
fn busy_wait(backoff: &mut u64, deadline: u64) -> bool {
    let now = time::monotonic_ns();
    if now >= deadline {
        return true;
    }
    signal::sleep_until(now.saturating_add(*backoff).min(deadline));
    *backoff = (*backoff * 2).min(BUSY_WAIT_MAX_NS);
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(family: u16, path: &[u8]) -> Vec<u8> {
        let mut v = family.to_ne_bytes().to_vec();
        v.extend_from_slice(path);
        v
    }

    fn read(bytes: &[u8]) -> Result<Addr, isize> {
        // SAFETY: the test's own bytes.
        unsafe { read_addr(bytes.as_ptr() as usize, bytes.len()) }
    }

    #[test]
    fn names_are_paths_up_to_their_nul() {
        assert_eq!(read(&addr(AF_UNIX, b"/tmp/a.sock\0")), Ok(Addr::Path(b"/tmp/a.sock".to_vec())));
        // Without a NUL, and with garbage after one.
        assert_eq!(read(&addr(AF_UNIX, b"rel")), Ok(Addr::Path(b"rel".to_vec())));
        assert_eq!(read(&addr(AF_UNIX, b"x\0yz")), Ok(Addr::Path(b"x".to_vec())));
        // The whole of `sun_path`, and a byte more.
        assert_eq!(read(&addr(AF_UNIX, &[b'p'; 108])), Ok(Addr::Path(alloc::vec![b'p'; 108])));
        assert_eq!(read(&addr(AF_UNIX, &[b'p'; 109])), Err(EINVAL));
        assert_eq!(read(&addr(AF_UNSPEC, b"")), Ok(Addr::Unspec));
        // Abstract names: every byte the length covers, NULs too, and
        // the empty one; the family alone.
        assert_eq!(read(&addr(AF_UNIX, b"\0abstract")), Ok(Addr::Abstract(b"abstract".to_vec())));
        assert_eq!(read(&addr(AF_UNIX, b"\0a\0b\0")), Ok(Addr::Abstract(b"a\0b\0".to_vec())));
        assert_eq!(read(&addr(AF_UNIX, b"\0")), Ok(Addr::Abstract(Vec::new())));
        assert_eq!(read(&addr(AF_UNIX, b"")), Ok(Addr::Unnamed));
        // Other families, too short.
        assert_eq!(read(&addr(crate::linux::sock::AF_INET, b"/x")), Err(EINVAL));
        assert_eq!(read(&[1]), Err(EINVAL));
        // SAFETY: a null address, which is refused.
        assert_eq!(unsafe { read_addr(0, 10) }, Err(EFAULT));
    }

    #[test]
    fn stored_names_say_their_length_with_the_nul() {
        let store = |name: &[u8], room: u32| {
            let mut buf = [0xaau8; 112];
            let mut len = room;
            // SAFETY: the test's buffer and length.
            unsafe { store_name(name, buf.as_mut_ptr() as usize, &mut len as *mut u32 as usize).unwrap() };
            (buf, len)
        };
        let (buf, len) = store(b"/tmp/s", 110);
        assert_eq!(len, 2 + 6 + 1);
        assert_eq!(&buf[..9], &[1, 0, b'/', b't', b'm', b'p', b'/', b's', 0]);
        assert_eq!(buf[9], 0xaa);
        // Unnamed: the family alone.
        let (buf, len) = store(b"", 110);
        assert_eq!((len, &buf[..3]), (2, &[1, 0, 0xaa][..]));
        // Abstract: its bytes, and nothing after them.
        let name = Addr::Abstract(b"x\0y".to_vec()).name();
        let (buf, len) = store(&name, 110);
        assert_eq!((len, &buf[..7]), (6, &[1, 0, 0, b'x', 0, b'y', 0xaa][..]));
        // Cut to the room there is, the whole length said.
        let (buf, len) = store(b"/tmp/s", 4);
        assert_eq!((len, &buf[..5]), (9, &[1, 0, b'/', b't', 0xaa][..]));
    }

    #[test]
    fn kinds_are_socket_types() {
        for k in [Kind::Stream, Kind::Datagram, Kind::SeqPacket] {
            assert_eq!(Kind::of(k.sock_type()), Some(k));
        }
        assert_eq!(Kind::of(3), None);
        assert_eq!((Kind::Stream.vfs(), Kind::Datagram.vfs(), Kind::SeqPacket.vfs()), (1, 2, 5));
    }

    #[test]
    fn sockets_travel_as_they_are() {
        let to = SocketAddress { path: "/tmp/b".into(), abstract_name: Bytes(Vec::new()) };
        let t = Travelling {
            kind: DGRAM,
            shape: shape::IDLE,
            local: Bytes(b"/tmp/a".to_vec()),
            bound: Some((7, Bytes(b"/tmp/a".to_vec()))),
            peer: (Bytes(b"\0x".to_vec()), 12),
            ended: true,
            dest: Some((to, Bytes(b"/tmp/b".to_vec()), 13)),
            options: Options { recv_timeout: Some(5), passcred: true, linger: Some(3), ..Options::default() },
            shut: (true, false),
        };
        let mut e = ipc::Encoder::new();
        ipc::Encode::encode(t.clone(), &mut e);
        assert!(e.handles.is_empty());
        assert_eq!(Travelling::decode(&mut ipc::Decoder::new(&e.bytes, Vec::new())).ok(), Some(t));
    }
}
