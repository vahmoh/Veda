// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `systest` — integration tests that run inside Veda.
//!
//! Each test prints `<name> ... ok|FAILED: reason` and the final line is `PASS`
//! or `FAIL (n failed)`; the kernel log prefixes every line with the program
//! name, so the serial log reads `systest: PASS`. `cargo automation test`
//! boots the system with `run=systest:host,ADDRESS,TCP-PORT,UDP-PORT` on the
//! kernel command line (the servers of the host machine's that the network
//! checks reach; `systest` alone runs them without) and checks the log.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use abi::Rights;
use ipc::Bytes;
use proto::fs::{FsError, open_flags, vfs};
use proto::init::TaskEvent;
use proto::launcher;
use runtime::object::{Channel, Event, Interrupt, Vmo};
use runtime::println;
use runtime::sync::{Condvar, Mutex};

#[cfg(target_arch = "x86_64")]
#[path = "architectures/amd64/mod.rs"]
mod architecture;
mod media;
mod memory;
mod packages;
mod policy;
mod ports;
mod posix;

runtime::entry!(main);

type TestResult = Result<(), String>;

fn check(cond: bool, what: &str) -> TestResult {
    if cond { Ok(()) } else { Err(what.to_string()) }
}

fn vfs_client() -> Result<vfs::Client, String> {
    proto::connect(vfs::NAME).map(vfs::Client::new).map_err(|e| alloc::format!("connect vfs: {e:?}"))
}

fn test_vfs_system_image() -> TestResult {
    let fs = vfs_client()?;
    let entries = fs.read_dir("/system/bin".into()).map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
    check(entries.iter().any(|e| e.name == "init" && e.executable), "init listed in /system/bin, a program")?;
    let st = fs.stat("/system/etc/version".into()).map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
    check(!st.is_dir && st.size > 0 && st.read_only, "version file stat")?;
    let (vmo, len) =
        fs.read_file("/system/etc/version".into()).map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
    let mut buf = alloc::vec![0u8; len as usize];
    vmo.read(0, &mut buf).map_err(|e| e.to_string())?;
    check(buf.starts_with(b"Veda"), "version file contents")?;
    // Its pages are the system image's: read-only, and a slice of them is
    // the same bytes. There is none past their end or from inside a page,
    // nor of memory a VMO has of its own.
    check(vmo.write(0, b"x") == Err(abi::Error::AccessDenied), "a system file's pages are read-only")?;
    let pages = vmo.slice(0, len as usize).map_err(|e| e.to_string())?;
    let mut again = alloc::vec![0u8; len as usize];
    pages.read(0, &mut again).map_err(|e| e.to_string())?;
    check(again == buf, "a slice of a system file's pages")?;
    let size = vmo.size().map_err(|e| e.to_string())?;
    check(matches!(vmo.slice(0, size + 4096), Err(abi::Error::OutOfRange)), "no slice past the end")?;
    check(matches!(vmo.slice(1, 1), Err(abi::Error::InvalidArgs)), "a slice starts on a page")?;
    let own = Vmo::create(4096).map_err(|e| e.to_string())?;
    check(matches!(own.slice(0, 4096), Err(abi::Error::NotSupported)), "no slice of a VMO's own memory")?;
    let denied = fs.open("/system/etc/version".into(), open_flags::WRITE).map_err(|e| e.to_string())?;
    check(denied == Err(FsError::ReadOnly), "system image is read-only")
}

fn test_vfs_read_write() -> TestResult {
    let fs = vfs_client()?;
    let path: String = "/home/user/Documents/systest.txt".into();
    let fd = fs
        .open(path.clone(), open_flags::WRITE | open_flags::CREATE | open_flags::TRUNCATE)
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    let data: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
    let n = fs.write(fd, 0, Bytes(data.clone())).map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
    check(n as usize == data.len(), "write length")?;
    fs.close(fd).map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
    let fd = fs.open(path.clone(), open_flags::READ).map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
    let back = fs.read(fd, 0, 20_000).map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
    check(back.0 == data, "read back what was written")?;
    fs.rename(path.clone(), "/home/user/Documents/renamed.txt".into())
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    check(fs.stat(path).map_err(|e| e.to_string())? == Err(FsError::NotFound), "old name gone after rename")?;
    fs.remove("/home/user/Documents/renamed.txt".into()).map_err(|e| e.to_string())?.map_err(|e| e.to_string())
}

/// The home directory reports its space, and a file that would not fit on
/// the home disk is refused instead of being lost at the next save.
fn test_vfs_space() -> TestResult {
    let fs = vfs_client()?;
    let home = fs.space("/home/user".into()).map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
    check(home.total > 0 && home.used <= home.total, "home space reported")?;
    let system = fs.space("/system/bin".into()).map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
    check(system.used > 0 && system.used == system.total, "system image space reported")?;
    if !home.persistent {
        return Ok(()); // No home disk: memory is the only limit.
    }
    let path: String = "/home/user/Documents/too-big.bin".into();
    let len = home.total - home.used + 1;
    let vmo = Vmo::create(len as usize).map_err(|e| e.to_string())?;
    let r = fs.write_file(path.clone(), vmo, len).map_err(|e| e.to_string())?;
    check(r == Err(FsError::NoSpace), "a file larger than the free space is refused")?;
    check(fs.stat(path).map_err(|e| e.to_string())? == Err(FsError::NotFound), "nothing was created")
}

