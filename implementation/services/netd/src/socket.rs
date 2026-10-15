// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Socket channels: the per-socket conversation with a client (see the
//! `vproto::net` module docs for the protocol and its credit-based flow
//! control).
//!
//! Nothing a client is owed is ever dropped because its channel is full (a
//! channel holds a bounded number of messages, whatever their size): data
//! is taken from the stack only once its message is queued, and an event
//! that does not fit (the connection, the end of the stream, an error, send
//! credit) is sent again when the channel has room, which the service waits
//! for. A TCP client may only send what fits the stack's send buffer, so
//! what it sent is in the stack the moment it arrives: closing its channel
//! loses none of it.

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::net::SocketAddr;

use vabi::Error;
use vipc::{Encode, Encoder};
use vnetstack::{PingEvent, SockId, Stack, TcpState};
use vproto::net::{MAX_SEND, NetError, RECEIVE_WINDOW, SocketEvent, SocketRequest};
use vrt::object::Channel;

/// Bytes of datagrams a UDP client may send ahead of what the stack has
/// taken.
pub const UDP_SEND_WINDOW: u32 = 64 * 1024;
/// Largest received chunk per message.
const CHUNK: usize = 16 * 1024;
/// Return send credit once this much accumulated (or the stack's send
/// buffer emptied).
const CREDIT_BATCH: u32 = 16 * 1024;
/// Most requests handled per socket per loop iteration.
const REQUEST_BUDGET: usize = 64;

/// The events of every socket are written through one encoder whose header
/// (an event, ordinal 0) stays in place.
pub struct Events(Encoder);

impl Events {
    /// An encoder for events.
    pub fn new() -> Events {
        Events(Encoder::with_header(0, 0, vipc::FLAG_EVENT))
    }

    /// Encodes an event with `fill` and writes it to `channel`.
    fn send(&mut self, channel: &Channel, fill: impl FnOnce(&mut Encoder)) -> Sent {
        let e = &mut self.0;
        e.bytes.truncate(vipc::HEADER_LEN);
        fill(e);
        match channel.write(&e.bytes, core::mem::take(&mut e.handles)) {
            Ok(()) => Sent::Yes,
            Err(Error::ShouldWait) => Sent::Full,
            Err(_) => Sent::Gone,
        }
    }
}

/// What became of an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sent {
    Yes,
    /// The client's queue is full: try again when it has room.
    Full,
    /// The client closed the channel.
    Gone,
}

enum Kind {
    Tcp {
        /// `Connected` was sent.
        announced: bool,
        eof_sent: bool,
        /// The client finished sending.
        shutdown: bool,
    },
    Listener,
    Udp {
        /// Datagrams from the client not yet taken by the stack.
        pending: VecDeque<(SocketAddr, Vec<u8>)>,
        /// Credit earned back (datagrams the stack took), not yet returned.
        credit_due: u32,
    },
    Ping,
}

/// One socket channel.
pub struct Conn {
    pub channel: Channel,
    pub id: SockId,
    kind: Kind,
    /// The process the socket belongs to.
    pub owner: u64,
    /// Send credit granted but not yet used by the client.
    credit_out: u32,
    /// Received bytes sent to the client and not yet acknowledged.
    rx_outstanding: u32,
    /// An error to report; the socket is finished once the client has it.
    failure: Option<NetError>,
    /// The client's queue was full: wait until it has room.
    pub blocked: bool,
    /// The socket is finished (error reported or the stream ended): drop
    /// it once the client closes the channel.
    pub done: bool,
}

impl Conn {
    fn new(channel: Channel, id: SockId, kind: Kind, owner: u64) -> Conn {
        Conn { channel, id, kind, owner, credit_out: 0, rx_outstanding: 0, failure: None, blocked: false, done: false }
    }

    /// A TCP connection being made.
    pub fn tcp(channel: Channel, id: SockId, owner: u64) -> Conn {
        Conn::new(channel, id, Kind::Tcp { announced: false, eof_sent: false, shutdown: false }, owner)
    }

