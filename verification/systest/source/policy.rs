// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The rules that keep one process from harming others: what the kernel
//! does with handles that travel on channels, how a server answers a request
//! it cannot serve, what the launcher, the registry and the scheduler let an
//! application do, who may change how the network is set up, and who may
//! lock and unlock the screen.

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use abi::{Error, Rights};
use ipc::{Bytes, Encoder, FLAG_REQUEST, IpcError, Refusal};
use proto::launcher;
use rt::object::{Channel, Event, Handle};
use rt::println;

use crate::{TestResult, check};

/// Whether `raw` names nothing in this process's handle table any more.
fn closed(raw: abi::RawHandle) -> bool {
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
        rt::sys::call(abi::nr::CHANNEL_CREATE, [out.as_mut_ptr() as usize, 1 << 7, 0, 0, 0, 0])
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
    check(writer.wait(abi::signals::PEER_CLOSED, 0).is_err(), "an end some handle still holds is open")?;
    drop(moved);
    check(writer.wait(abi::signals::PEER_CLOSED, 0).is_ok(), "closing the last handle closes it")?;
    Ok(())
}

ipc::protocol! {
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
    let serving = rt::thread::spawn(move || {
        for _ in 0..3 {
            let Ok(msg) = server.read_blocking(rt::time::deadline_after(rt::time::Duration::from_secs(10))) else {
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
    let too_large = c.get(abi::CHANNEL_MAX_BYTES as u32 + 1);
    check(too_large.err() == Some(IpcError::Refused(Refusal::TooLarge)), "an answer too large to send is refused")?;
    let deadline = rt::time::deadline_after(rt::time::Duration::from_secs(10));
    let unknown = ipc::call_until(c.channel(), Encoder::with_header(99, 77, FLAG_REQUEST), 77, deadline);
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
    pub const PRIORITY_DENIED: u32 = 8;
    pub const ALL: u32 = KILL_DENIED | WATCH_DENIED | FOREIGN_NAME_DENIED | PRIORITY_DENIED;
}

/// The service through which that application reports (a name of
/// systest's own, as the registry requires of it).
const VERDICT: &str = "systest/verdict";

/// How long the application's threads spin on every processor, and how long
/// it gives itself to start them all first.
const SPIN_NS: u64 = 2_000_000_000;
const SPIN_START_NS: u64 = 200_000_000;

/// The application's report: the rules that held ([`verdict`]), the
/// highest priority it could give a thread of its own, and when its threads
/// spin on every processor at that priority (monotonic nanoseconds).
struct Report {
    bits: u32,
    priority: u32,
    from: u64,
    until: u64,
}

impl Report {
    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(24);
        out.extend_from_slice(&self.bits.to_le_bytes());
        out.extend_from_slice(&self.priority.to_le_bytes());
        out.extend_from_slice(&self.from.to_le_bytes());
        out.extend_from_slice(&self.until.to_le_bytes());
        out
    }

    fn decode(b: &[u8]) -> Option<Report> {
        let word = |at: usize| Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?));
        let time = |at: usize| Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?));
        Some(Report { bits: word(0)?, priority: word(4)?, from: time(8)?, until: time(16)? })
    }
}