fn test_launcher() -> TestResult {
    let ch = proto::connect(launcher::NAME).map_err(|e| alloc::format!("{e:?}"))?;
    let l = launcher::Client::new(ch);
    let tasks = l.tasks().map_err(|e| e.to_string())?;
    check(tasks.iter().any(|t| t.name == "init"), "init is running")?;
    check(tasks.iter().any(|t| t.name == "filesystem"), "filesystem is running")
}

/// The supervisor in `init` restarts crashed system services: once the
/// desktop is up, kill the compositor, then wait for a new one to answer
/// display requests, for the desktop shell (which loses its windows with
/// it) to come back, and for the lock service to unlock the new one, which
/// starts locked (no password is set).
fn test_service_restart() -> TestResult {
    use proto::display::display;
    let l = launcher::Client::new(proto::connect(launcher::NAME).map_err(|e| alloc::format!("{e:?}"))?);
    let koid_of = |l: &launcher::Client, name: &str| -> Result<Option<u64>, String> {
        Ok(l.tasks().map_err(|e| e.to_string())?.iter().find(|t| t.name == name).map(|t| t.koid))
    };
    // A shell registers its service after creating its windows.
    let shell_up = || {
        let names = proto::with_registry(|r| r.list()).ok().and_then(|r| r.ok()).unwrap_or_default();
        names.iter().any(|n| n == "shell")
    };
    let wait = || runtime::time::sleep(runtime::time::Duration::from_millis(200));
    // The compositor goes with the desktop on the screen, as in use. (With
    // hardware virtualisation, systest gets here before the shell has made
    // its windows.)
    let start = runtime::time::Instant::now();
    while !shell_up() {
        if start.elapsed().as_millis() > 40_000 {
            return Err("the desktop shell did not start".into());
        }
        wait();
    }
    let shell_before = koid_of(&l, "shell")?;
    let old = koid_of(&l, "compositor")?.ok_or("the compositor is not running")?;
    l.kill(old).map_err(|e| e.to_string())?.map_err(|e| alloc::format!("kill: {e:?}"))?;
    let start = runtime::time::Instant::now();
    loop {
        if start.elapsed().as_millis() > 20_000 {
            return Err("the compositor was not restarted".into());
        }
        wait();
        if koid_of(&l, "compositor")?.is_some_and(|k| k != old) {
            break;
        }
    }
    // The registry queues this connection until the new compositor
    // registers, so the call waits for it to come up.
    let d = display::Client::new(proto::connect(display::NAME).map_err(|e| alloc::format!("{e:?}"))?);
    let info = d.screen_info().map_err(|e| e.to_string())?;
    check(info.width > 0 && info.height > 0, "restarted compositor reports the screen")?;
    loop {
        if start.elapsed().as_millis() > 40_000 {
            return Err("the desktop shell did not come back".into());
        }
        let shell = koid_of(&l, "shell")?;
        if shell.is_some() && shell != shell_before && shell_up() {
            break;
        }
        wait();
    }
    use proto::lock::lock;
    let lock = lock::Client::new(proto::connect(lock::NAME).map_err(|e| alloc::format!("{e:?}"))?);
    loop {
        if !lock.status().map_err(|e| e.to_string())?.locked {
            return Ok(());
        }
        if start.elapsed().as_millis() > 40_000 {
            return Err("the lock service did not unlock the new compositor".into());
        }
        wait();
    }
}

/// Applications that crash are reported to launcher watchers (the shell
/// shows a notification): start a copy of this program that faults.
fn test_crash_report() -> TestResult {
    let l = launcher::Client::new(proto::connect(launcher::NAME).map_err(|e| alloc::format!("{e:?}"))?);
    let events = l.watch().map_err(|e| e.to_string())?.map_err(|e| alloc::format!("watch: {e:?}"))?;
    println!("crash report: starting a copy of systest that faults on purpose");
    let koid = l
        .launch("/system/bin/systest".into(), alloc::vec!["crash".into()])
        .map_err(|e| e.to_string())?
        .map_err(|e| alloc::format!("launch: {e:?}"))?;
    let deadline = runtime::time::deadline_after(runtime::time::Duration::from_secs(10));
    // News that the installed applications changed may come first.
    let event = loop {
        let msg = events.read_blocking(deadline).map_err(|e| alloc::format!("no crash report: {e}"))?;
        match ipc::decode_event::<TaskEvent>(msg).map_err(|e| e.to_string())? {
            (_, TaskEvent::AppsChanged {}) => continue,
            (_, event) => break event,
        }
    };
    check(event == TaskEvent::Crashed { koid, name: "systest".into() }, "the report names the crashed process")
}

/// `systest crash`: the faulting child of [`test_crash_report`], which
/// faults as the architecture can on purpose.
fn crash() -> ! {
    architecture::crash()
}

