// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Sockets, and the calls on them:
//!
//! * Internet sockets (`AF_INET`, `AF_INET6`): TCP and UDP, sockets of the
//!   network service ([`crate::inet`]);
//! * Unix domain sockets (`AF_UNIX`): stream, datagram and seqpacket
//!   sockets named by paths, and socket pairs ([`crate::unix`]).

use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::carry::Rights;
use crate::error::SysResult;
use crate::fd::{self, Description, Object};
use crate::inet::{Inet, Kind, Options, Recv};
use crate::io;
use crate::linux::errno::*;
use crate::linux::sock::*;
use crate::linux::{Cmsghdr, Linger, Mmsghdr, Msghdr, Timeval, Ucred, msg, o};
use crate::sockaddr::{self, Family, Raw, Target};
use crate::unix::{self, Unix};
use crate::{process, user, vfs};

/// `socket`: an Internet socket, TCP (`SOCK_STREAM`) or UDP (`SOCK_DGRAM`),
/// or a Unix domain socket of any of its three types.
pub fn socket(domain: usize, kind: usize, protocol: usize) -> SysResult {
    let flags = kind as u32 & (NONBLOCK | CLOEXEC);
    let sock_type = kind as u32 & !(NONBLOCK | CLOEXEC);
    let object = if domain == AF_UNIX as usize {
        Object::Unix(Unix::new(unix_kind(sock_type, protocol)?))
    } else {
        let family = u16::try_from(domain).ok().and_then(Family::of).ok_or(EAFNOSUPPORT)?;
        let kind = match (sock_type, protocol as u32) {
            (STREAM, 0 | IPPROTO_TCP) => Kind::Stream,
            (DGRAM, 0 | IPPROTO_UDP) => Kind::Datagram,
            // Raw sockets, ICMP's, SCTP's...
            (STREAM | DGRAM, _) | (3..=10, _) => return Err(EPROTONOSUPPORT),
            _ => return Err(EINVAL),
        };
        Object::Inet(Inet::new(kind, family))
    };
    let desc = Description::new(object, o::RDWR | (flags & NONBLOCK));
    fd::insert(desc, flags & CLOEXEC != 0, 0).map(|n| n as usize)
}

/// The kind of Unix domain socket a `socket` type and protocol ask for.
fn unix_kind(sock_type: u32, protocol: usize) -> Result<unix::Kind, isize> {
    // The protocol is the domain's own, or none.
    if protocol != 0 && protocol != AF_UNIX as usize {
        return Err(EPROTONOSUPPORT);
    }
    match unix::Kind::of(sock_type) {
        Some(k) => Ok(k),
        // Raw and reliable-datagram sockets are of other domains.
        None if (3..=10).contains(&sock_type) => Err(ESOCKTNOSUPPORT),
        None => Err(EINVAL),
    }
}

/// `socketpair(AF_UNIX, ...)`: a connected pair of Unix domain sockets.
///
/// # Safety
/// `out` must be where the program wants the two descriptors.
pub unsafe fn socketpair(domain: usize, kind: usize, protocol: usize, out: usize) -> SysResult {
    let flags = kind as u32 & (NONBLOCK | CLOEXEC);
    match u16::try_from(domain) {
        Ok(AF_UNIX) => {}
        // Internet sockets come in no pairs.
        Ok(AF_INET | AF_INET6) => return Err(EOPNOTSUPP),
        _ => return Err(EAFNOSUPPORT),
    }
    let kind = unix_kind(kind as u32 & !(NONBLOCK | CLOEXEC), protocol)?;
    if out == 0 {
        return Err(EFAULT);
    }
    let (a, b) = Unix::pair(kind)?;
    let status = o::RDWR | (flags & NONBLOCK);
    let (a, b) = (Description::new(Object::Unix(a), status), Description::new(Object::Unix(b), status));
    // SAFETY: per the caller.
    unsafe { io::install_pair(a, b, flags & CLOEXEC != 0, out)? };
    Ok(0)
}

/// The Internet socket a description is: `EOPNOTSUPP` for a Unix domain
/// socket, `ENOTSOCK` for what is no socket.
fn inet(desc: &Description) -> Result<&Inet, isize> {
    match &desc.object {
        Object::Inet(s) => Ok(s),
        Object::Unix(_) => Err(EOPNOTSUPP),
        _ => Err(ENOTSOCK),
    }
}

/// The address the program passed, for socket `s`.
///
/// # Safety
/// `addr` must be null or point at `len` readable bytes.
unsafe fn target(s: &Inet, addr: usize, len: usize) -> Result<Target, isize> {
    // SAFETY: per the caller.
    unsafe { sockaddr::read(addr, len as u32 as usize, s.family, s.options().v6only) }
}

/// The address a program passed to a Unix domain socket.
///
/// # Safety
/// `addr` must be null or point at `len` readable bytes.
unsafe fn unix_addr(addr: usize, len: usize) -> Result<unix::Addr, isize> {
    // SAFETY: per the caller.
    unsafe { unix::read_addr(addr, len as u32 as usize) }
}

/// `bind`.
///
/// # Safety
/// `addr` must point at `len` readable bytes.
pub unsafe fn bind(fd: i32, addr: usize, len: usize) -> SysResult {
    let desc = fd::get(fd)?;
    if let Object::Unix(s) = &desc.object {
        // SAFETY: per the caller.
        return s.bind(unsafe { unix_addr(addr, len)? });
    }
    let s = inet(&desc)?;
    // SAFETY: per the caller.
    match unsafe { target(s, addr, len)? } {
        Target::Addr(a) => s.bind(a),
        Target::Unspec => Err(EAFNOSUPPORT),
    }
}

