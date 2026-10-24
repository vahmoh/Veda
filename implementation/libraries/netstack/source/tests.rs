// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Integration tests: whole conversations between two stacks over a
//! simulated cable, with a DHCP server in the harness and a DNS server on
//! the far stack. The cable can drop frames at random, go dark, or take only
//! a few frames at a time (a full link).

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use proto::net::{
    AddrFamily, Connectivity, DeviceInfo, FirewallConfig, FirewallProtocol, FirewallReason, InterfaceKind, IpConfig,
    NetError, ResolveResult, Scope, TcpOptions,
};
use smoltcp::phy::ChecksumCapabilities;
use smoltcp::wire::{
    DhcpMessageType, DhcpPacket, DhcpRepr, EthernetAddress, EthernetFrame, EthernetProtocol, Icmpv6DstUnreachable,
    Icmpv6Message, Icmpv6Packet, Icmpv6Repr, IpProtocol, Ipv4Packet, Ipv4Repr, Ipv6Packet, Ipv6Repr, UdpPacket,
    UdpRepr,
};

use crate::dns::tests::{encode_name, response};
use crate::{IfaceId, Link, Links, PingEvent, SockId, Stack, TcpState};

const CLIENT_MAC: [u8; 6] = [0x02, 0, 0, 0, 0, 0x15];
const SERVER_MAC: [u8; 6] = [0x02, 0, 0, 0, 0, 0x02];
const SERVER_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 2);
const CLIENT_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 15);

/// A deterministic random source for frame loss.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
}

/// What the harness's DHCP server hands out.
struct DhcpServer {
    enabled: bool,
    address: Ipv4Addr,
    dns: Vec<Ipv4Addr>,
    lease_s: u32,
    requests_seen: u32,
    /// After each ACK, send one for another client with this lease.
    forged_lease: Option<u32>,
}

/// One end of the cable: what arrived for a stack, and what it sent.
#[derive(Default)]
struct End {
    inbox: VecDeque<Vec<u8>>,
    outbox: VecDeque<Vec<u8>>,
    /// Frames the end takes before it is full (`usize::MAX`: no limit).
    room: usize,
    /// Times the stack found the end full.
    full: u32,
}

impl Link for End {
    fn receive(&mut self, buf: &mut [u8]) -> Option<usize> {
        loop {
            let f = self.inbox.pop_front()?;
            if f.len() <= buf.len() {
                buf[..f.len()].copy_from_slice(&f);
                return Some(f.len());
            }
        }
    }

    fn can_send(&self) -> bool {
        self.outbox.len() < self.room
    }

    fn send(&mut self, frame: &[u8]) -> bool {
        if !self.can_send() {
            self.full += 1;
            return false;
        }
        self.outbox.push_back(frame.to_vec());
        true
    }
}

/// A stack's one link, as [`Links`].
struct Cable<'a>(IfaceId, &'a mut End);

impl Links for Cable<'_> {
    fn link(&mut self, id: IfaceId) -> Option<&mut dyn Link> {
        (id == self.0).then_some(&mut *self.1 as &mut dyn Link)
    }
}

struct Net {
    a: Stack,
    b: Stack,
    a_eth: IfaceId,
    b_eth: IfaceId,
    a_end: End,
    b_end: End,
    now: u64,
    loss: u64,
    dark: bool,
    rng: Rng,
    dhcp: DhcpServer,
    /// DNS server socket on `b` and how it answers.
    dns_sock: SockId,
    dns_mode: DnsMode,
    dns_queries: u32,
    /// Every frame `a` sends, while `Some`.
    recorded: Option<Vec<Vec<u8>>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DnsMode {
    Answer,
    Silent,
    ServFail,
}

fn device(mac: [u8; 6]) -> DeviceInfo {
    DeviceInfo { kind: InterfaceKind::Ethernet, mac, mtu: 1500, driver: "test".into(), location: "cable".into() }
}

impl Net {
    fn new() -> Net {
        let mut a = Stack::new(b"client seed for tests, 32+ bytes long", 1_000_000);
        let mut b = Stack::new(b"server seed for tests, 32+ bytes long", 1_000_000);
        let a_eth = a.add_interface(&device(CLIENT_MAC), true).unwrap();
        let b_eth = b.add_interface(&device(SERVER_MAC), true).unwrap();
        b.configure(b_eth, IpConfig::Static { address: SERVER_IP.into(), prefix_len: 24, gateway: None, dns: vec![] })
            .unwrap();
        let (dns_sock, _) = b.udp_bind("0.0.0.0:53".parse().unwrap(), false, 0).unwrap();
        Net {
            a,
            b,
            a_eth,
            b_eth,
            a_end: End { room: usize::MAX, ..End::default() },
            b_end: End { room: usize::MAX, ..End::default() },
            now: 1_000_000,
            loss: 0,
            dark: false,
            rng: Rng(0x9E37_79B9_7F4A_7C15),
            dhcp: DhcpServer {
                enabled: true,
                address: CLIENT_IP,
                dns: vec![SERVER_IP],
                lease_s: 3600,
                requests_seen: 0,
                forged_lease: None,
            },
            dns_sock,
            dns_mode: DnsMode::Answer,
            dns_queries: 0,
            recorded: None,
        }
    }

    /// Advances time by `ms` milliseconds in 1 ms steps.
    fn run(&mut self, ms: u64) {
        for _ in 0..ms {
            self.step();
        }
    }

    fn step(&mut self) {
        self.now += 1000;
        self.a.poll(self.now, &mut Cable(self.a_eth, &mut self.a_end));
        self.b.poll(self.now, &mut Cable(self.b_eth, &mut self.b_end));
        let to_b: Vec<Vec<u8>> = self.a_end.outbox.drain(..).collect();
        if let Some(r) = self.recorded.as_mut() {
            r.extend(to_b.iter().cloned());
        }
        let mut to_a: Vec<Vec<u8>> = self.b_end.outbox.drain(..).collect();
        for f in to_b {
            if self.dark || self.rng.chance(self.loss) {
                continue;
            }
            if let Some(reply) = self.dhcp_answer(&f) {
                to_a.extend(reply);
                continue;
            }
            self.b_end.inbox.push_back(f);
        }
        for f in to_a {
            if self.dark || self.rng.chance(self.loss) {
                continue;
            }
            self.a_end.inbox.push_back(f);
        }
        self.serve_dns();
    }

    /// A frame arrives at `a` from the cable.
    fn arrive_at_a(&mut self, frame: &[u8]) {
        self.a_end.inbox.push_back(frame.to_vec());
    }

    fn run_until(&mut self, max_ms: u64, mut done: impl FnMut(&mut Net) -> bool) -> bool {
        for _ in 0..max_ms {
            self.step();
            if done(self) {
                return true;
            }
        }
        false
    }

    /// The harness DHCP server: answers DISCOVER with OFFER and REQUEST
    /// with ACK (the frames to `a`; none: no server). With `forged_lease`,
    /// every ACK is followed by one for another client.
    fn dhcp_answer(&mut self, frame: &[u8]) -> Option<Vec<Vec<u8>>> {
        let eth = EthernetFrame::new_checked(frame).ok()?;
        if eth.ethertype() != EthernetProtocol::Ipv4 {
            return None;
        }
        let ip = Ipv4Packet::new_checked(eth.payload()).ok()?;
        if ip.next_header() != IpProtocol::Udp {
            return None;
        }
        let udp = UdpPacket::new_checked(ip.payload()).ok()?;
        if udp.dst_port() != 67 {
            return None;
        }
        let dhcp = DhcpPacket::new_checked(udp.payload()).ok()?;
        let req = DhcpRepr::parse(&dhcp).ok()?;
        if !self.dhcp.enabled {
            // Swallow the request: no server on the network.
            return Some(Vec::new());
        }
        let kind = match req.message_type {
            DhcpMessageType::Discover => DhcpMessageType::Offer,
            DhcpMessageType::Request => {
                self.dhcp.requests_seen += 1;
                DhcpMessageType::Ack
            }
            _ => return None,
        };
        let mut out = vec![self.dhcp_reply(kind, &req, req.client_hardware_address, self.dhcp.lease_s)];
        if kind == DhcpMessageType::Ack
            && let Some(lease_s) = self.dhcp.forged_lease
        {
            // Another client's acknowledgement (broadcast), which `a` sees
            // too.
            out.push(self.dhcp_reply(kind, &req, EthernetAddress([2, 0, 0, 0, 0, 0x99]), lease_s));
        }
        Some(out)
    }

    /// The server's `kind` answer to `req`, for client `chaddr`.
    fn dhcp_reply(&self, kind: DhcpMessageType, req: &DhcpRepr, chaddr: EthernetAddress, lease_s: u32) -> Vec<u8> {
        let mut dns = heapless::Vec::new();
        for d in &self.dhcp.dns {
            let _ = dns.push(*d);
        }
        let repr = DhcpRepr {
            message_type: kind,
            transaction_id: req.transaction_id,
            secs: 0,
            client_hardware_address: chaddr,
            client_ip: Ipv4Addr::UNSPECIFIED,
            your_ip: self.dhcp.address,
            server_ip: SERVER_IP,
            router: Some(SERVER_IP),
            subnet_mask: Some(Ipv4Addr::new(255, 255, 255, 0)),
            relay_agent_ip: Ipv4Addr::UNSPECIFIED,
            broadcast: false,
            requested_ip: None,
            client_identifier: None,
            server_identifier: Some(SERVER_IP),
            parameter_request_list: None,
            dns_servers: Some(dns),
            max_size: None,
            lease_duration: Some(lease_s),
            renew_duration: None,
            rebind_duration: None,
            additional_options: &[],
        };
        let dhcp_len = repr.buffer_len();
        let mut out = vec![0u8; 14 + 20 + 8 + dhcp_len];
        let mut eth = EthernetFrame::new_unchecked(&mut out[..]);
        eth.set_dst_addr(EthernetAddress::BROADCAST);
        eth.set_src_addr(EthernetAddress(SERVER_MAC));
        eth.set_ethertype(EthernetProtocol::Ipv4);
        let caps = ChecksumCapabilities::default();
        let ip_repr = Ipv4Repr {
            src_addr: SERVER_IP,
            dst_addr: Ipv4Addr::BROADCAST,
            next_header: IpProtocol::Udp,
            payload_len: 8 + dhcp_len,
            hop_limit: 64,
        };
        ip_repr.emit(&mut Ipv4Packet::new_unchecked(&mut out[14..]), &caps);
        let udp_repr = UdpRepr { src_port: 67, dst_port: 68 };
        udp_repr.emit(
            &mut UdpPacket::new_unchecked(&mut out[34..]),
            &SERVER_IP.into(),
            &Ipv4Addr::BROADCAST.into(),
            dhcp_len,
            |buf| {
                let _ = repr.emit(&mut DhcpPacket::new_unchecked(buf));
            },
            &caps,
        );
        out
    }