fn test_threads_and_locks() -> TestResult {
    let counter = Arc::new(Mutex::new(0u64));
    let mut handles = Vec::new();
    for _ in 0..4 {
        let c = counter.clone();
        handles.push(runtime::thread::spawn(move || {
            for _ in 0..2000 {
                *c.lock() += 1;
            }
        }));
    }
    for h in handles {
        h.join().map_err(|e| e.to_string())?;
    }
    check(*counter.lock() == 8000, "mutex-protected counter")?;

    // Condition variable handshake between two threads.
    let state = Arc::new((Mutex::new(false), Condvar::new()));
    let s2 = state.clone();
    let waker = runtime::thread::spawn(move || {
        runtime::time::sleep(runtime::time::Duration::from_millis(20));
        *s2.0.lock() = true;
        s2.1.notify_all();
    });
    let mut ready = state.0.lock();
    while !*ready {
        ready = state.1.wait(ready);
    }
    drop(ready);
    waker.join().map_err(|e| e.to_string())?;

    // A thread sets its own priority (as the standard library's threads,
    // whose handles the C library keeps, do).
    let wanted = abi::priority::NORMAL - 1;
    let (set, done) =
        (Arc::new(Event::create().map_err(|e| e.to_string())?), Event::create().map_err(|e| e.to_string())?);
    let (theirs, finish) = (set.clone(), Event::from_handle(done.0.duplicate(None).map_err(|e| e.to_string())?));
    let thread = runtime::thread::spawn(move || {
        let r = runtime::thread::set_priority(wanted);
        let _ = theirs.signal();
        let _ = finish.wait(abi::signals::SIGNALED, abi::DEADLINE_INFINITE);
        r
    });
    set.wait(abi::signals::SIGNALED, abi::DEADLINE_INFINITE).map_err(|e| e.to_string())?;
    let priority = thread.thread().info().map_err(|e| e.to_string())?.priority;
    let _ = done.signal();
    thread.join().map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
    check(priority == wanted as u32, "a thread's priority, set by itself")?;
    priority_limits()
}

/// A process's threads run at its maximum priority at most, which its
/// creator gives it: a process made with `LOW` (the kernel object alone,
/// never started) reports it, its threads start at it, and none is raised
/// above it, by systest either, though one may go lower; a process made
/// without one has its creator's. (That none gets more than its creator
/// has is [`policy::test_application_limits`]'s.)
fn priority_limits() -> TestResult {
    use abi::priority::{LOW, REALTIME};
    let err = |e: abi::Error| e.to_string();
    let low = bare_process(LOW).map_err(err)?;
    let held = thread_below(&low, LOW);
    // Its thread holds it, never started: ended, it lets go.
    low.kill().map_err(err)?;
    held?;
    let own = runtime::object::process_self_info().map_err(err)?.max_priority;
    let inherited = bare_process(0).and_then(|p| p.info()).map_err(err)?.max_priority;
    check(inherited == own, "a process made without one has its creator's")?;
    check(bare_process(REALTIME + 1).err() == Some(abi::Error::InvalidArgs), "none is above REALTIME")
}

/// A thread made in `process`, whose maximum priority is `max`, below
/// `NORMAL`: it starts at `max`, is not raised above it, and goes lower.
fn thread_below(process: &runtime::object::Process, max: usize) -> TestResult {
    let err = |e: abi::Error| e.to_string();
    check(process.info().map_err(err)?.max_priority == max as u32, "a process's maximum priority, as given")?;
    let name = "main";
    let created = runtime::sys::call(
        abi::nr::THREAD_CREATE,
        [process.raw() as usize, name.as_ptr() as usize, name.len(), 0, 0, 0],
    );
    // SAFETY: a fresh handle from the kernel.
    let thread = runtime::object::Thread(unsafe { runtime::object::Handle::from_raw(created.map_err(err)? as u32) });
    let set = |p: usize| runtime::sys::call(abi::nr::THREAD_SET_PRIORITY, [thread.raw() as usize, p, 0, 0, 0, 0]);
    let at = || thread.info().map(|i| i.priority as usize).map_err(err);
    check(at()? == max, "its threads start at it, below NORMAL")?;
    check(set(max + 1).err() == Some(abi::Error::AccessDenied), "none is raised above it")?;
    check(set(max - 1).is_ok() && at()? == max - 1, "below it, one runs as set")
}

/// A process whose threads may run at `max_priority` at most (0: as high as
/// systest's): the kernel object alone, which nothing starts.
pub fn bare_process(max_priority: usize) -> Result<runtime::object::Process, abi::Error> {
    let name = "bare";
    let h = runtime::sys::call(abi::nr::PROCESS_CREATE, [name.as_ptr() as usize, name.len(), max_priority, 0, 0, 0])?;
    // SAFETY: a fresh handle from the kernel.
    Ok(runtime::object::Process(unsafe { runtime::object::Handle::from_raw(h as u32) }))
}

