// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `wifi` — Veda's Wi-Fi driver for Linux: the guest's Wi-Fi radios (the
//! cards Linux's mac80211 drivers drive: Intel's, MediaTek's, Realtek's,
//! Qualcomm's) as Veda's *managed* radios, each offered to Veda's Wi-Fi
//! service (`wlanphy`) as Veda's own drivers offer theirs.
//!
//! Linux is the radio's MLME (`mlme`): on the service's commands
//! (`wlanmlme_ctl`), through nl80211, it scans, sends the authentication
//! and association frames, retries, encrypts and decrypts, and watches the
//! link. What decides and authenticates stays in the service: which
//! network to join, SAE, the key handshakes; Linux gets the session keys
//! only, and the driver VM has no supplicant. EAPOL comes and goes through
//! nl80211's control port, owned by this program's socket: if the program
//! ends, Linux leaves the network.
//!
//! Data is Ethernet, through a raw packet socket on the radio's interface,
//! as for `net`'s cards (`guest_sys::netif::Frames`). Each radio has a
//! thread, which waits on its sockets and on Veda's handles at once (the
//! bridge's watches), and offers the radio again whenever the service
//! comes back. Radios that come later (USB) are found as they do; a
//! radio's thread ends when its interface goes, and the radio is served
//! anew if it is still there; a radio that cannot be used yet is tried
//! again, later each time.

mod mlme;
mod nl80211;

use std::time::{Duration, Instant};

use abi::signals;
use guest_sys::netif::{self, Frames, Interface};
use guest_sys::retry::{self, Backoff, Ended, Served};
use proto::netring::Link;
use proto::wlan::{PhyInfo, RadioState, channel_flags, wlanmlme_ctl, wlanphy};
use rt::guest::Watch;
use rt::object::Channel;
use rustix::event::{PollFd, PollFlags, poll};

use mlme::Radio;

/// Frames each way in a radio's link, and their slots (a frame and its
/// header; a probe response may be 2 KiB).
const SLOTS: u32 = 256;
const SLOT_SIZE: u32 = 4096;
/// How often the guest's new radios, and a radio's presence, are looked
/// at.
const CHECK: Duration = Duration::from_millis(500);

/// The radio, offered to the Wi-Fi service.
struct Attachment {
    client: wlanphy::Client,
    link: Link,
    control: Channel,
}

fn attach(info: &PhyInfo) -> Result<Attachment, String> {
    let ch = proto::connect(wlanphy::NAME).map_err(|e| format!("cannot reach the Wi-Fi service: {e:?}"))?;
    let client = wlanphy::Client::new(ch);
    let (link, ends) = Link::create(SLOTS, SLOT_SIZE).map_err(|e| format!("cannot create the link: {e}"))?;
    let (control, theirs) = Channel::create().map_err(|_| String::from("cannot create a channel"))?;
    match client.attach(info.clone(), ends, theirs, RadioState::Ready) {
        Ok(Ok(())) => Ok(Attachment { client, link, control }),
        Ok(Err(e)) => Err(format!("the Wi-Fi service refused the radio: {e:?}")),
        Err(_) => Err("the Wi-Fi service went away".into()),
    }
}

/// Serves the radio while it lasts: offered to the Wi-Fi service, again
/// whenever the service comes back.
fn serve(dev: Interface) {
    let name = format!("wifi: {}", dev.name);
    let mut backoff = Backoff::new(Duration::from_secs(1), Duration::from_secs(60));
    let mut radio = loop {
        if !dev.present() {
            println!("{name}: gone");
            return;
        }
        match Radio::open(dev.clone()) {
            Ok(r) => break r,
            Err(e) => {
                println!("{name}: cannot use the radio: {e}; trying again");
                backoff.wait();
            }
        }
    };
    let usable = radio.info.channels.iter().filter(|c| c.flags & channel_flags::DISABLED == 0).count();
    println!("{name} ({}, {}): MAC {}, {usable} channels", dev.driver, dev.location, dev.mac_text());
    let info = radio.info.clone();
    let offer = || {
        let att = attach(&info)?;
        println!("{name}: offered to the Wi-Fi service");
        Ok(att)
    };
    let why = retry::sessions(&name, offer, |att| {
        let ended = run(&name, &mut radio, &att);
        // Whoever comes next starts from a radio switched off.
        let _ = wlanmlme_ctl::Server::set_power(&mut radio, false);
        radio.out.clear();
        ended
    });
    println!("{name}: the radio is gone: {why}");
}

