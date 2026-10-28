// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The network protocols served by `network`, the network service.
//!
//! * [`net`] (service `"net"`): what applications use — TCP and UDP sockets,
//!   ping, name resolution, the state of interfaces and routes, and (for
//!   the Settings app and the Terminal) interface configuration. Most
//!   applications use the `net` library, which wraps this protocol in
//!   `TcpStream`-like types.
//! * [`netdev`] (service `"netdev"`): network devices (Ethernet drivers, and
//!   the Wi-Fi service for each wireless interface) attach to the stack.
//!   Frames travel through a [`crate::netring::Link`], never in messages.
//!
//! # Who may do what
//!
//! The service knows who opened each connection (the registry's client
//! identity), and:
//!
//! * only the device manager (whose drivers, the driver VM's programs
//!   included, speak as it) and the Wi-Fi service attach devices (the
//!   connections of others are closed);
//! * only Settings changes how the network is set up: it configures
//!   interfaces, renews leases, sets the DNS servers and sets the firewall
//!   (`PermissionDenied` for everyone else). The programs run in the
//!   Terminal speak for it, so the Terminal, which runs whatever the user
//!   asks, only looks: Settings and the Terminal flush the DNS cache, read
//!   the firewall's settings, log and counts, list every socket, and see the
//!   interfaces' hardware addresses (zeros for everyone else); others list
//!   their own sockets;
//! * the firewall ([`FirewallConfig`]) decides what other machines may
//!   start with this one, and which applications may use the network at
//!   all (`PermissionDenied` for those it keeps off);
//! * when an application its settings say nothing about starts taking
//!   connections from other machines, the user is asked whether it may
//!   ([`FirewallQuestion`]): only the desktop shell hears the questions and
//!   answers them, and an answer sets nothing but the rule for the
//!   application asked about;
//! * limits apply per process, whatever the number of connections: sockets
//!   (the connections a listener accepts included), lookups in progress,
//!   status watchers and connections; and of each, a part is kept for
//!   system services, so that applications together cannot use it all.
//!
//! # Sockets
//!
//! Every socket is its own channel. The client creates the channel pair,
//! keeps one end and passes the other in the request that opens the socket
//! ([`net::Client::tcp_connect`], ...). Both sides then exchange
//! [`SocketRequest`]s (client to `network`) and [`SocketEvent`]s (`network`
//! to client) as plain messages on that channel. Closing the channel closes
//! the socket: a TCP connection is shut down gracefully, and the datagrams
//! a UDP socket was sent go out first, if the stack can send them within a
//! few seconds.
//!
//! A socket bound to an address holds its port there: as on Linux, sockets
//! share a port when no address is both's, as an IPv4 socket on `0.0.0.0`
//! and an IPv6-only one on `::` do (`v6only`; without it, `::` is IPv4's
//! addresses too), or sockets on different addresses. What comes to an
//! address is the one socket's whose it is.
//!
//! Data flow is credit-based in both directions, which bounds the memory a
//! socket can pin in the kernel:
//!
//! * **Sending.** `network` grants send credit in bytes
//!   ([`SocketEvent::Connected`], [`SocketEvent::SendCredit`]). A client may
//!   only send that many bytes; `network` grants more as the stack's send
//!   buffer frees up (for TCP, what the client may send always fits the
//!   buffer, so closing the channel delivers everything sent before it).
//! * **Receiving.** `network` keeps at most [`RECEIVE_WINDOW`] bytes of
//!   [`SocketEvent::Data`] (or datagrams) unacknowledged; the client returns
//!   credit with [`SocketRequest::Consumed`] as its reader takes the data.
//!   `network` takes data from the stack only as far as the client's
//!   channel has room for it: when the client stops reading, the data waits
//!   in the stack and TCP's own flow control slows the peer.
//!
//! # Sockets that processes share
//!
//! Credit belongs to whoever took it off the channel, so a socket that
//! speaks the protocol above has one client. The POSIX layer's sockets,
//! which processes share as they share them on Linux, open sockets that
//! need no credit:
//!
//! * **TCP with its bytes on a kernel socket**
//!   ([`net::Client::tcp_connect_bytes`], [`net::Client::tcp_listen_bytes`],
//!   whose connections arrive as [`SocketEvent::AcceptedBytes`], and
//!   [`net::Client::tcp_bind`], whose socket listens or connects later on
//!   its channel: [`SocketRequest::Listen`], [`SocketRequest::Connect`]).
//!   The connection's bytes go both ways on a kernel socket of which the
//!   client holds one end: its buffers are the flow control, whoever holds
//!   the end reads and writes, and a reader who stops reading leaves the
//!   data in the stack (TCP's window closes).
//!   The end of the received stream is the kernel socket's (`network` stops
//!   writing), and the client's end of its stream is its end's (it stops
//!   writing, or every holder closes it). The channel carries only
//!   [`SocketEvent::Connected`], and [`SocketEvent::Error`], after which
//!   `network` closes its end of the kernel socket; and the requests
//!   [`SocketRequest::Abort`] and [`SocketRequest::SetNoDelay`].
//! * **UDP without credit** ([`net::Client::udp_bind_unmetered`]). The
//!   channel's room is the flow control both ways: `network` writes a
//!   datagram while the channel has room for it (otherwise it waits in the
//!   stack, which drops what does not fit, as UDP may), and stops reading
//!   [`SocketRequest::SendTo`]s while the datagrams it has not sent yet are
//!   more than [`UDP_UNMETERED_WINDOW`] bytes, so that a client sending
//!   too fast waits for room in the channel. Each datagram is one message,
//!   which any process holding the channel may take.

use alloc::string::String;
use alloc::vec::Vec;
use core::net::{IpAddr, SocketAddr};

use ipc::{Bytes, enumeration, message, protocol, union};
use runtime::object::{Channel, Socket};

use crate::netring::LinkEndpoints;

/// Largest payload of one [`SocketRequest::Send`] / `SendTo` message.
pub const MAX_SEND: usize = 16 * 1024;
/// Unacknowledged received bytes `network` allows per socket.
pub const RECEIVE_WINDOW: u32 = 256 * 1024;
/// Bytes of datagrams a UDP socket without credit may have waiting in
/// `network` to be sent before it stops reading the socket's requests.
pub const UDP_UNMETERED_WINDOW: usize = 256 * 1024;
/// Largest UDP payload a socket carries: what fits one channel message with
/// the datagram's address, a little less than an IPv4 datagram's 65507
/// bytes.
pub const MAX_DATAGRAM: usize = abi::CHANNEL_MAX_BYTES - DATAGRAM_MESSAGE_OVERHEAD;
/// The rest of a [`SocketEvent::Datagram`] or [`SocketRequest::SendTo`]
/// message: its header, the variant's tag, an IPv6 socket address and the
/// payload's length.
const DATAGRAM_MESSAGE_OVERHEAD: usize = ipc::HEADER_LEN + 4 + 27 + 4;

