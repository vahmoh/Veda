// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Network interfaces, for the drivers that carry an interface's frames to
//! Veda (`net`'s Ethernet cards, `wifi`'s radios): Linux's interfaces of
//! devices, as sysfs has them ([`Interface`]); switching one on and off,
//! and a raw packet socket on one (which `rustix` does not have: their
//! addresses, options and requests are here, as Linux's headers define
//! them); and its frames between the socket and the interface's link to
//! Veda's network stack ([`Frames`]).

use std::fs::File;
use std::io::{self, Read, Write};
use std::num::NonZeroU32;

use proto::netring::{Link, SlotMeta, kind};
use rustix::ioctl::{Opcode, Updater, ioctl};
use rustix::net::addr::{SocketAddrArg, SocketAddrLen, SocketAddrOpaque};
use rustix::net::{AddressFamily, Protocol, SocketFlags, SocketType, bind, socket_with, sockopt};

use crate::setsockopt;
use crate::sysfs::{link_name, read};

/// A network interface of Linux's for a device (a card, a radio), as sysfs
/// has it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interface {
    /// Its name (`eth0`, `wlan0`), and its index, which no interface that
    /// comes later has (a name another may have: Linux gives `eth0` again).
    pub name: String,
    pub index: i32,
    /// Its link's type (`ARPHRD_*`: 1 is Ethernet's).
    pub link_type: u16,
    pub mac: [u8; 6],
    pub mtu: u32,
    /// Its radio's name (`phy0`) if it is a Wi-Fi radio's (mac80211's);
    /// whether it is a radio's at all (older drivers have no `phy80211`).
    pub phy: Option<String>,
    pub wireless: bool,
    /// Linux's driver, and where the device is (`pci 00:03.0`).
    pub driver: String,
    pub location: String,
}

impl Interface {
    /// Interface `name`, if it is a device's (not a virtual interface's)
    /// with an address.
    pub fn of(name: &str) -> Option<Interface> {
        let dir = format!("/sys/class/net/{name}");
        let device = link_name(format!("{dir}/device"))?;
        let bus = link_name(format!("{dir}/device/subsystem")).unwrap_or_default();
        let location = match bus.as_str() {
            "pci" => format!("pci {}", device.trim_start_matches("0000:")),
            _ => format!("{bus} {device}"),
        };
        // A virtual device has no driver of a bus's: its class says.
        let driver = link_name(format!("{dir}/device/driver"))
            .or_else(|| (!bus.is_empty()).then_some(bus))
            .unwrap_or_else(|| String::from("linux"));
        let digits: Vec<u8> =
            read(format!("{dir}/address")).split(':').filter_map(|d| u8::from_str_radix(d, 16).ok()).collect();
        let phy = link_name(format!("{dir}/phy80211"));
        let wireless = phy.is_some() || std::fs::metadata(format!("{dir}/wireless")).is_ok();
        Some(Interface {
            name: name.to_string(),
            index: read(format!("{dir}/ifindex")).parse().ok()?,
            link_type: read(format!("{dir}/type")).parse().ok()?,
            mac: digits.try_into().ok()?,
            mtu: read(format!("{dir}/mtu")).parse().unwrap_or(1500),
            phy,
            wireless,
            driver,
            location,
        })
    }

    /// Whether it is still there: an interface of its name with its index.
    pub fn present(&self) -> bool {
        read(format!("/sys/class/net/{}/ifindex", self.name)) == self.index.to_string()
    }

    /// Whether its link is up (it has a carrier).
    pub fn carrier(&self) -> bool {
        read(format!("/sys/class/net/{}/carrier", self.name)) == "1"
    }

    /// One of its statistics (`tx_packets`, `rx_dropped`...).
    pub fn stat(&self, name: &str) -> u64 {
        read(format!("/sys/class/net/{}/statistics/{name}", self.name)).parse().unwrap_or(0)
    }

    /// Its address, as people write it.
    pub fn mac_text(&self) -> String {
        self.mac.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":")
    }
}

/// The names of Linux's network interfaces.
pub fn interfaces() -> Vec<String> {
    std::fs::read_dir("/sys/class/net")
        .map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default()
}

/// An interface's Ethernet frames between a raw packet socket on it and
/// its link to Veda's network stack, a copy each way: Veda's to the socket
/// while it takes them (one held while it has no room, until it has), the
/// socket's to Veda (dropped while the link is full, as a card drops what
/// it has no room for).
pub struct Frames {
    to_card: Vec<u8>,
    from_card: Vec<u8>,
    /// The length of a frame of Veda's in `to_card` the socket had no room
    /// for yet.
    held: Option<usize>,
}

impl Frames {
    /// Frames of up to `slot_size` bytes (the link's slots).
    pub fn new(slot_size: usize) -> Frames {
        Frames { to_card: vec![0; slot_size], from_card: vec![0; slot_size], held: None }
    }

