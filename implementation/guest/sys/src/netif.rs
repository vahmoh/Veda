// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Network interfaces, for the drivers that carry an interface's frames to
//! Veda: switching one on and off, and a raw packet socket on one (which
//! `rustix` does not have: their addresses, options and requests are
//! here, as Linux's headers define them).

use std::fs::File;
use std::io;
use std::num::NonZeroU32;

use rustix::ioctl::{Opcode, Updater, ioctl};
use rustix::net::addr::{SocketAddrArg, SocketAddrLen, SocketAddrOpaque};
use rustix::net::{AddressFamily, Protocol, SocketFlags, SocketType, bind, socket_with, sockopt};

use crate::setsockopt;

/// `ETH_P_ALL`, in network byte order.
const ALL_FRAMES: u16 = 0x0003u16.to_be();
const SOL_PACKET: i32 = 263;
const PACKET_ADD_MEMBERSHIP: i32 = 1;
const PACKET_MR_ALLMULTI: u16 = 2;
const PACKET_IGNORE_OUTGOING: i32 = 23;
const SIOCGIFFLAGS: Opcode = 0x8913;
const SIOCSIFFLAGS: Opcode = 0x8914;
const IFF_UP: i16 = 1;

/// `struct ifreq`, with the flags of its union (`ifr_flags`).
#[repr(C)]
struct IfReq {
    name: [u8; 16],
    flags: i16,
    _rest: [u8; 22],
}

/// A packet socket's address (`struct sockaddr_ll`).
#[repr(C)]
struct LinkAddr {
    family: u16,
    protocol: u16,
    ifindex: i32,
    hatype: u16,
    pkttype: u8,
    halen: u8,
    addr: [u8; 8],
}

// SAFETY: the whole of a `sockaddr_ll`, which lives through the call.
unsafe impl SocketAddrArg for LinkAddr {
    unsafe fn with_sockaddr<R>(&self, f: impl FnOnce(*const SocketAddrOpaque, SocketAddrLen) -> R) -> R {
        f((self as *const LinkAddr).cast(), size_of::<LinkAddr>() as SocketAddrLen)
    }
}

/// A packet socket's membership (`struct packet_mreq`).
#[repr(C)]
#[derive(Clone, Copy)]
struct Membership {
    ifindex: i32,
    kind: u16,
    alen: u16,
    address: [u8; 8],
}

/// Switches interface `name` on (`up`) or off.
pub fn set_up(name: &str, up: bool) -> io::Result<()> {
    // A packet socket of no protocol, which receives nothing: for the
    // requests only.
    let socket = socket_with(AddressFamily::PACKET, SocketType::RAW, SocketFlags::CLOEXEC, None)?;
    let mut request = IfReq { name: [0; 16], flags: 0, _rest: [0; 22] };
    let n = name.len().min(15);
    request.name[..n].copy_from_slice(&name.as_bytes()[..n]);
    // SAFETY: the requests read an ifreq's name, and read or write its
    // flags.
    unsafe { ioctl(&socket, Updater::<SIOCGIFFLAGS, IfReq>::new(&mut request))? };
    request.flags = if up { request.flags | IFF_UP } else { request.flags & !IFF_UP };
    // SAFETY: as above.
    unsafe { ioctl(&socket, Updater::<SIOCSIFFLAGS, IfReq>::new(&mut request))? };
    Ok(())
}

/// A raw packet socket on interface `index`, not blocking: every frame it
/// receives (multicast too, which IPv6 needs), none of those it sends.
pub fn packet_socket(index: i32) -> io::Result<File> {
    let all = NonZeroU32::new(ALL_FRAMES as u32).map(Protocol::from_raw);
    let socket =
        socket_with(AddressFamily::PACKET, SocketType::RAW, SocketFlags::NONBLOCK | SocketFlags::CLOEXEC, all)?;
    let family = AddressFamily::PACKET.as_raw();
    let address =
        LinkAddr { family, protocol: ALL_FRAMES, ifindex: index, hatype: 0, pkttype: 0, halen: 0, addr: [0; 8] };
    bind(&socket, &address)?;
    setsockopt(&socket, SOL_PACKET, PACKET_IGNORE_OUTGOING, 1i32)?;
    let membership = Membership { ifindex: index, kind: PACKET_MR_ALLMULTI, alen: 0, address: [0; 8] };
    setsockopt(&socket, SOL_PACKET, PACKET_ADD_MEMBERSHIP, membership)?;
    // Room for bursts while the program is not running.
    let _ = sockopt::set_socket_recv_buffer_size_force(&socket, 4 << 20);
    Ok(File::from(socket))
}