enumeration! {
    /// Why a network operation failed.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub enum NetError {
        /// The network service or the interface is not available.
        Unavailable = 1,
        /// No route to the destination network (not connected?).
        NoRoute = 2,
        /// The destination host did not answer address resolution.
        HostUnreachable = 3,
        /// The remote host refused the connection.
        ConnectionRefused = 4,
        /// The remote host reset the connection.
        ConnectionReset = 5,
        TimedOut = 6,
        /// The local address and port are already in use.
        AddressInUse = 7,
        /// The local address does not belong to this machine.
        AddressNotAvailable = 8,
        InvalidArgument = 9,
        /// The name does not exist (DNS NXDOMAIN) or has no addresses.
        NameNotFound = 10,
        /// The DNS servers failed or did not answer.
        DnsFailure = 11,
        /// Too many sockets.
        LimitReached = 12,
        /// The socket is closed.
        Closed = 13,
        PermissionDenied = 14,
        NotSupported = 15,
        /// The connection was dropped locally (for example its interface
        /// disappeared).
        Aborted = 16,
        /// The message is larger than the protocol allows.
        MessageTooLarge = 17,
        /// The interface is down (for example Wi-Fi disconnected).
        NetworkDown = 18,
        /// The destination is unreachable (an ICMP error).
        DestinationUnreachable = 19,
        /// A packet's time to live ran out (an ICMP error; used by ping).
        TimeExceeded = 20,
        /// The client broke the socket protocol (for example sent more than
        /// its credit).
        ProtocolViolation = 21,
        Internal = 22,
    }
}

impl NetError {
    pub const fn description(self) -> &'static str {
        match self {
            NetError::Unavailable => "the network is not available",
            NetError::NoRoute => "no route to the network",
            NetError::HostUnreachable => "host unreachable",
            NetError::ConnectionRefused => "connection refused",
            NetError::ConnectionReset => "connection reset by peer",
            NetError::TimedOut => "timed out",
            NetError::AddressInUse => "address already in use",
            NetError::AddressNotAvailable => "address not available",
            NetError::InvalidArgument => "invalid argument",
            NetError::NameNotFound => "name not found",
            NetError::DnsFailure => "DNS lookup failed",
            NetError::LimitReached => "too many sockets",
            NetError::Closed => "socket closed",
            NetError::PermissionDenied => "permission denied",
            NetError::NotSupported => "not supported",
            NetError::Aborted => "connection aborted",
            NetError::MessageTooLarge => "message too large",
            NetError::NetworkDown => "network is down",
            NetError::DestinationUnreachable => "destination unreachable",
            NetError::TimeExceeded => "time to live exceeded",
            NetError::ProtocolViolation => "socket protocol violation",
            NetError::Internal => "internal network error",
        }
    }
}

impl core::fmt::Display for NetError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.description())
    }
}

enumeration! {
    /// Which address families a name lookup should return.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum AddrFamily {
        /// IPv4, and IPv6 when the machine has IPv6 routes; IPv4 addresses
        /// first (they work through every NAT, while IPv6 often reaches the
        /// local network only).
        Any = 0,
        V4 = 4,
        V6 = 6,
    }
}

message! {
    /// Options for a TCP connection.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct TcpOptions {
        /// Send small segments immediately (disables Nagle's algorithm).
        pub nodelay: bool,
        /// Keep-alive probe interval in milliseconds (0 = off; otherwise at
        /// least 1000).
        pub keepalive_ms: u32,
        /// Give up connecting after this many milliseconds (0 = 20 s).
        pub connect_timeout_ms: u32,
        /// Abort the connection when it is idle (nothing received) this long
        /// in milliseconds (0 = never).
        pub idle_timeout_ms: u32,
    }
}

union! {
    /// Messages a client sends on a socket channel.
    #[derive(Debug)]
    pub enum SocketRequest {
        /// TCP: append bytes to the stream (at most [`MAX_SEND`] and the
        /// remaining send credit).
        1 => Send { data: Bytes },
        /// UDP: send one datagram (counts against the send credit).
        2 => SendTo { dest: SocketAddr, data: Bytes },
        /// The client has taken this many received bytes (returns receive
        /// credit).
        3 => Consumed { bytes: u32 },
        /// TCP: no more data will be sent (sends FIN; receiving continues).
        4 => Shutdown {},
        /// TCP: drop the connection at once (sends RST).
        5 => Abort {},
        /// TCP: change the Nagle setting.
        6 => SetNoDelay { nodelay: bool },
        /// Ping socket: send an ICMP echo request of `size` payload bytes.
        /// `ttl` 0 means the default (64).
        7 => Echo { dest: IpAddr, seq: u16, ttl: u8, size: u16 },
        /// TCP socket of [`net::Client::tcp_bind`]: listen on its address,
        /// its connections arriving as [`SocketEvent::AcceptedBytes`]. The
        /// answer comes on `reply`: [`SocketEvent::Listening`], or
        /// [`SocketEvent::Error`] (the socket stays bound). A listener
        /// answers with its address.
        8 => Listen { backlog: u32, options: TcpOptions, reply: Channel },
        /// TCP socket of [`net::Client::tcp_bind`]: connect to `remote` from
        /// its address, as [`net::Client::tcp_connect_bytes`] does, the
        /// connection's bytes on `bytes`.
        9 => Connect { remote: SocketAddr, options: TcpOptions, bytes: Socket },
    }
}

