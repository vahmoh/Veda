// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Socket addresses: as programs pass them (`struct sockaddr_in`, `struct
//! sockaddr_in6`) and as the network service has them (`SocketAddr`).
//!
//! An IPv6 socket reaches IPv4 too, as on Linux: an IPv4-mapped address
//! (`::ffff:a.b.c.d`) given to it is the IPv4 address it maps, and IPv4
//! peers are reported to it so — unless the program made it IPv6 alone
//! (`IPV6_V6ONLY`).

use core::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};

use crate::linux::errno::{EAFNOSUPPORT, EFAULT, EINVAL, ENETUNREACH};
use crate::linux::sock::{AF_INET, AF_INET6, AF_UNSPEC};
use crate::linux::{SockaddrIn, SockaddrIn6};
use crate::user;

/// A socket's address family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    V4,
    V6,
}

impl Family {
    /// The family of a `socket` domain, if it is an Internet one.
    pub fn of(domain: u16) -> Option<Family> {
        match domain {
            AF_INET => Some(Family::V4),
            AF_INET6 => Some(Family::V6),
            _ => None,
        }
    }

    /// Its `socket` domain (`AF_INET`, `AF_INET6`).
    pub fn domain(self) -> u16 {
        match self {
            Family::V4 => AF_INET,
            Family::V6 => AF_INET6,
        }
    }

    /// The unspecified address and port of the family (`INADDR_ANY`, port
    /// 0), which a socket that was not bound is bound to.
    pub fn any(self) -> SocketAddr {
        match self {
            Family::V4 => SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
            Family::V6 => SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0),
        }
    }
}

/// What an address a program passed names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Addr(SocketAddr),
    /// `AF_UNSPEC`: no address (which dissolves a datagram socket's
    /// association with its peer).
    Unspec,
}

/// The least an IPv6 address may take: without its scope (RFC 2133's
/// `struct sockaddr_in6`).
const SOCKADDR_IN6_SHORT: usize = 24;

/// Reads the address a program passed (`len` bytes at `addr`) for a socket
/// of `family`, IPv6 alone or not (`v6only`).
///
/// # Safety
/// `addr` must be null or point at `len` readable bytes.
pub unsafe fn read(addr: usize, len: usize, family: Family, v6only: bool) -> Result<Target, isize> {
    if addr == 0 {
        return Err(EFAULT);
    }
    if len < size_of::<u16>() {
        return Err(EINVAL);
    }
    // SAFETY: per the caller, at least the family is there.
    let domain: u16 = unsafe { user::read(addr)? };
    match (domain, family) {
        (AF_UNSPEC, _) => Ok(Target::Unspec),
        (AF_INET, Family::V4) => {
            if len < size_of::<SockaddrIn>() {
                return Err(EINVAL);
            }
            // SAFETY: as above, the whole address.
            let sa: SockaddrIn = unsafe { user::read(addr)? };
            Ok(Target::Addr(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::from(sa.addr), u16::from_be_bytes(sa.port)))))
        }
        (AF_INET6, Family::V6) => {
            if len < SOCKADDR_IN6_SHORT {
                return Err(EINVAL);
            }
            let mut raw = [0u8; size_of::<SockaddrIn6>()];
            let n = len.min(raw.len());
            // SAFETY: as above, the `len` bytes of the address.
            raw[..n].copy_from_slice(unsafe { user::slice(addr, n)? });
            // SAFETY: any bytes are a `SockaddrIn6`.
            let sa: SockaddrIn6 = unsafe { core::ptr::read_unaligned(raw.as_ptr().cast()) };
            let ip = Ipv6Addr::from(sa.addr);
            let port = u16::from_be_bytes(sa.port);
            match ip.to_ipv4_mapped() {
                Some(_) if v6only => Err(ENETUNREACH),
                Some(v4) => Ok(Target::Addr(SocketAddr::V4(SocketAddrV4::new(v4, port)))),
                None => Ok(Target::Addr(SocketAddr::V6(SocketAddrV6::new(
                    ip,
                    port,
                    u32::from_be_bytes(sa.flowinfo),
                    sa.scope_id,
                )))),
            }
        }
        _ => Err(EAFNOSUPPORT),
    }
}

/// An address as a program gets it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Raw {
    V4(SockaddrIn),
    V6(SockaddrIn6),
}

impl Raw {
    /// `addr` as a socket of `family` reports it: an IPv4 address to an IPv6
    /// socket as the IPv6 address that maps it. `None` for an IPv6 address
    /// to an IPv4 socket, which cannot have one.
    pub fn of(addr: SocketAddr, family: Family) -> Option<Raw> {
        let v4 = |a: SocketAddrV4| {
            Raw::V4(SockaddrIn { family: AF_INET, port: a.port().to_be_bytes(), addr: a.ip().octets(), zero: [0; 8] })
        };
        let v6 = |ip: Ipv6Addr, port: u16, flowinfo: u32, scope_id: u32| {
            Raw::V6(SockaddrIn6 {
                family: AF_INET6,
                port: port.to_be_bytes(),
                flowinfo: flowinfo.to_be_bytes(),
                addr: ip.octets(),
                scope_id,
            })
        };
        match (addr, family) {
            (SocketAddr::V4(a), Family::V4) => Some(v4(a)),
            (SocketAddr::V6(a), Family::V4) => a.ip().to_ipv4_mapped().map(|ip| v4(SocketAddrV4::new(ip, a.port()))),
            (SocketAddr::V4(a), Family::V6) => Some(v6(a.ip().to_ipv6_mapped(), a.port(), 0, 0)),
            (SocketAddr::V6(a), Family::V6) => Some(v6(*a.ip(), a.port(), a.flowinfo(), a.scope_id())),
        }
    }

