// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! A radio as the Wi-Fi service's managed radio: Linux is its MLME, on the
//! service's commands (`wlanmlme_ctl`), through nl80211 (`nl80211`). It
//! scans, and gives the service what it heard as the frames a scan would
//! have; it authenticates and associates with the access point the
//! service chose, the service's frames for them in Linux's, and gives the
//! service the access point's answers; it installs the session keys the
//! service gives, opens the port, and carries EAPOL between the service and
//! the control port; it watches the link, and says when it is lost, and the
//! access point's signal. What it tells the service waits in order
//! ([`Radio::out`]) until [`Radio::flush`] sends it: frames on the link,
//! events on the service's channel.

use std::fs::File;
use std::io;
use std::time::{Duration, Instant};

use guest_sys::netif::Interface;
use vipc::Bytes;
use vproto::netring::{Link, RxInfo, SlotMeta, kind};
use vproto::wlan::{
    Band, ChannelInfo, KeyKind, MLME_EVENT, MlmeEvent, PhyError, PhyInfo, PhyStats, channel_flags, phy_caps,
    wlanmlme_ctl,
};
use vrt::object::Channel;

use crate::nl80211::{self, Bss, Event, Nl80211};

/// How often the signal of the access point joined is asked for.
const SIGNAL_EVERY: Duration = Duration::from_secs(2);
/// Management frames of the access point's that come to the service:
/// SA Query responses (Linux answers the queries itself).
const ACTION: u16 = 0x00D0;
const SA_QUERY_RESPONSE: [u8; 2] = [8, 1];
/// Reason code of a station that leaves (IEEE 802.11-2020, 9.4.1.7).
const DEAUTH_LEAVING: u16 = 3;
/// The most EAPOL frames kept until the association's end (the access
/// point's first message, and its resending).
const EARLY_EAPOL: usize = 4;
const ENOENT: i32 = 2;
const EBUSY: i32 = 16;
const ENOLINK: i32 = 67;
const EOPNOTSUPP: i32 = 95;
const ENETDOWN: i32 = 100;
const ENOTCONN: i32 = 107;

/// The channel and band of a frequency (2.4 and 5 GHz).
fn channel_of(freq: u32) -> Option<(Band, u8)> {
    match freq {
        2484 => Some((Band::Ghz2, 14)),
        2412..=2472 => Some((Band::Ghz2, ((freq - 2407) / 5) as u8)),
        5160..=5885 => Some((Band::Ghz5, ((freq - 5000) / 5) as u8)),
        _ => None,
    }
}

fn freq_of(band: Band, channel: u8) -> u32 {
    let c = channel as u32;
    match band {
        Band::Ghz2 if c == 14 => 2484,
        Band::Ghz2 => 2407 + 5 * c,
        Band::Ghz5 => 5000 + 5 * c,
        Band::Ghz6 if c == 2 => 5935,
        Band::Ghz6 => 5950 + 5 * c,
    }
}

fn phy_error(e: &io::Error) -> PhyError {
    match e.raw_os_error() {
        // The radio is off, or joined nothing.
        Some(ENETDOWN | ENOTCONN | ENOLINK) => PhyError::NotReady,
        Some(EOPNOTSUPP) => PhyError::NotSupported,
        _ if e.kind() == io::ErrorKind::Unsupported => PhyError::NotSupported,
        _ => PhyError::Io,
    }
}

/// Where joining an access point is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    Authenticating,
    Associating,
    Associated,
}

/// The access point being joined, or joined.
#[derive(Clone, Copy)]
struct Peer {
    bssid: [u8; 6],
    freq: u32,
}

/// An authentication waiting for a scan: Linux joins only access points it
/// heard itself, lately.
struct Pending {
    ssid: Vec<u8>,
    body: Vec<u8>,
}

/// What the service is told, in order.
pub enum Out {
    Frame(Vec<u8>, RxInfo),
    Ethernet(Vec<u8>),
    Event(MlmeEvent),
}

/// A radio, as the service's.
pub struct Radio {
    /// Its station interface, its nl80211 socket, and the raw packet socket
    /// its data goes through.
    pub dev: Interface,
    pub nl: Nl80211,
    pub data: File,
    pub info: PhyInfo,
    /// The frequencies scans visit, and how many networks a scan may
    /// probe for by name.
    freqs: Vec<u32>,
    max_ssids: usize,
    powered: bool,
    /// The service's scan, since when.
    scanning: Option<Instant>,
    phase: Phase,
    peer: Option<Peer>,
    pending: Option<Pending>,
    /// EAPOL that came before the association's end: Linux takes the
    /// access point's first handshake message at once, and reports the
    /// association after it.
    early_eapol: Vec<Vec<u8>>,
    signal: Option<i8>,
    /// When to ask for the signal next.
    signal_due: Instant,
    pub out: Vec<Out>,
}