union! {
    /// Messages `network` sends on a socket channel.
    #[derive(Debug)]
    pub enum SocketEvent {
        /// TCP: the connection is established. `credit` is the initial send
        /// credit in bytes.
        1 => Connected { local: SocketAddr, remote: SocketAddr, credit: u32 },
        /// TCP: received bytes.
        2 => Data { data: Bytes },
        /// UDP: a received datagram.
        3 => Datagram { from: SocketAddr, data: Bytes },
        /// More send credit.
        4 => SendCredit { bytes: u32 },
        /// TCP: the peer finished sending (end of stream). Sending may
        /// continue.
        5 => Eof {},
        /// The socket failed and is closed (connection refused or reset,
        /// timeout, interface gone, ...).
        6 => Error { error: NetError },
        /// TCP listener: a new connection. `socket` speaks this same
        /// protocol and starts with `credit` bytes of send credit.
        7 => Accepted { socket: Channel, local: SocketAddr, remote: SocketAddr, credit: u32 },
        /// Ping socket: an echo reply. `rtt_us` is the round-trip time.
        8 => EchoReply { from: IpAddr, seq: u16, size: u16, rtt_us: u32 },
        /// Ping socket: an echo request failed: no answer in time
        /// (`TimedOut`), it could not be sent (`NoRoute`, `NetworkDown`,
        /// ...), or the router `from` reported it undeliverable
        /// (`DestinationUnreachable`, `HostUnreachable`) or out of hops
        /// (`TimeExceeded`).
        9 => EchoFailed { seq: u16, from: Option<IpAddr>, error: NetError },
        /// TCP listener of [`net::Client::tcp_listen_bytes`]: a new
        /// connection, whose bytes go on `bytes` (see the module's
        /// documentation) and its news and requests on `socket`.
        10 => AcceptedBytes { socket: Channel, bytes: Socket, local: SocketAddr, remote: SocketAddr },
        /// The answer to [`SocketRequest::Listen`], on its reply channel:
        /// the socket listens on `local`.
        11 => Listening { local: SocketAddr },
    }
}

impl SocketRequest {
    /// Encodes [`SocketRequest::Send`] straight from `data` (the bytes the
    /// request would encode, without copying `data` into a vector first).
    pub fn encode_send(data: &[u8], e: &mut ipc::Encoder) {
        ipc::Encode::encode(1u32, e);
        ipc::Encode::encode(data, e);
    }

    /// Encodes [`SocketRequest::SendTo`] straight from `data`.
    pub fn encode_send_to(dest: SocketAddr, data: &[u8], e: &mut ipc::Encoder) {
        ipc::Encode::encode(2u32, e);
        ipc::Encode::encode(dest, e);
        ipc::Encode::encode(data, e);
    }
}

impl SocketEvent {
    /// Encodes [`SocketEvent::Data`] straight from `data` (the bytes the
    /// event would encode, without copying `data` into a vector first).
    pub fn encode_data(data: &[u8], e: &mut ipc::Encoder) {
        ipc::Encode::encode(2u32, e);
        ipc::Encode::encode(data, e);
    }

    /// Encodes [`SocketEvent::Datagram`] straight from `data`.
    pub fn encode_datagram(from: SocketAddr, data: &[u8], e: &mut ipc::Encoder) {
        ipc::Encode::encode(3u32, e);
        ipc::Encode::encode(from, e);
        ipc::Encode::encode(data, e);
    }
}

union! {
    /// The answer to a name lookup ([`net::Client::resolve`]), sent once on
    /// the reply channel.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum ResolveResult {
        /// The addresses (preferred first) and the time they may be cached.
        1 => Found { addresses: Vec<IpAddr>, ttl_s: u32, canonical: String },
        2 => Failed { error: NetError },
    }
}

enumeration! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum InterfaceKind {
        Loopback = 1,
        Ethernet = 2,
        Wireless = 3,
    }
}

enumeration! {
    /// Where an address came from.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum AddressOrigin {
        Static = 1,
        Dhcp = 2,
        /// IPv6 stateless autoconfiguration.
        Slaac = 3,
        /// IPv6 link-local (fe80::/64).
        LinkLocal = 4,
        Loopback = 5,
    }
}

message! {
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct AddressInfo {
        pub address: IpAddr,
        pub prefix_len: u8,
        pub origin: AddressOrigin,
        /// Seconds the address stays valid (0 = forever).
        pub valid_s: u32,
    }
}

enumeration! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum DhcpState {
        /// DHCP is not used on this interface.
        Off = 0,
        /// Looking for a server.
        Discovering = 1,
        /// Waiting for the server to confirm an address.
        Requesting = 2,
        /// An address is leased.
        Bound = 3,
        /// No server answered (still retrying).
        NoServer = 4,
    }
}

message! {
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct DhcpInfo {
        pub state: DhcpState,
        pub server: Option<IpAddr>,
        /// Length of the lease in seconds.
        pub lease_s: u32,
        /// Seconds since the lease was obtained.
        pub age_s: u32,
    }
}

message! {
    /// Traffic counters of an interface.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct InterfaceStats {
        pub rx_packets: u64,
        pub rx_bytes: u64,
        pub tx_packets: u64,
        pub tx_bytes: u64,
        /// Frames lost because a queue was full.
        pub rx_dropped: u64,
        pub tx_dropped: u64,
        /// Malformed frames.
        pub rx_errors: u64,
    }
}

union! {
    /// How an interface gets its IPv4 configuration.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum IpConfig {
        1 => Dhcp {},
        2 => Static { address: IpAddr, prefix_len: u8, gateway: Option<IpAddr>, dns: Vec<IpAddr> },
        /// No IPv4 address (IPv6 link-local and autoconfiguration only).
        3 => Disabled {},
    }
}

message! {
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct InterfaceInfo {
        /// `lo`, `eth0`, `wlan0`, ...
        pub name: String,
        pub kind: InterfaceKind,
        pub mac: [u8; 6],
        pub mtu: u32,
        /// The link is up (cable connected, Wi-Fi associated).
        pub link_up: bool,
        /// Driver (or service) behind the interface.
        pub driver: String,
        pub config: IpConfig,
        pub addresses: Vec<AddressInfo>,
        pub gateways: Vec<IpAddr>,
        pub dns: Vec<IpAddr>,
        pub dhcp: DhcpInfo,
        /// Route preference: lower wins (Ethernet 100, Wi-Fi 600).
        pub metric: u32,
        pub stats: InterfaceStats,
    }
}

enumeration! {
    /// How far the machine can reach.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    pub enum Connectivity {
        /// No interface with an address.
        Offline = 0,
        /// An address, but no default route (local network only).
        Local = 1,
        /// A default route (whether the Internet answers is checked by
        /// diagnostics).
        Routable = 2,
    }
}

message! {
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct NetStatus {
        pub connectivity: Connectivity,
        /// Interface of the default route ("" if none).
        pub default_interface: String,
        pub default_gateway: Option<IpAddr>,
        pub dns_servers: Vec<IpAddr>,
        /// Seconds since boot when anything last changed.
        pub changed_s: u32,
    }
}

message! {
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct RouteInfo {
        /// Destination network.
        pub destination: IpAddr,
        pub prefix_len: u8,
        /// Next hop (None: directly connected).
        pub gateway: Option<IpAddr>,
        pub interface: String,
        pub metric: u32,
    }
}