    fn bytes(&self) -> &[u8] {
        let (p, n) = match self {
            Raw::V4(a) => (a as *const SockaddrIn as *const u8, size_of::<SockaddrIn>()),
            Raw::V6(a) => (a as *const SockaddrIn6 as *const u8, size_of::<SockaddrIn6>()),
        };
        // SAFETY: plain old data of `n` bytes, without padding.
        unsafe { core::slice::from_raw_parts(p, n) }
    }

    /// Stores the address for the program: at `addr`, which has room for
    /// the `socklen_t` at `addrlen` says, as much of it as fits; then its
    /// whole length at `addrlen` (which tells the program whether it was
    /// cut short). Nothing when `addr` is null, as for `recvfrom` without
    /// one.
    ///
    /// # Safety
    /// `addrlen` must be null or the program's `socklen_t`, and `addr` null
    /// or a buffer of that many bytes.
    pub unsafe fn store(&self, addr: usize, addrlen: usize) -> Result<(), isize> {
        if addr == 0 {
            return Ok(());
        }
        // SAFETY: per the caller.
        let room: u32 = unsafe { user::read(addrlen)? };
        let bytes = self.bytes();
        let n = bytes.len().min(room as usize);
        // SAFETY: per the caller, `room` bytes are there.
        unsafe { user::slice_mut(addr, n)? }.copy_from_slice(&bytes[..n]);
        // SAFETY: per the caller.
        unsafe { user::write(addrlen, bytes.len() as u32) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_raw(raw: &Raw, family: Family, v6only: bool) -> Result<Target, isize> {
        let bytes = raw.bytes();
        // SAFETY: the test's own bytes.
        unsafe { read(bytes.as_ptr() as usize, bytes.len(), family, v6only) }
    }

    #[test]
    fn addresses_go_both_ways() {
        let v4: SocketAddr = "192.0.2.1:8080".parse().unwrap();
        let raw = Raw::of(v4, Family::V4).unwrap();
        assert!(matches!(raw, Raw::V4(a) if a.port == [0x1f, 0x90] && a.addr == [192, 0, 2, 1]));
        assert_eq!(read_raw(&raw, Family::V4, false), Ok(Target::Addr(v4)));
        let v6: SocketAddr = "[2001:db8::7]:443".parse().unwrap();
        let raw = Raw::of(v6, Family::V6).unwrap();
        assert_eq!(read_raw(&raw, Family::V6, false), Ok(Target::Addr(v6)));
        // Each socket takes its own family alone.
        assert_eq!(read_raw(&raw, Family::V4, false), Err(EAFNOSUPPORT));
        assert_eq!(read_raw(&Raw::of(v4, Family::V4).unwrap(), Family::V6, false), Err(EAFNOSUPPORT));
    }

    #[test]
    fn ipv6_sockets_reach_ipv4_through_mapped_addresses() {
        let v4: SocketAddr = "10.0.2.2:53".parse().unwrap();
        let mapped = Raw::of(v4, Family::V6).unwrap();
        let Raw::V6(m) = mapped else { panic!("{mapped:?}") };
        assert_eq!(Ipv6Addr::from(m.addr), "::ffff:10.0.2.2".parse::<Ipv6Addr>().unwrap());
        assert_eq!(read_raw(&mapped, Family::V6, false), Ok(Target::Addr(v4)));
        assert_eq!(read_raw(&mapped, Family::V6, true), Err(ENETUNREACH));
        // An IPv4 socket has no IPv6 peers.
        let v6: SocketAddr = "[2001:db8::7]:443".parse().unwrap();
        assert_eq!(Raw::of(v6, Family::V4), None);
    }

    #[test]
    fn short_and_strange_addresses_are_refused() {
        let raw = Raw::of("192.0.2.1:1".parse().unwrap(), Family::V4).unwrap();
        let bytes = raw.bytes();
        // SAFETY: the test's own bytes (fewer than there are).
        assert_eq!(unsafe { read(bytes.as_ptr() as usize, 8, Family::V4, false) }, Err(EINVAL));
        // SAFETY: a null address.
        assert_eq!(unsafe { read(0, 16, Family::V4, false) }, Err(EFAULT));
        let unix = [1u8, 0, 0, 0];
        // SAFETY: the test's own bytes.
        assert_eq!(unsafe { read(unix.as_ptr() as usize, 4, Family::V4, false) }, Err(EAFNOSUPPORT));
        let unspec = [0u8; 16];
        // SAFETY: the test's own bytes.
        assert_eq!(unsafe { read(unspec.as_ptr() as usize, 16, Family::V6, false) }, Ok(Target::Unspec));
    }

    #[test]
    fn stored_addresses_say_their_length() {
        let raw = Raw::of("[2001:db8::7]:443".parse().unwrap(), Family::V6).unwrap();
        let mut buf = [0xaau8; 16];
        let mut len: u32 = 16;
        // SAFETY: the test's buffer and length.
        unsafe { raw.store(buf.as_mut_ptr() as usize, &mut len as *mut u32 as usize).unwrap() };
        assert_eq!(len, 28);
        assert_eq!(&buf[..4], &[10, 0, 1, 0xbb]);
    }
}