/// No ready thread waits for ever, nor does whoever waits for it: a thread
/// below the normal priority takes a lock, then a thread at the normal
/// priority spins on every processor, and this thread (normal too) waits
/// for the lock. The low thread never gets a processor again but for the
/// time slice at the normal priority the scheduler gives a thread that
/// has waited a second, in which it lets the lock go: this thread has the
/// lock within a few seconds, where it would wait for ever otherwise.
fn test_starved_threads_run() -> TestResult {
    use core::sync::atomic::AtomicBool;
    let err = |e: abi::Error| e.to_string();
    let lock = Arc::new(Mutex::new(()));
    let (held, go, stop) =
        (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
    let (l, h, g) = (lock.clone(), held.clone(), go.clone());
    let holder = runtime::thread::Builder::new()
        .name("holder")
        .priority(abi::priority::NORMAL - 2)
        .spawn(move || {
            let guard = l.lock();
            h.store(true, Ordering::Release);
            // Holds the lock until the processors are busy, never waiting.
            while !g.load(Ordering::Acquire) {
                core::hint::spin_loop();
            }
            drop(guard);
        })
        .map_err(err)?;
    while !held.load(Ordering::Acquire) {
        runtime::time::sleep(runtime::time::Duration::from_millis(1));
    }
    let spinners: Vec<_> = (0..runtime::pool::cpus())
        .map(|_| {
            let stop = stop.clone();
            runtime::thread::Builder::new().name("spinner").spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    core::hint::spin_loop();
                }
            })
        })
        .collect::<Result<_, _>>()
        .map_err(err)?;
    // The holder loses its processor to the spinners before it sees this.
    runtime::time::sleep(runtime::time::Duration::from_millis(50));
    go.store(true, Ordering::Release);
    let start = runtime::time::now_ns();
    let deadline = start + 5_000_000_000;
    let mut waited = None;
    while runtime::time::now_ns() < deadline {
        if lock.try_lock().is_some() {
            waited = Some(runtime::time::now_ns() - start);
            break;
        }
        runtime::time::sleep(runtime::time::Duration::from_millis(5));
    }
    stop.store(true, Ordering::Relaxed);
    for s in spinners {
        let _ = s.join();
    }
    holder.join().map_err(err)?;
    match waited {
        Some(ns) => {
            println!("starved threads run: the lock came after {} ms", ns / 1_000_000);
            Ok(())
        }
        None => Err("a thread below the normal priority, ready, never ran again to let a lock go".into()),
    }
}

fn test_ipc_primitives() -> TestResult {
    // Channel with a VMO handle travelling through it.
    let (a, b) = Channel::create().map_err(|e| e.to_string())?;
    let vmo = Vmo::create(8192).map_err(|e| e.to_string())?;
    vmo.write(4096, b"shared").map_err(|e| e.to_string())?;
    a.write(b"vmo", alloc::vec![vmo.into_handle()]).map_err(|e| e.to_string())?;
    let mut msg = b.read().map_err(|e| e.to_string())?;
    check(msg.handles.len() == 1, "handle received")?;
    let got = Vmo::from_handle(msg.handles.pop().unwrap());
    let mut buf = [0u8; 6];
    got.read(4096, &mut buf).map_err(|e| e.to_string())?;
    check(&buf == b"shared", "VMO contents shared")?;
    drop(a);
    check(b.read().err() == Some(abi::Error::PeerClosed), "peer closed detected")?;

    // Events wake waiters on another thread.
    let ev = Arc::new(Event::create().map_err(|e| e.to_string())?);
    let hits = Arc::new(AtomicU32::new(0));
    let (ev2, hits2) = (ev.clone(), hits.clone());
    let t = runtime::thread::spawn(move || {
        if ev2
            .wait(abi::signals::SIGNALED, runtime::time::deadline_after(runtime::time::Duration::from_secs(5)))
            .is_ok()
        {
            hits2.fetch_add(1, Ordering::SeqCst);
        }
    });
    runtime::time::sleep(runtime::time::Duration::from_millis(10));
    ev.signal().map_err(|e| e.to_string())?;
    t.join().map_err(|e| e.to_string())?;
    check(hits.load(Ordering::SeqCst) == 1, "event woke the waiter")?;

    // Timeouts expire.
    let start = runtime::time::Instant::now();
    let r = ev.0.wait(abi::signals::USER_0, runtime::time::deadline_after(runtime::time::Duration::from_millis(30)));
    check(r == Err(abi::Error::TimedOut) && start.elapsed().as_millis() >= 25, "wait timeout")
}

