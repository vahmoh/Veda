// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Names, for the C library's resolver: the network service looks them up.
//!
//! * `getaddrinfo` and `gethostbyname` ask the service's resolver
//!   ([`__veda_lookup_name`]), which caches answers, knows `localhost` and
//!   gives the addresses in the order they are best tried in (IPv4 first:
//!   they work through every NAT). musl looks in `/etc/hosts` first, if
//!   there is one.
//! * musl's own DNS client — `res_query` and `res_send`, and the reverse
//!   lookups of `getnameinfo` — sends its queries to the DNS servers the
//!   service uses ([`__veda_dns_servers`]) over the layer's sockets. There
//!   is no `/etc/resolv.conf`.

use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::{CStr, c_char, c_int};
use core::net::IpAddr;

use ipc::IpcError;
use proto::net::{AddrFamily, NetError, ResolveResult};
use rt::object::Channel;
use rt::time::Duration;

use crate::linux::sock::{AF_INET, AF_INET6, AF_UNSPEC};
use crate::netd;

/// musl's `struct address` (`src/network/lookup.h`): an address of either
/// family, IPv4's in the first four bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Address {
    pub family: c_int,
    pub scopeid: u32,
    pub addr: [u8; 16],
    pub sortkey: c_int,
}

impl Address {
    fn new(ip: IpAddr) -> Address {
        let mut addr = [0; 16];
        let family = match ip {
            IpAddr::V4(a) => {
                addr[..4].copy_from_slice(&a.octets());
                AF_INET
            }
            IpAddr::V6(a) => {
                addr = a.octets();
                AF_INET6
            }
        };
        Address { family: family as c_int, scopeid: 0, addr, sortkey: 0 }
    }
}

/// The most addresses musl takes from a lookup (`MAXADDRS`).
const MAX_ADDRESSES: usize = 48;
/// The size of musl's buffer for the canonical name, with its NUL.
const CANONICAL_SIZE: usize = 256;
/// Longest a lookup may take: the service gives up after ten seconds.
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(15);

/// `getaddrinfo`'s errors (`<netdb.h>`).
mod eai {
    use core::ffi::c_int;
    pub const NONAME: c_int = -2;
    pub const AGAIN: c_int = -3;
    pub const FAIL: c_int = -4;
    pub const FAMILY: c_int = -6;
}

/// What a lookup's failure means to `getaddrinfo`: the name does not exist,
/// or asking again later may help (the network or its DNS servers are not
/// there now), or neither.
fn eai(e: NetError) -> c_int {
    match e {
        NetError::NameNotFound | NetError::InvalidArgument => eai::NONAME,
        NetError::DnsFailure
        | NetError::TimedOut
        | NetError::Unavailable
        | NetError::NetworkDown
        | NetError::NoRoute
        | NetError::HostUnreachable
        | NetError::LimitReached => eai::AGAIN,
        _ => eai::FAIL,
    }
}

/// Looks `name` up: its addresses, best first, and its canonical name.
fn lookup(name: &str, family: AddrFamily) -> Result<(Vec<IpAddr>, String), NetError> {
    let name = String::from(name);
    // A reply channel for each attempt: one sent to a service that has
    // since restarted is gone with it.
    let reply = netd::with(|c| {
        let (ours, theirs) = Channel::create().map_err(IpcError::Kernel)?;
        Ok(c.resolve(name.clone(), family, theirs)?.map(|()| ours))
    })??;
    let message = reply.read_blocking(rt::time::deadline_after(LOOKUP_TIMEOUT)).map_err(|e| match e {
        abi::Error::TimedOut => NetError::TimedOut,
        _ => NetError::Unavailable,
    })?;
    match ipc::decode_event::<ResolveResult>(message) {
        Ok((_, ResolveResult::Found { addresses, canonical, .. })) => Ok((addresses, canonical)),
        Ok((_, ResolveResult::Failed { error })) => Err(error),
        Err(_) => Err(NetError::Internal),
    }
}

/// Looks up `name` for musl's `__lookup_name`: fills `buf` with its
/// addresses (of `family`, or of both for `AF_UNSPEC`), in the order the
/// network service prefers them, and `canon` with its canonical name.
/// Returns how many addresses there are, or an `EAI_*` error.
///
/// # Safety
/// `buf` must have room for musl's 48 addresses, `canon` for 256 bytes and
/// `name` must be a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __veda_lookup_name(
    buf: *mut Address,
    canon: *mut c_char,
    name: *const c_char,
    family: c_int,
) -> c_int {
    let family = match u16::try_from(family) {
        Ok(AF_UNSPEC) => AddrFamily::Any,
        Ok(AF_INET) => AddrFamily::V4,
        Ok(AF_INET6) => AddrFamily::V6,
        _ => return eai::FAMILY,
    };
    // SAFETY: per the caller.
    let Ok(name) = unsafe { CStr::from_ptr(name) }.to_str() else { return eai::NONAME };
    let (addresses, canonical) = match lookup(name, family) {
        Ok(found) => found,
        Err(e) => return eai(e),
    };
    if addresses.is_empty() {
        return eai::NONAME;
    }
    let n = addresses.len().min(MAX_ADDRESSES);
    for (i, ip) in addresses.into_iter().take(n).enumerate() {
        // SAFETY: `buf` has room for `MAX_ADDRESSES`.
        unsafe { buf.add(i).write(Address::new(ip)) };
    }
    if !canonical.is_empty() && canonical.len() < CANONICAL_SIZE {
        // SAFETY: `canon` has room for `CANONICAL_SIZE` bytes.
        unsafe {
            core::ptr::copy_nonoverlapping(canonical.as_ptr(), canon.cast::<u8>(), canonical.len());
            canon.add(canonical.len()).write(0);
        }
    }
    n as c_int
}

/// The DNS servers the network service uses (those DHCP gave, or those set
/// in Settings), for musl's DNS client: fills `out` with at most `max` of
/// them and returns how many (0: none, or no network service).
///
/// # Safety
/// `out` must have room for `max` addresses.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __veda_dns_servers(out: *mut Address, max: c_int) -> c_int {
    let Ok(status) = netd::with(|c| c.status()) else { return 0 };
    let n = status.dns_servers.len().min(usize::try_from(max).unwrap_or(0));
    for (i, ip) in status.dns_servers.into_iter().take(n).enumerate() {
        // SAFETY: `out` has room for `max` addresses.
        unsafe { out.add(i).write(Address::new(ip)) };
    }
    n as c_int
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_are_laid_out_as_musl_has_them() {
        assert_eq!(size_of::<Address>(), 28);
        assert_eq!(core::mem::offset_of!(Address, addr), 8);
        let v4 = Address::new("192.0.2.7".parse().unwrap());
        assert_eq!((v4.family, &v4.addr[..5]), (AF_INET as c_int, &[192, 0, 2, 7, 0][..]));
        let v6 = Address::new("2001:db8::1".parse().unwrap());
        assert_eq!((v6.family, v6.addr[0], v6.addr[15]), (AF_INET6 as c_int, 0x20, 1));
    }

    #[test]
    fn failures_say_whether_to_try_again() {
        assert_eq!(eai(NetError::NameNotFound), eai::NONAME);
        assert_eq!(eai(NetError::DnsFailure), eai::AGAIN);
        assert_eq!(eai(NetError::Unavailable), eai::AGAIN);
        assert_eq!(eai(NetError::Internal), eai::FAIL);
    }
}
