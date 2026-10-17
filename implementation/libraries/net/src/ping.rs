// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! ICMP echo ("ping").

use core::net::IpAddr;

use ipc::IpcError;
use proto::net::{NetError, SocketEvent, SocketRequest};
use rt::object::Channel;
use rt::time::Duration;

use crate::{send_request, with_client};

/// A successful echo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PingReply {
    pub from: IpAddr,
    pub seq: u16,
    pub size: u16,
    /// Round-trip time in microseconds.
    pub rtt_us: u32,
}

/// Sends ICMP echo requests.
pub struct Pinger {
    ch: Channel,
}

impl Pinger {
    pub fn new() -> Result<Pinger, NetError> {
        let ch = with_client(|c| {
            let (ours, theirs) = Channel::create().map_err(IpcError::Kernel)?;
            Ok(c.ping_open(theirs)?.map(|()| ours))
        })??;
        Ok(Pinger { ch })
    }

    /// Sends one echo request of `size` payload bytes and waits for its
    /// reply (the service gives up after four seconds).
    pub fn ping(&mut self, dest: IpAddr, seq: u16, size: u16, ttl: u8) -> Result<PingReply, NetError> {
        send_request(&self.ch, SocketRequest::Echo { dest, seq, ttl, size })?;
        let end = rt::time::deadline_after(Duration::from_secs(6));
        loop {
            let msg = self.ch.read_blocking(end).map_err(|e| match e {
                abi::Error::TimedOut => NetError::TimedOut,
                _ => NetError::Closed,
            })?;
            match ipc::decode_event::<SocketEvent>(msg) {
                Ok((_, SocketEvent::EchoReply { from, seq: s, size, rtt_us, .. })) if s == seq => {
                    return Ok(PingReply { from, seq, size, rtt_us });
                }
                Ok((_, SocketEvent::EchoFailed { seq: s, error, .. })) if s == seq => return Err(error),
                Ok((_, SocketEvent::Error { error })) => return Err(error),
                // Late answers to earlier requests.
                _ => {}
            }
        }
    }
}
