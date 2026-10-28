// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Internet sockets: TCP and UDP over IPv4 and IPv6. Each is a socket of
//! the network service (`network`), with a channel of its own, opened in
//! the ways of `proto::net` that processes can share (see "Sockets that
//! processes share" there), so that it can travel to the processes a
//! program starts and over Unix domain sockets ([`Inet::travelling`]):
//!
//! * a TCP connection's bytes go on a kernel socket between the program
//!   and the service, read and written as a pipe's ([`crate::stream`]): its
//!   buffers are the flow control, and its end, the stream's. The channel
//!   brings the connection (`Connected`) and its failure (`Error`, after
//!   which the service closes its end of the kernel socket), and takes
//!   `Abort` and `SetNoDelay`;
//! * a stream socket bound before it listens or connects is the service's
//!   bound socket, which holds its address (its port is the socket's) until
//!   `listen` makes it a listener or `connect` a connection from it, on
//!   the same channel;
//! * a listening socket's connections wait on its channel, one message
//!   each, until an `accept` takes one;
//! * a UDP socket's datagrams wait on its channel, one message each, until
//!   a receive takes one, and those it sends go on the channel as far as it
//!   has room.
//!
//! Its channel is shared (`runtime::object::Channel::create_shared`), as a
//! connection's kernel socket is, so whoever holds them reads and writes:
//! as on Linux, processes that share a socket share its connection's bytes,
//! a listener's connections and a datagram socket's datagrams, which go to
//! whichever takes them first.
//!
//! What else a socket holds — its state and options, a pending error, a
//! datagram looked at (`MSG_PEEK`) — is kept here, behind a lock. A thread
//! that must wait waits on the kernel socket or the channel, never under
//! the lock. While a connection is being made, which another thread may
//! see come, its readiness is also kept in the channel's user signals:
//! [`READY_IN`] while a read would not wait, [`READY_OUT`] while a write
//! would not.
//!
//! Compared with Linux:
//!
//! * sockets share a port when no address is both's, as on Linux without
//!   `SO_REUSEADDR` (an IPv4 socket on `0.0.0.0` and an IPv6-only one on
//!   `::`, sockets on different addresses), whatever `SO_REUSEADDR` and
//!   `SO_REUSEPORT` say; but a TCP port is free while connections use it,
//!   as with `SO_REUSEADDR` (each connection is told apart by its peer);
//! * datagram sockets bind (to any address, a free port) when they first
//!   send or connect, as on Linux, and when they first receive; a datagram
//!   socket's sends wait for room in its channel, not in a send buffer;
//! * the options programs set are kept, and those the service has take
//!   effect (`TCP_NODELAY`, `SO_KEEPALIVE` with `TCP_KEEPIDLE` as the
//!   interval, `SO_LINGER` with no time: a reset at close, unless the
//!   socket went to another process or came from one); there is no
//!   multicast, no out-of-band data and no ancillary data;
//! * a socket going to another process takes its options as they are
//!   (changed afterwards, they change in one process only); a connection's
//!   failure is reported to the process that hears of it first (the
//!   others see its stream end); a socket that is still connecting stays
//!   with the program;
//! * an edge-triggered `epoll` reports a listening or datagram socket when
//!   a connection or a datagram comes to wait where none did, not each one
//!   that comes while others wait (a program that reads until `EAGAIN`, as
//!   `epoll` asks of it, misses none).

use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::net::SocketAddr;

use abi::ObjectType;
use abi::signals::{PEER_CLOSED, PEER_WRITE_DISABLED, READABLE, USER_0, USER_1, WRITABLE};
use ipc::{Encode, Encoder, IpcError};
use proto::net::{MAX_DATAGRAM, NetError, SocketEvent, SocketRequest, TcpOptions};
use runtime::object::{Channel, Handle, Socket};
use runtime::sync::{Mutex, MutexGuard};

use crate::error::{self, SysResult};
use crate::io::Waitable;
use crate::linux::errno::*;
use crate::linux::sock::{DGRAM, IPPROTO_TCP, IPPROTO_UDP, STREAM};
use crate::network;
use crate::sockaddr::Family;
use crate::stream::Stream as Bytes;
use crate::{signal, time};

/// A read would not wait: the connection being made failed (or came).
pub const READY_IN: u32 = USER_0;
/// A write would not wait: the connection came, or failed.
pub const READY_OUT: u32 = USER_1;

/// What kind of socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// TCP (`SOCK_STREAM`).
    Stream,
    /// UDP (`SOCK_DGRAM`).
    Datagram,
}

impl Kind {
    /// The `socket` type.
    pub fn sock_type(self) -> u32 {
        match self {
            Kind::Stream => STREAM,
            Kind::Datagram => DGRAM,
        }
    }

    /// The protocol (`IPPROTO_TCP`, `IPPROTO_UDP`).
    pub fn protocol(self) -> u32 {
        match self {
            Kind::Stream => IPPROTO_TCP,
            Kind::Datagram => IPPROTO_UDP,
        }
    }
}

ipc::message! {
    /// The options a program set (`setsockopt`); what they are for is in
    /// [`crate::socket::setsockopt`].
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Options {
        pub reuse_addr: bool,
        pub reuse_port: bool,
        pub keepalive: bool,
        pub broadcast: bool,
        pub oob_inline: bool,
        pub nodelay: bool,
        /// Seconds idle before keep-alive probes, between them, and how
        /// many.
        pub keep_idle: u32,
        pub keep_interval: u32,
        pub keep_count: u32,
        /// `SO_LINGER`'s time, when it is on.
        pub linger: Option<u32>,
        /// How long reads and writes wait (ns; `None`: for ever).
        pub recv_timeout: Option<u64>,
        pub send_timeout: Option<u64>,
        pub ttl: u32,
        pub tos: u32,
        pub hops: i32,
        /// An IPv6 socket that does not reach IPv4.
        pub v6only: bool,
        /// TCP's hints, kept as given (the service has its own ways):
        /// acknowledging at once, holding back small segments, waking a
        /// listener only for data, and giving up on a peer that does not
        /// answer (ms).
        pub quickack: bool,
        pub cork: bool,
        pub defer_accept: u32,
        pub user_timeout: u32,
    }
}

