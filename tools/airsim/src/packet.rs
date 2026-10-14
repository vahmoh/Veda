// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! DHCP and DNS traffic on the wired side, recognised, and a DNS query
//! answered with SERVFAIL (the `dns servfail` condition). smoltcp reads
//! and writes the Ethernet, IP and UDP headers; of DNS, just enough is
//! read here (smoltcp keeps its DNS parser to itself).

use smoltcp::phy::ChecksumCapabilities;
use smoltcp::wire::{
    EthernetFrame, EthernetProtocol, EthernetRepr, IpAddress, IpProtocol, Ipv4Packet, Ipv4Repr, Ipv6Packet, UdpPacket,
    UdpRepr,
};

/// What a frame on the wired side carries, as far as the simulated network
/// conditions care.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// DHCP or DHCPv6.
    Dhcp,
    /// A DNS query (UDP to port 53).
    DnsQuery,
    Other,
}

const DNS_PORT: u16 = 53;

/// The UDP datagram of an IPv4 or IPv6 packet in an Ethernet frame.
fn udp(eth: &[u8]) -> Option<UdpPacket<&[u8]>> {
    let frame = EthernetFrame::new_checked(eth).ok()?;
    let payload = match frame.ethertype() {
        EthernetProtocol::Ipv4 => {
            let ip = Ipv4Packet::new_checked(frame.payload()).ok()?;
            // Only a packet's first fragment carries the UDP header.
            if ip.version() != 4 || ip.next_header() != IpProtocol::Udp || ip.frag_offset() != 0 {
                return None;
            }
            ip.payload()
        }
        EthernetProtocol::Ipv6 => {
            let ip = Ipv6Packet::new_checked(frame.payload()).ok()?;
            if ip.version() != 6 || ip.next_header() != IpProtocol::Udp {
                return None;
            }
            ip.payload()
        }
        _ => return None,
    };
    UdpPacket::new_checked(payload).ok()
}

pub fn classify(eth: &[u8]) -> Kind {
    const DHCP_PORTS: [u16; 4] = [67, 68, 546, 547];
    match udp(eth).map(|u| (u.src_port(), u.dst_port())) {
        Some((s, d)) if DHCP_PORTS.contains(&s) && DHCP_PORTS.contains(&d) => Kind::Dhcp,
        Some((_, DNS_PORT)) => Kind::DnsQuery,
        _ => Kind::Other,
    }
}

/// The length of a DNS question (name, type and class), if well formed.
fn question_len(q: &[u8]) -> Option<usize> {
    let mut i = 0;
    loop {
        let label = *q.get(i)? as usize;
        i += 1;
        if label == 0 {
            break;
        }
        // Questions from a stub resolver are never compressed.
        if label & 0xC0 != 0 || i + label > 255 {
            return None;
        }
        i += label;
    }
    (q.len() >= i + 4).then_some(i + 4)
}

