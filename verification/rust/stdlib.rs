// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! stdlib — checks of Rust's standard library on Veda, where it stands on
//! the C library (see documentation/RUST.md).
//!
//! Each check prints `ok - NAME` or `not ok - NAME: REASON`; the program
//! exits with the number of failed checks (0: all passed). `systest` runs it
//! inside Veda in a scratch directory, with its standard output and error
//! on a socket and the empty input as its standard input.
//!
//! `stdlib child MODE` is the program the process checks start; `stdlib
//! host ADDRESS TCP-PORT UDP-PORT` runs the checks with the host machine's
//! servers too (see `check_host`).

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::hash::BuildHasher;
use std::io::{self, IsTerminal, Read, Seek, SeekFrom, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::fs::{FileExt, FileTypeExt, MetadataExt};
use std::os::unix::net::{UnixDatagram, UnixListener, UnixStream};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Condvar, LazyLock, Mutex, OnceLock, RwLock, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use std::{env, panic, thread};

type Check = Result<(), String>;

/// `Err(what)` unless `ok`.
fn ensure(ok: bool, what: impl FnOnce() -> String) -> Check {
    if ok { Ok(()) } else { Err(what()) }
}

/// An I/O result's error as a reason.
fn io<T>(what: &str, r: io::Result<T>) -> Result<T, String> {
    r.map_err(|e| format!("{what}: {e}"))
}

/// The program's own file, which the process checks start again.
fn program() -> Result<PathBuf, String> {
    io("current_exe", env::current_exe())
}

fn check_startup() -> Check {
    ensure(env::consts::OS == "veda" && env::consts::FAMILY == "unix", || {
        format!("the system is {} ({})", env::consts::OS, env::consts::FAMILY)
    })?;
    let exe = program()?;
    ensure(exe.is_absolute() && exe.is_file(), || format!("current_exe is {}", exe.display()))?;
    let cwd = io("current_dir", env::current_dir())?;
    ensure(cwd.is_absolute() && cwd.is_dir(), || format!("current_dir is {}", cwd.display()))?;
    let home = env::var("HOME").unwrap_or_default();
    ensure(home.starts_with('/'), || format!("HOME is '{home}'"))?;
    ensure(env::temp_dir() == Path::new("/tmp"), || format!("temp_dir is {}", env::temp_dir().display()))
}

fn check_standard_streams() -> Check {
    // systest gives it the empty input (a terminal would wait for a line).
    if !io::stdin().is_terminal() {
        let mut input = Vec::new();
        let n = io("reading standard input", io::stdin().read_to_end(&mut input))?;
        ensure(n == 0, || format!("the empty input gave {n} bytes"))?;
    }
    let mut out = io::stdout().lock();
    io("writing standard output", out.write_all(b"").and_then(|()| out.flush()))?;
    let mut err = io::stderr().lock();
    io("writing standard error", err.write_all(b"").and_then(|()| err.flush()))
}

fn check_formatting_and_numbers() -> Check {
    let text = format!("{:>8.3}|{:<5}|{:#x}|{:e}", 3.14159, "veda", 255, 1500.0);
    ensure(text == "   3.142|veda |0xff|1.5e3", || format!("formatted '{text}'"))?;
    let parsed: f64 = "2.5e3".parse().map_err(|e| format!("parsing a float: {e}"))?;
    ensure(parsed == 2500.0, || format!("parsed {parsed}"))?;
    // The C library's mathematics.
    let (s, c) = 1.0f64.sin_cos();
    ensure((s * s + c * c - 1.0).abs() < 1e-12 && (2.0f64.sqrt().powi(2) - 2.0).abs() < 1e-12, || {
        format!("sin {s}, cos {c}")
    })
}

fn check_heap() -> Check {
    let numbers: Vec<u64> = (0..1_000_000).collect();
    let sum: u64 = numbers.iter().sum();
    ensure(sum == 499_999_500_000, || format!("sum {sum}"))?;
    // A large allocation comes zeroed and can be used throughout.
    let mut big = vec![0u8; 64 << 20];
    ensure(big.iter().step_by(4096).all(|&b| b == 0), || "64 MiB not zeroed".into())?;
    big.iter_mut().step_by(4096).for_each(|b| *b = 1);
    drop(big);
    let layout = std::alloc::Layout::from_size_align(8192, 4096).map_err(|e| e.to_string())?;
    // SAFETY: a layout of a non-zero size; the memory is freed with it.
    let page = unsafe { std::alloc::alloc(layout) };
    ensure(!page.is_null() && page.addr() % 4096 == 0, || format!("aligned allocation at {page:p}"))?;
    // SAFETY: allocated above with this layout.
    unsafe { std::alloc::dealloc(page, layout) };
    let mut s = String::new();
    for i in 0..10_000 {
        s.push_str(&i.to_string());
    }
    ensure(s.len() == 38_890, || format!("string of {} bytes", s.len()))
}

fn check_collections() -> Check {
    let map: HashMap<u32, u32> = (0..10_000).map(|i| (i, i * 2)).collect();
    ensure(map.len() == 10_000 && map[&4_321] == 8_642, || "hash map contents".into())?;
    let ordered: BTreeMap<&str, i32> = [("c", 3), ("a", 1), ("b", 2)].into_iter().collect();
    let keys: Vec<&str> = ordered.keys().copied().collect();
    ensure(keys == ["a", "b", "c"], || format!("keys {keys:?}"))
}

fn check_files() -> Check {
    io("write", fs::write("notes.txt", "hello"))?;
    let mut f = io("open for appending", OpenOptions::new().append(true).open("notes.txt"))?;
    io("append", f.write_all(b" world"))?;
    drop(f);
    let text = io("read", fs::read_to_string("notes.txt"))?;
    ensure(text == "hello world", || format!("read '{text}'"))?;
    let meta = io("metadata", fs::metadata("notes.txt"))?;
    ensure(meta.is_file() && meta.len() == 11 && meta.ino() != 0, || format!("metadata {meta:?}"))?;
    let age = io("modified", meta.modified())?.elapsed().unwrap_or_default();
    ensure(age < Duration::from_secs(60), || format!("modified {age:?} ago"))?;

    let mut f = io("open", OpenOptions::new().read(true).write(true).open("notes.txt"))?;
    io("seek", f.seek(SeekFrom::Start(6)))?;
    let mut word = String::new();
    io("read after seek", f.read_to_string(&mut word))?;
    ensure(word == "world", || format!("read '{word}' after seeking"))?;
    io("write_at", f.write_at(b"W", 6))?;
    let mut one = [0u8; 1];
    io("read_at", f.read_at(&mut one, 6))?;
    ensure(&one == b"W", || format!("read_at gave {one:?}"))?;
    io("set_len", f.set_len(5))?;
    io("sync_all", f.sync_all())?;
    let then = UNIX_EPOCH + Duration::from_secs(1_000_000_000);
    io("set_modified", f.set_modified(then))?;
    drop(f);
    let modified = io("modified", io("metadata", fs::metadata("notes.txt"))?.modified())?;
    ensure(modified == then, || format!("modified at {modified:?}, set to {then:?}"))?;
    ensure(io("metadata", fs::metadata("notes.txt"))?.len() == 5, || "set_len did not truncate".into())?;

    let copied = io("copy", fs::copy("notes.txt", "copy.txt"))?;
    ensure(copied == 5, || format!("copied {copied} bytes"))?;
    io("rename", fs::rename("copy.txt", "moved.txt"))?;
    let missing = File::open("copy.txt").map(|_| ()).map_err(|e| e.kind());
    ensure(missing == Err(io::ErrorKind::NotFound), || format!("the old name: {missing:?}"))?;
    let again = OpenOptions::new().write(true).create_new(true).open("moved.txt").map(|_| ()).map_err(|e| e.kind());
    ensure(again == Err(io::ErrorKind::AlreadyExists), || format!("create_new of an existing file: {again:?}"))?;
    io("remove_file", fs::remove_file("moved.txt"))
}

fn check_directories() -> Check {
    io("create_dir_all", fs::create_dir_all("tree/b/c"))?;
    for name in ["tree/one.txt", "tree/b/two.txt", "tree/b/c/three.txt"] {
        io(name, fs::write(name, name))?;
    }
    let mut names: Vec<String> = io("read_dir", fs::read_dir("tree"))?
        .map(|e| e.map(|e| e.file_name().to_string_lossy().into_owned()))
        .collect::<io::Result<_>>()
        .map_err(|e| format!("a directory entry: {e}"))?;
    names.sort();
    ensure(names == ["b", "one.txt"], || format!("tree has {names:?}"))?;
    let canonical = io("canonicalize", fs::canonicalize("tree/b/../one.txt"))?;
    let expected = io("current_dir", env::current_dir())?.join("tree/one.txt");
    ensure(canonical == expected, || format!("canonicalize gave {}", canonical.display()))?;
    let full = fs::remove_dir("tree/b").map_err(|e| e.kind());
    ensure(full == Err(io::ErrorKind::DirectoryNotEmpty), || format!("removing a full directory: {full:?}"))?;
    io("remove_dir_all", fs::remove_dir_all("tree"))?;
    ensure(!Path::new("tree").exists(), || "the tree is still there".into())?;
    // Veda has no links (see documentation/C.md).
    ensure(fs::hard_link("notes.txt", "link.txt").is_err(), || "a hard link was made".into())
}

/// `File::lock` and the like are the C library's `flock`, whose locks the
/// VFS arbitrates between processes: this process's exclusive lock is in a
/// child's way until it unlocks it.
fn check_file_locks() -> Check {
    let f = io("open", File::open("notes.txt"))?;
    io("lock", f.lock())?;
    let locked = child("try-lock", |_| {})?;
    let said = String::from_utf8_lossy(&locked.stdout);
    ensure(said == "Err(WouldBlock)\n", || format!("a child's try_lock while locked: '{}'", said.trim_end()))?;
    io("unlock", f.unlock())?;
    let unlocked = child("try-lock", |_| {})?;
    let said = String::from_utf8_lossy(&unlocked.stdout);
    ensure(said == "Ok(())\n", || format!("a child's try_lock once unlocked: '{}'", said.trim_end()))
}

fn check_threads() -> Check {
    let handles: Vec<_> = (0..8u64).map(|i| thread::spawn(move || (0..1000).map(|n| n * i).sum::<u64>())).collect();
    let total: u64 = handles.into_iter().map(|h| h.join().unwrap_or(0)).sum();
    ensure(total == 499_500 * 28, || format!("threads summed {total}"))?;
    let mut data = [1u32, 2, 3, 4];
    thread::scope(|s| {
        for x in &mut data {
            s.spawn(move || *x *= 10);
        }
    });
    ensure(data == [10, 20, 30, 40], || format!("scoped threads left {data:?}"))?;
    let named = thread::Builder::new()
        .name("worker".into())
        .stack_size(256 << 10)
        .spawn(|| thread::current().name().map(str::to_owned));
    let name = io("Builder::spawn", named)?.join().map_err(|_| "the named thread panicked".to_string())?;
    ensure(name.as_deref() == Some("worker"), || format!("the thread is named {name:?}"))?;
    let n = io("available_parallelism", thread::available_parallelism())?;
    ensure(n.get() >= 1, || format!("{n} processors"))
}

fn check_synchronization() -> Check {
    let counter = Arc::new(Mutex::new(0u32));
    let workers: Vec<_> = (0..8)
        .map(|_| {
            let counter = Arc::clone(&counter);
            thread::spawn(move || {
                for _ in 0..10_000 {
                    *counter.lock().unwrap() += 1;
                }
            })
        })
        .collect();
    workers.into_iter().for_each(|w| w.join().unwrap());
    let count = *counter.lock().unwrap();
    ensure(count == 80_000, || format!("the mutex counted {count}"))?;

    let pair = Arc::new((Mutex::new(false), Condvar::new()));
    let signaller = Arc::clone(&pair);
    let t = thread::spawn(move || {
        thread::sleep(Duration::from_millis(20));
        *signaller.0.lock().unwrap() = true;
        signaller.1.notify_one();
    });
    let ready = pair.1.wait_while(pair.0.lock().unwrap(), |ready| !*ready).unwrap();
    ensure(*ready, || "the condition variable woke too early".into())?;
    drop(ready);
    t.join().unwrap();
    let started = Instant::now();
    let (guard, timeout) = pair.1.wait_timeout(pair.0.lock().unwrap(), Duration::from_millis(50)).unwrap();
    drop(guard);
    ensure(timeout.timed_out() && started.elapsed() >= Duration::from_millis(50), || {
        format!("wait_timeout returned after {:?}", started.elapsed())
    })?;

    let lock = Arc::new(RwLock::new(Vec::new()));
    let writers: Vec<_> = (0..4)
        .map(|i| {
            let lock = Arc::clone(&lock);
            thread::spawn(move || lock.write().unwrap().push(i))
        })
        .collect();
    writers.into_iter().for_each(|w| w.join().unwrap());
    ensure(lock.read().unwrap().len() == 4, || "the writers' pushes".into())?;

    let barrier = Arc::new(Barrier::new(4));
    let leaders: usize = (0..4)
        .map(|_| {
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || barrier.wait().is_leader())
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|t| t.join().unwrap() as usize)
        .sum();
    ensure(leaders == 1, || format!("{leaders} leaders at the barrier"))?;

    static ONCE: OnceLock<u32> = OnceLock::new();
    static LAZY: LazyLock<String> = LazyLock::new(|| "computed".into());
    let firsts: Vec<_> = (0..4).map(|i| thread::spawn(move || *ONCE.get_or_init(|| i))).collect();
    let values: Vec<u32> = firsts.into_iter().map(|t| t.join().unwrap()).collect();
    ensure(values.windows(2).all(|w| w[0] == w[1]) && *LAZY == "computed", || format!("OnceLock gave {values:?}"))?;

    let (tx, rx) = mpsc::channel();
    let senders: Vec<_> = (0..4)
        .map(|i| {
            let tx = tx.clone();
            thread::spawn(move || (0..250).for_each(|n| tx.send(i * 1000 + n).unwrap()))
        })
        .collect();
    drop(tx);
    let received = rx.iter().count();
    senders.into_iter().for_each(|s| s.join().unwrap());
    ensure(received == 1000, || format!("received {received} messages"))?;
    let (tx, rx) = mpsc::sync_channel(0);
    let t = thread::spawn(move || tx.send("rendezvous").unwrap());
    let got = rx.recv().map_err(|e| e.to_string())?;
    t.join().unwrap();
    ensure(got == "rendezvous", || format!("received '{got}'"))?;

    // The channels above wake this thread as they hand it what it waits
    // for, which may leave it a wake-up token: the next park returns at
    // once, as it may (taken first). Every thread that could wake it has
    // ended.
    thread::park_timeout(Duration::ZERO);
    let started = Instant::now();
    thread::park_timeout(Duration::from_millis(30));
    let parked = started.elapsed();
    let sleeper = thread::spawn(thread::park);
    sleeper.thread().unpark();
    sleeper.join().unwrap();
    ensure(parked >= Duration::from_millis(30), || format!("park_timeout returned after {parked:?}"))
}

fn check_thread_locals() -> Check {
    static DROPPED: AtomicUsize = AtomicUsize::new(0);
    struct Guard(u32);
    impl Drop for Guard {
        fn drop(&mut self) {
            DROPPED.fetch_add(self.0 as usize, Ordering::SeqCst);
        }
    }
    thread_local! {
        static GUARD: Guard = const { Guard(1) };
        static COUNT: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    }
    COUNT.set(5);
    let other = thread::spawn(|| {
        GUARD.with(|g| g.0);
        COUNT.get()
    });
    let theirs = other.join().map_err(|_| "the thread panicked".to_string())?;
    ensure(theirs == 0 && COUNT.get() == 5, || format!("thread-local values {theirs} and {}", COUNT.get()))?;
    // The destructor ran as the thread ended.
    let dropped = DROPPED.load(Ordering::SeqCst);
    ensure(dropped == 1, || format!("{dropped} destructors ran"))
}

fn check_panics() -> Check {
    static LOCATION: Mutex<Option<u32>> = Mutex::new(None);
    let previous = panic::take_hook();
    panic::set_hook(Box::new(|info| *LOCATION.lock().unwrap() = info.location().map(|l| l.line())));
    struct Unwound<'a>(&'a AtomicUsize);
    impl Drop for Unwound<'_> {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    let unwound = AtomicUsize::new(0);
    let result = panic::catch_unwind(|| {
        let _guard = Unwound(&unwound);
        panic!("boom");
    });
    let line = LOCATION.lock().unwrap().take();
    let in_thread = thread::spawn(|| panic!("in a thread")).join();
    let resumed = panic::catch_unwind(|| panic::resume_unwind(Box::new(7u8)));
    panic::set_hook(previous);
    let message = result.as_ref().err().and_then(|p| p.downcast_ref::<&str>().copied());
    ensure(message == Some("boom"), || format!("caught {message:?}"))?;
    ensure(unwound.load(Ordering::SeqCst) == 1, || "unwinding did not drop the guard".into())?;
    ensure(line.is_some(), || "the hook saw no location".into())?;
    ensure(in_thread.is_err(), || "a thread's panic did not reach join".into())?;
    let payload = resumed.err().and_then(|p| p.downcast_ref::<u8>().copied());
    ensure(payload == Some(7), || format!("resume_unwind carried {payload:?}"))
}