impl Radio {
    /// The radio of interface `dev`, switched off, a station.
    pub fn open(dev: Interface) -> io::Result<Radio> {
        let mut nl = Nl80211::open(dev.index as u32)?;
        let interface = nl.interface()?;
        // Off until the service switches it on (and a station's).
        guest_sys::netif::set_up(&dev.name, false)?;
        if !Nl80211::is_station(&interface) {
            nl.set_station()?;
        }
        let wiphy = nl.wiphy(interface.wiphy)?;
        let mut channels = Vec::new();
        let mut freqs = Vec::new();
        for c in &wiphy.channels {
            let Some((band, number)) = channel_of(c.freq) else { continue };
            let mut flags = 0;
            if c.disabled {
                flags |= channel_flags::DISABLED;
            } else {
                freqs.push(c.freq);
            }
            if c.no_ir {
                flags |= channel_flags::NO_IR;
            }
            if c.radar {
                flags |= channel_flags::DFS;
            }
            channels.push(ChannelInfo { band, number, freq_mhz: c.freq as u16, flags, max_power_dbm: c.max_power_dbm });
        }
        let info = PhyInfo {
            driver: format!("{} (Linux)", dev.driver),
            location: dev.location.clone(),
            mac: dev.mac,
            channels,
            caps: phy_caps::MANAGED,
        };
        let data = guest_sys::netif::packet_socket(dev.index)?;
        Ok(Radio {
            dev,
            nl,
            data,
            info,
            freqs,
            max_ssids: wiphy.max_scan_ssids,
            powered: false,
            scanning: None,
            phase: Phase::Idle,
            peer: None,
            pending: None,
            early_eapol: Vec::new(),
            signal: None,
            signal_due: Instant::now(),
            out: Vec::new(),
        })
    }

    fn log(&self, what: &str, e: &io::Error) {
        match self.nl.reason() {
            Some(why) => println!("wifi: {}: {what}: {e} ({why})", self.dev.name),
            None => println!("wifi: {}: {what}: {e}", self.dev.name),
        }
    }

    fn event(&mut self, e: MlmeEvent) {
        self.out.push(Out::Event(e));
    }

    fn rx_info(&self, freq: u32, signal_dbm: i8) -> RxInfo {
        let (band, channel) = channel_of(freq).unwrap_or((Band::Ghz2, 0));
        RxInfo { channel, band: band as u8, signal_dbm, noise_dbm: 0, rate: 0 }
    }

    /// A management frame of the access point's, for the service.
    fn peer_frame(&mut self, frame: Vec<u8>) {
        let Some(p) = self.peer else { return };
        let info = self.rx_info(p.freq, self.signal.unwrap_or(0));
        self.out.push(Out::Frame(frame, info));
    }

    /// Whether a management frame is of the access point being joined.
    fn of_peer(&self, frame: &[u8]) -> bool {
        self.peer.is_some_and(|p| frame.get(16..22) == Some(&p.bssid[..]))
    }

    fn joined_nothing(&mut self) {
        self.phase = Phase::Idle;
        self.peer = None;
        self.pending = None;
        self.early_eapol.clear();
        self.signal = None;
    }

    /// Leaves the access point, if joining or joined.
    fn leave(&mut self, reason: u16) {
        if let (Some(p), true) = (self.peer, self.phase != Phase::Idle) {
            let _ = self.nl.deauthenticate(p.bssid, reason);
        }
        self.joined_nothing();
    }

    /// The access points a scan heard, as their frames: beacons, and the
    /// probe responses of the networks probed for by name.
    fn scan_results(&mut self, since: Instant) {
        let results = match self.nl.scan_results() {
            Ok(r) => r,
            Err(e) => return self.log("scan results", &e),
        };
        let age_limit = since.elapsed().as_millis() as u64 + 1000;
        for b in results {
            if b.age_ms > age_limit || channel_of(b.freq).is_none() {
                continue;
            }
            let info = self.rx_info(b.freq, b.signal_dbm);
            if let Some(ies) = &b.beacon_ies {
                self.out.push(Out::Frame(scan_frame(&b, [0xFF; 6], ies, false), info));
            }
            if let Some(ies) = &b.probe_ies {
                self.out.push(Out::Frame(scan_frame(&b, self.dev.mac, ies, true), info));
            }
        }
    }

