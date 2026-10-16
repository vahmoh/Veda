// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Sockets: the TCP, UDP and ping operations behind `netd`'s socket
//! channels.
//!
//! smoltcp sockets live in the socket set of one interface, so:
//!
//! * a TCP connection is created on the interface its route picks;
//! * a TCP listener or UDP socket bound to the unspecified address gets one
//!   smoltcp socket per interface (and new interfaces get one too), so it
//!   is reachable everywhere;
//! * a ping socket gets an ICMP socket on each interface it sends through.
//!
//! smoltcp closes a socket the same way after a reset and after a timeout;
//! the stack tells them apart (refused, reset, timed out) from its own
//! bookkeeping, so applications get accurate errors.
//!
//! Received data is never lost while the client may still read it: a
//! connection that closed normally keeps what it received after its smoltcp
//! socket is freed, and its end of stream is the peer's FIN, whatever is
//! still buffered. Data is handed out in place ([`Stack::tcp_recv_with`],
//! [`Stack::udp_recv_with`]) and dequeued only as far as the caller took it.
//!
//! A connection whose client closed it (an *orphan*) finishes sending on its
//! own, within bounds: it is reset if the peer sends more data (nobody will
//! read it), and dropped after [`ORPHAN_TIMEOUT_US`] without progress or when
//! more than [`MAX_ORPHANS`] are waiting.
//!
//! [`Stack::take_ready`] lists the sockets whose state changed since it was
//! last asked (data arrived, room to send, connected, failed, ...), so the
//! service attends to those alone.

use alloc::collections::{BTreeMap, BTreeSet, VecDeque, btree_map};
use alloc::string::String;
use alloc::vec::Vec;
use core::net::{IpAddr, Ipv6Addr, SocketAddr};

use smoltcp::iface::SocketHandle;
use smoltcp::phy::ChecksumCapabilities;
use smoltcp::socket::{icmp, tcp, udp};
use smoltcp::time::Duration;
use smoltcp::wire::{
    Icmpv4DstUnreachable, Icmpv4Packet, Icmpv4Repr, Icmpv6Message, Icmpv6Packet, Icmpv6Repr, IpAddress, IpEndpoint,
    IpListenEndpoint,
};
use vproto::net::{NetError, SocketInfo, SocketKind, TcpOptions};

use crate::{IfaceId, Stack};

/// Identifies a socket.
pub type SockId = u32;

pub const TCP_RX_BUFFER: usize = 64 * 1024;
pub const TCP_TX_BUFFER: usize = 64 * 1024;
const UDP_PACKETS: usize = 64;
const UDP_BYTES: usize = 64 * 1024;
const ICMP_PACKETS: usize = 16;
const ICMP_BYTES: usize = 16 * 1024;
/// Most sockets of all kinds (connections whose client closed them
/// included).
pub const MAX_SOCKETS: usize = 1024;
/// Most listening sockets per interface for one listener, and most
/// connections waiting to be accepted.
const LISTEN_PER_IFACE: usize = 8;
const DEFAULT_CONNECT_TIMEOUT_US: u64 = 20_000_000;
/// Give up on a connection whose peer acknowledged nothing for this long
/// while something was waiting for its acknowledgement.
const TCP_USER_TIMEOUT: Duration = Duration::from_secs(120);
/// The shortest keep-alive interval a client may ask for: shorter ones
/// would flood the network.
pub const MIN_KEEPALIVE_MS: u32 = 1000;
/// An orphan (a connection whose client closed it) that makes no progress
/// for this long is dropped (Linux's FIN timeout is the same).
pub const ORPHAN_TIMEOUT_US: u64 = 60_000_000;
/// Most orphans; beyond this, the oldest is dropped.
pub const MAX_ORPHANS: usize = 256;
/// Most ping results waiting for the client.
const MAX_PING_EVENTS: usize = 64;
const PING_TIMEOUT_US: u64 = 4_000_000;
const EPHEMERAL_FIRST: u16 = 49152;
const EPHEMERAL_COUNT: u32 = 16384;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Proto {
    Tcp,
    Udp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    Connecting,
    Open,
    Closed,
}

pub(crate) struct TcpSock {
    pub iface: IfaceId,
    pub handle: Option<SocketHandle>,
    /// Local port reserved in the table (0: owned by a listener).
    pub port: u16,
    pub phase: Phase,
    pub connect_deadline_us: u64,
    /// We sent our FIN (or reset it): a later `Closed` may be the normal
    /// end.
    pub closed_by_us: bool,
    /// We reset it.
    pub aborted: bool,
    /// The client is gone, since this time: the connection finishes on its
    /// own, within bounds.
    pub orphan_since: Option<u64>,
    /// When data last moved (received, or acknowledged by the peer).
    pub progress_us: u64,
    pub last_rx_queue: usize,
    pub last_tx_queue: usize,
    pub error: Option<NetError>,
    pub local: Option<SocketAddr>,
    pub remote: Option<SocketAddr>,
    pub idle_timeout_us: u64,
    pub owner: u64,
    /// The peer finished sending (its FIN arrived).
    pub peer_fin: bool,
    /// Received data the client had not taken when the connection closed
    /// normally (its smoltcp socket is freed by then).
    pub leftover: VecDeque<u8>,
}

pub(crate) struct Listener {
    pub local: SocketAddr,
    pub backlog: usize,
    pub members: Vec<(IfaceId, SocketHandle)>,
    pub accepted: VecDeque<SockId>,
    pub opts: TcpOptions,
    pub owner: u64,
}

pub(crate) struct UdpSock {
    pub local: SocketAddr,
    pub members: Vec<(IfaceId, SocketHandle)>,
    pub owner: u64,
    /// Bytes of datagrams received, last seen (to tell new arrivals).
    pub last_rx: usize,
    /// A datagram did not fit the send buffer when it held this many bytes:
    /// the socket is ready again once it holds fewer.
    pub full_at: Option<usize>,
}

pub(crate) struct PingSock {
    pub ident: u16,
    pub members: Vec<(IfaceId, SocketHandle)>,
    /// Outstanding requests: seq -> (sent at, destination).
    pub pending: BTreeMap<u16, (u64, IpAddr)>,
    pub events: VecDeque<PingEvent>,
    pub owner: u64,
}

pub(crate) enum Entry {
    Tcp(TcpSock),
    Listener(Listener),
    Udp(UdpSock),
    Ping(PingSock),
}

/// The socket table.
pub(crate) struct Table {
    pub map: BTreeMap<SockId, Entry>,
    next: SockId,
    /// Reserved local ports and how many sockets use each (the resolver's
    /// query sockets included).
    ports: BTreeMap<(Proto, u16), u32>,
    /// Sockets whose state changed since [`Stack::take_ready`] was last
    /// called.
    ready: BTreeSet<SockId>,
}

impl Table {
    pub fn new() -> Table {
        Table { map: BTreeMap::new(), next: 1, ports: BTreeMap::new(), ready: BTreeSet::new() }
    }