/// Serves the service until it goes away ([`Ended::Left`]) or the radio
/// does ([`Ended::Gone`]); an error ends the session (a watch failed).
fn run(name: &str, radio: &mut Radio, att: &Attachment) -> Result<Ended, String> {
    let watch = |handle, signals| Watch::new(handle, signals).map_err(|e| format!("cannot watch the service: {e}"));
    let control = watch(att.control.raw(), signals::READABLE | signals::PEER_CLOSED)?;
    let wake = watch(att.link.wake_event().raw(), signals::SIGNALED)?;
    let closed = watch(att.client.channel().raw(), signals::PEER_CLOSED)?;
    let failed = |e| format!("a watch failed ({e}); offering the radio again");
    let left = || {
        println!("{name}: the Wi-Fi service went away; offering the radio again");
        Ok(Ended::Left)
    };
    let mut frames = Frames::new(SLOT_SIZE as usize);
    let mut checked = Instant::now();
    loop {
        // The service's commands, then what Linux reports.
        loop {
            match att.control.read() {
                Ok(m) => match wlanmlme_ctl::dispatch(radio, m) {
                    Ok(reply) => {
                        if reply.send(&att.control).is_err() {
                            return left();
                        }
                    }
                    Err(e) => println!("{name}: bad request from the Wi-Fi service: {e}"),
                },
                Err(abi::Error::ShouldWait) => break,
                Err(_) => return left(),
            }
        }
        match radio.nl.events() {
            Ok(events) => events.into_iter().for_each(|e| radio.linux_event(e)),
            Err(e) => return Ok(Ended::Gone(format!("its nl80211 socket failed: {e}"))),
        }
        radio.check_signal();
        if checked.elapsed() >= CHECK {
            checked = Instant::now();
            if !radio.dev.present() {
                return Ok(Ended::Gone(String::from("its interface went")));
            }
        }
        if !radio.flush(&att.link, att.client.channel()) {
            return left();
        }
        // Its data (none while the radio is off: then its interface is
        // down, which reading it says once).
        let _ = frames.pump(&radio.data, &att.link);
        // Sleep until Linux, the service or the link has something: with a
        // frame held, the service's frames wait for the radio.
        let from_veda = !frames.holding();
        if from_veda && !att.link.prepare_wait(false) {
            continue;
        }
        let timeout = if radio.nl.has_events() { 0 } else { CHECK.as_millis() as i32 };
        let radio_events = if from_veda { PollFlags::IN } else { PollFlags::IN | PollFlags::OUT };
        let mut fds = vec![
            PollFd::new(&radio.data, radio_events),
            PollFd::new(&radio.nl, PollFlags::IN),
            PollFd::new(&control, PollFlags::IN),
            PollFd::new(&closed, PollFlags::IN),
        ];
        if from_veda {
            fds.push(PollFd::new(&wake, PollFlags::IN));
        }
        let _ = poll(&mut fds, guest_sys::poll_timeout(timeout).as_ref());
        let ready = |i: usize| fds.get(i).is_some_and(|f| !f.revents().is_empty());
        let (controlled, closing, woken) = (ready(2), ready(3), ready(4));
        drop(fds);
        att.link.finish_wait();
        if closed.take(closing).map_err(failed)?.is_some() {
            return left();
        }
        // Taken: the next poll waits for them again.
        control.take(controlled).map_err(failed)?;
        wake.take(woken).map_err(failed)?;
    }
}

fn main() {
    // The radios served, by name (`phy0`): Linux never gives a radio's
    // name twice, and a radio's second interface (a monitor's) is not
    // another radio.
    let mut radios = Served::new(CHECK);
    loop {
        let names = netif::interfaces();
        let present = names.iter().filter_map(|i| Interface::of(i)).filter_map(|i| Some((i.phy.clone()?, i)));
        radios.update(present.collect(), Instant::now(), serve);
        std::thread::sleep(CHECK);
    }
}