/// A sleep ends at its deadline, however near: never before it, and of a
/// hundred sleeps of 20 us, at least ten within 30 us of it, as a
/// processor's timer is armed for the deadline itself (were it armed at
/// least 50 us ahead, only a sleep that another interrupt happened to end
/// would end that soon). The sleeps are a high-priority thread's, which
/// threads still starting elsewhere do not hold up.
fn test_short_sleeps() -> TestResult {
    const SLEEP: u64 = 20_000;
    let sleeper = runtime::thread::Builder::new()
        .name("sleeper")
        .priority(abi::priority::HIGH)
        .spawn(|| {
            (0..100)
                .map(|_| {
                    let start = runtime::time::now_ns();
                    runtime::time::sleep(runtime::time::Duration::from_nanos(SLEEP));
                    runtime::time::now_ns() - start
                })
                .collect::<Vec<u64>>()
        })
        .map_err(|e| e.to_string())?;
    let took = sleeper.join().map_err(|e| e.to_string())?;
    let shortest = took.iter().copied().min().unwrap_or(0);
    let on_time = took.iter().filter(|&&t| t < SLEEP + 30_000).count();
    check(
        shortest >= SLEEP && on_time >= 10,
        &alloc::format!("of 100 sleeps of 20 us, {on_time} took less than 50 us (the shortest {shortest} ns)"),
    )
}

/// A thread's deadline interrupts the processor it sleeps on, not every
/// one: for 200 sleeps of a millisecond, the processors take fewer than
/// three timer interrupts for every two sleeps, in all (one a sleep, and
/// the rest of the system's). With one queue of deadlines for every
/// processor, each processor armed its timer for every sleeping thread's
/// deadline, and took an interrupt (and waited for the BKL) for each: more
/// than two a sleep on an idle system of four processors, and more the more
/// processors there are.
fn test_deadlines_interrupt_one_processor() -> TestResult {
    const SLEEPS: u64 = 200;
    let interrupts = || runtime::object::system_info().map(|i| i.timer_interrupts).map_err(|e| e.to_string());
    let before = interrupts()?;
    for _ in 0..SLEEPS {
        runtime::time::sleep(runtime::time::Duration::from_millis(1));
    }
    let taken = interrupts()? - before;
    check(
        taken < SLEEPS * 3 / 2,
        &alloc::format!(
            "{SLEEPS} sleeps of 1 ms took {taken} timer interrupts on {} processors",
            runtime::pool::cpus()
        ),
    )
}

/// A thread on every processor at `priority` that never waits nor makes a
/// system call, until `stop` is set.
fn spinners(priority: usize, stop: &Arc<AtomicBool>) -> Result<Vec<runtime::thread::JoinHandle<()>>, String> {
    (0..runtime::pool::cpus())
        .map(|_| {
            let stop = stop.clone();
            runtime::thread::Builder::new()
                .name("spinner")
                .priority(priority)
                .spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        core::hint::spin_loop();
                    }
                })
                .map_err(|e| e.to_string())
        })
        .collect()
}

/// A thread that wakes outranking a running one runs at once, though the
/// processor that woke it runs a thread as urgent as itself: on another
/// processor, whose less urgent thread it preempts. Every processor runs a
/// thread at the normal priority that never waits; a high-priority thread
/// wakes another of its priority, twenty times, and spins until that one
/// runs. Left to wait for a time slice to end somewhere, the woken one
/// would take milliseconds; it takes less than one at the median.
fn test_waking_preempts_elsewhere() -> TestResult {
    const ROUNDS: usize = 20;
    let stop = Arc::new(AtomicBool::new(false));
    let busy = spinners(abi::priority::NORMAL, &stop)?;
    let result = (|| {
        let event = Arc::new(Event::create().map_err(|e| e.to_string())?);
        // When the woken thread last ran (0: not since it was woken).
        let ran = Arc::new(AtomicU64::new(0));
        let (theirs, ran_at) = (event.clone(), ran.clone());
        let woken = runtime::thread::Builder::new()
            .name("woken")
            .priority(abi::priority::HIGH)
            .spawn(move || {
                for _ in 0..ROUNDS {
                    if theirs.wait(abi::signals::SIGNALED, abi::DEADLINE_INFINITE).is_err() {
                        return;
                    }
                    let _ = theirs.clear();
                    ran_at.store(runtime::time::now_ns(), Ordering::Release);
                }
            })
            .map_err(|e| e.to_string())?;
        let waker = runtime::thread::Builder::new()
            .name("waker")
            .priority(abi::priority::HIGH)
            .spawn(move || {
                (0..ROUNDS)
                    .map(|_| {
                        runtime::time::sleep(runtime::time::Duration::from_millis(1));
                        ran.store(0, Ordering::Release);
                        let start = runtime::time::now_ns();
                        let _ = event.signal();
                        // No system call: this processor stays this thread's.
                        loop {
                            let at = ran.load(Ordering::Acquire);
                            if at != 0 {
                                break at.saturating_sub(start);
                            }
                            core::hint::spin_loop();
                        }
                    })
                    .collect::<Vec<u64>>()
            })
            .map_err(|e| e.to_string())?;
        let mut took = waker.join().map_err(|e| e.to_string())?;
        woken.join().map_err(|e| e.to_string())?;
        took.sort_unstable();
        let median = took[ROUNDS / 2];
        check(
            median < 1_000_000,
            &alloc::format!(
                "a woken thread ran {} us after it was woken at the median, {} us at the latest",
                median / 1000,
                took[ROUNDS - 1] / 1000
            ),
        )
    })();
    stop.store(true, Ordering::Relaxed);
    for s in busy {
        let _ = s.join();
    }
    result
}

