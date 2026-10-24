// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The stub resolver: turns host names into addresses by asking the
//! configured DNS servers, and caches the answers.
//!
//! * Each query goes out from a fresh UDP socket with a random source port
//!   and a random 16-bit id; answers are accepted only from the server
//!   asked, with the same id and an echo of the question, which makes blind
//!   spoofing impractical. The socket is the resolver's own: small, on the
//!   interface the server is reached through, and not among the sockets of
//!   applications (it counts against no limit of theirs, and no listing
//!   shows it).
//! * Servers are tried in turn with growing timeouts; a server that answers
//!   SERVFAIL or REFUSED is skipped at once, and so is one that cannot be
//!   sent to (no route). A lookup gives up after [`LOOKUP_TIMEOUT_US`].
//! * CNAME chains inside an answer are followed (at most
//!   [`MAX_CNAME_CHAIN`] links); the cached lifetime is the shortest TTL on
//!   the chain, capped at a day. "Name not found" answers are cached too
//!   (RFC 2308), using the SOA minimum TTL.
//! * Messages are parsed with smoltcp's DNS wire code, which rejects
//!   compression loops; every length is checked.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::net::{IpAddr, SocketAddr};

use proto::net::{AddrFamily, NetError, ResolveResult};
use smoltcp::iface::SocketHandle;
use smoltcp::socket::udp;
use smoltcp::wire::{DnsPacket, DnsQuestion, DnsRcode, DnsRecord, DnsRecordData, IpEndpoint, IpListenEndpoint};

use crate::sockets::Proto;
use crate::{IfaceId, Stack};

/// Identifies a lookup in progress.
pub type ResolveId = u32;

/// Longest host name (RFC 1035: 253 characters in text form).
pub const MAX_NAME_LEN: usize = 253;
/// Give up on a lookup after this long.
pub const LOOKUP_TIMEOUT_US: u64 = 10_000_000;
/// Wait this long for the first answer from a server (doubling per round).
const FIRST_TIMEOUT_US: u64 = 1_500_000;
/// Rounds through the server list.
const ROUNDS: usize = 2;
const MAX_CNAME_CHAIN: usize = 8;
const MAX_CACHE: usize = 512;
const MAX_TTL_S: u32 = 86_400;
const DEFAULT_NEGATIVE_TTL_S: u32 = 60;
const MAX_NEGATIVE_TTL_S: u32 = 3_600;
/// UDP payload size we advertise with EDNS(0) (the DNS flag day value).
const EDNS_PAYLOAD: u16 = 1232;
const TYPE_A: u16 = 1;
const TYPE_AAAA: u16 = 28;
const TYPE_OPT: u16 = 41;
/// Most lookups in progress, of all clients together.
pub const MAX_PENDING_LOOKUPS: usize = 256;
/// Most DNS servers (configured or overriding).
pub const MAX_SERVERS: usize = 8;
/// A query socket's buffers: answers are at most [`EDNS_PAYLOAD`] bytes
/// (larger ones do not fit, and the next server is asked), queries far
/// less.
const QUERY_RX_PACKETS: usize = 4;
const QUERY_RX_BYTES: usize = 4 * EDNS_PAYLOAD as usize;
const QUERY_TX_PACKETS: usize = 1;
const QUERY_TX_BYTES: usize = 512;

/// Whether `servers` can be the DNS servers: a few unicast addresses.
pub(crate) fn valid_servers(servers: &[IpAddr]) -> bool {
    servers.len() <= MAX_SERVERS
        && servers
            .iter()
            .all(|s| !s.is_unspecified() && !s.is_multicast() && !matches!(s, IpAddr::V4(a) if a.is_broadcast()))
}

/// Checks a host name and returns it in canonical form (lower case, no
/// trailing dot).
pub fn validate_name(name: &str) -> Result<String, NetError> {
    let name = name.strip_suffix('.').unwrap_or(name);
    if name.is_empty() || name.len() > MAX_NAME_LEN {
        return Err(NetError::InvalidArgument);
    }
    for label in name.split('.') {
        let ok = !label.is_empty()
            && label.len() <= 63
            && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            && !label.starts_with('-')
            && !label.ends_with('-');
        if !ok {
            return Err(NetError::InvalidArgument);
        }
    }
    Ok(name.to_ascii_lowercase())
}