impl Default for Options {
    fn default() -> Options {
        Options {
            reuse_addr: false,
            reuse_port: false,
            keepalive: false,
            broadcast: false,
            oob_inline: false,
            nodelay: false,
            // Linux's.
            keep_idle: 7200,
            keep_interval: 75,
            keep_count: 9,
            linger: None,
            recv_timeout: None,
            send_timeout: None,
            ttl: 64,
            tos: 0,
            hops: -1,
            v6only: false,
            // Linux's: a connection starts acknowledging at once.
            quickack: true,
            cork: false,
            defer_accept: 0,
            user_timeout: 0,
        }
    }
}

impl Options {
    /// The connection options the service takes.
    fn tcp(&self) -> TcpOptions {
        let keepalive_ms = if self.keepalive { self.keep_idle.saturating_mul(1000).max(1000) } else { 0 };
        TcpOptions { nodelay: self.nodelay, keepalive_ms, connect_timeout_ms: 0, idle_timeout_ms: 0 }
    }
}

/// What a socket is, as it travels.
mod shape {
    /// Neither bound, connected nor listening; a datagram socket not bound.
    pub const IDLE: u32 = 0;
    /// Listening (its channel goes).
    pub const LISTENING: u32 = 1;
    /// A connection (its kernel socket goes, and its channel).
    pub const STREAM: u32 = 2;
    /// A datagram socket bound (its channel goes).
    pub const DATAGRAMS: u32 = 3;
    /// A stream socket bound, neither connected nor listening (its channel
    /// goes).
    pub const BOUND: u32 = 4;
}

ipc::message! {
    /// A socket on its way to another process, beside the handles that carry
    /// it (see [`Inet::travelling`]).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Travelling {
        /// Its family's `socket` domain.
        domain: u16,
        /// Its `socket` type.
        kind: u32,
        shape: u32,
        /// Where it is: bound, listening, or connected from.
        local: Option<SocketAddr>,
        /// A connection's peer, or a datagram socket's.
        remote: Option<SocketAddr>,
        options: Options,
        /// It reads no more, writes no more (`shutdown`).
        shut: (bool, bool),
        /// A datagram socket the service closed.
        closed: bool,
    }
}

/// A connection of a stream socket.
struct Stream {
    local: SocketAddr,
    remote: SocketAddr,
    /// Its bytes: this end of the kernel socket between the program and the
    /// service, which other processes may hold too.
    bytes: Arc<Bytes>,
}

/// A connection a listener accepted.
struct Accepted {
    channel: Channel,
    bytes: Socket,
    local: SocketAddr,
    remote: SocketAddr,
}

/// A bound datagram socket.
struct Datagrams {
    local: SocketAddr,
    /// The peer it is connected to.
    peer: Option<SocketAddr>,
    /// A datagram taken off the channel and not read yet (looked at with
    /// `MSG_PEEK`, or measured), in front of what the channel has.
    held: Option<(SocketAddr, Vec<u8>)>,
    /// The service closed the socket.
    closed: bool,
}

/// Where a socket is.
enum Conn {
    /// A stream socket neither bound, connected nor listening (not yet, or
    /// no longer); a datagram socket not bound yet.
    Idle,
    /// A stream socket bound: the service holds its address until it
    /// listens or connects.
    Bound {
        local: SocketAddr,
    },
    /// A connection being made, whose bytes are to go on `bytes`, from
    /// `from` if the socket was bound.
    Connecting {
        bytes: Socket,
        from: Option<SocketAddr>,
    },
    Stream(Stream),
    /// Its connections wait on the channel.
    Listening {
        local: SocketAddr,
    },
    Datagrams(Datagrams),
}

struct State {
    /// The channel to the network service, once the socket has one. Waits
    /// hold it too, so a thread that waits keeps it open.
    channel: Option<Arc<Channel>>,
    conn: Conn,
    options: Options,
    /// An error not reported yet (`SO_ERROR`).
    error: Option<isize>,
    /// The program stopped reading or writing (`shutdown`).
    shut_read: bool,
    shut_write: bool,
    /// The user signals last set on the channel.
    signalled: u32,
    /// How many events the channel brought (what edge-triggered `epoll`
    /// compares).
    events: u64,
    /// The service closed the channel: nothing more comes on it.
    lost: bool,
    /// The socket went to another process, or came from one: closing it
    /// here is no reset (`SO_LINGER`), which the process closing it last
    /// would make on Linux.
    shared: bool,
}

/// An Internet socket (`Object::Inet`).
pub struct Inet {
    pub kind: Kind,
    pub family: Family,
    state: Mutex<State>,
}

/// What a wait is for, beside the channel's messages.
#[derive(Clone, Copy)]
enum Want {
    In,
    Out,
}

/// What `poll` makes of a socket.
pub struct Readiness {
    pub input: bool,
    pub output: bool,
    pub hangup: bool,
    /// The peer finished sending, or the program stopped reading.
    pub read_hangup: bool,
    pub error: bool,
    /// What to wait on, and for which of its signals; none once nothing
    /// more can come.
    pub wait: Option<(Waitable, u32)>,
    /// How many events the channel brought.
    pub seq: u64,
}

/// Writes a request to the socket's channel without waiting:
/// `ShouldWait` while the channel is full.
fn request(channel: &Channel, fill: impl FnOnce(&mut Encoder)) -> Result<(), abi::Error> {
    let mut e = Encoder::with_header(0, 0, ipc::FLAG_EVENT);
    fill(&mut e);
    channel.write(&e.bytes, Vec::new())
}

/// Writes a request that carries handles to a socket's channel, waiting for
/// room in it: `fill` encodes it afresh for each try, with the handles it
/// makes (a write takes them, even one that fails), and returns what it
/// keeps of them.
fn send_with<T>(channel: &Channel, mut fill: impl FnMut(&mut Encoder) -> Result<T, isize>) -> Result<T, isize> {
    loop {
        let mut e = Encoder::with_header(0, 0, ipc::FLAG_EVENT);
        let kept = fill(&mut e)?;
        match channel.write(&e.bytes, core::mem::take(&mut e.handles)) {
            Ok(()) => return Ok(kept),
            // Full: the processes that share the socket wrote first.
            Err(abi::Error::ShouldWait) => {}
            Err(_) => return Err(network::errno(NetError::Unavailable)),
        }
        channel.wait(WRITABLE | PEER_CLOSED, abi::DEADLINE_INFINITE).map_err(error::kernel)?;
    }
}