/// Threads above the normal priority that keep the processors busy leave
/// the normal threads most of them: a process's threads run above normal
/// for half a processor's time in every 100 ms at most, together. A
/// high-priority thread spins on every processor for 400 ms, while this
/// one, at the normal priority, counts how long it runs meanwhile: at least
/// a fifth of the time, where without the budget it would not run at all.
fn test_elevated_budget() -> TestResult {
    const SPIN: u64 = 400_000_000;
    let start = runtime::time::now_ns();
    let spinning: Vec<_> = (0..runtime::pool::cpus())
        .map(|_| {
            runtime::thread::Builder::new()
                .name("spinner")
                .priority(abi::priority::HIGH)
                .spawn(move || {
                    let mut n = 0u32;
                    loop {
                        n = n.wrapping_add(1);
                        // A system call now and then, to see the time.
                        if n.is_multiple_of(4096) && runtime::time::now_ns() >= start + SPIN {
                            break;
                        }
                        core::hint::spin_loop();
                    }
                })
                .map_err(|e| e.to_string())
        })
        .collect::<Result<_, _>>()?;
    // The time this thread ran: the gaps between its readings of the clock
    // that are too short for another thread to have run in between.
    let (mut ran, mut last) = (0, runtime::time::now_ns());
    while last < start + SPIN {
        let now = runtime::time::now_ns();
        if now - last < 50_000 {
            ran += now - last;
        }
        last = now;
    }
    for s in spinning {
        s.join().map_err(|e| e.to_string())?;
    }
    check(
        ran >= SPIN / 5,
        &alloc::format!(
            "a normal thread ran {} ms of the 400 ms high-priority threads spun everywhere",
            ran / 1_000_000
        ),
    )
}

/// A thread's CPU time counts the turn it is running, on whichever
/// processor, up to the moment it is read, and so does its process's. A
/// thread spins, never waiting nor making a system call but those that read
/// its CPU time and its process's: it sees both advance every millisecond,
/// its own by no more than the clock does; this thread, reading them while
/// the other spins, never reads less than that one last read itself; and
/// once the spinner has ended, the process has kept all its time. A turn
/// lasts until the thread waits or another takes its processor: were it
/// counted only as it ended, the spinner's time would stand still.
fn test_cpu_time_while_running() -> TestResult {
    const ROUNDS: usize = 20;
    /// The clock before and after the spinning thread read its CPU time
    /// and its process's.
    struct Reading {
        before: u64,
        thread: u64,
        process: u64,
        after: u64,
    }
    /// What the spinner shares: its last readings of its CPU time and its
    /// process's, how many it has kept, and whether it is to stop.
    #[derive(Default)]
    struct Shared {
        thread: AtomicU64,
        process: AtomicU64,
        kept: AtomicU32,
        stop: AtomicBool,
    }
    let err = |e: abi::Error| e.to_string();
    let shared = Arc::new(Shared::default());
    let theirs = shared.clone();
    let start = runtime::object::process_self_info().map_err(err)?.cpu_time_ns;
    let spinner = runtime::thread::Builder::new()
        .name("spinner")
        .spawn(move || -> Result<Vec<Reading>, abi::Error> {
            let own = runtime::sys::call(abi::nr::THREAD_SELF, [0; 6])?;
            // SAFETY: a fresh handle from the kernel.
            let own = runtime::object::Thread(unsafe { runtime::object::Handle::from_raw(own as u32) });
            // Room for every reading kept, so that none asks for memory.
            let mut readings = Vec::with_capacity(ROUNDS + 1);
            loop {
                let before = runtime::time::now_ns();
                let thread = own.info()?.cpu_time_ns;
                let process = runtime::object::process_self_info()?.cpu_time_ns;
                let after = runtime::time::now_ns();
                theirs.thread.store(thread, Ordering::Release);
                theirs.process.store(process, Ordering::Release);
                if readings.len() <= ROUNDS {
                    readings.push(Reading { before, thread, process, after });
                    theirs.kept.fetch_add(1, Ordering::Release);
                } else if theirs.stop.load(Ordering::Acquire) {
                    return Ok(readings);
                }
                while runtime::time::now_ns() < after + 1_000_000 {
                    core::hint::spin_loop();
                }
            }
        })
        .map_err(err)?;
    let watched = runtime::object::Thread(spinner.thread().0.duplicate(None).map_err(err)?);
    // This thread reads them every millisecond, until the spinner has kept
    // all its readings, each after it has seen what the spinner last read.
    let crossed = (|| {
        let deadline = runtime::time::now_ns() + 10_000_000_000;
        loop {
            runtime::time::sleep(runtime::time::Duration::from_millis(1));
            let done = shared.kept.load(Ordering::Acquire) as usize > ROUNDS;
            let (thread_seen, process_seen) =
                (shared.thread.load(Ordering::Acquire), shared.process.load(Ordering::Acquire));
            let thread = watched.info().map_err(err)?.cpu_time_ns;
            let process = runtime::object::process_self_info().map_err(err)?.cpu_time_ns;
            check(
                thread >= thread_seen && process >= process_seen,
                &alloc::format!(
                    "read by another thread as it spun, a thread's CPU time was {thread} ns and its process's \
                     {process} ns, after it had read {thread_seen} ns and {process_seen} ns itself"
                ),
            )?;
            if done {
                return Ok(());
            }
            if runtime::time::now_ns() > deadline {
                return Err(alloc::format!(
                    "the spinning thread did not read its CPU time {} times in 10 s",
                    ROUNDS + 1
                ));
            }
        }
    })();
    shared.stop.store(true, Ordering::Release);
    let readings = spinner.join().map_err(err)?.map_err(err)?;
    let (first, last) = (&readings[0], &readings[ROUNDS]);
    let still = readings.windows(2).filter(|w| w[1].thread <= w[0].thread).count();
    let still_process = readings.windows(2).filter(|w| w[1].process <= w[0].process).count();
    check(
        still == 0 && still_process == 0,
        &alloc::format!(
            "a thread spinning saw its CPU time stand still in {still} of {ROUNDS} milliseconds (from {} ns to {} \
             ns), its process's in {still_process}",
            first.thread,
            last.thread
        ),
    )?;
    if let Some(w) = readings.windows(2).find(|w| w[1].thread - w[0].thread > w[1].after - w[0].before) {
        return Err(alloc::format!(
            "a thread's CPU time went from {} ns to {} ns while the clock went on {} ns",
            w[0].thread,
            w[1].thread,
            w[1].after - w[0].before
        ));
    }
    crossed?;
    let used = watched.info().map_err(err)?.cpu_time_ns;
    let grew = runtime::object::process_self_info().map_err(err)?.cpu_time_ns.saturating_sub(start);
    check(
        grew >= used,
        &alloc::format!("a process's CPU time grew by {grew} ns while a thread of its, now ended, used {used} ns"),
    )
}

