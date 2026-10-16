// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `stdcrates`: checks that crates which need Rust's standard library work on Veda as its ports
//! have them (`implementation/ports/libc`, `getrandom`, `getrandom-0.3`, `rustix`, `tokio`, `mio`
//! and `socket2`): libc's calls of the C library, both releases of getrandom, rand's generator,
//! which stands on one, rustix's files and processes, tempfile, which stands on getrandom and
//! rustix, and tokio's tasks, timers and sockets, on the C library's epoll and eventfd (mio's).
//!
//! `systest` runs it inside Veda. It says `ok - <check>` or `not ok - <check>: <why>` for each
//! check, then `PASS: 0 failed` (or `FAIL: n failed`), and exits with the number that failed,
//! as `verification/rust/stdlib.rs` does. `stdcrates child` is the child process tokio's check
//! starts.

use std::ffi::CStr;
use std::io::{Read, Seek, SeekFrom, Write};
use std::time::{Duration, Instant};

use rand::RngExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

type Check = Result<(), String>;

/// A check, and what the report calls it.
type Named = (&'static str, fn() -> Check);

fn ensure(ok: bool, what: &str) -> Check {
    if ok { Ok(()) } else { Err(what.into()) }
}

/// The C library through libc: the process's id, the system's name, random bytes.
fn check_libc() -> Check {
    // SAFETY: `getpid` takes nothing.
    let pid = unsafe { libc::getpid() };
    ensure(pid > 0, "getpid")?;
    // SAFETY: an all-zero `utsname` is a valid one, for `uname` to fill.
    let mut name: libc::utsname = unsafe { std::mem::zeroed() };
    // SAFETY: `name` is a `utsname`.
    ensure(unsafe { libc::uname(&mut name) } == 0, "uname")?;
    // SAFETY: `uname` wrote a NUL-terminated string there.
    let system = unsafe { CStr::from_ptr(name.sysname.as_ptr()) };
    ensure(system.to_bytes() == b"Veda", "uname's system is not Veda")?;
    let mut bytes = [0u8; 32];
    // SAFETY: `bytes` holds as many bytes as asked for.
    let n = unsafe { libc::getrandom(bytes.as_mut_ptr().cast(), bytes.len(), 0) };
    ensure(n == bytes.len() as isize && bytes != [0; 32], "getrandom through libc")
}

/// Random bytes from both releases of getrandom: never the same twice.
fn check_getrandom() -> Check {
    let (mut first, mut second) = ([0u8; 32], [0u8; 32]);
    getrandom::fill(&mut first).map_err(|e| format!("getrandom 0.4: {e}"))?;
    getrandom::fill(&mut second).map_err(|e| format!("getrandom 0.4: {e}"))?;
    ensure(first != second, "getrandom 0.4 gave the same bytes twice")?;
    getrandom_0_3::fill(&mut first).map_err(|e| format!("getrandom 0.3: {e}"))?;
    ensure(first != second, "getrandom 0.3 gave the bytes 0.4 did")?;
    let a = getrandom::u64().map_err(|e| format!("getrandom 0.4: {e}"))?;
    let b = getrandom_0_3::u64().map_err(|e| format!("getrandom 0.3: {e}"))?;
    ensure(a != b, "both releases gave the same number")
}

/// rand's generator, which its first use seeds from getrandom.
fn check_rand() -> Check {
    let mut rng = rand::rng();
    let (a, b): (u64, u64) = (rng.random(), rng.random());
    ensure(a != b, "rand gave the same number twice")?;
    let roll = rng.random_range(1..=6);
    ensure((1..=6).contains(&roll), "random_range out of its range")
}

/// rustix's calls, on the C library: the process, and a file made, written, read and removed.
fn check_rustix() -> Check {
    use rustix::fs::{self, Mode, OFlags};
    // SAFETY: `getpid` takes nothing.
    let pid = unsafe { libc::getpid() };
    ensure(rustix::process::getpid().as_raw_nonzero().get() == pid, "rustix's process id is not libc's")?;
    let system = fs::stat("/system").map_err(|e| format!("stat /system: {e}"))?;
    ensure(fs::FileType::from_raw_mode(system.st_mode) == fs::FileType::Directory, "/system is no directory")?;
    let dir = std::env::temp_dir().join(format!("stdcrates-{pid}"));
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join("rustix");
    let file = fs::open(&path, OFlags::CREATE | OFlags::RDWR | OFlags::TRUNC | OFlags::CLOEXEC, Mode::from(0o644))
        .map_err(|e| format!("open: {e}"))?;
    let written = rustix::io::write(&file, b"veda").map_err(|e| format!("write: {e}"))?;
    ensure(written == 4, "a short write")?;
    ensure(fs::fstat(&file).map_err(|e| format!("fstat: {e}"))?.st_size == 4, "fstat's size")?;
    let mut back = [0u8; 4];
    let read = rustix::io::pread(&file, &mut back, 0).map_err(|e| format!("pread: {e}"))?;
    ensure(read == 4 && &back == b"veda", "pread gave other bytes")?;
    drop(file);
    fs::unlink(&path).map_err(|e| format!("unlink: {e}"))?;
    ensure(fs::stat(&path).is_err(), "the file is still there")?;
    std::fs::remove_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))
}