/// An application may not end the system's processes, hear of crashes,
/// register a name that is not its own, or keep the system's services from
/// the processors (which the scheduler gives to the highest-priority
/// threads ready): a copy of systest started through the launcher, as
/// applications are, tries the first three on a restartable service (the
/// audio service, so that a broken rule costs only a restart), to raise a
/// thread above NORMAL, the services' priority, and to make a process that
/// may, and reports what was refused. Then it spins a thread on every
/// processor, as high as it could raise one, while the window system and
/// init must go on answering.
pub fn test_application_limits() -> TestResult {
    use proto::display::display;
    let s = |e: IpcError| e.to_string();
    let l = launcher::Client::new(proto::connect(launcher::NAME).map_err(|e| alloc::format!("{e:?}"))?);
    let koid_of = |name: &str| -> Result<Option<u64>, String> {
        Ok(l.tasks().map_err(s)?.iter().find(|t| t.name == name).map(|t| t.koid))
    };
    let Some(audio) = koid_of("audio")? else {
        println!("application limits: no audio service; skipped");
        return Ok(());
    };
    // Up before the application spins: the calls made then are answers
    // alone.
    let d = display::Client::new(proto::connect(display::NAME).map_err(|e| alloc::format!("{e:?}"))?);
    d.screen_info().map_err(s)?;
    let listener = proto::register(VERDICT).map_err(|e| alloc::format!("register {VERDICT}: {e:?}"))?;
    let app = l
        .launch("/system/bin/systest".into(), vec!["limits".into(), audio.to_string()])
        .map_err(s)?
        .map_err(|e| alloc::format!("launch: {e:?}"))?;
    println!("application limits: started a copy of systest as an application (process {app})");
    let deadline = rt::time::deadline_after(rt::time::Duration::from_secs(20));
    listener.wait(abi::signals::READABLE, deadline).map_err(|e| alloc::format!("no report: {e}"))?;
    let report = proto::accept(&listener).ok_or("the application did not connect")?;
    let msg = report.read_blocking(deadline).map_err(|e| alloc::format!("no verdict: {e}"))?;
    let r = Report::decode(&msg.bytes).ok_or("a malformed verdict")?;
    // Calls to both made and answered while its threads spin on every
    // processor: none, if they keep the services from them.
    let until = r.until.min(deadline);
    rt::time::sleep_until(r.from.min(until));
    let mut answered = 0;
    while rt::time::now_ns() < until {
        if d.screen_info().is_ok() && l.tasks().is_ok() && rt::time::now_ns() < until {
            answered += 1;
        }
    }
    println!(
        "application limits: its threads spun at priority {} on every processor; the window system and init \
         answered {answered} times meanwhile",
        r.priority
    );
    check(r.bits & verdict::KILL_DENIED != 0, "an application may not end a system service")?;
    check(r.bits & verdict::WATCH_DENIED != 0, "an application may not hear of crashes")?;
    check(r.bits & verdict::FOREIGN_NAME_DENIED != 0, "an application may not register another's name")?;
    check(
        r.bits & verdict::PRIORITY_DENIED != 0,
        "an application may not raise a thread above NORMAL, nor make a process that may",
    )?;
    check(answered > 0, "the window system and init answer while an application spins on every processor")?;
    check(koid_of("audio")? == Some(audio), "the audio service still runs as it did")
}

/// Only Settings changes how the network is set up (its interfaces, its
/// DNS servers, its firewall), and only Settings and the Terminal read the
/// firewall: the network service refuses them to a system service, as to
/// any other program.
pub fn test_network_configuration() -> TestResult {
    use proto::net::{FirewallConfig, IpConfig, NetError, Scope, net};
    let c = net::Client::new(proto::connect(net::NAME).map_err(|e| alloc::format!("{e:?}"))?);
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
    if let Ok(ch) = proto::connect(launcher::NAME) {
        let l = launcher::Client::new(ch);
        if l.kill(target) == Ok(Err(proto::init::LaunchError::Denied)) {
            bits |= verdict::KILL_DENIED;
        }
        if matches!(l.watch(), Ok(Err(proto::init::LaunchError::Denied))) {
            bits |= verdict::WATCH_DENIED;
        }
    }
    // The shell's name is a system service's; any name outside systest's own
    // is refused, registered yet or not.
    if proto::register("shell").is_err() && proto::register("not-systest").is_err() {
        bits |= verdict::FOREIGN_NAME_DENIED;
    }
    // The highest priority it may give a thread of its own, and the most a
    // process it makes may have, asking for the most there is.
    let priority = (abi::priority::NORMAL + 1..=abi::priority::REALTIME)
        .rev()
        .find(|&p| rt::thread::set_priority(p).is_ok())
        .unwrap_or(abi::priority::NORMAL);
    let _ = rt::thread::set_priority(abi::priority::NORMAL);
    let child = crate::bare_process(abi::priority::REALTIME).and_then(|p| p.info()).map(|i| i.max_priority);
    if priority == abi::priority::NORMAL && child == Ok(abi::priority::NORMAL as u32) {
        bits |= verdict::PRIORITY_DENIED;
    }
    println!("application limits: the rules that held: {bits:#x} of {:#x}", verdict::ALL);
    let Ok(report) = proto::connect(VERDICT) else { return 1 };
    let from = rt::time::now_ns() + SPIN_START_NS;
    let until = from + SPIN_NS;
    let sent = Report { bits, priority: priority as u32, from, until }.encode();
    if report.write(&sent, Vec::new()).is_err() {
        return 1;
    }
    spin(priority, until);
    // Stay until the verdict is read (the channel's other end closes).
    let _ = report.wait(abi::signals::PEER_CLOSED, rt::time::deadline_after(rt::time::Duration::from_secs(20)));
    i32::from(bits != verdict::ALL)
}

