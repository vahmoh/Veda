// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Descriptors sent to another process over a Unix domain socket
//! (`SCM_RIGHTS`), and sockets given to a process this one starts: each
//! goes as the handles that carry its object, which the kernel moves, and
//! what they do not say: what it is, its access mode and status flags, a
//! directory's path, a socket's names, peer and options.
//!
//! What a descriptor refers to is shared with the process it goes to, as an
//! open file description is on Linux: a file's connection is duplicated
//! (the VFS keeps one offset for both), a pipe's endpoint, a socket's
//! kernel socket or channel (an Internet socket's channel to the network
//! service, and the kernel socket a connection's bytes go on) too. The
//! flags travel as they are when it is sent: changing them afterwards
//! (`O_NONBLOCK`) changes them on one side only.
//!
//! A memory file's memory goes as a handle with the access the descriptor
//! has (no `WRITE` right for one that may not write), its size and seals
//! with it ([`crate::memfd`]).
//!
//! What has no handle to share — an eventfd, an epoll instance — cannot be
//! sent (`EOPNOTSUPP`) or given to a child, nor can a socket still
//! connecting.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use abi::ObjectType;
use ipc::{Bytes, Decode, Decoder, Encode, Encoder, message};
use runtime::object::{Channel, Handle, Process, Socket, Vmo};

use crate::error;
use crate::fd::{Description, Object};
use crate::file::{Directory, VfsFile};
use crate::inet::Inet;
use crate::linux::errno::EOPNOTSUPP;
use crate::log::Log;
use crate::memfd::{Memfd, Shared};
use crate::stream::Stream;
use crate::unix::Unix;

/// What a descriptor is, as it travels.
mod kind {
    pub const FILE: u32 = 1;
    pub const DIR: u32 = 2;
    pub const STREAM: u32 = 3;
    pub const UNIX: u32 = 4;
    pub const NULL: u32 = 5;
    pub const LOG: u32 = 6;
    pub const PIDFD: u32 = 7;
    pub const INET: u32 = 8;
    pub const MEMFD: u32 = 9;
}

message! {
    /// What a memory file's handle does not say.
    struct MemfdState {
        size: u64,
        seals: u32,
    }
}

message! {
    /// A descriptor on its way.
    struct Carried {
        kind: u32,
        /// Its access mode and status flags.
        flags: u32,
        /// What its handles do not say: a directory's path; a socket's
        /// `Travelling` ([`crate::unix`]'s, [`crate::inet`]'s), encoded.
        state: Bytes,
        /// The handles that carry it.
        handles: Vec<Handle>,
    }
}

/// `m`, encoded (it carries no handle).
fn encoded(m: impl Encode) -> Vec<u8> {
    let mut e = Encoder::new();
    m.encode(&mut e);
    e.bytes
}

/// What `bytes` encode, if they do.
fn decoded<T: Decode>(bytes: &[u8]) -> Option<T> {
    T::decode(&mut Decoder::new(bytes, Vec::new())).ok()
}

/// A socket on its way to a process this one starts.
pub struct ToChild {
    /// What makes its handles a socket: an entry of the process's startup
    /// message.
    pub entry: Vec<u8>,
    /// The handles that carry it (as its descriptor's roles): none, one, or
    /// a connection's two.
    pub handles: Vec<Handle>,
}

/// What a process this one starts needs to have the socket of descriptor
/// `desc`, which its handles alone do not make one; `None` for a socket
/// that does not go (one still connecting).
pub fn for_child(desc: &Description) -> Result<Option<ToChild>, isize> {
    let mut c = match carry(desc) {
        Ok(c) => c,
        Err(EOPNOTSUPP) => return Ok(None),
        Err(e) => return Err(e),
    };
    let handles = core::mem::take(&mut c.handles);
    Ok(Some(ToChild { entry: encoded(c), handles }))
}

/// The socket a process was started with, from what [`for_child`] made of
/// it in the process that started it, and the handles of its descriptor's
/// roles; `None` if it is not what it says.
pub fn from_parent(entry: &[u8], handles: Vec<Handle>) -> Option<Arc<Description>> {
    let mut c: Carried = decoded(entry)?;
    c.handles = handles;
    adopt(c)
}

/// Descriptors on their way to another process.
#[derive(Debug, Default)]
pub struct Rights {
    pub bytes: Vec<u8>,
    pub handles: Vec<Handle>,
}

impl Rights {
    /// What carries the descriptions `descs` to another process (nothing
    /// for none).
    pub fn of(descs: &[Arc<Description>]) -> Result<Rights, isize> {
        if descs.is_empty() {
            return Ok(Rights::default());
        }
        let carried = descs.iter().map(|d| carry(d)).collect::<Result<Vec<_>, _>>()?;
        let mut e = Encoder::new();
        carried.encode(&mut e);
        Ok(Rights { bytes: e.bytes, handles: e.handles })
    }