    /// The DNS server on `b`: `*.example` names resolve, `nx.example`
    /// does not exist.
    fn serve_dns(&mut self) {
        let mut buf = vec![0u8; 2048];
        while let Some((len, from)) = udp_recv(&mut self.b, self.dns_sock, &mut buf) {
            self.dns_queries += 1;
            let q = &buf[..len];
            let reply = match self.dns_mode {
                DnsMode::Silent => continue,
                DnsMode::ServFail => response(q, 2, &[], &[]),
                DnsMode::Answer => {
                    let name = qname(q);
                    let qtype = u16::from_be_bytes([q[q.len() - 15], q[q.len() - 14]]);
                    if name == "nx.example" {
                        response(q, 3, &[], &[])
                    } else if name == "alias.example" {
                        response(
                            q,
                            0,
                            &[
                                ("alias.example", 5, 600, encode_name("host.example")),
                                ("host.example", 1, 120, vec![10, 0, 2, 2]),
                            ],
                            &[],
                        )
                    } else if qtype == 1 {
                        response(q, 0, &[(name.as_str(), 1, 300, vec![10, 0, 2, 2])], &[])
                    } else {
                        response(q, 0, &[], &[])
                    }
                }
            };
            let _ = self.b.udp_send(self.dns_sock, from, &reply);
        }
    }

    fn configured(&mut self) -> bool {
        self.run_until(10_000, |n| n.a.status().connectivity == Connectivity::Routable)
    }

    fn resolve(&mut self, name: &str) -> ResolveResult {
        let id = self.a.resolve(name, AddrFamily::Any).unwrap();
        for _ in 0..20_000 {
            if let Some(r) = self.a.resolve_result(id) {
                return r;
            }
            self.step();
        }
        panic!("lookup of {name} never finished");
    }
}

/// Takes what a TCP connection of `stack` received, up to `buf.len()`.
fn tcp_recv(stack: &mut Stack, id: SockId, buf: &mut [u8]) -> Result<usize, NetError> {
    let max = buf.len();
    stack.tcp_recv_with(id, max, |data| {
        buf[..data.len()].copy_from_slice(data);
        data.len()
    })
}

/// Takes the next datagram of a UDP socket of `stack`: (length, sender).
fn udp_recv(stack: &mut Stack, id: SockId, buf: &mut [u8]) -> Option<(usize, SocketAddr)> {
    let mut got = None;
    stack.udp_recv_with(id, |from, data| {
        let n = data.len().min(buf.len());
        buf[..n].copy_from_slice(&data[..n]);
        got = Some((n, from));
        true
    })?;
    got
}

/// Takes the ping results of a ping socket of `stack`.
fn ping_events(stack: &mut Stack, id: SockId) -> Vec<PingEvent> {
    let mut out = Vec::new();
    while let Some(ev) = stack.ping_next(id) {
        stack.ping_taken(id);
        out.push(ev);
    }
    out
}

fn qname(q: &[u8]) -> String {
    let mut i = 12;
    let mut parts = Vec::new();
    while q[i] != 0 {
        let n = q[i] as usize;
        parts.push(String::from_utf8_lossy(&q[i + 1..i + 1 + n]).into_owned());
        i += 1 + n;
    }
    parts.join(".")
}

fn connect(n: &mut Net, to: SocketAddr) -> SockId {
    let s = n.a.tcp_connect(to, &TcpOptions::default(), 1).unwrap();
    let ok = n.run_until(5_000, |n| matches!(n.a.tcp_view(s).unwrap().state, TcpState::Open { .. }));
    assert!(ok, "connection to {to} not established: {:?}", n.a.tcp_view(s));
    s
}

/// Moves `data` from `a` to `b` over connection (`sa`, `sb`) and checks it
/// arrives intact.
fn transfer(n: &mut Net, sa: SockId, sb: SockId, data: &[u8], max_ms: u64) {
    let mut sent = 0;
    let mut got = Vec::new();
    let mut buf = vec![0u8; 8192];
    for _ in 0..max_ms {
        if sent < data.len() {
            sent += n.a.tcp_send(sa, &data[sent..]).unwrap();
        }
        n.step();
        loop {
            let k = tcp_recv(&mut n.b, sb, &mut buf).unwrap();
            if k == 0 {
                break;
            }
            got.extend_from_slice(&buf[..k]);
        }
        if got.len() == data.len() {
            break;
        }
    }
    assert_eq!(got.len(), data.len(), "transfer incomplete");
    assert!(got == data, "data corrupted");
}

fn accept(n: &mut Net, listener: SockId) -> SockId {
    let mut acc = None;
    n.run_until(5_000, |n| {
        acc = n.b.tcp_accept(listener);
        acc.is_some()
    });
    acc.expect("no connection accepted")
}

fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 7 + i / 251) as u8).collect()
}

/// The link-local address of `stack`'s cable.
fn link_local(stack: &Stack) -> Ipv6Addr {
    stack
        .interfaces()
        .into_iter()
        .flat_map(|i| i.addresses)
        .find_map(|a| match a.address {
            IpAddr::V6(v) if v.is_unicast_link_local() => Some(v),
            _ => None,
        })
        .expect("a link-local address")
}

#[test]
fn dhcp_configures_the_interface() {
    let mut n = Net::new();
    assert!(n.configured());
    let info = n.a.interfaces().into_iter().find(|i| i.name == "eth0").unwrap();
    assert!(info.addresses.iter().any(|a| a.address == IpAddr::V4(CLIENT_IP) && a.prefix_len == 24));
    assert_eq!(info.gateways.first(), Some(&IpAddr::V4(SERVER_IP)));
    assert_eq!(info.dns, vec![IpAddr::V4(SERVER_IP)]);
    assert_eq!(info.dhcp.lease_s, 3600);
    let st = n.a.status();
    assert_eq!(st.default_interface, "eth0");
    assert_eq!(st.default_gateway, Some(IpAddr::V4(SERVER_IP)));
    // An IPv6 link-local address, which does not give away the MAC (no
    // EUI-64 identifier: no 02:00:00:ff:fe:00:00:15 in it).
    let ll = info.addresses.iter().find_map(|a| match a.address {
        IpAddr::V6(v) if v.segments()[0] == 0xfe80 => Some(v),
        _ => None,
    });
    let ll = ll.expect("a link-local address");
    assert_ne!(ll.segments()[4..], [0x0000, 0x00ff, 0xfe00, 0x0015]);
    assert_ne!(&ll.octets()[8..], &[0x00, 0x00, 0x00, 0xff, 0xfe, 0x00, 0x00, 0x15]);
    // Stable: the same interface on the same network gets the same address.
    let mut again = Stack::new(b"client seed for tests, 32+ bytes long", 1_000_000);
    let id = again.add_interface(&device(CLIENT_MAC), true).unwrap();
    let same = again.interfaces().into_iter().find(|i| i.name == again.interface_name(id).unwrap()).unwrap();
    assert!(same.addresses.iter().any(|a| a.address == IpAddr::V6(ll)));
}

#[test]
fn dhcp_retries_until_a_server_answers() {
    let mut n = Net::new();
    n.dhcp.enabled = false;
    n.run(10_000);
    assert_eq!(n.a.status().connectivity, Connectivity::Offline);
    n.dhcp.enabled = true;
    assert!(n.configured(), "no lease after the server came back");
}

#[test]
fn tcp_round_trip() {
    let mut n = Net::new();
    assert!(n.configured());
    let (l, _) = n.b.tcp_listen("0.0.0.0:7".parse().unwrap(), false, 4, &TcpOptions::default(), 0).unwrap();
    let sa = connect(&mut n, "10.0.2.2:7".parse().unwrap());
    let sb = accept(&mut n, l);
    let data = pattern(300_000);
    transfer(&mut n, sa, sb, &data, 60_000);
    // And back the other way.
    let back = pattern(50_000);
    let mut sent = 0;
    let mut got = Vec::new();
    let mut buf = vec![0u8; 4096];
    for _ in 0..30_000 {
        if sent < back.len() {
            sent += n.b.tcp_send(sb, &back[sent..]).unwrap();
        }
        n.step();
        while let Ok(k) = tcp_recv(&mut n.a, sa, &mut buf) {
            if k == 0 {
                break;
            }
            got.extend_from_slice(&buf[..k]);
        }
        if got.len() == back.len() {
            break;
        }
    }
    assert!(got == back);
    // A graceful close: the server sees end of stream.
    n.a.close(sa);
    let eof = n.run_until(5_000, |n| n.b.tcp_view(sb).unwrap().eof);
    assert!(eof);
}

