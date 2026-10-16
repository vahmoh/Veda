// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The firewall: what other machines may start with this one, decided
//! before smoltcp sees a packet.
//!
//! Every frame an interface other than loopback receives passes
//! [`Firewall::inbound`] first, and every frame it sends passes
//! [`Firewall::outbound`]; what programs of the machine send each other,
//! on the loopback interface, passes neither. The settings are
//! `vproto::net::FirewallConfig`'s, which `netd` gives the stack.
//!
//! * **What comes in.** A new connection (a TCP segment that only
//!   synchronises) or a datagram for a port is let in when a port rule opens
//!   the port to where it comes from, or the socket on the port is open to
//!   it (by its application's rule, which `netd` sets with
//!   [`Firewall::set_exposure`]). A port rule that closes the port to the
//!   source wins over both, and blocking everything incoming sets every
//!   rule that lets something in aside. Replies always come in: the
//!   segments of a connection (smoltcp takes them if it has the
//!   connection), and datagrams from where this machine sent one in the
//!   last three minutes; so do DHCP's answers, the ICMP errors that tell
//!   TCP and ping what became of their packets, and IPv6's neighbour
//!   discovery (from the link itself: a hop limit of 255).
//! * **Stealth.** smoltcp answers what comes for a port nothing uses (a
//!   reset, "port unreachable"), which shows the machine is there: in
//!   stealth mode it is dropped instead, and [`Firewall::outbound`] drops
//!   such answers too, a reset for a connection this machine sent nothing
//!   on (it remembers the connections it did), "port unreachable" and
//!   "protocol unreachable". A ping is answered only from where the
//!   settings say.
//! * **What no host sends** is dropped whether the firewall is on or not:
//!   a source address that is this machine's own, a loopback, multicast or
//!   broadcast one (IPv6's unspecified address only for neighbour
//!   discovery and multicast listener reports), a loopback destination,
//!   impossible TCP flags (a SYN with a FIN or a reset, a segment with
//!   neither SYN, ACK nor reset), port 0, IPv6's type 0 routing header,
//!   ICMP redirects and the like.
//! * **Fragments.** smoltcp reassembles IPv4 datagrams, whose ports are in
//!   the first piece only: the first piece is judged as the whole datagram
//!   would be, and must hold its whole transport header; the others come in
//!   for a minute after a first piece of their datagram did, unless they
//!   would rewrite its TCP header (RFC 1858). IPv6 fragments, which smoltcp
//!   does not reassemble, are dropped.
//! * **Rates.** The new connections let in from one address, and the pings
//!   answered, are limited (a burst, then so many a second), and so are they
//!   from all addresses together, so that a flood takes no more than a share
//!   of the machine.
//! * **The log** keeps the last [`LOG_LEN`] things turned away, repeats
//!   within a second of each other counted into one (a scan from one
//!   address, into one entry), and counts everything ([`FirewallStats`]).

use alloc::borrow::ToOwned;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use core::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use smoltcp::phy::Medium;
use smoltcp::wire::{
    Icmpv4Message, Icmpv4Packet, Icmpv6Message, Icmpv6Packet, IpAddress, IpCidr, IpProtocol, Ipv4Packet, Ipv6Packet,
    TcpPacket, UdpPacket,
};
use vproto::net::{FirewallConfig, FirewallEvent, FirewallProtocol, FirewallReason, FirewallStats, Scope};

use crate::{SockId, Stack};

/// The things turned away that the log keeps.
pub const LOG_LEN: usize = 256;
/// The conversations remembered of each protocol, at most.
const MAX_FLOWS: usize = 4096;
/// A UDP conversation is forgotten this long after its last datagram.
const UDP_IDLE_US: u64 = 180_000_000;
/// A TCP connection, this long after the last segment this machine sent on
/// it (as long as Linux keeps an established connection's state).
const TCP_IDLE_US: u64 = 5 * 24 * 3600 * 1_000_000;
/// The first piece of a fragmented datagram lets the others in for this
/// long (as long as smoltcp waits for them).
const FRAGMENT_US: u64 = 60_000_000;
/// Fragmented datagrams being let in at once, at most.
const MAX_FRAGMENTED: usize = 256;
/// The addresses whose rates are kept, at most.
const MAX_SOURCES: usize = 1024;
/// Repeats this close together count into one entry of the log.
const FOLD_US: u64 = 1_000_000;
/// What the log looks back over for an entry to count a repeat into.
const FOLD_BACK: usize = 16;
/// The ethertypes of what smoltcp takes.
const ETHERTYPE_IPV4: u16 = 0x0800;
const ETHERTYPE_ARP: u16 = 0x0806;
const ETHERTYPE_IPV6: u16 = 0x86dd;
const ETHERNET_HEADER: usize = 14;

/// A rate: a burst, then so many a second.
#[derive(Debug, Clone, Copy)]
struct Rate {
    burst: u64,
    per_second: u64,
}

/// New connections let in from one address, and from all together.
const CONNECTIONS_FROM_ONE: Rate = Rate { burst: 20, per_second: 10 };
const CONNECTIONS_FROM_ALL: Rate = Rate { burst: 200, per_second: 100 };
/// Pings answered, to one address and to all together.
const PINGS_FROM_ONE: Rate = Rate { burst: 5, per_second: 1 };
const PINGS_FROM_ALL: Rate = Rate { burst: 50, per_second: 20 };

/// The tokens of a rate, in thousandths.
#[derive(Debug, Clone, Copy)]
struct Bucket {
    milli: u64,
    at: u64,
}

impl Bucket {
    fn full(rate: Rate, now: u64) -> Bucket {
        Bucket { milli: rate.burst * 1000, at: now }
    }

    /// Takes a token, if the rate has one now.
    fn take(&mut self, rate: Rate, now: u64) -> bool {
        let earned = now.saturating_sub(self.at).saturating_mul(rate.per_second) / 1000;
        self.milli = self.milli.saturating_add(earned).min(rate.burst * 1000);
        self.at = now;
        if self.milli < 1000 {
            return false;
        }
        self.milli -= 1000;
        true
    }
}

/// What a rate limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Limit {
    Connections = 0,
    Pings = 1,
}

/// A conversation: this machine's port, and the other end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Flow {
    port: u16,
    remote: IpAddr,
    remote_port: u16,
}

/// A socket that takes what other machines start (a TCP listener, a UDP
/// socket), and where from.
#[derive(Debug, Clone)]
struct Receiver {
    sock: SockId,
    from: Option<Scope>,
    app: String,
}

/// What the firewall needs of the interface a frame comes in on.
pub(crate) struct Place<'a> {
    pub name: &'a str,
    pub addrs: &'a [IpCidr],
}