/// tempfile: a named file and a directory of their own, gone when dropped.
fn check_tempfile() -> Check {
    let mut file = tempfile::NamedTempFile::new().map_err(|e| format!("NamedTempFile: {e}"))?;
    file.write_all(b"temporary").map_err(|e| format!("write: {e}"))?;
    file.seek(SeekFrom::Start(0)).map_err(|e| format!("seek: {e}"))?;
    let mut text = String::new();
    file.read_to_string(&mut text).map_err(|e| format!("read: {e}"))?;
    ensure(text == "temporary", "it read back other text")?;
    let path = file.path().to_path_buf();
    drop(file);
    ensure(!path.exists(), "the named file outlived its handle")?;
    let dir = tempfile::tempdir().map_err(|e| format!("tempdir: {e}"))?;
    std::fs::write(dir.path().join("inside"), b"x").map_err(|e| format!("a file inside: {e}"))?;
    let kept = dir.path().to_path_buf();
    drop(dir);
    ensure(!kept.exists(), "the directory outlived its handle")
}

/// Connections that echo what comes on them, at once.
const CONNECTIONS: usize = 16;
/// What each sends and gets back.
const ECHOED: usize = 256 * 1024;

/// An echo server on the loopback interface, and `CONNECTIONS` clients at
/// once, each writing from one task and reading from another; then a
/// timer, a time-out, and datagrams.
async fn echoes() -> Check {
    let e = |what: &'static str| move |err: std::io::Error| format!("{what}: {err}");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.map_err(e("bind"))?;
    let addr = listener.local_addr().map_err(e("local_addr"))?;
    let server = tokio::spawn(async move {
        for _ in 0..CONNECTIONS {
            let (mut conn, _) = listener.accept().await?;
            tokio::spawn(async move {
                let (mut r, mut w) = conn.split();
                tokio::io::copy(&mut r, &mut w).await?;
                w.shutdown().await
            });
        }
        Ok::<(), std::io::Error>(())
    });
    let mut clients = Vec::new();
    for c in 0..CONNECTIONS {
        clients.push(tokio::spawn(async move {
            let stream = tokio::net::TcpStream::connect(addr).await?;
            stream.set_nodelay(true)?;
            let (mut r, mut w) = stream.into_split();
            let data: Vec<u8> = (0..ECHOED).map(|i| (i * 7 + c) as u8).collect();
            let sent = data.clone();
            let writer = tokio::spawn(async move {
                for piece in sent.chunks(10_000) {
                    w.write_all(piece).await?;
                }
                w.shutdown().await
            });
            let mut back = Vec::with_capacity(ECHOED);
            r.read_to_end(&mut back).await?;
            writer.await.map_err(std::io::Error::other)??;
            Ok::<bool, std::io::Error>(back == data)
        }));
    }
    for (c, client) in clients.into_iter().enumerate() {
        let same = client.await.map_err(|err| format!("client {c}: {err}"))?.map_err(e("a client"))?;
        ensure(same, &format!("client {c} got other bytes back"))?;
    }
    server.await.map_err(|err| format!("the server: {err}"))?.map_err(e("the server"))?;
    // A timer, and a read that times out.
    let started = Instant::now();
    tokio::time::sleep(Duration::from_millis(30)).await;
    ensure(started.elapsed() >= Duration::from_millis(30), "the timer fired early")?;
    let quiet = tokio::net::UdpSocket::bind("127.0.0.1:0").await.map_err(e("bind"))?;
    let mut buf = [0u8; 16];
    let timed_out = tokio::time::timeout(Duration::from_millis(30), quiet.recv(&mut buf)).await;
    ensure(timed_out.is_err(), "a read with nothing to read ended")?;
    // Datagrams.
    let other = tokio::net::UdpSocket::bind("127.0.0.1:0").await.map_err(e("bind"))?;
    let to = quiet.local_addr().map_err(e("local_addr"))?;
    other.send_to(b"datagram", to).await.map_err(e("send_to"))?;
    let (n, from) = quiet.recv_from(&mut buf).await.map_err(e("recv_from"))?;
    ensure(&buf[..n] == b"datagram" && Some(from) == other.local_addr().ok(), "the datagram")
}