#[test]
fn tcp_survives_heavy_loss() {
    let mut n = Net::new();
    assert!(n.configured());
    let (l, _) = n.b.tcp_listen("0.0.0.0:9000".parse().unwrap(), false, 2, &TcpOptions::default(), 0).unwrap();
    let sa = connect(&mut n, "10.0.2.2:9000".parse().unwrap());
    let sb = accept(&mut n, l);
    n.loss = 15;
    transfer(&mut n, sa, sb, &pattern(120_000), 600_000);
}

#[test]
fn refused_and_timed_out_connections() {
    let mut n = Net::new();
    assert!(n.configured());
    let s = n.a.tcp_connect("10.0.2.2:81".parse().unwrap(), &TcpOptions::default(), 1).unwrap();
    n.run_until(3_000, |n| matches!(n.a.tcp_view(s).unwrap().state, TcpState::Closed { .. }));
    assert_eq!(n.a.tcp_view(s).unwrap().state, TcpState::Closed { error: Some(NetError::ConnectionRefused) });
    // Nobody answers: the connect timeout fires.
    n.dark = true;
    let opts = TcpOptions { connect_timeout_ms: 2_000, ..TcpOptions::default() };
    let t = n.a.tcp_connect("10.0.2.2:80".parse().unwrap(), &opts, 1).unwrap();
    n.run(2_500);
    assert_eq!(n.a.tcp_view(t).unwrap().state, TcpState::Closed { error: Some(NetError::TimedOut) });
}

/// A TCP socket bound before it listens or connects has its port from the
/// start (port 0: a free one), which no other socket binds or listens on
/// while it has it; it listens there, and its port is free once it closes.
#[test]
fn bound_sockets_hold_their_port() {
    let mut n = Net::new();
    assert!(n.configured());
    let o = TcpOptions::default();
    let (b, local) = n.b.tcp_bind("0.0.0.0:0".parse().unwrap(), false, 0).unwrap();
    assert_ne!(local.port(), 0);
    assert_eq!(n.b.tcp_bind(local, false, 0), Err(NetError::AddressInUse));
    assert_eq!(n.b.tcp_listen(local, false, 4, &o, 0), Err(NetError::AddressInUse));
    assert_eq!(n.b.port_holders(FirewallProtocol::Tcp, local.port()), vec![b]);
    assert_eq!(n.b.tcp_listen_bound(b, 4, &o), Ok(local));
    // Listening, it is a listener, and no longer bound.
    assert_eq!(n.b.tcp_listen_bound(b, 4, &o), Err(NetError::InvalidArgument));
    let sa = connect(&mut n, SocketAddr::new(IpAddr::V4(SERVER_IP), local.port()));
    let sb = accept(&mut n, b);
    transfer(&mut n, sa, sb, b"bound first", 1_000);
    n.b.close(b);
    assert!(n.b.port_holders(FirewallProtocol::Tcp, local.port()).is_empty());
    let (again, _) = n.b.tcp_bind(local, false, 0).unwrap();
    // A socket bound and closed gives its port back too.
    n.b.close(again);
    assert!(n.b.tcp_bind(local, false, 0).is_ok());
}

/// A bound TCP socket connects from its address and port; not from an
/// address the route's interface does not have, and not between the same two
/// ends as another connection.
#[test]
fn bound_sockets_connect_from_their_address() {
    let mut n = Net::new();
    assert!(n.configured());
    let o = TcpOptions::default();
    let (l, _) = n.b.tcp_listen("0.0.0.0:7".parse().unwrap(), false, 4, &o, 0).unwrap();
    let server: SocketAddr = "10.0.2.2:7".parse().unwrap();
    let from = SocketAddr::new(IpAddr::V4(CLIENT_IP), 40000);
    let (s, _) = n.a.tcp_bind(from, false, 1).unwrap();
    assert_eq!(n.a.tcp_connect_bound(s, "10.0.2.2:0".parse().unwrap(), &o), Err(NetError::InvalidArgument));
    n.a.tcp_connect_bound(s, server, &o).unwrap();
    let open = n.run_until(5_000, |n| matches!(n.a.tcp_view(s).unwrap().state, TcpState::Open { .. }));
    assert!(open, "no connection: {:?}", n.a.tcp_view(s));
    assert_eq!(n.a.tcp_view(s).unwrap().state, TcpState::Open { local: from, remote: server });
    let sb = accept(&mut n, l);
    assert_eq!(n.b.tcp_remote(sb), Some(from));
    // The port is free to bind (connections share theirs), not to connect
    // to the same peer from.
    let (t, _) = n.a.tcp_bind(from, false, 1).unwrap();
    assert_eq!(n.a.tcp_connect_bound(t, server, &o), Err(NetError::AddressNotAvailable));
    // The loopback address has no way to other machines.
    let (u, _) = n.a.tcp_bind("127.0.0.1:0".parse().unwrap(), false, 1).unwrap();
    assert_eq!(n.a.tcp_connect_bound(u, server, &o), Err(NetError::InvalidArgument));
    // Failing, they stay bound.
    assert_eq!(n.a.port_holders(FirewallProtocol::Tcp, from.port()), vec![t]);
}

/// Sockets share a port when no address is in both their bindings, as on
/// Linux: an IPv4 socket and an IPv6-only one, or sockets on different
/// addresses. Each takes what comes to its own addresses.
#[test]
fn sockets_bound_to_addresses_apart_share_a_port() {
    let mut n = Net::new();
    assert!(n.configured());
    let o = TcpOptions::default();
    let (any4, any6): (SocketAddr, SocketAddr) = ("0.0.0.0:7".parse().unwrap(), "[::]:7".parse().unwrap());
    let (l4, _) = n.b.tcp_listen(any4, false, 4, &o, 0).unwrap();
    // `::` is IPv4's addresses too, unless IPv6-only.
    assert_eq!(n.b.tcp_listen(any6, false, 4, &o, 0), Err(NetError::AddressInUse));
    let (l6, _) = n.b.tcp_listen(any6, true, 4, &o, 0).unwrap();
    for (taken, v6only) in [("10.0.2.2:7", false), ("[::1]:7", true), ("[::]:7", true)] {
        assert_eq!(n.b.tcp_bind(taken.parse().unwrap(), v6only, 0), Err(NetError::AddressInUse), "{taken}");
    }
    assert_eq!(n.b.port_holders(FirewallProtocol::Tcp, 7).len(), 2);
    let sa = connect(&mut n, "10.0.2.2:7".parse().unwrap());
    let sb = accept(&mut n, l4);
    transfer(&mut n, sa, sb, b"over IPv4", 1_000);
    let b_ll = link_local(&n.b);
    let sa6 = connect(&mut n, SocketAddr::new(IpAddr::V6(b_ll), 7));
    let sb6 = accept(&mut n, l6);
    transfer(&mut n, sa6, sb6, b"over IPv6", 1_000);
    assert_eq!(n.b.tcp_accept(l4), None);
    // Addresses of one version apart, and UDP alike.
    let (one, _) = n.b.udp_bind("10.0.2.2:5000".parse().unwrap(), false, 0).unwrap();
    let (other, _) = n.b.udp_bind("127.0.0.1:5000".parse().unwrap(), false, 0).unwrap();
    assert_eq!(n.b.udp_bind("0.0.0.0:5000".parse().unwrap(), false, 0), Err(NetError::AddressInUse));
    let (only6, _) = n.b.udp_bind("[::]:5000".parse().unwrap(), true, 0).unwrap();
    let (from, _) = n.a.udp_bind("[::]:0".parse().unwrap(), false, 1).unwrap();
    n.a.udp_send(from, "10.0.2.2:5000".parse().unwrap(), b"to IPv4").unwrap();
    n.a.udp_send(from, SocketAddr::new(IpAddr::V6(b_ll), 5000), b"to IPv6").unwrap();
    let mut buf = [0u8; 64];
    let mut got = Vec::new();
    n.run_until(2_000, |n| {
        for s in [one, other, only6] {
            if let Some((len, _)) = udp_recv(&mut n.b, s, &mut buf) {
                got.push((s, buf[..len].to_vec()));
            }
        }
        got.len() == 2
    });
    assert_eq!(got, vec![(one, b"to IPv4".to_vec()), (only6, b"to IPv6".to_vec())]);
}

/// A socket reaches the addresses of its version alone: from an IPv4
/// address, no IPv6 one (smoltcp could not make such a packet), and from an
/// IPv6-only socket, no IPv4 one.
#[test]
fn sockets_reach_their_version_alone() {
    let mut n = Net::new();
    assert!(n.configured());
    let o = TcpOptions::default();
    let cases = [("127.0.0.1:0", false, "[::1]:9"), ("0.0.0.0:0", false, "[::1]:9"), ("[::]:0", true, "127.0.0.1:9")];
    for (local, v6only, remote) in cases {
        let (u, _) = n.a.udp_bind(local.parse().unwrap(), v6only, 1).unwrap();
        assert_eq!(n.a.udp_send(u, remote.parse().unwrap(), b"x"), Err(NetError::NoRoute), "{local} to {remote}");
        let (t, _) = n.a.tcp_bind(local.parse().unwrap(), v6only, 1).unwrap();
        assert_eq!(n.a.tcp_connect_bound(t, remote.parse().unwrap(), &o), Err(NetError::NoRoute));
    }
    // Both versions, from `::`.
    let (both, _) = n.a.udp_bind("[::]:0".parse().unwrap(), false, 1).unwrap();
    n.a.udp_send(both, "127.0.0.1:9".parse().unwrap(), b"x").unwrap();
    n.a.udp_send(both, "[::1]:9".parse().unwrap(), b"x").unwrap();
    n.run(10);
}

