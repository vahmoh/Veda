// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `net` — Veda's network driver for Linux: the guest's Ethernet cards
//! (Linux's network interfaces of PCI and USB devices; Wi-Fi radios are
//! `wifi`'s) as Veda's network devices, each attached to Veda's network
//! service (`netdev`) as Veda's own drivers attach theirs.
//!
//! A card's frames go between a raw packet socket on its interface and its
//! link to the service, a copy each way (`guest_sys::netif::Frames`).
//! Linux has no IP stack in the driver VM: it moves frames and answers
//! nothing on the network itself; addresses, DHCP, ARP and everything
//! above are Veda's. Each card has a thread, which waits on the socket and
//! the link's Veda event at once (the bridge's watches), and attaches the
//! card again whenever the service comes back. Cards that come later (USB)
//! are found as they do; a card's thread ends when it goes, and a card
//! that comes then is served anew, whatever its name (Linux gives a name
//! again: cards are known by their interfaces' indices, which it does not).
//! A card that cannot be used is tried again, later each time.

use std::fs::File;
use std::io;
use std::time::{Duration, Instant};

use abi::signals;
use guest_sys::netif::{self, Frames, Interface};
use guest_sys::retry::{self, Backoff, Ended, Served};
use proto::net::{DeviceAttachment, DeviceInfo, InterfaceKind};
use rt::guest::Watch;
use rustix::event::{PollFd, PollFlags, poll};

/// Frames each way in a card's link, and their slots (a frame and its
/// header).
const SLOTS: u32 = 256;
const SLOT_SIZE: u32 = 2048;
/// How often a card's carrier (and the guest's new cards) are looked at.
const CHECK: Duration = Duration::from_millis(500);
/// ARPHRD_ETHER.
const ETHERNET: u16 = 1;

/// Whether `i` is an Ethernet card's interface (not a radio's).
fn is_card(i: &Interface) -> bool {
    i.link_type == ETHERNET && !i.wireless
}

/// The card's interface up, and a raw packet socket on it.
fn open(card: &Interface) -> io::Result<File> {
    netif::set_up(&card.name, true)?;
    netif::packet_socket(card.index)
}

/// Serves `card` while it lasts: attached to the network service, again
/// whenever the service comes back.
fn serve(card: Interface) {
    let name = format!("net: {}", card.name);
    let mut backoff = Backoff::new(Duration::from_secs(1), Duration::from_secs(60));
    let socket = loop {
        if !card.present() {
            println!("{name}: gone");
            return;
        }
        match open(&card) {
            Ok(s) => break s,
            Err(e) => {
                println!("{name}: cannot use the card: {e}; trying again");
                backoff.wait();
            }
        }
    };
    let link = if card.carrier() { "up" } else { "down" };
    println!("{name} ({}, {}): MAC {}, link {link}", card.driver, card.location, card.mac_text());
    let attach = || {
        let info = DeviceInfo {
            kind: InterfaceKind::Ethernet,
            mac: card.mac,
            mtu: card.mtu,
            driver: format!("{} (Linux)", card.driver),
            location: card.location.clone(),
        };
        let att = DeviceAttachment::attach(info, SLOTS, SLOT_SIZE, card.carrier())
            .map_err(|e| format!("cannot attach: {e}"))?;
        println!("{name}: attached as {}", att.name);
        Ok(att)
    };
    let why = retry::sessions(&name, attach, |att| run(&name, &card, &socket, &att));
    println!("{name}: the card is gone: {why}");
}

/// Moves frames until the network service goes away ([`Ended::Left`]) or
/// the card does ([`Ended::Gone`]); an error ends the session (a watch
/// failed).
fn run(name: &str, card: &Interface, socket: &File, att: &DeviceAttachment) -> Result<Ended, String> {
    let watch = |handle, signals| Watch::new(handle, signals).map_err(|e| format!("cannot watch the link: {e}"));
    let wake = watch(att.link.wake_event().raw(), signals::SIGNALED)?;
    let closed = watch(att.channel().raw(), signals::PEER_CLOSED)?;
    let failed = |e| format!("the link's watch failed ({e}); attaching again");
    let mut frames = Frames::new(SLOT_SIZE as usize);
    let mut link_up = card.carrier();
    let mut checked = Instant::now();
    loop {
        if let Err(e) = frames.pump(socket, &att.link) {
            return Ok(Ended::Gone(format!("{e}")));
        }
        if checked.elapsed() >= CHECK {
            checked = Instant::now();
            if !card.present() {
                return Ok(Ended::Gone(String::from("its interface went")));
            }
            let up = card.carrier();
            if up != link_up {
                link_up = up;
                println!("{name}: link {}", if up { "up" } else { "down" });
                if !att.set_link(up) {
                    println!("{name}: the network service went away; attaching again");
                    return Ok(Ended::Left);
                }
            }
        }
        // Sleep until the card, the service or the link has something:
        // with a frame held, the service's frames wait for the card.
        let from_veda = !frames.holding();
        if from_veda && !att.link.prepare_wait(false) {
            continue;
        }
        let card_events = if from_veda { PollFlags::IN } else { PollFlags::IN | PollFlags::OUT };
        let mut fds = vec![PollFd::new(socket, card_events), PollFd::new(&closed, PollFlags::IN)];
        if from_veda {
            fds.push(PollFd::new(&wake, PollFlags::IN));
        }
        let _ = poll(&mut fds, guest_sys::poll_timeout(CHECK.as_millis() as i32).as_ref());
        let closing = !fds[1].revents().is_empty();
        let woken = fds.get(2).is_some_and(|f| !f.revents().is_empty());
        drop(fds);
        att.link.finish_wait();
        if closed.take(closing).map_err(failed)?.is_some() {
            println!("{name}: the network service went away; attaching again");
            return Ok(Ended::Left);
        }
        // Taken: the next poll waits for the event again.
        wake.take(woken).map_err(failed)?;
    }
}

fn main() {
    // The cards, by their interfaces' indices.
    let mut cards = Served::new(CHECK);
    loop {
        let names = netif::interfaces();
        let present = names.iter().filter_map(|i| Interface::of(i)).filter(is_card);
        cards.update(present.map(|c| (c.index, c)).collect(), Instant::now(), serve);
        std::thread::sleep(CHECK);
    }
}