/// Write-combining memory (a display engine's pictures, which it reads
/// past the processor's caches) is as consistent as other memory: what a
/// mapping writes is what the kernel reads of it, and the other way; the
/// pages it commits read as zeros. Flags it does not know are refused.
fn test_write_combining_memory() -> TestResult {
    use abi::{map_flags, vmo_flags};
    const PAGE: usize = abi::PAGE_SIZE;
    let err = |e: abi::Error| e.to_string();
    let word = |i: usize| 0x5eda_0000_0000_0000 | i as u64;
    let vmo = Vmo::create_with(4 * PAGE, vmo_flags::WRITE_COMBINING).map_err(err)?;
    let mapped = Vmo::from_handle(vmo.0.duplicate(None).map_err(err)?);
    let map = runtime::vm::Mapping::new(mapped, 4 * PAGE, map_flags::READ | map_flags::WRITE).map_err(err)?;
    // The first page through the mapping (which commits it), the second
    // through the kernel.
    for i in 0..PAGE / 8 {
        // SAFETY: inside the mapping, which is writable.
        unsafe { (map.as_ptr() as *mut u64).add(i).write_volatile(word(i)) };
    }
    let pattern: Vec<u8> = (0..PAGE).map(|i| (i % 251) as u8).collect();
    vmo.write(PAGE, &pattern).map_err(err)?;
    let mut first = alloc::vec![0u8; PAGE];
    vmo.read(0, &mut first).map_err(err)?;
    let words = first.as_chunks::<8>().0;
    check(
        words.iter().enumerate().all(|(i, w)| u64::from_le_bytes(*w) == word(i)),
        "the kernel reads what a mapping wrote",
    )?;
    // SAFETY: inside the mapping, which is readable.
    let (second, rest) = unsafe {
        (
            core::slice::from_raw_parts(map.as_ptr().add(PAGE), PAGE),
            core::slice::from_raw_parts(map.as_ptr().add(2 * PAGE), 2 * PAGE),
        )
    };
    check(second == &pattern[..], "a mapping reads what the kernel wrote")?;
    check(rest.iter().all(|&b| b == 0), "the pages it commits read as zeros")?;
    let committed = Vmo::create_with(2 * PAGE, vmo_flags::COMMIT | vmo_flags::WRITE_COMBINING).map_err(err)?;
    let mut zeros = alloc::vec![1u8; 2 * PAGE];
    committed.read(0, &mut zeros).map_err(err)?;
    check(zeros.iter().all(|&b| b == 0), "committed at once, they read as zeros")?;
    // (No flag of `vmo_flags`.)
    check(Vmo::create_with(PAGE, 1 << 2).err() == Some(abi::Error::InvalidArgs), "flags it does not know are refused")
}