impl Place<'_> {
    /// Whether `ip` is one of the interface's addresses.
    fn owns(&self, ip: IpAddr) -> bool {
        self.addrs.iter().any(|c| IpAddr::from(c.address()) == ip)
    }

    /// Whether `ip` is within `scope`, seen from this interface.
    fn within(&self, scope: &Scope, ip: IpAddr) -> bool {
        match *scope {
            Scope::Anywhere {} => true,
            Scope::LocalNetwork {} => {
                link_local(ip) || self.addrs.iter().any(|c| c.contains_addr(&IpAddress::from(ip)))
            }
            Scope::Prefix { address, len } => in_prefix(address, len, ip),
        }
    }
}

/// Whether `ip` is a link-local address (169.254.0.0/16, fe80::/10).
fn link_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(a) => a.is_link_local(),
        IpAddr::V6(a) => a.segments()[0] & 0xffc0 == 0xfe80,
    }
}

/// Whether the first `len` bits of `ip` are `prefix`'s.
fn in_prefix(prefix: IpAddr, len: u8, ip: IpAddr) -> bool {
    match (prefix, ip) {
        (IpAddr::V4(p), IpAddr::V4(a)) => {
            let mask = u32::MAX.checked_shl(32 - u32::from(len.min(32))).unwrap_or(0);
            u32::from(p) & mask == u32::from(a) & mask
        }
        (IpAddr::V6(p), IpAddr::V6(a)) => {
            let mask = u128::MAX.checked_shl(128 - u32::from(len.min(128))).unwrap_or(0);
            u128::from(p) & mask == u128::from(a) & mask
        }
        _ => false,
    }
}

/// Whether no host sends from `src`.
fn forged_v4(src: Ipv4Addr) -> bool {
    src.is_loopback() || src.is_unspecified() || src.octets()[0] >= 224
}

/// Whether no host sends from `src` (the unspecified address aside).
fn forged_v6(src: Ipv6Addr) -> bool {
    src.is_loopback() || src.is_multicast() || src.to_ipv4_mapped().is_some()
}

/// What becomes of a new connection or datagram.
enum Admit {
    /// A rule lets it in.
    Yes,
    /// It is for a socket, or a port, closed to where it comes from.
    No,
    /// It is for a socket of an application kept off the network.
    Blocked,
    /// Nothing uses the port.
    Closed,
}

/// An entry of the log, when it last happened by the stack's clock.
#[derive(Debug, Clone)]
pub struct LogEntry {
    pub at_us: u64,
    pub event: FirewallEvent,
}

/// The firewall's settings and what it keeps track of.
pub struct Firewall {
    config: FirewallConfig,
    /// The sockets that take what other machines start, by protocol and
    /// port (a port has one listener, or one UDP socket).
    receivers: BTreeMap<(FirewallProtocol, u16), Receiver>,
    ports_of: BTreeMap<SockId, (FirewallProtocol, u16)>,
    /// When the last datagram of each UDP conversation went either way.
    udp: BTreeMap<Flow, u64>,
    /// When this machine last sent a segment on each TCP connection.
    tcp: BTreeMap<Flow, u64>,
    /// Fragmented datagrams let in (source, destination, protocol and
    /// identification), until when their pieces may come.
    fragments: BTreeMap<(IpAddr, IpAddr, u8, u16), u64>,
    sources: BTreeMap<(Limit, IpAddr), Bucket>,
    all: [Option<Bucket>; 2],
    log: VecDeque<LogEntry>,
    stats: FirewallStats,
    swept_at: u64,
}

impl Default for Firewall {
    fn default() -> Firewall {
        Firewall::new()
    }
}

impl Firewall {
    /// A firewall that is off (until [`Firewall::set_config`]).
    pub fn new() -> Firewall {
        Firewall {
            config: FirewallConfig { enabled: false, ..FirewallConfig::default() },
            receivers: BTreeMap::new(),
            ports_of: BTreeMap::new(),
            udp: BTreeMap::new(),
            tcp: BTreeMap::new(),
            fragments: BTreeMap::new(),
            sources: BTreeMap::new(),
            all: [None, None],
            log: VecDeque::new(),
            stats: FirewallStats::default(),
            swept_at: 0,
        }
    }

    pub fn config(&self) -> &FirewallConfig {
        &self.config
    }

    /// Applies new settings (which the caller has checked).
    pub fn set_config(&mut self, config: FirewallConfig) {
        self.config = config;
    }

    /// Socket `sock` takes what other machines start on `port`.
    pub(crate) fn add_receiver(&mut self, sock: SockId, protocol: FirewallProtocol, port: u16) {
        self.receivers.insert((protocol, port), Receiver { sock, from: None, app: String::new() });
        self.ports_of.insert(sock, (protocol, port));
    }

    /// Socket `sock` is gone.
    pub(crate) fn remove_receiver(&mut self, sock: SockId) {
        if let Some(key) = self.ports_of.remove(&sock)
            && self.receivers.get(&key).is_some_and(|r| r.sock == sock)
        {
            self.receivers.remove(&key);
        }
    }

    /// Where socket `sock`, of application `app`, takes what other machines
    /// start from (`None`: nowhere, as a socket is until this says).
    pub fn set_exposure(&mut self, sock: SockId, from: Option<Scope>, app: &str) {
        if let Some(r) = self.ports_of.get(&sock).and_then(|key| self.receivers.get_mut(key)) {
            r.from = from;
            app.clone_into(&mut r.app);
        }
    }

    /// What was turned away lately, oldest first.
    pub fn log(&self) -> impl Iterator<Item = &LogEntry> {
        self.log.iter()
    }

    pub fn stats(&self) -> FirewallStats {
        self.stats
    }

    /// An application kept off the network reached for `remote` with
    /// `protocol`.
    pub fn note_app_blocked(&mut self, protocol: u8, remote: SocketAddr, app: &str, now: u64) {
        self.record(FirewallReason::AppBlocked, "", protocol, remote, 0, app, now);
    }

    /// Forgets the conversations, fragments and rates that have run out
    /// (at most once a second).
    pub(crate) fn sweep(&mut self, now: u64) {
        if now.saturating_sub(self.swept_at) < 1_000_000 {
            return;
        }
        self.swept_at = now;
        self.udp.retain(|_, seen| now.saturating_sub(*seen) < UDP_IDLE_US);
        self.tcp.retain(|_, seen| now.saturating_sub(*seen) < TCP_IDLE_US);
        self.fragments.retain(|_, until| now < *until);
        // A bucket left alone a minute is full again, as a new one is.
        self.sources.retain(|_, b| now.saturating_sub(b.at) < 60_000_000);
    }

    /// Whether a frame interface `place` received goes on to smoltcp.
    pub(crate) fn inbound(&mut self, frame: &[u8], medium: Medium, place: &Place, now: u64) -> bool {
        let ip = match medium {
            Medium::Ethernet => {
                let Some(ethertype) = frame.get(12..14).map(|b| u16::from_be_bytes([b[0], b[1]])) else {
                    return false;
                };
                match ethertype {
                    ETHERTYPE_ARP => return true,
                    ETHERTYPE_IPV4 | ETHERTYPE_IPV6 => &frame[ETHERNET_HEADER..],
                    _ => return false,
                }
            }
            _ => frame,
        };
        match ip.first().map(|b| b >> 4) {
            Some(4) => self.inbound_v4(ip, place, now),
            Some(6) => self.inbound_v6(ip, place, now),
            _ => false,
        }
    }