/// The same over a Unix domain socket bound to a path in `/tmp`: a
/// listener and `CONNECTIONS` clients at once, which see who they are
/// connected to; and a pair of datagram sockets.
async fn unix_echoes() -> Check {
    let e = |what: &'static str| move |err: std::io::Error| format!("{what}: {err}");
    let path = std::path::PathBuf::from(format!("/tmp/stdcrates-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let listener = tokio::net::UnixListener::bind(&path).map_err(e("bind"))?;
    let server = tokio::spawn(async move {
        for _ in 0..CONNECTIONS {
            let (mut conn, _) = listener.accept().await?;
            tokio::spawn(async move {
                let (mut r, mut w) = conn.split();
                tokio::io::copy(&mut r, &mut w).await?;
                w.shutdown().await
            });
        }
        Ok::<(), std::io::Error>(())
    });
    let mut clients = Vec::new();
    for c in 0..CONNECTIONS {
        let path = path.clone();
        clients.push(tokio::spawn(async move {
            let stream = tokio::net::UnixStream::connect(&path).await?;
            let peer = stream.peer_cred()?;
            let named = stream.peer_addr()?.as_pathname() == Some(path.as_path());
            let (mut r, mut w) = stream.into_split();
            let data: Vec<u8> = (0..ECHOED).map(|i| (i * 13 + c) as u8).collect();
            let sent = data.clone();
            let writer = tokio::spawn(async move {
                for piece in sent.chunks(10_000) {
                    w.write_all(piece).await?;
                }
                w.shutdown().await
            });
            let mut back = Vec::with_capacity(ECHOED);
            r.read_to_end(&mut back).await?;
            writer.await.map_err(std::io::Error::other)??;
            Ok::<bool, std::io::Error>(back == data && named && peer.pid() == Some(std::process::id() as i32))
        }));
    }
    for (c, client) in clients.into_iter().enumerate() {
        let same = client.await.map_err(|err| format!("client {c}: {err}"))?.map_err(e("a client"))?;
        ensure(same, &format!("unix client {c} got other bytes back, or the wrong peer"))?;
    }
    server.await.map_err(|err| format!("the server: {err}"))?.map_err(e("the server"))?;
    std::fs::remove_file(&path).map_err(e("remove_file"))?;
    let (a, b) = tokio::net::UnixDatagram::pair().map_err(e("UnixDatagram::pair"))?;
    a.send(b"one").await.map_err(e("send"))?;
    a.send(b"two").await.map_err(e("send"))?;
    let mut buf = [0u8; 16];
    let first = b.recv(&mut buf).await.map_err(e("recv"))?;
    ensure(&buf[..first] == b"one", "the first datagram")?;
    let second = b.recv(&mut buf).await.map_err(e("recv"))?;
    ensure(&buf[..second] == b"two", "the second datagram")
}

/// What the child process says, and how it ends.
const CHILD_SAYS: &str = "hello from a child";
const CHILD_EXIT: i32 = 3;

/// A child process: started, its output read through a pipe and its end
/// waited for, both through the reactor (its end through a `pidfd_open`
/// descriptor).
async fn child() -> Check {
    let program = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let out = tokio::process::Command::new(program).arg("child").output().await.map_err(|e| format!("child: {e}"))?;
    let said = String::from_utf8_lossy(&out.stdout);
    ensure(out.status.code() == Some(CHILD_EXIT) && said.trim_end() == CHILD_SAYS, "the child's output and status")
}

/// tokio on its runtime of one thread, and on one of several (whose
/// threads wake each other, and the reactor, through an eventfd): TCP, UDP
/// and Unix domain sockets, timers, a child process.
fn check_tokio() -> Check {
    use tokio::runtime::Builder;
    let runtimes = [("one thread", Builder::new_current_thread()), ("several", Builder::new_multi_thread())];
    for (name, mut builder) in runtimes {
        let runtime = builder.worker_threads(4).enable_all().build().map_err(|e| format!("{name}: {e}"))?;
        runtime.block_on(echoes()).map_err(|why| format!("{name}: {why}"))?;
        runtime.block_on(unix_echoes()).map_err(|why| format!("{name}: {why}"))?;
        runtime.block_on(child()).map_err(|why| format!("{name}: {why}"))?;
    }
    Ok(())
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("child") {
        println!("{CHILD_SAYS}");
        std::process::exit(CHILD_EXIT);
    }
    let checks: [Named; 6] = [
        ("libc", check_libc),
        ("getrandom", check_getrandom),
        ("rand", check_rand),
        ("rustix", check_rustix),
        ("tempfile", check_tempfile),
        ("tokio", check_tokio),
    ];
    let mut failures = 0;
    for (name, check) in checks {
        match check() {
            Ok(()) => println!("ok - {name}"),
            Err(why) => {
                failures += 1;
                println!("not ok - {name}: {why}");
            }
        }
    }
    println!("{}: {failures} failed", if failures == 0 { "PASS" } else { "FAIL" });
    std::process::exit(failures);
}