/// Spins a thread on every processor at `priority` until `until`
/// (monotonic nanoseconds), as a program gone wrong might.
fn spin(priority: usize, until: u64) {
    let cpus = rt::object::system_info().map_or(1, |i| i.cpu_count.max(1) as usize);
    let spinners: Vec<_> = (0..cpus)
        .filter_map(|_| {
            let spinner = rt::thread::Builder::new().name("spinner").priority(priority);
            spinner
                .spawn(move || {
                    while rt::time::now_ns() < until {
                        core::hint::spin_loop();
                    }
                })
                .ok()
        })
        .collect();
    for s in spinners {
        let _ = s.join();
    }
}

/// The lock is the system's: a program may neither lock nor unlock the
/// screen, nor watch the lock, nor set the lock screen's password or when
/// it locks by itself (Settings alone may), nor keep the screen awake from
/// a window not its own. On a fresh system no password is set, and the
/// lock service unlocks the screen the window system starts locked.
pub fn test_the_lock() -> TestResult {
    use proto::display::{DisplayError, display};
    use proto::lock::{LockError, lock};
    let d = display::Client::new(proto::connect(display::NAME).map_err(|e| alloc::format!("{e:?}"))?);
    check(matches!(d.lock(), Ok(Err(DisplayError::Denied))), "a program may not lock the screen")?;
    check(matches!(d.unlock(), Ok(Err(DisplayError::Denied))), "a program may not unlock the screen")?;
    let (_ours, theirs) = Channel::create().map_err(|e| e.to_string())?;
    check(matches!(d.watch_lock(theirs), Ok(Err(DisplayError::Denied))), "a program may not watch the lock")?;
    // Asked for never, which would change nothing on a fresh system were
    // it taken.
    let idle = d.set_idle_timeout(0);
    check(matches!(idle, Ok(Err(DisplayError::Denied))), "a program may not set the idle timeout")?;
    let awake = d.keep_awake(u32::MAX, true);
    check(matches!(awake, Ok(Err(DisplayError::NoSuchWindow))), "a program keeps the screen awake from its windows")?;
    let active = d.user_active();
    check(matches!(active, Ok(Err(DisplayError::Denied))), "a program may not say that the user is there")?;
    let l = lock::Client::new(proto::connect(lock::NAME).map_err(|e| alloc::format!("{e:?}"))?);
    let set = l.set_password(String::new(), "veda1234".into());
    check(matches!(set, Ok(Err(LockError::Denied))), "a program may not set the lock screen's password")?;
    check(matches!(l.idle_timeout(), Ok(Err(LockError::Denied))), "a program may not read when the screen locks")?;
    let idle = l.set_idle_timeout(0);
    check(matches!(idle, Ok(Err(LockError::Denied))), "a program may not choose when the screen locks")?;
    let start = rt::time::Instant::now();
    loop {
        let s = l.status().map_err(|e| e.to_string())?;
        check(!s.password, "no password is set on a fresh system")?;
        if !s.locked {
            return Ok(());
        }
        if start.elapsed().as_millis() > 20_000 {
            return Err("the lock service did not unlock the screen".into());
        }
        rt::time::sleep(rt::time::Duration::from_millis(100));
    }
}
