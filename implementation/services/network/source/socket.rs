// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Socket channels: the per-socket conversation with a client (see the
//! `proto::net` module docs for the protocol and its credit-based flow
//! control).
//!
//! Nothing a client is owed is ever dropped because its channel is full (a
//! channel holds a bounded number of messages, whatever their size): data
//! is taken from the stack only once its message is queued, and an event
//! that does not fit (the connection, the end of the stream, an error, send
//! credit) is sent again when the channel has room, which the service waits
//! for. A TCP client may only send what fits the stack's send buffer, so
//! what it sent is in the stack the moment it arrives: closing its channel
//! loses none of it. A client that closed its channel is sent nothing more,
//! but what it sent before is handled all the same (its last data, its
//! shutdown, its last datagrams, which the socket stays for until the stack
//! has sent them, a few seconds at most), however soon the service finds
//! the channel closed.
//!
//! A TCP connection whose bytes go on a kernel socket (the POSIX layer's,
//! which processes share: see `proto::net`) is moved between the stack and
//! that socket as far as each has room. What its clients wrote before they
//! closed their end is sent all the same: the connection is closed once the
//! end is gone and all of it is in the stack. An error is reported on the
//! channel before the service closes its end of the kernel socket, so that a
//! client that finds the stream ended finds the error too. A UDP socket
//! without credit stops reading its client's datagrams while those it has
//! not sent yet are many, and writes received datagrams while the channel
//! has room.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use core::net::SocketAddr;

use abi::signals::{PEER_CLOSED, PEER_WRITE_DISABLED, READABLE, WRITABLE};
use abi::{Error, RawHandle};
use ipc::{Encode, Encoder};
use netstack::{PingEvent, SockId, Stack, TcpState};
use proto::net::{MAX_SEND, NetError, RECEIVE_WINDOW, SocketEvent, SocketRequest, TcpOptions, UDP_UNMETERED_WINDOW};
use runtime::object::{Channel, Socket};

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
        Events(Encoder::with_header(0, 0, ipc::FLAG_EVENT))
    }

    /// Encodes an event with `fill` and writes it to `channel`.
    fn send(&mut self, channel: &Channel, fill: impl FnOnce(&mut Encoder)) -> Sent {
        let e = &mut self.0;
        e.bytes.truncate(ipc::HEADER_LEN);
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
    /// A TCP socket bound to an address, which its client makes a listener
    /// or a connection, both of the `Bytes` kind (see [`Turn`]).
    Bound {
        /// Its client is a system service, whose listening nobody is asked
        /// about.
        service: bool,
    },
    Tcp {
        /// `Connected` was sent.
        announced: bool,
        eof_sent: bool,
        /// The client finished sending.
        shutdown: bool,
    },
    /// A TCP connection whose bytes go on a kernel socket.
    Bytes(Bytes),
    /// Its connections are of the `Bytes` kind, or else `Tcp`.
    Listener {
        bytes: bool,
    },
    Udp {
        /// Datagrams from the client not yet taken by the stack, and their
        /// bytes.
        pending: VecDeque<(SocketAddr, Vec<u8>)>,
        pending_bytes: usize,
        /// Credit earned back (datagrams the stack took), not yet returned.
        credit_due: u32,
        /// The client has credit (else the channel's room is the flow
        /// control).
        metered: bool,
    },
    Ping,
}

/// What the client of a bound socket asked to make of it, which the
/// service does, the firewall having its say ([`Conn::take_turn`]).
pub enum Turn {
    /// Listen, the answer going on `reply` (asked of a listener too, which
    /// answers with its address).
    Listen { backlog: u32, options: TcpOptions, reply: Channel },
    /// Connect, the connection's bytes going on `bytes`.
    Connect { remote: SocketAddr, options: TcpOptions, bytes: Socket },
}

