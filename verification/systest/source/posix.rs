// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Tests of what C programs stand on: sockets, memory protection, private
//! memory, the thread exit futex and futexes in shared memory, the VFS's
//! open-file connections and shared memory objects, ending a job, processes
//! going once they have ended and starting nothing once killed, signals
//! between processes and the interruptions they come with, and the C
//! programs themselves (`/system/tests/c`, built by the cross toolchain),
//! and the Rust programs on its standard library, which stand on the C
//! library too (`/system/tests/rust`, built by Rust's), as Python does
//! (`/system/tests/python`).

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

use abi::signals::{PEER_CLOSED, PEER_WRITE_DISABLED, READABLE};
use abi::startup::role;
use abi::{Error, Rights, map_flags, nr};
use ipc::Bytes;
use proto::fs::{FsError, device, file, open_flags, seek, vfs};
use rt::object::{Channel, Process, Socket, Vmo};
use rt::process::Spawn;
use rt::sys::call;
use rt::vm;

use crate::{TestResult, check, vfs_client};

fn s<E: core::fmt::Display>(e: E) -> String {
    e.to_string()
}

/// Sockets: a byte stream in both directions, duplicated endpoints, the
/// end of the stream, all-or-nothing small writes.
pub fn test_sockets() -> TestResult {
    let (a, b) = Socket::create().map_err(s)?;
    check(a.write(b"ping").map_err(s)? == 4, "write")?;
    let mut buf = [0u8; 16];
    check(b.read(&mut buf).map_err(s)? == 4 && &buf[..4] == b"ping", "read")?;
    check(b.read(&mut buf) == Err(Error::ShouldWait), "empty socket waits")?;
    b.write(b"pong").map_err(s)?;
    check(a.read(&mut buf).map_err(s)? == 4 && &buf[..4] == b"pong", "other direction")?;

    // A duplicate keeps the endpoint open; the peer sees the end only when
    // every handle is closed.
    let a2 = a.duplicate(None).map_err(s)?;
    drop(a);
    a2.write(b"x").map_err(s)?;
    check(b.wait(PEER_CLOSED, 0).is_err(), "not closed while a duplicate lives")?;
    drop(a2);
    check(b.read(&mut buf).map_err(s)? == 1, "data written before closing arrives")?;
    check(b.read(&mut buf) == Err(Error::PeerClosed), "then the end of the stream")?;

    // Shutting down one direction leaves the other working.
    let (c, d) = Socket::create().map_err(s)?;
    c.write(b"last").map_err(s)?;
    c.shutdown().map_err(s)?;
    check(c.write(b"more") == Err(Error::BadState), "no writes after shutdown")?;
    check(d.wait(PEER_WRITE_DISABLED, 0).is_ok(), "peer told")?;
    check(d.read(&mut buf).map_err(s)? == 4, "buffered data still read")?;
    check(d.read(&mut buf) == Err(Error::PeerClosed), "then the end")?;
    d.write(b"back").map_err(s)?;
    check(c.read(&mut buf).map_err(s)? == 4, "the other direction still works")?;

    // Fill a socket: small writes then go in whole or not at all.
    let (c, d) = Socket::create().map_err(s)?;
    let big = alloc::vec![7u8; 64 * 1024];
    let mut written = 0;
    loop {
        match c.write(&big) {
            Ok(n) => written += n,
            Err(Error::ShouldWait) => break,
            Err(e) => return Err(e.to_string()),
        }
    }
    check(written == 256 * 1024, "capacity")?;
    let info = d.info().map_err(s)?;
    check(info.readable == 256 * 1024 && c.info().map_err(s)?.writable == 0, "socket info")?;
    let mut sink = alloc::vec![0u8; 3000];
    d.read(&mut sink).map_err(s)?;
    check(c.write(&[1u8; 4096]) == Err(Error::ShouldWait), "a 4096-byte write is not split")?;
    check(c.write(&[1u8; 2000]).map_err(s)? == 2000, "a smaller one fits")?;
    check(d.wait(READABLE, 0).is_ok(), "readable")?;
    test_socket_handles()
}

/// Handles with a socket's bytes (what Unix domain sockets send
/// descriptors with): they come with the first byte written with them; a
/// read takes one write's at most and ends before the next's; a look
/// leaves them; a read that does not ask for them closes them; whoever
/// holds the endpoint reads them; and nothing goes that would keep itself
/// alive.
fn test_socket_handles() -> TestResult {
    let (c, d) = Socket::create().map_err(s)?;
    // Each handle a socket's end: closing it is seen at its peer.
    let (p1, q1) = Socket::create().map_err(s)?;
    let (p2, q2) = Socket::create().map_err(s)?;
    check(c.write_with(b"ab", alloc::vec![p1.into_handle()]).map_err(s)? == 2, "a write with a handle")?;
    c.write(b"cd").map_err(s)?;
    check(c.write_with(b"ef", alloc::vec![p2.into_handle()]).map_err(s)? == 2, "another")?;
    let mut buf = [0u8; 16];
    let (n, handles, closed) = d.read_with(&mut buf, 4).map_err(s)?;
    check(n == 4 && &buf[..4] == b"abcd" && handles.len() == 1 && closed == 0, "one write's handles, up to the next")?;
    check(handles[0].koid() != 0 && q1.wait(PEER_CLOSED, 0).is_err(), "the handle came, its object alive")?;
    drop(handles);
    check(q1.wait(PEER_CLOSED, 0).is_ok(), "closing it closes it")?;
    check(d.peek(&mut buf).map_err(s)? == 2 && &buf[..2] == b"ef", "a look")?;
    let d2 = d.duplicate(None).map_err(s)?;
    let (n, handles, _) = d2.read_with(&mut buf, 4).map_err(s)?;
    check(n == 2 && handles.len() == 1, "the handles stay through a look, for whoever holds the endpoint")?;
    drop(handles);
    check(q2.wait(PEER_CLOSED, 0).is_ok(), "the second handle")?;

    // A read that does not ask closes them; so does one with no room.
    let (p3, q3) = Socket::create().map_err(s)?;
    c.write_with(b"x", alloc::vec![p3.into_handle()]).map_err(s)?;
    check(d.read(&mut buf).map_err(s)? == 1 && q3.wait(PEER_CLOSED, 0).is_ok(), "an unasked-for handle is closed")?;
    let (p4, q4) = Socket::create().map_err(s)?;
    let (p5, q5) = Socket::create().map_err(s)?;
    c.write_with(b"y", alloc::vec![p4.into_handle(), p5.into_handle()]).map_err(s)?;
    let (n, handles, closed) = d.read_with(&mut buf, 1).map_err(s)?;
    check(n == 1 && handles.len() == 1 && closed == 1, "handles beyond the room are closed")?;
    check(q4.wait(PEER_CLOSED, 0).is_err() && q5.wait(PEER_CLOSED, 0).is_ok(), "the first kept, the other closed")?;
    drop(handles);

    // Handles go with bytes; an endpoint of the socket cannot go over it,
    // nor a channel whose queued message holds one.
    let (p6, _q6) = Socket::create().map_err(s)?;
    check(c.write_with(b"", alloc::vec![p6.into_handle()]) == Err(Error::InvalidArgs), "no byte to go with")?;
    let c2 = c.duplicate(None).map_err(s)?;
    check(c.write_with(b"z", alloc::vec![c2.into_handle()]) == Err(Error::NotSupported), "itself")?;
    let (x, y) = Channel::create().map_err(s)?;
    x.write(b"", alloc::vec![d.duplicate(None).map_err(s)?.into_handle()]).map_err(s)?;
    check(c.write_with(b"z", alloc::vec![y.into_handle()]) == Err(Error::NotSupported), "a channel that reaches it")?;
    // The other way round: a socket whose bytes hold a channel's end
    // cannot go on that channel.
    let (e, f) = Socket::create().map_err(s)?;
    let (g, h) = Channel::create().map_err(s)?;
    e.write_with(b"!", alloc::vec![h.into_handle()]).map_err(s)?;
    check(g.write(b"", alloc::vec![f.into_handle()]) == Err(Error::NotSupported), "a socket that reaches the channel")
}

