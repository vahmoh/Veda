// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Ports: a registration gives one packet when its signals are active (at
//! once if they are already), is undone with its packets, and a set of
//! more handles than one wait takes waits on a port instead of polling.

use alloc::string::ToString;
use alloc::vec::Vec;

use abi::signals::{READABLE, SIGNALED};
use abi::{Error, PortPacket};
use ipc::WaitSet;
use rt::object::{self, Channel, Event, Port};

use crate::{TestResult, check};

fn err(e: Error) -> alloc::string::String {
    e.to_string()
}

/// The packets a port holds now (none waited for).
fn packets(port: &Port) -> Result<Vec<PortPacket>, Error> {
    let mut out = [PortPacket::default(); 16];
    match port.wait_packets(&mut out, 0) {
        Ok(n) => Ok(out[..n].to_vec()),
        Err(Error::TimedOut) => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

/// One packet per registration, when its signals become active or at once
/// if they are; registered again, a signal still active gives another.
pub fn test_ports() -> TestResult {
    let port = Port::create().map_err(err)?;
    let (a, b) = Channel::create().map_err(err)?;
    object::wait_async(a.raw(), &port, 7, READABLE).map_err(err)?;
    check(packets(&port).map_err(err)?.is_empty(), "nothing before the signal")?;
    b.write(b"hi", Vec::new()).map_err(err)?;
    let got = packets(&port).map_err(err)?;
    check(got.len() == 1 && got[0].key == 7 && got[0].observed & READABLE != 0, "a packet once readable")?;
    check(packets(&port).map_err(err)?.is_empty(), "one packet per registration")?;
    // Still readable: registered again, it is done at once.
    object::wait_async(a.raw(), &port, 8, READABLE).map_err(err)?;
    let got = packets(&port).map_err(err)?;
    check(got.len() == 1 && got[0].key == 8, "a registration whose signal is active is done at once")?;

    // Undone: no packet, and one already queued goes too.
    let event = Event::create().map_err(err)?;
    object::wait_async(event.raw(), &port, 9, SIGNALED).map_err(err)?;
    object::wait_async_cancel(event.raw(), &port, 9).map_err(err)?;
    event.signal().map_err(err)?;
    check(packets(&port).map_err(err)?.is_empty(), "an undone registration gives no packet")?;
    object::wait_async(event.raw(), &port, 10, SIGNALED).map_err(err)?;
    object::wait_async_cancel(event.raw(), &port, 10).map_err(err)?;
    check(packets(&port).map_err(err)?.is_empty(), "undoing drops the packet queued")?;

    // A port is not waited for through a port, nor sent anywhere, nor
    // emptied of more packets than a wait takes.
    let other = Port::create().map_err(err)?;
    check(object::wait_async(other.raw(), &port, 11, READABLE) == Err(Error::NotSupported), "ports are not watched")?;
    let (c, _d) = Channel::create().map_err(err)?;
    let sent = c.write(b"", alloc::vec![other.into_handle()]);
    check(sent == Err(Error::AccessDenied), "a port cannot travel")?;
    let mut many = [PortPacket::default(); abi::PORT_WAIT_MAX + 1];
    let call =
        rt::sys::call(abi::nr::PORT_WAIT, [port.raw() as usize, many.as_mut_ptr() as usize, many.len(), 0, 0, 0]);
    check(call == Err(Error::InvalidArgs), "a wait takes at most PORT_WAIT_MAX packets")?;

    // A port that goes leaves its registrations harmless.
    let gone = Port::create().map_err(err)?;
    let late = Event::create().map_err(err)?;
    object::wait_async(late.raw(), &gone, 12, SIGNALED).map_err(err)?;
    drop(gone);
    late.signal().map_err(err)
}

/// A set of more handles than one call takes waits on a port: what is
/// signalled is found among thousands, at once and again, with none of the
/// others reported.
pub fn test_many_handles() -> TestResult {
    const N: usize = 3000;
    let events: Vec<Event> = (0..N).map(|_| Event::create()).collect::<Result<_, _>>().map_err(err)?;
    let mut ws = WaitSet::new();
    for round in [2500usize, 7, 1500] {
        ws.clear();
        for (i, e) in events.iter().enumerate() {
            ws.add(e.raw(), SIGNALED, i as u64);
        }
        events[round].signal().map_err(err)?;
        let deadline = rt::time::now_ns() + 5_000_000_000;
        let ready = ws.wait(deadline).map_err(err)?;
        let keys: Vec<u64> = ready.iter().map(|(k, _)| *k).collect();
        check(keys == alloc::vec![round as u64], "only the event signalled is ready")?;
        events[round].clear().map_err(err)?;
    }
    // Nothing signalled: the deadline passes, nothing is reported.
    ws.clear();
    for (i, e) in events.iter().enumerate() {
        ws.add(e.raw(), SIGNALED, i as u64);
    }
    let ready = ws.wait(rt::time::now_ns() + 20_000_000).map_err(err)?;
    check(ready.is_empty(), "a deadline that passes reports nothing")
}