fn check_backtraces() -> Check {
    // Unwinding by GCC's unwinder, symbols and lines from the program's own
    // file (which the C library names by its path, AT_EXECFN).
    let trace = std::backtrace::Backtrace::force_capture();
    ensure(trace.status() == std::backtrace::BacktraceStatus::Captured, || "no backtrace captured".into())?;
    let text = trace.to_string();
    ensure(text.contains("check_backtraces") && text.contains("stdlib.rs:"), || format!("backtrace:\n{text}"))
}

fn check_time() -> Check {
    let before = Instant::now();
    thread::sleep(Duration::from_millis(50));
    let slept = before.elapsed();
    ensure(slept >= Duration::from_millis(50) && slept < Duration::from_secs(5), || format!("slept {slept:?}"))?;
    let since_epoch = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|e| e.to_string())?;
    // 2026-01-01.
    ensure(since_epoch.as_secs() >= 1_767_225_600, || format!("the time is {since_epoch:?} since 1970"))
}

/// Runs this program as `stdlib child MODE`: its exit status and output.
fn child(mode: &str, configure: impl FnOnce(&mut Command)) -> Result<std::process::Output, String> {
    let mut command = Command::new(program()?);
    command.args(["child", mode]);
    configure(&mut command);
    io(&format!("running the {mode} child"), command.output())
}