/// `connect`: a datagram socket's `AF_UNSPEC` leaves it without a peer.
///
/// # Safety
/// `addr` must point at `len` readable bytes.
pub unsafe fn connect(fd: i32, addr: usize, len: usize) -> SysResult {
    let desc = fd::get(fd)?;
    if let Object::Unix(s) = &desc.object {
        // SAFETY: per the caller.
        let to = match unsafe { unix_addr(addr, len)? } {
            unix::Addr::Unspec => None,
            to => Some(to),
        };
        let r = s.connect(to, desc.nonblocking());
        // What epoll instances watch it for has changed.
        desc.changed();
        return r;
    }
    let s = inet(&desc)?;
    // SAFETY: per the caller.
    let peer = match unsafe { target(s, addr, len)? } {
        Target::Addr(a) => Some(a),
        Target::Unspec => None,
    };
    s.connect(peer, desc.nonblocking())
}

pub fn listen(fd: i32, backlog: i32) -> SysResult {
    let desc = fd::get(fd)?;
    if let Object::Unix(s) = &desc.object {
        let r = s.listen();
        desc.changed();
        return r;
    }
    inet(&desc)?.listen(backlog)
}

/// `accept4` (and `accept`): the next connection as a new descriptor, and
/// who it is from.
///
/// # Safety
/// `addr` must be null or a buffer of as many bytes as the `socklen_t` at
/// `addrlen` says.
pub unsafe fn accept4(fd: i32, addr: usize, addrlen: usize, flags: u32) -> SysResult {
    if flags & !(NONBLOCK | CLOEXEC) != 0 {
        return Err(EINVAL);
    }
    let desc = fd::get(fd)?;
    let status = o::RDWR | (flags & NONBLOCK);
    if let Object::Unix(s) = &desc.object {
        let conn = s.accept(desc.nonblocking())?;
        let peer = conn.peer_name().unwrap_or_default();
        let n = fd::insert(Description::new(Object::Unix(conn), status), flags & CLOEXEC != 0, 0)?;
        // SAFETY: per the caller.
        if let Err(e) = unsafe { unix::store_name(&peer, addr, addrlen) } {
            let _ = fd::remove(n);
            return Err(e);
        }
        return Ok(n as usize);
    }
    let s = inet(&desc)?;
    let conn = s.accept(desc.nonblocking())?;
    let peer = conn.peer().ok().and_then(|p| Raw::of(p, s.family));
    let n = fd::insert(Description::new(Object::Inet(conn), status), flags & CLOEXEC != 0, 0)?;
    if let Some(raw) = peer
        // SAFETY: per the caller.
        && let Err(e) = unsafe { raw.store(addr, addrlen) }
    {
        let _ = fd::remove(n);
        return Err(e);
    }
    Ok(n as usize)
}

/// `getsockname` (`peer` false) and `getpeername`.
///
/// # Safety
/// As [`Raw::store`].
pub unsafe fn name(fd: i32, addr: usize, addrlen: usize, peer: bool) -> SysResult {
    let desc = fd::get(fd)?;
    let s = match &desc.object {
        Object::Inet(s) => s,
        Object::Unix(s) => {
            let name = if peer { s.peer_name()? } else { s.local() };
            if addr == 0 {
                return Err(EFAULT);
            }
            // SAFETY: per the caller.
            unsafe { unix::store_name(&name, addr, addrlen)? };
            return Ok(0);
        }
        _ => return Err(ENOTSOCK),
    };
    let a = if peer { s.peer()? } else { s.local() };
    if addr == 0 {
        return Err(EFAULT);
    }
    // A socket only has addresses of its family (`Inet` drops the rest).
    let raw = Raw::of(a, s.family).ok_or(EAFNOSUPPORT)?;
    // SAFETY: per the caller.
    unsafe { raw.store(addr, addrlen)? };
    Ok(0)
}

/// How a receive goes, from its flags.
fn how(desc: &Description, flags: u32) -> Result<Recv, isize> {
    // There is no out-of-band data.
    if flags & msg::OOB != 0 {
        return Err(EINVAL);
    }
    Ok(Recv {
        nonblock: desc.nonblocking() || flags & msg::DONTWAIT != 0,
        peek: flags & msg::PEEK != 0,
        waitall: flags & msg::WAITALL != 0,
        trunc: flags & msg::TRUNC != 0,
    })
}

/// Stores who sent what a Unix domain socket received: a datagram's
/// sender's name (none for one without a name, as on Linux), nothing for
/// a stream's bytes.
///
/// # Safety
/// As [`unix::store_name`].
unsafe fn store_from(from: Option<&[u8]>, addr: usize, addrlen: usize) -> Result<(), isize> {
    match from {
        // SAFETY: per the caller.
        Some(name) if !name.is_empty() => unsafe { unix::store_name(name, addr, addrlen) },
        // SAFETY: per the caller.
        _ if addr != 0 => unsafe { user::write(addrlen, 0u32) },
        _ => Ok(()),
    }
}