    /// Moves what can be moved now. An error is reading the socket's (but
    /// for having nothing to read): the interface went down, or away.
    pub fn pump(&mut self, socket: &File, link: &Link) -> io::Result<()> {
        loop {
            if let Some(len) = self.held {
                match (&*socket).write(&self.to_card[..len]) {
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    // Dropped, as a card drops what it cannot send.
                    _ => self.held = None,
                }
            }
            match link.recv(&mut self.to_card) {
                Some((meta, len)) if meta.kind == kind::ETHERNET => self.held = Some(len),
                Some(_) => {}
                None => break,
            }
        }
        loop {
            match (&*socket).read(&mut self.from_card) {
                Ok(n) => {
                    link.send(SlotMeta::ethernet(), &self.from_card[..n]);
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }

    /// Whether a frame of Veda's waits for room on the socket: then its
    /// room is waited for, and Veda's next frames wait.
    pub fn holding(&self) -> bool {
        self.held.is_some()
    }
}

/// `ETH_P_ALL`, in network byte order.
const ALL_FRAMES: u16 = 0x0003u16.to_be();
const SOL_PACKET: i32 = 263;
const PACKET_ADD_MEMBERSHIP: i32 = 1;
const PACKET_MR_ALLMULTI: u16 = 2;
const PACKET_IGNORE_OUTGOING: i32 = 23;
const SIOCGIFFLAGS: Opcode = 0x8913;
const SIOCSIFFLAGS: Opcode = 0x8914;
const IFF_UP: i16 = 1;

/// `struct ifreq`, with the flags of its union (`ifr_flags`).
#[repr(C)]
struct IfReq {
    name: [u8; 16],
    flags: i16,
    _rest: [u8; 22],
}

/// A packet socket's address (`struct sockaddr_ll`).
#[repr(C)]
struct LinkAddr {
    family: u16,
    protocol: u16,
    ifindex: i32,
    hatype: u16,
    pkttype: u8,
    halen: u8,
    addr: [u8; 8],
}

// SAFETY: the whole of a `sockaddr_ll`, which lives through the call.
unsafe impl SocketAddrArg for LinkAddr {
    unsafe fn with_sockaddr<R>(&self, f: impl FnOnce(*const SocketAddrOpaque, SocketAddrLen) -> R) -> R {
        f((self as *const LinkAddr).cast(), size_of::<LinkAddr>() as SocketAddrLen)
    }
}

/// A packet socket's membership (`struct packet_mreq`).
#[repr(C)]
#[derive(Clone, Copy)]
struct Membership {
    ifindex: i32,
    kind: u16,
    alen: u16,
    address: [u8; 8],
}

/// Switches interface `name` on (`up`) or off.
pub fn set_up(name: &str, up: bool) -> io::Result<()> {
    // A packet socket of no protocol, which receives nothing: for the
    // requests only.
    let socket = socket_with(AddressFamily::PACKET, SocketType::RAW, SocketFlags::CLOEXEC, None)?;
    let mut request = IfReq { name: [0; 16], flags: 0, _rest: [0; 22] };
    let n = name.len().min(15);
    request.name[..n].copy_from_slice(&name.as_bytes()[..n]);
    // SAFETY: the requests read an ifreq's name, and read or write its
    // flags.
    unsafe { ioctl(&socket, Updater::<SIOCGIFFLAGS, IfReq>::new(&mut request))? };
    request.flags = if up { request.flags | IFF_UP } else { request.flags & !IFF_UP };
    // SAFETY: as above.
    unsafe { ioctl(&socket, Updater::<SIOCSIFFLAGS, IfReq>::new(&mut request))? };
    Ok(())
}

/// A raw packet socket on interface `index`, not blocking: every frame it
/// receives (multicast too, which IPv6 needs), none of those it sends.
pub fn packet_socket(index: i32) -> io::Result<File> {
    let all = NonZeroU32::new(ALL_FRAMES as u32).map(Protocol::from_raw);
    let socket =
        socket_with(AddressFamily::PACKET, SocketType::RAW, SocketFlags::NONBLOCK | SocketFlags::CLOEXEC, all)?;
    let family = AddressFamily::PACKET.as_raw();
    let address =
        LinkAddr { family, protocol: ALL_FRAMES, ifindex: index, hatype: 0, pkttype: 0, halen: 0, addr: [0; 8] };
    bind(&socket, &address)?;
    setsockopt(&socket, SOL_PACKET, PACKET_IGNORE_OUTGOING, 1i32)?;
    let membership = Membership { ifindex: index, kind: PACKET_MR_ALLMULTI, alen: 0, address: [0; 8] };
    setsockopt(&socket, SOL_PACKET, PACKET_ADD_MEMBERSHIP, membership)?;
    // Room for bursts while the program is not running.
    let _ = sockopt::set_socket_recv_buffer_size_force(&socket, 4 << 20);
    Ok(File::from(socket))
}