/// What one query learned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Answer {
    Addresses {
        addrs: Vec<IpAddr>,
        ttl_s: u32,
        canonical: String,
    },
    /// The name does not exist.
    NxDomain {
        ttl_s: u32,
    },
    /// The name exists but has no records of this type.
    NoData {
        ttl_s: u32,
    },
    Failed(NetError),
}

/// One query sent to one server, on a socket of the resolver's own.
#[derive(Debug)]
struct Attempt {
    iface: IfaceId,
    handle: SocketHandle,
    port: u16,
    server: SocketAddr,
    txid: u16,
    timeout_at_us: u64,
}

#[derive(Debug)]
struct Query {
    qtype: u16,
    attempt: Option<Attempt>,
    tries: usize,
    answer: Option<Answer>,
}

#[derive(Debug)]
struct Lookup {
    name: String,
    queries: Vec<Query>,
    deadline_us: u64,
}

#[derive(Debug, Clone)]
struct CacheEntry {
    answer: Answer,
    expires_us: u64,
}

/// Resolver state (owned by the [`Stack`]).
pub(crate) struct Resolver {
    cache: BTreeMap<(String, u16), CacheEntry>,
    lookups: BTreeMap<ResolveId, Lookup>,
    results: BTreeMap<ResolveId, ResolveResult>,
    next: ResolveId,
}

impl Resolver {
    pub fn new() -> Resolver {
        Resolver { cache: BTreeMap::new(), lookups: BTreeMap::new(), results: BTreeMap::new(), next: 1 }
    }

    pub fn flush(&mut self) {
        self.cache.clear();
    }

    fn new_id(&mut self) -> ResolveId {
        let id = self.next;
        self.next = self.next.wrapping_add(1).max(1);
        id
    }

    /// The earliest timer.
    pub fn deadline(&self) -> Option<u64> {
        let mut best: Option<u64> = None;
        for l in self.lookups.values() {
            for q in &l.queries {
                if let Some(a) = &q.attempt {
                    best = Some(best.map_or(a.timeout_at_us, |b: u64| b.min(a.timeout_at_us)));
                }
            }
            best = Some(best.map_or(l.deadline_us, |b| b.min(l.deadline_us)));
        }
        if !self.results.is_empty() {
            best = Some(0);
        }
        best
    }

    fn cached(&self, name: &str, qtype: u16, now: u64) -> Option<Answer> {
        self.cache.get(&(String::from(name), qtype)).filter(|e| e.expires_us > now).map(|e| e.answer.clone())
    }

    fn store(&mut self, name: &str, qtype: u16, answer: &Answer, now: u64) {
        let ttl = match answer {
            Answer::Addresses { ttl_s, .. } => (*ttl_s).min(MAX_TTL_S),
            Answer::NxDomain { ttl_s } | Answer::NoData { ttl_s } => (*ttl_s).min(MAX_NEGATIVE_TTL_S),
            Answer::Failed(_) => return,
        };
        if ttl == 0 {
            return;
        }
        if self.cache.len() >= MAX_CACHE {
            self.cache.retain(|_, e| e.expires_us > now);
            if self.cache.len() >= MAX_CACHE
                && let Some(k) = self.cache.iter().min_by_key(|(_, e)| e.expires_us).map(|(k, _)| k.clone())
            {
                self.cache.remove(&k);
            }
        }
        self.cache.insert(
            (String::from(name), qtype),
            CacheEntry { answer: answer.clone(), expires_us: now + ttl as u64 * 1_000_000 },
        );
    }
}

/// Combines the answers to the queries of one lookup.
fn combine(name: &str, answers: &[Answer]) -> ResolveResult {
    let mut addrs = Vec::new();
    let mut ttl = u32::MAX;
    let mut canonical = String::new();
    let mut not_found = false;
    let mut failure = None;
    for a in answers {
        match a {
            Answer::Addresses { addrs: list, ttl_s, canonical: c } => {
                addrs.extend_from_slice(list);
                ttl = ttl.min(*ttl_s);
                if canonical.is_empty() {
                    canonical = c.clone();
                }
            }
            Answer::NxDomain { .. } | Answer::NoData { .. } => not_found = true,
            Answer::Failed(e) => failure = Some(*e),
        }
    }
    if !addrs.is_empty() {
        // IPv4 first: it works through every NAT, while IPv6 often reaches
        // only the local network. Callers try the addresses in order.
        addrs.sort_by_key(|a| a.is_ipv6());
        return ResolveResult::Found {
            addresses: addrs,
            ttl_s: ttl,
            canonical: if canonical.is_empty() { String::from(name) } else { canonical },
        };
    }
    if not_found && failure.is_none() {
        return ResolveResult::Failed { error: NetError::NameNotFound };
    }
    ResolveResult::Failed { error: failure.unwrap_or(NetError::DnsFailure) }
}

