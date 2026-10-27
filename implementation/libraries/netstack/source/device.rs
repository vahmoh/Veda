// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The smoltcp device behind each interface, over the interface's [`Link`]:
//! a network device's shared frame rings in `network`, a simulated cable in
//! the tests, and a queue of the stack's own for the loopback interface.
//!
//! A frame is copied once on its way in, out of the link into the
//! interface's receive buffer, where smoltcp parses it, and once on its way
//! out, from the buffer smoltcp built it in into the link. No frame is
//! allocated. When the link has no room, smoltcp is told so and keeps what
//! it has to send until the link drains (the caller waits for room rather
//! than the stack dropping frames).

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use proto::net::InterfaceStats;
use smoltcp::phy::{self, Checksum, ChecksumCapabilities, DeviceCapabilities, Medium};
use smoltcp::time::Instant;

use crate::IfaceId;
use crate::firewall::{Firewall, Place};

/// The largest frame an interface receives: what a device link's slot
/// holds. Frames may exceed the MTU: a hypervisor bridged to a host whose
/// network adapter merges TCP segments (receive segment coalescing) passes
/// on frames several times the MTU, valid in every other way.
pub const MAX_RX_FRAME: usize = 16384;
/// Frames taken from one link per poll, so that a device that keeps
/// sending cannot hold the stack in one poll for ever.
pub const RX_BUDGET: usize = 256;

/// Where an interface's frames come from and go to.
pub trait Link {
    /// Copies the next frame the device received into `buf` and returns
    /// its length, or `None` if there is none. Frames longer than `buf` are
    /// skipped (and counted as malformed).
    fn receive(&mut self, buf: &mut [u8]) -> Option<usize>;

    /// Whether the device can take a frame now.
    fn can_send(&self) -> bool;

    /// Hands `frame` to the device; `false` if it was dropped (no room, or
    /// too large).
    fn send(&mut self, frame: &[u8]) -> bool;

    /// Frames the device had to drop because the stack had no room for
    /// them, since the link was made.
    fn dropped(&self) -> u64 {
        0
    }

    /// Malformed frames skipped, since the link was made.
    fn malformed(&self) -> u64 {
        0
    }
}

/// The links of the interfaces, by interface.
pub trait Links {
    /// The link of interface `id`, if it has one now.
    fn link(&mut self, id: IfaceId) -> Option<&mut dyn Link>;
}

/// No links at all (an interface whose device is not there: timers run,
/// nothing moves).
pub struct NoLinks;

impl Links for NoLinks {
    fn link(&mut self, _id: IfaceId) -> Option<&mut dyn Link> {
        None
    }
}

/// The loopback interface's link: what it sends, it receives. Its frame
/// buffers are reused.
#[derive(Default)]
pub struct Loopback {
    queue: VecDeque<Vec<u8>>,
    spare: Vec<Vec<u8>>,
}

/// Most frames on their way through the loopback interface.
const LOOPBACK_QUEUE: usize = 64;

impl Loopback {
    /// Whether frames wait to come back.
    pub fn pending(&self) -> bool {
        !self.queue.is_empty()
    }
}

impl Link for Loopback {
    fn receive(&mut self, buf: &mut [u8]) -> Option<usize> {
        let frame = self.queue.pop_front()?;
        let n = frame.len().min(buf.len());
        buf[..n].copy_from_slice(&frame[..n]);
        self.spare.push(frame);
        Some(n)
    }

    fn can_send(&self) -> bool {
        self.queue.len() < LOOPBACK_QUEUE
    }

    fn send(&mut self, frame: &[u8]) -> bool {
        if !self.can_send() {
            return false;
        }
        let mut v = self.spare.pop().unwrap_or_default();
        v.clear();
        v.extend_from_slice(frame);
        self.queue.push_back(v);
        true
    }
}

/// An interface's buffers and what its device does, for smoltcp during one
/// poll.
pub struct Frames {
    medium: Medium,
    /// Largest frame sent (Ethernet: IP MTU + 14).
    mtu: usize,
    /// Frames come straight back (the loopback interface).
    loopback: bool,
    rx: Vec<u8>,
    tx: Vec<u8>,
    /// The link was full when smoltcp had something to send.
    pub blocked: bool,
    pub stats: InterfaceStats,
    /// The link's own counts (dropped by the device, malformed), last seen.
    pub link_dropped: u64,
    pub link_malformed: u64,
}

impl Frames {
    /// The buffers of an interface on `medium` that sends frames of up to
    /// `mtu` bytes (`loopback`: the loopback interface's).
    pub fn new(medium: Medium, mtu: usize, loopback: bool) -> Frames {
        Frames {
            medium,
            mtu,
            loopback,
            rx: alloc::vec![0u8; mtu.max(MAX_RX_FRAME)],
            tx: alloc::vec![0u8; mtu],
            blocked: false,
            stats: InterfaceStats::default(),
            link_dropped: 0,
            link_malformed: 0,
        }
    }