/// `vm_protect` works on parts of mappings and never grants more than the
/// handle the VMO was mapped with; ranges that do not fit in user space are
/// refused.
pub fn test_memory_protection() -> TestResult {
    let vmo = Vmo::create(4 * 4096).map_err(s)?;
    let addr = vm::map(None, &vmo, 0, 4 * 4096, 0, map_flags::READ | map_flags::WRITE).map_err(s)?;
    // SAFETY: our fresh, writable mapping.
    unsafe { core::ptr::write_volatile((addr + 4096 * 3) as *mut u8, 9) };
    vm::protect(None, addr + 4096, 4096, map_flags::READ).map_err(|e| format!("partial protect: {e}"))?;
    vm::protect(None, addr + 4096, 4096, map_flags::READ | map_flags::WRITE).map_err(s)?;
    // SAFETY: as above (writable again).
    unsafe { core::ptr::write_volatile((addr + 4096) as *mut u8, 1) };
    vm::protect(None, addr + 2 * 4096, 4096, 0).map_err(|e| format!("protect none: {e}"))?;
    vm::unmap(None, addr, 4 * 4096).map_err(s)?;
    check(vm::protect(None, addr, 4096, map_flags::READ) == Err(Error::NotFound), "protecting unmapped memory fails")?;

    // A read-only handle maps read-only and stays so.
    let ro =
        Vmo::from_handle(vmo.0.duplicate(Some(Rights(Rights::BASIC.0 | Rights::READ.0 | Rights::MAP.0))).map_err(s)?);
    let at = vm::map(None, &ro, 0, 4096, 0, map_flags::READ).map_err(s)?;
    let r = vm::protect(None, at, 4096, map_flags::READ | map_flags::WRITE);
    vm::unmap(None, at, 4096).map_err(s)?;
    check(r == Err(Error::AccessDenied), "write access beyond the handle's rights is refused")?;

    // Addresses and lengths a program may get wrong, or forge: refused,
    // never wrapped around.
    let rw = map_flags::READ | map_flags::WRITE;
    let page = vm::allocate(None, 4096, 0, rw | map_flags::COMMIT).map_err(s)?;
    let high = vm::allocate(None, 8192, usize::MAX & !4095, rw).map_err(s)?;
    vm::unmap(None, high, 8192).map_err(s)?;
    check(high + 8192 <= abi::USER_SPACE_END, "a hint beyond user space is not taken")?;
    let wrap = vm::map(None, &vmo, 0, usize::MAX, page, map_flags::READ | map_flags::FIXED);
    check(wrap.is_err(), "a length that wraps is refused")?;
    for r in [
        vm::unmap(None, page, usize::MAX),
        vm::protect(None, page, usize::MAX, map_flags::READ),
        vm::decommit(None, page, usize::MAX),
    ] {
        check(r == Err(Error::InvalidArgs), "a range that wraps is refused")?;
    }
    // SAFETY: still our mapping, writable.
    unsafe { core::ptr::write_volatile(page as *mut u8, 1) };
    vm::unmap(None, page, 4096).map_err(s)
}

/// Private memory (`vm_allocate`): its pages are freed as soon as they are
/// unmapped or decommitted, and decommitted pages read as zeros.
pub fn test_private_memory() -> TestResult {
    const PAGE: usize = abi::PAGE_SIZE;
    let rw = map_flags::READ | map_flags::WRITE;
    let resident = || rt::object::process_self_info().map(|i| i.memory_bytes as usize).map_err(s);
    let addr = vm::allocate(None, 16 * PAGE, 0, rw).map_err(s)?;
    let untouched = resident()?;
    for i in 0..16 {
        // SAFETY: our fresh, writable mapping.
        unsafe { core::ptr::write_volatile((addr + i * PAGE) as *mut u8, i as u8 + 1) };
    }
    let touched = resident()?;
    check(touched >= untouched + 16 * PAGE, "pages are committed when touched")?;
    vm::unmap(None, addr + 4 * PAGE, 4 * PAGE).map_err(s)?;
    check(touched.saturating_sub(resident()?) >= 4 * PAGE, "unmapped pages are freed")?;
    vm::decommit(None, addr + 8 * PAGE, 4 * PAGE).map_err(s)?;
    check(touched.saturating_sub(resident()?) >= 8 * PAGE, "decommitted pages are freed")?;
    // SAFETY: both pages are still mapped, readable.
    let (dropped, kept) = unsafe {
        (
            core::ptr::read_volatile((addr + 8 * PAGE) as *const u8),
            core::ptr::read_volatile((addr + 12 * PAGE) as *const u8),
        )
    };
    check(dropped == 0 && kept == 13, "decommitted pages read as zeros, the others keep their contents")?;
    // A reservation gives pages back: inaccessible memory is decommitted too.
    vm::protect(None, addr + 12 * PAGE, 4 * PAGE, 0).map_err(s)?;
    vm::decommit(None, addr + 12 * PAGE, 4 * PAGE).map_err(|e| format!("decommit inaccessible pages: {e}"))?;
    check(vm::decommit(None, addr, 8 * PAGE) == Err(Error::NotFound), "a range with a hole is refused")?;
    vm::unmap(None, addr, 16 * PAGE).map_err(s)?;

    // Memory a VMO handle reaches keeps its contents.
    let vmo = Vmo::create(PAGE).map_err(s)?;
    let at = vm::map(None, &vmo, 0, PAGE, 0, rw).map_err(s)?;
    let r = vm::decommit(None, at, PAGE);
    vm::unmap(None, at, PAGE).map_err(s)?;
    check(r == Err(Error::NotSupported), "memory a VMO backs is not decommitted")
}

/// The clocks a program reads itself (`rt::time`, without a system call)
/// are the kernel's: each reading lies between the kernel's before and
/// after it, and the monotonic one never goes back.
pub fn test_clocks() -> TestResult {
    use abi::clock;
    for (id, name) in [(clock::MONOTONIC, "monotonic"), (clock::REALTIME, "real-time"), (clock::UTC, "UTC")] {
        let kernel = || call(nr::CLOCK_GET, [id, 0, 0, 0, 0, 0]).map(|v| v as u64).map_err(s);
        let before = kernel()?;
        let here = rt::sys::clock_get(id);
        let after = kernel()?;
        check(
            before <= here && here <= after,
            &format!("the {name} clock read here lies between the kernel's readings ({before} <= {here} <= {after})"),
        )?;
    }
    let mut last = rt::time::now_ns();
    for _ in 0..100_000 {
        let now = rt::time::now_ns();
        check(now >= last, "the monotonic clock never goes back")?;
        last = now;
    }
    Ok(())
}