    /// Authenticates with the access point being joined, if Linux knows it.
    fn try_authenticate(&mut self, ssid: &[u8], body: &[u8]) -> io::Result<()> {
        let Some(p) = self.peer else { return Err(io::Error::from(io::ErrorKind::NotConnected)) };
        self.nl.authenticate(p.bssid, p.freq, ssid, body)
    }

    /// What Linux reports.
    pub fn linux_event(&mut self, e: Event) {
        match e {
            Event::ScanDone => {
                if let Some(since) = self.scanning.take() {
                    self.scan_results(since);
                    self.event(MlmeEvent::ScanDone {});
                }
                // An authentication that waited for Linux to hear its access
                // point: now, or never.
                if let Some(p) = self.pending.take()
                    && let Err(e) = self.try_authenticate(&p.ssid, &p.body)
                {
                    self.log("authenticate", &e);
                    self.joined_nothing();
                    self.event(MlmeEvent::Timeout {});
                }
            }
            Event::Authentication(f) if self.of_peer(&f) => self.peer_frame(f),
            Event::Association(f) if self.of_peer(&f) => {
                // Capability, then the status: 0 is success.
                let status = f.get(26..28).map_or(1, |s| u16::from_le_bytes([s[0], s[1]]));
                self.phase = if status == 0 { Phase::Associated } else { Phase::Idle };
                self.signal_due = Instant::now();
                self.peer_frame(f);
                // The handshake's first message, after the association.
                let early = std::mem::take(&mut self.early_eapol);
                if status == 0 {
                    self.out.extend(early.into_iter().map(Out::Ethernet));
                }
            }
            Event::TimedOut(bssid) if self.peer.is_some_and(|p| p.bssid == bssid) => {
                self.joined_nothing();
                self.event(MlmeEvent::Timeout {});
            }
            Event::Left(f) if self.of_peer(&f) => {
                let ours = f.get(10..16) == Some(&self.dev.mac[..]);
                if !ours {
                    // The access point's: the service's station reads it.
                    self.peer_frame(f);
                } else if self.phase == Phase::Associated {
                    // Linux gave up on the access point (it stopped being
                    // heard).
                    self.event(MlmeEvent::LinkLost {});
                }
                self.joined_nothing();
            }
            Event::Unprotected(f) if self.of_peer(&f) => {
                let reason = f.get(24..26).map_or(0, |r| u16::from_le_bytes([r[0], r[1]]));
                self.event(MlmeEvent::UnprotectedDeauth { reason });
            }
            // Joining failed without an answer to report.
            Event::Connect { status } if status != 0 && self.phase == Phase::Associating => {
                self.joined_nothing();
                self.event(MlmeEvent::Timeout {});
            }
            Event::Disconnect if self.phase == Phase::Associated => {
                self.joined_nothing();
                self.event(MlmeEvent::LinkLost {});
            }
            Event::Eapol { source, frame } => {
                let mut eth = Vec::with_capacity(14 + frame.len());
                eth.extend_from_slice(&self.dev.mac);
                eth.extend_from_slice(&source);
                eth.extend_from_slice(&nl80211::EAPOL.to_be_bytes());
                eth.extend_from_slice(&frame);
                if self.phase == Phase::Associating {
                    if self.early_eapol.len() < EARLY_EAPOL {
                        self.early_eapol.push(eth);
                    }
                } else {
                    self.out.push(Out::Ethernet(eth));
                }
            }
            Event::Frame(f) if self.of_peer(&f) => self.peer_frame(f),
            _ => {}
        }
    }

    /// Reports the access point's signal when it changed.
    pub fn check_signal(&mut self) {
        let Some(p) = self.peer else { return };
        if self.phase != Phase::Associated || Instant::now() < self.signal_due {
            return;
        }
        self.signal_due = Instant::now() + SIGNAL_EVERY;
        if let Ok(Some(dbm)) = self.nl.signal(p.bssid)
            && self.signal != Some(dbm)
        {
            self.signal = Some(dbm);
            self.event(MlmeEvent::Signal { dbm });
        }
    }

