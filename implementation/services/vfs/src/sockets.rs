// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Unix domain sockets bound to paths or abstract names
//! (`vfs::bind_socket`): what the VFS keeps of each, beside its node in the
//! writable file system for a path.
//!
//! A socket's program gives the VFS one end of the socket's inbox, a
//! channel, and keeps the other. The VFS writes on it what comes for the
//! socket (`vproto::fs::socket_event`): the connections to a listening
//! stream or seqpacket socket, which carry the ends the connecting program
//! made (the bytes then go between the two programs without the VFS), and
//! the datagrams sent to a datagram socket. A full inbox makes connecting
//! and sending wait (`Busy`), as a full backlog or receive queue does on
//! Linux.
//!
//! A socket outlives neither its program's end of the inbox nor its node in
//! the file system, whichever goes last: a node whose socket was closed
//! stays, refusing connections, until it is removed (as on Linux), and a
//! socket whose node was removed keeps its name for the connections it
//! makes. An abstract name has no node: it is the socket's while the
//! socket is open, and free once it is closed. Abstract names are apart
//! for each type of socket, as Linux keeps them (a stream socket and a
//! datagram socket may have the same). A closed socket is found out when
//! something is sent to it, its abstract name is asked for, or its program
//! has bound as many sockets as it may.
//!
//! Who connects or sends is told to the other end: the process, as it says
//! (the registry knows a POSIX program by the program that started it, for
//! which it speaks), with the name of its socket if this program bound
//! it. Which program a socket is, the registry says.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use vabi::signals::PEER_CLOSED;
use vipc::{Bytes, IpcError};
use vproto::fs::{FsError, SocketConnection, SocketDatagram, SocketPeer, socket_event, socket_kind};
use vrt::object::{Channel, Handle};

use vfs_core::tree::NodeId;

/// Sockets one program may have bound and open at once: each holds a
/// handle of the VFS's.
pub const MAX_PER_PROGRAM: usize = 1024;
/// Sockets bound and open in all.
pub const MAX_SOCKETS: usize = 8192;

/// Where a socket is bound.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Place {
    /// A node of the file system, while it is there.
    Node(Option<NodeId>),
    /// An abstract name, of its type's.
    Abstract(Vec<u8>),
}

/// A socket bound to a path or an abstract name.
struct Bound {
    /// The VFS's end of its inbox; `None` once its program closed the
    /// socket.
    inbox: Option<Channel>,
    /// Its type (`socket_kind`).
    kind: u32,
    /// The name its program bound it to: what `getsockname` says, and what
    /// its peers are told.
    name: Vec<u8>,
    /// The program that bound it, as the registry knows it.
    owner: u64,
    /// The process that bound it, as it says: who its connections are
    /// with.
    koid: u64,
    /// It takes connections (`listen`).
    listening: bool,
    place: Place,
}

impl Bound {
    /// Whether it is to be kept: its node is in the file system, or its
    /// program has it open (which alone keeps an abstract name, or the
    /// name of a socket whose node was removed). A closed socket's inbox is
    /// let go either way.
    fn named(&mut self) -> bool {
        let open = self.alive();
        open || matches!(self.place, Place::Node(Some(_)))
    }

    /// Whether its program still has it: once it is closed, its inbox is
    /// let go.
    fn alive(&mut self) -> bool {
        if let Some(inbox) = &self.inbox
            && matches!(inbox.wait(PEER_CLOSED, 0), Ok(s) if s & PEER_CLOSED != 0)
        {
            self.inbox = None;
        }
        self.inbox.is_some()
    }

    /// Writes event `ordinal` on its inbox: `Refused` when its program
    /// closed it, `Busy` while the inbox is full.
    fn deliver<T: vipc::Encode>(&mut self, ordinal: u32, payload: T) -> Result<(), FsError> {
        let inbox = self.inbox.as_ref().ok_or(FsError::Refused)?;
        match vipc::send_event(inbox, ordinal, payload) {
            Ok(()) => Ok(()),
            Err(IpcError::Kernel(vabi::Error::ShouldWait)) => Err(FsError::Busy),
            Err(IpcError::PeerClosed) => {
                self.inbox = None;
                Err(FsError::Refused)
            }
            Err(_) => Err(FsError::Invalid),
        }
    }
}

/// The sockets bound to paths or abstract names, by their ids.
pub struct Sockets {
    bound: BTreeMap<u64, Bound>,
    /// The abstract names taken, by type and name.
    names: BTreeMap<(u32, Vec<u8>), u64>,
    next: u64,
}

impl Sockets {
    pub fn new() -> Sockets {
        Sockets { bound: BTreeMap::new(), names: BTreeMap::new(), next: 1 }
    }

    /// Forgets socket `id` if nothing names it any more.
    fn forget_if_gone(&mut self, id: u64) {
        let Some(b) = self.bound.get_mut(&id) else { return };
        if !b.named() {
            if let Place::Abstract(name) = &b.place {
                self.names.remove(&(b.kind, name.clone()));
            }
            self.bound.remove(&id);
        }
    }

    /// How many sockets `owner` has open, and how many are open in all.
    fn open(&self, owner: u64) -> (usize, usize) {
        let open = self.bound.values().filter(|b| b.inbox.is_some());
        open.fold((0, 0), |(mine, all), b| (mine + (b.owner == owner) as usize, all + 1))
    }

    /// Lets go of the sockets their programs closed, and forgets those no
    /// node names either.
    fn sweep(&mut self) {
        let ids: Vec<u64> = self.bound.keys().copied().collect();
        for id in ids {
            self.forget_if_gone(id);
        }
    }