/// Memory threads are reading on other processors is gone for them as soon
/// as the process unmaps it: the kernel has every processor running the
/// process forget the page, not only the one that unmapped it. Run in a
/// copy, which a reader's fault ends.
pub fn test_unmapped_on_every_processor() -> TestResult {
    let copy = start_copy("unmapped")?;
    let code = copy.join(rt::time::deadline_after(rt::time::Duration::from_secs(20))).map_err(s)?;
    check(code != 3, "a thread read a page after it was unmapped (see the log)")?;
    check(code == abi::EXIT_CODE_CRASHED, &format!("the copy ends with a reader's fault (it ended with {code})"))
}

/// Memory barriers on the processors running a process's threads
/// (`vm_barrier`, which the C library's `membarrier` makes): with a thread
/// busy on each other processor, each call returns once all of them have
/// gone through one, a thousand times in a few seconds at most. (That they
/// order memory, no test can see for sure: the dynamic linker relies on it
/// as it gives every thread a library's thread-local storage, which
/// `verification/c/libraries.c` checks.)
pub fn test_barrier_on_every_processor() -> TestResult {
    use core::sync::atomic::{AtomicBool, AtomicUsize};
    static STOP: AtomicBool = AtomicBool::new(false);
    static SPINNING: AtomicUsize = AtomicUsize::new(0);
    let cpus = rt::object::system_info().map_or(2, |i| i.cpu_count as usize);
    let threads: Vec<_> = (1..cpus.max(2))
        .map(|_| {
            rt::thread::spawn(|| {
                SPINNING.fetch_add(1, Ordering::AcqRel);
                while !STOP.load(Ordering::Acquire) {
                    core::hint::spin_loop();
                }
            })
        })
        .collect();
    let until = rt::time::now_ns() + 5_000_000_000;
    while SPINNING.load(Ordering::Acquire) < threads.len() && rt::time::now_ns() < until {
        core::hint::spin_loop();
    }
    let started = rt::time::now_ns();
    let done = (0..1000).try_for_each(|_| vm::barrier());
    let took = rt::time::now_ns() - started;
    STOP.store(true, Ordering::Release);
    for t in threads {
        let _ = t.join();
    }
    check(done.is_ok(), &format!("vm_barrier failed: {done:?}"))?;
    check(took < 5_000_000_000, &format!("a thousand barriers took {} ms", took / 1_000_000))
}

/// `systest unmapped`: a thread for each other processor reads a page over
/// and over while the main thread, keeping its own processor busy (so that
/// no reader runs where the unmap flushes the TLB itself), unmaps it and
/// says so. Ends with a reader's fault; 3 if a reader that knew the page
/// was unmapped could read it all the same (through a translation its
/// processor kept), 1 if the readers did not start.
pub fn unmapped() -> i32 {
    use core::sync::atomic::AtomicUsize;
    const READERS: usize = 63;
    static UNMAPPED: AtomicUsize = AtomicUsize::new(0);
    static STALE: AtomicUsize = AtomicUsize::new(0);
    static READS: [AtomicUsize; READERS] = [const { AtomicUsize::new(0) }; READERS];
    const PAGE: usize = abi::PAGE_SIZE;
    let cpus = rt::object::system_info().map_or(2, |i| i.cpu_count as usize);
    let readers = cpus.clamp(2, READERS + 1) - 1;
    let Ok(at) = vm::allocate(None, PAGE, 0, map_flags::READ | map_flags::WRITE) else { return 1 };
    // SAFETY: our fresh, writable page.
    unsafe { core::ptr::write_volatile(at as *mut u64, 0xabab_abab_abab_abab) };
    for reads in READS.iter().take(readers) {
        let _ = rt::thread::spawn(move || {
            loop {
                let unmapped = UNMAPPED.load(Ordering::Acquire) != 0;
                // SAFETY: mapped until the main thread unmaps it; a read after
                // that faults, ending the process.
                let _ = unsafe { core::ptr::read_volatile(at as *const u64) };
                if unmapped {
                    STALE.store(1, Ordering::Release);
                }
                reads.fetch_add(1, Ordering::Relaxed);
            }
        });
    }
    let until = rt::time::now_ns() + 5_000_000_000;
    while READS.iter().take(readers).any(|r| r.load(Ordering::Relaxed) < 1000) {
        if rt::time::now_ns() >= until {
            rt::println!("unmapped: the readers did not start");
            return 1;
        }
        core::hint::spin_loop();
    }
    if vm::unmap(None, at, PAGE).is_err() {
        return 1;
    }
    // Every processor that could translate the page has forgotten it.
    UNMAPPED.store(1, Ordering::Release);
    let until = rt::time::now_ns() + 2_000_000_000;
    while rt::time::now_ns() < until {
        if STALE.load(Ordering::Acquire) != 0 {
            rt::println!("unmapped: a thread read the page after it was unmapped");
            return 3;
        }
        core::hint::spin_loop();
    }
    rt::println!("unmapped: no reader read the page again");
    1
}

/// Starts a copy of systest that does `what` (see `main`), as its child.
fn start_copy(what: &str) -> Result<Process, String> {
    start_copy_with(&[what])
}

/// Starts a copy of systest with `args`, which say what it is to do.
pub(crate) fn start_copy_with(args: &[&str]) -> Result<Process, String> {
    let fs = vfs_client()?;
    let path = "/system/bin/systest";
    let (vmo, size) = fs.read_file(path.into()).map_err(s)?.map_err(|e| format!("{path}: {e}"))?;
    let image = vm::Mapping::new(vmo, size as usize, map_flags::READ).map_err(s)?;
    let registry = proto::with_registry(|r| r.clone_registry())
        .map_err(|e| format!("{e:?}"))?
        .map_err(s)?
        .map_err(|e| format!("{e:?}"))?;
    let mut spawn = Spawn::new("systest").path(path).handle(role::REGISTRY, registry.into_handle());
    for a in args {
        spawn = spawn.arg(a);
    }
    // SAFETY: a read-only mapping of the VFS's copy of the file.
    spawn.start(unsafe { image.as_slice() }).map_err(|e| format!("{path}: {e}"))
}

/// A program started from the system image's pages runs from them, which
/// are none of its memory, where a copy of it holds its code.
pub fn test_shared_program_pages() -> TestResult {
    let fs = vfs_client()?;
    let path = "/system/bin/systest";
    let (vmo, size) = fs.read_file(path.into()).map_err(s)?.map_err(|e| format!("{path}: {e}"))?;
    let size = size as usize;
    let start = |shared: bool| -> Result<Process, String> {
        let registry = proto::with_registry(|r| r.clone_registry())
            .map_err(|e| format!("{e:?}"))?
            .map_err(s)?
            .map_err(|e| format!("{e:?}"))?;
        let spawn = Spawn::new("systest").path(path).arg("wait").handle(role::REGISTRY, registry.into_handle());
        let started = if shared {
            spawn.start_file(&vmo, size)
        } else {
            let image = vm::Mapping::new(Vmo(vmo.0.duplicate(None).map_err(s)?), size, map_flags::READ).map_err(s)?;
            // SAFETY: a read-only mapping of the system image's pages.
            spawn.start(unsafe { &image.as_slice()[..size] })
        };
        started.map_err(|e| format!("{path}: {e}"))
    };
    let (shared, copied) = (start(true)?, start(false)?);
    // Both settle into their wait.
    rt::time::sleep(rt::time::Duration::from_millis(200));
    let memory = |p: &Process| p.info().map(|i| i.memory_bytes as usize).map_err(s);
    let (a, b) = (memory(&shared), memory(&copied));
    let _ = (shared.kill(), copied.kill());
    let (a, b) = (a?, b?);
    check(a + size / 4 < b, &format!("{a} bytes of memory for the program from the image's pages, {b} for a copy"))
}