fn check_processes() -> Check {
    let echo = child("echo", |c| {
        c.env("VEDA_TEST", "from-parent");
    })?;
    let said = String::from_utf8_lossy(&echo.stdout);
    ensure(echo.status.code() == Some(7) && said == "child says hello from-parent\n", || {
        format!("echo: {} '{said}'", echo.status)
    })?;
    let aborted = child("abort", |_| {})?;
    ensure(aborted.status.signal() == Some(6), || format!("abort: {}", aborted.status))?;
    let panicked = child("panic", |_| {})?;
    let message = String::from_utf8_lossy(&panicked.stderr);
    ensure(panicked.status.code() == Some(101) && message.contains("panicked"), || {
        format!("panic: {} '{message}'", panicked.status)
    })?;
    io("create_dir", fs::create_dir_all("elsewhere"))?;
    io("write", fs::write("elsewhere/marker", ""))?;
    let moved = child("cwd", |c| {
        c.current_dir("elsewhere");
    })?;
    ensure(moved.status.code() == Some(0), || format!("the child's directory: {}", moved.status))?;
    let empty = child("env", |c| {
        c.env_clear();
    })?;
    ensure(String::from_utf8_lossy(&empty.stdout) == "0\n", || "env_clear left variables".into())?;
    // A process's hash maps take their keys from the system's random bytes:
    // two processes' differ.
    let hashes = [child("hash", |_| {})?, child("hash", |_| {})?].map(|o| o.stdout);
    ensure(hashes[0] != hashes[1], || "two processes hash alike: no random keys".into())?;

    let mut cat = Command::new(program()?);
    let cat = cat.args(["child", "cat"]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn();
    let mut cat = io("spawning the cat child", cat)?;
    ensure(cat.id() != std::process::id(), || "the child has our id".into())?;
    let mut stdin = cat.stdin.take().ok_or("no stdin")?;
    io("writing to the child", stdin.write_all(b"through a pipe"))?;
    drop(stdin);
    let out = io("waiting for the child", cat.wait_with_output())?;
    ensure(out.stdout == b"through a pipe", || format!("cat gave '{}'", String::from_utf8_lossy(&out.stdout)))?;

    let mut sleeper = io("spawning the sleep child", Command::new(program()?).args(["child", "sleep"]).spawn())?;
    io("kill", sleeper.kill())?;
    let killed = io("wait", sleeper.wait())?;
    ensure(killed.signal() == Some(9), || format!("killed: {killed}"))?;

    for missing in ["/system/tests/rust/missing", "missing-program"] {
        let r = Command::new(missing).spawn().map(|_| ()).map_err(|e| e.kind());
        ensure(r == Err(io::ErrorKind::NotFound), || format!("starting {missing}: {r:?}"))?;
    }
    // There is no fork or exec (see documentation/C.md): what only a child
    // between the two could do is refused.
    let mut forked = Command::new(program()?);
    // SAFETY: the closure does nothing at all.
    unsafe { forked.args(["child", "echo"]).pre_exec(|| Ok(())) };
    let r = forked.spawn().map(|_| ()).map_err(|e| e.kind());
    ensure(r == Err(io::ErrorKind::Unsupported), || format!("pre_exec: {r:?}"))?;
    // A failed exec leaves its process as exec had begun to make it (SIGPIPE
    // to its default, say): a child tries it.
    let exec = child("exec", |_| {})?;
    let told = String::from_utf8_lossy(&exec.stdout);
    ensure(told == "Unsupported\n", || format!("exec: '{told}'"))
}

fn check_pipes() -> Check {
    let (mut reader, mut writer) = io("pipe", io::pipe())?;
    let sent = thread::spawn(move || writer.write_all(&vec![0x5a; 1 << 20]));
    let mut received = Vec::new();
    io("reading the pipe", reader.read_to_end(&mut received))?;
    io("writing the pipe", sent.join().map_err(|_| io::Error::other("the writer panicked")).and_then(|r| r))?;
    ensure(received.len() == 1 << 20 && received.iter().all(|&b| b == 0x5a), || {
        format!("received {} bytes", received.len())
    })?;
    // Writing to a pipe nobody reads is an error, not the end (SIGPIPE is
    // ignored).
    let (reader, mut writer) = io("pipe", io::pipe())?;
    drop(reader);
    let r = writer.write_all(b"lost").map_err(|e| e.kind());
    ensure(r == Err(io::ErrorKind::BrokenPipe), || format!("writing to a closed pipe: {r:?}"))
}

fn check_sockets() -> Check {
    let (mut a, mut b) = io("UnixStream::pair", UnixStream::pair())?;
    io("write", a.write_all(b"ping"))?;
    let mut buf = [0u8; 4];
    io("read", b.read_exact(&mut buf))?;
    ensure(&buf == b"ping", || format!("read {buf:?}"))?;
    io("shutdown", a.shutdown(Shutdown::Write))?;
    let n = io("read after shutdown", b.read(&mut buf))?;
    ensure(n == 0, || format!("read {n} bytes after the peer shut down"))
}

/// A path for a socket of this process's in `/tmp`, nothing there yet.
fn socket_path(name: &str) -> String {
    let path = format!("/tmp/stdlib-{}-{name}.sock", std::process::id());
    let _ = fs::remove_file(&path);
    path
}

/// Unix domain sockets named by paths: a listener, which `stat` shows as a
/// socket, takes a connection from another process (a child that answers
/// in capitals what it reads) and from this one; names, time-outs and what
/// happens once the listener is gone.
fn check_unix_streams() -> Check {
    let path = socket_path("stream");
    let listener = io("bind", UnixListener::bind(&path))?;
    let kind = io("metadata", fs::metadata(&path))?.file_type();
    ensure(kind.is_socket(), || format!("{path} is {kind:?}, no socket"))?;
    let again = UnixListener::bind(&path).map(|_| ()).map_err(|e| e.kind());
    ensure(again == Err(io::ErrorKind::AddrInUse), || format!("binding twice: {again:?}"))?;
    let local = io("local_addr", listener.local_addr())?;
    ensure(local.as_pathname() == Some(Path::new(&path)), || format!("local address {local:?}"))?;

    let mut upper = io("spawning the unix child", Command::new(program()?).args(["child", "unix", &path]).spawn())?;
    let (mut conn, from) = io("accept", listener.accept())?;
    ensure(from.is_unnamed(), || format!("the child's socket is named: {from:?}"))?;
    io("write", conn.write_all(b"from the parent"))?;
    io("shutdown", conn.shutdown(Shutdown::Write))?;
    let mut answer = String::new();
    io("read", conn.read_to_string(&mut answer))?;
    ensure(answer == "FROM THE PARENT", || format!("the child answered '{answer}'"))?;
    let status = io("wait", upper.wait())?;
    ensure(status.code() == Some(0), || format!("the child: {status}"))?;

    // From a thread of this process, which reads with a time-out.
    let client = thread::spawn({
        let path = path.clone();
        move || -> Check {
            let mut s = io("connect", UnixStream::connect(&path))?;
            let peer = io("peer_addr", s.peer_addr())?;
            ensure(peer.as_pathname() == Some(Path::new(&path)), || format!("peer address {peer:?}"))?;
            io("set_read_timeout", s.set_read_timeout(Some(Duration::from_millis(50))))?;
            let mut buf = [0u8; 8];
            let quiet = s.read(&mut buf).map_err(|e| e.kind());
            ensure(quiet == Err(io::ErrorKind::WouldBlock), || format!("a quiet read: {quiet:?}"))?;
            io("write", s.write_all(b"ping"))?;
            io("read", s.read_exact(&mut buf[..4]))?;
            ensure(&buf[..4] == b"pong", || format!("read {:?}", &buf[..4]))
        }
    });
    let (mut conn, _) = io("accept", listener.accept())?;
    let mut buf = [0u8; 4];
    io("read", conn.read_exact(&mut buf))?;
    io("write", conn.write_all(b"pong"))?;
    client.join().map_err(|_| "the client panicked".to_string())??;

    io("set_nonblocking", listener.set_nonblocking(true))?;
    let waiting = listener.accept().map(|_| ()).map_err(|e| e.kind());
    ensure(waiting == Err(io::ErrorKind::WouldBlock), || format!("accept with nobody connecting: {waiting:?}"))?;
    // The name goes with the listener's node; a socket closed refuses.
    drop(listener);
    let refused = UnixStream::connect(&path).map(|_| ()).map_err(|e| e.kind());
    ensure(refused == Err(io::ErrorKind::ConnectionRefused), || format!("connecting to a closed socket: {refused:?}"))?;
    io("remove_file", fs::remove_file(&path))?;
    let gone = UnixStream::connect(&path).map(|_| ()).map_err(|e| e.kind());
    ensure(gone == Err(io::ErrorKind::NotFound), || format!("connecting to no socket: {gone:?}"))
}

/// Starts this program as a child in `mode`, with `socket` as its standard
/// input.
fn spawn_with(mode: &str, socket: OwnedFd) -> Result<Child, String> {
    io(&format!("spawning the {mode} child"), Command::new(program()?).args(["child", mode]).stdin(socket).spawn())
}

/// The process id a connection was greeted with ("accepted by ID").
fn greeted(connection: io::Result<impl Read>) -> Result<u32, String> {
    let mut said = String::new();
    io("read the greeting", io("connect", connection)?.read_to_string(&mut said))?;
    said.strip_prefix("accepted by ").and_then(|id| id.parse().ok()).ok_or_else(|| format!("greeted with '{said}'"))
}

/// Whether two children, waited for, exited well after taking one each (a
/// connection, a datagram): `by`, the ids they answered with.
fn one_each(kids: Vec<Child>, by: &[u32]) -> Check {
    let ids: Vec<u32> = kids.iter().map(Child::id).collect();
    for mut kid in kids {
        let status = io("wait", kid.wait())?;
        ensure(status.success(), || format!("a child: {status}"))?;
    }
    let each = by.len() == 2 && by[0] != by[1] && by.iter().all(|id| ids.contains(id));
    ensure(each, || format!("taken by {by:?}, the children are {ids:?}"))
}

/// Listening sockets shared with the processes this one starts, as
/// pre-forking servers share them: a Unix domain listener and a TCP one,
/// each the standard input of two children, which accept a connection each
/// (the Unix domain one after this process closed its own: it is open while
/// a process has it).
fn check_shared_listeners() -> Check {
    let path = socket_path("shared");
    let unix = io("bind", UnixListener::bind(&path))?;
    let kids = (0..2)
        .map(|_| spawn_with("accept-unix", io("try_clone", unix.try_clone())?.into()))
        .collect::<Result<Vec<_>, _>>()?;
    drop(unix);
    let by = (0..2).map(|_| greeted(UnixStream::connect(&path))).collect::<Result<Vec<_>, _>>()?;
    one_each(kids, &by)?;
    io("remove_file", fs::remove_file(&path))?;

    let tcp = io("bind", TcpListener::bind((Ipv4Addr::LOCALHOST, 0)))?;
    let addr = io("local_addr", tcp.local_addr())?;
    let kids = (0..2)
        .map(|_| spawn_with("accept-tcp", io("try_clone", tcp.try_clone())?.into()))
        .collect::<Result<Vec<_>, _>>()?;
    let by = (0..2).map(|_| greeted(TcpStream::connect(addr))).collect::<Result<Vec<_>, _>>()?;
    one_each(kids, &by)
}

/// Connected sockets shared with the processes this one starts: a TCP
/// connection as a child's standard input and output, one that a child and
/// this process both write to (open until the last of them closes it), and
/// a UDP socket from which two children take a datagram each.
fn check_shared_connections() -> Check {
    let listener = io("bind", TcpListener::bind((Ipv4Addr::LOCALHOST, 0)))?;
    let addr = io("local_addr", listener.local_addr())?;
    let connection = || -> Result<(TcpStream, TcpStream), String> {
        let client = io("connect", TcpStream::connect(addr))?;
        Ok((client, io("accept", listener.accept())?.0))
    };

    let (mut client, server) = connection()?;
    let input = OwnedFd::from(io("try_clone", server.try_clone())?);
    let upper = Command::new(program()?).args(["child", "upper"]).stdin(input).stdout(OwnedFd::from(server)).spawn();
    let mut upper = io("spawning the upper child", upper)?;
    io("write", client.write_all(b"hello, child\n"))?;
    io("shutdown", client.shutdown(Shutdown::Write))?;
    let mut said = String::new();
    io("read", client.read_to_string(&mut said))?;
    let status = io("wait", upper.wait())?;
    ensure(status.success() && said == "HELLO, CHILD\n", || format!("the child ({status}) answered '{said}'"))?;

    let (mut client, mut server) = connection()?;
    let output = OwnedFd::from(io("try_clone", server.try_clone())?);
    let say = child("say", |c| {
        c.stdout(output);
    })?;
    ensure(say.status.success(), || format!("the say child: {}", say.status))?;
    io("write", server.write_all(b"and the parent\n"))?;
    drop(server);
    let mut said = String::new();
    io("read", client.read_to_string(&mut said))?;
    ensure(said == "from the child\nand the parent\n", || format!("read '{said}'"))?;

    // A datagram lost fails the check rather than holding it up for ever;
    // the children find the time-out set as it was.
    let shared = io("bind", UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)))?;
    let asker = io("bind", UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)))?;
    let to = io("local_addr", shared.local_addr())?;
    io("set_read_timeout", shared.set_read_timeout(Some(Duration::from_secs(10))))?;
    io("set_read_timeout", asker.set_read_timeout(Some(Duration::from_secs(10))))?;
    let kids = (0..2)
        .map(|_| spawn_with("udp-answer", io("try_clone", shared.try_clone())?.into()))
        .collect::<Result<Vec<_>, _>>()?;
    drop(shared);
    let mut buf = [0u8; 64];
    let mut by = Vec::new();
    for _ in 0..2 {
        io("send_to", asker.send_to(b"who?", to))?;
        let n = io("recv", asker.recv(&mut buf))?;
        let said = String::from_utf8_lossy(&buf[..n]);
        by.push(said.strip_prefix("answered by ").and_then(|id| id.parse().ok()).ok_or(format!("answered '{said}'"))?);
    }
    one_each(kids, &by)
}