    fn inbound_v4(&mut self, ip: &[u8], place: &Place, now: u64) -> bool {
        let Ok(p) = Ipv4Packet::new_checked(ip) else { return false };
        let (src, dst) = (p.src_addr(), p.dst_addr());
        let from = IpAddr::V4(src);
        let protocol = u8::from(p.next_header());
        if forged_v4(src) || dst.is_loopback() || place.owns(from) {
            return self.refuse(FirewallReason::Invalid, place, protocol, SocketAddr::new(from, 0), 0, "", now);
        }
        let key = (from, IpAddr::V4(dst), protocol, p.ident());
        if p.frag_offset() != 0 {
            if !self.config.enabled {
                return true;
            }
            // A piece that would rewrite the TCP header (RFC 1858) is
            // turned away with its datagram's others.
            let header_kept = p.next_header() != IpProtocol::Tcp || p.frag_offset() >= 20;
            let known = self.fragments.get(&key).is_some_and(|&until| now < until);
            return (known && header_kept)
                || self.refuse(FirewallReason::Fragment, place, protocol, SocketAddr::new(from, 0), 0, "", now);
        }
        let first_piece = p.more_frags();
        let pass = self.transport(p.next_header(), p.payload(), from, place, now, first_piece);
        if pass && first_piece && self.config.enabled {
            if self.fragments.len() >= MAX_FRAGMENTED {
                self.fragments.pop_first();
            }
            self.fragments.insert(key, now + FRAGMENT_US);
        }
        pass
    }

    fn inbound_v6(&mut self, ip: &[u8], place: &Place, now: u64) -> bool {
        let Ok(p) = Ipv6Packet::new_checked(ip) else { return false };
        let (src, dst) = (p.src_addr(), p.dst_addr());
        let from = IpAddr::V6(src);
        // The extension headers, to the transport's.
        let mut next = p.next_header();
        let mut rest = p.payload();
        let mut headers = 0;
        loop {
            match next {
                IpProtocol::HopByHop | IpProtocol::Ipv6Opts | IpProtocol::Ipv6Route => {
                    headers += 1;
                    let len = rest.get(1).map(|&l| (usize::from(l) + 1) * 8).filter(|&l| l <= rest.len());
                    // The type 0 routing header (RFC 5095) bounced packets
                    // between hosts.
                    let type0 = next == IpProtocol::Ipv6Route && rest.get(2) == Some(&0);
                    let Some(len) = len.filter(|_| headers <= 8 && !type0) else {
                        let protocol = u8::from(next);
                        return self.refuse(
                            FirewallReason::Invalid,
                            place,
                            protocol,
                            SocketAddr::new(from, 0),
                            0,
                            "",
                            now,
                        );
                    };
                    next = IpProtocol::from(rest[0]);
                    rest = &rest[len..];
                }
                IpProtocol::Ipv6Frag => {
                    let protocol = u8::from(next);
                    return self.refuse(
                        FirewallReason::Fragment,
                        place,
                        protocol,
                        SocketAddr::new(from, 0),
                        0,
                        "",
                        now,
                    );
                }
                _ => break,
            }
        }
        let protocol = u8::from(next);
        let invalid = |fw: &mut Firewall| {
            fw.refuse(FirewallReason::Invalid, place, protocol, SocketAddr::new(from, 0), 0, "", now)
        };
        if forged_v6(src) || dst.is_loopback() || (!src.is_unspecified() && place.owns(from)) {
            return invalid(self);
        }
        if next != IpProtocol::Icmpv6 {
            // From no address comes nothing but neighbour discovery.
            if src.is_unspecified() {
                return invalid(self);
            }
            return self.transport(next, rest, from, place, now, false);
        }
        let Ok(m) = Icmpv6Packet::new_checked(rest) else { return invalid(self) };
        match m.msg_type() {
            // Duplicate address detection and listener reports come from no
            // address; nothing else does.
            Icmpv6Message::NeighborSolicit | Icmpv6Message::MldReport if src.is_unspecified() => true,
            _ if src.is_unspecified() => invalid(self),
            Icmpv6Message::EchoRequest => self.ping(protocol, from, place, now),
            Icmpv6Message::EchoReply
            | Icmpv6Message::DstUnreachable
            | Icmpv6Message::PktTooBig
            | Icmpv6Message::TimeExceeded
            | Icmpv6Message::ParamProblem
            | Icmpv6Message::MldQuery
            | Icmpv6Message::MldReport => true,
            // Neighbour discovery comes from the link itself: a hop limit of
            // 255, which no router passes on.
            Icmpv6Message::RouterAdvert | Icmpv6Message::NeighborSolicit | Icmpv6Message::NeighborAdvert => {
                p.hop_limit() == 255 || invalid(self)
            }
            // Router solicitations (this machine routes nothing), redirects.
            _ => invalid(self),
        }
    }

    /// Whether a datagram of `protocol` from `from` goes on to smoltcp
    /// (`piece`: the first piece of a fragmented one).
    fn transport(
        &mut self,
        protocol: IpProtocol,
        payload: &[u8],
        from: IpAddr,
        place: &Place,
        now: u64,
        piece: bool,
    ) -> bool {
        let number = u8::from(protocol);
        let short = if piece { FirewallReason::Fragment } else { FirewallReason::Invalid };
        let on = self.config.enabled;
        match protocol {
            IpProtocol::Tcp => {
                let Ok(t) = TcpPacket::new_checked(payload) else {
                    return self.refuse(short, place, number, SocketAddr::new(from, 0), 0, "", now);
                };
                let (remote, port) = (SocketAddr::new(from, t.src_port()), t.dst_port());
                let (syn, ack, fin, rst) = (t.syn(), t.ack(), t.fin(), t.rst());
                if t.src_port() == 0 || port == 0 || syn && (fin || rst) || fin && rst || !syn && !ack && !rst {
                    return self.refuse(FirewallReason::Invalid, place, number, remote, port, "", now);
                }
                // A segment of a connection: smoltcp takes it if it has one.
                if !syn || ack || !on {
                    return true;
                }
                self.admit_new(FirewallProtocol::Tcp, number, remote, port, place, now)
            }
            IpProtocol::Udp => {
                let Ok(u) = UdpPacket::new_checked(payload) else {
                    return self.refuse(short, place, number, SocketAddr::new(from, 0), 0, "", now);
                };
                let (remote, port) = (SocketAddr::new(from, u.src_port()), u.dst_port());
                if port == 0 {
                    return self.refuse(FirewallReason::Invalid, place, number, remote, port, "", now);
                }
                if !on {
                    return true;
                }
                let flow = Flow { port, remote: from, remote_port: u.src_port() };
                if let Some(seen) = self.udp.get_mut(&flow)
                    && now.saturating_sub(*seen) < UDP_IDLE_US
                {
                    *seen = now;
                    return true;
                }
                // DHCP's answers to this machine's client.
                if from.is_ipv4() && u.src_port() == 67 && port == 68 {
                    return true;
                }
                self.admit_new(FirewallProtocol::Udp, number, remote, port, place, now)
            }
            IpProtocol::Icmp => {
                let Ok(m) = Icmpv4Packet::new_checked(payload) else {
                    return self.refuse(short, place, number, SocketAddr::new(from, 0), 0, "", now);
                };
                match m.msg_type() {
                    Icmpv4Message::EchoRequest => self.ping(number, from, place, now),
                    Icmpv4Message::EchoReply
                    | Icmpv4Message::DstUnreachable
                    | Icmpv4Message::TimeExceeded
                    | Icmpv4Message::ParamProblem => true,
                    // Redirects, router discovery, timestamps.
                    _ => self.refuse(FirewallReason::Invalid, place, number, SocketAddr::new(from, 0), 0, "", now),
                }
            }
            _ => self.refuse(FirewallReason::Invalid, place, number, SocketAddr::new(from, 0), 0, "", now),
        }
    }