/// The processes the kernel lists, however many there are.
fn processes() -> Vec<abi::ProcessInfo> {
    let mut list = alloc::vec![abi::ProcessInfo::default(); 256];
    loop {
        let n = rt::object::process_list(&mut list).unwrap_or(0);
        if n <= list.len() {
            list.truncate(n);
            return list;
        }
        list = alloc::vec![abi::ProcessInfo::default(); n + 32];
    }
}

/// The live process that `parent` started, if any.
fn child_of(parent: u64) -> Option<u64> {
    processes().iter().find(|p| p.parent_koid == parent && p.state == abi::process_state::RUNNING).map(|p| p.koid)
}

/// Whether process `koid` still runs.
fn running(koid: u64) -> bool {
    processes().iter().any(|p| p.koid == koid && p.state == abi::process_state::RUNNING)
}

/// Killing a job (`kill_tree`) ends the processes its leader started too.
pub fn test_kill_job() -> TestResult {
    let leader = start_copy("job")?;
    let koid = leader.0.koid();
    let deadline = rt::time::deadline_after(rt::time::Duration::from_secs(20));
    let member = loop {
        if let Some(c) = child_of(koid) {
            break c;
        }
        if rt::time::now_ns() > deadline {
            let _ = leader.kill_tree();
            return Err("the job's leader started nothing".into());
        }
        rt::time::sleep(rt::time::Duration::from_millis(20));
    };
    leader.kill_tree().map_err(s)?;
    leader.join(deadline).map_err(s)?;
    while running(member) {
        if rt::time::now_ns() > deadline {
            return Err("the process the leader started still runs".into());
        }
        rt::time::sleep(rt::time::Duration::from_millis(20));
    }
    Ok(())
}

/// A signal for a job (`process_signal` with `kill_flags::DESCENDANTS`, as
/// the Terminal's Ctrl+C sends `SIGINT`) reaches the processes its leader
/// started too: these take no signals (no signal entry), so one that ends
/// a process by default ends them, as that signal ends a program, and one
/// that does nothing by default leaves them alone. Sending one takes
/// `Rights::SIGNAL`; a job's, `Rights::MANAGE` too.
pub fn test_signal_job() -> TestResult {
    const SIGTERM: usize = 15;
    const SIGWINCH: usize = 28;
    let leader = start_copy("job")?;
    let koid = leader.0.koid();
    let deadline = rt::time::deadline_after(rt::time::Duration::from_secs(20));
    let member = loop {
        if let Some(c) = child_of(koid) {
            break c;
        }
        if rt::time::now_ns() > deadline {
            let _ = leader.kill_tree();
            return Err("the job's leader started nothing".into());
        }
        rt::time::sleep(rt::time::Duration::from_millis(20));
    };
    let outcome = (|| {
        let signal_only = Process(leader.0.duplicate(Some(Rights(Rights::SIGNAL.0 | Rights::WAIT.0))).map_err(s)?);
        let job = abi::kill_flags::DESCENDANTS;
        check(signal_only.signal(SIGTERM, job) == Err(Error::AccessDenied), "a job's signal without MANAGE")?;
        let watching = Process(leader.0.duplicate(Some(Rights::WAIT)).map_err(s)?);
        check(watching.signal(SIGTERM, 0) == Err(Error::AccessDenied), "a signal without SIGNAL")?;
        check(leader.signal(0, 0) == Err(Error::InvalidArgs), "signal 0")?;
        check(leader.signal(abi::posix_signal::MAX + 1, 0) == Err(Error::InvalidArgs), "a signal past the last")?;
        signal_only.signal(SIGWINCH, 0).map_err(s)?;
        leader.signal(SIGWINCH, job).map_err(s)?;
        rt::time::sleep(rt::time::Duration::from_millis(100));
        check(running(koid) && running(member), "a signal that does nothing by default ended the job")?;
        leader.signal(SIGTERM, job).map_err(s)?;
        let code = leader.join(deadline).map_err(s)?;
        check(abi::exit_signal(code) == Some(SIGTERM as u32), "the leader did not end as SIGTERM ends a program")?;
        while running(member) {
            if rt::time::now_ns() > deadline {
                return Err("the process the leader started still runs".into());
            }
            rt::time::sleep(rt::time::Duration::from_millis(20));
        }
        Ok(())
    })();
    if outcome.is_err() {
        let _ = leader.kill_tree();
    }
    outcome
}

/// A thread's interruption (`thread_interrupt`) is for a process that acts
/// on it (one with a signal entry, which systest has not): this one's
/// waits go on, as though nothing came; and only a thread of the caller's
/// own process is interrupted.
pub fn test_interruptions() -> TestResult {
    static WORD: AtomicU32 = AtomicU32::new(1);
    let me = call(nr::THREAD_SELF, [0; 6]).map_err(s)?;
    // SAFETY: a fresh handle from the kernel.
    let me = rt::object::Thread(unsafe { rt::object::Handle::from_raw(me as u32) });
    call(nr::THREAD_INTERRUPT, [me.raw() as usize, 0, 0, 0, 0, 0]).map_err(s)?;
    let deadline = rt::time::deadline_after(rt::time::Duration::from_millis(50));
    let deferred = abi::wait_flags::DEFERRED;
    let waited = call(nr::FUTEX_WAIT, [WORD.as_ptr() as usize, 1, deadline as usize, deferred, 0, 0]);
    check(waited == Err(Error::TimedOut), "a wait of a process without a signal entry was interrupted")?;
    let waited =
        call(nr::SLEEP, [rt::time::deadline_after(rt::time::Duration::from_millis(20)) as usize, 0, 0, 0, 0, 0]);
    check(waited == Ok(0), "a sleep of a process without a signal entry was interrupted")?;
    check(call(nr::SLEEP, [0, 1 << 3, 0, 0, 0, 0]) == Err(Error::InvalidArgs), "a wait's flags it has not")?;
    // Another process's thread is not this one's to interrupt.
    let copy = start_copy("wait")?;
    let outcome = (|| {
        let theirs = call(nr::THREAD_CREATE, [copy.raw() as usize, b"t".as_ptr() as usize, 1, 0, 0, 0]);
        let theirs = theirs.map_err(|e| format!("a thread of the copy: {e}"))?;
        // SAFETY: a fresh handle from the kernel.
        let theirs = rt::object::Thread(unsafe { rt::object::Handle::from_raw(theirs as u32) });
        let r = call(nr::THREAD_INTERRUPT, [theirs.raw() as usize, 0, 0, 0, 0, 0]);
        check(r == Err(Error::AccessDenied), "another process's thread was interrupted")
    })();
    let _ = copy.kill();
    outcome
}

/// `systest job`: the leader of [`test_kill_job`]'s job, which starts a
/// copy that waits and waits itself.
pub fn job() -> i32 {
    match start_copy("wait") {
        Ok(_member) => {
            wait();
            0
        }
        Err(e) => {
            rt::println!("job: {e}");
            1
        }
    }
}