/// Encodes a query for `name` (already validated).
pub(crate) fn build_query(txid: u16, name: &str, qtype: u16) -> Vec<u8> {
    let mut m = Vec::with_capacity(32 + name.len());
    m.extend_from_slice(&txid.to_be_bytes());
    m.extend_from_slice(&0x0100u16.to_be_bytes()); // RD
    m.extend_from_slice(&1u16.to_be_bytes()); // QDCOUNT
    m.extend_from_slice(&0u16.to_be_bytes()); // ANCOUNT
    m.extend_from_slice(&0u16.to_be_bytes()); // NSCOUNT
    m.extend_from_slice(&1u16.to_be_bytes()); // ARCOUNT (EDNS)
    for label in name.split('.') {
        m.push(label.len() as u8);
        m.extend_from_slice(label.as_bytes());
    }
    m.push(0);
    m.extend_from_slice(&qtype.to_be_bytes());
    m.extend_from_slice(&1u16.to_be_bytes()); // class IN
    // EDNS(0) OPT record: root name, type 41, class = payload size.
    m.push(0);
    m.extend_from_slice(&TYPE_OPT.to_be_bytes());
    m.extend_from_slice(&EDNS_PAYLOAD.to_be_bytes());
    m.extend_from_slice(&0u32.to_be_bytes());
    m.extend_from_slice(&0u16.to_be_bytes());
    m
}

/// Decodes a (possibly compressed) name inside `packet` to lower-case text.
fn decode_name<T: AsRef<[u8]>>(packet: &DnsPacket<T>, bytes: &[u8]) -> Option<String> {
    let mut out = String::new();
    let mut total = 0usize;
    for label in packet.parse_name(bytes) {
        let label = label.ok()?;
        total += label.len() + 1;
        if total > 255 {
            return None;
        }
        if !out.is_empty() {
            out.push('.');
        }
        for &b in label {
            // Keep names printable; odd bytes cannot match a valid query.
            out.push(if b.is_ascii_graphic() { b.to_ascii_lowercase() as char } else { '?' });
        }
    }
    Some(out)
}

/// Skips a name without following pointers; returns the rest.
fn skip_name(mut b: &[u8]) -> Option<&[u8]> {
    loop {
        let x = *b.first()?;
        match x {
            0 => return Some(&b[1..]),
            x if x & 0xC0 == 0xC0 => return b.get(2..),
            x if x & 0xC0 == 0 => b = b.get(1 + x as usize..)?,
            _ => return None,
        }
    }
}

/// Why a datagram was not taken as the answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reject {
    /// Not ours (wrong id or question): keep waiting.
    Ignore,
    /// The server failed (SERVFAIL, REFUSED, malformed): try another.
    ServerFailure,
}

/// An answer record the resolver uses.
#[derive(Debug)]
struct Record {
    owner: String,
    ttl: u32,
    data: RecordData,
}

#[derive(Debug)]
enum RecordData {
    /// From an A or AAAA record.
    Address(IpAddr),
    /// The name a CNAME record's owner is an alias of.
    Cname(String),
}