/// Unix domain datagram sockets: by path, from sockets with and without a
/// name, connected, and in pairs.
fn check_unix_datagrams() -> Check {
    let (a_path, b_path) = (socket_path("a"), socket_path("b"));
    let a = io("bind", UnixDatagram::bind(&a_path))?;
    let b = io("bind", UnixDatagram::bind(&b_path))?;
    let unnamed = io("unbound", UnixDatagram::unbound())?;
    io("send_to", unnamed.send_to(b"anonymous", &a_path))?;
    io("send_to", b.send_to(b"from b", &a_path))?;
    let mut buf = [0u8; 64];
    let (n, from) = io("recv_from", a.recv_from(&mut buf))?;
    ensure(&buf[..n] == b"anonymous" && from.is_unnamed(), || format!("got {:?} from {from:?}", &buf[..n]))?;
    let (n, from) = io("recv_from", a.recv_from(&mut buf))?;
    ensure(&buf[..n] == b"from b" && from.as_pathname() == Some(Path::new(&b_path)), || {
        format!("got {:?} from {from:?}", &buf[..n])
    })?;
    // Connected, it sends without an address; datagrams keep their bounds.
    io("connect", a.connect(&b_path))?;
    io("send", a.send(b"one"))?;
    io("send", a.send(b"two"))?;
    let mut small = [0u8; 2];
    let n = io("recv", b.recv(&mut small))?;
    ensure(n == 2 && &small == b"on", || format!("a datagram cut to fit: {n} {small:?}"))?;
    let n = io("recv", b.recv(&mut buf))?;
    ensure(&buf[..n] == b"two", || format!("the next datagram: {:?}", &buf[..n]))?;
    io("set_nonblocking", b.set_nonblocking(true))?;
    let none = b.recv(&mut buf).map_err(|e| e.kind());
    ensure(none == Err(io::ErrorKind::WouldBlock), || format!("nothing to receive: {none:?}"))?;
    let missing = unnamed.send_to(b"x", "/tmp/no-such.sock").map_err(|e| e.kind());
    ensure(missing == Err(io::ErrorKind::NotFound), || format!("sending to nothing: {missing:?}"))?;

    let (x, y) = io("UnixDatagram::pair", UnixDatagram::pair())?;
    io("send", x.send(b"to y"))?;
    io("send", y.send(b"to x"))?;
    let n = io("recv", y.recv(&mut buf))?;
    ensure(&buf[..n] == b"to y", || format!("received {:?}", &buf[..n]))?;
    let n = io("recv", x.recv(&mut buf))?;
    ensure(&buf[..n] == b"to x", || format!("received {:?}", &buf[..n]))?;
    for p in [&a_path, &b_path] {
        io("remove_file", fs::remove_file(p))?;
    }
    Ok(())
}