    fn insert(&mut self, e: Entry) -> SockId {
        loop {
            let id = self.next;
            self.next = self.next.wrapping_add(1).max(1);
            if let btree_map::Entry::Vacant(slot) = self.map.entry(id) {
                slot.insert(e);
                return id;
            }
        }
    }

    pub(crate) fn reserve(&mut self, proto: Proto, port: u16) {
        *self.ports.entry((proto, port)).or_insert(0) += 1;
    }

    pub(crate) fn release(&mut self, proto: Proto, port: u16) {
        if port == 0 {
            return;
        }
        if let Some(n) = self.ports.get_mut(&(proto, port)) {
            *n -= 1;
            if *n == 0 {
                self.ports.remove(&(proto, port));
            }
        }
    }

    fn in_use(&self, proto: Proto, port: u16) -> bool {
        self.ports.contains_key(&(proto, port))
    }

    fn mark_ready(&mut self, id: SockId) {
        self.ready.insert(id);
    }
}

/// The state of a TCP connection as seen by the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TcpState {
    Connecting,
    Open {
        local: SocketAddr,
        remote: SocketAddr,
    },
    /// Closed, with the reason unless it closed normally (received data may
    /// still be waiting).
    Closed {
        error: Option<NetError>,
    },
}

/// What a client of a TCP socket needs to know after a poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TcpView {
    pub state: TcpState,
    /// Bytes ready to be received.
    pub recv_queued: usize,
    /// The peer finished sending and everything was received.
    pub eof: bool,
    /// Bytes the send buffer can take now.
    pub send_space: usize,
    /// Bytes sent but not yet acknowledged, or not yet sent.
    pub send_queued: usize,
}

/// The outcome of an echo request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PingEvent {
    Reply {
        from: IpAddr,
        seq: u16,
        size: u16,
        rtt_us: u32,
    },
    /// The request failed: no answer in time, or a router (`from`) said why
    /// it could not deliver it.
    Failed {
        seq: u16,
        from: Option<IpAddr>,
        error: NetError,
    },
}

fn tcp_socket() -> tcp::Socket<'static> {
    tcp::Socket::new(
        tcp::SocketBuffer::new(alloc::vec![0u8; TCP_RX_BUFFER]),
        tcp::SocketBuffer::new(alloc::vec![0u8; TCP_TX_BUFFER]),
    )
}

fn udp_socket(packets: usize, bytes: usize) -> udp::Socket<'static> {
    udp::Socket::new(
        udp::PacketBuffer::new(alloc::vec![udp::PacketMetadata::EMPTY; packets], alloc::vec![0u8; bytes]),
        udp::PacketBuffer::new(alloc::vec![udp::PacketMetadata::EMPTY; packets], alloc::vec![0u8; bytes]),
    )
}

fn icmp_socket() -> icmp::Socket<'static> {
    icmp::Socket::new(
        icmp::PacketBuffer::new(alloc::vec![icmp::PacketMetadata::EMPTY; ICMP_PACKETS], alloc::vec![0u8; ICMP_BYTES]),
        icmp::PacketBuffer::new(alloc::vec![icmp::PacketMetadata::EMPTY; ICMP_PACKETS], alloc::vec![0u8; ICMP_BYTES]),
    )
}

/// Checks a client's TCP options.
fn validate(opts: &TcpOptions) -> Result<(), NetError> {
    if opts.keepalive_ms != 0 && opts.keepalive_ms < MIN_KEEPALIVE_MS {
        return Err(NetError::InvalidArgument);
    }
    Ok(())
}

fn apply_options(s: &mut tcp::Socket, opts: &TcpOptions) {
    s.set_nagle_enabled(!opts.nodelay);
    s.set_keep_alive(if opts.keepalive_ms > 0 { Some(Duration::from_millis(opts.keepalive_ms as u64)) } else { None });
    s.set_timeout(Some(TCP_USER_TIMEOUT));
    s.set_ack_delay(Some(Duration::from_millis(10)));
}

fn state_name(s: tcp::State) -> &'static str {
    match s {
        tcp::State::Closed => "CLOSED",
        tcp::State::Listen => "LISTEN",
        tcp::State::SynSent => "SYN-SENT",
        tcp::State::SynReceived => "SYN-RECEIVED",
        tcp::State::Established => "ESTABLISHED",
        tcp::State::FinWait1 => "FIN-WAIT-1",
        tcp::State::FinWait2 => "FIN-WAIT-2",
        tcp::State::CloseWait => "CLOSE-WAIT",
        tcp::State::Closing => "CLOSING",
        tcp::State::LastAck => "LAST-ACK",
        tcp::State::TimeWait => "TIME-WAIT",
    }
}

/// Throws away what a smoltcp socket received.
fn discard_received(s: &mut tcp::Socket) {
    // The buffer is a ring: at most two runs.
    for _ in 0..2 {
        if s.recv(|data| (data.len(), data.len())).unwrap_or(0) == 0 {
            break;
        }
    }
}

/// Takes everything still in a smoltcp socket's receive buffer.
fn take_received(s: &mut tcp::Socket) -> VecDeque<u8> {
    let mut out = VecDeque::with_capacity(s.recv_queue());
    // The buffer is a ring: at most two runs.
    for _ in 0..2 {
        let n = s
            .recv(|data| {
                out.extend(data.iter());
                (data.len(), data.len())
            })
            .unwrap_or(0);
        if n == 0 {
            break;
        }
    }
    out
}

/// What came back for an echo request.
enum PingAnswer {
    Reply {
        seq: u16,
        size: usize,
    },
    /// A router (or the destination) could not deliver it.
    Error {
        seq: u16,
        error: NetError,
    },
}

/// What an ICMP packet received by a ping socket of identifier `ident`
/// says: the reply to a request, or an error about one (whose quoted
/// request tells which).
fn ping_answer(packet: &[u8], from: IpAddress, ident: u16) -> Option<PingAnswer> {
    // The identifier and sequence number of the echo request an error
    // quotes (ICMP type `request`), after the quoted IP header.
    let quoted = |data: &[u8], request: u8| {
        (data.len() >= 8 && data[0] == request && u16::from_be_bytes([data[4], data[5]]) == ident)
            .then(|| u16::from_be_bytes([data[6], data[7]]))
    };
    match from {
        IpAddress::Ipv4(_) => {
            let p = Icmpv4Packet::new_checked(packet).ok()?;
            match Icmpv4Repr::parse(&p, &ChecksumCapabilities::default()).ok()? {
                Icmpv4Repr::EchoReply { ident: i, seq_no, data } if i == ident => {
                    Some(PingAnswer::Reply { seq: seq_no, size: data.len() })
                }
                Icmpv4Repr::DstUnreachable { reason, data, .. } => {
                    let error = match reason {
                        Icmpv4DstUnreachable::HostUnreachable | Icmpv4DstUnreachable::DstHostUnknown => {
                            NetError::HostUnreachable
                        }
                        _ => NetError::DestinationUnreachable,
                    };
                    Some(PingAnswer::Error { seq: quoted(data, 8)?, error })
                }
                Icmpv4Repr::TimeExceeded { data, .. } => {
                    Some(PingAnswer::Error { seq: quoted(data, 8)?, error: NetError::TimeExceeded })
                }
                _ => None,
            }
        }
        // The checksum was verified when the packet arrived.
        IpAddress::Ipv6(_) => {
            let p = Icmpv6Packet::new_checked(packet).ok()?;
            match p.msg_type() {
                Icmpv6Message::EchoReply if p.echo_ident() == ident => {
                    Some(PingAnswer::Reply { seq: p.echo_seq_no(), size: p.payload().len() })
                }
                // An error's body: the quoted IPv6 header (40 bytes), then
                // the request.
                Icmpv6Message::DstUnreachable | Icmpv6Message::TimeExceeded => {
                    let error = match (p.msg_type(), p.msg_code()) {
                        (Icmpv6Message::TimeExceeded, _) => NetError::TimeExceeded,
                        // Address unreachable.
                        (_, 3) => NetError::HostUnreachable,
                        _ => NetError::DestinationUnreachable,
                    };
                    let quote = p.payload().get(40..)?;
                    Some(PingAnswer::Error { seq: quoted(quote, 128)?, error })
                }
                _ => None,
            }
        }
    }
}