    /// Whether a new connection or datagram from `remote` for `port` comes
    /// in.
    fn admit_new(
        &mut self,
        protocol: FirewallProtocol,
        number: u8,
        remote: SocketAddr,
        port: u16,
        place: &Place,
        now: u64,
    ) -> bool {
        match self.admit(protocol, port, remote.ip(), place) {
            Admit::Yes => {
                if protocol == FirewallProtocol::Tcp && !self.limit(Limit::Connections, remote.ip(), now) {
                    let app = self.app_on(protocol, port);
                    return self.refuse(FirewallReason::RateLimited, place, number, remote, port, &app, now);
                }
                self.stats.allowed += 1;
                true
            }
            Admit::No => {
                let app = self.app_on(protocol, port);
                self.refuse(FirewallReason::NotAllowed, place, number, remote, port, &app, now)
            }
            Admit::Blocked => {
                let app = self.app_on(protocol, port);
                self.refuse(FirewallReason::AppBlocked, place, number, remote, port, &app, now)
            }
            Admit::Closed if self.config.stealth => {
                self.refuse(FirewallReason::Probe, place, number, remote, port, "", now)
            }
            // smoltcp says the port is closed.
            Admit::Closed => true,
        }
    }

    /// The rules about a new connection or datagram from `from` for `port`.
    fn admit(&self, protocol: FirewallProtocol, port: u16, from: IpAddr, place: &Place) -> Admit {
        let receiver = self.receivers.get(&(protocol, port));
        let rules = &self.config.ports;
        if rules.iter().any(|r| !r.allow && r.covers(protocol, port) && place.within(&r.from, from)) {
            return Admit::No;
        }
        // Nothing reaches an application kept off the network, whatever
        // opens its port.
        if receiver.is_some_and(|r| !self.config.allows_network(&r.app)) {
            return Admit::Blocked;
        }
        if self.config.block_incoming {
            return if receiver.is_some() { Admit::No } else { Admit::Closed };
        }
        if rules.iter().any(|r| r.allow && r.covers(protocol, port) && place.within(&r.from, from)) {
            return Admit::Yes;
        }
        match receiver {
            Some(r) if r.from.as_ref().is_some_and(|s| place.within(s, from)) => Admit::Yes,
            Some(_) => Admit::No,
            None => Admit::Closed,
        }
    }

    /// The application whose socket is on `port` ("" if none).
    fn app_on(&self, protocol: FirewallProtocol, port: u16) -> String {
        self.receivers.get(&(protocol, port)).map(|r| r.app.clone()).unwrap_or_default()
    }

    /// Whether a ping from `from` is answered.
    fn ping(&mut self, number: u8, from: IpAddr, place: &Place, now: u64) -> bool {
        if !self.config.enabled {
            return true;
        }
        let remote = SocketAddr::new(from, 0);
        if !self.config.ping.as_ref().is_some_and(|s| place.within(s, from)) {
            return self.refuse(FirewallReason::Ping, place, number, remote, 0, "", now);
        }
        if !self.limit(Limit::Pings, from, now) {
            return self.refuse(FirewallReason::RateLimited, place, number, remote, 0, "", now);
        }
        true
    }

    /// Whether the rates of `kind` let one more from `from` through.
    fn limit(&mut self, kind: Limit, from: IpAddr, now: u64) -> bool {
        let (one, all) = match kind {
            Limit::Connections => (CONNECTIONS_FROM_ONE, CONNECTIONS_FROM_ALL),
            Limit::Pings => (PINGS_FROM_ONE, PINGS_FROM_ALL),
        };
        if !self.sources.contains_key(&(kind, from)) && self.sources.len() >= MAX_SOURCES {
            self.sources.pop_first();
        }
        let mine = self.sources.entry((kind, from)).or_insert(Bucket::full(one, now));
        if !mine.take(one, now) {
            return false;
        }
        self.all[kind as usize].get_or_insert(Bucket::full(all, now)).take(all, now)
    }

    /// Counts what was turned away, and logs it: false, for the caller to
    /// return.
    #[allow(clippy::too_many_arguments)]
    fn refuse(
        &mut self,
        reason: FirewallReason,
        place: &Place,
        protocol: u8,
        remote: SocketAddr,
        port: u16,
        app: &str,
        now: u64,
    ) -> bool {
        self.record(reason, place.name, protocol, remote, port, app, now);
        false
    }

    #[allow(clippy::too_many_arguments)]
    fn record(
        &mut self,
        reason: FirewallReason,
        interface: &str,
        protocol: u8,
        remote: SocketAddr,
        port: u16,
        app: &str,
        now: u64,
    ) {
        let s = &mut self.stats;
        match reason {
            FirewallReason::NotAllowed => s.not_allowed += 1,
            FirewallReason::Probe => s.probes += 1,
            FirewallReason::Ping => s.pings += 1,
            FirewallReason::Invalid => s.invalid += 1,
            FirewallReason::RateLimited => s.rate_limited += 1,
            FirewallReason::Fragment => s.fragments += 1,
            FirewallReason::AppBlocked => s.app_blocked += 1,
        }
        if !self.config.log {
            return;
        }
        // A scan's probes, a flood's packets, count into one entry.
        let same = |e: &FirewallEvent| {
            e.reason == reason
                && e.interface == interface
                && e.protocol == protocol
                && e.remote.ip() == remote.ip()
                && (reason == FirewallReason::Probe || e.port == port)
                && e.app == app
        };
        let recent = |e: &&mut LogEntry| now.saturating_sub(e.at_us) < FOLD_US && same(&e.event);
        if let Some(e) = self.log.iter_mut().rev().take(FOLD_BACK).find(recent) {
            e.at_us = now;
            e.event.count = e.event.count.saturating_add(1);
            e.event.port = port;
            return;
        }
        if self.log.len() >= LOG_LEN {
            self.log.pop_front();
        }
        let event = FirewallEvent {
            at: 0,
            reason,
            interface: String::from(interface),
            protocol,
            remote,
            port,
            app: String::from(app),
            count: 1,
        };
        self.log.push_back(LogEntry { at_us: now, event });
    }

