// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The network service (`netd`): the connection to it that sockets and name
//! lookups share, and the `errno` values of its errors.

use ipc::IpcError;
use proto::net::{NetError, net};
use rt::sync::Mutex;

use crate::linux::errno::*;

/// The connection to the network service, made on first use.
static CLIENT: Mutex<Option<net::Client>> = Mutex::new(None);

/// Whether the network service is registered: a connection to a service
/// that is not would wait until it is.
fn registered() -> bool {
    proto::with_registry(|r| r.list().is_ok_and(|names| names.iter().any(|n| n == net::NAME))).unwrap_or(false)
}

/// Runs `f` with the connection to the network service, connecting first if
/// need be, and once more if the service restarted. Calls from several
/// threads take turns.
pub fn with<R>(mut f: impl FnMut(&net::Client) -> Result<R, IpcError>) -> Result<R, NetError> {
    let mut slot = CLIENT.lock();
    for _ in 0..2 {
        if slot.is_none() {
            if !registered() {
                return Err(NetError::Unavailable);
            }
            let channel = proto::connect(net::NAME).map_err(|_| NetError::Unavailable)?;
            *slot = Some(net::Client::new(channel));
        }
        let Some(client) = slot.as_ref() else { break };
        match f(client) {
            Ok(r) => return Ok(r),
            Err(IpcError::PeerClosed) => *slot = None,
            Err(_) => return Err(NetError::Internal),
        }
    }
    Err(NetError::Unavailable)
}

/// The `errno` for an error of the network service.
pub fn errno(e: NetError) -> isize {
    match e {
        NetError::Unavailable | NetError::NetworkDown => ENETDOWN,
        NetError::NoRoute => ENETUNREACH,
        NetError::HostUnreachable | NetError::DestinationUnreachable | NetError::TimeExceeded => EHOSTUNREACH,
        NetError::ConnectionRefused => ECONNREFUSED,
        // A socket the service closed (its connection is gone) as much as
        // one the peer reset.
        NetError::ConnectionReset | NetError::Closed => ECONNRESET,
        NetError::Aborted => ECONNABORTED,
        NetError::TimedOut => ETIMEDOUT,
        NetError::AddressInUse => EADDRINUSE,
        NetError::AddressNotAvailable => EADDRNOTAVAIL,
        NetError::InvalidArgument => EINVAL,
        NetError::LimitReached => ENOBUFS,
        NetError::PermissionDenied => EACCES,
        NetError::NotSupported => EOPNOTSUPP,
        NetError::MessageTooLarge => EMSGSIZE,
        NetError::ProtocolViolation => EPROTO,
        NetError::NameNotFound | NetError::DnsFailure | NetError::Internal => EIO,
    }
}