/// Sends `data` on an Internet socket: to `name` (`namelen` bytes), if
/// given, on a datagram socket; a stream has its peer, and passes over an
/// address, as on Linux.
///
/// # Safety
/// `name` must be null or point at `namelen` readable bytes.
unsafe fn send_inet(desc: &Description, s: &Inet, data: &[u8], flags: u32, name: usize, namelen: usize) -> SysResult {
    if flags & msg::OOB != 0 {
        return Err(EOPNOTSUPP);
    }
    let dest = match (s.kind, name) {
        // SAFETY: per the caller.
        (Kind::Datagram, n) if n != 0 => match unsafe { target(s, n, namelen)? } {
            Target::Addr(a) if a.port() == 0 => return Err(EINVAL),
            Target::Addr(a) => Some(a),
            Target::Unspec => None,
        },
        _ => None,
    };
    let nonblock = desc.nonblocking() || flags & msg::DONTWAIT != 0;
    s.send(data, dest, nonblock, flags & msg::NOSIGNAL != 0)
}

/// Sends `data` on a Unix domain socket, to `name` (`namelen` bytes) if
/// given, with the descriptors `fds`.
///
/// # Safety
/// `name` must be null or point at `namelen` readable bytes.
unsafe fn send_unix(
    desc: &Description,
    s: &Unix,
    data: &[u8],
    flags: u32,
    name: usize,
    namelen: usize,
    fds: &[Arc<Description>],
) -> SysResult {
    if flags & msg::OOB != 0 {
        return Err(EOPNOTSUPP);
    }
    let dest = match name {
        0 => None,
        // SAFETY: per the caller.
        _ => match unsafe { unix_addr(name, namelen)? } {
            unix::Addr::Unspec | unix::Addr::Unnamed => return Err(EINVAL),
            to => Some(to),
        },
    };
    let nonblock = desc.nonblocking() || flags & msg::DONTWAIT != 0;
    s.send(data, dest, fds, nonblock, flags & msg::NOSIGNAL != 0)
}

/// The bytes an ancillary message takes with its padding (`CMSG_ALIGN`).
fn cmsg_align(len: usize) -> usize {
    len.next_multiple_of(size_of::<usize>())
}

/// Descriptors one message carries at most: the handles a channel message
/// takes.
const MAX_RIGHTS: usize = abi::CHANNEL_MAX_HANDLES;

/// The descriptors a message's ancillary data sends (`SCM_RIGHTS`, in
/// order). Credentials (`SCM_CREDENTIALS`) are checked: a program says who
/// it is and no more (the receiver is told who sent it anyway).
/// Ancillary data of other levels is passed over, as Linux's Unix domain
/// sockets do.
///
/// # Safety
/// `control` must point at `len` readable bytes.
unsafe fn ancillary(control: usize, len: usize) -> Result<Vec<Arc<Description>>, isize> {
    const HEADER: usize = size_of::<Cmsghdr>();
    let mut fds = Vec::new();
    let mut at = 0;
    while at + HEADER <= len {
        // SAFETY: per the caller.
        let h: Cmsghdr = unsafe { user::read(control + at)? };
        if h.len < HEADER || h.len > len - at {
            return Err(EINVAL);
        }
        let data = control + at + HEADER;
        let n = h.len - HEADER;
        match (h.level as u32, h.kind as u32) {
            (SOL_SOCKET, SCM_RIGHTS) => {
                for i in 0..n / size_of::<i32>() {
                    // SAFETY: per the caller, within the message.
                    let fd: i32 = unsafe { user::read(data + i * size_of::<i32>())? };
                    fds.push(fd::get(fd)?);
                }
            }
            (SOL_SOCKET, SCM_CREDENTIALS) => {
                if n < size_of::<Ucred>() {
                    return Err(EINVAL);
                }
                // SAFETY: as above.
                let c: Ucred = unsafe { user::read(data)? };
                if c.pid != process::pid() || c.uid != vfs::UID || c.gid != vfs::UID {
                    return Err(EPERM);
                }
            }
            (SOL_SOCKET, _) => return Err(EINVAL),
            _ => {}
        }
        at += cmsg_align(h.len);
    }
    if fds.len() > MAX_RIGHTS {
        return Err(ETOOMANYREFS);
    }
    Ok(fds)
}

/// Writes an ancillary message (`level`, `kind`, the bytes of `data`) at
/// `*at` of the program's buffer of `room` bytes at `control`, if it fits:
/// whether it did.
///
/// # Safety
/// `control` must be a buffer of `room` bytes of the program's.
unsafe fn put_cmsg(control: usize, room: usize, at: &mut usize, kind: u32, data: &[u8]) -> Result<bool, isize> {
    let len = size_of::<Cmsghdr>() + data.len();
    if control == 0 || room < *at + len {
        return Ok(false);
    }
    let h = Cmsghdr { len, level: SOL_SOCKET as i32, kind: kind as i32 };
    // SAFETY: per the caller, there is room for both.
    unsafe {
        user::write(control + *at, h)?;
        user::slice_mut(control + *at + size_of::<Cmsghdr>(), data.len())?.copy_from_slice(data);
    }
    *at = (*at + cmsg_align(len)).min(room);
    Ok(true)
}