/// The host machine's servers, when `systest` names them (`stdlib host
/// ADDRESS TCP-PORT UDP-PORT`): a web server that answers `GET /hello` with
/// "hello from the host", and a UDP echo.
static HOST: OnceLock<(IpAddr, u16, u16)> = OnceLock::new();

/// A connection on the loopback interface: a server in a thread answers in
/// capitals what the client sent, once the client has finished sending.
fn check_tcp() -> Check {
    let listener = io("bind", TcpListener::bind("127.0.0.1:0"))?;
    let addr = io("local_addr", listener.local_addr())?;
    ensure(addr.ip() == Ipv4Addr::LOCALHOST && addr.port() != 0, || format!("bound to {addr}"))?;
    let server = thread::spawn(move || -> io::Result<SocketAddr> {
        let (mut conn, peer) = listener.accept()?;
        let mut text = String::new();
        conn.read_to_string(&mut text)?;
        conn.write_all(text.to_uppercase().as_bytes())?;
        Ok(peer)
    });
    let mut client = io("connect", TcpStream::connect(addr))?;
    io("set_nodelay", client.set_nodelay(true))?;
    ensure(io("nodelay", client.nodelay())?, || "TCP_NODELAY is off".into())?;
    let peer = io("peer_addr", client.peer_addr())?;
    ensure(peer == addr, || format!("connected to {peer}, not {addr}"))?;
    let local = io("local_addr", client.local_addr())?;
    io("write", client.write_all(b"hello from veda"))?;
    io("shutdown", client.shutdown(Shutdown::Write))?;
    let mut answer = String::new();
    io("read", client.read_to_string(&mut answer))?;
    ensure(answer == "HELLO FROM VEDA", || format!("the server answered '{answer}'"))?;
    let seen = server.join().map_err(|_| "the server panicked".to_string())?;
    let seen = io("the server", seen)?;
    ensure(seen == local, || format!("the server saw {seen}, the client is {local}"))
}