/// The service's answer to `Listen` on `reply`: where the socket listens.
fn listened(reply: &Channel) -> Result<SocketAddr, isize> {
    // It answers at once, unless it is gone.
    reply.wait(READABLE | PEER_CLOSED, abi::DEADLINE_INFINITE).map_err(error::kernel)?;
    let answer = reply.read().map_err(|_| network::errno(NetError::Unavailable))?;
    match ipc::decode_event::<SocketEvent>(answer) {
        Ok((_, SocketEvent::Listening { local })) => Ok(local),
        Ok((_, SocketEvent::Error { error })) => Err(network::errno(error)),
        _ => Err(network::errno(NetError::ProtocolViolation)),
    }
}

/// A deadline `timeout` (ns) from now; `None` for none.
fn deadline(timeout: Option<u64>) -> u64 {
    timeout.map_or(abi::DEADLINE_INFINITE, |t| time::monotonic_ns().saturating_add(t))
}

/// Waits on `channel` for a message, the end of the service or `signals`
/// until `deadline`; `EAGAIN` when it passes (what a socket's time-outs
/// make a call fail with, as on Linux), `ERESTARTSYS` (`EINTR`, with a
/// time-out) when a signal is to be acted on.
fn wait(channel: &Channel, signals: u32, deadline: u64) -> Result<(), isize> {
    match crate::interrupt::wait_one(&channel.0, READABLE | PEER_CLOSED | signals, deadline) {
        Ok(_) => Ok(()),
        Err(abi::Error::TimedOut) => Err(EAGAIN),
        Err(abi::Error::Interrupted) => Err(crate::stream::interrupted(deadline)),
        Err(e) => Err(error::kernel(e)),
    }
}

/// Reads into `buf` what a connection's kernel socket has (waiting for
/// something until `deadline` unless `nonblock`; for all of it with
/// `waitall`); with `peek`, what it returns stays to be read again. Returns
/// the count and whether the stream ended (0 then: the end).
fn read_bytes(
    bytes: &Bytes,
    buf: &mut [u8],
    how: Recv,
    deadline: u64,
    shut_read: bool,
) -> Result<(usize, bool), isize> {
    if buf.is_empty() {
        return Ok((0, false));
    }
    let socket = &bytes.socket;
    let mut got = 0;
    loop {
        let mut ended = false;
        let read = if how.peek { socket.peek(&mut buf[got..]) } else { socket.read(&mut buf[got..]) };
        match read {
            Ok(n) => got += n,
            Err(abi::Error::PeerClosed) => ended = true,
            Err(abi::Error::ShouldWait) => {}
            Err(_) if got > 0 => return Ok((got, false)),
            Err(e) => return Err(error::kernel(e)),
        }
        if got == buf.len() || (got > 0 && (!how.waitall || how.peek)) || ended || shut_read {
            return Ok((got, ended && got == 0));
        }
        if how.nonblock {
            return if got > 0 { Ok((got, false)) } else { Err(EAGAIN) };
        }
        match crate::interrupt::wait_one(&socket.0, READABLE | PEER_CLOSED | PEER_WRITE_DISABLED, deadline) {
            Ok(_) => {}
            Err(_) if got > 0 => return Ok((got, false)),
            Err(abi::Error::TimedOut) => return Err(EAGAIN),
            Err(abi::Error::Interrupted) => return Err(crate::stream::interrupted(deadline)),
            Err(e) => return Err(error::kernel(e)),
        }
    }
}

impl State {
    fn new() -> State {
        State {
            channel: None,
            conn: Conn::Idle,
            options: Options::default(),
            error: None,
            shut_read: false,
            shut_write: false,
            signalled: 0,
            events: 0,
            lost: false,
            shared: false,
        }
    }

    /// Takes the events of a connection being made, or made, off the
    /// channel (a listener's and a datagram socket's stay there until
    /// taken; the service's going is seen at once).
    fn drain(&mut self) {
        let Some(channel) = self.channel.clone() else { return };
        match self.conn {
            Conn::Connecting { .. } | Conn::Stream(_) => {}
            Conn::Listening { .. } => {
                if !self.lost && channel.wait(PEER_CLOSED, 0).is_ok() {
                    self.lose();
                }
                return;
            }
            Conn::Idle | Conn::Bound { .. } | Conn::Datagrams(_) => return,
        }
        loop {
            match channel.read() {
                Ok(message) => {
                    // A message that is not an event is the service's
                    // mistake: passed over.
                    if let Ok((_, event)) = ipc::decode_event::<SocketEvent>(message) {
                        self.absorb(event);
                    }
                }
                Err(abi::Error::ShouldWait) => break,
                Err(_) => {
                    self.lose();
                    break;
                }
            }
        }
        self.signal();
    }

    /// The service is gone, and the socket with it.
    fn lose(&mut self) {
        if !self.lost {
            self.lost = true;
            self.events = self.events.wrapping_add(1);
            self.fail(NetError::Closed);
        }
    }

    fn absorb(&mut self, event: SocketEvent) {
        self.events = self.events.wrapping_add(1);
        match event {
            SocketEvent::Connected { local, remote, .. } => {
                if let Conn::Connecting { .. } = self.conn {
                    let Conn::Connecting { bytes, .. } = core::mem::replace(&mut self.conn, Conn::Idle) else {
                        return;
                    };
                    self.conn = Conn::Stream(Stream { local, remote, bytes: Arc::new(Bytes::new(bytes)) });
                }
            }
            SocketEvent::Error { error } => self.fail(error),
            _ => {}
        }
    }

    /// The socket failed: its error is pending, and a connection being made
    /// is no more (the socket may connect again).
    fn fail(&mut self, e: NetError) {
        match &mut self.conn {
            Conn::Connecting { .. } => {
                self.conn = Conn::Idle;
                self.error.get_or_insert(match e {
                    // The service went away while connecting.
                    NetError::Closed => ECONNREFUSED,
                    other => network::errno(other),
                });
                // Waiters keep the channel while they wait; the service
                // drops the connection once this end is closed.
                self.signal_with(READY_IN | READY_OUT);
                self.channel = None;
            }
            // A connection the service closed ends as its stream does: the
            // kernel socket says so.
            Conn::Stream(_) if e == NetError::Closed => {}
            Conn::Stream(_) | Conn::Listening { .. } => {
                self.error.get_or_insert(network::errno(e));
            }
            Conn::Datagrams(d) if !d.closed => {
                d.closed = true;
                self.error.get_or_insert(network::errno(e));
            }
            _ => {}
        }
    }