/// Parses an answer to the query (`txid`, `name`, `qtype`).
pub(crate) fn parse_response(msg: &[u8], txid: u16, name: &str, qtype: u16) -> Result<Answer, Reject> {
    let packet = DnsPacket::new_checked(msg).map_err(|_| Reject::Ignore)?;
    if packet.transaction_id() != txid {
        return Err(Reject::Ignore);
    }
    let flags = u16::from_be_bytes([msg[2], msg[3]]);
    if flags & 0x8000 == 0 || (flags >> 11) & 0xF != 0 {
        return Err(Reject::Ignore);
    }
    if packet.question_count() != 1 {
        return Err(Reject::Ignore);
    }
    let (mut rest, question) = DnsQuestion::parse(packet.payload()).map_err(|_| Reject::Ignore)?;
    let qname = decode_name(&packet, question.name).ok_or(Reject::Ignore)?;
    if qname != name || u16::from(question.type_) != qtype {
        return Err(Reject::Ignore);
    }
    let rcode = packet.rcode();
    let truncated = flags & 0x0200 != 0;
    // Answer section.
    let mut records: Vec<Record> = Vec::new();
    for _ in 0..packet.answer_record_count().min(64) {
        let (next, rec) = match DnsRecord::parse(rest) {
            Ok(v) => v,
            // An unparsable record (other class, odd length): stop reading
            // answers rather than trust a broken message.
            Err(_) => break,
        };
        rest = next;
        let Some(owner) = decode_name(&packet, rec.name) else { break };
        let data = match rec.data {
            DnsRecordData::A(a) => RecordData::Address(IpAddr::V4(a)),
            DnsRecordData::Aaaa(a) => RecordData::Address(IpAddr::V6(a)),
            DnsRecordData::Cname(target) => match decode_name(&packet, target) {
                Some(target) => RecordData::Cname(target),
                None => continue,
            },
            DnsRecordData::Other(..) => continue,
        };
        records.push(Record { owner, ttl: rec.ttl, data });
    }
    // The SOA minimum in the authority section bounds negative caching.
    let mut soa_ttl: Option<u32> = None;
    for _ in 0..packet.authority_record_count().min(16) {
        let Some(after_name) = skip_name(rest) else { break };
        if after_name.len() < 10 {
            break;
        }
        let rtype = u16::from_be_bytes([after_name[0], after_name[1]]);
        let ttl = u32::from_be_bytes([after_name[4], after_name[5], after_name[6], after_name[7]]);
        let len = u16::from_be_bytes([after_name[8], after_name[9]]) as usize;
        let Some(rdata) = after_name.get(10..10 + len) else { break };
        if rtype == 6 {
            // MNAME, RNAME, then five 32-bit numbers; MINIMUM is last.
            if let Some(r) = skip_name(rdata).and_then(skip_name)
                && r.len() >= 20
            {
                let minimum = u32::from_be_bytes([r[16], r[17], r[18], r[19]]);
                soa_ttl = Some(ttl.min(minimum));
            }
        }
        rest = &after_name[10 + len..];
    }
    let negative_ttl = soa_ttl.unwrap_or(DEFAULT_NEGATIVE_TTL_S);
    match rcode {
        DnsRcode::NoError => {}
        DnsRcode::NXDomain => return Ok(Answer::NxDomain { ttl_s: negative_ttl }),
        _ => return Err(Reject::ServerFailure),
    }
    // Follow the CNAME chain from the question name.
    let mut current = String::from(name);
    let mut ttl = u32::MAX;
    for _ in 0..MAX_CNAME_CHAIN {
        let link = records.iter().find_map(|r| match &r.data {
            RecordData::Cname(target) if r.owner == current => Some((target, r.ttl)),
            _ => None,
        });
        let Some((target, link_ttl)) = link else { break };
        ttl = ttl.min(link_ttl);
        current = target.clone();
    }
    // The addresses of the type asked for, at the chain's end.
    let mut addrs = Vec::new();
    for r in records.iter().filter(|r| r.owner == current) {
        if let RecordData::Address(a) = r.data
            && matches!((qtype, a), (TYPE_A, IpAddr::V4(_)) | (TYPE_AAAA, IpAddr::V6(_)))
            && !addrs.contains(&a)
        {
            addrs.push(a);
            ttl = ttl.min(r.ttl);
        }
    }
    if addrs.is_empty() {
        if truncated {
            // The answer did not fit and we do not retry over TCP yet.
            return Err(Reject::ServerFailure);
        }
        return Ok(Answer::NoData { ttl_s: negative_ttl });
    }
    Ok(Answer::Addresses { addrs, ttl_s: ttl, canonical: current })
}