    /// A TCP listener.
    pub fn listener(channel: Channel, id: SockId, owner: u64) -> Conn {
        Conn::new(channel, id, Kind::Listener, owner)
    }

    /// A UDP socket; its first pump grants the initial send credit.
    pub fn udp(channel: Channel, id: SockId, owner: u64) -> Conn {
        Conn::new(channel, id, Kind::Udp { pending: VecDeque::new(), credit_due: UDP_SEND_WINDOW }, owner)
    }

    /// A ping socket.
    pub fn ping(channel: Channel, id: SockId, owner: u64) -> Conn {
        Conn::new(channel, id, Kind::Ping, owner)
    }

    /// Whether the client must be heard: requests are read only from a
    /// socket that is not finished.
    pub fn wants_requests(&self) -> bool {
        !self.done
    }

    /// Records an error for the client; it is reported on the next pump.
    fn fail(&mut self, error: NetError) {
        if !self.done {
            self.failure.get_or_insert(error);
        }
    }

    /// The client broke the protocol: its TCP stream is no longer what it
    /// meant to send, so the connection is reset rather than ended.
    fn violation(&mut self, stack: &mut Stack) {
        if matches!(self.kind, Kind::Tcp { .. }) {
            stack.tcp_abort(self.id);
        }
        self.fail(NetError::ProtocolViolation);
    }

    /// Handles the client's requests. Returns `false` when the client closed
    /// the channel (and every request it sent before was handled).
    pub fn handle_requests(&mut self, stack: &mut Stack) -> bool {
        for _ in 0..REQUEST_BUDGET {
            let msg = match self.channel.read() {
                Ok(m) => m,
                Err(Error::ShouldWait) => return true,
                Err(_) => return false,
            };
            if self.done || self.failure.is_some() {
                continue;
            }
            match vipc::decode_event::<SocketRequest>(msg) {
                Ok((_, req)) => self.handle(stack, req),
                Err(_) => self.violation(stack),
            }
        }
        true
    }

    fn handle(&mut self, stack: &mut Stack, req: SocketRequest) {
        match (&mut self.kind, req) {
            (Kind::Tcp { announced: true, shutdown: false, .. }, SocketRequest::Send { data }) => {
                let n = data.0.len();
                if n > MAX_SEND || n as u32 > self.credit_out {
                    return self.violation(stack);
                }
                self.credit_out -= n as u32;
                // The credit never exceeds the room in the stack's send
                // buffer: all of it fits, unless the connection failed (its
                // error is reported by the pump).
                match stack.tcp_send(self.id, &data.0) {
                    Ok(k) if k == n => {}
                    Ok(_) => self.fail(NetError::Internal),
                    Err(_) => {}
                }
            }
            (Kind::Udp { pending, .. }, SocketRequest::SendTo { dest, data }) => {
                let n = data.0.len();
                if n > vproto::net::MAX_DATAGRAM || n as u32 > self.credit_out {
                    return self.violation(stack);
                }
                self.credit_out -= n as u32;
                pending.push_back((dest, data.0));
            }
            (Kind::Tcp { .. } | Kind::Udp { .. }, SocketRequest::Consumed { bytes }) => {
                self.rx_outstanding = self.rx_outstanding.saturating_sub(bytes);
            }
            (Kind::Tcp { shutdown, .. }, SocketRequest::Shutdown {}) => {
                if !*shutdown {
                    *shutdown = true;
                    stack.tcp_shutdown(self.id);
                }
            }
            (Kind::Tcp { .. }, SocketRequest::Abort {}) => stack.tcp_abort(self.id),
            (Kind::Tcp { .. }, SocketRequest::SetNoDelay { nodelay }) => stack.tcp_set_nodelay(self.id, nodelay),
            (Kind::Ping, SocketRequest::Echo { dest, seq, ttl, size }) => {
                stack.ping_send(self.id, dest, seq, ttl, size)
            }
            _ => self.violation(stack),
        }
    }