/// A TCP connection's bytes on a kernel socket.
struct Bytes {
    /// The service's end; `None` once the connection failed (which its
    /// clients see as the end of the stream, the error reported first).
    end: Option<Socket>,
    /// `Connected` was sent.
    announced: bool,
    /// The end of the received stream was passed on: this end stopped
    /// writing.
    eof_sent: bool,
    /// The clients' stream ended (their end stopped writing, or all of
    /// them closed it), and the connection's after it.
    shutdown: bool,
    /// Received bytes wait for room in the kernel socket.
    full: bool,
    /// The clients' bytes wait for room in the stack.
    held: bool,
    /// Every client closed its end of the kernel socket.
    gone: bool,
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
    /// The client closed its channel: nothing more is sent to it.
    gone: bool,
    /// The client closed its channel, and the socket goes on (see
    /// [`Conn::outlives_channel`]): the channel is not waited on.
    forgotten: bool,
    /// The socket is finished (error reported or the stream ended): drop
    /// it once the client closes the channel.
    pub done: bool,
    /// What the client asked to make of the socket, which the service is
    /// to do before it reads on.
    turn: Option<Turn>,
    /// The application the socket is for, by the name the system knows it
    /// by: the firewall's rules name it.
    pub app: String,
    /// The application may use the network (the firewall keeps it off
    /// otherwise: what it sends elsewhere than the machine itself is
    /// dropped).
    network: bool,
}

impl Bytes {
    fn new(end: Socket, announced: bool) -> Bytes {
        Bytes { end: Some(end), announced, eof_sent: false, shutdown: false, full: false, held: false, gone: false }
    }
}

impl Conn {
    fn new(channel: Channel, id: SockId, kind: Kind, owner: u64, app: String) -> Conn {
        Conn {
            channel,
            id,
            kind,
            owner,
            credit_out: 0,
            rx_outstanding: 0,
            failure: None,
            blocked: false,
            gone: false,
            forgotten: false,
            done: false,
            turn: None,
            app,
            network: true,
        }
    }

    /// A TCP socket bound to an address, of a system service's if
    /// `service`.
    pub fn bound(channel: Channel, id: SockId, owner: u64, service: bool, app: String) -> Conn {
        Conn::new(channel, id, Kind::Bound { service }, owner, app)
    }

    /// A TCP connection being made.
    pub fn tcp(channel: Channel, id: SockId, owner: u64, app: String) -> Conn {
        Conn::new(channel, id, Kind::Tcp { announced: false, eof_sent: false, shutdown: false }, owner, app)
    }

    /// A TCP connection being made whose bytes go on a kernel socket, of
    /// which `end` is the service's end.
    pub fn tcp_bytes(channel: Channel, end: Socket, id: SockId, owner: u64, app: String) -> Conn {
        Conn::new(channel, id, Kind::Bytes(Bytes::new(end, false)), owner, app)
    }

    /// A TCP listener; its connections' bytes go on kernel sockets with
    /// `bytes`.
    pub fn listener(channel: Channel, id: SockId, owner: u64, bytes: bool, app: String) -> Conn {
        Conn::new(channel, id, Kind::Listener { bytes }, owner, app)
    }

    /// A UDP socket; with credit (`metered`), its first pump grants the
    /// initial send credit.
    pub fn udp(channel: Channel, id: SockId, owner: u64, metered: bool, app: String) -> Conn {
        let credit_due = if metered { UDP_SEND_WINDOW } else { 0 };
        let kind = Kind::Udp { pending: VecDeque::new(), pending_bytes: 0, credit_due, metered };
        Conn::new(channel, id, kind, owner, app)
    }

    /// A ping socket.
    pub fn ping(channel: Channel, id: SockId, owner: u64, app: String) -> Conn {
        Conn::new(channel, id, Kind::Ping, owner, app)
    }

    /// Whether the socket takes what other machines start: a listener, or
    /// a UDP socket.
    pub fn receives(&self) -> bool {
        matches!(self.kind, Kind::Listener { .. } | Kind::Udp { .. })
    }

    /// Whether the socket is a TCP connection.
    pub fn is_connection(&self) -> bool {
        matches!(self.kind, Kind::Tcp { .. } | Kind::Bytes(_))
    }

    /// Whether the application may use the network, as the firewall says.
    pub fn set_network(&mut self, may: bool) {
        self.network = may;
    }

    /// What the client asked to make of the socket, if anything: the
    /// service does it, and reads on.
    pub fn take_turn(&mut self) -> Option<Turn> {
        self.turn.take()
    }