enumeration! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum SocketKind {
        Tcp = 1,
        TcpListener = 2,
        Udp = 3,
        Ping = 4,
    }
}

message! {
    /// A socket as listed by `netstat`.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct SocketInfo {
        pub kind: SocketKind,
        pub local: Option<SocketAddr>,
        pub remote: Option<SocketAddr>,
        /// TCP state name ("ESTABLISHED", ...) or "".
        pub state: String,
        /// Process id of the owner (0 for a connection its client closed,
        /// which finishes on its own).
        pub owner: u64,
        pub interface: String,
        pub rx_queued: u32,
        pub tx_queued: u32,
    }
}

union! {
    /// Notifications on a [`net::Client::watch`] channel.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum NetEvent {
        /// The overall status changed.
        1 => StatusChanged { status: NetStatus },
        /// An interface appeared, disappeared or changed (link, addresses).
        2 => InterfaceChanged { name: String },
    }
}

/// Event ordinal of [`NetEvent`]s on a watch channel.
pub const NET_EVENT: u32 = 1;

// ---------------------------------------------------------------------
// The firewall

/// Most rules of each kind in a [`FirewallConfig`].
pub const MAX_FIREWALL_RULES: usize = 256;
/// Longest application name a firewall rule takes, in bytes.
pub const MAX_APP_NAME: usize = 64;

enumeration! {
    /// The transport protocol a port rule is for.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    pub enum FirewallProtocol {
        Tcp = 1,
        Udp = 2,
    }
}

union! {
    /// Where a firewall rule lets traffic come from.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Scope {
        /// Any address.
        1 => Anywhere {},
        /// The networks of the interface it comes in on: the subnets of its
        /// addresses, and the link-local ones (IPv4's 169.254.0.0/16, IPv6's
        /// fe80::/10).
        2 => LocalNetwork {},
        /// The addresses whose first `len` bits are `address`'s.
        3 => Prefix { address: IpAddr, len: u8 },
    }
}

impl Scope {
    /// Reads `anywhere` (or `any`), `local`, or a prefix (`192.0.2.0/24`;
    /// an address alone is a prefix of its whole length).
    pub fn parse(text: &str) -> Option<Scope> {
        match text {
            "anywhere" | "any" => return Some(Scope::Anywhere {}),
            "local" => return Some(Scope::LocalNetwork {}),
            _ => {}
        }
        let (address, len) = match text.split_once('/') {
            Some((a, l)) => (a.parse::<IpAddr>().ok()?, l.parse::<u8>().ok()?),
            None => {
                let a = text.parse::<IpAddr>().ok()?;
                (a, if a.is_ipv4() { 32 } else { 128 })
            }
        };
        let scope = Scope::Prefix { address, len };
        scope.is_valid().then_some(scope)
    }

    /// Whether a prefix is no longer than its address.
    fn is_valid(&self) -> bool {
        match self {
            Scope::Prefix { address: IpAddr::V4(_), len } => *len <= 32,
            Scope::Prefix { address: IpAddr::V6(_), len } => *len <= 128,
            _ => true,
        }
    }
}

impl core::fmt::Display for Scope {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Scope::Anywhere {} => f.write_str("anywhere"),
            Scope::LocalNetwork {} => f.write_str("the local network"),
            Scope::Prefix { address, len } => write!(f, "{address}/{len}"),
        }
    }
}

message! {
    /// Whether an application accepts connections and datagrams from other
    /// machines, and from where.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct AppRule {
        /// The application, by the name the system knows it by: an
        /// application's id (`music`), or `terminal` for the programs run in
        /// the Terminal, which speak for it.
        pub app: String,
        /// Where what it receives may come from (`None`: nowhere, which the
        /// user chose; the application is not asked about again).
        pub from: Option<Scope>,
    }
}

message! {
    /// Ports opened to other machines, or closed to them, whatever program
    /// uses them.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct PortRule {
        pub protocol: FirewallProtocol,
        /// The first and the last port of the range (the same for one port).
        pub first: u16,
        pub last: u16,
        /// Where the traffic comes from.
        pub from: Scope,
        /// Let it in; otherwise turn it away, whatever lets it in elsewhere.
        pub allow: bool,
    }
}

impl PortRule {
    /// Whether the rule is about `port` of `protocol`.
    pub fn covers(&self, protocol: FirewallProtocol, port: u16) -> bool {
        self.protocol == protocol && (self.first..=self.last).contains(&port)
    }
}

message! {
    /// The firewall's settings, which Settings changes and `network` keeps
    /// on the home disk ([`FirewallConfig::default`] holds until it has
    /// them).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct FirewallConfig {
        /// Off, nothing is filtered (packets no host sends are still
        /// dropped).
        pub enabled: bool,
        /// Only replies to what this machine sent come in: no rule lets
        /// anything else in.
        pub block_incoming: bool,
        /// What nobody asked for gets no answer: a closed port neither resets
        /// a connection nor says it is unreachable, so that a scan finds
        /// nothing.
        pub stealth: bool,
        /// Who gets an answer to a ping (`None`: nobody).
        pub ping: Option<Scope>,
        /// Keep a record of what was turned away.
        pub log: bool,
        /// Applications that accept connections from other machines, and
        /// those the user chose not to let in (the first rule for an
        /// application decides).
        pub incoming: Vec<AppRule>,
        /// Applications kept off the network (they still reach the machine
        /// itself, through the loopback interface).
        pub blocked: Vec<String>,
        /// Ports opened or closed to other machines.
        pub ports: Vec<PortRule>,
    }
}

impl Default for FirewallConfig {
    /// On: nothing comes in that this machine did not ask for, nothing
    /// answers a scan or a ping, and what is turned away is recorded.
    fn default() -> FirewallConfig {
        FirewallConfig {
            enabled: true,
            block_incoming: false,
            stealth: true,
            ping: None,
            log: true,
            incoming: Vec::new(),
            blocked: Vec::new(),
            ports: Vec::new(),
        }
    }
}