impl Stack {
    /// Starts looking up `name`. Literal addresses, `localhost` and cached
    /// names are answered at once (collect them with
    /// [`Stack::resolve_result`] like any other).
    pub fn resolve(&mut self, name: &str, family: AddrFamily) -> Result<ResolveId, NetError> {
        let now = self.now_us;
        let id = self.dns.new_id();
        if let Ok(addr) = name.parse::<IpAddr>() {
            let ok = match family {
                AddrFamily::Any => true,
                AddrFamily::V4 => addr.is_ipv4(),
                AddrFamily::V6 => addr.is_ipv6(),
            };
            let result = if ok {
                ResolveResult::Found { addresses: alloc::vec![addr], ttl_s: 0, canonical: String::from(name) }
            } else {
                ResolveResult::Failed { error: NetError::NameNotFound }
            };
            self.dns.results.insert(id, result);
            return Ok(id);
        }
        let name = validate_name(name)?;
        if name == "localhost" || name.ends_with(".localhost") {
            let mut addresses = Vec::new();
            if family != AddrFamily::V6 {
                addresses.push(IpAddr::V4(core::net::Ipv4Addr::LOCALHOST));
            }
            if family != AddrFamily::V4 {
                addresses.push(IpAddr::V6(core::net::Ipv6Addr::LOCALHOST));
            }
            self.dns.results.insert(id, ResolveResult::Found { addresses, ttl_s: 0, canonical: name });
            return Ok(id);
        }
        let qtypes: &[u16] = match family {
            AddrFamily::V4 => &[TYPE_A],
            AddrFamily::V6 => &[TYPE_AAAA],
            AddrFamily::Any if self.has_ipv6_route() => &[TYPE_A, TYPE_AAAA],
            AddrFamily::Any => &[TYPE_A],
        };
        // Answer from the cache when every type is cached.
        let cached: Vec<Option<Answer>> = qtypes.iter().map(|&t| self.dns.cached(&name, t, now)).collect();
        if cached.iter().all(Option::is_some) {
            let answers: Vec<Answer> = cached.into_iter().flatten().collect();
            let result = match combine(&name, &answers) {
                ResolveResult::Found { addresses, canonical, .. } => {
                    // Report the time left, not the original TTL.
                    let left = qtypes
                        .iter()
                        .filter_map(|&t| self.dns.cache.get(&(name.clone(), t)))
                        .map(|e| ((e.expires_us.saturating_sub(now)) / 1_000_000) as u32)
                        .min()
                        .unwrap_or(0);
                    ResolveResult::Found { addresses, ttl_s: left, canonical }
                }
                other => other,
            };
            self.dns.results.insert(id, result);
            return Ok(id);
        }
        if self.dns.lookups.len() >= MAX_PENDING_LOOKUPS {
            return Err(NetError::LimitReached);
        }
        if self.dns_servers().is_empty() {
            self.dns.results.insert(id, ResolveResult::Failed { error: NetError::DnsFailure });
            return Ok(id);
        }
        let queries = qtypes.iter().map(|&qtype| Query { qtype, attempt: None, tries: 0, answer: None }).collect();
        self.dns.lookups.insert(id, Lookup { name, queries, deadline_us: now + LOOKUP_TIMEOUT_US });
        self.dns_advance(id);
        Ok(id)
    }

    /// The result of a lookup, once it is finished.
    pub fn resolve_result(&mut self, id: ResolveId) -> Option<ResolveResult> {
        self.dns.results.remove(&id)
    }

    /// Abandons a lookup.
    pub fn resolve_cancel(&mut self, id: ResolveId) {
        self.dns.results.remove(&id);
        if let Some(l) = self.dns.lookups.remove(&id) {
            for q in l.queries {
                if let Some(a) = q.attempt {
                    self.dns_close(a);
                }
            }
        }
    }

    /// Opens a query socket towards `server` and sends `msg`.
    fn dns_send(&mut self, server: SocketAddr, msg: &[u8]) -> Result<(IfaceId, SocketHandle, u16), NetError> {
        let via = self.route(server.ip())?;
        let port = self.alloc_port(Proto::Udp)?;
        let i = self.iface_mut(via).ok_or(NetError::NoRoute)?;
        let mut s = udp::Socket::new(
            udp::PacketBuffer::new(
                alloc::vec![udp::PacketMetadata::EMPTY; QUERY_RX_PACKETS],
                alloc::vec![0u8; QUERY_RX_BYTES],
            ),
            udp::PacketBuffer::new(
                alloc::vec![udp::PacketMetadata::EMPTY; QUERY_TX_PACKETS],
                alloc::vec![0u8; QUERY_TX_BYTES],
            ),
        );
        s.bind(IpListenEndpoint { addr: None, port }).map_err(|_| NetError::Internal)?;
        s.send_slice(msg, IpEndpoint::from(server)).map_err(|e| match e {
            udp::SendError::BufferFull => NetError::Internal,
            udp::SendError::Unaddressable => NetError::NoRoute,
        })?;
        let handle = i.sockets.add(s);
        self.socks.reserve(Proto::Udp, port);
        Ok((via, handle, port))
    }