/// Writes what came with a message into the program's buffer for
/// ancillary data (`control`, `room` bytes): who sent it
/// (`SCM_CREDENTIALS`), if asked, then the descriptors that came
/// (`SCM_RIGHTS`), installed as far as they fit (`cloexec`: close on
/// `exec`), those that do not closed. Returns the bytes it took and
/// whether something did not fit (`MSG_CTRUNC`).
///
/// # Safety
/// `control` must be null or a buffer of `room` bytes of the program's.
unsafe fn store_ancillary(
    control: usize,
    room: usize,
    creds: Option<Ucred>,
    rights: Option<Rights>,
    cloexec: bool,
) -> Result<(usize, bool), isize> {
    let mut at = 0;
    let mut cut = false;
    if let Some(c) = creds {
        // SAFETY: `c` is plain old data of its size.
        let bytes = unsafe { core::slice::from_raw_parts(&c as *const Ucred as *const u8, size_of::<Ucred>()) };
        // SAFETY: per the caller.
        cut |= !unsafe { put_cmsg(control, room, &mut at, SCM_CREDENTIALS, bytes)? };
    }
    let descs = rights.map(Rights::descriptions).unwrap_or_default();
    if !descs.is_empty() {
        let fit = (room.saturating_sub(at + size_of::<Cmsghdr>()) / size_of::<i32>()).min(descs.len());
        let fit = if control == 0 { 0 } else { fit };
        cut |= fit < descs.len();
        let mut installed: Vec<i32> = Vec::with_capacity(fit);
        for d in descs.into_iter().take(fit) {
            match fd::insert(d, cloexec, 0) {
                Ok(n) => installed.push(n),
                // No descriptor free: the rest are lost, as on Linux.
                Err(_) => {
                    cut = true;
                    break;
                }
            }
        }
        if !installed.is_empty() {
            let bytes: Vec<u8> = installed.iter().flat_map(|n| n.to_ne_bytes()).collect();
            // SAFETY: per the caller; it fits, as counted above.
            if let Err(e) = unsafe { put_cmsg(control, room, &mut at, SCM_RIGHTS, &bytes) } {
                for n in installed {
                    let _ = fd::remove(n);
                }
                return Err(e);
            }
        }
    }
    Ok((at, cut))
}

/// `sendto` (and `send`).
///
/// # Safety
/// `buf` must be a buffer of `len` bytes of the program's; `addr`, if not
/// null, point at `addrlen` bytes.
pub unsafe fn sendto(fd: i32, buf: usize, len: usize, flags: u32, addr: usize, addrlen: usize) -> SysResult {
    let desc = fd::get(fd)?;
    // SAFETY: per the caller.
    let data = unsafe { user::slice(buf, len)? };
    match &desc.object {
        // SAFETY: per the caller.
        Object::Inet(s) => unsafe { send_inet(&desc, s, data, flags, addr, addrlen) },
        // SAFETY: per the caller.
        Object::Unix(s) => unsafe { send_unix(&desc, s, data, flags, addr, addrlen, &[]) },
        _ => Err(ENOTSOCK),
    }
}

/// `recvfrom` (and `recv`): who sent it, from a datagram socket; a stream's
/// peer has no address there (0 bytes stored).
///
/// # Safety
/// `buf` must be a buffer of `len` bytes of the program's; `addr` null or a
/// buffer of as many bytes as the `socklen_t` at `addrlen` says.
pub unsafe fn recvfrom(fd: i32, buf: usize, len: usize, flags: u32, addr: usize, addrlen: usize) -> SysResult {
    let desc = fd::get(fd)?;
    // SAFETY: per the caller.
    let buf = unsafe { user::slice_mut(buf, len)? };
    match &desc.object {
        Object::Inet(s) => {
            let r = s.recv(&mut [buf], how(&desc, flags)?)?;
            match r.from.and_then(|f| Raw::of(f, s.family)) {
                // SAFETY: per the caller.
                Some(raw) => unsafe { raw.store(addr, addrlen)? },
                // SAFETY: per the caller.
                None if addr != 0 => unsafe { user::write(addrlen, 0u32)? },
                None => {}
            }
            Ok(r.len)
        }
        Object::Unix(s) => {
            let r = s.recv(&mut [buf], how(&desc, flags)?)?;
            // SAFETY: per the caller.
            unsafe { store_from(r.from.as_deref(), addr, addrlen)? };
            Ok(r.len)
        }
        _ => Err(ENOTSOCK),
    }
}

/// `sendmsg`: the buffers in one piece, to the message's address on a
/// datagram socket; on a Unix domain socket, with the descriptors its
/// ancillary data names (`SCM_RIGHTS`). Internet sockets take no ancillary
/// data.
///
/// # Safety
/// `message` must be the program's `struct msghdr`.
pub unsafe fn sendmsg(fd: i32, message: usize, flags: u32) -> SysResult {
    let desc = fd::get(fd)?;
    // SAFETY: per the caller.
    let m: Msghdr = unsafe { user::read(message)? };
    if !matches!(desc.object, Object::Inet(_) | Object::Unix(_)) {
        return Err(ENOTSOCK);
    }
    // SAFETY: the message's buffers are the program's.
    let data = unsafe { io::gather(&io::iovecs(m.iov, m.iovlen)?)? };
    match &desc.object {
        Object::Inet(_) if m.controllen != 0 => Err(EOPNOTSUPP),
        // SAFETY: the message's address is the program's.
        Object::Inet(s) => unsafe { send_inet(&desc, s, &data, flags, m.name, m.namelen as usize) },
        Object::Unix(s) => {
            // SAFETY: the message's ancillary data is the program's.
            let fds = if m.control != 0 { unsafe { ancillary(m.control, m.controllen)? } } else { Vec::new() };
            // SAFETY: the message's address is the program's.
            unsafe { send_unix(&desc, s, &data, flags, m.name, m.namelen as usize, &fds) }
        }
        _ => Err(ENOTSOCK),
    }
}