/// Processes that ended go once nothing holds them: none is listed of the
/// POSIX programs started and waited for here, a few of each linking
/// (static, at a fixed address and position-independent, and dynamic),
/// nor of the copies of this program that end themselves, by returning
/// from `main`, killing themselves and with a signal of their own, once
/// their handles are closed. Until then, each is listed as ended.
pub fn test_ended_processes_go() -> TestResult {
    const SIGTERM: usize = 15;
    let deadline = rt::time::deadline_after(rt::time::Duration::from_secs(60));
    let mut ended = Vec::new();
    // Waits for `process`, `what` it is, to end with `expected`, listed as
    // ended while held, and notes it.
    let mut wait_for = |process: Process, what: &str, expected: i64| -> TestResult {
        let code = process.join(deadline).map_err(s)?;
        check(code == expected, &format!("{what} ended with {code}"))?;
        let koid = process.0.koid();
        let listed = processes().iter().any(|p| p.koid == koid && p.state != abi::process_state::RUNNING);
        check(listed, &format!("{what} is not listed as ended while held"))?;
        ended.push(koid);
        Ok(())
    };
    if vfs_client()?.stat("/system/tests/c/hello".into()).map_err(s)?.is_ok() {
        for _ in 0..3 {
            for variant in ["", "-pie", "-dynamic"] {
                let path = format!("/system/tests/c/hello{variant}");
                // What it writes stays on the socket, unread.
                let (process, _output) = start_c(&path, &[], "/tmp")?;
                wait_for(process, &path, 42)?;
            }
        }
    } else {
        rt::println!("ended processes: no c programs in this image (no cross toolchain was built)");
    }
    let signalled = abi::EXIT_CODE_SIGNALED - SIGTERM as i64;
    for (how, expected) in [("exit", 0), ("kill", abi::EXIT_CODE_KILLED), ("signal", signalled)] {
        wait_for(start_copy_with(&["end", how])?, &format!("the copy that ends by {how}"), expected)?;
    }
    let left: Vec<String> =
        processes().iter().filter(|p| ended.contains(&p.koid)).map(|p| format!("{} ({})", p.name(), p.koid)).collect();
    check(left.is_empty(), &format!("processes that ended are listed still: {}", left.join(", ")))
}

/// `systest end HOW`: ends itself by its own call, as HOW says: `exit`
/// returns from `main`, `kill` kills itself and `signal` sends itself
/// `SIGTERM`, which ends a process that takes no signals.
pub fn end(how: Option<&String>) -> i32 {
    const SIGTERM: usize = 15;
    let how = how.map_or("", String::as_str);
    let ended = match how {
        "exit" => return 0,
        "kill" => Process::current().and_then(|p| p.kill()),
        "signal" => Process::current().and_then(|p| p.signal(SIGTERM, 0)),
        _ => return 2,
    };
    rt::println!("end: running still after {how} ({ended:?})");
    1
}

/// A process killed before it starts (from the task list, which shows it
/// running, or by the out-of-memory killer) starts nothing: its thread,
/// made as `Spawn` makes it, is refused a start by `process_start`, which
/// closes the bootstrap handle as any start that fails does, and by
/// `thread_start`, and no new thread is made in it. Nothing of it is left:
/// no thread, no handle, and its thread never ran.
pub fn test_killed_before_start() -> TestResult {
    let deadline = rt::time::deadline_after(rt::time::Duration::from_secs(5));
    let process = crate::bare_process(0).map_err(s)?;
    let made = call(nr::THREAD_CREATE, [process.raw() as usize, "main".as_ptr() as usize, 4, 0, 0, 0]).map_err(s)?;
    // SAFETY: a fresh handle from the kernel.
    let thread = rt::object::Thread(unsafe { rt::object::Handle::from_raw(made as u32) });
    process.kill().map_err(s)?;
    // None of its threads had started: it ended at once.
    check(process.join(deadline).map_err(s)? == abi::EXIT_CODE_KILLED, "the killed process's exit code")?;
    // Nothing is mapped where it would start: it never does.
    let (entry, stack) = (abi::USER_SPACE_START, abi::USER_SPACE_START + abi::PAGE_SIZE);
    let (ours, theirs) = Channel::create().map_err(s)?;
    let bootstrap = theirs.into_handle().into_raw() as usize;
    let started = call(nr::PROCESS_START, [process.raw() as usize, thread.raw() as usize, entry, stack, bootstrap, 0]);
    check(started == Err(Error::BadState), &format!("process_start in a killed process: {started:?}"))?;
    check(ours.wait(PEER_CLOSED, deadline).is_ok(), "the refused start's bootstrap handle is open still")?;
    let started = call(nr::THREAD_START, [thread.raw() as usize, entry, stack, 0, 0, 0]);
    check(started == Err(Error::BadState), &format!("thread_start in a killed process: {started:?}"))?;
    let another = call(nr::THREAD_CREATE, [process.raw() as usize, "t".as_ptr() as usize, 1, 0, 0, 0]);
    // SAFETY: a fresh handle from the kernel, if it made a thread.
    let another = another.map(|h| unsafe { rt::object::Handle::from_raw(h as u32) });
    check(another.as_ref().err() == Some(&Error::BadState), "a thread was made in a killed process")?;
    let info = process.info().map_err(s)?;
    let left = (info.state, info.threads, info.handles);
    let what = format!("the killed process: state {}, {} threads, {} handles", left.0, left.1, left.2);
    check(left == (abi::process_state::KILLED, 0, 0), &what)?;
    let ran = thread.info().map_err(s)?.cpu_time_ns;
    check(ran == 0, &format!("the killed process's thread ran for {ran} ns"))
}

/// A process that gave up abilities (`process_restrict`, as the media
/// decoder does before reading a file) is refused them, and keeps the rest.
pub fn test_process_restrict() -> TestResult {
    let copy = start_copy("restricted")?;
    let code = copy.join(rt::time::deadline_after(rt::time::Duration::from_secs(20))).map_err(s)?;
    check(code == 0, "the restricted copy is refused what it gave up, and only that (see its log)")
}

/// `systest restricted`: gives up what the media decoder gives up, then
/// tries each.
pub fn restricted() -> i32 {
    match try_restricted() {
        Ok(()) => 0,
        Err(e) => {
            rt::println!("restricted: {e}");
            1
        }
    }
}

fn try_restricted() -> TestResult {
    use abi::restrict;
    const PAGE: usize = abi::PAGE_SIZE;
    let mut log = [0u8; 64];
    check(rt::object::log_read(0, &mut log).is_ok(), "the log can be read until then")?;
    check(rt::process::restrict(1 << 20) == Err(Error::InvalidArgs), "abilities it does not know are refused")?;
    rt::process::restrict(restrict::ALL).map_err(s)?;
    let denied = Err(Error::AccessDenied);
    check(rt::object::log_read(0, &mut log).map(|_| ()) == denied, "the log is refused")?;
    let mut list = [abi::ProcessInfo::default(); 4];
    check(rt::object::process_list(&mut list).map(|_| ()) == denied, "the process list is refused")?;
    let name = b"escape";
    let created = call(nr::PROCESS_CREATE, [name.as_ptr() as usize, name.len(), 0, 0, 0, 0]);
    check(created.map(|_| ()) == denied, "a new process is refused")?;
    let (rx, rw) = (map_flags::READ | map_flags::EXECUTE, map_flags::READ | map_flags::WRITE);
    check(vm::allocate(None, PAGE, 0, rx).map(|_| ()) == denied, "executable memory is refused")?;
    let vmo = Vmo::create(PAGE).map_err(s)?;
    check(vm::map(None, &vmo, 0, PAGE, 0, rx).map(|_| ()) == denied, "an executable mapping is refused")?;
    // What it keeps: memory, pages made other than executable, threads, and
    // giving up more.
    let at = vm::allocate(None, PAGE, 0, rw).map_err(s)?;
    check(vm::protect(None, at, PAGE, rx) == denied, "pages are not made executable")?;
    vm::protect(None, at, PAGE, map_flags::READ).map_err(|e| format!("pages made read-only: {e}"))?;
    vm::unmap(None, at, PAGE).map_err(s)?;
    let t = rt::thread::spawn(|| {});
    t.join().map_err(|e| format!("a thread: {e}"))?;
    rt::process::restrict(restrict::NO_LOG).map_err(s)
}