/// 4 MiB each way at once, through flow control in both directions: the
/// server echoes what comes, the client writes from one thread and reads
/// from another (a clone of the stream).
fn check_tcp_bulk() -> Check {
    const SIZE: usize = 4 << 20;
    let byte = |i: usize| (i * 7 % 251) as u8;
    let listener = io("bind", TcpListener::bind("127.0.0.1:0"))?;
    let addr = io("local_addr", listener.local_addr())?;
    let server = thread::spawn(move || -> io::Result<u64> {
        let (conn, _) = listener.accept()?;
        let mut reader = conn.try_clone()?;
        let mut writer = conn;
        let n = io::copy(&mut reader, &mut writer)?;
        writer.shutdown(Shutdown::Write)?;
        Ok(n)
    });
    let client = io("connect", TcpStream::connect(addr))?;
    let mut sender = io("try_clone", client.try_clone())?;
    let started = Instant::now();
    let send = thread::spawn(move || -> io::Result<()> {
        let data: Vec<u8> = (0..SIZE).map(byte).collect();
        for piece in data.chunks(100_000) {
            sender.write_all(piece)?;
        }
        sender.shutdown(Shutdown::Write)
    });
    let mut back = Vec::with_capacity(SIZE);
    let mut receiver = client;
    io("read", receiver.read_to_end(&mut back))?;
    let elapsed = started.elapsed();
    io("the sender", send.join().map_err(|_| io::Error::other("the sender panicked")).and_then(|r| r))?;
    let echoed = io("the server", server.join().map_err(|_| io::Error::other("the server panicked")).and_then(|r| r))?;
    ensure(echoed == SIZE as u64, || format!("the server echoed {echoed} bytes"))?;
    let wrong = back.iter().enumerate().position(|(i, b)| *b != byte(i));
    ensure(back.len() == SIZE && wrong.is_none(), || format!("{} bytes back, wrong from {wrong:?}", back.len()))?;
    println!("# 2 x 4 MiB over loopback TCP in {} ms", elapsed.as_millis());
    Ok(())
}

/// A connection's last bytes arrive however soon after them its writer
/// closes it: rounds of a server that writes in small pieces and closes the
/// connection at once (the network service may find the socket closed
/// before it has taken the last of what was written).
fn check_tcp_last_writes() -> Check {
    const ROUNDS: usize = 32;
    const SIZE: usize = 96 * 1024;
    let byte = |i: usize| (i % 253) as u8;
    let listener = io("bind", TcpListener::bind("127.0.0.1:0"))?;
    let addr = io("local_addr", listener.local_addr())?;
    let server = thread::spawn(move || -> io::Result<()> {
        let data: Vec<u8> = (0..SIZE).map(byte).collect();
        for _ in 0..ROUNDS {
            let (mut conn, _) = listener.accept()?;
            for piece in data.chunks(1500) {
                conn.write_all(piece)?;
            }
        }
        Ok(())
    });
    for round in 0..ROUNDS {
        let mut conn = io("connect", TcpStream::connect(addr))?;
        let mut got = Vec::with_capacity(SIZE);
        io("read", conn.read_to_end(&mut got))?;
        let wrong = got.iter().enumerate().position(|(i, b)| *b != byte(i));
        ensure(got.len() == SIZE && wrong.is_none(), || {
            format!("round {round}: {} bytes of {SIZE}, wrong from {wrong:?}", got.len())
        })?;
    }
    io("the server", server.join().map_err(|_| io::Error::other("the server panicked")).and_then(|r| r))
}

/// What a connection does besides carrying bytes: peeking, time-outs,
/// reading without waiting, its options and errors, IPv6.
fn check_tcp_behaviour() -> Check {
    let listener = io("bind", TcpListener::bind("127.0.0.1:0"))?;
    let addr = io("local_addr", listener.local_addr())?;
    io("set_nonblocking", listener.set_nonblocking(true))?;
    let none = listener.accept().map(|_| ()).map_err(|e| e.kind());
    ensure(none == Err(io::ErrorKind::WouldBlock), || format!("accept without a connection gave {none:?}"))?;
    let mut client = io("connect", TcpStream::connect(addr))?;
    let mut conn = loop {
        match listener.accept() {
            Ok((conn, _)) => break conn,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => thread::sleep(Duration::from_millis(5)),
            Err(e) => return Err(format!("accept: {e}")),
        }
    };
    io("set_nonblocking", conn.set_nonblocking(false))?;
    // Nothing comes: a read without waiting, and one that waits 100 ms.
    io("set_nonblocking", client.set_nonblocking(true))?;
    let mut buf = [0u8; 16];
    let r = client.read(&mut buf).map_err(|e| e.kind());
    ensure(r == Err(io::ErrorKind::WouldBlock), || format!("a read without waiting gave {r:?}"))?;
    io("set_nonblocking", client.set_nonblocking(false))?;
    io("set_read_timeout", client.set_read_timeout(Some(Duration::from_millis(100))))?;
    let started = Instant::now();
    let r = client.read(&mut buf).map_err(|e| e.kind());
    let waited = started.elapsed();
    ensure(
        r == Err(io::ErrorKind::WouldBlock) && waited >= Duration::from_millis(90) && waited < Duration::from_secs(5),
        || format!("a read with a time-out gave {r:?} after {waited:?}"),
    )?;
    // Peeking leaves the bytes to be read.
    io("write", conn.write_all(b"peekaboo"))?;
    let n = io("peek", client.peek(&mut buf[..4]))?;
    ensure(&buf[..n] == b"peek", || format!("peeked {:?}", &buf[..n]))?;
    io("read_exact", client.read_exact(&mut buf[..8]))?;
    ensure(&buf[..8] == b"peekaboo", || format!("read {:?}", &buf[..8]))?;
    io("set_ttl", client.set_ttl(32))?;
    ensure(io("ttl", client.ttl())? == 32, || "the time to live did not change".into())?;
    ensure(io("take_error", client.take_error())?.is_none(), || "an error without a cause".into())?;
    // A blocked reader wakes for what comes while another thread writes on
    // the same connection.
    let mut reader = io("try_clone", client.try_clone())?;
    io("set_read_timeout", reader.set_read_timeout(Some(Duration::from_secs(10))))?;
    let read = thread::spawn(move || -> io::Result<Vec<u8>> {
        let mut got = [0u8; 5];
        reader.read_exact(&mut got)?;
        Ok(got.to_vec())
    });
    thread::sleep(Duration::from_millis(50));
    io("write", client.write_all(b"ping"))?;
    io("read_exact", conn.read_exact(&mut buf[..4]))?;
    io("write", conn.write_all(b"pong!"))?;
    let got = read.join().map_err(|_| "the reader panicked".to_string())?;
    let got = io("the reader", got)?;
    ensure(got == b"pong!", || format!("the reader got {got:?}"))?;
    // The end of the stream, then nothing more.
    drop(conn);
    let n = io("read at the end", client.read(&mut buf))?;
    ensure(n == 0, || format!("read {n} bytes after the peer closed"))?;
    // A port nobody listens on refuses, at once.
    drop(listener);
    let refused = TcpStream::connect(addr).map(|_| ()).map_err(|e| e.kind());
    ensure(refused == Err(io::ErrorKind::ConnectionRefused), || format!("a closed port gave {refused:?}"))?;
    let refused = TcpStream::connect_timeout(&addr, Duration::from_secs(5)).map(|_| ()).map_err(|e| e.kind());
    ensure(refused == Err(io::ErrorKind::ConnectionRefused), || format!("connect_timeout gave {refused:?}"))?;
    // IPv6's loopback.
    let v6 = io("bind [::1]", TcpListener::bind("[::1]:0"))?;
    let v6_addr = io("local_addr", v6.local_addr())?;
    let mut c6 = io("connect [::1]", TcpStream::connect(v6_addr))?;
    let (mut s6, from) = io("accept on [::1]", v6.accept())?;
    ensure(from.ip() == Ipv6Addr::LOCALHOST, || format!("the connection came from {from}"))?;
    io("write", c6.write_all(b"6"))?;
    io("read_exact", s6.read_exact(&mut buf[..1]))?;
    ensure(buf[0] == b'6', || format!("read {:?} over IPv6", buf[0]))
}