/// Whether a local address is acceptable for binding: unspecified, or one
/// of the machine's addresses.
fn bind_ok(stack: &Stack, addr: IpAddr) -> bool {
    addr.is_unspecified() || stack.iface_ids().iter().any(|&id| stack.iface(id).unwrap().iface.has_ip_addr(addr))
}

/// Interfaces a socket bound to `local` should be present on.
fn ifaces_for(stack: &Stack, local: IpAddr) -> Vec<IfaceId> {
    stack
        .iface_ids()
        .into_iter()
        .filter(|&id| local.is_unspecified() || stack.iface(id).unwrap().iface.has_ip_addr(local))
        .collect()
}

impl Stack {
    pub(crate) fn alloc_port(&mut self, proto: Proto) -> Result<u16, NetError> {
        let start = self.random_u32() % EPHEMERAL_COUNT;
        for k in 0..EPHEMERAL_COUNT {
            let port = EPHEMERAL_FIRST + ((start + k) % EPHEMERAL_COUNT) as u16;
            if !self.socks.in_use(proto, port) {
                return Ok(port);
            }
        }
        Err(NetError::AddressInUse)
    }

    fn check_limit(&self) -> Result<(), NetError> {
        if self.socks.map.len() >= MAX_SOCKETS { Err(NetError::LimitReached) } else { Ok(()) }
    }

    /// Sockets in the table (of all clients, and connections whose client
    /// closed them), at most [`MAX_SOCKETS`].
    pub fn socket_count(&self) -> usize {
        self.socks.map.len()
    }

    /// The sockets whose state changed since the last call (data arrived,
    /// room to send, connected, closed, an accepted connection, a ping
    /// result).
    pub fn take_ready(&mut self) -> Vec<SockId> {
        core::mem::take(&mut self.socks.ready).into_iter().collect()
    }

    // ---------------------------------------------------------------
    // TCP

    /// Starts a TCP connection to `remote` for process `owner`.
    pub fn tcp_connect(&mut self, remote: SocketAddr, opts: &TcpOptions, owner: u64) -> Result<SockId, NetError> {
        if remote.port() == 0 {
            return Err(NetError::InvalidArgument);
        }
        validate(opts)?;
        self.check_limit()?;
        let id_if = self.route(remote.ip())?;
        let port = self.alloc_port(Proto::Tcp)?;
        let now = self.now_us;
        let i = self.iface_mut(id_if).ok_or(NetError::NoRoute)?;
        let mut s = tcp_socket();
        apply_options(&mut s, opts);
        s.connect(i.iface.context(), IpEndpoint::from(remote), port).map_err(|e| match e {
            tcp::ConnectError::Unaddressable => NetError::NoRoute,
            tcp::ConnectError::InvalidState => NetError::Internal,
        })?;
        let handle = i.sockets.add(s);
        self.socks.reserve(Proto::Tcp, port);
        let timeout = if opts.connect_timeout_ms > 0 {
            opts.connect_timeout_ms as u64 * 1000
        } else {
            DEFAULT_CONNECT_TIMEOUT_US
        };
        Ok(self.socks.insert(Entry::Tcp(TcpSock {
            iface: id_if,
            handle: Some(handle),
            port,
            phase: Phase::Connecting,
            connect_deadline_us: now + timeout,
            closed_by_us: false,
            aborted: false,
            orphan_since: None,
            progress_us: now,
            last_rx_queue: 0,
            last_tx_queue: 0,
            error: None,
            local: None,
            remote: Some(remote),
            idle_timeout_us: opts.idle_timeout_ms as u64 * 1000,
            owner,
            peer_fin: false,
            leftover: VecDeque::new(),
        })))
    }

    /// Listens on `local` (port 0 picks a free one) for process `owner`.
    /// Returns the listener and the address bound.
    pub fn tcp_listen(
        &mut self,
        local: SocketAddr,
        backlog: u32,
        opts: &TcpOptions,
        owner: u64,
    ) -> Result<(SockId, SocketAddr), NetError> {
        validate(opts)?;
        self.check_limit()?;
        if !bind_ok(self, local.ip()) {
            return Err(NetError::AddressNotAvailable);
        }
        let port = if local.port() == 0 { self.alloc_port(Proto::Tcp)? } else { local.port() };
        let listening = self.socks.map.values().any(|e| matches!(e, Entry::Listener(l) if l.local.port() == port));
        if listening {
            return Err(NetError::AddressInUse);
        }
        let local = SocketAddr::new(local.ip(), port);
        let backlog = (backlog as usize).clamp(1, LISTEN_PER_IFACE);
        let mut l = Listener { local, backlog, members: Vec::new(), accepted: VecDeque::new(), opts: *opts, owner };
        for id in ifaces_for(self, local.ip()) {
            self.add_listen_members(&mut l, id);
        }
        if l.members.is_empty() {
            return Err(NetError::AddressNotAvailable);
        }
        self.socks.reserve(Proto::Tcp, port);
        Ok((self.socks.insert(Entry::Listener(l)), local))
    }

    /// Gives a listener listening sockets on interface `id`: as many as its
    /// backlog has room for, counting the connections not accepted yet, so
    /// that a client that does not accept holds at most its backlog (further
    /// connection attempts are refused).
    fn add_listen_members(&mut self, l: &mut Listener, id: IfaceId) {
        let have = l.members.iter().filter(|(i, _)| *i == id).count();
        let want = l.backlog.saturating_sub(l.accepted.len());
        let Some(i) = self.iface_mut(id) else { return };
        for _ in have..want {
            let mut s = tcp_socket();
            apply_options(&mut s, &l.opts);
            let ep = if l.local.ip().is_unspecified() {
                IpListenEndpoint { addr: None, port: l.local.port() }
            } else {
                IpListenEndpoint { addr: Some(l.local.ip().into()), port: l.local.port() }
            };
            if s.listen(ep).is_ok() {
                l.members.push((id, i.sockets.add(s)));
            }
        }
    }