impl FirewallConfig {
    /// Whether the settings may be applied: `InvalidArgument` for more than
    /// [`MAX_FIREWALL_RULES`] rules of a kind, an application name that is
    /// empty, longer than [`MAX_APP_NAME`] or not printable, a prefix longer
    /// than its address, or a port range that is empty or starts at 0.
    pub fn check(&self) -> Result<(), NetError> {
        let name_ok = |n: &str| !n.is_empty() && n.len() <= MAX_APP_NAME && !n.chars().any(char::is_control);
        let ok = self.incoming.len() <= MAX_FIREWALL_RULES
            && self.blocked.len() <= MAX_FIREWALL_RULES
            && self.ports.len() <= MAX_FIREWALL_RULES
            && self.ping.as_ref().is_none_or(Scope::is_valid)
            && self.incoming.iter().all(|r| name_ok(&r.app) && r.from.as_ref().is_none_or(Scope::is_valid))
            && self.blocked.iter().all(|n| name_ok(n))
            && self.ports.iter().all(|r| r.first >= 1 && r.first <= r.last && r.from.is_valid());
        if ok { Ok(()) } else { Err(NetError::InvalidArgument) }
    }

    /// Whether `app` may use the network.
    pub fn allows_network(&self, app: &str) -> bool {
        !self.enabled || !self.blocked.iter().any(|b| b == app)
    }

    /// Where the sockets of `app` accept what other machines start from:
    /// `None` for nowhere (no rule for it, a rule that lets it in from
    /// nowhere, or it is kept off the network).
    pub fn incoming_for(&self, app: &str) -> Option<Scope> {
        if !self.allows_network(app) {
            return None;
        }
        self.incoming.iter().find(|r| r.app == app).and_then(|r| r.from)
    }

    /// Whether the user is to be asked about `app`, which takes connections
    /// from other machines: the firewall is on, lets something in, and its
    /// settings say nothing about the application (neither a rule for it
    /// nor keeping it off the network).
    pub fn undecided(&self, app: &str) -> bool {
        self.enabled
            && !self.block_incoming
            && !self.blocked.iter().any(|b| b == app)
            && !self.incoming.iter().any(|r| r.app == app)
    }
}

message! {
    /// An application that started taking connections or datagrams from
    /// other machines, about which the firewall's settings say nothing:
    /// until the user answers, nothing from other machines reaches it.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct FirewallQuestion {
        /// The application (as [`AppRule::app`] names it).
        pub app: String,
        /// What it listens with: the protocol and the port (the first, if it
        /// listens on several).
        pub protocol: FirewallProtocol,
        pub port: u16,
        /// When it was asked (Unix time, seconds).
        pub at: u64,
    }
}

union! {
    /// Notifications on a [`net::Client::watch_firewall`] channel.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum FirewallNotice {
        /// The user is to be asked about an application.
        1 => Asked { question: FirewallQuestion },
        /// The question about `app` needs no answer any more: it was
        /// answered (on the desktop or in Settings), or the settings changed
        /// so that it does not matter.
        2 => Settled { app: String },
    }
}

/// Event ordinal of [`FirewallNotice`]s on a firewall watch channel.
pub const FIREWALL_NOTICE: u32 = 1;

enumeration! {
    /// Why the firewall turned something away.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    pub enum FirewallReason {
        /// A connection or a datagram from another machine that no rule lets
        /// in.
        NotAllowed = 1,
        /// A connection or a datagram for a port nothing uses, unanswered in
        /// stealth mode: most likely a scan.
        Probe = 2,
        /// A ping that gets no answer.
        Ping = 3,
        /// A packet no host sends: a forged source address (this machine's,
        /// loopback, multicast), impossible TCP flags, a protocol or a message
        /// the machine does not take.
        Invalid = 4,
        /// More new connections or pings from one address, or from all of
        /// them, than the firewall lets through.
        RateLimited = 5,
        /// A piece of a fragmented packet whose first piece was not let in,
        /// or one that could hide where the packet goes.
        Fragment = 6,
        /// An application kept off the network reached for it, or what came
        /// in was for one.
        AppBlocked = 7,
    }
}

message! {
    /// What the firewall turned away; repeats within a second of each other
    /// count into one.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct FirewallEvent {
        /// When it last happened (Unix time, seconds).
        pub at: u64,
        pub reason: FirewallReason,
        /// The interface it came in on ("" for what an application sent).
        pub interface: String,
        /// The IP protocol (6 TCP, 17 UDP, 1 ICMP, 58 ICMPv6).
        pub protocol: u8,
        /// The other machine (port 0 for a protocol without ports).
        pub remote: SocketAddr,
        /// This machine's port (0 if none).
        pub port: u16,
        /// The application concerned ("" if none).
        pub app: String,
        /// How many times.
        pub count: u32,
    }
}

message! {
    /// The firewall's counts since the system started.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct FirewallStats {
        /// Connections and datagrams from other machines that rules let in.
        pub allowed: u64,
        pub not_allowed: u64,
        pub probes: u64,
        pub pings: u64,
        pub invalid: u64,
        pub rate_limited: u64,
        pub fragments: u64,
        pub app_blocked: u64,
    }
}

/// Formats a hardware (MAC) address: `02:00:00:57:4c:01`.
pub fn format_mac(m: &[u8; 6]) -> String {
    alloc::format!("{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}", m[0], m[1], m[2], m[3], m[4], m[5])
}

protocol! {
    /// The network service.
    pub mod net = "net" {
        /// Connects to `remote`. `socket` is the service's end of the new
        /// socket channel; the result arrives on it ([`SocketEvent::Connected`]
        /// or [`SocketEvent::Error`]). An immediate error means the request
        /// itself was rejected.
        1 => fn tcp_connect(remote: SocketAddr, options: TcpOptions, socket: Channel) -> Result<(), NetError>;
        /// Listens on `local` (port 0 picks a free port; IPv6's addresses
        /// alone on `::` if `v6only`). Connections arrive as
        /// [`SocketEvent::Accepted`] on `listener`. Returns the address
        /// actually bound.
        2 => fn tcp_listen(local: SocketAddr, v6only: bool, backlog: u32, options: TcpOptions, listener: Channel) -> Result<SocketAddr, NetError>;
        /// Opens a UDP socket bound to `local` (port 0 picks a free port;
        /// IPv6's addresses alone on `::` if `v6only`). The initial send
        /// credit arrives as a [`SocketEvent::SendCredit`].
        3 => fn udp_bind(local: SocketAddr, v6only: bool, socket: Channel) -> Result<SocketAddr, NetError>;
        /// Like [`tcp_connect`](net::Client::tcp_connect), but the
        /// connection's bytes go on `bytes`, the service's end of a kernel
        /// socket, and `socket` carries the rest, without credit (see the
        /// module's documentation).
        6 => fn tcp_connect_bytes(remote: SocketAddr, options: TcpOptions, socket: Channel, bytes: Socket) -> Result<(), NetError>;
        /// Like [`tcp_listen`](net::Client::tcp_listen), its connections
        /// arriving as [`SocketEvent::AcceptedBytes`].
        7 => fn tcp_listen_bytes(local: SocketAddr, v6only: bool, backlog: u32, options: TcpOptions, listener: Channel) -> Result<SocketAddr, NetError>;
        /// Like [`udp_bind`](net::Client::udp_bind), without credit: the
        /// channel's room is the flow control (see the module's
        /// documentation).
        8 => fn udp_bind_unmetered(local: SocketAddr, v6only: bool, socket: Channel) -> Result<SocketAddr, NetError>;
        /// Binds a TCP socket to `local` (port 0 picks a free port; IPv6's
        /// addresses alone on `::` if `v6only`) before it listens or
        /// connects, as POSIX's `bind` does: no other socket binds or
        /// listens on the port at its addresses while it has it. `socket` is
        /// the service's end of its channel, on which
        /// [`SocketRequest::Listen`] makes it a listener,
        /// [`SocketRequest::Connect`] a connection from the address, both of
        /// the kind whose bytes go on kernel sockets (see the module's
        /// documentation). Returns the address bound.
        9 => fn tcp_bind(local: SocketAddr, v6only: bool, socket: Channel) -> Result<SocketAddr, NetError>;
        /// Opens a ping socket (ICMP echo).
        4 => fn ping_open(socket: Channel) -> Result<(), NetError>;
        /// Looks up `name` (a host name, or an address literal). One
        /// [`ResolveResult`] arrives on `reply` as an event with ordinal
        /// [`RESOLVE_RESULT`].
        5 => fn resolve(name: String, family: AddrFamily, reply: Channel) -> Result<(), NetError>;