    /// Sets the channel's user signals once a connection being made is
    /// made, so that every thread waiting for it wakes (its failure sets
    /// them too: see `fail`).
    fn signal(&mut self) {
        if matches!(self.conn, Conn::Stream(_)) {
            self.signal_with(READY_IN | READY_OUT);
        }
    }

    fn signal_with(&mut self, signals: u32) {
        if signals == self.signalled {
            return;
        }
        if let Some(channel) = &self.channel
            && channel.0.signal(READY_IN | READY_OUT, signals).is_ok()
        {
            self.signalled = signals;
        }
    }

    /// The pending error, now reported.
    fn take_error(&mut self) -> Option<isize> {
        let e = self.error.take();
        if e.is_some() {
            self.signal();
        }
        e
    }

    /// The next connection on a listener's channel, without waiting (an
    /// error the service reports there instead is pending afterwards).
    fn next_connection(&mut self) -> Option<Accepted> {
        let channel = self.channel.clone()?;
        loop {
            match channel.read() {
                Ok(message) => match ipc::decode_event::<SocketEvent>(message) {
                    Ok((_, SocketEvent::AcceptedBytes { socket, bytes, local, remote })) => {
                        return Some(Accepted { channel: socket, bytes, local, remote });
                    }
                    Ok((_, SocketEvent::Error { error })) => {
                        self.fail(error);
                        return None;
                    }
                    // Not what this listener is sent: passed over.
                    _ => {}
                },
                Err(abi::Error::ShouldWait) => return None,
                Err(_) => {
                    self.lose();
                    return None;
                }
            }
        }
    }

    /// The next datagram for a datagram socket, without waiting: the one
    /// held, or the next on the channel it hears (a connected socket hears
    /// its peer alone). With `keep`, it stays to be read again.
    fn next_datagram(&mut self, keep: bool) -> Option<(SocketAddr, Vec<u8>)> {
        let channel = self.channel.clone()?;
        let Conn::Datagrams(d) = &mut self.conn else { return None };
        let next = match d.held.take() {
            Some(held) => Some(held),
            None => loop {
                match channel.read() {
                    Ok(message) => match ipc::decode_event::<SocketEvent>(message) {
                        Ok((_, SocketEvent::Datagram { from, data })) => {
                            if d.peer.is_none_or(|p| p.ip() == from.ip() && p.port() == from.port()) {
                                break Some((from, data.0));
                            }
                        }
                        Ok((_, SocketEvent::Error { error })) => {
                            self.fail(error);
                            return None;
                        }
                        _ => {}
                    },
                    Err(abi::Error::ShouldWait) => break None,
                    Err(_) => {
                        self.lose();
                        return None;
                    }
                }
            },
        };
        if keep && let (Some(datagram), Conn::Datagrams(d)) = (&next, &mut self.conn) {
            d.held = Some(datagram.clone());
        }
        next
    }
}

impl Inet {
    pub fn new(kind: Kind, family: Family) -> Inet {
        Inet { kind, family, state: Mutex::new(State::new()) }
    }

    /// A connection a listener accepted.
    fn accepted(listener: &Inet, a: Accepted, options: Options) -> Inet {
        let mut state = State::new();
        state.options = options;
        state.channel = Some(Arc::new(a.channel));
        state.conn = Conn::Stream(Stream { local: a.local, remote: a.remote, bytes: Arc::new(Bytes::new(a.bytes)) });
        Inet { kind: Kind::Stream, family: listener.family, state: Mutex::new(state) }
    }