#[test]
fn reset_connections_report_reset() {
    let mut n = Net::new();
    assert!(n.configured());
    let (l, _) = n.b.tcp_listen("0.0.0.0:7".parse().unwrap(), false, 1, &TcpOptions::default(), 0).unwrap();
    let sa = connect(&mut n, "10.0.2.2:7".parse().unwrap());
    let sb = accept(&mut n, l);
    n.b.tcp_abort(sb);
    n.run_until(2_000, |n| matches!(n.a.tcp_view(sa).unwrap().state, TcpState::Closed { .. }));
    assert_eq!(n.a.tcp_view(sa).unwrap().state, TcpState::Closed { error: Some(NetError::ConnectionReset) });
}

#[test]
fn short_link_outage_keeps_connections() {
    let mut n = Net::new();
    assert!(n.configured());
    let (l, _) = n.b.tcp_listen("0.0.0.0:7".parse().unwrap(), false, 1, &TcpOptions::default(), 0).unwrap();
    let sa = connect(&mut n, "10.0.2.2:7".parse().unwrap());
    let sb = accept(&mut n, l);
    transfer(&mut n, sa, sb, &pattern(10_000), 10_000);
    // Wi-Fi drops for five seconds, then reconnects (DHCP runs again and
    // hands out the same address).
    n.a.set_link(n.a_eth, false);
    n.dark = true;
    n.run(5_000);
    n.dark = false;
    n.a.set_link(n.a_eth, true);
    assert!(n.configured());
    transfer(&mut n, sa, sb, &pattern(20_000), 120_000);
    assert!(n.dhcp.requests_seen >= 2);
}

#[test]
fn a_new_address_aborts_old_connections() {
    let mut n = Net::new();
    assert!(n.configured());
    let (l, _) = n.b.tcp_listen("0.0.0.0:7".parse().unwrap(), false, 1, &TcpOptions::default(), 0).unwrap();
    let sa = connect(&mut n, "10.0.2.2:7".parse().unwrap());
    let _sb = accept(&mut n, l);
    n.a.set_link(n.a_eth, false);
    n.run(100);
    n.dhcp.address = Ipv4Addr::new(10, 0, 2, 16);
    n.a.set_link(n.a_eth, true);
    n.run_until(10_000, |n| matches!(n.a.tcp_view(sa).unwrap().state, TcpState::Closed { .. }));
    assert_eq!(n.a.tcp_view(sa).unwrap().state, TcpState::Closed { error: Some(NetError::NetworkDown) });
}

#[test]
fn long_outage_drops_the_address() {
    let mut n = Net::new();
    assert!(n.configured());
    n.a.set_link(n.a_eth, false);
    n.run(crate::LINK_GRACE_US / 1000 + 100);
    assert_ne!(n.a.status().connectivity, Connectivity::Routable);
    let info = n.a.interfaces().into_iter().find(|i| i.name == "eth0").unwrap();
    assert!(!info.addresses.iter().any(|a| a.address.is_ipv4()));
    assert_eq!(n.a.tcp_connect("10.0.2.2:7".parse().unwrap(), &TcpOptions::default(), 1), Err(NetError::NetworkDown));
}

#[test]
fn removing_the_interface_aborts_connections() {
    let mut n = Net::new();
    assert!(n.configured());
    let (l, _) = n.b.tcp_listen("0.0.0.0:7".parse().unwrap(), false, 1, &TcpOptions::default(), 0).unwrap();
    let sa = connect(&mut n, "10.0.2.2:7".parse().unwrap());
    let _ = accept(&mut n, l);
    n.a.remove_interface(n.a_eth);
    n.a.poll(n.now + 1000, &mut crate::NoLinks);
    assert_eq!(n.a.tcp_view(sa).unwrap().state, TcpState::Closed { error: Some(NetError::Aborted) });
    assert_eq!(n.a.status().connectivity, Connectivity::Offline);
}