/// Datagrams on the loopback interface: to an address, to a connected
/// peer, peeked, cut short, timed out.
fn check_udp() -> Check {
    let a = io("bind", UdpSocket::bind("127.0.0.1:0"))?;
    let b = io("bind", UdpSocket::bind("127.0.0.1:0"))?;
    let (a_addr, b_addr) = (io("local_addr", a.local_addr())?, io("local_addr", b.local_addr())?);
    ensure(a_addr.port() != 0 && a_addr.port() != b_addr.port(), || format!("bound to {a_addr} and {b_addr}"))?;
    io("set_read_timeout", b.set_read_timeout(Some(Duration::from_secs(5))))?;
    io("set_read_timeout", a.set_read_timeout(Some(Duration::from_secs(5))))?;
    let sent = io("send_to", a.send_to(b"one", b_addr))?;
    let mut buf = [0u8; 64];
    let (n, from) = io("recv_from", b.recv_from(&mut buf))?;
    ensure(sent == 3 && &buf[..n] == b"one" && from == a_addr, || format!("got {:?} from {from}", &buf[..n]))?;
    // Connected, each to the other.
    io("connect", a.connect(b_addr))?;
    io("connect", b.connect(a_addr))?;
    ensure(io("peer_addr", a.peer_addr())? == b_addr, || "a has another peer".into())?;
    io("send", a.send(b"two"))?;
    let n = io("recv", b.recv(&mut buf))?;
    ensure(&buf[..n] == b"two", || format!("got {:?}", &buf[..n]))?;
    io("send", b.send(b"three"))?;
    let n = io("peek", a.peek(&mut buf))?;
    ensure(&buf[..n] == b"three", || format!("peeked {:?}", &buf[..n]))?;
    let n = io("recv", a.recv(&mut buf))?;
    ensure(&buf[..n] == b"three", || format!("got {:?}", &buf[..n]))?;
    // A datagram longer than the buffer is cut, the rest dropped.
    io("send", b.send(&[7; 100]))?;
    let n = io("recv", a.recv(&mut buf[..10]))?;
    ensure(n == 10, || format!("a 100-byte datagram gave {n} bytes"))?;
    // A connected socket hears its peer alone.
    let c = io("bind", UdpSocket::bind("127.0.0.1:0"))?;
    io("send_to", c.send_to(b"stranger", a_addr))?;
    io("send", b.send(b"friend"))?;
    let n = io("recv", a.recv(&mut buf))?;
    ensure(&buf[..n] == b"friend", || format!("got {:?}", &buf[..n]))?;
    io("set_nonblocking", a.set_nonblocking(true))?;
    let r = a.recv(&mut buf).map_err(|e| e.kind());
    ensure(r == Err(io::ErrorKind::WouldBlock), || format!("a read with nothing there gave {r:?}"))?;
    io("set_nonblocking", a.set_nonblocking(false))?;
    io("set_read_timeout", a.set_read_timeout(Some(Duration::from_millis(100))))?;
    let started = Instant::now();
    let r = a.recv(&mut buf).map_err(|e| e.kind());
    let waited = started.elapsed();
    ensure(r == Err(io::ErrorKind::WouldBlock) && waited >= Duration::from_millis(90), || {
        format!("a read with a time-out gave {r:?} after {waited:?}")
    })?;
    // IPv6's loopback.
    let v6 = io("bind [::1]", UdpSocket::bind("[::1]:0"))?;
    let v6_addr = io("local_addr", v6.local_addr())?;
    io("set_read_timeout", v6.set_read_timeout(Some(Duration::from_secs(5))))?;
    let other = io("bind [::1]", UdpSocket::bind("[::1]:0"))?;
    io("send_to", other.send_to(b"six", v6_addr))?;
    let (n, from) = io("recv_from", v6.recv_from(&mut buf))?;
    ensure(&buf[..n] == b"six" && from.ip() == Ipv6Addr::LOCALHOST, || format!("got {:?} from {from}", &buf[..n]))
}

/// Names, which the network service looks up.
fn check_names() -> Check {
    let addrs: Vec<SocketAddr> = io("localhost", ("localhost", 80).to_socket_addrs())?.collect();
    let loopback = SocketAddr::from((Ipv4Addr::LOCALHOST, 80));
    ensure(addrs.first() == Some(&loopback), || format!("localhost is {addrs:?}"))?;
    let literal: Vec<SocketAddr> = io("a literal", "192.0.2.7:8080".to_socket_addrs())?.collect();
    ensure(literal == ["192.0.2.7:8080".parse().unwrap()], || format!("192.0.2.7:8080 is {literal:?}"))?;
    let v6: Vec<SocketAddr> = io("an IPv6 literal", "[::1]:443".to_socket_addrs())?.collect();
    ensure(v6 == [SocketAddr::from((Ipv6Addr::LOCALHOST, 443))], || format!("[::1]:443 is {v6:?}"))?;
    ensure("no such host:80".to_socket_addrs().is_err(), || "a name with spaces was found".into())?;
    // Connecting by name.
    let listener = io("bind", TcpListener::bind("127.0.0.1:0"))?;
    let port = io("local_addr", listener.local_addr())?.port();
    let client = io("connect to localhost", TcpStream::connect(("localhost", port)))?;
    let peer = io("peer_addr", client.peer_addr())?;
    ensure(peer == SocketAddr::from((Ipv4Addr::LOCALHOST, port)), || format!("localhost was {peer}"))
}