    /// Whether a frame an interface sends goes out; what it says of
    /// conversations is remembered either way.
    pub(crate) fn outbound(&mut self, frame: &[u8], medium: Medium, now: u64) -> bool {
        let ip = match medium {
            Medium::Ethernet => match frame.get(12..14).map(|b| u16::from_be_bytes([b[0], b[1]])) {
                Some(ETHERTYPE_IPV4 | ETHERTYPE_IPV6) => &frame[ETHERNET_HEADER..],
                _ => return true,
            },
            _ => frame,
        };
        let (protocol, payload, dst) = match ip.first().map(|b| b >> 4) {
            Some(4) => {
                let Ok(p) = Ipv4Packet::new_checked(ip) else { return true };
                if p.frag_offset() != 0 {
                    return true;
                }
                (p.next_header(), p.payload(), IpAddr::V4(p.dst_addr()))
            }
            Some(6) => {
                let Ok(p) = Ipv6Packet::new_checked(ip) else { return true };
                // smoltcp adds a hop-by-hop header to listener reports alone.
                let (mut next, mut rest) = (p.next_header(), p.payload());
                while next == IpProtocol::HopByHop {
                    let Some(len) = rest.get(1).map(|&l| (usize::from(l) + 1) * 8).filter(|&l| l <= rest.len()) else {
                        return true;
                    };
                    next = IpProtocol::from(rest[0]);
                    rest = &rest[len..];
                }
                (next, rest, IpAddr::V6(p.dst_addr()))
            }
            _ => return true,
        };
        let stealth = self.config.enabled && self.config.stealth;
        match protocol {
            IpProtocol::Tcp => {
                let Ok(t) = TcpPacket::new_checked(payload) else { return true };
                let flow = Flow { port: t.src_port(), remote: dst, remote_port: t.dst_port() };
                if t.rst() {
                    // A reset for a connection this machine sent nothing on
                    // answers a probe.
                    return !stealth || self.tcp.get(&flow).is_some_and(|&seen| now.saturating_sub(seen) < TCP_IDLE_US);
                }
                remember(&mut self.tcp, flow, now);
                true
            }
            IpProtocol::Udp => {
                if let Ok(u) = UdpPacket::new_checked(payload) {
                    remember(&mut self.udp, Flow { port: u.src_port(), remote: dst, remote_port: u.dst_port() }, now);
                }
                true
            }
            // "Protocol unreachable" and "port unreachable".
            IpProtocol::Icmp => {
                !(stealth
                    && Icmpv4Packet::new_checked(payload)
                        .is_ok_and(|m| m.msg_type() == Icmpv4Message::DstUnreachable && matches!(m.msg_code(), 2 | 3)))
            }
            // "Port unreachable", and "unrecognized next header".
            IpProtocol::Icmpv6 => {
                !(stealth
                    && Icmpv6Packet::new_checked(payload).is_ok_and(|m| {
                        m.msg_type() == Icmpv6Message::DstUnreachable && m.msg_code() == 4
                            || m.msg_type() == Icmpv6Message::ParamProblem && m.msg_code() == 1
                    }))
            }
            _ => true,
        }
    }
}

/// Notes a conversation seen now (the one seen longest ago making room, if
/// need be).
fn remember(flows: &mut BTreeMap<Flow, u64>, flow: Flow, now: u64) {
    if let Some(seen) = flows.get_mut(&flow) {
        *seen = now;
        return;
    }
    if flows.len() >= MAX_FLOWS
        && let Some(oldest) = flows.iter().min_by_key(|(_, seen)| **seen).map(|(f, _)| *f)
    {
        flows.remove(&oldest);
    }
    flows.insert(flow, now);
}

impl Stack {
    /// The firewall's settings.
    pub fn firewall_config(&self) -> &FirewallConfig {
        self.firewall.config()
    }

    /// Applies the firewall's settings (which the caller has checked:
    /// `FirewallConfig::check`).
    pub fn set_firewall(&mut self, config: FirewallConfig) {
        self.firewall.set_config(config);
    }

    /// Where socket `id`, a listener or a UDP socket of application `app`,
    /// takes what other machines start from (`None`, as until this says:
    /// nowhere).
    pub fn set_exposure(&mut self, id: SockId, from: Option<Scope>, app: &str) {
        self.firewall.set_exposure(id, from, app);
    }

    /// What the firewall turned away lately, oldest first, each with when
    /// it last happened (the stack's clock).
    pub fn firewall_log(&self) -> impl Iterator<Item = &LogEntry> {
        self.firewall.log()
    }

    /// The firewall's counts.
    pub fn firewall_stats(&self) -> FirewallStats {
        self.firewall.stats()
    }