    /// Moves data and news between the stack and the client, as far as the
    /// client's channel has room. `room` is how many more sockets the
    /// owner may have (for the connections a listener accepts); accepted
    /// connections are added to `accepted`.
    pub fn pump(&mut self, stack: &mut Stack, events: &mut Events, room: &mut usize, accepted: &mut Vec<Conn>) {
        if self.done {
            return;
        }
        if self.failure.is_none() {
            let sent = match self.kind {
                Kind::Tcp { .. } => self.pump_tcp(stack, events),
                Kind::Listener => self.pump_listener(stack, events, room, accepted),
                Kind::Udp { .. } => self.pump_udp(stack, events),
                Kind::Ping => self.pump_ping(stack, events),
            };
            match sent {
                Sent::Yes => {}
                Sent::Full => return self.blocked = true,
                Sent::Gone => return self.done = true,
            }
        }
        // The error that ends the socket, after everything before it.
        if let Some(error) = self.failure {
            match events.send(&self.channel, |e| SocketEvent::Error { error }.encode(e)) {
                Sent::Yes | Sent::Gone => {
                    self.failure = None;
                    self.done = true;
                }
                Sent::Full => self.blocked = true,
            }
        }
    }

    fn pump_tcp(&mut self, stack: &mut Stack, events: &mut Events) -> Sent {
        let Some(view) = stack.tcp_view(self.id) else {
            self.fail(NetError::Closed);
            return Sent::Yes;
        };
        let Kind::Tcp { announced, eof_sent, .. } = &mut self.kind else { return Sent::Yes };
        match view.state {
            TcpState::Connecting => return Sent::Yes,
            TcpState::Closed { error: Some(error) } => {
                self.fail(error);
                return Sent::Yes;
            }
            TcpState::Open { local, remote } if !*announced => {
                // All the room in the stack's send buffer.
                let credit = view.send_space.min(u32::MAX as usize) as u32;
                match events.send(&self.channel, |e| SocketEvent::Connected { local, remote, credit }.encode(e)) {
                    Sent::Yes => {}
                    other => return other,
                }
                *announced = true;
                self.credit_out = credit;
            }
            _ => {}
        }
        let eof_sent = *eof_sent;
        // Received data, within the client's window and its channel's room.
        while self.rx_outstanding < RECEIVE_WINDOW {
            let max = ((RECEIVE_WINDOW - self.rx_outstanding) as usize).min(CHUNK);
            let mut sent = Sent::Yes;
            let channel = &self.channel;
            let n = stack
                .tcp_recv_with(self.id, max, |data| {
                    sent = events.send(channel, |e| SocketEvent::encode_data(data, e));
                    if sent == Sent::Yes { data.len() } else { 0 }
                })
                .unwrap_or(0);
            if sent != Sent::Yes {
                return sent;
            }
            if n == 0 {
                break;
            }
            self.rx_outstanding += n as u32;
        }
        let Some(view) = stack.tcp_view(self.id) else { return Sent::Yes };
        // The end of the stream, once everything before it was delivered;
        // a connection that closed normally has nothing more to say.
        let closed = matches!(view.state, TcpState::Closed { .. });
        if (view.eof || (closed && view.recv_queued == 0)) && !eof_sent {
            match events.send(&self.channel, |e| SocketEvent::Eof {}.encode(e)) {
                Sent::Yes => {}
                other => return other,
            }
            if let Kind::Tcp { eof_sent, .. } = &mut self.kind {
                *eof_sent = true;
            }
        }
        if closed {
            if view.recv_queued == 0 {
                self.done = true;
            }
            return Sent::Yes;
        }
        // Send credit: the room the stack's send buffer gained.
        let room = view.send_space.min(u32::MAX as usize) as u32;
        if matches!(self.kind, Kind::Tcp { announced: true, shutdown: false, .. }) && room > self.credit_out {
            let bytes = room - self.credit_out;
            if bytes >= CREDIT_BATCH || view.send_queued == 0 {
                match events.send(&self.channel, |e| SocketEvent::SendCredit { bytes }.encode(e)) {
                    Sent::Yes => self.credit_out += bytes,
                    other => return other,
                }
            }
        }
        Sent::Yes
    }