    /// Whether a query socket (bound to every address) is on `port`.
    pub(crate) fn dns_has_port(&self, port: u16) -> bool {
        self.dns.lookups.values().flat_map(|l| &l.queries).any(|q| q.attempt.as_ref().is_some_and(|a| a.port == port))
    }

    /// Closes a query socket.
    fn dns_close(&mut self, a: Attempt) {
        if let Some(i) = self.iface_mut(a.iface) {
            i.sockets.remove(a.handle);
        }
        self.socks.release(Proto::Udp, a.port);
    }

    /// An interface is going away: queries sent through it are abandoned
    /// (their sockets go with it) and asked again elsewhere.
    pub(crate) fn dns_on_iface_removed(&mut self, id: IfaceId) {
        let mut retry = Vec::new();
        for (&lid, l) in self.dns.lookups.iter_mut() {
            for q in &mut l.queries {
                if q.attempt.as_ref().is_some_and(|a| a.iface == id) {
                    let a = q.attempt.take().expect("attempt");
                    self.socks.release(Proto::Udp, a.port);
                    retry.push(lid);
                }
            }
        }
        for lid in retry {
            self.dns_advance(lid);
        }
    }

    /// Ids of lookups whose results are ready.
    pub fn resolve_ready(&self) -> Vec<ResolveId> {
        self.dns.results.keys().copied().collect()
    }

    /// Empties the DNS cache.
    pub fn flush_dns_cache(&mut self) {
        self.dns.flush();
    }

    /// Sends the next attempt of every query of a lookup that needs one. A
    /// server that cannot be sent to is passed over at once, so the next
    /// one is asked without waiting; a query runs out of servers after
    /// [`ROUNDS`] rounds through them.
    fn dns_advance(&mut self, id: ResolveId) {
        let now = self.now_us;
        let servers: Vec<SocketAddr> = self.dns_servers().into_iter().take(3).map(|s| SocketAddr::new(s, 53)).collect();
        let Some(mut lookup) = self.dns.lookups.remove(&id) else { return };
        for q in &mut lookup.queries {
            if q.answer.is_some() || q.attempt.is_some() {
                continue;
            }
            let mut failure = if servers.is_empty() { NetError::DnsFailure } else { NetError::TimedOut };
            while q.tries < servers.len() * ROUNDS {
                let server = servers[q.tries % servers.len()];
                let round = q.tries / servers.len();
                q.tries += 1;
                let txid = self.random_u32() as u16;
                let msg = build_query(txid, &lookup.name, q.qtype);
                match self.dns_send(server, &msg) {
                    Ok((iface, handle, port)) => {
                        let timeout_at_us = now + (FIRST_TIMEOUT_US << round.min(4));
                        q.attempt = Some(Attempt { iface, handle, port, server, txid, timeout_at_us });
                        break;
                    }
                    Err(e) => failure = e,
                }
            }
            if q.attempt.is_none() {
                q.answer = Some(Answer::Failed(failure));
            }
        }
        self.dns.lookups.insert(id, lookup);
    }

    /// What arrived for an attempt: an answer, a failure, or nothing yet.
    fn dns_receive(&mut self, a: &Attempt, name: &str, qtype: u16) -> Option<Result<Answer, Reject>> {
        let i = self.iface_mut(a.iface)?;
        let s = i.sockets.get_mut::<udp::Socket>(a.handle);
        while let Ok((msg, meta)) = s.recv() {
            if SocketAddr::from(meta.endpoint) != a.server {
                continue;
            }
            match parse_response(msg, a.txid, name, qtype) {
                Err(Reject::Ignore) => continue,
                r => return Some(r),
            }
        }
        None
    }