        /// The overall state.
        10 => fn status() -> NetStatus;
        /// The interfaces (their hardware addresses are zeros for clients
        /// other than Settings and the Terminal).
        11 => fn interfaces() -> Vec<InterfaceInfo>;
        12 => fn routes() -> Vec<RouteInfo>;
        /// The caller's sockets (every socket for Settings and the
        /// Terminal).
        13 => fn sockets() -> Vec<SocketInfo>;
        /// Sends [`NetEvent`]s (ordinal [`NET_EVENT`]) on `events` until it
        /// is closed.
        14 => fn watch(events: Channel) -> Result<(), NetError>;

        /// Sets how `interface` obtains its IPv4 configuration (Settings
        /// only).
        20 => fn configure(interface: String, config: IpConfig) -> Result<(), NetError>;
        /// Restarts DHCP on `interface` (asks for a new lease; Settings only).
        21 => fn renew(interface: String) -> Result<(), NetError>;
        /// Overrides the DNS servers (empty: use the ones DHCP provides;
        /// Settings only).
        22 => fn set_dns_servers(servers: Vec<IpAddr>) -> Result<(), NetError>;
        /// Empties the DNS cache (Settings and the Terminal only).
        23 => fn flush_dns_cache() -> Result<(), NetError>;

        /// The firewall's settings (Settings and the Terminal only).
        30 => fn firewall() -> Result<FirewallConfig, NetError>;
        /// Replaces the firewall's settings, which `network` keeps from
        /// then on (Settings only; `InvalidArgument` for what
        /// [`FirewallConfig::check`] refuses).
        31 => fn set_firewall(config: FirewallConfig) -> Result<(), NetError>;
        /// What the firewall turned away lately, oldest first (Settings and
        /// the Terminal only).
        32 => fn firewall_log() -> Result<Vec<FirewallEvent>, NetError>;
        /// The firewall's counts (Settings and the Terminal only).
        33 => fn firewall_stats() -> Result<FirewallStats, NetError>;
        /// The questions waiting for the user's answer (Settings and the
        /// Terminal only).
        34 => fn firewall_questions() -> Result<Vec<FirewallQuestion>, NetError>;
        /// Sends [`FirewallNotice`]s (ordinal [`FIREWALL_NOTICE`]) on
        /// `events`, those of the questions waiting first, until it is
        /// closed or another takes its place (the desktop shell only, which
        /// asks the user).
        35 => fn watch_firewall(events: Channel) -> Result<(), NetError>;
        /// The user's answer to the question about `app`: where it may
        /// accept connections from (`None`: nowhere). It becomes the
        /// application's rule, kept as Settings' changes are (the desktop
        /// shell only; `InvalidArgument` if no question about `app` waits).
        36 => fn answer_firewall(app: String, from: Option<Scope>) -> Result<(), NetError>;
    }
}

/// Event ordinal of the [`ResolveResult`] on a resolve reply channel.
pub const RESOLVE_RESULT: u32 = 1;

enumeration! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum NetdevError {
        /// The device description is invalid (bad MAC, MTU, ...).
        Invalid = 1,
        /// The frame link could not be attached.
        BadLink = 2,
        /// Too many interfaces.
        LimitReached = 3,
    }
}

message! {
    /// A network device offered to the stack.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct DeviceInfo {
        pub kind: InterfaceKind,
        pub mac: [u8; 6],
        /// Largest IP packet (1500 for Ethernet).
        pub mtu: u32,
        /// Driver or service name, e.g. `virtio_net (Linux)`.
        pub driver: String,
        /// Where the device is, e.g. `pci 00:03.0`.
        pub location: String,
    }
}

protocol! {
    /// Network devices attaching to the stack. One device per channel;
    /// closing the channel removes the interface.
    pub mod netdev = "netdev" {
        /// Attaches a device whose frames flow through `link` (slots of kind
        /// [`crate::netring::kind::ETHERNET`]). Returns the interface name.
        1 => fn attach(info: DeviceInfo, link: LinkEndpoints, link_up: bool) -> Result<String, NetdevError>;
        /// The link went up or down (cable, Wi-Fi association).
        2 => fn set_link(up: bool) -> ();
    }
}

/// Why a device could not attach to the stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachError {
    /// The network service is not running (or went away).
    Unavailable,
    NoMemory,
    /// The stack refused the device.
    Rejected(NetdevError),
}

impl core::fmt::Display for AttachError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            AttachError::Unavailable => f.write_str("the network service is not available"),
            AttachError::NoMemory => f.write_str("out of memory"),
            AttachError::Rejected(e) => write!(f, "the network service refused the device ({e:?})"),
        }
    }
}

/// A network device's connection to the stack, for drivers and the Wi-Fi
/// service: the `netdev` channel plus the frame link.
pub struct DeviceAttachment {
    client: netdev::Client,
    /// The frame link (slots of kind [`crate::netring::kind::ETHERNET`]).
    pub link: crate::netring::Link,
    /// The interface name the stack assigned (`eth0`, `wlan0`, ...).
    pub name: String,
}