    /// For a bound socket, whether its client is a system service; `None`
    /// for any other socket.
    pub fn bound_by_service(&self) -> Option<bool> {
        match self.kind {
            Kind::Bound { service } => Some(service),
            _ => None,
        }
    }

    /// The bound socket listens now (the stack's socket is a listener).
    pub fn listening(&mut self) {
        self.kind = Kind::Listener { bytes: true };
    }

    /// The bound socket is a connection being made now, whose bytes go on
    /// `end`, the service's end of a kernel socket.
    pub fn connecting(&mut self, end: Socket) {
        self.kind = Kind::Bytes(Bytes::new(end, false));
    }

    /// Whether the client must be heard: requests are read only from a
    /// socket that is not finished, and not from a UDP socket without credit
    /// while the datagrams it has not sent yet are many.
    pub fn wants_requests(&self) -> bool {
        match &self.kind {
            Kind::Udp { pending_bytes, metered: false, .. } => !self.done && *pending_bytes < UDP_UNMETERED_WINDOW,
            _ => !self.done,
        }
    }

    /// The kernel socket a connection's bytes go on, and what of it to wait
    /// for: room for the bytes received, the clients' bytes while the stack
    /// has room for them, and the clients' going.
    pub fn bytes_wait(&self) -> Option<(RawHandle, u32)> {
        let Kind::Bytes(b) = &self.kind else { return None };
        let end = b.end.as_ref()?;
        let mut want = 0;
        if b.full {
            want |= WRITABLE;
        }
        if !b.shutdown && !b.held {
            want |= READABLE | PEER_WRITE_DISABLED;
        }
        if !b.gone {
            want |= PEER_CLOSED;
        }
        (want != 0).then(|| (end.raw(), want))
    }

    /// The kernel socket of a connection's bytes raised `observed`.
    pub fn bytes_signalled(&mut self, observed: u32) {
        if let Kind::Bytes(b) = &mut self.kind {
            if observed & WRITABLE != 0 {
                b.full = false;
            }
            if observed & PEER_CLOSED != 0 {
                b.gone = true;
            }
        }
    }

    /// Whether the socket goes on when its client closes the channel: a
    /// connection whose bytes its clients still hold, or that has theirs
    /// still to send; a UDP socket with datagrams still to send, here or in
    /// the stack (closing it drops those).
    pub fn outlives_channel(&self, stack: &Stack) -> bool {
        match &self.kind {
            Kind::Bytes(b) => b.end.is_some() && !(b.gone && b.shutdown),
            Kind::Udp { pending, .. } => !pending.is_empty() || stack.udp_send_queue(self.id) > 0,
            _ => false,
        }
    }

    /// The client closed the channel of a socket that goes on (see
    /// [`Conn::outlives_channel`]): nothing more is sent on the channel, nor
    /// waited for on it.
    pub fn forget_channel(&mut self) {
        self.gone = true;
        self.forgotten = true;
        // Nor is its room waited for.
        self.blocked = false;
    }

    /// Whether the channel is waited on.
    pub fn channel_heard(&self) -> bool {
        !self.forgotten
    }

    /// Whether the socket is over for the service: a connection's clients
    /// closed the kernel socket of its bytes, and those are all in the stack
    /// (which finishes the connection), or it failed with its channel
    /// closed; a UDP socket's client closed its channel, and its datagrams
    /// are all sent (closing it drops what it has not).
    pub fn finished(&self, stack: &Stack) -> bool {
        match &self.kind {
            Kind::Bytes(b) => (b.gone && b.shutdown) || (self.forgotten && (self.done || b.end.is_none())),
            Kind::Udp { pending, .. } => self.forgotten && pending.is_empty() && stack.udp_send_queue(self.id) == 0,
            _ => false,
        }
    }

    /// Whether the socket is to be looked at after every poll until it is
    /// finished: a UDP socket whose client closed its channel, whose
    /// datagrams the stack says nothing of sending.
    pub fn lingers(&self) -> bool {
        self.forgotten && matches!(self.kind, Kind::Udp { .. })
    }

