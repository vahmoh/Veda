// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The rules that keep one process from harming others: what the kernel
//! does with handles that travel on channels, how a server answers a request
//! it cannot serve, what the launcher and the registry let an application
//! do, and who may change how the network is set up.

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use vabi::{Error, Rights};
use vipc::{Bytes, Encoder, FLAG_REQUEST, IpcError, Refusal};
use vproto::launcher;
use vrt::object::{Channel, Event, Handle};
use vrt::println;

use crate::{TestResult, check};

/// Whether `raw` names nothing in this process's handle table any more.
fn closed(raw: vabi::RawHandle) -> bool {
    // SAFETY: the value is only asked about, then forgotten again: nothing
    // is closed twice.
    let h = unsafe { Handle::from_raw(raw) };
    let gone = h.basic_info().err() == Some(Error::BadHandle);
    core::mem::forget(h);
    gone
}

/// A channel write takes its handles whatever happens: a refused one closes
/// them, so that none is left in the table without an owner. No end of a
/// channel may travel on it, nor a channel that could reach it through
/// queued messages (they would keep each other alive for ever).
pub fn test_channel_handles() -> TestResult {
    let err = |e: Error| e.to_string();
    let (a, b) = Channel::create().map_err(err)?;
    let event = Event::create().map_err(err)?;
    let stuck = event.0.duplicate(Some(Rights(Rights::BASIC.0 & !Rights::TRANSFER.0))).map_err(err)?;
    let raw = stuck.raw();
    check(a.write(b"x", vec![stuck]) == Err(Error::AccessDenied), "a handle that may not travel is refused")?;
    check(closed(raw), "the refused write closed it")?;
    let (c, d) = Channel::create().map_err(err)?;
    check(
        a.write(b"x", vec![b.into_handle()]) == Err(Error::NotSupported),
        "a channel's own end may not travel on it",
    )?;
    // c's queue (d's messages to it) gets a's peer... then a's queue may not
    // get c's peer.
    let (e, f) = Channel::create().map_err(err)?;
    d.write(b"x", vec![f.into_handle()]).map_err(err)?;
    check(
        e.write(b"x", vec![c.into_handle()]) == Err(Error::NotSupported),
        "channels that would keep each other alive are refused",
    )?;
    drop((a, d, e, event));
    Ok(())
}

/// A channel's end has one handle at a time, unless the channel was made
/// shared: then several handles (several processes) hold it, each message
/// is read by one of them, and the other end sees `PEER_CLOSED` only once
/// the last of them is closed.
pub fn test_shared_channels() -> TestResult {
    let err = |e: Error| e.to_string();
    let (a, _b) = Channel::create().map_err(err)?;
    check(a.0.duplicate(None).err() == Some(Error::AccessDenied), "a channel's end has one handle")?;
    let mut out = [0u32; 2];
    check(
        vrt::sys::call(vabi::nr::CHANNEL_CREATE, [out.as_mut_ptr() as usize, 1 << 7, 0, 0, 0, 0])
            == Err(Error::InvalidArgs),
        "unknown flags are refused",
    )?;
    let (inbox, writer) = Channel::create_shared().map_err(err)?;
    let other = Channel::from_handle(inbox.0.duplicate(None).map_err(err)?);
    writer.write(b"1", vec![]).map_err(err)?;
    writer.write(b"2", vec![]).map_err(err)?;
    let first = inbox.read().map_err(err)?;
    let second = other.read().map_err(err)?;
    check(first.bytes == b"1" && second.bytes == b"2", "each message is read by one holder, in order")?;
    check(inbox.read().err() == Some(Error::ShouldWait) && other.read().err() == Some(Error::ShouldWait), "and once")?;
    // A shared end travels like any other, and stays shared.
    let (c, d) = Channel::create().map_err(err)?;
    c.write(b"", vec![other.into_handle()]).map_err(err)?;
    let moved = d.read().map_err(err)?.handles.pop().map(Channel::from_handle).ok_or("no handle came")?;
    check(moved.0.duplicate(None).is_ok(), "a shared end that travelled may be duplicated")?;
    drop(inbox);
    check(writer.wait(vabi::signals::PEER_CLOSED, 0).is_err(), "an end some handle still holds is open")?;
    drop(moved);
    check(writer.wait(vabi::signals::PEER_CLOSED, 0).is_ok(), "closing the last handle closes it")?;
    Ok(())
}