impl DeviceAttachment {
    /// Connects to the stack and attaches a device with a link of `slots`
    /// slots of `slot_size` bytes per direction. Blocks until the network
    /// service answers (it may not have started yet).
    pub fn attach(
        info: DeviceInfo,
        slots: u32,
        slot_size: u32,
        link_up: bool,
    ) -> Result<DeviceAttachment, AttachError> {
        Self::attach_timeout(info, slots, slot_size, link_up, 0)
    }

    /// Like [`DeviceAttachment::attach`], but each call to the stack gives
    /// up after `timeout_ns` nanoseconds (0: wait forever), for services
    /// that must stay responsive while the network service restarts.
    pub fn attach_timeout(
        info: DeviceInfo,
        slots: u32,
        slot_size: u32,
        link_up: bool,
        timeout_ns: u64,
    ) -> Result<DeviceAttachment, AttachError> {
        let channel = crate::connect(netdev::NAME).map_err(|_| AttachError::Unavailable)?;
        let client = netdev::Client::new(channel);
        client.set_timeout(timeout_ns);
        let (link, ends) = crate::netring::Link::create(slots, slot_size).map_err(|_| AttachError::NoMemory)?;
        match client.attach(info, ends, link_up) {
            Ok(Ok(name)) => Ok(DeviceAttachment { client, link, name }),
            Ok(Err(e)) => Err(AttachError::Rejected(e)),
            Err(_) => Err(AttachError::Unavailable),
        }
    }

    /// Reports a link change. Returns `false` if the stack is gone.
    pub fn set_link(&self, up: bool) -> bool {
        self.client.set_link(up).is_ok()
    }