    /// Records an error for the client; it is reported on the next pump,
    /// and the socket is finished.
    pub fn fail(&mut self, error: NetError) {
        if !self.done {
            self.failure.get_or_insert(error);
        }
    }

    /// The client broke the protocol: its TCP stream is no longer what it
    /// meant to send, so the connection is reset rather than ended.
    fn violation(&mut self, stack: &mut Stack) {
        if matches!(self.kind, Kind::Tcp { .. } | Kind::Bytes(_)) {
            stack.tcp_abort(self.id);
        }
        self.fail(NetError::ProtocolViolation);
    }

    /// Handles the client's requests, up to one the service is to do
    /// ([`Conn::take_turn`]). Returns `false` when the client closed the
    /// channel (and every request it sent before was handled).
    pub fn handle_requests(&mut self, stack: &mut Stack) -> bool {
        for _ in 0..REQUEST_BUDGET {
            if self.turn.is_some() {
                return true;
            }
            let msg = match self.channel.read() {
                Ok(m) => m,
                Err(Error::ShouldWait) => return true,
                Err(_) => return false,
            };
            if self.done || self.failure.is_some() {
                continue;
            }
            if !self.wants_requests() {
                // Read on once the datagrams waiting have gone (after this
                // one, handled as the others).
                if let Ok((_, req)) = ipc::decode_event::<SocketRequest>(msg) {
                    self.handle(stack, req);
                }
                return true;
            }
            match ipc::decode_event::<SocketRequest>(msg) {
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
            (Kind::Udp { pending, pending_bytes, credit_due, metered }, SocketRequest::SendTo { dest, data }) => {
                let n = data.0.len();
                if n > proto::net::MAX_DATAGRAM || (*metered && n as u32 > self.credit_out) {
                    return self.violation(stack);
                }
                if *metered {
                    self.credit_out -= n as u32;
                }
                if !self.network && !dest.ip().is_loopback() {
                    // Kept off the network: dropped, its credit returned as
                    // if sent.
                    stack.note_app_blocked(17, dest, &self.app);
                    if *metered {
                        *credit_due += n as u32;
                    }
                    return;
                }
                *pending_bytes += n;
                pending.push_back((dest, data.0));
            }
            (Kind::Tcp { .. } | Kind::Udp { metered: true, .. }, SocketRequest::Consumed { bytes }) => {
                self.rx_outstanding = self.rx_outstanding.saturating_sub(bytes);
            }
            // Credit a datagram socket without it does not owe.
            (Kind::Udp { metered: false, .. }, SocketRequest::Consumed { .. }) => {}
            (Kind::Tcp { shutdown, .. }, SocketRequest::Shutdown {}) => {
                if !*shutdown {
                    *shutdown = true;
                    stack.tcp_shutdown(self.id);
                }
            }
            (Kind::Tcp { .. } | Kind::Bytes(_), SocketRequest::Abort {}) => stack.tcp_abort(self.id),
            (Kind::Tcp { .. } | Kind::Bytes(_), SocketRequest::SetNoDelay { nodelay }) => {
                stack.tcp_set_nodelay(self.id, nodelay)
            }
            (Kind::Ping, SocketRequest::Echo { dest, seq, ttl, size }) => {
                stack.ping_send(self.id, dest, seq, ttl, size)
            }
            // The processes that share a bound socket may each ask it to
            // listen.
            (
                Kind::Bound { .. } | Kind::Listener { bytes: true },
                SocketRequest::Listen { backlog, options, reply },
            ) => self.turn = Some(Turn::Listen { backlog, options, reply }),
            (Kind::Bound { .. }, SocketRequest::Connect { remote, options, bytes }) => {
                self.turn = Some(Turn::Connect { remote, options, bytes })
            }
            _ => self.violation(stack),
        }
    }

    /// Moves data and news between the stack and the client, as far as the
    /// client's channel has room. `room` is how many more sockets the
    /// owner may have (for the connections a listener accepts); accepted
    /// connections are added to `accepted`.
    pub fn pump(&mut self, stack: &mut Stack, events: &mut Events, room: &mut usize, accepted: &mut Vec<Conn>) {
        // What goes on without the channel goes on (see `outlives_channel`).
        if self.done || (self.gone && !self.forgotten && !matches!(self.kind, Kind::Bytes(_))) {
            return;
        }
        if self.failure.is_none() {
            let sent = match self.kind {
                // Nothing comes to a bound socket.
                Kind::Bound { .. } => Sent::Yes,
                Kind::Tcp { .. } => self.pump_tcp(stack, events),
                Kind::Bytes(_) => self.pump_bytes(stack, events),
                Kind::Listener { bytes } => self.pump_listener(stack, events, room, accepted, bytes),
                Kind::Udp { .. } => self.pump_udp(stack, events),
                Kind::Ping => self.pump_ping(stack, events),
            };
            match sent {
                Sent::Yes => {}
                Sent::Full => return self.blocked = true,
                // The client closed its channel, perhaps after its last
                // requests: those are still to be handled, and the socket
                // closed after them (see `handle_requests`), not finished
                // here.
                Sent::Gone => return self.gone = true,
            }
        }
        // The error that ends the socket, after everything before it; then
        // the end of a connection's bytes, which its clients see end.
        if let Some(error) = self.failure {
            match events.send(&self.channel, |e| SocketEvent::Error { error }.encode(e)) {
                Sent::Yes | Sent::Gone => {
                    self.failure = None;
                    self.done = true;
                    if let Kind::Bytes(b) = &mut self.kind {
                        b.end = None;
                    }
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

    fn pump_bytes(&mut self, stack: &mut Stack, events: &mut Events) -> Sent {
        let id = self.id;
        let Some(view) = stack.tcp_view(id) else {
            self.fail(NetError::Closed);
            return Sent::Yes;
        };
        let Kind::Bytes(b) = &mut self.kind else { return Sent::Yes };
        match view.state {
            TcpState::Connecting => return Sent::Yes,
            TcpState::Open { local, remote } if !b.announced => {
                match events.send(&self.channel, |e| SocketEvent::Connected { local, remote, credit: 0 }.encode(e)) {
                    Sent::Full => return Sent::Full,
                    // Nobody hears it (the clients closed the channel): the
                    // bytes go on all the same.
                    Sent::Yes | Sent::Gone => b.announced = true,
                }
            }
            _ => {}
        }
        let (closed, error) = match view.state {
            TcpState::Closed { error } => (true, error),
            _ => (false, None),
        };
        let Some(end) = &b.end else { return Sent::Yes };
        // Received bytes, as far as the kernel socket has room.
        while !b.full {
            let mut full = false;
            let n = stack
                .tcp_recv_with(id, CHUNK, |data| match end.write(data) {
                    Ok(n) => n,
                    Err(Error::ShouldWait) => {
                        full = true;
                        0
                    }
                    // Nobody reads any more: dropped, as on Linux data that
                    // comes to a socket closed.
                    Err(_) => data.len(),
                })
                .unwrap_or(0);
            b.full = full;
            if n == 0 {
                break;
            }
        }
        let Some(view) = stack.tcp_view(id) else { return Sent::Yes };
        if view.recv_queued == 0 {
            if let Some(error) = error {
                // After what was received, the error; then the end, which the
                // clients see end (see `pump`).
                if b.gone {
                    b.shutdown = true;
                }
                self.fail(error);
                return Sent::Yes;
            }
            // The end of the received stream, once all of it went on.
            if !b.eof_sent && (view.eof || closed) {
                let _ = end.shutdown();
                b.eof_sent = true;
            }
        }
        // The clients' bytes, as far as the stack has room: all of them, and
        // then their stream's end, however soon the clients went.
        b.held = false;
        let mut chunk = [0u8; CHUNK];
        while !b.shutdown && !closed {
            let room = stack.tcp_view(id).map_or(0, |v| v.send_space).min(CHUNK);
            if room == 0 {
                b.held = true;
                break;
            }
            match end.read(&mut chunk[..room]) {
                // The room asked for: all of it fits.
                Ok(n) => {
                    let _ = stack.tcp_send(id, &chunk[..n]);
                }
                Err(Error::ShouldWait) => break,
                // The clients' stream ended: they stopped writing, or every
                // one of them closed it.
                Err(_) => {
                    stack.tcp_shutdown(id);
                    b.shutdown = true;
                }
            }
        }
        if closed && b.gone {
            // Nowhere to send what is left.
            b.shutdown = true;
        }
        Sent::Yes
    }

    fn pump_listener(
        &mut self,
        stack: &mut Stack,
        events: &mut Events,
        room: &mut usize,
        accepted: &mut Vec<Conn>,
        bytes: bool,
    ) -> Sent {
        while let Some(new) = stack.tcp_accept_peek(self.id) {
            let view = stack.tcp_view(new);
            let Some(TcpState::Open { local, remote }) = view.map(|v| v.state) else {
                // Gone before it was handed over.
                stack.tcp_accept(self.id);
                stack.close(new);
                continue;
            };
            // The owner has as many sockets as it may, or its application is
            // kept off the network (the firewall turns away what comes for
            // it before it is a connection): refuse it.
            if *room == 0 || (!self.network && !remote.ip().is_loopback()) {
                stack.tcp_accept(self.id);
                stack.tcp_abort(new);
                stack.close(new);
                continue;
            }
            if bytes {
                // A connection its clients may share: a shared channel, and a
                // kernel socket for its bytes.
                let (Ok((ours, theirs)), Ok((end, theirs_end))) = (Channel::create_shared(), Socket::create()) else {
                    return Sent::Yes;
                };
                let ev = SocketEvent::AcceptedBytes { socket: theirs, bytes: theirs_end, local, remote };
                match events.send(&self.channel, |e| ev.encode(e)) {
                    Sent::Yes => {}
                    // Still queued: offered again (on new ends) when the
                    // client has room.
                    other => return other,
                }
                stack.tcp_accept(self.id);
                *room -= 1;
                let mut conn = Conn::new(ours, new, Kind::Bytes(Bytes::new(end, true)), self.owner, self.app.clone());
                conn.network = self.network;
                accepted.push(conn);
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
            let kind = Kind::Tcp { announced: true, eof_sent: false, shutdown: false };
            let mut conn = Conn::new(ours, new, kind, self.owner, self.app.clone());
            conn.credit_out = credit;
            conn.network = self.network;
            accepted.push(conn);
        }
        Sent::Yes
    }

    fn pump_udp(&mut self, stack: &mut Stack, events: &mut Events) -> Sent {
        let metered = matches!(self.kind, Kind::Udp { metered: true, .. });
        // The client's datagrams first: those it sent before it closed the
        // channel go all the same.
        if let Kind::Udp { pending, pending_bytes, credit_due, metered } = &mut self.kind {
            while let Some((dest, data)) = pending.pop_front() {
                if let Err(NetError::LimitReached) = stack.udp_send(self.id, dest, &data) {
                    // The stack's buffer is full: it says when it has sent
                    // some.
                    pending.push_front((dest, data));
                    break;
                }
                // Sent, or undeliverable and dropped, as on any system.
                *pending_bytes -= data.len();
                if *metered {
                    *credit_due += data.len() as u32;
                }
            }
        }
        if self.forgotten {
            // Nobody to hear what comes.
            return Sent::Yes;
        }
        // Received datagrams, within the client's window (if it has credit)
        // and its channel's room; a datagram is taken from the stack only
        // once it is queued.
        while !metered || self.rx_outstanding < RECEIVE_WINDOW {
            let mut sent = Sent::Yes;
            let mut len = 0;
            let channel = &self.channel;
            let network = self.network;
            let taken = stack.udp_recv_with(self.id, |from, data| {
                if !network && !from.ip().is_loopback() {
                    // Kept off the network: what came before that is
                    // dropped.
                    return true;
                }
                len = data.len();
                sent = events.send(channel, |e| SocketEvent::encode_datagram(from, data, e));
                sent == Sent::Yes
            });
            match taken {
                None => break,
                Some(true) if metered => self.rx_outstanding += len as u32,
                Some(true) => {}
                Some(false) => return sent,
            }
        }
        let Kind::Udp { credit_due, .. } = &mut self.kind else { return Sent::Yes };
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