    /// Tells the service what came: frames on `link` (dropped if it is
    /// full, as a radio drops), events on its channel `events`. `false` if
    /// the service is gone.
    pub fn flush(&mut self, link: &Link, events: &Channel) -> bool {
        for o in self.out.drain(..) {
            match o {
                Out::Frame(frame, info) => {
                    link.send(SlotMeta { kind: kind::IEEE80211, flags: 0, meta: info.pack() }, &frame);
                }
                Out::Ethernet(eth) => {
                    link.send(SlotMeta::ethernet(), &eth);
                }
                Out::Event(e) => {
                    if vipc::send_event(events, MLME_EVENT, e).is_err() {
                        return false;
                    }
                }
            }
        }
        true
    }
}

/// A Beacon (or, `probe`, a Probe Response to `to`) of what a scan heard.
fn scan_frame(b: &Bss, to: [u8; 6], ies: &[u8], probe: bool) -> Vec<u8> {
    let mut f = Vec::with_capacity(36 + ies.len());
    f.extend_from_slice(&[if probe { 0x50 } else { 0x80 }, 0, 0, 0]);
    f.extend_from_slice(&to);
    f.extend_from_slice(&b.bssid);
    f.extend_from_slice(&b.bssid);
    f.extend_from_slice(&[0, 0]);
    f.extend_from_slice(&b.tsf.to_le_bytes());
    f.extend_from_slice(&b.interval.to_le_bytes());
    f.extend_from_slice(&b.capability.to_le_bytes());
    f.extend_from_slice(ies);
    f
}

/// The service's commands.
impl wlanmlme_ctl::Server for Radio {
    fn set_power(&mut self, on: bool) -> Result<(), PhyError> {
        if on == self.powered {
            return Ok(());
        }
        if !on {
            self.leave(DEAUTH_LEAVING);
            self.scanning = None;
        }
        if let Err(e) = guest_sys::netif::set_up(&self.dev.name, on) {
            self.log("switching the radio", &e);
            return Err(phy_error(&e));
        }
        self.powered = on;
        if on && let Err(e) = self.nl.register_frame(ACTION, &SA_QUERY_RESPONSE) {
            self.log("SA Query responses", &e);
        }
        Ok(())
    }

    fn stats(&mut self) -> PhyStats {
        let d = &self.dev;
        PhyStats {
            tx_frames: d.stat("tx_packets"),
            tx_failed: d.stat("tx_errors") + d.stat("tx_dropped"),
            rx_frames: d.stat("rx_packets"),
            rx_dropped: d.stat("rx_dropped"),
        }
    }

    fn scan(&mut self, ssids: Vec<Bytes>) -> Result<(), PhyError> {
        if !self.powered {
            return Err(PhyError::NotReady);
        }
        let mut probe: Vec<&[u8]> =
            ssids.iter().map(|s| &s.0[..]).filter(|s| !s.is_empty()).take(self.max_ssids.saturating_sub(1)).collect();
        // And every network.
        probe.push(&[]);
        match self.nl.trigger_scan(&probe, &self.freqs) {
            Ok(()) => {}
            // Scanning already (for an authentication): its results do.
            Err(e) if e.raw_os_error() == Some(EBUSY) => {}
            Err(e) => {
                self.log("scan", &e);
                return Err(phy_error(&e));
            }
        }
        self.scanning.get_or_insert_with(Instant::now);
        Ok(())
    }

    fn authenticate(
        &mut self,
        bssid: [u8; 6],
        band: Band,
        channel: u8,
        ssid: Bytes,
        body: Bytes,
    ) -> Result<(), PhyError> {
        if !self.powered {
            return Err(PhyError::NotReady);
        }
        if body.0.len() < 6 {
            return Err(PhyError::NotSupported);
        }
        let freq = freq_of(band, channel);
        if self.peer.is_some_and(|p| p.bssid != bssid) {
            self.leave(DEAUTH_LEAVING);
        }
        self.pending = None;
        self.peer = Some(Peer { bssid, freq });
        self.phase = Phase::Authenticating;
        match self.try_authenticate(&ssid.0, &body.0) {
            Ok(()) => Ok(()),
            // Linux has not heard the access point lately: it listens for
            // it first.
            Err(e) if e.raw_os_error() == Some(ENOENT) => {
                match self.nl.trigger_scan(&[&ssid.0, &[]], &[freq]) {
                    Ok(()) => {}
                    Err(e) if e.raw_os_error() == Some(EBUSY) => {}
                    Err(e) => {
                        self.log("scan for the access point", &e);
                        self.joined_nothing();
                        return Err(phy_error(&e));
                    }
                }
                self.pending = Some(Pending { ssid: ssid.0, body: body.0 });
                Ok(())
            }
            Err(e) => {
                self.log("authenticate", &e);
                self.joined_nothing();
                Err(phy_error(&e))
            }
        }
    }