/// `systest wait`: waits until it is killed (a minute at most).
pub fn wait() {
    rt::time::sleep(rt::time::Duration::from_secs(60));
}

/// A thread's exit futex word is cleared, and its waiter woken, once the
/// thread has ended.
pub fn test_exit_futex() -> TestResult {
    static WORD: AtomicU32 = AtomicU32::new(1);
    let t = rt::thread::spawn(|| {
        let _ = call(nr::THREAD_SET_EXIT_FUTEX, [WORD.as_ptr() as usize, 0, 0, 0, 0, 0]);
    });
    let deadline = rt::time::deadline_after(rt::time::Duration::from_secs(5));
    while WORD.load(Ordering::Acquire) != 0 {
        match call(nr::FUTEX_WAIT, [WORD.as_ptr() as usize, 1, deadline as usize, 0, 0, 0]) {
            Ok(_) | Err(Error::ShouldWait) => {}
            Err(e) => return Err(format!("futex wait: {e}")),
        }
    }
    t.join().map_err(s)
}

/// Wakes the futex at `addr` until a waiter is woken, which may not be
/// waiting yet: `false` if none is within five seconds.
fn wake_one(addr: usize) -> bool {
    let deadline = rt::time::deadline_after(rt::time::Duration::from_secs(5));
    while rt::time::now_ns() < deadline {
        if call(nr::FUTEX_WAKE, [addr, 1, 0, 0, 0, 0]) == Ok(1) {
            return true;
        }
        rt::time::sleep(rt::time::Duration::from_millis(1));
    }
    false
}

/// A futex is the word in its memory, wherever that is mapped: a wait
/// through one mapping of a VMO is woken through another, as through
/// another process's; and so is a wait on a thread's exit futex there (as
/// the C library's threads end, their exit futex a word of its data, which
/// is a VMO's).
pub fn test_shared_futex() -> TestResult {
    let page = abi::PAGE_SIZE;
    let vmo = Vmo::create(page).map_err(s)?;
    let rw = map_flags::READ | map_flags::WRITE;
    let a = vmo.map(0, page, rw).map_err(s)?;
    let b = vmo.map(0, page, rw).map_err(s)?;
    let (wa, wb) = (a + 64, b + 64);
    // SAFETY: both mappings are this test's, and the word is aligned.
    let word = unsafe { &*(wa as *const AtomicU32) };
    word.store(1, Ordering::SeqCst);
    let waiter = rt::thread::spawn(move || {
        let deadline = rt::time::deadline_after(rt::time::Duration::from_secs(10));
        call(nr::FUTEX_WAIT, [wa, 1, deadline as usize, 0, 0, 0])
    });
    check(wake_one(wb), "a wait through one mapping is woken through another")?;
    check(waiter.join().map_err(s)? == Ok(0), "the wait ends woken")?;

    // A thread's exit futex: cleared as it ends, and its waiter woken,
    // through the other mapping.
    word.store(1, Ordering::SeqCst);
    let t = rt::thread::spawn(move || {
        let _ = call(nr::THREAD_SET_EXIT_FUTEX, [wa, 0, 0, 0, 0, 0]);
    });
    let deadline = rt::time::deadline_after(rt::time::Duration::from_secs(5));
    while word.load(Ordering::Acquire) != 0 {
        match call(nr::FUTEX_WAIT, [wb, 1, deadline as usize, 0, 0, 0]) {
            Ok(_) | Err(Error::ShouldWait) => {}
            Err(e) => return Err(format!("waiting for a thread to end: {e}")),
        }
    }
    t.join().map_err(s)?;
    vm::unmap(None, a, page).map_err(s)?;
    vm::unmap(None, b, page).map_err(s)
}

fn open_file(fs: &vfs::Client, path: &str, flags: u32) -> Result<file::Client, String> {
    let (ch, _) = fs.open_file(path.into(), flags).map_err(s)?.map_err(|e| format!("open {path}: {e}"))?;
    Ok(file::Client::new(ch))
}

/// `/dev/shm`'s shared memory objects: files whose bytes are memory, which
/// their connections hand out to map, shared with what else maps it; their
/// names, links among them, and their lives.
pub fn test_vfs_shared_memory() -> TestResult {
    let fs = vfs_client()?;
    let (path, other) = ("/dev/shm/systest", "/dev/shm/systest-link");
    let rw = open_flags::READ | open_flags::WRITE;
    let f = open_file(&fs, path, rw | open_flags::CREATE | open_flags::EXCLUSIVE)?;
    let st = f.stat().map_err(s)?.map_err(s)?;
    check(st.device == device::SHM && st.size == 0 && !st.is_dir, "a new object, empty")?;
    f.truncate(5000).map_err(s)?.map_err(s)?;
    check(f.stat().map_err(s)?.map_err(s)?.size == 5000, "it has the size it was given")?;
    // Its memory, mapped past its size, is what its files read.
    let page = abi::PAGE_SIZE;
    let vmo = f.memory(2 * page as u64).map_err(s)?.map_err(s)?;
    let at = vmo.map(0, 2 * page, map_flags::READ | map_flags::WRITE).map_err(s)?;
    // SAFETY: the mapping was just made, two pages, writable.
    unsafe { core::ptr::copy_nonoverlapping(b"shared".as_ptr(), (at + 100) as *mut u8, 6) };
    let g = open_file(&fs, path, open_flags::READ)?;
    check(g.read_at(100, 6).map_err(s)?.map_err(s)?.0 == b"shared", "files read what its mappings write")?;
    check(g.stat().map_err(s)?.map_err(s)?.size == 5000, "a mapping leaves the size alone")?;
    f.write_at(200, Bytes(b"written".to_vec())).map_err(s)?.map_err(s)?;
    // SAFETY: within the mapping.
    let seen = unsafe { core::slice::from_raw_parts((at + 200) as *const u8, 7) };
    check(seen == b"written", "mappings see what its files write")?;
    vm::unmap(None, at, 2 * page).map_err(s)?;
    // Opened to read, its memory cannot be written.
    let ro = g.memory(page as u64).map_err(s)?.map_err(s)?;
    check(ro.map(0, page, map_flags::READ | map_flags::WRITE) == Err(Error::AccessDenied), "read-only memory")?;
    let r = ro.map(0, page, map_flags::READ).map_err(s)?;
    vm::unmap(None, r, page).map_err(s)?;
    let plain = open_file(&fs, "/tmp/systest-plain", rw | open_flags::CREATE)?;
    check(matches!(plain.memory(page as u64).map_err(s)?, Err(FsError::Invalid)), "only objects have memory")?;
    drop(plain);
    fs.remove("/tmp/systest-plain".into()).map_err(s)?.map_err(s)?;

    // Names: links, which are the object's own, and its directory's.
    fs.link(path.into(), other.into()).map_err(s)?.map_err(s)?;
    let (a, b) = (fs.stat(path.into()).map_err(s)?.map_err(s)?, fs.stat(other.into()).map_err(s)?.map_err(s)?);
    check(a.inode == b.inode && b.size == 5000, "a link names the same object")?;
    check(fs.link(path.into(), other.into()).map_err(s)? == Err(FsError::Exists), "a name taken is not given")?;
    check(fs.link("/tmp/x".into(), other.into()).map_err(s)? == Err(FsError::Invalid), "nothing else has links")?;
    let names: Vec<String> =
        fs.read_dir("/dev/shm".into()).map_err(s)?.map_err(s)?.into_iter().map(|e| e.name).collect();
    check(names.iter().any(|n| n == "systest") && names.iter().any(|n| n == "systest-link"), "listed")?;
    let dev = fs.read_dir("/dev".into()).map_err(s)?.map_err(s)?;
    check(dev.iter().any(|e| e.name == "shm" && e.is_dir), "/dev lists it")?;
    check(fs.mkdir("/dev/shm/dir".into()).map_err(s)? == Err(FsError::ReadOnly), "objects alone are there")?;

    // Removed, it stays for what has it open, and goes with the last.
    fs.remove(path.into()).map_err(s)?.map_err(s)?;
    check(fs.stat(other.into()).map_err(s)?.is_ok(), "its other name keeps it")?;
    fs.remove(other.into()).map_err(s)?.map_err(s)?;
    check(fs.stat(path.into()).map_err(s)? == Err(FsError::NotFound), "names gone")?;
    check(f.read_at(100, 6).map_err(s)?.map_err(s)?.0 == b"shared", "still there while open")?;
    Ok(())
}