/// The host machine, through QEMU's network: its web server and its UDP
/// echo, when `systest` names them.
fn check_host() -> Check {
    let Some(&(ip, tcp, udp)) = HOST.get() else {
        println!("# the host: no servers were named");
        return Ok(());
    };
    let server = SocketAddr::new(ip, tcp);
    // The network comes up after the system starts (its card is the driver
    // VM's): until it does, a connection fails at once.
    let started = Instant::now();
    let mut s = loop {
        match TcpStream::connect_timeout(&server, Duration::from_secs(10)) {
            Ok(s) => break s,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::NetworkDown | io::ErrorKind::NetworkUnreachable | io::ErrorKind::HostUnreachable
                ) && started.elapsed() < Duration::from_secs(60) =>
            {
                thread::sleep(Duration::from_millis(250))
            }
            Err(e) => return Err(format!("connect to {server}: {e}")),
        }
    };
    io("set_read_timeout", s.set_read_timeout(Some(Duration::from_secs(10))))?;
    io("request", write!(s, "GET /hello HTTP/1.1\r\nHost: {ip}\r\nConnection: close\r\n\r\n"))?;
    let mut answer = String::new();
    io("response", s.read_to_string(&mut answer))?;
    ensure(answer.starts_with("HTTP/1.1 200 OK\r\n") && answer.ends_with("\r\n\r\nhello from the host\n"), || {
        format!("the host answered '{answer}'")
    })?;
    let echo = SocketAddr::new(ip, udp);
    let u = io("bind", UdpSocket::bind("0.0.0.0:0"))?;
    io("set_read_timeout", u.set_read_timeout(Some(Duration::from_secs(2))))?;
    let mut buf = [0u8; 64];
    // A datagram may be lost: a few tries.
    for _ in 0..5 {
        io("send_to", u.send_to(b"echo from veda", echo))?;
        match u.recv_from(&mut buf) {
            Ok((n, from)) => {
                return ensure(&buf[..n] == b"echo from veda" && from == echo, || {
                    format!("got {:?} from {from}", &buf[..n])
                });
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
            Err(e) => return Err(format!("recv_from: {e}")),
        }
    }
    Err(format!("{echo} did not echo"))
}

#[inline(never)]
fn recurse(depth: u32) -> u32 {
    let frame = std::hint::black_box([depth as u8; 4096]);
    if depth == 0 { frame[0] as u32 } else { recurse(depth - 1).wrapping_add(frame[4095] as u32) }
}

fn check_stack() -> Check {
    // 4 MiB of frames on the main thread, whose stack is 8 MiB.
    let r = recurse(1000);
    ensure(r == (1..=1000u32).map(|d| d as u8 as u32).sum::<u32>(), || format!("recursion gave {r}"))
}

/// What the process checks start.
fn child_mode(mode: &str) -> i32 {
    match mode {
        "echo" => {
            println!("child says hello {}", env::var("VEDA_TEST").unwrap_or_else(|_| "(no env)".into()));
            7
        }
        "abort" => std::process::abort(),
        "panic" => panic!("the child panics"),
        // Its parent made `marker` in the directory it gave it.
        "cwd" => i32::from(!Path::new("marker").exists()),
        "env" => {
            println!("{}", env::vars_os().count());
            0
        }
        "hash" => {
            println!("{}", std::hash::RandomState::new().hash_one("veda"));
            0
        }
        "cat" => {
            let copied = io::copy(&mut io::stdin(), &mut io::stdout());
            i32::from(copied.is_err())
        }
        "sleep" => {
            thread::sleep(Duration::from_secs(60));
            0
        }
        // It tries to lock `notes.txt`, which its parent may hold locked.
        "try-lock" => match File::open("notes.txt") {
            Ok(f) => {
                println!("{:?}", f.try_lock().map_err(|e| io::Error::from(e).kind()));
                0
            }
            Err(_) => 1,
        },
        // A Unix domain socket's client: it answers in capitals what it
        // reads.
        "unix" => {
            let Some(path) = env::args().nth(3) else { return 2 };
            let Ok(mut s) = UnixStream::connect(&path) else { return 3 };
            let mut said = Vec::new();
            if s.read_to_end(&mut said).is_err() || s.write_all(&said.to_ascii_uppercase()).is_err() {
                return 4;
            }
            0
        }
        // Its standard input listens: one connection, greeted with this
        // process's id.
        "accept-unix" | "accept-tcp" => {
            let greeting = format!("accepted by {}", std::process::id());
            // SAFETY: descriptor 0 is the listener its parent gave it, which
            // nothing else here owns.
            let accepted = if mode == "accept-unix" {
                unsafe { UnixListener::from_raw_fd(0) }.accept().and_then(|(mut c, _)| c.write_all(greeting.as_bytes()))
            } else {
                unsafe { TcpListener::from_raw_fd(0) }.accept().and_then(|(mut c, _)| c.write_all(greeting.as_bytes()))
            };
            i32::from(accepted.is_err())
        }
        // Its standard input and output are a connection: what comes, in
        // capitals, once it ends.
        "upper" => {
            let mut said = Vec::new();
            let answered =
                io::stdin().read_to_end(&mut said).and_then(|_| io::stdout().write_all(&said.to_ascii_uppercase()));
            i32::from(answered.is_err())
        }
        // Its standard output is a connection, which its parent writes to
        // after it: the child's line.
        "say" => i32::from(io::stdout().write_all(b"from the child\n").is_err()),
        // Its standard input is a UDP socket, with the read time-out its
        // parent set: one datagram, answered with this process's id.
        "udp-answer" => {
            // SAFETY: descriptor 0 is the socket its parent gave it, which
            // nothing else here owns.
            let socket = unsafe { UdpSocket::from_raw_fd(0) };
            if socket.read_timeout().ok().flatten() != Some(Duration::from_secs(10)) {
                return 2;
            }
            let mut buf = [0u8; 64];
            let answer = format!("answered by {}", std::process::id());
            let answered = socket.recv_from(&mut buf).and_then(|(_, from)| socket.send_to(answer.as_bytes(), from));
            i32::from(answered.is_err())
        }
        "exec" => match program() {
            Ok(exe) => {
                println!("{:?}", Command::new(exe).args(["child", "echo"]).exec().kind());
                0
            }
            Err(_) => 1,
        },
        _ => 99,
    }
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if let [_, child, mode, ..] = args.as_slice()
        && child == "child"
    {
        std::process::exit(child_mode(mode));
    }
    if let [_, host, ip, tcp, udp] = args.as_slice()
        && host == "host"
        && let (Ok(ip), Ok(tcp), Ok(udp)) = (ip.parse(), tcp.parse(), udp.parse())
    {
        let _ = HOST.set((ip, tcp, udp));
    }
    let exe = program().map(|p| p.display().to_string()).unwrap_or_else(|e| e);
    println!("stdlib: {exe}, terminal {}", io::stdout().is_terminal());
    let checks: &[(&str, fn() -> Check)] = &[
        ("startup", check_startup),
        ("standard streams", check_standard_streams),
        ("formatting and numbers", check_formatting_and_numbers),
        ("heap", check_heap),
        ("collections", check_collections),
        ("files", check_files),
        ("directories", check_directories),
        ("file locks", check_file_locks),
        ("threads", check_threads),
        ("synchronization", check_synchronization),
        ("thread locals", check_thread_locals),
        ("panics", check_panics),
        ("backtraces", check_backtraces),
        ("time", check_time),
        ("processes", check_processes),
        ("pipes", check_pipes),
        ("sockets", check_sockets),
        ("unix streams", check_unix_streams),
        ("unix datagrams", check_unix_datagrams),
        ("shared listeners", check_shared_listeners),
        ("shared connections", check_shared_connections),
        ("tcp", check_tcp),
        ("tcp bulk", check_tcp_bulk),
        ("tcp last writes", check_tcp_last_writes),
        ("tcp behaviour", check_tcp_behaviour),
        ("udp", check_udp),
        ("names", check_names),
        ("the host", check_host),
        ("stack", check_stack),
    ];
    let mut failures = 0;
    for (name, check) in checks {
        match check() {
            Ok(()) => println!("ok - {name}"),
            Err(reason) => {
                failures += 1;
                println!("not ok - {name}: {reason}");
            }
        }
    }
    println!("{}: {failures} failed", if failures == 0 { "PASS" } else { "FAIL" });
    std::process::exit(failures);
}