    /// The state, with the events that came taken in.
    fn state(&self) -> MutexGuard<'_, State> {
        let mut st = self.state.lock();
        st.drain();
        st
    }

    /// The channel to wait on for a connection being made, and what of it,
    /// for `want`.
    fn waiter(st: &State, want: Want) -> Option<(Arc<Channel>, u32)> {
        let signals = match want {
            Want::In => READY_IN,
            Want::Out => READY_OUT,
        };
        st.channel.clone().map(|c| (c, signals))
    }

    pub fn options(&self) -> Options {
        self.state.lock().options
    }

    /// `IPV6_V6ONLY`: before the socket has an address alone (`EINVAL`
    /// after, as on Linux), which the service binds by it.
    pub fn set_v6only(&self, on: bool) -> SysResult {
        let mut st = self.state();
        if !matches!(st.conn, Conn::Idle) {
            return Err(EINVAL);
        }
        st.options.v6only = on;
        Ok(0)
    }

    /// Changes the options; a connection takes `TCP_NODELAY` at once.
    pub fn set_options(&self, f: impl FnOnce(&mut Options)) -> SysResult {
        let mut st = self.state();
        let before = st.options.nodelay;
        f(&mut st.options);
        let nodelay = st.options.nodelay;
        if nodelay != before
            && let (Conn::Stream(_), Some(channel)) = (&st.conn, &st.channel)
        {
            // A full channel: the setting goes with the next one.
            let _ = request(channel, |e| SocketRequest::SetNoDelay { nodelay }.encode(e));
        }
        Ok(0)
    }

    /// `SO_ERROR`: the pending error, now reported (0: none).
    pub fn take_error(&self) -> isize {
        self.state().take_error().unwrap_or(0)
    }

    /// Whether it listens (`SO_ACCEPTCONN`).
    pub fn listening(&self) -> bool {
        matches!(self.state.lock().conn, Conn::Listening { .. })
    }

    /// `getsockname`: where it is bound (the unspecified address, port 0,
    /// before it is).
    pub fn local(&self) -> SocketAddr {
        let st = self.state();
        match &st.conn {
            Conn::Stream(s) => s.local,
            Conn::Listening { local } => *local,
            Conn::Datagrams(d) => d.local,
            Conn::Bound { local } => *local,
            Conn::Connecting { from, .. } => from.unwrap_or(self.family.any()),
            Conn::Idle => self.family.any(),
        }
    }

    /// `getpeername`.
    pub fn peer(&self) -> Result<SocketAddr, isize> {
        match &self.state().conn {
            Conn::Stream(s) => Ok(s.remote),
            Conn::Datagrams(Datagrams { peer: Some(p), .. }) => Ok(*p),
            _ => Err(ENOTCONN),
        }
    }

    /// `bind`.
    pub fn bind(&self, addr: SocketAddr) -> SysResult {
        let mut st = self.state();
        if !matches!(st.conn, Conn::Idle) {
            return Err(EINVAL);
        }
        match self.kind {
            Kind::Stream => self.bind_stream(&mut st, addr),
            Kind::Datagram => self.bind_datagrams(&mut st, addr),
        }
        .map(|()| 0)
    }

    /// Binds a stream socket to `addr`: the service holds it for the socket
    /// until it listens or connects.
    fn bind_stream(&self, st: &mut State, addr: SocketAddr) -> Result<(), isize> {
        let v6only = st.options.v6only;
        let (channel, local) = network::with(|c| {
            // Shared, so that the socket can travel to other processes.
            let (ours, theirs) = Channel::create_shared().map_err(IpcError::Kernel)?;
            Ok(c.tcp_bind(addr, v6only, theirs)?.map(|local| (ours, local)))
        })
        .map_err(network::errno)?
        .map_err(network::errno)?;
        st.channel = Some(Arc::new(channel));
        st.conn = Conn::Bound { local };
        Ok(())
    }

    /// Binds a datagram socket to `addr`.
    fn bind_datagrams(&self, st: &mut State, addr: SocketAddr) -> Result<(), isize> {
        let v6only = st.options.v6only;
        let (channel, local) = network::with(|c| {
            // Shared, so that the socket can travel to other processes.
            let (ours, theirs) = Channel::create_shared().map_err(IpcError::Kernel)?;
            Ok(c.udp_bind_unmetered(addr, v6only, theirs)?.map(|local| (ours, local)))
        })
        .map_err(network::errno)?
        .map_err(network::errno)?;
        st.channel = Some(Arc::new(channel));
        st.conn = Conn::Datagrams(Datagrams { local, peer: None, held: None, closed: false });
        Ok(())
    }

    /// Binds a datagram socket that is not bound yet, to any address and a
    /// free port.
    fn autobind(&self, st: &mut State) -> Result<(), isize> {
        if matches!(st.conn, Conn::Idle) {
            self.bind_datagrams(st, self.family.any())?;
        }
        Ok(())
    }

    /// `listen`: a socket not bound listens on any address, on a free
    /// port.
    pub fn listen(&self, backlog: i32) -> SysResult {
        if self.kind != Kind::Stream {
            return Err(EOPNOTSUPP);
        }
        let mut st = self.state();
        let backlog = backlog.clamp(1, 4096) as u32;
        let (options, v6only) = (st.options.tcp(), st.options.v6only);
        let local = match st.conn {
            Conn::Listening { .. } => return Ok(0),
            Conn::Idle => {
                let any = self.family.any();
                let (channel, local) = network::with(|c| {
                    // Shared, so that the socket can travel to other
                    // processes.
                    let (ours, theirs) = Channel::create_shared().map_err(IpcError::Kernel)?;
                    Ok(c.tcp_listen_bytes(any, v6only, backlog, options, theirs)?.map(|local| (ours, local)))
                })
                .map_err(network::errno)?
                .map_err(network::errno)?;
                st.channel = Some(Arc::new(channel));
                local
            }
            // A socket that cannot listen stays bound.
            Conn::Bound { .. } => {
                let channel = st.channel.clone().ok_or(EINVAL)?;
                let reply = send_with(&channel, |e| {
                    let (reply, theirs) = Channel::create().map_err(error::kernel)?;
                    SocketRequest::Listen { backlog, options, reply: theirs }.encode(e);
                    Ok(reply)
                })?;
                listened(&reply)?
            }
            _ => return Err(EINVAL),
        };
        st.conn = Conn::Listening { local };
        Ok(0)
    }

    /// `accept`: the next connection, waiting for one unless `nonblock`.
    pub fn accept(&self, nonblock: bool) -> Result<Inet, isize> {
        let end = deadline(self.state.lock().options.recv_timeout);
        loop {
            let channel = {
                let mut st = self.state();
                if !matches!(st.conn, Conn::Listening { .. }) {
                    return Err(EINVAL);
                }
                if let Some(a) = st.next_connection() {
                    let options = st.options;
                    return Ok(Inet::accepted(self, a, options));
                }
                if let Some(e) = st.take_error() {
                    return Err(e);
                }
                if nonblock {
                    return Err(EAGAIN);
                }
                st.channel.clone().ok_or(EINVAL)?
            };
            // Another process may take what comes first: then wait again.
            wait(&channel, 0, end)?;
        }
    }

    /// The socket as it travels to another process (sent over a Unix domain
    /// socket, or given to a process this one starts), and copies of the
    /// handles that carry it: a connection's kernel socket and channel, a
    /// listener's or a datagram socket's channel, none for an idle socket.
    /// `EOPNOTSUPP` for a connection being made, which stays with the
    /// program.
    pub fn travelling(&self) -> Result<(Travelling, Vec<Handle>), isize> {
        let mut st = self.state();
        let duplicate = |c: &Channel| c.0.duplicate(None).map_err(error::kernel);
        let channel = st.channel.clone();
        let (shape, local, remote, handles) = match (&st.conn, &channel) {
            (Conn::Idle, _) => (shape::IDLE, None, None, Vec::new()),
            (Conn::Bound { local }, Some(c)) => (shape::BOUND, Some(*local), None, vec![duplicate(c)?]),
            (Conn::Listening { local }, Some(c)) => (shape::LISTENING, Some(*local), None, vec![duplicate(c)?]),
            (Conn::Stream(s), Some(c)) => {
                let bytes = s.bytes.socket.0.duplicate(None).map_err(error::kernel)?;
                (shape::STREAM, Some(s.local), Some(s.remote), vec![bytes, duplicate(c)?])
            }
            (Conn::Datagrams(d), Some(c)) => (shape::DATAGRAMS, Some(d.local), d.peer, vec![duplicate(c)?]),
            _ => return Err(EOPNOTSUPP),
        };
        let closed = matches!(&st.conn, Conn::Datagrams(d) if d.closed);
        st.shared = true;
        let travelling = Travelling {
            domain: self.family.domain(),
            kind: self.kind.sock_type(),
            shape,
            local,
            remote,
            options: st.options,
            shut: (st.shut_read, st.shut_write),
            closed,
        };
        Ok((travelling, handles))
    }

    /// The socket that came from another process ([`Inet::travelling`]),
    /// with the handles that carry it; `None` if they are not what it
    /// says.
    pub fn arrived(t: Travelling, handles: Vec<Handle>) -> Option<Inet> {
        let family = Family::of(t.domain)?;
        let kind = match t.kind {
            STREAM => Kind::Stream,
            DGRAM => Kind::Datagram,
            _ => return None,
        };
        let is = |h: &Handle, ty: ObjectType| h.basic_info().is_ok_and(|i| i.object_type == ty as u32);
        let mut handles = handles.into_iter();
        let mut next = |ty: ObjectType| handles.next().filter(|h| is(h, ty));
        let mut state = State::new();
        let channel = |h: Handle| Some(Arc::new(Channel::from_handle(h)));
        state.conn = match (t.shape, kind) {
            (shape::IDLE, _) => Conn::Idle,
            (shape::BOUND, Kind::Stream) => {
                state.channel = channel(next(ObjectType::Channel)?);
                Conn::Bound { local: t.local? }
            }
            (shape::LISTENING, Kind::Stream) => {
                state.channel = channel(next(ObjectType::Channel)?);
                Conn::Listening { local: t.local? }
            }
            (shape::STREAM, Kind::Stream) => {
                let bytes = Socket::from_handle(next(ObjectType::Socket)?);
                state.channel = channel(next(ObjectType::Channel)?);
                Conn::Stream(Stream { local: t.local?, remote: t.remote?, bytes: Arc::new(Bytes::new(bytes)) })
            }
            (shape::DATAGRAMS, Kind::Datagram) => {
                state.channel = channel(next(ObjectType::Channel)?);
                Conn::Datagrams(Datagrams { local: t.local?, peer: t.remote, held: None, closed: t.closed })
            }
            _ => return None,
        };
        state.options = t.options;
        (state.shut_read, state.shut_write) = t.shut;
        state.shared = true;
        Some(Inet { kind, family, state: Mutex::new(state) })
    }

    /// `connect`: a stream socket connects (waiting for the connection
    /// unless `nonblock`: `EINPROGRESS`); a datagram socket takes `peer` as
    /// its peer (`None`: none).
    pub fn connect(&self, peer: Option<SocketAddr>, nonblock: bool) -> SysResult {
        match (self.kind, peer) {
            (Kind::Datagram, peer) => self.connect_datagrams(peer),
            (Kind::Stream, Some(remote)) => self.connect_stream(remote, nonblock),
            (Kind::Stream, None) => Err(EAFNOSUPPORT),
        }
    }

    fn connect_datagrams(&self, peer: Option<SocketAddr>) -> SysResult {
        let mut st = self.state();
        if peer.is_none() && matches!(st.conn, Conn::Idle) {
            return Ok(0);
        }
        self.autobind(&mut st)?;
        if let Conn::Datagrams(d) = &mut st.conn {
            d.peer = peer;
            // What came from others before is not for it.
            if let (Some(p), Some((from, _))) = (peer, &d.held)
                && !(from.ip() == p.ip() && from.port() == p.port())
            {
                d.held = None;
            }
        }
        Ok(0)
    }

    fn connect_stream(&self, remote: SocketAddr, nonblock: bool) -> SysResult {
        let end = {
            let mut st = self.state();
            // A connection that failed since the last call says so now.
            if let Some(e) = st.take_error() {
                return Err(e);
            }
            let options = st.options.tcp();
            let connecting = match st.conn {
                Conn::Idle => {
                    let (channel, bytes) = network::with(|c| {
                        // Shared, so that the connection can travel to other
                        // processes, its bytes on a kernel socket.
                        let (ours, theirs) = Channel::create_shared().map_err(IpcError::Kernel)?;
                        let (bytes, their_bytes) = Socket::create().map_err(IpcError::Kernel)?;
                        Ok(c.tcp_connect_bytes(remote, options, theirs, their_bytes)?.map(|()| (ours, bytes)))
                    })
                    .map_err(network::errno)?
                    .map_err(network::errno)?;
                    st.channel = Some(Arc::new(channel));
                    Some(Conn::Connecting { bytes, from: None })
                }
                // From its address, on its channel.
                Conn::Bound { local } => {
                    let channel = st.channel.clone().ok_or(EINVAL)?;
                    let bytes = send_with(&channel, |e| {
                        let (bytes, theirs) = Socket::create().map_err(error::kernel)?;
                        SocketRequest::Connect { remote, options, bytes: theirs }.encode(e);
                        Ok(bytes)
                    })?;
                    Some(Conn::Connecting { bytes, from: Some(local) })
                }
                Conn::Connecting { .. } if nonblock => return Err(EALREADY),
                Conn::Connecting { .. } => None,
                Conn::Stream(_) | Conn::Listening { .. } => return Err(EISCONN),
                Conn::Datagrams(_) => return Err(EINVAL),
            };
            if let Some(connecting) = connecting {
                st.signalled = 0;
                st.conn = connecting;
                st.shut_read = false;
                st.shut_write = false;
                if nonblock {
                    return Err(EINPROGRESS);
                }
            }
            deadline(st.options.send_timeout)
        };
        loop {
            let (channel, signals) = {
                let mut st = self.state();
                match st.conn {
                    Conn::Stream(_) => return Ok(0),
                    Conn::Connecting { .. } => {}
                    _ => return Err(st.take_error().unwrap_or(ECONNREFUSED)),
                }
                Self::waiter(&st, Want::Out).ok_or(ECONNREFUSED)?
            };
            // A connection that takes longer than the socket's send
            // time-out goes on without the caller, as one a signal
            // interrupts does (`EINTR`, as POSIX has it: never started
            // again, which would find it connecting).
            match wait(&channel, signals, end) {
                Ok(()) => {}
                Err(EINTR | ERESTARTSYS) => return Err(EINTR),
                Err(_) => return Err(EINPROGRESS),
            }
        }
    }

    /// `shutdown`. A connection's stream ends for every process that has
    /// it, as on Linux: its kernel socket stops writing.
    pub fn shutdown(&self, read: bool, write: bool) -> SysResult {
        let mut st = self.state();
        let connected = match &st.conn {
            Conn::Stream(_) => true,
            Conn::Datagrams(d) => d.peer.is_some(),
            _ => false,
        };
        if !connected {
            return Err(ENOTCONN);
        }
        st.shut_read |= read;
        if write && !st.shut_write {
            st.shut_write = true;
            if let Conn::Stream(s) = &st.conn {
                // The service reads what came before, then the end.
                let _ = s.bytes.socket.shutdown();
            }
        }
        Ok(0)
    }

    /// Bytes that can be read without waiting (`FIONREAD`): a datagram
    /// socket's next datagram's.
    pub fn available(&self) -> usize {
        let mut st = self.state();
        match &st.conn {
            Conn::Stream(s) => s.bytes.available(),
            Conn::Datagrams(_) => st.next_datagram(true).map_or(0, |(_, data)| data.len()),
            _ => 0,
        }
    }

    /// Receives into `bufs` in turn: from a stream what is there (waiting
    /// for something unless `nonblock`; all of it with `waitall`), from a
    /// datagram socket one datagram, cut to fit. Leaves it to be read
    /// again with `peek`. Returns the count (a datagram's whole length with
    /// `trunc`), who sent it (datagrams) and whether it was cut short.
    pub fn recv(&self, bufs: &mut [&mut [u8]], how: Recv) -> Result<Received, isize> {
        match self.kind {
            Kind::Stream => self.recv_stream(bufs, how),
            Kind::Datagram => self.recv_datagram(bufs, how),
        }
    }

    /// The connection's bytes and whether this end reads no more, once it
    /// is made (waiting for it unless `nonblock`).
    fn connection(&self, nonblock: bool, want: Want, end: u64) -> Result<(Arc<Bytes>, bool, bool), isize> {
        loop {
            let (channel, signals) = {
                let mut st = self.state();
                match &st.conn {
                    Conn::Stream(s) => return Ok((s.bytes.clone(), st.shut_read, st.shut_write)),
                    Conn::Connecting { .. } => {}
                    _ => return Err(st.take_error().unwrap_or(ENOTCONN)),
                }
                if let Some(e) = st.take_error() {
                    return Err(e);
                }
                if nonblock {
                    return Err(EAGAIN);
                }
                Self::waiter(&st, want).ok_or(ENOTCONN)?
            };
            wait(&channel, signals, end)?;
        }
    }

    fn recv_stream(&self, bufs: &mut [&mut [u8]], how: Recv) -> Result<Received, isize> {
        let end = deadline(self.state.lock().options.recv_timeout);
        let (bytes, shut_read, _) = self.connection(how.nonblock, Want::In, end)?;
        let (got, ended) = match bufs {
            [] => (0, false),
            [buf] => read_bytes(&bytes, buf, how, end, shut_read)?,
            _ => {
                // Into one buffer, then into the program's in turn.
                let mut all = vec![0u8; bufs.iter().map(|b| b.len()).sum()];
                let (got, ended) = read_bytes(&bytes, &mut all, how, end, shut_read)?;
                let mut at = 0;
                for buf in bufs.iter_mut() {
                    let k = buf.len().min(got - at);
                    buf[..k].copy_from_slice(&all[at..at + k]);
                    at += k;
                }
                (got, ended)
            }
        };
        if ended {
            // The end of the stream, or the error that ended it.
            if let Some(e) = self.state().take_error() {
                return Err(e);
            }
        }
        Ok(Received { len: got, from: None, truncated: false })
    }

    fn recv_datagram(&self, bufs: &mut [&mut [u8]], how: Recv) -> Result<Received, isize> {
        let end = deadline(self.state.lock().options.recv_timeout);
        loop {
            let channel = {
                let mut st = self.state();
                self.autobind(&mut st)?;
                if let Some((from, data)) = st.next_datagram(how.peek) {
                    let mut n = 0;
                    for buf in bufs.iter_mut() {
                        let k = buf.len().min(data.len() - n);
                        buf[..k].copy_from_slice(&data[n..n + k]);
                        n += k;
                        if n == data.len() {
                            break;
                        }
                    }
                    let len = data.len();
                    let reported = if how.trunc { len } else { n };
                    return Ok(Received { len: reported, from: Some(from), truncated: n < len });
                }
                if let Some(e) = st.take_error() {
                    return Err(e);
                }
                if st.shut_read || matches!(&st.conn, Conn::Datagrams(d) if d.closed) || st.lost {
                    return Ok(Received { len: 0, from: None, truncated: false });
                }
                if how.nonblock {
                    return Err(EAGAIN);
                }
                st.channel.clone().ok_or(ENOTCONN)?
            };
            // Another process, or thread, may take what comes first: then
            // wait again.
            wait(&channel, 0, end)?;
        }
    }

    /// Sends `data`: on a stream all of it, waiting for room unless
    /// `nonblock` (then what fits); on a datagram socket one datagram, to
    /// `dest` or its peer. A write to a stream that ended raises `SIGPIPE`
    /// unless `nosignal`.
    pub fn send(&self, data: &[u8], dest: Option<SocketAddr>, nonblock: bool, nosignal: bool) -> SysResult {
        let r = match self.kind {
            // A stream has its peer: an address given is passed over, as
            // on Linux.
            Kind::Stream => self.send_stream(data, nonblock),
            Kind::Datagram => self.send_datagram(data, dest, nonblock),
        };
        if r == Err(EPIPE) && !nosignal {
            signal::raise(crate::linux::sig::PIPE);
        }
        r
    }

    fn send_stream(&self, data: &[u8], nonblock: bool) -> SysResult {
        let end = deadline(self.state.lock().options.send_timeout);
        let (bytes, _, shut_write) = match self.connection(nonblock, Want::Out, end) {
            Ok(c) => c,
            // Not connected: nobody to write to.
            Err(ENOTCONN) => return Err(EPIPE),
            Err(e) => return Err(e),
        };
        if shut_write {
            return Err(EPIPE);
        }
        match bytes.send_until(data, nonblock, false, end) {
            // The stream ended: the error that ended it, if the service
            // said one.
            Err(EPIPE) => Err(self.state().take_error().unwrap_or(EPIPE)),
            other => other,
        }
    }

    fn send_datagram(&self, data: &[u8], dest: Option<SocketAddr>, nonblock: bool) -> SysResult {
        if data.len() > MAX_DATAGRAM {
            return Err(EMSGSIZE);
        }
        let end = deadline(self.state.lock().options.send_timeout);
        loop {
            let channel = {
                let mut st = self.state();
                self.autobind(&mut st)?;
                if let Some(e) = st.take_error() {
                    return Err(e);
                }
                let shut_write = st.shut_write;
                let Some(channel) = st.channel.clone() else { return Err(ENOTCONN) };
                let Conn::Datagrams(d) = &mut st.conn else { return Err(ENOTCONN) };
                if d.closed || shut_write {
                    return Err(EPIPE);
                }
                let dest = dest.or(d.peer).ok_or(EDESTADDRREQ)?;
                match request(&channel, |e| SocketRequest::encode_send_to(dest, data, e)) {
                    Ok(()) => return Ok(data.len()),
                    Err(abi::Error::ShouldWait) if nonblock => return Err(EAGAIN),
                    Err(abi::Error::ShouldWait) => {}
                    Err(_) => {
                        d.closed = true;
                        return Err(EPIPE);
                    }
                }
                channel
            };
            // Room in the channel, which other processes may take first.
            match crate::interrupt::wait_one(&channel.0, WRITABLE | PEER_CLOSED, end) {
                Ok(_) => {}
                Err(abi::Error::TimedOut) => return Err(EAGAIN),
                Err(abi::Error::Interrupted) => return Err(crate::stream::interrupted(end)),
                Err(e) => return Err(error::kernel(e)),
            }
        }
    }

    /// What `poll` makes of the socket, for reading (`want_in`) and writing
    /// (`want_out`).
    pub fn readiness(&self, want_in: bool, want_out: bool) -> Readiness {
        let st = self.state();
        let error = st.error.is_some();
        let seq = st.events;
        let mut r =
            Readiness { input: error, output: error, hangup: st.lost, read_hangup: false, error, wait: None, seq };
        let channel = |signals: u32| st.channel.clone().map(|c| (Waitable::Channel(c), signals));
        match &st.conn {
            // A stream socket that is not connected is hung up, as on Linux
            // (and a write to it fails at once).
            Conn::Idle | Conn::Bound { .. } => {
                r.output = true;
                r.hangup |= self.kind == Kind::Stream;
            }
            Conn::Connecting { .. } => {
                let mut signals = READABLE | PEER_CLOSED;
                if want_in && !r.input {
                    signals |= READY_IN;
                }
                if want_out && !r.output {
                    signals |= READY_OUT;
                }
                r.wait = channel(signals);
            }
            Conn::Stream(s) => {
                let now = s.bytes.socket.wait(READABLE | WRITABLE | PEER_CLOSED | PEER_WRITE_DISABLED, 0).unwrap_or(0);
                let closed = now & PEER_CLOSED != 0;
                let ended = now & PEER_WRITE_DISABLED != 0;
                r.read_hangup = closed || ended || st.shut_read;
                r.input |= now & READABLE != 0 || r.read_hangup;
                // A write to a stream that ended does not wait: it fails.
                r.output |= now & WRITABLE != 0 || closed || st.shut_write;
                r.hangup |= closed || (r.read_hangup && st.shut_write);
                // What is not raised yet (raised, it would be waited for
                // without waiting).
                let mut signals = 0;
                if !closed {
                    signals |= PEER_CLOSED;
                }
                if !ended && !closed {
                    signals |= PEER_WRITE_DISABLED;
                }
                if want_in && !r.input {
                    signals |= READABLE;
                }
                if want_out && !r.output {
                    signals |= WRITABLE;
                }
                r.wait = (signals != 0).then(|| (Waitable::Bytes(s.bytes.clone()), signals));
            }
            // Connections and datagrams stay on the channel until taken:
            // more of them waiting do not change a socket that has some
            // (waiting for them would not wait at all).
            Conn::Listening { .. } => {
                r.input |= st.lost || st.channel.as_ref().is_some_and(|c| c.wait(READABLE, 0).is_ok());
                let signals = if r.input { 0 } else { READABLE };
                r.wait = if st.lost { None } else { channel(signals | PEER_CLOSED) };
            }
            Conn::Datagrams(d) => {
                let now =
                    st.channel.as_ref().and_then(|c| c.wait(READABLE | WRITABLE | PEER_CLOSED, 0).ok()).unwrap_or(0);
                let closed = d.closed || st.lost || (now & PEER_CLOSED != 0 && now & READABLE == 0);
                r.input |= d.held.is_some() || now & READABLE != 0 || closed || st.shut_read;
                r.output |= now & WRITABLE != 0 || closed;
                r.hangup |= closed;
                r.read_hangup = st.shut_read;
                let mut signals = if now & PEER_CLOSED != 0 { 0 } else { PEER_CLOSED };
                if !r.input {
                    signals |= READABLE;
                }
                if want_out && !r.output {
                    signals |= WRITABLE;
                }
                r.wait = if closed { None } else { channel(signals) };
            }
        }
        r
    }

    /// The channel's object id, which identifies the socket (`fstat`).
    pub fn id(&self) -> u64 {
        self.state.lock().channel.as_ref().map_or(0, |c| c.0.koid())
    }
}