/// The VFS's open files: a shared offset across duplicated connections,
/// files that outlive their names, renames that replace, exclusive
/// creation, devices.
pub fn test_vfs_open_files() -> TestResult {
    let fs = vfs_client()?;
    let path = "/tmp/systest-open.txt";
    let f = open_file(&fs, path, open_flags::READ | open_flags::WRITE | open_flags::CREATE | open_flags::TRUNCATE)?;
    f.write(Bytes(b"hello world".to_vec())).map_err(s)?.map_err(s)?;
    let g = file::Client::new(f.duplicate().map_err(s)?.map_err(s)?);
    check(g.seek(0, seek::CURRENT).map_err(s)?.map_err(s)? == 11, "duplicates share the offset")?;
    g.seek(6, seek::SET).map_err(s)?.map_err(s)?;
    check(f.read(5).map_err(s)?.map_err(s)?.0 == b"world", "reading at the shared offset")?;
    let st = f.stat().map_err(s)?.map_err(s)?;
    check(st.size == 11 && st.inode != 0 && !st.executable, "stat")?;

    // Removed while open: still there for those who have it.
    fs.remove(path.into()).map_err(s)?.map_err(s)?;
    check(fs.stat(path.into()).map_err(s)? == Err(FsError::NotFound), "name gone")?;
    check(f.read_at(0, 5).map_err(s)?.map_err(s)?.0 == b"hello", "contents still readable")?;
    f.write_at(0, Bytes(b"HELLO".to_vec())).map_err(s)?.map_err(s)?;
    check(g.read_at(0, 5).map_err(s)?.map_err(s)?.0 == b"HELLO", "and writable")?;
    drop((f, g));

    // Exclusive creation and replacing renames.
    let a = "/tmp/systest-a.txt";
    let b = "/tmp/systest-b.txt";
    for (p, text) in [(a, "A"), (b, "B")] {
        let h = open_file(&fs, p, open_flags::WRITE | open_flags::CREATE | open_flags::TRUNCATE)?;
        h.write(Bytes(text.as_bytes().to_vec())).map_err(s)?.map_err(s)?;
    }
    let excl = fs.open_file(a.into(), open_flags::WRITE | open_flags::CREATE | open_flags::EXCLUSIVE).map_err(s)?;
    check(matches!(excl, Err(FsError::Exists)), "exclusive creation of an existing file fails")?;
    check(fs.rename(a.into(), b.into()).map_err(s)? == Err(FsError::Exists), "rename does not replace")?;
    fs.replace(a.into(), b.into()).map_err(s)?.map_err(s)?;
    let h = open_file(&fs, b, open_flags::READ)?;
    check(h.read(10).map_err(s)?.map_err(s)?.0 == b"A", "replace moved the file over the other")?;
    fs.remove(b.into()).map_err(s)?.map_err(s)?;

    // Devices.
    let null = open_file(&fs, "/dev/null", open_flags::READ | open_flags::WRITE)?;
    check(null.write(Bytes(alloc::vec![1; 100])).map_err(s)?.map_err(s)? == 100, "/dev/null swallows")?;
    check(null.read(10).map_err(s)?.map_err(s)?.0.is_empty(), "/dev/null is empty")?;
    let st = fs.stat("/dev/zero".into()).map_err(s)?.map_err(s)?;
    check(st.char_device && !st.is_dir, "/dev/zero is a device")?;
    let full = open_file(&fs, "/dev/full", open_flags::WRITE)?;
    check(full.write(Bytes(alloc::vec![0; 1])).map_err(s)? == Err(FsError::NoSpace), "/dev/full is full")?;
    let names: Vec<String> = fs.read_dir("/dev".into()).map_err(s)?.map_err(s)?.into_iter().map(|e| e.name).collect();
    check(names.iter().any(|n| n == "urandom"), "/dev lists its devices")
}

/// Runs a program of the C library's (in C, C++ or Rust) with its output on
/// a socket: the exit code and what it wrote.
pub(crate) fn run_c(path: &str, args: &[&str], cwd: &str) -> Result<(i64, String), String> {
    run_c_for(path, args, cwd, 120)
}

/// [`run_c`], giving the program `seconds` to finish.
pub(crate) fn run_c_for(path: &str, args: &[&str], cwd: &str, seconds: u64) -> Result<(i64, String), String> {
    let (process, ours) = start_c(path, args, cwd)?;
    let deadline = rt::time::deadline_after(rt::time::Duration::from_secs(seconds));
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match ours.read(&mut buf) {
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(Error::ShouldWait) => {
                if ours.wait(READABLE | PEER_CLOSED, deadline) == Err(Error::TimedOut) {
                    let _ = process.kill();
                    return Err(format!("{path} did not finish: {}", String::from_utf8_lossy(&out)));
                }
            }
            Err(_) => break,
        }
    }
    let code = process.join(deadline).map_err(s)?;
    Ok((code, String::from_utf8_lossy(&out).into_owned()))
}

/// Starts a program of the C library's, its output on the socket that
/// comes with it.
fn start_c(path: &str, args: &[&str], cwd: &str) -> Result<(Process, Socket), String> {
    let fs = vfs_client()?;
    let (vmo, size) = fs.read_file(path.into()).map_err(s)?.map_err(|e| format!("{path}: {e}"))?;
    let image = vm::Mapping::new(vmo, size as usize, map_flags::READ).map_err(s)?;
    let (ours, theirs) = Socket::create().map_err(s)?;
    let registry = proto::with_registry(|r| r.clone_registry())
        .map_err(|e| format!("{e:?}"))?
        .map_err(s)?
        .map_err(|e| format!("{e:?}"))?;
    // A program linked dynamically starts with the system's dynamic linker.
    let linker = |path: &str| {
        let (vmo, size) = fs.read_file(path.into()).ok()?.ok()?;
        Some((vmo, usize::try_from(size).ok()?))
    };
    let mut spawn = Spawn::new("c-test")
        .path(path)
        .dynamic_linker(&linker)
        .cwd(cwd)
        .env("HOME=/home/user")
        .env("PATH=/system/bin")
        .handle(role::FD + 1, theirs.duplicate(None).map_err(s)?.into_handle())
        .handle(role::FD + 2, theirs.into_handle())
        .handle(role::REGISTRY, registry.into_handle());
    for a in args {
        spawn = spawn.arg(a);
    }
    // SAFETY: a read-only mapping of the VFS's copy of the file.
    let process: Process = spawn.start(unsafe { image.as_slice() }).map_err(|e| format!("{path}: {e}"))?;
    Ok((process, ours))
}