/// `recvmsg`: into the buffers in turn, as `readv`, with who sent it from a
/// datagram socket (and `MSG_TRUNC` if it was longer than the buffers); a
/// Unix domain socket's descriptors that came with it, and who sent it
/// with `SO_PASSCRED`, as ancillary data (`MSG_CTRUNC` if that did not
/// fit).
///
/// # Safety
/// `message` must be the program's `struct msghdr`.
pub unsafe fn recvmsg(fd: i32, message: usize, flags: u32) -> SysResult {
    let desc = fd::get(fd)?;
    // SAFETY: per the caller.
    let mut m: Msghdr = unsafe { user::read(message)? };
    // SAFETY: the message's buffers are the program's.
    let vecs = unsafe { io::iovecs(m.iov, m.iovlen)? };
    // SAFETY: as above.
    let mut bufs: Vec<&mut [u8]> =
        vecs.iter().map(|v| unsafe { user::slice_mut(v.base as usize, v.len) }).collect::<Result<_, _>>()?;
    // The name's room is in the message, not behind a pointer.
    let mut room = m.namelen;
    let room_at = &mut room as *mut u32 as usize;
    let (got, truncated, (control, cut)) = match &desc.object {
        Object::Inet(s) => {
            let r = s.recv(&mut bufs, how(&desc, flags)?)?;
            m.namelen = 0;
            if let Some(raw) = r.from.and_then(|f| Raw::of(f, s.family)) {
                // SAFETY: the message's name has `namelen` bytes of room.
                unsafe { raw.store(m.name, room_at)? };
                m.namelen = if m.name != 0 { room } else { 0 };
            }
            (r.len, r.truncated, (0, false))
        }
        Object::Unix(s) => {
            let r = s.recv(&mut bufs, how(&desc, flags)?)?;
            // SAFETY: as above.
            unsafe { store_from(r.from.as_deref(), m.name, room_at)? };
            m.namelen = if m.name != 0 { room } else { 0 };
            let creds = s.options().passcred.then_some(Ucred { pid: r.pid, uid: vfs::UID, gid: vfs::UID });
            let cloexec = flags & msg::CMSG_CLOEXEC != 0;
            // SAFETY: the message's buffer for ancillary data is the
            // program's.
            let control = unsafe { store_ancillary(m.control, m.controllen, creds, r.rights, cloexec)? };
            (r.len, r.truncated, control)
        }
        _ => return Err(ENOTSOCK),
    };
    m.flags = if truncated { msg::TRUNC as i32 } else { 0 } | if cut { msg::CTRUNC as i32 } else { 0 };
    m.controllen = control;
    // SAFETY: per the caller.
    unsafe { user::write(message, m)? };
    Ok(got)
}

/// `sendmmsg`: [`sendmsg`] for each message in turn, until one fails (which
/// fails the call only if it is the first). Returns how many went.
///
/// # Safety
/// `messages` must be the program's `count` `struct mmsghdr`s.
pub unsafe fn sendmmsg(fd: i32, messages: usize, count: usize, flags: u32) -> SysResult {
    let mut sent = 0;
    for i in 0..count.min(1024) {
        let at = messages + i * size_of::<Mmsghdr>();
        // SAFETY: per the caller.
        match unsafe { sendmsg(fd, at, flags) } {
            Ok(n) => {
                // SAFETY: as above.
                unsafe { user::write(at + core::mem::offset_of!(Mmsghdr, len), n as u32)? };
                sent += 1;
            }
            Err(e) if sent == 0 => return Err(e),
            Err(_) => break,
        }
    }
    Ok(sent)
}

/// `recvmmsg`: [`recvmsg`] for each message in turn, until there is no
/// more without waiting (with `MSG_WAITFORONE`, after the first), one fails
/// (which fails the call only if it is the first) or the time-out (checked
/// after each message, as on Linux) passes. Returns how many came.
///
/// # Safety
/// `messages` must be the program's `count` `struct mmsghdr`s.
pub unsafe fn recvmmsg(fd: i32, messages: usize, count: usize, flags: u32, deadline: Option<u64>) -> SysResult {
    let mut got = 0;
    for i in 0..count.min(1024) {
        let at = messages + i * size_of::<Mmsghdr>();
        let flags = if got > 0 && flags & msg::WAITFORONE != 0 { flags | msg::DONTWAIT } else { flags };
        // SAFETY: per the caller.
        match unsafe { recvmsg(fd, at, flags & !msg::WAITFORONE) } {
            Ok(n) => {
                // SAFETY: as above.
                unsafe { user::write(at + core::mem::offset_of!(Mmsghdr, len), n as u32)? };
                got += 1;
            }
            Err(e) if got == 0 => return Err(e),
            Err(_) => break,
        }
        if deadline.is_some_and(|d| crate::time::monotonic_ns() >= d) {
            break;
        }
    }
    Ok(got)
}

/// `shutdown`. A Unix domain stream socket's peer reads to the end of what
/// was sent once this end writes no more (`SHUT_WR`, `SHUT_RDWR`);
/// stopping reading (`SHUT_RD`) makes reads end once nothing is left.
pub fn shutdown(fd: i32, how: u32) -> SysResult {
    let (read, write) = match how {
        SHUT_RD => (true, false),
        SHUT_WR => (false, true),
        SHUT_RDWR => (true, true),
        _ => return Err(EINVAL),
    };
    let desc = fd::get(fd)?;
    match &desc.object {
        Object::Inet(s) => s.shutdown(read, write),
        Object::Unix(s) => {
            let r = s.shutdown(read, write);
            desc.changed();
            r
        }
        _ => Err(ENOTSOCK),
    }
}