#[test]
fn dns_lookups_cache_and_fail_correctly() {
    let mut n = Net::new();
    assert!(n.configured());
    match n.resolve("www.example") {
        ResolveResult::Found { addresses, ttl_s, .. } => {
            assert_eq!(addresses, vec![IpAddr::V4(SERVER_IP)]);
            assert_eq!(ttl_s, 300);
        }
        other => panic!("{other:?}"),
    }
    let before = n.dns_queries;
    assert!(matches!(n.resolve("WWW.Example."), ResolveResult::Found { .. }));
    assert_eq!(n.dns_queries, before, "second lookup should come from the cache");
    match n.resolve("alias.example") {
        ResolveResult::Found { canonical, ttl_s, .. } => {
            assert_eq!(canonical, "host.example");
            assert_eq!(ttl_s, 120);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(n.resolve("nx.example"), ResolveResult::Failed { error: NetError::NameNotFound });
    let before = n.dns_queries;
    assert_eq!(n.resolve("nx.example"), ResolveResult::Failed { error: NetError::NameNotFound });
    assert_eq!(n.dns_queries, before, "negative answers are cached");
    assert!(matches!(n.resolve("10.1.2.3"), ResolveResult::Found { .. }));
    assert!(matches!(n.resolve("localhost"), ResolveResult::Found { .. }));
    assert_eq!(n.a.resolve("bad name!", AddrFamily::Any), Err(NetError::InvalidArgument));
}

#[test]
fn dns_failures_and_failover() {
    let mut n = Net::new();
    // The first server does not exist; the second answers.
    n.dhcp.dns = vec![Ipv4Addr::new(10, 0, 2, 9), SERVER_IP];
    assert!(n.configured());
    let t0 = n.now;
    assert!(matches!(n.resolve("one.example"), ResolveResult::Found { .. }));
    assert!(n.now - t0 < 5_000_000, "failover took too long");
    n.a.flush_dns_cache();
    n.dns_mode = DnsMode::ServFail;
    assert_eq!(n.resolve("two.example"), ResolveResult::Failed { error: NetError::DnsFailure });
    n.dns_mode = DnsMode::Silent;
    let t0 = n.now;
    assert_eq!(n.resolve("three.example"), ResolveResult::Failed { error: NetError::DnsFailure });
    assert!(n.now - t0 <= crate::dns::LOOKUP_TIMEOUT_US + 10_000, "lookup must give up in time");
    // Every query socket was closed again.
    assert_eq!(n.a.socket_infos(None).len(), 0);
}

#[test]
fn udp_and_ping() {
    let mut n = Net::new();
    assert!(n.configured());
    let (sa, local) = n.a.udp_bind("0.0.0.0:0".parse().unwrap(), false, 1).unwrap();
    assert!(local.port() >= 49152);
    let (sb, _) = n.b.udp_bind("0.0.0.0:5000".parse().unwrap(), false, 0).unwrap();
    n.a.udp_send(sa, "10.0.2.2:5000".parse().unwrap(), b"hello").unwrap();
    let mut buf = [0u8; 64];
    let mut got = None;
    n.run_until(2_000, |n| {
        got = udp_recv(&mut n.b, sb, &mut buf);
        got.is_some()
    });
    let (len, from) = got.expect("datagram lost");
    assert_eq!(&buf[..len], b"hello");
    assert_eq!(from, SocketAddr::new(IpAddr::V4(CLIENT_IP), local.port()));
    // Ping the server (smoltcp answers echo requests).
    let p = n.a.ping_open(1).unwrap();
    n.a.ping_send(p, IpAddr::V4(SERVER_IP), 1, 0, 56);
    let mut events = Vec::new();
    n.run_until(2_000, |n| {
        events.extend(ping_events(&mut n.a, p));
        !events.is_empty()
    });
    assert!(matches!(events[0], PingEvent::Reply { seq: 1, size: 56, .. }), "{events:?}");
    // An unanswered ping times out.
    n.dark = true;
    n.a.ping_send(p, IpAddr::V4(SERVER_IP), 2, 0, 56);
    let mut events = Vec::new();
    n.run_until(6_000, |n| {
        events.extend(ping_events(&mut n.a, p));
        !events.is_empty()
    });
    assert_eq!(events, vec![PingEvent::Failed { seq: 2, from: None, error: NetError::TimedOut }]);
}

#[test]
fn routers_tell_ping_why_a_request_failed() {
    // A ping with a TTL of 1: the router (b) says the request ran out of
    // hops, quoting its header and just the first eight bytes of its data,
    // as RFC 792 has it (routers may quote more).
    let mut n = Net::new();
    assert!(n.configured());
    let p = n.a.ping_open(1).unwrap();
    n.recorded = Some(Vec::new());
    n.a.ping_send(p, "192.0.2.7".parse().unwrap(), 5, 1, 32);
    let find = |frames: &[Vec<u8>]| {
        frames.iter().find_map(|f| {
            let eth = EthernetFrame::new_checked(&f[..]).ok()?;
            let ip = Ipv4Packet::new_checked(eth.payload()).ok()?;
            (ip.next_header() == IpProtocol::Icmp).then(|| (eth.src_addr(), ip.payload().to_vec(), ip.dst_addr()))
        })
    };
    let mut request = None;
    n.run_until(1_000, |n| {
        request = find(n.recorded.as_deref().unwrap_or_default());
        request.is_some()
    });
    let (a_mac, icmp, dst) = request.expect("the echo request");
    let caps = ChecksumCapabilities::default();
    let error = smoltcp::wire::Icmpv4Repr::TimeExceeded {
        reason: smoltcp::wire::Icmpv4TimeExceeded::TtlExpired,
        header: Ipv4Repr {
            src_addr: CLIENT_IP,
            dst_addr: dst,
            next_header: IpProtocol::Icmp,
            payload_len: icmp.len(),
            hop_limit: 1,
        },
        data: &icmp[..8],
    };
    let ip = Ipv4Repr {
        src_addr: SERVER_IP,
        dst_addr: CLIENT_IP,
        next_header: IpProtocol::Icmp,
        payload_len: error.buffer_len(),
        hop_limit: 64,
    };
    let mut frame = vec![0u8; 14 + 20 + error.buffer_len()];
    let mut eth = EthernetFrame::new_unchecked(&mut frame[..]);
    eth.set_dst_addr(a_mac);
    eth.set_src_addr(EthernetAddress(SERVER_MAC));
    eth.set_ethertype(EthernetProtocol::Ipv4);
    ip.emit(&mut Ipv4Packet::new_unchecked(&mut frame[14..]), &caps);
    error.emit(&mut smoltcp::wire::Icmpv4Packet::new_unchecked(&mut frame[34..]), &caps);
    n.arrive_at_a(&frame);
    let mut events = Vec::new();
    n.run_until(1_000, |n| {
        events.extend(ping_events(&mut n.a, p));
        !events.is_empty()
    });
    assert_eq!(
        events,
        vec![PingEvent::Failed { seq: 5, from: Some(IpAddr::V4(SERVER_IP)), error: NetError::TimeExceeded }]
    );
}

#[test]
fn ipv6_errors_tell_ping_why_too() {
    // A ping to the far end's link-local address, taken off the cable on
    // its way: the far end says the address is unreachable, quoting it.
    let mut n = Net::new();
    assert!(n.configured());
    let b_ll = link_local(&n.b);
    let p = n.a.ping_open(1).unwrap();
    n.recorded = Some(Vec::new());
    n.a.ping_send(p, IpAddr::V6(b_ll), 9, 0, 32);
    let find = |frames: &[Vec<u8>]| {
        frames.iter().find_map(|f| {
            let eth = EthernetFrame::new_checked(&f[..]).ok()?;
            let ip = Ipv6Packet::new_checked(eth.payload()).ok()?;
            let icmp = Icmpv6Packet::new_checked(ip.payload()).ok()?;
            (ip.next_header() == IpProtocol::Icmpv6 && icmp.msg_type() == Icmpv6Message::EchoRequest)
                .then(|| (eth.src_addr(), ip.payload().to_vec(), ip.src_addr()))
        })
    };
    let mut request = None;
    n.run_until(1_000, |n| {
        request = find(n.recorded.as_deref().unwrap_or_default());
        if request.is_some() {
            // The far end never gets it (nor answers it).
            n.b_end.inbox.clear();
        }
        request.is_some()
    });
    let (a_mac, icmp, a_ll) = request.expect("the echo request");
    let caps = ChecksumCapabilities::default();
    let error = Icmpv6Repr::DstUnreachable {
        reason: Icmpv6DstUnreachable::AddrUnreachable,
        header: Ipv6Repr {
            src_addr: a_ll,
            dst_addr: b_ll,
            next_header: IpProtocol::Icmpv6,
            payload_len: icmp.len(),
            hop_limit: 64,
        },
        data: &icmp,
    };
    let ip = Ipv6Repr {
        src_addr: b_ll,
        dst_addr: a_ll,
        next_header: IpProtocol::Icmpv6,
        payload_len: error.buffer_len(),
        hop_limit: 64,
    };
    let mut frame = vec![0u8; 14 + 40 + error.buffer_len()];
    let mut eth = EthernetFrame::new_unchecked(&mut frame[..]);
    eth.set_dst_addr(a_mac);
    eth.set_src_addr(EthernetAddress(SERVER_MAC));
    eth.set_ethertype(EthernetProtocol::Ipv6);
    ip.emit(&mut Ipv6Packet::new_unchecked(&mut frame[14..]));
    error.emit(&b_ll, &a_ll, &mut Icmpv6Packet::new_unchecked(&mut frame[54..]), &caps);
    n.arrive_at_a(&frame);
    let mut events = Vec::new();
    n.run_until(1_000, |n| {
        events.extend(ping_events(&mut n.a, p));
        !events.is_empty()
    });
    assert_eq!(
        events,
        vec![PingEvent::Failed { seq: 9, from: Some(IpAddr::V6(b_ll)), error: NetError::HostUnreachable }]
    );
}

#[test]
fn loopback_connections() {
    let mut n = Net::new();
    let (l, _) = n.a.tcp_listen("127.0.0.1:8080".parse().unwrap(), false, 2, &TcpOptions::default(), 0).unwrap();
    let c = n.a.tcp_connect("127.0.0.1:8080".parse().unwrap(), &TcpOptions::default(), 1).unwrap();
    let mut acc = None;
    n.run_until(2_000, |n| {
        acc = n.a.tcp_accept(l);
        acc.is_some()
    });
    let s = acc.expect("loopback accept");
    n.run(10);
    assert!(matches!(n.a.tcp_view(c).unwrap().state, TcpState::Open { .. }));
    assert_eq!(n.a.tcp_send(c, b"ping").unwrap(), 4);
    let mut buf = [0u8; 8];
    let mut k = 0;
    n.run_until(1_000, |n| {
        k = tcp_recv(&mut n.a, s, &mut buf).unwrap();
        k > 0
    });
    assert_eq!(&buf[..k], b"ping");
}

/// Polls as the network service does: once, then again only while the
/// stack's deadline has come (the clock standing still): what crosses the
/// loopback interface must not wait for a timer. Returns the sockets the
/// stack reported ready.
fn poll_as_the_service(stack: &mut Stack, now: u64) -> Vec<SockId> {
    let mut ready = Vec::new();
    for _ in 0..1000 {
        stack.poll(now, &mut crate::NoLinks);
        ready.extend(stack.take_ready());
        if stack.next_deadline().is_none_or(|t| t > now) {
            return ready;
        }
    }
    panic!("the stack never settles");
}

/// What comes after what the service saw makes a socket ready, even when it
/// arrives first thing in a poll, before anything looked at the socket
/// again: data after what was taken, as much as before or less, and room an
/// acknowledgement frees after more was queued. (The network service moves
/// what comes as it comes, and hears of it from nothing else.)
#[test]
fn what_comes_after_the_last_taken_makes_a_socket_ready() {
    let mut n = Net::new();
    assert!(n.configured());
    let (ua, _) = n.a.udp_bind("0.0.0.0:0".parse().unwrap(), false, 1).unwrap();
    let (ub, _) = n.b.udp_bind("0.0.0.0:5000".parse().unwrap(), false, 0).unwrap();
    let to_b = SocketAddr::new(IpAddr::V4(SERVER_IP), 5000);
    let mut buf = [0u8; 8];
    // Each time, the next is on the cable when `b` takes the last.
    n.a.udp_send(ua, to_b, b"who?").unwrap();
    assert!(n.run_until(1_000, |n| n.b.take_ready().contains(&ub)), "datagram 0");
    for round in 1..4 {
        n.a.udp_send(ua, to_b, b"who?").unwrap();
        n.step();
        assert_eq!(udp_recv(&mut n.b, ub, &mut buf).map(|(k, _)| k), Some(4));
        assert!(n.run_until(1_000, |n| n.b.take_ready().contains(&ub)), "datagram {round}");
    }
    let (ta, tb) = pair(&mut n, 7);
    assert_eq!(n.a.tcp_send(ta, b"ping"), Ok(4));
    assert!(n.run_until(1_000, |n| n.b.take_ready().contains(&tb)), "bytes 0");
    for round in 1..4 {
        // Acknowledged, so that the next goes at once.
        assert!(n.run_until(1_000, |n| n.a.tcp_view(ta).unwrap().send_queued == 0));
        assert_eq!(n.a.tcp_send(ta, b"ping"), Ok(4));
        n.step();
        assert_eq!(tcp_recv(&mut n.b, tb, &mut buf), Ok(4));
        assert!(n.run_until(1_000, |n| n.b.take_ready().contains(&tb)), "bytes {round}");
    }
    // An acknowledgement on its way to `a`, and more queued before it
    // arrives.
    n.run(100);
    assert_eq!(n.a.tcp_send(ta, b"ping"), Ok(4));
    assert!(n.run_until(1_000, |n| !n.a_end.inbox.is_empty()), "no acknowledgement");
    let _ = n.a.take_ready();
    assert_eq!(n.a.tcp_send(ta, b"pingping"), Ok(8));
    n.step();
    assert!(n.a.take_ready().contains(&ta), "the room freed went unnoticed");
}

#[test]
fn loopback_traffic_waits_for_no_timer() {
    let mut s = Stack::new(b"loopback seed for tests, 32+ bytes", 1_000_000);
    let now = 1_000_000;
    // A datagram, which arrives in the poll that sends it.
    let (a, _) = s.udp_bind("127.0.0.1:0".parse().unwrap(), false, 0).unwrap();
    let (b, b_addr) = s.udp_bind("127.0.0.1:0".parse().unwrap(), false, 0).unwrap();
    poll_as_the_service(&mut s, now);
    s.udp_send(a, b_addr, b"hi").unwrap();
    assert!(poll_as_the_service(&mut s, now).contains(&b));
    let mut buf = [0u8; 8];
    assert_eq!(udp_recv(&mut s, b, &mut buf).map(|(n, _)| n), Some(2));
    // A connection, made and used while the clock stands still.
    let (l, _) = s.tcp_listen("127.0.0.1:8080".parse().unwrap(), false, 2, &TcpOptions::default(), 0).unwrap();
    let c = s.tcp_connect("127.0.0.1:8080".parse().unwrap(), &TcpOptions::default(), 1).unwrap();
    poll_as_the_service(&mut s, now);
    let server = s.tcp_accept(l).expect("the connection is accepted at once");
    assert!(matches!(s.tcp_view(c).unwrap().state, TcpState::Open { .. }));
    assert_eq!(s.tcp_send(c, b"ping").unwrap(), 4);
    assert!(poll_as_the_service(&mut s, now).contains(&server));
    assert_eq!(tcp_recv(&mut s, server, &mut buf), Ok(4));
    assert_eq!(&buf[..4], b"ping");
}

#[test]
fn routes_prefer_wired_interfaces() {
    let mut n = Net::new();
    assert!(n.configured());
    let wlan = DeviceInfo { kind: InterfaceKind::Wireless, mac: [2, 0, 0, 0, 0, 0x77], ..device(CLIENT_MAC) };
    let w = n.a.add_interface(&wlan, true).unwrap();
    n.a.configure(
        w,
        IpConfig::Static {
            address: "192.168.1.5".parse().unwrap(),
            prefix_len: 24,
            gateway: Some("192.168.1.1".parse().unwrap()),
            dns: vec![],
        },
    )
    .unwrap();
    n.run(10);
    assert_eq!(n.a.status().default_interface, "eth0");
    assert_eq!(n.a.route("8.8.8.8".parse().unwrap()), Ok(n.a_eth));
    // Directly connected networks win over the default route.
    assert_eq!(n.a.route("192.168.1.77".parse().unwrap()), Ok(w));
    n.a.set_link(n.a_eth, false);
    n.run(10);
    assert_eq!(n.a.status().default_interface, "wlan0");
    assert_eq!(n.a.route("8.8.8.8".parse().unwrap()), Ok(w));
}

#[test]
fn hostile_frames_do_not_crash_the_stack() {
    let mut n = Net::new();
    assert!(n.configured());
    let mut rng = Rng(42);
    let mut frame = vec![0u8; 1514];
    for round in 0..20_000 {
        let len = (rng.next() % 1514) as usize;
        for b in frame[..len].iter_mut() {
            *b = rng.next() as u8;
        }
        // Make many of them look like IPv4/IPv6/ARP to reach deeper code.
        if len > 14 {
            frame[0..6].copy_from_slice(&CLIENT_MAC);
            let ty: [u8; 2] = match round % 4 {
                0 => [0x08, 0x00],
                1 => [0x86, 0xDD],
                2 => [0x08, 0x06],
                _ => [frame[12], frame[13]],
            };
            frame[12..14].copy_from_slice(&ty);
        }
        n.arrive_at_a(&frame[..len]);
        if round % 64 == 0 {
            n.step();
        }
    }
    n.run(100);
    assert!(matches!(n.resolve("still.example"), ResolveResult::Found { .. }));
}

#[test]
fn bad_requests_are_rejected() {
    let mut n = Net::new();
    assert!(n.configured());
    let o = TcpOptions::default();
    assert_eq!(n.a.tcp_connect("10.0.2.2:0".parse().unwrap(), &o, 1), Err(NetError::InvalidArgument));
    assert_eq!(n.a.tcp_connect("0.0.0.0:80".parse().unwrap(), &o, 1), Err(NetError::InvalidArgument));
    assert_eq!(n.a.tcp_connect("224.0.0.1:80".parse().unwrap(), &o, 1), Err(NetError::InvalidArgument));
    assert_eq!(n.a.udp_bind("192.0.2.1:1".parse().unwrap(), false, 1), Err(NetError::AddressNotAvailable));
    let (_, _) = n.a.udp_bind("0.0.0.0:6000".parse().unwrap(), false, 1).unwrap();
    assert_eq!(n.a.udp_bind("0.0.0.0:6000".parse().unwrap(), false, 1), Err(NetError::AddressInUse));
    assert!(
        n.a.configure(
            n.a_eth,
            IpConfig::Static { address: "10.0.0.1".parse().unwrap(), prefix_len: 40, gateway: None, dns: vec![] }
        )
        .is_err()
    );
    assert_eq!(n.a.add_interface(&device([1, 2, 3, 4, 5, 6]), true), Err(proto::net::NetdevError::Invalid));
}

#[test]
fn socket_limit_is_enforced() {
    let mut n = Net::new();
    let mut ids = Vec::new();
    loop {
        match n.a.ping_open(1) {
            Ok(id) => ids.push(id),
            Err(e) => {
                assert_eq!(e, NetError::LimitReached);
                break;
            }
        }
        assert!(ids.len() <= crate::sockets::MAX_SOCKETS);
    }
    for id in ids {
        n.a.close(id);
    }
    assert!(n.a.ping_open(1).is_ok());
}

/// Connects `a` to a listener on `b` (port `port`, `backlog`): (a's
/// socket, b's).
fn pair(n: &mut Net, port: u16) -> (SockId, SockId) {
    let local: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port);
    let (l, _) = n.b.tcp_listen(local, false, 4, &TcpOptions::default(), 0).unwrap();
    let sa = connect(n, SocketAddr::new(IpAddr::V4(SERVER_IP), port));
    let sb = accept(n, l);
    (sa, sb)
}

/// Sends all of `data` from `from` on `stack` (stepping the cable).
fn send_all(n: &mut Net, on_a: bool, id: SockId, data: &[u8]) {
    let mut sent = 0;
    for _ in 0..60_000 {
        if sent == data.len() {
            return;
        }
        let stack = if on_a { &mut n.a } else { &mut n.b };
        sent += stack.tcp_send(id, &data[sent..]).unwrap();
        n.step();
    }
    panic!("could not send {} bytes", data.len());
}

#[test]
fn idle_connections_outlive_the_user_timeout() {
    // Nothing waits for an acknowledgement: two quiet minutes and more are
    // no reason to give up on the peer.
    let mut n = Net::new();
    assert!(n.configured());
    let (sa, sb) = pair(&mut n, 7);
    transfer(&mut n, sa, sb, &pattern(1000), 5_000);
    n.run(150_000);
    assert!(matches!(n.a.tcp_view(sa).unwrap().state, TcpState::Open { .. }), "{:?}", n.a.tcp_view(sa));
    assert!(matches!(n.b.tcp_view(sb).unwrap().state, TcpState::Open { .. }), "{:?}", n.b.tcp_view(sb));
    transfer(&mut n, sa, sb, &pattern(5000), 10_000);
}

#[test]
fn the_end_of_stream_follows_the_fin_and_the_data_before_it() {
    let mut n = Net::new();
    assert!(n.configured());
    let (sa, sb) = pair(&mut n, 7);
    let data = pattern(10_000);
    send_all(&mut n, false, sb, &data);
    n.b.tcp_shutdown(sb);
    // a reads nothing until everything (and the FIN) is there.
    assert!(n.run_until(5_000, |n| {
        let v = n.a.tcp_view(sa).unwrap();
        v.recv_queued == data.len() && n.a.socket_infos(None).iter().any(|i| i.state == "CLOSE-WAIT")
    }));
    assert!(!n.a.tcp_view(sa).unwrap().eof, "data before the end of stream");
    let mut got = Vec::new();
    let mut buf = vec![0u8; 4096];
    while let Ok(k) = tcp_recv(&mut n.a, sa, &mut buf) {
        if k == 0 {
            break;
        }
        got.extend_from_slice(&buf[..k]);
    }
    assert!(got == data);
    // The end of stream shows as soon as the data is taken.
    assert!(n.a.tcp_view(sa).unwrap().eof);
}

#[test]
fn data_received_before_a_normal_close_stays_readable() {
    // a finishes sending first; b answers and closes too. a's client is
    // slow: by the time it reads, TIME-WAIT is over and the connection is
    // closed, but what arrived is still there, then the end of stream.
    let mut n = Net::new();
    assert!(n.configured());
    let (sa, sb) = pair(&mut n, 7);
    n.a.tcp_shutdown(sa);
    assert!(n.run_until(5_000, |n| n.b.tcp_view(sb).unwrap().eof));
    let data = pattern(20_000);
    send_all(&mut n, false, sb, &data);
    n.b.tcp_shutdown(sb);
    n.run(15_000);
    let v = n.a.tcp_view(sa).unwrap();
    assert_eq!(v.state, TcpState::Closed { error: None });
    assert_eq!(v.recv_queued, data.len());
    assert!(!v.eof);
    let mut got = Vec::new();
    let mut buf = vec![0u8; 3000];
    while let Ok(k) = tcp_recv(&mut n.a, sa, &mut buf) {
        if k == 0 {
            break;
        }
        got.extend_from_slice(&buf[..k]);
    }
    assert!(got == data);
    assert!(n.a.tcp_view(sa).unwrap().eof);
}

#[test]
fn closing_delivers_what_was_sent_then_a_fin() {
    let mut n = Net::new();
    assert!(n.configured());
    let (sa, sb) = pair(&mut n, 7);
    let data = pattern(60_000);
    let mut sent = 0;
    while sent < data.len() {
        let k = n.a.tcp_send(sa, &data[sent..]).unwrap();
        assert!(k > 0, "the send buffer takes 64 KiB");
        sent += k;
    }
    // The client goes away at once; the connection finishes on its own.
    n.a.close(sa);
    let mut got = Vec::new();
    let mut buf = vec![0u8; 8192];
    assert!(n.run_until(20_000, |n| {
        while let Ok(k) = tcp_recv(&mut n.b, sb, &mut buf) {
            if k == 0 {
                break;
            }
            got.extend_from_slice(&buf[..k]);
        }
        n.b.tcp_view(sb).unwrap().eof
    }));
    assert!(got == data, "{} of {} bytes", got.len(), data.len());
}

#[test]
fn orphans_that_receive_data_are_reset() {
    // b's client closes the connection while a keeps sending: nobody will
    // read that, so a is told (RFC 1122).
    let mut n = Net::new();
    assert!(n.configured());
    let (sa, sb) = pair(&mut n, 7);
    n.b.close(sb);
    n.run(100);
    let _ = n.a.tcp_send(sa, b"more for nobody");
    assert!(n.run_until(5_000, |n| matches!(n.a.tcp_view(sa).unwrap().state, TcpState::Closed { .. })));
    assert_eq!(n.a.tcp_view(sa).unwrap().state, TcpState::Closed { error: Some(NetError::ConnectionReset) });
    n.run(100);
    assert!(n.b.socket_infos(None).iter().all(|i| i.local.is_none_or(|l| l.port() != 7) || i.state == "LISTEN"));
}

#[test]
fn orphans_wait_for_the_peer_only_so_long() {
    // a's client closes; b acknowledges the FIN but never closes its side.
    // a gives up after the FIN timeout (and resets the connection).
    let mut n = Net::new();
    assert!(n.configured());
    let (sa, sb) = pair(&mut n, 7);
    n.a.close(sa);
    assert!(n.run_until(5_000, |n| n.b.tcp_view(sb).unwrap().eof));
    let a_has = |n: &Net| n.a.socket_infos(None).iter().any(|i| i.kind == proto::net::SocketKind::Tcp);
    assert!(a_has(&n), "FIN-WAIT-2");
    n.run(crate::ORPHAN_TIMEOUT_US / 1000 - 2_000);
    assert!(a_has(&n), "gone too early");
    n.run(4_000);
    assert!(!a_has(&n), "an orphan outlived the FIN timeout");
    assert!(n.run_until(2_000, |n| matches!(n.b.tcp_view(sb).unwrap().state, TcpState::Closed { .. })));
    assert_eq!(n.b.tcp_view(sb).unwrap().state, TcpState::Closed { error: Some(NetError::ConnectionReset) });
}

#[test]
fn keep_alives_need_at_least_a_second() {
    let mut n = Net::new();
    assert!(n.configured());
    let to: SocketAddr = "10.0.2.2:7".parse().unwrap();
    let opts = |ms| TcpOptions { keepalive_ms: ms, ..TcpOptions::default() };
    assert_eq!(n.a.tcp_connect(to, &opts(1), 1), Err(NetError::InvalidArgument));
    assert_eq!(n.a.tcp_connect(to, &opts(999), 1), Err(NetError::InvalidArgument));
    assert_eq!(
        n.a.tcp_listen("0.0.0.0:8".parse().unwrap(), false, 1, &opts(10), 1).map(|_| ()),
        Err(NetError::InvalidArgument)
    );
    assert!(n.a.tcp_connect(to, &opts(1000), 1).is_ok());
    assert!(n.a.tcp_connect(to, &opts(0), 1).is_ok());
}

#[test]
fn unanswered_keep_alives_time_the_connection_out() {
    // The peer vanishes from a quiet connection: keep-alive finds out, and
    // the connection timed out (it was not reset).
    let mut n = Net::new();
    assert!(n.configured());
    let (l, _) = n.b.tcp_listen("0.0.0.0:7".parse().unwrap(), false, 4, &TcpOptions::default(), 0).unwrap();
    let opts = TcpOptions { keepalive_ms: 1000, ..TcpOptions::default() };
    let sa = n.a.tcp_connect("10.0.2.2:7".parse().unwrap(), &opts, 1).unwrap();
    let _sb = accept(&mut n, l);
    n.run(5_000);
    assert!(matches!(n.a.tcp_view(sa).unwrap().state, TcpState::Open { .. }));
    n.dark = true;
    n.run_until(200_000, |n| matches!(n.a.tcp_view(sa).unwrap().state, TcpState::Closed { .. }));
    assert_eq!(n.a.tcp_view(sa).unwrap().state, TcpState::Closed { error: Some(NetError::TimedOut) });
}

#[test]
fn a_reset_after_our_half_close_is_no_end_of_stream() {
    // The client finished sending and reads on; the peer resets the
    // connection instead of finishing it: an error, not the end of the
    // stream.
    let mut n = Net::new();
    assert!(n.configured());
    let (sa, sb) = pair(&mut n, 7);
    n.a.tcp_shutdown(sa);
    n.run(100);
    n.b.tcp_abort(sb);
    n.run_until(1_000, |n| matches!(n.a.tcp_view(sa).unwrap().state, TcpState::Closed { .. }));
    let v = n.a.tcp_view(sa).unwrap();
    assert_eq!(v.state, TcpState::Closed { error: Some(NetError::ConnectionReset) });
    assert!(!v.eof);
}

#[test]
fn a_listener_holds_at_most_its_backlog() {
    // A client that does not accept: connections beyond its backlog are
    // refused instead of piling up.
    let mut n = Net::new();
    assert!(n.configured());
    let (l, _) = n.b.tcp_listen("0.0.0.0:7".parse().unwrap(), false, 2, &TcpOptions::default(), 0).unwrap();
    let to: SocketAddr = "10.0.2.2:7".parse().unwrap();
    let mut conns = Vec::new();
    for _ in 0..6 {
        // One after the other, each given time to be established.
        let c = n.a.tcp_connect(to, &TcpOptions::default(), 1).unwrap();
        n.run_until(1_000, |n| !matches!(n.a.tcp_view(c).unwrap().state, TcpState::Connecting));
        conns.push(c);
    }
    n.run(100);
    let open = conns.iter().filter(|&&c| matches!(n.a.tcp_view(c).unwrap().state, TcpState::Open { .. })).count();
    let refused = conns
        .iter()
        .filter(|&&c| n.a.tcp_view(c).unwrap().state == TcpState::Closed { error: Some(NetError::ConnectionRefused) })
        .count();
    assert_eq!((open, refused), (2, 4));
    let waiting = n.b.socket_infos(None).iter().filter(|i| i.kind == proto::net::SocketKind::Tcp).count();
    assert_eq!(waiting, 2);
    // Accepting makes room again.
    assert!(n.b.tcp_accept(l).is_some());
    let c = n.a.tcp_connect(to, &TcpOptions::default(), 1).unwrap();
    assert!(n.run_until(3_000, |n| matches!(n.a.tcp_view(c).unwrap().state, TcpState::Open { .. })));
}

#[test]
fn accepted_connections_respect_the_socket_limit() {
    let mut n = Net::new();
    assert!(n.configured());
    let (l, _) = n.b.tcp_listen("0.0.0.0:7".parse().unwrap(), false, 4, &TcpOptions::default(), 0).unwrap();
    while n.b.ping_open(0).is_ok() {}
    let c = n.a.tcp_connect("10.0.2.2:7".parse().unwrap(), &TcpOptions::default(), 1).unwrap();
    assert!(n.run_until(3_000, |n| matches!(n.a.tcp_view(c).unwrap().state, TcpState::Closed { .. })));
    assert_eq!(n.a.tcp_view(c).unwrap().state, TcpState::Closed { error: Some(NetError::ConnectionReset) });
    assert_eq!(n.b.tcp_accept(l), None);
    n.run(10);
    assert!(n.b.socket_infos(None).len() <= crate::MAX_SOCKETS);
}

#[test]
fn lookups_need_no_room_in_the_socket_table() {
    let mut n = Net::new();
    assert!(n.configured());
    while n.a.ping_open(1).is_ok() {}
    assert!(matches!(n.resolve("full.example"), ResolveResult::Found { .. }));
    // Only the pings are listed: the resolver's sockets are its own.
    assert!(n.a.socket_infos(None).iter().all(|i| i.kind == proto::net::SocketKind::Ping));
}

#[test]
fn unreachable_dns_servers_are_passed_over_at_once() {
    let mut n = Net::new();
    assert!(n.configured());
    // No IPv6 route: the first server cannot be sent to at all.
    n.a.set_dns_servers(vec!["2001:db8::53".parse().unwrap(), IpAddr::V4(SERVER_IP)]).unwrap();
    let t0 = n.now;
    assert!(matches!(n.resolve("quick.example"), ResolveResult::Found { .. }));
    assert!(n.now - t0 < 500_000, "the lookup waited {} µs", n.now - t0);
    // Without any server that can be sent to, it fails at once too.
    n.a.set_dns_servers(vec!["2001:db8::53".parse().unwrap()]).unwrap();
    let t0 = n.now;
    assert!(matches!(n.resolve("none.example"), ResolveResult::Failed { .. }));
    assert!(n.now - t0 < 500_000, "the lookup waited {} µs", n.now - t0);
}

#[test]
fn static_configurations_are_checked() {
    let mut n = Net::new();
    let cfg = |dns: Vec<IpAddr>, gateway: Option<IpAddr>| IpConfig::Static {
        address: "10.0.2.15".parse().unwrap(),
        prefix_len: 24,
        gateway,
        dns,
    };
    let bad: [IpAddr; 3] =
        ["0.0.0.0".parse().unwrap(), "224.0.0.1".parse().unwrap(), "255.255.255.255".parse().unwrap()];
    for d in bad {
        assert_eq!(n.a.configure(n.a_eth, cfg(vec![d], None)), Err(NetError::InvalidArgument), "{d}");
        assert_eq!(n.a.configure(n.a_eth, cfg(vec![], Some(d))), Err(NetError::InvalidArgument), "{d}");
        assert_eq!(n.a.set_dns_servers(vec![d]), Err(NetError::InvalidArgument), "{d}");
    }
    let many: Vec<IpAddr> = (1..=9).map(|k| IpAddr::V4(Ipv4Addr::new(10, 0, 0, k))).collect();
    assert_eq!(n.a.configure(n.a_eth, cfg(many, None)), Err(NetError::InvalidArgument));
    assert!(n.a.configure(n.a_eth, cfg(vec![IpAddr::V4(SERVER_IP)], Some(IpAddr::V4(SERVER_IP)))).is_ok());
}

#[test]
fn the_lease_is_the_one_the_client_accepted() {
    // Another client's acknowledgement, broadcast as servers do, must not
    // change ours.
    let mut n = Net::new();
    n.dhcp.forged_lease = Some(5);
    assert!(n.configured());
    let info = n.a.interfaces().into_iter().find(|i| i.name == "eth0").unwrap();
    assert_eq!(info.dhcp.lease_s, 3600);
    let v4 = info.addresses.iter().find(|a| a.address.is_ipv4()).unwrap();
    assert!((3590..=3600).contains(&v4.valid_s), "{}", v4.valid_s);
}

#[test]
fn a_full_link_holds_frames_back() {
    // The device takes two frames per step: the stack waits for room
    // rather than dropping what it has to send.
    let mut n = Net::new();
    assert!(n.configured());
    let (sa, sb) = pair(&mut n, 7);
    n.a_end.room = 2;
    let mut blocked = false;
    let data = pattern(200_000);
    let mut sent = 0;
    let mut got = Vec::new();
    let mut buf = vec![0u8; 8192];
    for _ in 0..120_000 {
        if sent < data.len() {
            sent += n.a.tcp_send(sa, &data[sent..]).unwrap();
        }
        n.step();
        blocked |= n.a.link_blocked(n.a_eth);
        while let Ok(k) = tcp_recv(&mut n.b, sb, &mut buf) {
            if k == 0 {
                break;
            }
            got.extend_from_slice(&buf[..k]);
        }
        if got.len() == data.len() {
            break;
        }
    }
    assert!(got == data, "{} of {} bytes", got.len(), data.len());
    assert!(blocked && n.a_end.full == 0, "the stack must not send into a full link");
    let stats = n.a.interfaces().into_iter().find(|i| i.name == "eth0").unwrap().stats;
    assert_eq!(stats.tx_dropped, 0);
}

#[test]
fn sockets_are_ready_when_something_changes() {
    let mut n = Net::new();
    assert!(n.configured());
    let (sa, sb) = pair(&mut n, 7);
    n.run(100);
    let _ = n.a.take_ready();
    n.run(100);
    assert!(n.a.take_ready().is_empty(), "nothing happened");
    send_all(&mut n, false, sb, b"news");
    assert!(n.run_until(1_000, |n| n.a.take_ready().contains(&sa)));
}

#[test]
fn lookups_survive_the_loss_of_an_interface() {
    let mut n = Net::new();
    assert!(n.configured());
    n.dns_mode = DnsMode::Silent;
    let id = n.a.resolve("lost.example", AddrFamily::Any).unwrap();
    n.run(10);
    n.a.remove_interface(n.a_eth);
    // A new interface takes the slot of the old one.
    let again = n.a.add_interface(&device(CLIENT_MAC), true).unwrap();
    assert_eq!(again, n.a_eth);
    let r = (0..20_000).find_map(|_| {
        n.step();
        n.a.resolve_result(id)
    });
    assert!(matches!(r, Some(ResolveResult::Failed { .. })), "{r:?}");
}

#[test]
fn received_data_leaves_the_stack_only_as_far_as_it_is_taken() {
    // The service hands data to a client whose queue may be full: what it
    // could not pass on stays in the stack (and TCP's window closes).
    let mut n = Net::new();
    assert!(n.configured());
    let (sa, sb) = pair(&mut n, 7);
    send_all(&mut n, false, sb, &pattern(5000));
    assert!(n.run_until(2_000, |n| n.a.tcp_view(sa).unwrap().recv_queued == 5000));
    // Offered, refused: nothing dequeued.
    assert_eq!(
        n.a.tcp_recv_with(sa, 1000, |data| {
            assert_eq!(data.len(), 1000);
            0
        }),
        Ok(0)
    );
    assert_eq!(n.a.tcp_view(sa).unwrap().recv_queued, 5000);
    // Partly taken: the rest stays, in order.
    assert_eq!(n.a.tcp_recv_with(sa, 4096, |_| 100), Ok(100));
    let mut buf = vec![0u8; 8192];
    let k = tcp_recv(&mut n.a, sa, &mut buf).unwrap();
    assert_eq!(&buf[..k], &pattern(5000)[100..100 + k]);
    // UDP: a datagram not taken is offered again.
    let (ua, _) = n.a.udp_bind("0.0.0.0:0".parse().unwrap(), false, 1).unwrap();
    let (ub, _) = n.b.udp_bind("0.0.0.0:5001".parse().unwrap(), false, 0).unwrap();
    n.a.udp_send(ua, "10.0.2.2:5001".parse().unwrap(), b"datagram").unwrap();
    assert!(n.run_until(2_000, |n| n.b.udp_recv_with(ub, |_, _| false) == Some(false)));
    assert_eq!(n.b.udp_recv_with(ub, |_, data| data == b"datagram"), Some(true));
    assert_eq!(n.b.udp_recv_with(ub, |_, _| true), None);
}

/// b, behind a firewall of the default settings, as `change` makes them.
fn guard_b(n: &mut Net, change: impl FnOnce(&mut FirewallConfig)) {
    let mut c = FirewallConfig::default();
    change(&mut c);
    n.b.set_firewall(c);
}

#[test]
fn the_firewall_keeps_out_connections_nobody_allowed() {
    let mut n = Net::new();
    assert!(n.configured());
    guard_b(&mut n, |_| {});
    let (l, _) = n.b.tcp_listen("0.0.0.0:7".parse().unwrap(), false, 4, &TcpOptions::default(), 0).unwrap();
    let s = n.a.tcp_connect("10.0.2.2:7".parse().unwrap(), &TcpOptions::default(), 1).unwrap();
    n.run(2_000);
    assert_eq!(n.a.tcp_view(s).unwrap().state, TcpState::Connecting);
    assert!(n.b.tcp_accept(l).is_none());
    assert!(n.b.firewall_stats().not_allowed >= 1);
    // Open to the local network: the SYN sent again gets through.
    n.b.set_exposure(l, Some(Scope::LocalNetwork {}), "echo");
    assert!(n.run_until(6_000, |n| matches!(n.a.tcp_view(s).unwrap().state, TcpState::Open { .. })));
    accept(&mut n, l);
    assert!(n.b.firewall_log().any(|e| e.event.port == 7 && e.event.reason == FirewallReason::NotAllowed));
}

#[test]
fn closed_ports_refuse_unless_stealth_hides_them() {
    let mut n = Net::new();
    assert!(n.configured());
    guard_b(&mut n, |c| c.stealth = false);
    let s = n.a.tcp_connect("10.0.2.2:9".parse().unwrap(), &TcpOptions::default(), 1).unwrap();
    n.run_until(3_000, |n| matches!(n.a.tcp_view(s).unwrap().state, TcpState::Closed { .. }));
    assert_eq!(n.a.tcp_view(s).unwrap().state, TcpState::Closed { error: Some(NetError::ConnectionRefused) });
    // In stealth mode, no reset: the machine does not show itself.
    guard_b(&mut n, |_| {});
    let s = n.a.tcp_connect("10.0.2.2:9".parse().unwrap(), &TcpOptions::default(), 1).unwrap();
    n.run(3_000);
    assert_eq!(n.a.tcp_view(s).unwrap().state, TcpState::Connecting);
    assert!(n.b.firewall_stats().probes >= 1);
}

#[test]
fn what_this_machine_asked_for_comes_back_through_its_firewall() {
    let mut n = Net::new();
    // a behind a firewall from the start: DHCP's answers, and DNS's, come in.
    n.a.set_firewall(FirewallConfig::default());
    assert!(n.configured());
    assert!(matches!(n.resolve("www.example"), ResolveResult::Found { .. }));
    let (sa, _) = n.a.udp_bind("0.0.0.0:0".parse().unwrap(), false, 1).unwrap();
    let (sb, _) = n.b.udp_bind("0.0.0.0:5000".parse().unwrap(), false, 0).unwrap();
    n.a.udp_send(sa, "10.0.2.2:5000".parse().unwrap(), b"question").unwrap();
    let mut buf = [0u8; 64];
    let mut got = None;
    n.run_until(2_000, |n| {
        got = udp_recv(&mut n.b, sb, &mut buf);
        got.is_some()
    });
    let (_, from) = got.expect("the question never came");
    n.b.udp_send(sb, from, b"answer").unwrap();
    let mut back = None;
    n.run_until(2_000, |n| {
        back = udp_recv(&mut n.a, sa, &mut buf);
        back.is_some()
    });
    assert_eq!(back.map(|(len, _)| &buf[..len]), Some(&b"answer"[..]));
    // What nobody asked for, to another of a's ports, does not come in.
    let (sc, port) = n.a.udp_bind("0.0.0.0:0".parse().unwrap(), false, 1).unwrap();
    n.b.udp_send(sb, SocketAddr::new(IpAddr::V4(CLIENT_IP), port.port()), b"unasked").unwrap();
    n.run(500);
    assert!(udp_recv(&mut n.a, sc, &mut buf).is_none());
    assert!(n.a.firewall_stats().not_allowed >= 1);
    // a's own connections to b work as ever.
    let (_, _) = pair(&mut n, 8);
}

#[test]
fn pings_are_answered_only_from_where_allowed() {
    let mut n = Net::new();
    assert!(n.configured());
    guard_b(&mut n, |_| {});
    let p = n.a.ping_open(1).unwrap();
    n.a.ping_send(p, IpAddr::V4(SERVER_IP), 1, 0, 56);
    let mut events = Vec::new();
    n.run_until(6_000, |n| {
        events.extend(ping_events(&mut n.a, p));
        !events.is_empty()
    });
    assert!(matches!(events[0], PingEvent::Failed { seq: 1, error: NetError::TimedOut, .. }), "{events:?}");
    guard_b(&mut n, |c| c.ping = Some(Scope::LocalNetwork {}));
    n.a.ping_send(p, IpAddr::V4(SERVER_IP), 2, 0, 56);
    let mut events = Vec::new();
    n.run_until(2_000, |n| {
        events.extend(ping_events(&mut n.a, p));
        !events.is_empty()
    });
    assert!(matches!(events[0], PingEvent::Reply { seq: 2, .. }), "{events:?}");
}