/// The host machine's servers, when the kernel command line named them
/// (`run=systest:host,ADDRESS,TCP-PORT,UDP-PORT`, which `cargo automation
/// test` gives): the arguments that have the checks of the C library and of
/// Rust's standard library reach them too.
fn host_args() -> Vec<String> {
    let args = rt::env::args();
    match args.get(1..5) {
        Some([host, address, tcp, udp]) if host == "host" => {
            alloc::vec![host.clone(), address.clone(), tcp.clone(), udp.clone()]
        }
        _ => Vec::new(),
    }
}

/// C and C++ programs built by the cross toolchain run, linked statically
/// (at a fixed address and position-independent) and dynamically: the
/// smallest one, then the checks of the C library and the POSIX layer
/// (`verification/c/posix.c`, with the host's servers when there are some),
/// of the C++ library (`verification/c/cxx.cc`) and of shared libraries
/// and loading them (`verification/c/libraries.c`).
pub fn test_c_programs() -> TestResult {
    let fs = vfs_client()?;
    if fs.stat("/system/tests/c/hello".into()).map_err(s)?.is_err() {
        rt::println!("c programs: none in this image (no cross toolchain was built)");
        return Ok(());
    }
    // Each program linked statically, at a fixed address and
    // position-independent, and dynamically.
    for variant in ["", "-pie", "-dynamic"] {
        let hello = format!("hello{variant}");
        let (code, out) = run_c(&format!("/system/tests/c/{hello}"), &["one", "two"], "/tmp")?;
        rt::println!("{hello}: {}", out.trim_end());
        check(code == 42, &format!("{hello} exited with {code}"))?;
        let greeting = out.contains("Hello from C on Veda! argc=3 argv[0]=c-test") && out.contains("cwd=/tmp");
        check(greeting, &format!("{hello}'s output"))?;

        // The checks of the C library and the POSIX layer, of the C++
        // library, and of shared libraries.
        let host = host_args();
        for checks in ["posix", "cxx", "libraries"] {
            let program = format!("{checks}{variant}");
            let dir = format!("/tmp/{program}-test");
            let _ = fs.mkdir(dir.clone());
            let args: Vec<&str> =
                if checks == "posix" { host.iter().map(String::as_str).collect() } else { Vec::new() };
            let (code, out) = run_c(&format!("/system/tests/c/{program}"), &args, &dir)?;
            for line in out.lines().filter(|l| !l.starts_with("ok - ")) {
                rt::println!("{program}: {line}");
            }
            let passed = code == 0 && out.contains("PASS: 0 failed");
            check(passed, &format!("{program} checks failed (exit code {code})"))?;
        }
    }
    Ok(())
}

/// Rust programs built by Rust's toolchain run, linked statically
/// (position-independent, as Rust links them for Veda, and at a fixed
/// address) and dynamically: the checks of its standard library
/// (`verification/rust/stdlib.rs`, with the host's servers when there are
/// some), and of loading libraries (`verification/rust/loading.rs`).
pub fn test_rust_programs() -> TestResult {
    let fs = vfs_client()?;
    if fs.stat("/system/tests/rust/stdlib".into()).map_err(s)?.is_err() {
        rt::println!("rust programs: none in this image (no Rust toolchain was built)");
        return Ok(());
    }
    let host = host_args();
    for variant in ["", "-fixed", "-dynamic"] {
        for checks in ["stdlib", "loading"] {
            let program = format!("{checks}{variant}");
            let dir = format!("/tmp/{program}-test");
            let _ = fs.mkdir(dir.clone());
            let args: Vec<&str> =
                if checks == "stdlib" { host.iter().map(String::as_str).collect() } else { Vec::new() };
            let (code, out) = run_c(&format!("/system/tests/rust/{program}"), &args, &dir)?;
            for line in out.lines().filter(|l| !l.starts_with("ok - ")) {
                rt::println!("{program}: {line}");
            }
            let passed = code == 0 && out.contains("PASS: 0 failed");
            check(passed, &format!("{program} checks failed (exit code {code})"))?;
        }
    }
    Ok(())
}

/// A dynamically linked program on the standard library loads libraries at
/// run time with libloading (`verification/plugins`), where the image has
/// the libraries it loads (the C and Rust test programs').
pub fn test_plugins() -> TestResult {
    let fs = vfs_client()?;
    let libraries = ["/system/tests/c/lib/libplugin.so", "/system/tests/rust/lib/librustplugin.so"];
    if libraries.iter().any(|l| !fs.stat((*l).into()).is_ok_and(|r| r.is_ok())) {
        rt::println!("plugins: no test libraries in this image (no toolchain was built)");
        return Ok(());
    }
    let dir = String::from("/tmp/plugins-test");
    let _ = fs.mkdir(dir.clone());
    let (code, out) = run_c("/system/bin/plugins", &[], &dir)?;
    for line in out.lines().filter(|l| !l.starts_with("ok - ")) {
        rt::println!("plugins: {line}");
    }
    let passed = code == 0 && out.contains("PASS: 0 failed");
    check(passed, &format!("plugins checks failed (exit code {code})"))
}

/// Python's standard library works on Veda as `documentation/PYTHON.md`
/// says (`verification/python/stdlib.py`, with the host's servers when
/// there are some), where the image has Python: its interpreter, from
/// subprocesses and virtual environments (with pip) to TLS, SQLite, the
/// compression formats, `ctypes` and an extension module it compiles, and
/// what Veda has not failing as said. Given ten minutes, as it builds an
/// environment.
pub fn test_python() -> TestResult {
    let fs = vfs_client()?;
    if fs.stat("/system/bin/python3".into()).map_err(s)?.is_err() {
        rt::println!("python: not in this image (Python was not built)");
        return Ok(());
    }
    let host = host_args();
    let mut args = alloc::vec!["/system/tests/python/stdlib.py"];
    args.extend(host.iter().map(String::as_str));
    let dir = String::from("/tmp/python-test");
    let _ = fs.mkdir(dir.clone());
    let (code, out) = run_c_for("/system/bin/python3", &args, &dir, 600)?;
    for line in out.lines().filter(|l| !l.starts_with("ok - ")) {
        rt::println!("python: {line}");
    }
    let passed = code == 0 && out.contains("PASS: 0 failed");
    check(passed, &format!("Python's checks failed (exit code {code})"))
}

/// Crates that need Rust's standard library work as Veda's ports of them
/// have them: libc, getrandom (both releases), rand, rustix, tempfile and
/// tokio, on the C library's epoll, eventfd and sockets
/// (`verification/stdcrates`, a program on the standard library).
pub fn test_std_crates() -> TestResult {
    let dir = String::from("/tmp/stdcrates-test");
    let _ = vfs_client()?.mkdir(dir.clone());
    let (code, out) = run_c("/system/bin/stdcrates", &[], &dir)?;
    for line in out.lines().filter(|l| !l.starts_with("ok - ")) {
        rt::println!("stdcrates: {line}");
    }
    let passed = code == 0 && out.contains("PASS: 0 failed");
    check(passed, &format!("stdcrates checks failed (exit code {code})"))
}