    /// The `netdev` channel: `PEER_CLOSED` on it means the network service
    /// went away, and the device should attach again.
    pub fn channel(&self) -> &Channel {
        self.client.channel()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use alloc::vec;
    use ipc::{Decode, Decoder, Encode, Encoder};

    fn roundtrip<T: Encode + Decode>(v: T) -> T {
        let mut e = Encoder::new();
        v.encode(&mut e);
        let mut d = Decoder::new(&e.bytes, Vec::new());
        let out = T::decode(&mut d).unwrap();
        d.finish().unwrap();
        out
    }

    #[test]
    fn socket_messages_round_trip() {
        let dest: SocketAddr = "10.0.2.3:53".parse().unwrap();
        let r = roundtrip(SocketRequest::SendTo { dest, data: Bytes(vec![1, 2, 3]) });
        assert!(matches!(&r, SocketRequest::SendTo { dest: to, data } if *to == dest && data.0 == [1, 2, 3]), "{r:?}");
        let e = SocketEvent::EchoReply { from: "1.1.1.1".parse().unwrap(), seq: 7, size: 56, rtt_us: 12_345 };
        let mut enc = Encoder::new();
        e.encode(&mut enc);
        let mut d = Decoder::new(&enc.bytes, Vec::new());
        assert!(matches!(
            SocketEvent::decode(&mut d).unwrap(),
            SocketEvent::EchoReply { seq: 7, size: 56, rtt_us: 12_345, .. }
        ));
        let f = ResolveResult::Found {
            addresses: vec!["2001:db8::1".parse().unwrap(), "192.0.2.1".parse().unwrap()],
            ttl_s: 300,
            canonical: "example.com".into(),
        };
        assert_eq!(roundtrip(f.clone()), f);
    }

    fn rules() -> FirewallConfig {
        FirewallConfig {
            enabled: true,
            block_incoming: false,
            stealth: false,
            ping: Some(Scope::LocalNetwork {}),
            log: true,
            incoming: vec![
                AppRule { app: "music".to_string(), from: Some(Scope::LocalNetwork {}) },
                AppRule {
                    app: "terminal".to_string(),
                    from: Some(Scope::Prefix { address: "2001:db8::".parse().unwrap(), len: 32 }),
                },
                AppRule { app: "photos".to_string(), from: None },
            ],
            blocked: vec!["videos".to_string()],
            ports: vec![PortRule {
                protocol: FirewallProtocol::Tcp,
                first: 8000,
                last: 8080,
                from: Scope::Anywhere {},
                allow: true,
            }],
        }
    }

    #[test]
    fn firewall_settings_round_trip_and_are_checked() {
        let c = rules();
        assert_eq!(roundtrip(c.clone()), c);
        assert_eq!(c.check(), Ok(()));
        let e = FirewallEvent {
            at: 1_790_000_000,
            reason: FirewallReason::Probe,
            interface: "eth0".to_string(),
            protocol: 6,
            remote: "198.51.100.7:40000".parse().unwrap(),
            port: 22,
            app: String::new(),
            count: 3,
        };
        assert_eq!(roundtrip(e.clone()), e);
        let bad = |f: &dyn Fn(&mut FirewallConfig)| {
            let mut c = rules();
            f(&mut c);
            c.check()
        };
        assert_eq!(bad(&|c| c.blocked.push(String::new())), Err(NetError::InvalidArgument));
        assert_eq!(bad(&|c| c.blocked.push("a\nb".to_string())), Err(NetError::InvalidArgument));
        assert_eq!(bad(&|c| c.blocked.push("x".repeat(MAX_APP_NAME + 1))), Err(NetError::InvalidArgument));
        assert_eq!(bad(&|c| c.ports[0].first = 0), Err(NetError::InvalidArgument));
        assert_eq!(bad(&|c| c.ports[0].last = 7999), Err(NetError::InvalidArgument));
        assert_eq!(
            bad(&|c| c.ping = Some(Scope::Prefix { address: "10.0.0.0".parse().unwrap(), len: 33 })),
            Err(NetError::InvalidArgument)
        );
        assert_eq!(bad(&|c| c.blocked = vec!["x".to_string(); MAX_FIREWALL_RULES + 1]), Err(NetError::InvalidArgument));
        assert_eq!(
            bad(&|c| c.incoming[1].from = Some(Scope::Prefix { address: "2001:db8::".parse().unwrap(), len: 129 })),
            Err(NetError::InvalidArgument)
        );
        let q = FirewallQuestion { app: "terminal".to_string(), protocol: FirewallProtocol::Udp, port: 5353, at: 7 };
        assert_eq!(roundtrip(q.clone()), q);
        let asked = FirewallNotice::Asked { question: q };
        assert_eq!(roundtrip(asked.clone()), asked);
        let settled = FirewallNotice::Settled { app: "terminal".to_string() };
        assert_eq!(roundtrip(settled.clone()), settled);
    }

    #[test]
    fn the_default_firewall_lets_nothing_in_and_answers_nothing() {
        let d = FirewallConfig::default();
        assert!(d.enabled && d.stealth && d.log && !d.block_incoming);
        assert_eq!(d.ping, None);
        assert!(d.incoming.is_empty() && d.ports.is_empty() && d.blocked.is_empty());
        assert_eq!(d.incoming_for("music"), None);
        assert!(d.allows_network("music"));
    }

    #[test]
    fn applications_are_let_in_and_kept_out_by_name() {
        let mut c = rules();
        assert_eq!(c.incoming_for("music"), Some(Scope::LocalNetwork {}));
        assert_eq!(c.incoming_for("editor"), None);
        // Not let in, as the user answered.
        assert_eq!(c.incoming_for("photos"), None);
        assert!(!c.allows_network("videos") && c.allows_network("music"));
        // Kept off the network: nothing comes in for it either.
        c.blocked.push("music".to_string());
        assert_eq!(c.incoming_for("music"), None);
        // The firewall off keeps no one off.
        c.enabled = false;
        assert!(c.allows_network("videos"));
        let r = &c.ports[0];
        assert!(r.covers(FirewallProtocol::Tcp, 8000) && r.covers(FirewallProtocol::Tcp, 8080));
        assert!(!r.covers(FirewallProtocol::Tcp, 8081) && !r.covers(FirewallProtocol::Udp, 8000));
    }

    #[test]
    fn the_user_is_asked_only_about_applications_the_settings_say_nothing_of() {
        let mut c = rules();
        // A rule either way, or kept off the network: decided.
        for app in ["music", "terminal", "photos", "videos"] {
            assert!(!c.undecided(app), "{app}");
        }
        assert!(c.undecided("editor"));
        // Nothing comes in whatever the answer: no question.
        c.block_incoming = true;
        assert!(!c.undecided("editor"));
        c.block_incoming = false;
        c.enabled = false;
        assert!(!c.undecided("editor"));
    }

    #[test]
    fn scopes_read_and_print() {
        assert_eq!(Scope::parse("any"), Some(Scope::Anywhere {}));
        assert_eq!(Scope::parse("local"), Some(Scope::LocalNetwork {}));
        let p = Scope::parse("192.0.2.0/24").unwrap();
        assert_eq!(p, Scope::Prefix { address: "192.0.2.0".parse().unwrap(), len: 24 });
        assert_eq!(p.to_string(), "192.0.2.0/24");
        assert_eq!(
            Scope::parse("2001:db8::1"),
            Some(Scope::Prefix { address: "2001:db8::1".parse().unwrap(), len: 128 })
        );
        assert_eq!(Scope::parse("10.0.0.0/33"), None);
        assert_eq!(Scope::parse("somewhere"), None);
        assert_eq!(Scope::LocalNetwork {}.to_string(), "the local network");
    }

    #[test]
    fn events_encode_straight_from_slices() {
        let from: SocketAddr = "[2001:db8::1]:5353".parse().unwrap();
        let payload = vec![7u8; MAX_DATAGRAM];
        let mut e = Encoder::with_header(0, 0, ipc::FLAG_EVENT);
        SocketEvent::encode_datagram(from, &payload, &mut e);
        // The largest datagram, from the longest address, fits a message.
        assert!(e.bytes.len() <= abi::CHANNEL_MAX_BYTES);
        let mut d = Decoder::new(&e.bytes[ipc::HEADER_LEN..], Vec::new());
        match SocketEvent::decode(&mut d).unwrap() {
            SocketEvent::Datagram { from: f, data } => assert_eq!((f, data.0), (from, payload.clone())),
            other => panic!("{other:?}"),
        }
        let mut e = Encoder::new();
        SocketEvent::encode_data(b"hello", &mut e);
        let mut d = Decoder::new(&e.bytes, Vec::new());
        assert!(matches!(SocketEvent::decode(&mut d).unwrap(), SocketEvent::Data { data } if data.0 == b"hello"));
        // And a request to send the largest datagram fits too.
        let mut e = Encoder::with_header(0, 0, ipc::FLAG_EVENT);
        SocketRequest::SendTo { dest: from, data: Bytes(payload.clone()) }.encode(&mut e);
        assert!(e.bytes.len() <= abi::CHANNEL_MAX_BYTES);
        // Requests encoded from slices are the requests themselves.
        let mut e = Encoder::new();
        SocketRequest::encode_send_to(from, &payload, &mut e);
        let mut d = Decoder::new(&e.bytes, Vec::new());
        let r = SocketRequest::decode(&mut d).unwrap();
        assert!(matches!(&r, SocketRequest::SendTo { dest, data } if *dest == from && data.0 == payload), "{r:?}");
        let mut e = Encoder::new();
        SocketRequest::encode_send(b"hello", &mut e);
        let mut d = Decoder::new(&e.bytes, Vec::new());
        let r = SocketRequest::decode(&mut d).unwrap();
        assert!(matches!(&r, SocketRequest::Send { data } if data.0 == b"hello"), "{r:?}");
    }

    #[test]
    fn macs_are_formatted() {
        assert_eq!(format_mac(&[2, 0, 0, 0x57, 0x4c, 1]), "02:00:00:57:4c:01");
    }

    #[test]
    fn errors_have_descriptions() {
        assert_eq!(NetError::ConnectionRefused.to_string(), "connection refused");
        assert_eq!(roundtrip(NetError::TimeExceeded), NetError::TimeExceeded);
    }

    #[test]
    fn interface_info_round_trips() {
        let info = InterfaceInfo {
            name: "wlan0".into(),
            kind: InterfaceKind::Wireless,
            mac: [2, 0, 0, 0x57, 0x4c, 1],
            mtu: 1500,
            link_up: true,
            driver: "wlan".into(),
            config: IpConfig::Dhcp {},
            addresses: vec![AddressInfo {
                address: "10.0.2.15".parse().unwrap(),
                prefix_len: 24,
                origin: AddressOrigin::Dhcp,
                valid_s: 86400,
            }],
            gateways: vec!["10.0.2.2".parse().unwrap()],
            dns: vec!["10.0.2.3".parse().unwrap()],
            dhcp: DhcpInfo {
                state: DhcpState::Bound,
                server: Some("10.0.2.2".parse().unwrap()),
                lease_s: 86400,
                age_s: 5,
            },
            metric: 600,
            stats: InterfaceStats::default(),
        };
        assert_eq!(roundtrip(info.clone()), info);
    }
}