vipc::protocol! {
    /// A server whose answers can be too large to send.
    mod big = "systest/big" {
        1 => fn get(len: u32) -> Bytes;
    }
}

struct Big;

impl big::Server for Big {
    fn get(&mut self, len: u32) -> Bytes {
        Bytes(vec![7; len as usize])
    }
}

/// Every request is answered: an answer larger than a channel carries, or a
/// request for a method the server does not know, fails the caller's call
/// rather than leaving it waiting for ever.
pub fn test_ipc_refusals() -> TestResult {
    let (client, server) = Channel::create().map_err(|e| e.to_string())?;
    let serving = vrt::thread::spawn(move || {
        for _ in 0..3 {
            let Ok(msg) = server.read_blocking(vrt::time::deadline_after(vrt::time::Duration::from_secs(10))) else {
                return;
            };
            if let Ok(reply) = big::dispatch(&mut Big, msg) {
                let _ = reply.send(&server);
            }
        }
    });
    let c = big::Client::new(client);
    c.set_timeout(10_000_000_000);
    check(c.get(1000).is_ok_and(|b| b.0.len() == 1000), "an answer that fits arrives")?;
    let too_large = c.get(vabi::CHANNEL_MAX_BYTES as u32 + 1);
    check(too_large.err() == Some(IpcError::Refused(Refusal::TooLarge)), "an answer too large to send is refused")?;
    let deadline = vrt::time::deadline_after(vrt::time::Duration::from_secs(10));
    let unknown = vipc::call_until(c.channel(), Encoder::with_header(99, 77, FLAG_REQUEST), 77, deadline);
    check(
        unknown.err() == Some(IpcError::Refused(Refusal::UnknownMethod)),
        "a request for an unknown method is refused",
    )?;
    let _ = serving.join();
    Ok(())
}

/// What the application started by [`test_application_limits`] reports, one
/// bit per rule that held.
mod verdict {
    pub const KILL_DENIED: u32 = 1;
    pub const WATCH_DENIED: u32 = 2;
    pub const FOREIGN_NAME_DENIED: u32 = 4;
    pub const ALL: u32 = KILL_DENIED | WATCH_DENIED | FOREIGN_NAME_DENIED;
}

/// The service through which that application reports (a name of
/// systest's own, as the registry requires of it).
const VERDICT: &str = "systest/verdict";

/// An application may not end the system's processes, hear of crashes, or
/// register a name that is not its own: a copy of systest started through
/// the launcher, as applications are, tries all three on a restartable
/// service (the audio service, so that a broken rule costs only a restart)
/// and reports what was refused.
pub fn test_application_limits() -> TestResult {
    let s = |e: IpcError| e.to_string();
    let l = launcher::Client::new(vproto::connect(launcher::NAME).map_err(|e| alloc::format!("{e:?}"))?);
    let koid_of = |name: &str| -> Result<Option<u64>, String> {
        Ok(l.tasks().map_err(s)?.iter().find(|t| t.name == name).map(|t| t.koid))
    };
    let Some(audio) = koid_of("audio")? else {
        println!("application limits: no audio service; skipped");
        return Ok(());
    };
    let listener = vproto::register(VERDICT).map_err(|e| alloc::format!("register {VERDICT}: {e:?}"))?;
    let app = l
        .launch("/system/bin/systest".into(), vec!["limits".into(), audio.to_string()])
        .map_err(s)?
        .map_err(|e| alloc::format!("launch: {e:?}"))?;
    println!("application limits: started a copy of systest as an application (process {app})");
    let deadline = vrt::time::deadline_after(vrt::time::Duration::from_secs(20));
    listener.wait(vabi::signals::READABLE, deadline).map_err(|e| alloc::format!("no report: {e}"))?;
    let report = vproto::accept(&listener).ok_or("the application did not connect")?;
    let msg = report.read_blocking(deadline).map_err(|e| alloc::format!("no verdict: {e}"))?;
    let bits = msg.bytes.first_chunk::<4>().map(|b| u32::from_le_bytes(*b)).ok_or("a malformed verdict")?;
    check(bits & verdict::KILL_DENIED != 0, "an application may not end a system service")?;
    check(bits & verdict::WATCH_DENIED != 0, "an application may not hear of crashes")?;
    check(bits & verdict::FOREIGN_NAME_DENIED != 0, "an application may not register another's name")?;
    check(koid_of("audio")? == Some(audio), "the audio service still runs as it did")
}