    /// The handles that carry the descriptors with a stream's bytes, which
    /// carry only handles: a memory object with what the others do not say
    /// first, then the others.
    pub fn into_handles(self) -> Result<Vec<Handle>, isize> {
        let len = self.bytes.len();
        let vmo = Vmo::create(len + 4).map_err(error::kernel)?;
        vmo.write(0, &(len as u32).to_le_bytes()).map_err(error::kernel)?;
        vmo.write(4, &self.bytes).map_err(error::kernel)?;
        let mut handles = Vec::with_capacity(self.handles.len() + 1);
        handles.push(vmo.into_handle());
        handles.extend(self.handles);
        Ok(handles)
    }

    /// The descriptors the handles that came with a stream's bytes carry
    /// ([`into_handles`](Self::into_handles)); `None` for what they do not.
    pub fn from_handles(handles: Vec<Handle>) -> Option<Rights> {
        let mut handles = handles.into_iter();
        let first = handles.next()?;
        if first.basic_info().ok()?.object_type != ObjectType::Vmo as u32 {
            return None;
        }
        let vmo = Vmo::from_handle(first);
        let mut len = [0u8; 4];
        vmo.read(0, &mut len).ok()?;
        let len = u32::from_le_bytes(len) as usize;
        // Never more than the object holds.
        if len + 4 > vmo.size().ok()? {
            return None;
        }
        let mut bytes = vec![0; len];
        vmo.read(4, &mut bytes).ok()?;
        Some(Rights { bytes, handles: handles.collect() })
    }

    /// The descriptions that came, made here again (those that cannot be
    /// are left out, their handles closed).
    pub fn descriptions(self) -> Vec<Arc<Description>> {
        let mut d = Decoder::new(&self.bytes, self.handles);
        let Ok(carried) = Vec::<Carried>::decode(&mut d) else { return Vec::new() };
        carried.into_iter().filter_map(adopt).collect()
    }
}

/// What carries `desc`.
fn carry(desc: &Description) -> Result<Carried, isize> {
    let carried = |kind, state: Vec<u8>, handles: Vec<Handle>| Carried {
        kind,
        flags: desc.flags(),
        state: Bytes(state),
        handles,
    };
    Ok(match &desc.object {
        Object::File(f) => carried(kind::FILE, Vec::new(), vec![f.duplicate()?.into_handle()]),
        Object::Dir(d) => carried(kind::DIR, d.path.as_bytes().to_vec(), Vec::new()),
        Object::Stream(s) => {
            carried(kind::STREAM, Vec::new(), vec![s.shared_handle(desc.readable(), desc.writable())?])
        }
        Object::Unix(u) => {
            let (travelling, handle) = u.travelling(desc.readable(), desc.writable())?;
            carried(kind::UNIX, encoded(travelling), handle.into_iter().collect())
        }
        Object::Inet(s) => {
            let (travelling, handles) = s.travelling()?;
            carried(kind::INET, encoded(travelling), handles)
        }
        Object::Null => carried(kind::NULL, Vec::new(), Vec::new()),
        Object::Log(_) => carried(kind::LOG, Vec::new(), Vec::new()),
        Object::Pidfd(p) => carried(kind::PIDFD, Vec::new(), vec![p.0.duplicate(None).map_err(error::kernel)?]),
        Object::Memfd(m) => {
            let (vmo, shared) = m.travelling(desc.writable())?;
            let state = encoded(MemfdState { size: shared.size, seals: shared.seals });
            carried(kind::MEMFD, state, vec![vmo.into_handle()])
        }
        Object::EventFd(_) | Object::TimerFd(_) | Object::Epoll(_) => return Err(EOPNOTSUPP),
    })
}

/// The description `c` carries, if its handle is what it says.
fn adopt(c: Carried) -> Option<Arc<Description>> {
    let mut handles = c.handles.into_iter();
    // The next handle, if it is an object of `kind`.
    let mut next =
        |kind: ObjectType| handles.next().filter(|h| h.basic_info().is_ok_and(|i| i.object_type == kind as u32));
    let object = match c.kind {
        kind::FILE => Object::File(VfsFile::new(Channel::from_handle(next(ObjectType::Channel)?))),
        kind::DIR => Object::Dir(Directory::new(String::from_utf8(c.state.0).ok()?)),
        kind::STREAM => Object::Stream(Stream::new(Socket::from_handle(next(ObjectType::Socket)?))),
        // Their handles' kinds are what they say, checked as they arrive.
        kind::UNIX => Object::Unix(Unix::arrived(decoded(&c.state.0)?, handles.next())?),
        kind::INET => Object::Inet(Inet::arrived(decoded(&c.state.0)?, handles.collect())?),
        kind::NULL => Object::Null,
        kind::LOG => Object::Log(Log::new()),
        kind::PIDFD => Object::Pidfd(Process::from_handle(next(ObjectType::Process)?)),
        kind::MEMFD => {
            let state: MemfdState = decoded(&c.state.0)?;
            let shared = Shared { size: state.size, seals: state.seals };
            Object::Memfd(Memfd::arrived(Vmo::from_handle(next(ObjectType::Vmo)?), shared)?)
        }
        _ => return None,
    };
    Some(Description::new(object, c.flags))
}