/// What the network service lets a socket have in its send buffer: a
/// connection's (the stack's buffer) and a datagram socket's (its window).
const SEND_BUFFER: i32 = 64 * 1024;

/// An `int` option's value (at least the `int`'s bytes).
///
/// # Safety
/// `value` must point at `len` readable bytes.
unsafe fn int_value(value: usize, len: usize) -> Result<i32, isize> {
    if len < size_of::<i32>() {
        return Err(EINVAL);
    }
    // SAFETY: per the caller.
    unsafe { user::read(value) }
}

/// A `struct timeval` time-out as nanoseconds; `None` for none (0).
///
/// # Safety
/// `value` must point at `len` readable bytes.
unsafe fn timeout_value(value: usize, len: usize) -> Result<Option<u64>, isize> {
    if len < size_of::<Timeval>() {
        return Err(EINVAL);
    }
    // SAFETY: per the caller.
    let tv: Timeval = unsafe { user::read(value)? };
    if tv.sec < 0 || !(0..1_000_000).contains(&tv.usec) {
        return Err(EDOM);
    }
    let ns = (tv.sec as u64).saturating_mul(1_000_000_000).saturating_add(tv.usec as u64 * 1000);
    Ok((ns > 0).then_some(ns))
}

/// `setsockopt`. The options of the socket level, TCP's and IP's that
/// programs set are kept; those the network service has take effect
/// (`TCP_NODELAY`; `SO_KEEPALIVE`, with `TCP_KEEPIDLE` as the interval, on
/// a connection made after; `SO_LINGER` with no time); the time-outs are
/// the layer's. The sizes of buffers are the service's, and hints about
/// segments (`TCP_CORK`, `TCP_QUICKACK`...) are taken as given. Others,
/// multicast's among them, are not there (`ENOPROTOOPT`).
///
/// # Safety
/// `value` must point at `len` readable bytes.
pub unsafe fn setsockopt(fd: i32, level: u32, name: u32, value: usize, len: usize) -> SysResult {
    const TCP_CORK: u32 = linux_raw_sys::net::TCP_CORK;
    const TCP_DEFER_ACCEPT: u32 = linux_raw_sys::net::TCP_DEFER_ACCEPT;
    const TCP_QUICKACK: u32 = linux_raw_sys::net::TCP_QUICKACK;
    const TCP_USER_TIMEOUT: u32 = linux_raw_sys::net::TCP_USER_TIMEOUT;
    let desc = fd::get(fd)?;
    let len = len as u32 as usize;
    let s = match &desc.object {
        Object::Inet(s) => s,
        // SAFETY: per the caller.
        Object::Unix(s) => return unsafe { setsockopt_unix(s, level, name, value, len) },
        _ => return Err(ENOTSOCK),
    };
    // SAFETY: per the caller.
    let int = || unsafe { int_value(value, len) };
    let flag = |set: fn(&mut Options, bool)| -> SysResult {
        let on = int()? != 0;
        s.set_options(|o| set(o, on))
    };
    let stream = s.kind == Kind::Stream;
    match (level, name) {
        (SOL_SOCKET, SO_REUSEADDR) => flag(|o, on| o.reuse_addr = on),
        (SOL_SOCKET, SO_REUSEPORT) => flag(|o, on| o.reuse_port = on),
        (SOL_SOCKET, SO_KEEPALIVE) => flag(|o, on| o.keepalive = on),
        (SOL_SOCKET, SO_BROADCAST) => flag(|o, on| o.broadcast = on),
        (SOL_SOCKET, SO_OOBINLINE) => flag(|o, on| o.oob_inline = on),
        (SOL_SOCKET, SO_SNDBUF | SO_RCVBUF | SO_RCVLOWAT | SO_SNDLOWAT) => int().map(|_| 0),
        (SOL_SOCKET, SO_LINGER) => {
            if len < size_of::<Linger>() {
                return Err(EINVAL);
            }
            // SAFETY: per the caller, and there is room for a `linger`.
            let l: Linger = unsafe { user::read(value)? };
            s.set_options(|o| o.linger = (l.onoff != 0).then_some(l.linger.max(0) as u32))
        }
        (SOL_SOCKET, SO_RCVTIMEO) => {
            // SAFETY: per the caller.
            let t = unsafe { timeout_value(value, len)? };
            s.set_options(|o| o.recv_timeout = t)
        }
        (SOL_SOCKET, SO_SNDTIMEO) => {
            // SAFETY: per the caller.
            let t = unsafe { timeout_value(value, len)? };
            s.set_options(|o| o.send_timeout = t)
        }
        (IPPROTO_TCP, TCP_NODELAY) if stream => flag(|o, on| o.nodelay = on),
        (IPPROTO_TCP, TCP_KEEPIDLE | TCP_KEEPINTVL | TCP_KEEPCNT) if stream => {
            let v = u32::try_from(int()?).ok().filter(|v| (1..=32767).contains(v)).ok_or(EINVAL)?;
            s.set_options(|o| match name {
                TCP_KEEPIDLE => o.keep_idle = v,
                TCP_KEEPINTVL => o.keep_interval = v,
                _ => o.keep_count = v,
            })
        }
        (IPPROTO_TCP, TCP_MAXSEG | TCP_CORK | TCP_QUICKACK | TCP_DEFER_ACCEPT | TCP_USER_TIMEOUT) if stream => {
            int().map(|_| 0)
        }
        (IPPROTO_IP, IP_TTL) => {
            let ttl = match int()? {
                -1 => 64,
                t @ 1..=255 => t as u32,
                _ => return Err(EINVAL),
            };
            s.set_options(|o| o.ttl = ttl)
        }
        (IPPROTO_IP, IP_TOS) => {
            let tos = int()? as u32 & 0xff;
            s.set_options(|o| o.tos = tos)
        }
        (IPPROTO_IPV6, IPV6_V6ONLY) if s.family == Family::V6 => flag(|o, on| o.v6only = on),
        (IPPROTO_IPV6, IPV6_UNICAST_HOPS) if s.family == Family::V6 => {
            let hops = int()?;
            if !(-1..=255).contains(&hops) {
                return Err(EINVAL);
            }
            s.set_options(|o| o.hops = hops)
        }
        _ => Err(ENOPROTOOPT),
    }
}