/// Only Settings changes how the network is set up (its interfaces, its
/// DNS servers, its firewall), and only Settings and the Terminal read the
/// firewall: the network service refuses them to a system service, as to
/// any other program.
pub fn test_network_configuration() -> TestResult {
    use vproto::net::{FirewallConfig, IpConfig, NetError, Scope, net};
    let c = net::Client::new(vproto::connect(net::NAME).map_err(|e| alloc::format!("{e:?}"))?);
    let refused = |r: Result<Result<(), NetError>, IpcError>| r == Ok(Err(NetError::PermissionDenied));
    check(
        matches!(c.firewall(), Ok(Err(NetError::PermissionDenied))),
        "the firewall's settings are refused to a service",
    )?;
    check(
        matches!(c.firewall_log(), Ok(Err(NetError::PermissionDenied))),
        "the firewall's log is refused to a service",
    )?;
    let off = FirewallConfig { enabled: false, ..FirewallConfig::default() };
    check(refused(c.set_firewall(off)), "a service may not turn the firewall off")?;
    // The questions about applications that listen, and their answers, are
    // the desktop shell's.
    check(
        matches!(c.firewall_questions(), Ok(Err(NetError::PermissionDenied))),
        "the firewall's questions are refused to a service",
    )?;
    let (_ours, theirs) = Channel::create().map_err(|e| alloc::format!("{e:?}"))?;
    check(refused(c.watch_firewall(theirs)), "a service may not hear the firewall's questions")?;
    check(
        refused(c.answer_firewall("systest".into(), Some(Scope::Anywhere {}))),
        "a service may not answer the firewall's questions",
    )?;
    check(
        refused(c.set_dns_servers(vec!["192.0.2.53".parse().map_err(|_| "an address")?])),
        "a service may not change the DNS servers",
    )?;
    check(refused(c.configure("lo".into(), IpConfig::Dhcp {})), "a service may not configure an interface")
}

/// `systest limits KOID`: the application side of
/// [`test_application_limits`].
pub fn limits(target: Option<&String>) -> i32 {
    let mut bits = 0;
    let target: u64 = target.and_then(|t| t.parse().ok()).unwrap_or(0);
    if let Ok(ch) = vproto::connect(launcher::NAME) {
        let l = launcher::Client::new(ch);
        if l.kill(target) == Ok(Err(vproto::init::LaunchError::Denied)) {
            bits |= verdict::KILL_DENIED;
        }
        if matches!(l.watch(), Ok(Err(vproto::init::LaunchError::Denied))) {
            bits |= verdict::WATCH_DENIED;
        }
    }
    // The shell's name is a system service's; any name outside systest's own
    // is refused, registered yet or not.
    if vproto::register("shell").is_err() && vproto::register("not-systest").is_err() {
        bits |= verdict::FOREIGN_NAME_DENIED;
    }
    println!("application limits: the rules that held: {bits:#x} of {:#x}", verdict::ALL);
    let Ok(report) = vproto::connect(VERDICT) else { return 1 };
    let sent: Vec<u8> = bits.to_le_bytes().to_vec();
    if report.write(&sent, Vec::new()).is_err() {
        return 1;
    }
    // Stay until the verdict is read (the channel's other end closes).
    let _ = report.wait(vabi::signals::PEER_CLOSED, vrt::time::deadline_after(vrt::time::Duration::from_secs(20)));
    i32::from(bits != verdict::ALL)
}