    /// The next connection a listener has accepted, without taking it.
    pub fn tcp_accept_peek(&self, listener: SockId) -> Option<SockId> {
        match self.socks.map.get(&listener) {
            Some(Entry::Listener(l)) => l.accepted.front().copied(),
            _ => None,
        }
    }

    /// Takes the next connection a listener has accepted: from now on it is
    /// the client's.
    pub fn tcp_accept(&mut self, listener: SockId) -> Option<SockId> {
        match self.socks.map.get_mut(&listener) {
            Some(Entry::Listener(l)) => l.accepted.pop_front(),
            _ => None,
        }
    }

    fn tcp_sock(&self, id: SockId) -> Option<&TcpSock> {
        match self.socks.map.get(&id) {
            Some(Entry::Tcp(t)) => Some(t),
            _ => None,
        }
    }

    fn with_tcp<R>(&mut self, id: SockId, f: impl FnOnce(&mut tcp::Socket<'static>) -> R) -> Option<R> {
        let (iface, handle) = match self.socks.map.get(&id) {
            Some(Entry::Tcp(t)) => (t.iface, t.handle?),
            _ => return None,
        };
        let i = self.iface_mut(iface)?;
        Some(f(i.sockets.get_mut::<tcp::Socket>(handle)))
    }

    /// The client's view of a TCP connection.
    pub fn tcp_view(&mut self, id: SockId) -> Option<TcpView> {
        let t = self.tcp_sock(id)?;
        let (phase, error, local, remote, peer_fin, leftover) =
            (t.phase, t.error, t.local, t.remote, t.peer_fin, t.leftover.len());
        let numbers =
            self.with_tcp(id, |s| (s.recv_queue(), s.send_capacity() - s.send_queue(), s.send_queue(), s.may_send()));
        let (recv_queued, send_space, send_queued, may_send) = numbers.unwrap_or((0, 0, 0, false));
        let recv_queued = recv_queued + leftover;
        let state = match phase {
            Phase::Connecting => TcpState::Connecting,
            Phase::Open => match (local, remote) {
                (Some(local), Some(remote)) => TcpState::Open { local, remote },
                _ => TcpState::Connecting,
            },
            Phase::Closed => TcpState::Closed { error },
        };
        Some(TcpView {
            state,
            recv_queued,
            eof: peer_fin && recv_queued == 0,
            send_space: if may_send { send_space } else { 0 },
            send_queued,
        })
    }

    /// Appends data to a connection's send buffer; returns how much fit.
    pub fn tcp_send(&mut self, id: SockId, data: &[u8]) -> Result<usize, NetError> {
        let t = self.tcp_sock(id).ok_or(NetError::Closed)?;
        if t.phase == Phase::Closed {
            return Err(t.error.unwrap_or(NetError::Closed));
        }
        let sent = match self.with_tcp(id, |s| s.send_slice(data)) {
            Some(Ok(n)) => n,
            Some(Err(_)) | None => return Err(NetError::Closed),
        };
        // What is queued now has been seen: an acknowledgement that takes
        // some of it, even in the same poll, makes the socket ready.
        if let (Some(queued), Some(Entry::Tcp(t))) =
            (self.with_tcp(id, |s| s.send_queue()), self.socks.map.get_mut(&id))
        {
            t.last_tx_queue = t.last_tx_queue.max(queued);
        }
        Ok(sent)
    }

    /// Offers up to `max` received bytes (a contiguous run) to `take`,
    /// which returns how many it took; only those are dequeued. Returns the
    /// number taken (0: nothing received, or `take` took nothing).
    pub fn tcp_recv_with(
        &mut self,
        id: SockId,
        max: usize,
        take: impl FnOnce(&[u8]) -> usize,
    ) -> Result<usize, NetError> {
        let leftover = match self.socks.map.get_mut(&id) {
            Some(Entry::Tcp(t)) => &mut t.leftover,
            _ => return Err(NetError::Closed),
        };
        if !leftover.is_empty() {
            let (run, _) = leftover.as_slices();
            let k = run.len().min(max);
            let n = take(&run[..k]).min(k);
            leftover.drain(..n);
            return Ok(n);
        }
        let n = self.with_tcp(id, |s| {
            s.recv(|data| {
                let k = data.len().min(max);
                let n = take(&data[..k]).min(k);
                (n, n)
            })
            .unwrap_or(0)
        });
        // What is left has been seen: data that comes after it, even in the
        // same poll, makes the socket ready.
        if let (Some(left), Some(Entry::Tcp(t))) = (self.with_tcp(id, |s| s.recv_queue()), self.socks.map.get_mut(&id))
        {
            t.last_rx_queue = t.last_rx_queue.min(left);
        }
        Ok(n.unwrap_or(0))
    }

    /// Finishes sending (FIN after the data queued); receiving continues.
    pub fn tcp_shutdown(&mut self, id: SockId) {
        if let Some(Entry::Tcp(t)) = self.socks.map.get_mut(&id) {
            t.closed_by_us = true;
        }
        self.with_tcp(id, |s| s.close());
    }

    /// Drops a connection at once (RST).
    pub fn tcp_abort(&mut self, id: SockId) {
        if let Some(Entry::Tcp(t)) = self.socks.map.get_mut(&id) {
            t.closed_by_us = true;
            t.aborted = true;
        }
        self.with_tcp(id, |s| s.abort());
    }

    pub fn tcp_set_nodelay(&mut self, id: SockId, nodelay: bool) {
        self.with_tcp(id, |s| s.set_nagle_enabled(!nodelay));
    }

    // ---------------------------------------------------------------
    // UDP

    /// Opens a UDP socket bound to `local` (port 0 picks a free one) for
    /// process `owner`.
    pub fn udp_bind(&mut self, local: SocketAddr, owner: u64) -> Result<(SockId, SocketAddr), NetError> {
        self.check_limit()?;
        if !bind_ok(self, local.ip()) {
            return Err(NetError::AddressNotAvailable);
        }
        let port = if local.port() == 0 { self.alloc_port(Proto::Udp)? } else { local.port() };
        if self.socks.in_use(Proto::Udp, port) {
            return Err(NetError::AddressInUse);
        }
        let local = SocketAddr::new(local.ip(), port);
        let mut u = UdpSock { local, members: Vec::new(), owner, last_rx: 0, full_at: None };
        for id in ifaces_for(self, local.ip()) {
            self.add_udp_member(&mut u, id);
        }
        self.socks.reserve(Proto::Udp, port);
        Ok((self.socks.insert(Entry::Udp(u)), local))
    }

    fn add_udp_member(&mut self, u: &mut UdpSock, id: IfaceId) {
        if u.members.iter().any(|(i, _)| *i == id) {
            return;
        }
        let Some(i) = self.iface_mut(id) else { return };
        let mut s = udp_socket(UDP_PACKETS, UDP_BYTES);
        let ep = if u.local.ip().is_unspecified() {
            IpListenEndpoint { addr: None, port: u.local.port() }
        } else {
            IpListenEndpoint { addr: Some(u.local.ip().into()), port: u.local.port() }
        };
        if s.bind(ep).is_ok() {
            u.members.push((id, i.sockets.add(s)));
        }
    }

    /// Sends a datagram. `Err(LimitReached)` means the send buffer is full
    /// (try again after a poll).
    pub fn udp_send(&mut self, id: SockId, dest: SocketAddr, data: &[u8]) -> Result<(), NetError> {
        if dest.port() == 0 || data.len() > vproto::net::MAX_DATAGRAM {
            return Err(NetError::InvalidArgument);
        }
        let via = self.route(dest.ip())?;
        let member = match self.socks.map.get(&id) {
            Some(Entry::Udp(u)) => u.members.iter().find(|(i, _)| *i == via).map(|(_, h)| *h),
            _ => return Err(NetError::Closed),
        };
        let handle = member.ok_or(NetError::NoRoute)?;
        let i = self.iface_mut(via).ok_or(NetError::NoRoute)?;
        let s = i.sockets.get_mut::<udp::Socket>(handle);
        if data.len() > s.payload_send_capacity() {
            return Err(NetError::MessageTooLarge);
        }
        match s.send_slice(data, IpEndpoint::from(dest)) {
            Ok(()) => Ok(()),
            Err(udp::SendError::Unaddressable) => Err(NetError::NoRoute),
            Err(udp::SendError::BufferFull) => {
                let queued = self.udp_send_queue(id);
                if let Some(Entry::Udp(u)) = self.socks.map.get_mut(&id) {
                    u.full_at = Some(queued);
                }
                Err(NetError::LimitReached)
            }
        }
    }

    /// Bytes waiting in a UDP socket's send buffers.
    /// Bytes of datagrams a UDP socket was given and has not sent yet.
    pub fn udp_send_queue(&self, id: SockId) -> usize {
        match self.socks.map.get(&id) {
            Some(Entry::Udp(u)) => u
                .members
                .iter()
                .filter_map(|&(iface, h)| Some(self.iface(iface)?.sockets.get::<udp::Socket>(h).send_queue()))
                .sum(),
            _ => 0,
        }
    }

    /// Offers the next received datagram to `take`, which returns whether
    /// it took it; a datagram not taken stays queued. `None` if none is
    /// waiting.
    pub fn udp_recv_with(&mut self, id: SockId, take: impl FnOnce(SocketAddr, &[u8]) -> bool) -> Option<bool> {
        let members = match self.socks.map.get(&id) {
            Some(Entry::Udp(u)) => &u.members,
            _ => return None,
        };
        let (iface, handle) = members.iter().copied().find(|&(iface, handle)| {
            self.iface(iface).is_some_and(|i| i.sockets.get::<udp::Socket>(handle).can_recv())
        })?;
        let i = self.iface_mut(iface)?;
        let s = i.sockets.get_mut::<udp::Socket>(handle);
        let taken = match s.peek() {
            Ok((data, meta)) => take(SocketAddr::from(meta.endpoint), data),
            Err(_) => return None,
        };
        if taken {
            let _ = s.recv();
            // What is left has been seen: a datagram that comes after it,
            // even in the same poll, makes the socket ready.
            let left: usize = self.udp_recv_queue(id);
            if let Some(Entry::Udp(u)) = self.socks.map.get_mut(&id) {
                u.last_rx = u.last_rx.min(left);
            }
        }
        Some(taken)
    }

    /// Bytes of the datagrams a UDP socket has received and not given out.
    fn udp_recv_queue(&self, id: SockId) -> usize {
        let Some(Entry::Udp(u)) = self.socks.map.get(&id) else { return 0 };
        u.members
            .iter()
            .filter_map(|&(iface, h)| Some(self.iface(iface)?.sockets.get::<udp::Socket>(h).recv_queue()))
            .sum()
    }

    // ---------------------------------------------------------------
    // Ping

    /// Opens a ping socket for process `owner`.
    pub fn ping_open(&mut self, owner: u64) -> Result<SockId, NetError> {
        self.check_limit()?;
        let ident = self.random_u32() as u16;
        Ok(self.socks.insert(Entry::Ping(PingSock {
            ident,
            members: Vec::new(),
            pending: BTreeMap::new(),
            events: VecDeque::new(),
            owner,
        })))
    }

    /// Sends an ICMP echo request with `size` payload bytes. Its outcome,
    /// a failure to send it included, comes as a [`PingEvent`].
    pub fn ping_send(&mut self, id: SockId, dest: IpAddr, seq: u16, ttl: u8, size: u16) {
        if let Err(error) = self.try_ping_send(id, dest, seq, ttl, size) {
            self.ping_event(id, PingEvent::Failed { seq, from: None, error });
        }
    }

    fn try_ping_send(&mut self, id: SockId, dest: IpAddr, seq: u16, ttl: u8, size: u16) -> Result<(), NetError> {
        if size as usize > 8192 {
            return Err(NetError::MessageTooLarge);
        }
        let via = self.route(dest)?;
        let now = self.now_us;
        let (ident, existing) = match self.socks.map.get(&id) {
            Some(Entry::Ping(p)) => (p.ident, p.members.iter().find(|(i, _)| *i == via).map(|(_, h)| *h)),
            _ => return Err(NetError::Closed),
        };
        let handle = match existing {
            Some(h) => h,
            None => {
                let i = self.iface_mut(via).ok_or(NetError::NoRoute)?;
                let mut s = icmp_socket();
                s.bind(icmp::Endpoint::Ident(ident)).map_err(|_| NetError::Internal)?;
                let h = i.sockets.add(s);
                if let Some(Entry::Ping(p)) = self.socks.map.get_mut(&id) {
                    p.members.push((via, h));
                }
                h
            }
        };
        let i = self.iface_mut(via).ok_or(NetError::NoRoute)?;
        let caps = ChecksumCapabilities::default();
        let src6 = match dest {
            IpAddr::V6(d) => Some(i.iface.get_source_address_ipv6(&d)),
            IpAddr::V4(_) => None,
        };
        let s = i.sockets.get_mut::<icmp::Socket>(handle);
        s.set_hop_limit(if ttl == 0 { None } else { Some(ttl) });
        let fill = |buf: &mut [u8]| {
            for (n, b) in buf.iter_mut().enumerate() {
                *b = (n as u8).wrapping_add(0x20);
            }
        };
        let mut payload = [0u8; 8192];
        let payload = &mut payload[..size as usize];
        fill(payload);
        let sent = match dest {
            IpAddr::V4(d) => {
                let repr = Icmpv4Repr::EchoRequest { ident, seq_no: seq, data: payload };
                s.send(repr.buffer_len(), IpAddress::Ipv4(d)).map(|buf| {
                    repr.emit(&mut Icmpv4Packet::new_unchecked(buf), &caps);
                })
            }
            IpAddr::V6(d) => {
                let repr = Icmpv6Repr::EchoRequest { ident, seq_no: seq, data: payload };
                let src = src6.unwrap_or(Ipv6Addr::UNSPECIFIED);
                s.send(repr.buffer_len(), IpAddress::Ipv6(d)).map(|buf| {
                    repr.emit(&src, &d, &mut Icmpv6Packet::new_unchecked(buf), &caps);
                })
            }
        };
        sent.map_err(|e| match e {
            icmp::SendError::BufferFull => NetError::LimitReached,
            icmp::SendError::Unaddressable => NetError::NoRoute,
        })?;
        if let Some(Entry::Ping(p)) = self.socks.map.get_mut(&id) {
            p.pending.insert(seq, (now, dest));
        }
        Ok(())
    }

    /// Queues a ping result for the client (results beyond
    /// `MAX_PING_EVENTS` that the client has not taken are dropped).
    fn ping_event(&mut self, id: SockId, ev: PingEvent) {
        if let Some(Entry::Ping(p)) = self.socks.map.get_mut(&id)
            && p.events.len() < MAX_PING_EVENTS
        {
            p.events.push_back(ev);
            self.socks.mark_ready(id);
        }
    }

    /// The oldest ping result the client has not taken.
    pub fn ping_next(&self, id: SockId) -> Option<PingEvent> {
        match self.socks.map.get(&id) {
            Some(Entry::Ping(p)) => p.events.front().copied(),
            _ => None,
        }
    }

    /// The client took the oldest ping result.
    pub fn ping_taken(&mut self, id: SockId) {
        if let Some(Entry::Ping(p)) = self.socks.map.get_mut(&id) {
            p.events.pop_front();
        }
    }

    // ---------------------------------------------------------------
    // Closing and housekeeping

    /// The client closed the socket. A TCP connection is shut down
    /// gracefully: the data the client sent is delivered, then a FIN; what
    /// it had not read is discarded. Everything else goes now.
    pub fn close(&mut self, id: SockId) {
        let now = self.now_us;
        let Some(entry) = self.socks.map.get_mut(&id) else { return };
        if let Entry::Tcp(t) = entry
            && t.phase != Phase::Closed
            && t.handle.is_some()
        {
            t.orphan_since = Some(now);
            t.closed_by_us = true;
            t.leftover.clear();
            self.with_tcp(id, |s| {
                // Nobody will read it: data arriving from now on resets the
                // connection (RFC 1122, 4.2.2.13).
                discard_received(s);
                s.close();
            });
            if let Some(Entry::Tcp(t)) = self.socks.map.get_mut(&id) {
                t.last_rx_queue = 0;
            }
            self.limit_orphans();
            return;
        }
        self.remove_socket(id);
    }

    /// Drops the oldest orphans beyond [`MAX_ORPHANS`].
    fn limit_orphans(&mut self) {
        let mut orphans: Vec<(u64, SockId)> = self
            .socks
            .map
            .iter()
            .filter_map(|(&id, e)| match e {
                Entry::Tcp(t) => t.orphan_since.map(|since| (since, id)),
                _ => None,
            })
            .collect();
        if orphans.len() <= MAX_ORPHANS {
            return;
        }
        orphans.sort_unstable();
        let excess = orphans.len() - MAX_ORPHANS;
        for &(_, id) in &orphans[..excess] {
            self.abort_orphan(id);
        }
    }

    /// Resets a connection nobody will use any more. It goes once smoltcp
    /// has sent the RST (the next housekeeping after a poll).
    fn abort_orphan(&mut self, id: SockId) {
        let now = self.now_us;
        if self.with_tcp(id, |s| s.abort()).is_none() {
            return self.remove_socket(id);
        }
        if let Some(Entry::Tcp(t)) = self.socks.map.get_mut(&id) {
            t.closed_by_us = true;
            t.aborted = true;
            t.orphan_since.get_or_insert(now);
        }
    }

    fn remove_socket(&mut self, id: SockId) {
        let Some(entry) = self.socks.map.remove(&id) else { return };
        self.socks.ready.remove(&id);
        match entry {
            Entry::Tcp(t) => {
                if let Some(h) = t.handle
                    && let Some(i) = self.iface_mut(t.iface)
                {
                    i.sockets.remove(h);
                }
                self.socks.release(Proto::Tcp, t.port);
            }
            Entry::Listener(l) => {
                for (iface, h) in l.members {
                    if let Some(i) = self.iface_mut(iface) {
                        i.sockets.remove(h);
                    }
                }
                // Connections nobody accepted are reset.
                for a in l.accepted {
                    self.abort_orphan(a);
                }
                self.socks.release(Proto::Tcp, l.local.port());
            }
            Entry::Udp(u) => {
                for (iface, h) in u.members {
                    if let Some(i) = self.iface_mut(iface) {
                        i.sockets.remove(h);
                    }
                }
                self.socks.release(Proto::Udp, u.local.port());
            }
            Entry::Ping(p) => {
                for (iface, h) in p.members {
                    if let Some(i) = self.iface_mut(iface) {
                        i.sockets.remove(h);
                    }
                }
            }
        }
    }

    /// Marks a TCP connection closed with an error and frees its smoltcp
    /// socket.
    fn fail_tcp(&mut self, id: SockId, error: NetError) {
        let (iface, handle, orphan) = match self.socks.map.get_mut(&id) {
            Some(Entry::Tcp(t)) => {
                t.phase = Phase::Closed;
                t.error.get_or_insert(error);
                (t.iface, t.handle.take(), t.orphan_since.is_some())
            }
            _ => return,
        };
        if let Some(h) = handle
            && let Some(i) = self.iface_mut(iface)
        {
            i.sockets.get_mut::<tcp::Socket>(h).abort();
            i.sockets.remove(h);
        }
        if orphan {
            self.remove_socket(id);
        } else {
            self.socks.mark_ready(id);
        }
    }

    /// Socket housekeeping after the interfaces ran.
    pub(crate) fn socks_maintain(&mut self) {
        let now = self.now_us;
        let ids: Vec<SockId> = self.socks.map.keys().copied().collect();
        for id in ids {
            match self.socks.map.get(&id) {
                Some(Entry::Tcp(_)) => self.maintain_tcp(id, now),
                Some(Entry::Listener(_)) => self.maintain_listener(id, now),
                Some(Entry::Udp(_)) => self.maintain_udp(id),
                Some(Entry::Ping(_)) => self.maintain_ping(id, now),
                None => {}
            }
        }
    }

    fn maintain_tcp(&mut self, id: SockId, now: u64) {
        let Some(snapshot) = self.with_tcp(id, |s| {
            (s.state(), s.local_endpoint(), s.remote_endpoint(), s.recv_queue(), s.send_queue(), s.has_timed_out())
        }) else {
            // No smoltcp socket any more: an orphan has nothing left.
            if matches!(self.socks.map.get(&id), Some(Entry::Tcp(t)) if t.orphan_since.is_some()) {
                self.remove_socket(id);
            }
            return;
        };
        let (state, local, remote, rxq, txq, timed_out) = snapshot;
        let Some(Entry::Tcp(t)) = self.socks.map.get_mut(&id) else { return };
        let mut ready = rxq > t.last_rx_queue || txq < t.last_tx_queue;
        if rxq != t.last_rx_queue || txq < t.last_tx_queue {
            t.progress_us = now;
        }
        t.last_rx_queue = rxq;
        t.last_tx_queue = txq;
        if let (Some(l), Some(r)) = (local, remote) {
            t.local = Some(l.into());
            t.remote = Some(r.into());
        }
        let established =
            !matches!(state, tcp::State::Closed | tcp::State::Listen | tcp::State::SynSent | tcp::State::SynReceived);
        if t.phase == Phase::Connecting && established {
            t.phase = Phase::Open;
            t.progress_us = now;
            ready = true;
        }
        // The peer's FIN arrived: these are the states that follow it.
        let fin =
            matches!(state, tcp::State::CloseWait | tcp::State::Closing | tcp::State::LastAck | tcp::State::TimeWait);
        if fin && !t.peer_fin {
            t.peer_fin = true;
            ready = true;
        }
        if let Some(since) = t.orphan_since {
            // Data after the client closed (nobody will read it), or no
            // progress for too long: give up on the connection.
            let stalled = now.saturating_sub(since.max(t.progress_us)) >= ORPHAN_TIMEOUT_US;
            match state {
                // Closed (a reset sent), or waiting out TIME-WAIT with
                // nothing left to deliver (the peer saw our ACK).
                tcp::State::Closed | tcp::State::TimeWait => self.remove_socket(id),
                _ if rxq > 0 || stalled => self.abort_orphan(id),
                _ => {}
            }
            return;
        }
        match state {
            // smoltcp forgets what was received when TIME-WAIT ends: keep
            // what the client has not read yet (nothing more can arrive).
            tcp::State::TimeWait if rxq > 0 => {
                let rest = self.with_tcp(id, take_received).unwrap_or_default();
                if let Some(Entry::Tcp(t)) = self.socks.map.get_mut(&id) {
                    t.leftover.extend(rest);
                    t.last_rx_queue = 0;
                }
            }
            tcp::State::Closed => {
                let error = match t.phase {
                    Phase::Connecting | Phase::Open if t.aborted => None,
                    Phase::Connecting => Some(if timed_out || now >= t.connect_deadline_us {
                        NetError::TimedOut
                    } else {
                        NetError::ConnectionRefused
                    }),
                    // The normal end: our FIN went, and the peer's came.
                    Phase::Open if t.closed_by_us && t.peer_fin => None,
                    // smoltcp gave up on the peer (something of ours went
                    // unacknowledged, or keep-alive probes unanswered, for
                    // TCP_USER_TIMEOUT), or the peer reset it, our half
                    // closed or not.
                    Phase::Open => Some(if timed_out { NetError::TimedOut } else { NetError::ConnectionReset }),
                    Phase::Closed => t.error,
                };
                match error {
                    Some(e) => self.fail_tcp(id, e),
                    None => {
                        // Closed normally: what the client has not read yet
                        // stays readable.
                        t.phase = Phase::Closed;
                        let (iface, handle) = (t.iface, t.handle.take());
                        if let Some(h) = handle
                            && let Some(i) = self.iface_mut(iface)
                        {
                            let rest = take_received(i.sockets.get_mut::<tcp::Socket>(h));
                            i.sockets.remove(h);
                            if let Some(Entry::Tcp(t)) = self.socks.map.get_mut(&id) {
                                t.leftover.extend(rest);
                            }
                        }
                        self.socks.mark_ready(id);
                    }
                }
                return;
            }
            tcp::State::SynSent if now >= t.connect_deadline_us => return self.fail_tcp(id, NetError::TimedOut),
            _ if t.idle_timeout_us > 0 && established && now.saturating_sub(t.progress_us) >= t.idle_timeout_us => {
                return self.fail_tcp(id, NetError::TimedOut);
            }
            _ => {}
        }
        if ready {
            self.socks.mark_ready(id);
        }
    }

    fn maintain_listener(&mut self, id: SockId, now: u64) {
        let Some(Entry::Listener(l)) = self.socks.map.get_mut(&id) else { return };
        let members = core::mem::take(&mut l.members);
        let owner = l.owner;
        let mut keep = Vec::new();
        let mut accepted = Vec::new();
        for (iface, h) in members {
            let Some(i) = self.iface_mut(iface) else { continue };
            let s = i.sockets.get::<tcp::Socket>(h);
            match s.state() {
                tcp::State::Listen | tcp::State::SynReceived => keep.push((iface, h)),
                tcp::State::Closed => {
                    i.sockets.remove(h);
                }
                _ => accepted.push((iface, h, s.local_endpoint(), s.remote_endpoint())),
            }
        }
        let mut new_ids = Vec::new();
        for (iface, h, local, remote) in accepted {
            // With the table full, the connection is refused: it stays only
            // until its RST is sent.
            let full = self.check_limit().is_err();
            let new = self.socks.insert(Entry::Tcp(TcpSock {
                iface,
                handle: Some(h),
                port: 0,
                phase: Phase::Open,
                connect_deadline_us: now,
                closed_by_us: false,
                aborted: false,
                orphan_since: None,
                progress_us: now,
                last_rx_queue: 0,
                last_tx_queue: 0,
                error: None,
                local: local.map(SocketAddr::from),
                remote: remote.map(SocketAddr::from),
                idle_timeout_us: 0,
                owner,
                peer_fin: false,
                leftover: VecDeque::new(),
            }));
            if full {
                self.abort_orphan(new);
            } else {
                new_ids.push(new);
            }
        }
        let mut l = match self.socks.map.remove(&id) {
            Some(Entry::Listener(l)) => l,
            _ => return,
        };
        l.members = keep;
        if !new_ids.is_empty() {
            l.accepted.extend(new_ids);
            self.socks.mark_ready(id);
        }
        // Replace the sockets that turned into connections, as far as the
        // backlog allows.
        for iface in ifaces_for(self, l.local.ip()) {
            self.add_listen_members(&mut l, iface);
        }
        self.socks.map.insert(id, Entry::Listener(l));
    }

    fn maintain_udp(&mut self, id: SockId) {
        let Some(Entry::Udp(u)) = self.socks.map.get(&id) else { return };
        let rx = self.udp_recv_queue(id);
        let tx = if u.full_at.is_some() { self.udp_send_queue(id) } else { 0 };
        let Some(Entry::Udp(u)) = self.socks.map.get_mut(&id) else { return };
        // Datagrams arrived, or room to send freed up after a datagram did
        // not fit.
        let mut ready = rx > u.last_rx;
        u.last_rx = rx;
        if u.full_at.is_some_and(|at| tx < at) {
            u.full_at = None;
            ready = true;
        }
        if ready {
            self.socks.mark_ready(id);
        }
    }

    fn maintain_ping(&mut self, id: SockId, now: u64) {
        let Some(Entry::Ping(p)) = self.socks.map.get(&id) else { return };
        let ident = p.ident;
        let members = p.members.clone();
        let mut answers = Vec::new();
        for (iface, h) in members {
            let Some(i) = self.iface_mut(iface) else { continue };
            let s = i.sockets.get_mut::<icmp::Socket>(h);
            while let Ok((packet, from)) = s.recv() {
                if let Some(answer) = ping_answer(packet, from, ident) {
                    answers.push((IpAddr::from(from), answer));
                }
            }
        }
        let Some(Entry::Ping(p)) = self.socks.map.get_mut(&id) else { return };
        let mut events = Vec::new();
        for (from, answer) in answers {
            let seq = match answer {
                PingAnswer::Reply { seq, .. } | PingAnswer::Error { seq, .. } => seq,
            };
            let Some((sent, _)) = p.pending.remove(&seq) else { continue };
            events.push(match answer {
                PingAnswer::Reply { size, .. } => {
                    let rtt = now.saturating_sub(sent).min(u32::MAX as u64) as u32;
                    PingEvent::Reply { from, seq, size: size as u16, rtt_us: rtt }
                }
                PingAnswer::Error { error, .. } => PingEvent::Failed { seq, from: Some(from), error },
            });
        }
        let expired: Vec<u16> =
            p.pending.iter().filter(|(_, (t, _))| now.saturating_sub(*t) >= PING_TIMEOUT_US).map(|(s, _)| *s).collect();
        for seq in expired {
            p.pending.remove(&seq);
            events.push(PingEvent::Failed { seq, from: None, error: NetError::TimedOut });
        }
        for ev in events {
            self.ping_event(id, ev);
        }
    }

    /// The earliest socket timer.
    pub(crate) fn socks_deadline(&self) -> Option<u64> {
        let mut best: Option<u64> = None;
        let mut take = |t: u64| best = Some(best.map_or(t, |b| b.min(t)));
        for e in self.socks.map.values() {
            match e {
                Entry::Tcp(TcpSock { orphan_since: Some(since), handle: Some(_), progress_us, .. }) => {
                    take((*since).max(*progress_us) + ORPHAN_TIMEOUT_US)
                }
                Entry::Tcp(t) if t.phase == Phase::Connecting => take(t.connect_deadline_us),
                Entry::Tcp(t) if t.phase == Phase::Open && t.idle_timeout_us > 0 => {
                    take(t.progress_us + t.idle_timeout_us)
                }
                Entry::Ping(p) => {
                    for (sent, _) in p.pending.values() {
                        take(sent + PING_TIMEOUT_US);
                    }
                }
                _ => {}
            }
        }
        best
    }

    /// A new interface: sockets bound to the unspecified address join it.
    pub(crate) fn socks_on_iface_added(&mut self, id: IfaceId) {
        let ids: Vec<SockId> = self.socks.map.keys().copied().collect();
        for sid in ids {
            match self.socks.map.remove(&sid) {
                Some(Entry::Udp(mut u)) => {
                    if u.local.ip().is_unspecified() {
                        self.add_udp_member(&mut u, id);
                    }
                    self.socks.map.insert(sid, Entry::Udp(u));
                }
                Some(Entry::Listener(mut l)) => {
                    if l.local.ip().is_unspecified() {
                        self.add_listen_members(&mut l, id);
                    }
                    self.socks.map.insert(sid, Entry::Listener(l));
                }
                Some(other) => {
                    self.socks.map.insert(sid, other);
                }
                None => {}
            }
        }
    }

    /// An interface is going away: its connections fail and other sockets
    /// leave it.
    pub(crate) fn socks_on_iface_removed(&mut self, id: IfaceId) {
        let ids: Vec<SockId> = self.socks.map.keys().copied().collect();
        for sid in ids {
            let on_iface = matches!(self.socks.map.get(&sid), Some(Entry::Tcp(t)) if t.iface == id);
            if on_iface {
                self.fail_tcp(sid, NetError::Aborted);
                continue;
            }
            match self.socks.map.get_mut(&sid) {
                Some(Entry::Udp(u)) => u.members.retain(|(i, _)| *i != id),
                Some(Entry::Listener(l)) => l.members.retain(|(i, _)| *i != id),
                Some(Entry::Ping(p)) => p.members.retain(|(i, _)| *i != id),
                _ => {}
            }
        }
    }

    /// A local address disappeared: connections using it fail.
    pub(crate) fn socks_address_lost(&mut self, addr: IpAddr) {
        let ids: Vec<SockId> = self
            .socks
            .map
            .iter()
            .filter(|(_, e)| matches!(e, Entry::Tcp(t) if t.local.is_some_and(|l| l.ip() == addr) && t.phase != Phase::Closed))
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.fail_tcp(id, NetError::NetworkDown);
        }
    }