    fn pump_listener(
        &mut self,
        stack: &mut Stack,
        events: &mut Events,
        room: &mut usize,
        accepted: &mut Vec<Conn>,
    ) -> Sent {
        while let Some(new) = stack.tcp_accept_peek(self.id) {
            let view = stack.tcp_view(new);
            let Some(TcpState::Open { local, remote }) = view.map(|v| v.state) else {
                // Gone before it was handed over.
                stack.tcp_accept(self.id);
                stack.close(new);
                continue;
            };
            if *room == 0 {
                // The owner has as many sockets as it may: refuse it.
                stack.tcp_accept(self.id);
                stack.tcp_abort(new);
                stack.close(new);
                continue;
            }
            let Ok((ours, theirs)) = Channel::create() else { return Sent::Yes };
            let credit = view.map_or(0, |v| v.send_space).min(u32::MAX as usize) as u32;
            let ev = SocketEvent::Accepted { socket: theirs, local, remote, credit };
            match events.send(&self.channel, |e| ev.encode(e)) {
                Sent::Yes => {}
                // Still queued: offered again (on a new channel) when the
                // client has room.
                other => return other,
            }
            stack.tcp_accept(self.id);
            *room -= 1;
            let mut conn =
                Conn::new(ours, new, Kind::Tcp { announced: true, eof_sent: false, shutdown: false }, self.owner);
            conn.credit_out = credit;
            accepted.push(conn);
        }
        Sent::Yes
    }

    fn pump_udp(&mut self, stack: &mut Stack, events: &mut Events) -> Sent {
        // Received datagrams, within the client's window and its channel's
        // room; a datagram is taken from the stack only once it is queued.
        while self.rx_outstanding < RECEIVE_WINDOW {
            let mut sent = Sent::Yes;
            let mut len = 0;
            let channel = &self.channel;
            let taken = stack.udp_recv_with(self.id, |from, data| {
                len = data.len();
                sent = events.send(channel, |e| SocketEvent::encode_datagram(from, data, e));
                sent == Sent::Yes
            });
            match taken {
                None => break,
                Some(true) => self.rx_outstanding += len as u32,
                Some(false) => return sent,
            }
        }
        let Kind::Udp { pending, credit_due } = &mut self.kind else { return Sent::Yes };
        while let Some((dest, data)) = pending.pop_front() {
            if let Err(NetError::LimitReached) = stack.udp_send(self.id, dest, &data) {
                // The stack's buffer is full: it says when it has sent some.
                pending.push_front((dest, data));
                break;
            }
            // Sent, or undeliverable and dropped, as on any system.
            *credit_due += data.len() as u32;
        }
        if *credit_due > 0 {
            let bytes = *credit_due;
            match events.send(&self.channel, |e| SocketEvent::SendCredit { bytes }.encode(e)) {
                Sent::Yes => {
                    *credit_due = 0;
                    self.credit_out += bytes;
                }
                other => return other,
            }
        }
        Sent::Yes
    }

    fn pump_ping(&mut self, stack: &mut Stack, events: &mut Events) -> Sent {
        while let Some(ev) = stack.ping_next(self.id) {
            let ev = match ev {
                PingEvent::Reply { from, seq, size, rtt_us } => SocketEvent::EchoReply { from, seq, size, rtt_us },
                PingEvent::Failed { seq, from, error } => SocketEvent::EchoFailed { seq, from, error },
            };
            match events.send(&self.channel, |e| ev.encode(e)) {
                Sent::Yes => stack.ping_taken(self.id),
                other => return other,
            }
        }
        Sent::Yes
    }
}