    /// Resolver housekeeping: collects answers, handles timeouts, finishes
    /// lookups.
    pub(crate) fn dns_maintain(&mut self) {
        let now = self.now_us;
        let ids: Vec<ResolveId> = self.dns.lookups.keys().copied().collect();
        for id in ids {
            let Some(mut lookup) = self.dns.lookups.remove(&id) else { continue };
            let mut retry = false;
            for q in &mut lookup.queries {
                let Some(attempt) = q.attempt.take() else { continue };
                match self.dns_receive(&attempt, &lookup.name, q.qtype) {
                    Some(Ok(answer)) => {
                        self.dns_close(attempt);
                        q.answer = Some(answer);
                    }
                    Some(Err(_)) => {
                        self.dns_close(attempt);
                        retry = true;
                    }
                    None if now >= attempt.timeout_at_us => {
                        self.dns_close(attempt);
                        retry = true;
                    }
                    None => q.attempt = Some(attempt),
                }
            }
            let done = lookup.queries.iter().all(|q| q.answer.is_some());
            if done || now >= lookup.deadline_us {
                for q in &mut lookup.queries {
                    if let Some(a) = q.attempt.take() {
                        self.dns_close(a);
                    }
                    if q.answer.is_none() {
                        q.answer = Some(Answer::Failed(NetError::TimedOut));
                    }
                    let answer = q.answer.clone().unwrap();
                    self.dns.store(&lookup.name, q.qtype, &answer, now);
                }
                let answers: Vec<Answer> = lookup.queries.iter().filter_map(|q| q.answer.clone()).collect();
                let result = match combine(&lookup.name, &answers) {
                    // A timeout of the whole lookup is a DNS failure to the
                    // user, unless a server said the name does not exist.
                    ResolveResult::Failed { error: NetError::TimedOut } => {
                        ResolveResult::Failed { error: NetError::DnsFailure }
                    }
                    r => r,
                };
                self.dns.results.insert(id, result);
                continue;
            }
            self.dns.lookups.insert(id, lookup);
            if retry {
                self.dns_advance(id);
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use alloc::vec;

    /// Builds a response to `query` with the given rcode and records
    /// (name, type, ttl, rdata) in the answer section and optional
    /// authority records.
    pub fn response(
        query: &[u8],
        rcode: u8,
        answers: &[(&str, u16, u32, Vec<u8>)],
        authority: &[(&str, u16, u32, Vec<u8>)],
    ) -> Vec<u8> {
        // Header + question from the query (without its OPT record).
        let qend = 12 + skip_name(&query[12..]).map(|r| query.len() - 12 - r.len()).unwrap() + 4;
        let mut m = query[..qend].to_vec();
        m[2] = 0x81; // QR, RD
        m[3] = 0x80 | rcode; // RA
        m[6..8].copy_from_slice(&(answers.len() as u16).to_be_bytes());
        m[8..10].copy_from_slice(&(authority.len() as u16).to_be_bytes());
        m[10..12].copy_from_slice(&0u16.to_be_bytes());
        for (name, ty, ttl, data) in answers.iter().chain(authority.iter()) {
            for label in name.split('.') {
                m.push(label.len() as u8);
                m.extend_from_slice(label.as_bytes());
            }
            m.push(0);
            m.extend_from_slice(&ty.to_be_bytes());
            m.extend_from_slice(&1u16.to_be_bytes());
            m.extend_from_slice(&ttl.to_be_bytes());
            m.extend_from_slice(&(data.len() as u16).to_be_bytes());
            m.extend_from_slice(data);
        }
        m
    }

    pub fn encode_name(name: &str) -> Vec<u8> {
        let mut v = Vec::new();
        for label in name.split('.') {
            v.push(label.len() as u8);
            v.extend_from_slice(label.as_bytes());
        }
        v.push(0);
        v
    }

    #[test]
    fn names_are_validated_and_normalised() {
        assert_eq!(validate_name("Example.COM."), Ok("example.com".into()));
        assert_eq!(validate_name("a-b.c_d.e"), Ok("a-b.c_d.e".into()));
        assert!(validate_name("").is_err());
        assert!(validate_name("bad..name").is_err());
        assert!(validate_name("-lead.example").is_err());
        assert!(validate_name("sp ace.example").is_err());
        assert!(validate_name(&"a".repeat(64)).is_err());
        assert!(validate_name(&["abc"; 70].join(".")).is_err());
    }

    #[test]
    fn query_layout() {
        let q = build_query(0xBEEF, "example.com", TYPE_A);
        assert_eq!(&q[0..2], &[0xBE, 0xEF]);
        assert_eq!(&q[4..6], &[0, 1]);
        assert_eq!(&q[12..25], b"\x07example\x03com\x00");
        assert_eq!(&q[25..29], &[0, 1, 0, 1]);
        // EDNS OPT record at the end.
        assert_eq!(&q[29..32], &[0, 0, 41]);
    }

    #[test]
    fn parses_addresses_and_follows_cnames() {
        let q = build_query(7, "www.example.com", TYPE_A);
        let r = response(
            &q,
            0,
            &[
                ("www.example.com", 5, 300, encode_name("cdn.example.net")),
                ("cdn.example.net", 1, 60, vec![93, 184, 215, 14]),
                ("cdn.example.net", 1, 120, vec![93, 184, 215, 15]),
                // Not the type asked for.
                ("cdn.example.net", 28, 30, "2001:db8::1".parse::<core::net::Ipv6Addr>().unwrap().octets().to_vec()),
                ("unrelated.example", 1, 10, vec![1, 2, 3, 4]),
            ],
            &[],
        );
        let a = parse_response(&r, 7, "www.example.com", TYPE_A).unwrap();
        assert_eq!(
            a,
            Answer::Addresses {
                addrs: vec!["93.184.215.14".parse().unwrap(), "93.184.215.15".parse().unwrap()],
                ttl_s: 60,
                canonical: "cdn.example.net".into(),
            }
        );
    }

    #[test]
    fn rejects_mismatched_answers() {
        let q = build_query(7, "example.com", TYPE_A);
        let r = response(&q, 0, &[("example.com", 1, 60, vec![1, 1, 1, 1])], &[]);
        assert_eq!(parse_response(&r, 8, "example.com", TYPE_A), Err(Reject::Ignore));
        assert_eq!(parse_response(&r, 7, "other.com", TYPE_A), Err(Reject::Ignore));
        assert_eq!(parse_response(&r, 7, "example.com", TYPE_AAAA), Err(Reject::Ignore));
        // A query (QR clear) is not an answer.
        assert_eq!(parse_response(&q, 7, "example.com", TYPE_A), Err(Reject::Ignore));
        // Records for other names are not taken.
        let r2 = response(&q, 0, &[("evil.example", 1, 60, vec![6, 6, 6, 6])], &[]);
        assert_eq!(parse_response(&r2, 7, "example.com", TYPE_A), Ok(Answer::NoData { ttl_s: DEFAULT_NEGATIVE_TTL_S }));
    }

    #[test]
    fn negative_answers_use_the_soa_minimum() {
        let q = build_query(9, "nope.example.com", TYPE_A);
        let mut soa = encode_name("ns.example.com");
        soa.extend(encode_name("admin.example.com"));
        for v in [1u32, 7200, 3600, 1209600, 300] {
            soa.extend_from_slice(&v.to_be_bytes());
        }
        let r = response(&q, 3, &[], &[("example.com", 6, 900, soa)]);
        assert_eq!(parse_response(&r, 9, "nope.example.com", TYPE_A), Ok(Answer::NxDomain { ttl_s: 300 }));
        let servfail = response(&q, 2, &[], &[]);
        assert_eq!(parse_response(&servfail, 9, "nope.example.com", TYPE_A), Err(Reject::ServerFailure));
    }

    #[test]
    fn compression_pointers_and_loops() {
        let q = build_query(1, "a.example", TYPE_A);
        let mut r = response(&q, 0, &[], &[]);
        r[6..8].copy_from_slice(&1u16.to_be_bytes());
        // Answer: name = pointer to the question name (offset 12).
        r.extend_from_slice(&[0xC0, 12, 0, 1, 0, 1, 0, 0, 0, 30, 0, 4, 10, 0, 0, 1]);
        assert_eq!(
            parse_response(&r, 1, "a.example", TYPE_A),
            Ok(Answer::Addresses {
                addrs: vec!["10.0.0.1".parse().unwrap()],
                ttl_s: 30,
                canonical: "a.example".into()
            })
        );
        // A self-referencing pointer must not hang or panic.
        let mut bad = response(&q, 0, &[], &[]);
        bad[6..8].copy_from_slice(&1u16.to_be_bytes());
        let at = bad.len() as u8;
        bad.extend_from_slice(&[0xC0, at, 0, 1, 0, 1, 0, 0, 0, 30, 0, 4, 10, 0, 0, 1]);
        assert!(matches!(parse_response(&bad, 1, "a.example", TYPE_A), Ok(Answer::NoData { .. })));
    }

    #[test]
    fn garbage_never_panics() {
        let mut seed = 0x1234_5678u32;
        let q = build_query(5, "fuzz.example", TYPE_A);
        let good = response(&q, 0, &[("fuzz.example", 1, 60, vec![1, 2, 3, 4])], &[]);
        for i in 0..20_000 {
            let mut m = good.clone();
            for _ in 0..(i % 6) + 1 {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let pos = seed as usize % m.len();
                m[pos] = (seed >> 8) as u8;
            }
            m.truncate(seed as usize % (m.len() + 1));
            let _ = parse_response(&m, 5, "fuzz.example", TYPE_A);
        }
    }
}