    fn associate(
        &mut self,
        bssid: [u8; 6],
        band: Band,
        channel: u8,
        ssid: Bytes,
        ies: Bytes,
        pmf: bool,
    ) -> Result<(), PhyError> {
        if !self.powered {
            return Err(PhyError::NotReady);
        }
        let freq = freq_of(band, channel);
        self.peer = Some(Peer { bssid, freq });
        if let Err(e) = self.nl.associate(bssid, freq, &ssid.0, &ies.0, pmf) {
            self.log("associate", &e);
            self.joined_nothing();
            return Err(phy_error(&e));
        }
        self.phase = Phase::Associating;
        Ok(())
    }

    fn deauthenticate(&mut self, _bssid: [u8; 6], reason: u16) -> Result<(), PhyError> {
        self.leave(reason);
        Ok(())
    }

    fn send_eapol(&mut self, peer: [u8; 6], frame: Bytes, encrypt: bool) -> Result<(), PhyError> {
        self.nl.send_eapol(peer, &frame.0, encrypt).map_err(|e| {
            self.log("EAPOL", &e);
            phy_error(&e)
        })
    }

    fn install_key(&mut self, kind: KeyKind, index: u8, key: Bytes, rsc: u64, peer: [u8; 6]) -> Result<(), PhyError> {
        let (cipher, peer) = match kind {
            KeyKind::Pairwise => (nl80211::CCMP_128, Some(peer)),
            KeyKind::Group => (nl80211::CCMP_128, None),
            KeyKind::Integrity => (nl80211::BIP_CMAC_128, None),
        };
        self.nl.new_key(cipher, index, &key.0, rsc, peer).map_err(|e| {
            self.log("installing a key", &e);
            phy_error(&e)
        })
    }

    fn authorize(&mut self, peer: [u8; 6]) -> Result<(), PhyError> {
        self.nl.authorize(peer).map_err(|e| {
            self.log("opening the port", &e);
            phy_error(&e)
        })
    }

    fn send_management(&mut self, frame: Bytes) -> Result<(), PhyError> {
        let Some(p) = self.peer else { return Err(PhyError::NotReady) };
        self.nl.send_frame(p.freq, &frame.0).map_err(|e| {
            self.log("management frame", &e);
            phy_error(&e)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_channel_is_its_frequency() {
        assert_eq!(channel_of(2412), Some((Band::Ghz2, 1)));
        assert_eq!(channel_of(2484), Some((Band::Ghz2, 14)));
        assert_eq!(channel_of(5180), Some((Band::Ghz5, 36)));
        assert_eq!(channel_of(5955), None);
        assert_eq!(channel_of(0), None);
        for (band, channel) in
            [(Band::Ghz2, 1), (Band::Ghz2, 13), (Band::Ghz2, 14), (Band::Ghz5, 36), (Band::Ghz5, 165)]
        {
            assert_eq!(channel_of(freq_of(band, channel)), Some((band, channel)));
        }
    }

    #[test]
    fn what_a_scan_heard_is_a_beacon_or_a_probe_response() {
        let b = Bss {
            bssid: [2, 0, 0, 0, 0, 1],
            freq: 2412,
            signal_dbm: -40,
            tsf: 0x0102_0304_0506_0708,
            interval: 100,
            capability: 0x0431,
            age_ms: 0,
            probe_ies: None,
            beacon_ies: None,
        };
        let ies = [0, 4, b'V', b'e', b'd', b'a'];
        let beacon = scan_frame(&b, [0xFF; 6], &ies, false);
        assert_eq!(beacon[0], 0x80);
        assert_eq!((&beacon[4..10], &beacon[10..16], &beacon[16..22]), (&[0xFF; 6][..], &b.bssid[..], &b.bssid[..]));
        assert_eq!(&beacon[24..32], &b.tsf.to_le_bytes());
        assert_eq!(&beacon[32..34], &100u16.to_le_bytes());
        assert_eq!(&beacon[34..36], &0x0431u16.to_le_bytes());
        assert_eq!(&beacon[36..], &ies);
        let probe = scan_frame(&b, [2, 0, 0, 0, 0, 9], &ies, true);
        assert_eq!((probe[0], &probe[4..10]), (0x50, &[2, 0, 0, 0, 0, 9][..]));
    }
}