impl Drop for Inet {
    fn drop(&mut self) {
        // With `SO_LINGER` on and no time, a connection ends with a reset
        // (unless other processes may have it); otherwise its stream ends
        // once every process has closed it, the data sent before delivered.
        let st = self.state.get_mut();
        if st.options.linger == Some(0)
            && !st.shared
            && let (Conn::Stream(_), Some(channel)) = (&st.conn, &st.channel)
        {
            let _ = request(channel, |e| SocketRequest::Abort {}.encode(e));
        }
    }
}

/// How to receive.
#[derive(Debug, Clone, Copy, Default)]
pub struct Recv {
    pub nonblock: bool,
    /// Leave what is read to be read again (`MSG_PEEK`).
    pub peek: bool,
    /// Fill the buffers, unless the stream ends first (`MSG_WAITALL`).
    pub waitall: bool,
    /// Report a datagram's whole length (`MSG_TRUNC`).
    pub trunc: bool,
}

/// What a receive got.
#[derive(Debug)]
pub struct Received {
    pub len: usize,
    /// Who sent it (a datagram socket's).
    pub from: Option<SocketAddr>,
    /// A datagram was longer than the buffers.
    pub truncated: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sockets_travel_as_they_are() {
        let options = Options { nodelay: true, recv_timeout: Some(9), hops: 3, v6only: true, ..Options::default() };
        let t = Travelling {
            domain: Family::V6.domain(),
            kind: STREAM,
            shape: shape::STREAM,
            local: Some("[::1]:8080".parse().unwrap()),
            remote: Some("[::1]:40000".parse().unwrap()),
            options,
            shut: (false, true),
            closed: false,
        };
        let mut e = ipc::Encoder::new();
        t.clone().encode(&mut e);
        let back = <Travelling as ipc::Decode>::decode(&mut ipc::Decoder::new(&e.bytes, Vec::new()));
        assert_eq!(back.ok(), Some(t));
    }
}