    /// The device smoltcp polls, over `link` (`None`: no device attached,
    /// so nothing moves), through the firewall's `gate` (`None`: none, as
    /// for the loopback interface).
    pub fn device<'a>(&'a mut self, link: Option<&'a mut dyn Link>, gate: Option<Gate<'a>>) -> LinkDevice<'a> {
        self.blocked = false;
        LinkDevice { link, frames: self, budget: RX_BUDGET, gate }
    }

    /// What the interface's frames are (Ethernet, or bare IP packets).
    pub fn medium(&self) -> Medium {
        self.medium
    }

    /// The statistics with the link's own counts.
    pub fn statistics(&self) -> InterfaceStats {
        let mut s = self.stats;
        s.rx_dropped += self.link_dropped;
        s.rx_errors += self.link_malformed;
        s
    }
}

/// The firewall, as an interface's device has it: what it receives and
/// sends passes it ([`Firewall::inbound`], [`Firewall::outbound`]).
pub struct Gate<'a> {
    pub firewall: &'a mut Firewall,
    pub place: Place<'a>,
    pub medium: Medium,
    pub now_us: u64,
}

/// smoltcp's device: the link, and the interface's buffers.
pub struct LinkDevice<'a> {
    link: Option<&'a mut dyn Link>,
    frames: &'a mut Frames,
    /// Frames still to be taken in this poll (those the firewall drops
    /// included).
    budget: usize,
    gate: Option<Gate<'a>>,
}

impl Drop for LinkDevice<'_> {
    fn drop(&mut self) {
        if let Some(link) = &self.link {
            self.frames.link_dropped = link.dropped();
            self.frames.link_malformed = link.malformed();
        }
    }
}

/// A received frame, in the interface's receive buffer.
pub struct RxToken<'a>(&'a [u8]);

impl phy::RxToken for RxToken<'_> {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(self.0)
    }
}

/// Room for a frame to send: smoltcp builds it in the interface's send
/// buffer, which then goes to the link (if the firewall lets it).
pub struct TxToken<'a, 'g> {
    link: &'a mut dyn Link,
    buf: &'a mut [u8],
    stats: &'a mut InterfaceStats,
    gate: Option<&'a mut Gate<'g>>,
}

impl phy::TxToken for TxToken<'_, '_> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let Some(buf) = self.buf.get_mut(..len) else {
            // Larger than the device's capabilities allow: smoltcp does not
            // ask for that, but a frame that does not fit is dropped rather
            // than truncated.
            self.stats.tx_dropped += 1;
            return f(&mut alloc::vec![0u8; len]);
        };
        let r = f(buf);
        if let Some(g) = self.gate
            && !g.firewall.outbound(buf, g.medium, g.now_us)
        {
            return r;
        }
        if self.link.send(buf) {
            self.stats.tx_packets += 1;
            self.stats.tx_bytes += len as u64;
        } else {
            self.stats.tx_dropped += 1;
        }
        r
    }
}

impl<'g> phy::Device for LinkDevice<'g> {
    type RxToken<'b>
        = RxToken<'b>
    where
        Self: 'b;
    type TxToken<'b>
        = TxToken<'b, 'g>
    where
        Self: 'b;

    fn receive(&mut self, _now: Instant) -> Option<(RxToken<'_>, TxToken<'_, 'g>)> {
        let link = self.link.as_deref_mut()?;
        let Frames { rx, tx, stats, .. } = &mut *self.frames;
        loop {
            if self.budget == 0 {
                return None;
            }
            let n = link.receive(rx)?.min(rx.len());
            self.budget -= 1;
            stats.rx_packets += 1;
            stats.rx_bytes += n as u64;
            if let Some(g) = &mut self.gate
                && !g.firewall.inbound(&rx[..n], g.medium, &g.place, g.now_us)
            {
                continue;
            }
            return Some((RxToken(&rx[..n]), TxToken { link, buf: tx, stats, gate: self.gate.as_mut() }));
        }
    }

    fn transmit(&mut self, _now: Instant) -> Option<TxToken<'_, 'g>> {
        let link = self.link.as_deref_mut()?;
        if !link.can_send() {
            self.frames.blocked = true;
            return None;
        }
        let Frames { tx, stats, .. } = &mut *self.frames;
        Some(TxToken { link, buf: tx, stats, gate: self.gate.as_mut() })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = self.frames.medium;
        caps.max_transmission_unit = self.frames.mtu;
        caps.max_burst_size = None;
        caps.checksum = if self.frames.loopback {
            // Loopback frames never leave memory: skip checksums both ways.
            ChecksumCapabilities::ignored()
        } else {
            let mut checksum = ChecksumCapabilities::default();
            checksum.ipv4 = Checksum::Both;
            checksum.udp = Checksum::Both;
            checksum.tcp = Checksum::Both;
            checksum.icmpv4 = Checksum::Both;
            checksum.icmpv6 = Checksum::Both;
            checksum
        };
        caps
    }
}