    /// The sockets of process `owner` (or every socket, with `None`), for
    /// `netstat`. Connections whose client closed them belong to nobody.
    pub fn socket_infos(&self, owner: Option<u64>) -> Vec<SocketInfo> {
        let mut out = Vec::new();
        let mine = |o: u64| owner.is_none_or(|w| w == o);
        for e in self.socks.map.values() {
            match e {
                Entry::Tcp(t) if t.orphan_since.is_none() || owner.is_none() => {
                    if !mine(t.owner) {
                        continue;
                    }
                    let (state, rxq, txq) =
                        match t.handle.and_then(|h| self.iface(t.iface).map(|i| i.sockets.get::<tcp::Socket>(h))) {
                            Some(s) => (state_name(s.state()), s.recv_queue(), s.send_queue()),
                            None => ("CLOSED", t.leftover.len(), 0),
                        };
                    out.push(SocketInfo {
                        kind: SocketKind::Tcp,
                        local: t.local,
                        remote: t.remote,
                        state: state.into(),
                        owner: if t.orphan_since.is_some() { 0 } else { t.owner },
                        interface: self.iface(t.iface).map(|i| i.name.clone()).unwrap_or_default(),
                        rx_queued: rxq as u32,
                        tx_queued: txq as u32,
                    });
                }
                Entry::Tcp(_) => {}
                Entry::Listener(l) if mine(l.owner) => out.push(SocketInfo {
                    kind: SocketKind::TcpListener,
                    local: Some(l.local),
                    remote: None,
                    state: "LISTEN".into(),
                    owner: l.owner,
                    interface: String::new(),
                    rx_queued: l.accepted.len() as u32,
                    tx_queued: 0,
                }),
                Entry::Udp(u) if mine(u.owner) => out.push(SocketInfo {
                    kind: SocketKind::Udp,
                    local: Some(u.local),
                    remote: None,
                    state: String::new(),
                    owner: u.owner,
                    interface: String::new(),
                    rx_queued: u.last_rx as u32,
                    tx_queued: 0,
                }),
                Entry::Ping(p) if mine(p.owner) => out.push(SocketInfo {
                    kind: SocketKind::Ping,
                    local: None,
                    remote: None,
                    state: String::new(),
                    owner: p.owner,
                    interface: String::new(),
                    rx_queued: p.events.len() as u32,
                    tx_queued: p.pending.len() as u32,
                }),
                _ => {}
            }
        }
        out
    }
}