/// A SERVFAIL answer to a DNS query carried over IPv4 (`None` for anything
/// else). The answer comes from the address the query was sent to and
/// repeats the question.
pub fn dns_servfail(eth: &[u8]) -> Option<Vec<u8>> {
    let frame = EthernetFrame::new_checked(eth).ok()?;
    let query = udp(eth)?;
    if query.dst_port() != DNS_PORT || frame.ethertype() != EthernetProtocol::Ipv4 {
        return None;
    }
    let ip = Ipv4Packet::new_checked(frame.payload()).ok()?;
    let dns = query.payload();
    // A query (QR clear) with at most one question.
    if dns.len() < 12 || dns[2] & 0x80 != 0 {
        return None;
    }
    let questions = u16::from_be_bytes([dns[4], dns[5]]);
    let header_and_question = match questions {
        0 => 12,
        1 => 12 + question_len(&dns[12..])?,
        _ => return None,
    };
    let mut answer = dns[..header_and_question].to_vec();
    answer[2] = 0x80 | (dns[2] & 0x79); // QR, with the query's opcode and RD
    answer[3] = 0x80 | 2; // RA, RCODE = SERVFAIL
    answer[6..12].fill(0); // no answer, authority or additional records

    let (server, client) = (ip.dst_addr(), ip.src_addr());
    let eth_repr =
        EthernetRepr { src_addr: frame.dst_addr(), dst_addr: frame.src_addr(), ethertype: EthernetProtocol::Ipv4 };
    let udp_repr = UdpRepr { src_port: DNS_PORT, dst_port: query.src_port() };
    let udp_len = udp_repr.header_len() + answer.len();
    let ip_repr = Ipv4Repr {
        src_addr: server,
        dst_addr: client,
        next_header: IpProtocol::Udp,
        payload_len: udp_len,
        hop_limit: 64,
    };
    let mut out = vec![0; eth_repr.buffer_len() + ip_repr.buffer_len() + udp_len];
    let checksums = ChecksumCapabilities::default();
    let mut frame = EthernetFrame::new_unchecked(&mut out[..]);
    eth_repr.emit(&mut frame);
    let mut packet = Ipv4Packet::new_unchecked(frame.payload_mut());
    ip_repr.emit(&mut packet, &checksums);
    let mut datagram = UdpPacket::new_unchecked(packet.payload_mut());
    let (from, to) = (IpAddress::Ipv4(server), IpAddress::Ipv4(client));
    udp_repr.emit(&mut datagram, &from, &to, answer.len(), |b| b.copy_from_slice(&answer), &checksums);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An Ethernet frame with an IPv4/UDP packet (header checksums left
    /// zero; nothing here checks them).
    fn udp4(src_port: u16, dst_port: u16, payload: &[u8]) -> Vec<u8> {
        let mut f = vec![0x52, 0x55, 10, 0, 2, 2, 0x02, 0x56, 0x57, 0xAA, 0, 1, 0x08, 0x00];
        let total = (20 + 8 + payload.len()) as u16;
        f.extend_from_slice(&[0x45, 0, (total >> 8) as u8, total as u8, 0x12, 0x34, 0, 0, 64, 17, 0, 0]);
        f.extend_from_slice(&[10, 0, 2, 15, 10, 0, 2, 3]);
        f.extend_from_slice(&src_port.to_be_bytes());
        f.extend_from_slice(&dst_port.to_be_bytes());
        f.extend_from_slice(&((8 + payload.len()) as u16).to_be_bytes());
        f.extend_from_slice(&[0, 0]);
        f.extend_from_slice(payload);
        f
    }

    fn query(name: &[&str]) -> Vec<u8> {
        let mut q = vec![0xAB, 0xCD, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 1];
        for l in name {
            q.push(l.len() as u8);
            q.extend_from_slice(l.as_bytes());
        }
        q.extend_from_slice(&[0, 0, 1, 0, 1]);
        // An EDNS OPT record in the additional section.
        q.extend_from_slice(&[0, 0, 41, 0x04, 0xD0, 0, 0, 0, 0, 0, 0]);
        q
    }

    #[test]
    fn recognises_dhcp_and_dns() {
        assert_eq!(classify(&udp4(68, 67, &[1; 240])), Kind::Dhcp);
        assert_eq!(classify(&udp4(67, 68, &[2; 240])), Kind::Dhcp);
        assert_eq!(classify(&udp4(49152, 53, &query(&["example", "com"]))), Kind::DnsQuery);
        assert_eq!(classify(&udp4(53, 49152, &[0; 12])), Kind::Other);
        assert_eq!(classify(&udp4(49152, 443, &[0; 12])), Kind::Other);
        // DHCPv6 in IPv6.
        let mut v6 = vec![0x33, 0x33, 0, 1, 0, 2, 0x02, 0x56, 0x57, 0xAA, 0, 1, 0x86, 0xDD];
        v6.extend_from_slice(&[0x60, 0, 0, 0, 0, 12, 17, 1]);
        v6.extend_from_slice(&[0; 32]);
        v6.extend_from_slice(&[0x02, 0x22, 0x02, 0x23, 0, 12, 0, 0, 1, 2, 3, 4]);
        assert_eq!(classify(&v6), Kind::Dhcp);
        // Truncated and non-IP frames.
        assert_eq!(classify(&[0; 10]), Kind::Other);
        assert_eq!(classify(&udp4(68, 67, &[])[..30]), Kind::Other);
    }

    #[test]
    fn servfail_answer_is_well_formed() {
        let q = udp4(50000, 53, &query(&["www", "example", "com"]));
        let a = dns_servfail(&q).unwrap();
        // Addressed back to the querier.
        assert_eq!(a[0..6], q[6..12]);
        assert_eq!(a[6..12], q[0..6]);
        assert_eq!(a[26..30], q[30..34]);
        assert_eq!(a[30..34], q[26..30]);
        assert_eq!(u16::from_be_bytes([a[34], a[35]]), 53);
        assert_eq!(u16::from_be_bytes([a[36], a[37]]), 50000);
        // Both checksums verify.
        let ip = Ipv4Packet::new_checked(&a[14..]).unwrap();
        assert!(ip.verify_checksum());
        let udp_len = u16::from_be_bytes([a[38], a[39]]);
        assert_eq!(udp_len as usize, a.len() - 34);
        let udp = UdpPacket::new_checked(ip.payload()).unwrap();
        assert!(udp.verify_checksum(&IpAddress::Ipv4(ip.src_addr()), &IpAddress::Ipv4(ip.dst_addr())));
        // Same ID and question, SERVFAIL, no other records.
        let dns = &a[42..];
        assert_eq!(dns[0..2], [0xAB, 0xCD]);
        assert_eq!(dns[2] & 0x80, 0x80);
        assert_eq!(dns[3] & 0x0F, 2);
        assert_eq!(dns[4..12], [0, 1, 0, 0, 0, 0, 0, 0]);
        assert_eq!(dns.len(), 12 + 17 + 4);
    }

    #[test]
    fn servfail_is_only_built_for_queries() {
        let mut answer = query(&["example", "com"]);
        answer[2] |= 0x80;
        assert!(dns_servfail(&udp4(50000, 53, &answer)).is_none());
        assert!(dns_servfail(&udp4(50000, 80, &query(&["a"]))).is_none());
        assert!(dns_servfail(&udp4(50000, 53, &[0; 5])).is_none());
        // A question running past the end of the packet.
        assert!(dns_servfail(&udp4(50000, 53, &[0, 1, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 9, b'a'])).is_none());
        // Random bytes never panic.
        let mut s = 0x9E37_79B9u32;
        for _ in 0..2000 {
            let len = (s % 80) as usize;
            let mut p = Vec::with_capacity(len);
            for _ in 0..len {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                p.push(s as u8);
            }
            let f = udp4(50000, 53, &p);
            let _ = dns_servfail(&f);
            let _ = classify(&f[..f.len().min((s % 64) as usize)]);
        }
    }
}