    /// The id a new socket of `owner`'s is to have, if it may have one
    /// more: [`MAX_PER_PROGRAM`] of its own, [`MAX_SOCKETS`] in all.
    pub fn reserve(&mut self, owner: u64) -> Result<u64, FsError> {
        let full = |(mine, all): (usize, usize)| mine >= MAX_PER_PROGRAM || all >= MAX_SOCKETS;
        if full(self.open(owner)) {
            // Some of them may have been closed since.
            self.sweep();
            if full(self.open(owner)) {
                return Err(FsError::TooMany);
            }
        }
        Ok(self.next)
    }

    /// Records socket [`reserve`](Self::reserve)d, now that its node is
    /// made: `owner`'s, bound by its process `koid`.
    pub fn bind(&mut self, owner: u64, koid: u64, kind: u32, name: Vec<u8>, inbox: Channel, node: NodeId) -> u64 {
        self.insert(owner, koid, kind, name, inbox, Place::Node(Some(node)))
    }

    /// Binds a socket of `owner`'s, of process `koid`, to abstract name
    /// `at` of `kind`'s: `Exists` while an open socket of the type has
    /// it.
    pub fn bind_abstract(
        &mut self,
        owner: u64,
        koid: u64,
        kind: u32,
        at: Vec<u8>,
        name: Vec<u8>,
        inbox: Channel,
    ) -> Result<u64, FsError> {
        if let Some(&id) = self.names.get(&(kind, at.clone())) {
            // Free again once its socket is closed.
            self.forget_if_gone(id);
            if self.names.contains_key(&(kind, at.clone())) {
                return Err(FsError::Exists);
            }
        }
        self.reserve(owner)?;
        let id = self.insert(owner, koid, kind, name, inbox, Place::Abstract(at.clone()));
        self.names.insert((kind, at), id);
        Ok(id)
    }

    fn insert(&mut self, owner: u64, koid: u64, kind: u32, name: Vec<u8>, inbox: Channel, place: Place) -> u64 {
        let id = self.next;
        self.next += 1;
        let bound = Bound { inbox: Some(inbox), kind, name, owner, koid, listening: false, place };
        self.bound.insert(id, bound);
        id
    }

    /// The socket of `kind` an abstract name names: `Refused` for none, as
    /// on Linux.
    pub fn find_abstract(&mut self, kind: u32, name: &[u8]) -> Result<u64, FsError> {
        let id = *self.names.get(&(kind, name.to_vec())).ok_or(FsError::Refused)?;
        self.forget_if_gone(id);
        if self.names.contains_key(&(kind, name.to_vec())) { Ok(id) } else { Err(FsError::Refused) }
    }

    /// Socket `id`'s node was removed from the file system: it is
    /// forgotten if its program closed it too.
    pub fn unlinked(&mut self, id: u64) {
        let Some(b) = self.bound.get_mut(&id) else { return };
        b.place = Place::Node(None);
        self.forget_if_gone(id);
    }

    /// Socket `id`, `owner`'s, takes connections from now on.
    pub fn listen(&mut self, id: u64, owner: u64) -> Result<(), FsError> {
        match self.bound.get_mut(&id) {
            Some(b) if b.owner == owner && b.kind != socket_kind::DATAGRAM => {
                b.listening = true;
                Ok(())
            }
            _ => Err(FsError::Invalid),
        }
    }

    /// Who connects or sends: process `koid` of `owner`, from its socket
    /// `from` (0: one not bound, or not by it).
    pub fn peer(&self, from: u64, owner: u64, koid: u64) -> SocketPeer {
        let name = match self.bound.get(&from) {
            Some(b) if from != 0 && b.owner == owner => b.name.clone(),
            _ => Vec::new(),
        };
        SocketPeer { koid, name: Bytes(name) }
    }

    /// The socket `id` of `kind`, which takes what comes for it: `Refused`
    /// once its program closed it, `WrongType` for another type.
    fn reach(&mut self, id: u64, kind: u32) -> Result<&mut Bound, FsError> {
        let b = self.bound.get_mut(&id).ok_or(FsError::Refused)?;
        if !b.alive() {
            return Err(FsError::Refused);
        }
        if b.kind != kind {
            return Err(FsError::WrongType);
        }
        Ok(b)
    }

    /// Connects `from` to socket `id`, of `kind`: hands a listening stream
    /// or seqpacket socket the connection's `ends`; a datagram socket is
    /// only checked (it is where the connecting socket sends from now on).
    /// Returns who is at the other end.
    pub fn connect(&mut self, id: u64, kind: u32, from: SocketPeer, ends: Vec<Handle>) -> Result<SocketPeer, FsError> {
        // A connection carries its one end; a datagram socket's, none.
        if ends.len() != usize::from(kind != socket_kind::DATAGRAM) {
            return Err(FsError::Invalid);
        }
        let b = self.reach(id, kind)?;
        let peer = SocketPeer { koid: b.koid, name: Bytes(b.name.clone()) };
        if kind != socket_kind::DATAGRAM {
            if !b.listening {
                return Err(FsError::Refused);
            }
            b.deliver(socket_event::CONNECTION, SocketConnection { from, ends })?;
        }
        Ok(peer)
    }

    /// Sends a datagram to datagram socket `id`.
    pub fn send(&mut self, id: u64, datagram: SocketDatagram) -> Result<(), FsError> {
        self.reach(id, socket_kind::DATAGRAM)?.deliver(socket_event::DATAGRAM, datagram)
    }
}
