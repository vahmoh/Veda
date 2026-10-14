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
//! `stdlib child MODE` is the program the process checks start.

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::hash::BuildHasher;
use std::io::{self, IsTerminal, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{FileExt, MetadataExt};
use std::os::unix::net::UnixStream;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
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

fn check_file_locks() -> Check {
    // Veda's files have no locks: the standard library says so.
    let f = io("open", File::open("notes.txt"))?;
    let lock = f.lock().map_err(|e| e.kind());
    ensure(lock == Err(io::ErrorKind::Unsupported), || format!("lock: {lock:?}"))
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
    io("shutdown", a.shutdown(std::net::Shutdown::Write))?;
    let n = io("read after shutdown", b.read(&mut buf))?;
    ensure(n == 0, || format!("read {n} bytes after the peer shut down"))?;
    // No network sockets yet (see documentation/C.md).
    ensure(std::net::TcpStream::connect("127.0.0.1:9").is_err(), || "a TCP connection was made".into())
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
    if let [_, child, mode] = args.as_slice()
        && child == "child"
    {
        std::process::exit(child_mode(mode));
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