/// Stores an option's value: as much of it as the program has room for (the
/// `socklen_t` at `len`), and its length there.
///
/// # Safety
/// `len` must be the program's `socklen_t`, `value` a buffer of that many
/// bytes.
unsafe fn store_value<T: Copy>(value: usize, len: usize, v: T) -> SysResult {
    // SAFETY: per the caller.
    let room = unsafe { user::read::<u32>(len)? } as usize;
    let n = room.min(size_of::<T>());
    // SAFETY: `v` is plain old data of `size_of::<T>()` bytes.
    let bytes = unsafe { core::slice::from_raw_parts(&v as *const T as *const u8, size_of::<T>()) };
    // SAFETY: per the caller.
    unsafe { user::slice_mut(value, n)? }.copy_from_slice(&bytes[..n]);
    // SAFETY: per the caller.
    unsafe { user::write(len, n as u32)? };
    Ok(0)
}

fn timeval(ns: Option<u64>) -> Timeval {
    let ns = ns.unwrap_or(0);
    Timeval { sec: (ns / 1_000_000_000) as i64, usec: (ns % 1_000_000_000 / 1000) as i64 }
}

/// `getsockopt`: the options [`setsockopt`] keeps, what the socket is
/// (`SO_TYPE`, `SO_DOMAIN`, `SO_PROTOCOL`, `SO_ACCEPTCONN`), its pending
/// error (`SO_ERROR`, now reported) and the service's buffer sizes.
///
/// # Safety
/// `len` must be the program's `socklen_t`, `value` a buffer of that many
/// bytes.
pub unsafe fn getsockopt(fd: i32, level: u32, name: u32, value: usize, len: usize) -> SysResult {
    let desc = fd::get(fd)?;
    // SAFETY: per the caller.
    let int = |v: i32| unsafe { store_value(value, len, v) };
    let s = match &desc.object {
        Object::Inet(s) => s,
        // SAFETY: per the caller.
        Object::Unix(s) => return unsafe { getsockopt_unix(s, level, name, value, len) },
        _ => return Err(ENOTSOCK),
    };
    let o = s.options();
    let stream = s.kind == Kind::Stream;
    match (level, name) {
        (SOL_SOCKET, SO_TYPE) => int(s.kind.sock_type() as i32),
        (SOL_SOCKET, SO_DOMAIN) => int(s.family.domain() as i32),
        (SOL_SOCKET, SO_PROTOCOL) => int(s.kind.protocol() as i32),
        (SOL_SOCKET, SO_ACCEPTCONN) => int(s.listening() as i32),
        (SOL_SOCKET, SO_ERROR) => int(s.take_error() as i32),
        (SOL_SOCKET, SO_REUSEADDR) => int(o.reuse_addr as i32),
        (SOL_SOCKET, SO_REUSEPORT) => int(o.reuse_port as i32),
        (SOL_SOCKET, SO_KEEPALIVE) => int(o.keepalive as i32),
        (SOL_SOCKET, SO_BROADCAST) => int(o.broadcast as i32),
        (SOL_SOCKET, SO_OOBINLINE) => int(o.oob_inline as i32),
        // What the service keeps for a socket.
        (SOL_SOCKET, SO_SNDBUF) => int(SEND_BUFFER),
        (SOL_SOCKET, SO_RCVBUF) => int(proto::net::RECEIVE_WINDOW as i32),
        (SOL_SOCKET, SO_RCVLOWAT | SO_SNDLOWAT) => int(1),
        (SOL_SOCKET, SO_LINGER) => {
            let l = Linger { onoff: o.linger.is_some() as i32, linger: o.linger.unwrap_or(0) as i32 };
            // SAFETY: per the caller.
            unsafe { store_value(value, len, l) }
        }
        // SAFETY: per the caller.
        (SOL_SOCKET, SO_RCVTIMEO) => unsafe { store_value(value, len, timeval(o.recv_timeout)) },
        // SAFETY: per the caller.
        (SOL_SOCKET, SO_SNDTIMEO) => unsafe { store_value(value, len, timeval(o.send_timeout)) },
        (IPPROTO_TCP, TCP_NODELAY) if stream => int(o.nodelay as i32),
        (IPPROTO_TCP, TCP_KEEPIDLE) if stream => int(o.keep_idle as i32),
        (IPPROTO_TCP, TCP_KEEPINTVL) if stream => int(o.keep_interval as i32),
        (IPPROTO_TCP, TCP_KEEPCNT) if stream => int(o.keep_count as i32),
        (IPPROTO_TCP, TCP_MAXSEG) if stream => int(1460),
        (IPPROTO_IP, IP_TTL) => int(o.ttl as i32),
        (IPPROTO_IP, IP_TOS) => int(o.tos as i32),
        (IPPROTO_IPV6, IPV6_V6ONLY) if s.family == Family::V6 => int(o.v6only as i32),
        (IPPROTO_IPV6, IPV6_UNICAST_HOPS) if s.family == Family::V6 => int(if o.hops < 0 { 64 } else { o.hops }),
        _ => Err(ENOPROTOOPT),
    }
}