/// Interrupts a program raises: an edge signals each time; a
/// level-triggered one stays raised until it is ended, which signals its
/// event; a duplicate without `SIGNAL` cannot raise it.
fn test_software_interrupts() -> TestResult {
    let err = |e: abi::Error| e.to_string();
    let now = 0;
    let ended = Event::create().map_err(err)?;
    let level = Interrupt::create_software(true, Some(&ended)).map_err(err)?;
    check(level.wait_irq(now).is_err(), "a new interrupt is not raised")?;
    level.raise().map_err(err)?;
    level.raise().map_err(err)?;
    check(level.wait_irq(now).is_ok(), "raised")?;
    check(ended.wait(abi::signals::SIGNALED, now).is_err(), "not ended while raised")?;
    let consumer = Interrupt(level.0.duplicate(Some(Rights(Rights::BASIC.0 | Rights::WRITE.0))).map_err(err)?);
    check(consumer.raise() == Err(abi::Error::AccessDenied), "its consumer cannot raise it")?;
    consumer.ack().map_err(err)?;
    check(level.wait_irq(now).is_err(), "acknowledged")?;
    check(ended.wait(abi::signals::SIGNALED, now).is_ok(), "the ending signals its event")?;
    ended.clear().map_err(err)?;
    consumer.ack().map_err(err)?;
    check(ended.wait(abi::signals::SIGNALED, now).is_err(), "only an interrupt raised ends")?;

    let edge = Interrupt::create_software(false, None).map_err(err)?;
    edge.raise().map_err(err)?;
    check(edge.wait_irq(now).is_ok(), "an edge signals")?;
    edge.ack().map_err(err)?;
    check(edge.wait_irq(now).is_err(), "and is acknowledged")
}

/// A named test.
type Test = (&'static str, fn() -> TestResult);

fn main() -> i32 {
    match runtime::env::args().get(1).map(|a| a.as_str()) {
        Some("crash") => crash(),
        Some("hold") => return memory::hold(),
        Some("fill") => return memory::fill(),
        Some("limits") => return policy::limits(runtime::env::args().get(2)),
        Some("job") => return posix::job(),
        Some("end") => return posix::end(runtime::env::args().get(2)),
        Some("restricted") => return posix::restricted(),
        Some("unmapped") => return posix::unmapped(),
        Some("wait") => {
            posix::wait();
            return 0;
        }
        _ => {}
    }
    println!("starting");
    let tests: [Test; 52] = [
        ("starved threads run", test_starved_threads_run),
        ("ipc primitives", test_ipc_primitives),
        ("short sleeps", test_short_sleeps),
        ("deadlines interrupt one processor", test_deadlines_interrupt_one_processor),
        ("waking preempts elsewhere", test_waking_preempts_elsewhere),
        ("priorities above normal have a budget", test_elevated_budget),
        ("cpu time counts the running turn", test_cpu_time_while_running),
        ("ports", ports::test_ports),
        ("waiting on many handles", ports::test_many_handles),
        ("handles on channels", policy::test_channel_handles),
        ("shared channels", policy::test_shared_channels),
        ("network configuration", policy::test_network_configuration),
        ("ipc refusals", policy::test_ipc_refusals),
        ("write-combining memory", test_write_combining_memory),
        ("software interrupts", test_software_interrupts),
        ("sockets", posix::test_sockets),
        ("memory protection", posix::test_memory_protection),
        ("private memory", posix::test_private_memory),
        ("unmapped memory on every processor", posix::test_unmapped_on_every_processor),
        ("memory barriers on every processor", posix::test_barrier_on_every_processor),
        ("clocks read in programs", posix::test_clocks),
        ("thread exit futex", posix::test_exit_futex),
        ("futexes in shared memory", posix::test_shared_futex),
        ("threads and locks", test_threads_and_locks),
        ("vfs system image", test_vfs_system_image),
        ("vfs read/write", test_vfs_read_write),
        ("vfs space", test_vfs_space),
        ("vfs open files", posix::test_vfs_open_files),
        ("vfs shared memory", posix::test_vfs_shared_memory),
        ("programs run from the system image", posix::test_shared_program_pages),
        ("c programs", posix::test_c_programs),
        ("rust programs", posix::test_rust_programs),
        ("crates on the standard library", posix::test_std_crates),
        ("libraries loaded at run time", posix::test_plugins),
        ("python", posix::test_python),
        ("media checkasm", media::test_checkasm),
        ("media decoder", media::test_decoder),
        ("killing a job", posix::test_kill_job),
        ("signalling a job", posix::test_signal_job),
        ("ended processes go", posix::test_ended_processes_go),
        ("processes killed before they start", posix::test_killed_before_start),
        ("interruptions", posix::test_interruptions),
        ("giving abilities up", posix::test_process_restrict),
        ("launcher", test_launcher),
        ("packages", packages::test_packages),
        ("packages' applications", packages::test_applications),
        ("what applications may do", policy::test_application_limits),
        ("the lock", policy::test_the_lock),
        ("private directories", policy::test_private_directories),
        ("crash report", test_crash_report),
        ("service restart", test_service_restart),
        // Last: it fills the memory, which the others need not see.
        ("memory running out", memory::test_memory_runs_out),
    ];
    let mut failed = 0;
    for (name, f) in tests {
        match f() {
            Ok(()) => println!("{} ... ok", name),
            Err(e) => {
                failed += 1;
                println!("{} ... FAILED: {}", name, e);
            }
        }
    }
    if failed == 0 {
        println!("PASS");
        0
    } else {
        println!("FAIL ({} failed)", failed);
        1
    }
}