    /// Application `app`, kept off the network, reached for `remote` with
    /// IP protocol `protocol`.
    pub fn note_app_blocked(&mut self, protocol: u8, remote: SocketAddr, app: &str) {
        let now = self.now_us;
        self.firewall.note_app_blocked(protocol, remote, app, now);
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;
    use alloc::vec;
    use alloc::vec::Vec;

    use smoltcp::phy::ChecksumCapabilities;
    use smoltcp::wire::{Ipv4Cidr, Ipv4Repr, Ipv6Cidr, Ipv6Repr};
    use vproto::net::{AppRule, PortRule};

    use super::*;

    const ME: &str = "192.168.1.10";
    const NEIGHBOUR: &str = "192.168.1.20";
    const STRANGER: &str = "203.0.113.5";

    fn addrs() -> [IpCidr; 2] {
        [
            IpCidr::Ipv4(Ipv4Cidr::new(ME.parse().unwrap(), 24)),
            IpCidr::Ipv6(Ipv6Cidr::new("fe80::10".parse().unwrap(), 64)),
        ]
    }

    fn on(change: impl FnOnce(&mut FirewallConfig)) -> Firewall {
        let mut fw = Firewall::new();
        let mut c = FirewallConfig::default();
        change(&mut c);
        fw.set_config(c);
        fw
    }

    /// An IPv4 packet (bare, as the loopback interface's medium has them;
    /// `frag`: the offset in bytes and whether more pieces follow).
    fn v4(src: &str, dst: &str, protocol: IpProtocol, payload: &[u8], frag: (u16, bool)) -> Vec<u8> {
        let repr = Ipv4Repr {
            src_addr: src.parse().unwrap(),
            dst_addr: dst.parse().unwrap(),
            next_header: protocol,
            payload_len: payload.len(),
            hop_limit: 64,
        };
        let mut buf = vec![0u8; repr.buffer_len() + payload.len()];
        let mut p = Ipv4Packet::new_unchecked(&mut buf);
        repr.emit(&mut p, &ChecksumCapabilities::default());
        p.set_ident(77);
        p.set_frag_offset(frag.0);
        p.set_more_frags(frag.1);
        p.payload_mut().copy_from_slice(payload);
        buf
    }

    fn v6(src: &str, dst: &str, protocol: IpProtocol, hops: u8, payload: &[u8]) -> Vec<u8> {
        let repr = Ipv6Repr {
            src_addr: src.parse().unwrap(),
            dst_addr: dst.parse().unwrap(),
            next_header: protocol,
            payload_len: payload.len(),
            hop_limit: hops,
        };
        let mut buf = vec![0u8; repr.buffer_len() + payload.len()];
        let mut p = Ipv6Packet::new_unchecked(&mut buf);
        repr.emit(&mut p);
        p.payload_mut().copy_from_slice(payload);
        buf
    }

    /// A TCP header: `flags` are FIN 1, SYN 2, RST 4, PSH 8, ACK 16, URG 32.
    fn tcp(src_port: u16, dst_port: u16, flags: u8) -> Vec<u8> {
        let mut h = vec![0u8; 20];
        h[0..2].copy_from_slice(&src_port.to_be_bytes());
        h[2..4].copy_from_slice(&dst_port.to_be_bytes());
        h[12] = 5 << 4;
        h[13] = flags;
        h
    }

    const SYN: u8 = 2;
    const ACK: u8 = 16;

    fn udp(src_port: u16, dst_port: u16) -> Vec<u8> {
        let mut h = vec![0u8; 12];
        h[0..2].copy_from_slice(&src_port.to_be_bytes());
        h[2..4].copy_from_slice(&dst_port.to_be_bytes());
        h[4..6].copy_from_slice(&12u16.to_be_bytes());
        h
    }

    fn icmp(kind: u8, code: u8) -> Vec<u8> {
        vec![kind, code, 0, 0, 0, 1, 0, 1]
    }

    fn syn_from(src: &str, port: u16) -> Vec<u8> {
        v4(src, ME, IpProtocol::Tcp, &tcp(40000, port, SYN), (0, false))
    }

    fn comes_in(fw: &mut Firewall, packet: &[u8], now: u64) -> bool {
        let a = addrs();
        fw.inbound(packet, Medium::Ip, &Place { name: "eth0", addrs: &a }, now)
    }

    fn goes_out(fw: &mut Firewall, packet: &[u8], now: u64) -> bool {
        fw.outbound(packet, Medium::Ip, now)
    }

    #[test]
    fn off_it_drops_only_what_no_host_sends() {
        let mut fw = Firewall::new();
        assert!(comes_in(&mut fw, &syn_from(STRANGER, 22), 0));
        assert!(comes_in(&mut fw, &v4(STRANGER, ME, IpProtocol::Icmp, &icmp(8, 0), (0, false)), 0));
        // This machine's own address, loopback, multicast: forged.
        for src in [ME, "127.0.0.1", "224.0.0.9", "0.0.0.0", "255.255.255.255"] {
            assert!(!comes_in(&mut fw, &syn_from(src, 22), 0), "{src}");
        }
        assert!(!comes_in(&mut fw, &v4(STRANGER, "127.0.0.1", IpProtocol::Udp, &udp(1, 2), (0, false)), 0));
        // SYN with FIN; nothing at all; FIN alone (a scan's probes).
        for flags in [SYN | 1, 0, 1, 1 | 8 | 32] {
            assert!(!comes_in(&mut fw, &v4(STRANGER, ME, IpProtocol::Tcp, &tcp(1, 22, flags), (0, false)), 0));
        }
        assert_eq!(fw.stats().invalid, 10);
        // ICMP redirects.
        assert!(!comes_in(&mut fw, &v4(NEIGHBOUR, ME, IpProtocol::Icmp, &icmp(5, 1), (0, false)), 0));
    }

    #[test]
    fn new_connections_need_a_rule_or_an_open_socket() {
        let mut fw = on(|_| {});
        fw.add_receiver(1, FirewallProtocol::Tcp, 22);
        // A listener open to nobody yet.
        assert!(!comes_in(&mut fw, &syn_from(NEIGHBOUR, 22), 0));
        assert_eq!(fw.log().last().map(|e| e.event.reason), Some(FirewallReason::NotAllowed));
        fw.set_exposure(1, Some(Scope::LocalNetwork {}), "terminal");
        assert!(comes_in(&mut fw, &syn_from(NEIGHBOUR, 22), 0));
        assert!(!comes_in(&mut fw, &syn_from(STRANGER, 22), 0));
        assert_eq!(fw.log().last().map(|e| e.event.app.as_str()), Some("terminal"));
        // What a connection sends after its SYN is smoltcp's to take.
        assert!(comes_in(&mut fw, &v4(STRANGER, ME, IpProtocol::Tcp, &tcp(40000, 22, ACK), (0, false)), 0));
        // A port rule opens a port to where it says, whatever uses it.
        let mut fw = on(|c| {
            c.ports.push(PortRule {
                protocol: FirewallProtocol::Tcp,
                first: 8000,
                last: 8100,
                from: Scope::Prefix { address: "203.0.113.0".parse().unwrap(), len: 24 },
                allow: true,
            })
        });
        assert!(comes_in(&mut fw, &syn_from(STRANGER, 8080), 0));
        assert!(!comes_in(&mut fw, &syn_from(NEIGHBOUR, 8080), 0));
        // But not to an application kept off the network, nor are its
        // datagrams let in.
        let mut fw = on(|c| {
            c.blocked.push("videos".into());
            c.ports.push(PortRule {
                protocol: FirewallProtocol::Tcp,
                first: 8080,
                last: 8080,
                from: Scope::Anywhere {},
                allow: true,
            });
            c.ports.push(PortRule {
                protocol: FirewallProtocol::Udp,
                first: 5353,
                last: 5353,
                from: Scope::Anywhere {},
                allow: true,
            });
        });
        fw.add_receiver(1, FirewallProtocol::Tcp, 8080);
        fw.set_exposure(1, None, "videos");
        fw.add_receiver(2, FirewallProtocol::Udp, 5353);
        fw.set_exposure(2, None, "videos");
        assert!(!comes_in(&mut fw, &syn_from(STRANGER, 8080), 0));
        let last = fw.log().last().map(|e| (e.event.reason, e.event.app.clone(), e.event.port));
        assert_eq!(last, Some((FirewallReason::AppBlocked, "videos".into(), 8080)));
        assert!(!comes_in(&mut fw, &v4(NEIGHBOUR, ME, IpProtocol::Udp, &udp(5353, 5353), (0, false)), 0));
        assert_eq!(fw.stats().app_blocked, 2);
        // Once let back on, the rule opens its port again.
        fw.set_config(FirewallConfig { blocked: Vec::new(), ..fw.config.clone() });
        assert!(comes_in(&mut fw, &syn_from(STRANGER, 8080), 0));
        // One that closes a port wins over the socket's own rule.
        let mut fw = on(|c| {
            c.ports.push(PortRule {
                protocol: FirewallProtocol::Tcp,
                first: 22,
                last: 22,
                from: Scope::Anywhere {},
                allow: false,
            })
        });
        fw.add_receiver(1, FirewallProtocol::Tcp, 22);
        fw.set_exposure(1, Some(Scope::Anywhere {}), "terminal");
        assert!(!comes_in(&mut fw, &syn_from(NEIGHBOUR, 22), 0));
        // Blocking everything incoming sets every rule aside.
        let mut fw = on(|c| c.block_incoming = true);
        fw.add_receiver(1, FirewallProtocol::Tcp, 22);
        fw.set_exposure(1, Some(Scope::Anywhere {}), "terminal");
        assert!(!comes_in(&mut fw, &syn_from(NEIGHBOUR, 22), 0));
        // A socket that is gone is a closed port.
        let mut fw = on(|c| c.stealth = false);
        fw.add_receiver(1, FirewallProtocol::Tcp, 22);
        fw.remove_receiver(1);
        assert!(comes_in(&mut fw, &syn_from(NEIGHBOUR, 22), 0));
    }

    #[test]
    fn stealth_answers_no_probe() {
        // A closed port: smoltcp's reset would answer, and goes out unless
        // in stealth mode.
        let mut open = on(|c| c.stealth = false);
        assert!(comes_in(&mut open, &syn_from(STRANGER, 23), 0));
        let reset = v4(ME, STRANGER, IpProtocol::Tcp, &tcp(23, 40000, 4 | ACK), (0, false));
        assert!(goes_out(&mut open, &reset, 0));
        let mut fw = on(|_| {});
        assert!(!comes_in(&mut fw, &syn_from(STRANGER, 23), 0));
        assert_eq!(fw.stats().probes, 1);
        assert!(!goes_out(&mut fw, &reset, 0));
        // A connection this machine sent on is reset when it must be.
        let out = v4(ME, STRANGER, IpProtocol::Tcp, &tcp(23, 40000, ACK), (0, false));
        assert!(goes_out(&mut fw, &out, 0));
        assert!(goes_out(&mut fw, &reset, 10));
        // "Port unreachable" stays in too.
        let unreachable = v4(ME, STRANGER, IpProtocol::Icmp, &icmp(3, 3), (0, false));
        assert!(!goes_out(&mut fw, &unreachable, 0));
        assert!(goes_out(&mut open, &unreachable, 0));
    }

    #[test]
    fn replies_come_in_and_others_do_not() {
        let mut fw = on(|_| {});
        let query = v4(ME, "198.51.100.53", IpProtocol::Udp, &udp(50000, 53), (0, false));
        assert!(goes_out(&mut fw, &query, 0));
        let answer = v4("198.51.100.53", ME, IpProtocol::Udp, &udp(53, 50000), (0, false));
        assert!(comes_in(&mut fw, &answer, 1_000_000));
        // From elsewhere, to the same port: not a reply.
        let other = v4("198.51.100.54", ME, IpProtocol::Udp, &udp(53, 50000), (0, false));
        assert!(!comes_in(&mut fw, &other, 1_000_000));
        // Forgotten after three minutes without a datagram.
        assert!(comes_in(&mut fw, &answer, 1_000_000 + UDP_IDLE_US - 1));
        assert!(!comes_in(&mut fw, &answer, 1_000_000 + 2 * UDP_IDLE_US));
        // DHCP's answers.
        assert!(comes_in(&mut fw, &v4(NEIGHBOUR, "255.255.255.255", IpProtocol::Udp, &udp(67, 68), (0, false)), 0));
        // A UDP socket open to the local network.
        fw.add_receiver(2, FirewallProtocol::Udp, 5353);
        fw.set_exposure(2, Some(Scope::LocalNetwork {}), "music");
        assert!(comes_in(&mut fw, &v4(NEIGHBOUR, ME, IpProtocol::Udp, &udp(5353, 5353), (0, false)), 0));
        assert!(!comes_in(&mut fw, &v4(STRANGER, ME, IpProtocol::Udp, &udp(5353, 5353), (0, false)), 0));
    }

    #[test]
    fn pings_are_answered_from_where_the_settings_say() {
        let echo = |src| v4(src, ME, IpProtocol::Icmp, &icmp(8, 0), (0, false));
        let mut fw = on(|_| {});
        assert!(!comes_in(&mut fw, &echo(NEIGHBOUR), 0));
        assert_eq!(fw.stats().pings, 1);
        let mut fw = on(|c| c.ping = Some(Scope::LocalNetwork {}));
        assert!(comes_in(&mut fw, &echo(NEIGHBOUR), 0));
        assert!(!comes_in(&mut fw, &echo(STRANGER), 0));
        // A burst, then one a second.
        for _ in 1..PINGS_FROM_ONE.burst {
            assert!(comes_in(&mut fw, &echo(NEIGHBOUR), 0));
        }
        assert!(!comes_in(&mut fw, &echo(NEIGHBOUR), 0));
        assert_eq!(fw.stats().rate_limited, 1);
        assert!(comes_in(&mut fw, &echo(NEIGHBOUR), 1_000_000));
        // Replies to this machine's pings, and the errors about its
        // packets, always come in.
        let mut fw = on(|_| {});
        for (kind, code) in [(0, 0), (3, 4), (11, 0)] {
            assert!(comes_in(&mut fw, &v4(STRANGER, ME, IpProtocol::Icmp, &icmp(kind, code), (0, false)), 0));
        }
    }

    #[test]
    fn a_flood_of_connections_is_held_back() {
        let mut fw = on(|_| {});
        fw.add_receiver(1, FirewallProtocol::Tcp, 80);
        fw.set_exposure(1, Some(Scope::Anywhere {}), "web");
        let mut let_in = 0;
        for _ in 0..100 {
            let_in += comes_in(&mut fw, &syn_from(STRANGER, 80), 0) as u64;
        }
        assert_eq!(let_in, CONNECTIONS_FROM_ONE.burst);
        // The log counts a flood from one address into one entry.
        let floods = fw.log().filter(|e| e.event.reason == FirewallReason::RateLimited);
        assert_eq!(floods.map(|e| e.event.count).collect::<Vec<_>>(), vec![80]);
        // Another address is not held back by that one.
        assert!(comes_in(&mut fw, &syn_from(NEIGHBOUR, 80), 0));
        // All of them together are, past the burst.
        let mut all = 0;
        for i in 0..2000u32 {
            let src = Ipv4Addr::from(0x0a00_0000 + i).to_string();
            all += comes_in(&mut fw, &syn_from(&src, 80), 0) as u64;
        }
        assert!(all < CONNECTIONS_FROM_ALL.burst, "{all}");
    }

    #[test]
    fn fragments_follow_their_first_piece() {
        let mut fw = on(|_| {});
        fw.add_receiver(1, FirewallProtocol::Udp, 9000);
        fw.set_exposure(1, Some(Scope::Anywhere {}), "game");
        let first = v4(STRANGER, ME, IpProtocol::Udp, &udp(9000, 9000), (0, true));
        let later = v4(STRANGER, ME, IpProtocol::Udp, &[0; 16], (16, false));
        // A later piece before any first: turned away.
        assert!(!comes_in(&mut fw, &later, 0));
        assert!(comes_in(&mut fw, &first, 0));
        assert!(comes_in(&mut fw, &later, 1_000_000));
        assert!(!comes_in(&mut fw, &later, FRAGMENT_US + 1));
        // A first piece too short for its TCP header, and one that would
        // rewrite it.
        let short = v4(STRANGER, ME, IpProtocol::Tcp, &tcp(40000, 22, SYN)[..8], (0, true));
        assert!(!comes_in(&mut fw, &short, 0));
        let mut fw = on(|c| c.stealth = false);
        assert!(comes_in(&mut fw, &v4(STRANGER, ME, IpProtocol::Tcp, &tcp(40000, 22, SYN), (0, true)), 0));
        assert!(!comes_in(&mut fw, &v4(STRANGER, ME, IpProtocol::Tcp, &[0; 16], (8, false)), 0));
        assert_eq!(fw.stats().fragments, 1);
    }

    #[test]
    fn ipv6_neighbour_discovery_comes_from_the_link_alone() {
        let mut fw = on(|_| {});
        // A message as long as a neighbour solicitation's (its header and
        // target address).
        let nd = |kind: u8| {
            let mut m = icmp(kind, 0);
            m.resize(24, 0);
            m
        };
        // Neighbour solicitations and advertisements, router
        // advertisements: from the link (a hop limit of 255).
        for kind in [134, 135, 136] {
            assert!(comes_in(&mut fw, &v6("fe80::1", "fe80::10", IpProtocol::Icmpv6, 255, &nd(kind)), 0));
            assert!(!comes_in(&mut fw, &v6("fe80::1", "fe80::10", IpProtocol::Icmpv6, 64, &nd(kind)), 0));
        }
        // Duplicate address detection, from no address.
        assert!(comes_in(&mut fw, &v6("::", "ff02::1:ff00:10", IpProtocol::Icmpv6, 255, &nd(135)), 0));
        assert!(!comes_in(&mut fw, &v6("::", "fe80::10", IpProtocol::Udp, 64, &udp(1, 2)), 0));
        // Redirects and router solicitations are not for this machine.
        assert!(!comes_in(&mut fw, &v6("fe80::1", "fe80::10", IpProtocol::Icmpv6, 255, &nd(137)), 0));
        assert!(!comes_in(&mut fw, &v6("fe80::1", "fe80::10", IpProtocol::Icmpv6, 255, &nd(133)), 0));
        // Packet too big: what TCP needs to find the path's size.
        assert!(comes_in(&mut fw, &v6("2001:db8::1", "fe80::10", IpProtocol::Icmpv6, 64, &nd(2)), 0));
        // The type 0 routing header, and fragments.
        let mut routed = vec![u8::from(IpProtocol::Udp), 0, 0, 1, 0, 0, 0, 0];
        routed.extend(udp(1, 2));
        assert!(!comes_in(&mut fw, &v6("2001:db8::1", "fe80::10", IpProtocol::Ipv6Route, 64, &routed), 0));
        let mut piece = vec![u8::from(IpProtocol::Udp), 0, 0, 1, 0, 0, 0, 7];
        piece.extend(udp(1, 2));
        assert!(!comes_in(&mut fw, &v6("2001:db8::1", "fe80::10", IpProtocol::Ipv6Frag, 64, &piece), 0));
        // A link-local neighbour is on the local network.
        fw.add_receiver(1, FirewallProtocol::Tcp, 22);
        fw.set_exposure(1, Some(Scope::LocalNetwork {}), "terminal");
        let syn = tcp(40000, 22, SYN);
        assert!(comes_in(&mut fw, &v6("fe80::1", "fe80::10", IpProtocol::Tcp, 64, &syn), 0));
        assert!(!comes_in(&mut fw, &v6("2001:db8::1", "fe80::10", IpProtocol::Tcp, 64, &syn), 0));
    }

    #[test]
    fn the_log_folds_repeats_and_keeps_the_latest() {
        let mut fw = on(|_| {});
        // A scan: probes of many ports, one entry.
        for port in 1..=50 {
            assert!(!comes_in(&mut fw, &syn_from(STRANGER, port), port as u64 * 1000));
        }
        let entries: Vec<_> = fw.log().collect();
        assert_eq!(entries.len(), 1);
        assert_eq!((entries[0].event.count, entries[0].event.reason), (50, FirewallReason::Probe));
        assert_eq!(entries[0].event.interface, "eth0");
        // A second later, a new entry.
        assert!(!comes_in(&mut fw, &syn_from(STRANGER, 1), 3_000_000));
        assert_eq!(fw.log().count(), 2);
        // No more than LOG_LEN entries; the oldest go first.
        for i in 0..LOG_LEN as u32 + 10 {
            let src = Ipv4Addr::from(0x0a00_0000 + i).to_string();
            comes_in(&mut fw, &syn_from(&src, 1), 4_000_000);
        }
        assert_eq!(fw.log().count(), LOG_LEN);
        assert_eq!(fw.stats().probes, 50 + 1 + LOG_LEN as u64 + 10);
        // Without the log, still counted.
        let mut quiet = on(|c| c.log = false);
        assert!(!comes_in(&mut quiet, &syn_from(STRANGER, 1), 0));
        assert_eq!((quiet.log().count(), quiet.stats().probes), (0, 1));
        // What netd turns away is logged too.
        quiet.set_config(FirewallConfig::default());
        quiet.note_app_blocked(6, "198.51.100.1:443".parse().unwrap(), "videos", 0);
        let e = &quiet.log().last().unwrap().event;
        assert_eq!((e.reason, e.app.as_str(), e.interface.as_str()), (FirewallReason::AppBlocked, "videos", ""));
    }

    #[test]
    fn rules_name_applications_as_the_system_does() {
        // The settings say which applications are let in; netd turns that
        // into each socket's exposure.
        let c = FirewallConfig {
            incoming: vec![
                AppRule { app: "music".to_string(), from: Some(Scope::LocalNetwork {}) },
                AppRule { app: "photos".to_string(), from: None },
            ],
            ..FirewallConfig::default()
        };
        let mut fw = on(|x| *x = c.clone());
        fw.add_receiver(1, FirewallProtocol::Tcp, 7000);
        fw.set_exposure(1, c.incoming_for("music"), "music");
        assert!(comes_in(&mut fw, &syn_from(NEIGHBOUR, 7000), 0));
        fw.add_receiver(2, FirewallProtocol::Tcp, 7001);
        fw.set_exposure(2, c.incoming_for("editor"), "editor");
        assert!(!comes_in(&mut fw, &syn_from(NEIGHBOUR, 7001), 0));
        // Not let in, as the user answered when asked.
        fw.add_receiver(3, FirewallProtocol::Tcp, 7002);
        fw.set_exposure(3, c.incoming_for("photos"), "photos");
        assert!(!comes_in(&mut fw, &syn_from(NEIGHBOUR, 7002), 0));
        assert_eq!(fw.log().last().map(|e| e.event.reason), Some(FirewallReason::NotAllowed));
    }
}