/// `setsockopt` on a Unix domain socket: the time-outs and `SO_PASSCRED`,
/// which change what it does; the socket level's others are kept to be
/// read back, as Linux takes them from any socket.
///
/// # Safety
/// `value` must point at `len` readable bytes.
unsafe fn setsockopt_unix(s: &Unix, level: u32, name: u32, value: usize, len: usize) -> SysResult {
    // SAFETY: per the caller.
    let int = || unsafe { int_value(value, len) };
    let flag = |set: fn(&mut unix::Options, bool)| -> SysResult {
        let on = int()? != 0;
        s.set_options(|o| set(o, on))
    };
    match (level, name) {
        (SOL_SOCKET, SO_PASSCRED) => flag(|o, on| o.passcred = on),
        (SOL_SOCKET, SO_REUSEADDR) => flag(|o, on| o.reuse_addr = on),
        (SOL_SOCKET, SO_KEEPALIVE) => flag(|o, on| o.keepalive = on),
        (SOL_SOCKET, SO_BROADCAST) => flag(|o, on| o.broadcast = on),
        (SOL_SOCKET, SO_OOBINLINE) => flag(|o, on| o.oob_inline = on),
        (SOL_SOCKET, SO_SNDBUF | SO_RCVBUF | SO_RCVLOWAT | SO_SNDLOWAT) => int().map(|_| 0),
        (SOL_SOCKET, SO_LINGER) => {
            if len < size_of::<Linger>() {
                return Err(EINVAL);
            }
            // SAFETY: per the caller, and there is room for a `linger`.
            let l: Linger = unsafe { user::read(value)? };
            s.set_options(|o| o.linger = (l.onoff != 0).then_some(l.linger.max(0) as u32))
        }
        (SOL_SOCKET, SO_RCVTIMEO) => {
            // SAFETY: per the caller.
            let t = unsafe { timeout_value(value, len)? };
            s.set_options(|o| o.recv_timeout = t)
        }
        (SOL_SOCKET, SO_SNDTIMEO) => {
            // SAFETY: per the caller.
            let t = unsafe { timeout_value(value, len)? };
            s.set_options(|o| o.send_timeout = t)
        }
        _ => Err(ENOPROTOOPT),
    }
}

/// `getsockopt` on a Unix domain socket: what [`setsockopt_unix`] keeps,
/// what the socket is, and who is at the other end (`SO_PEERCRED`).
///
/// # Safety
/// `len` must be the program's `socklen_t`, `value` a buffer of that many
/// bytes.
unsafe fn getsockopt_unix(s: &Unix, level: u32, name: u32, value: usize, len: usize) -> SysResult {
    // SAFETY: per the caller.
    let int = |v: i32| unsafe { store_value(value, len, v) };
    let o = s.options();
    match (level, name) {
        (SOL_SOCKET, SO_TYPE) => int(s.kind.sock_type() as i32),
        (SOL_SOCKET, SO_DOMAIN) => int(AF_UNIX as i32),
        (SOL_SOCKET, SO_PROTOCOL | SO_ERROR) => int(0),
        (SOL_SOCKET, SO_ACCEPTCONN) => int(s.listening() as i32),
        (SOL_SOCKET, SO_PASSCRED) => int(o.passcred as i32),
        (SOL_SOCKET, SO_REUSEADDR) => int(o.reuse_addr as i32),
        (SOL_SOCKET, SO_KEEPALIVE) => int(o.keepalive as i32),
        (SOL_SOCKET, SO_BROADCAST) => int(o.broadcast as i32),
        (SOL_SOCKET, SO_OOBINLINE) => int(o.oob_inline as i32),
        (SOL_SOCKET, SO_SNDBUF | SO_RCVBUF) => int(s.buffer_size() as i32),
        (SOL_SOCKET, SO_RCVLOWAT | SO_SNDLOWAT) => int(1),
        (SOL_SOCKET, SO_LINGER) => {
            let l = Linger { onoff: o.linger.is_some() as i32, linger: o.linger.unwrap_or(0) as i32 };
            // SAFETY: per the caller.
            unsafe { store_value(value, len, l) }
        }
        // SAFETY: per the caller.
        (SOL_SOCKET, SO_RCVTIMEO) => unsafe { store_value(value, len, timeval(o.recv_timeout)) },
        // SAFETY: per the caller.
        (SOL_SOCKET, SO_SNDTIMEO) => unsafe { store_value(value, len, timeval(o.send_timeout)) },
        // SAFETY: per the caller.
        (SOL_SOCKET, SO_PEERCRED) => unsafe { store_value(value, len, s.peer_cred()) },
        _ => Err(ENOPROTOOPT),
    }
}
