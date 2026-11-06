// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! checks — a Linux program on Rust's standard library, built for Linux on
//! musl as Rust ships it (statically), that checks what Veda's Linux
//! compatibility gives the standard library: threads and their locks and
//! channels, thread-local storage, files and directories, the program's
//! path, time, a panic caught as it unwinds, much memory, and TCP on the
//! loopback interface.
//!
//! Each check prints "ok - NAME" or "not ok - NAME: REASON", and the last
//! line is "PASS: N failed"; the program exits with the number of failed
//! checks. `systest` runs it inside Veda in a scratch directory.

use std::cell::Cell;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime};

struct Checks {
    failed: u32,
}

impl Checks {
    fn check(&mut self, name: &str, ok: bool, why: impl FnOnce() -> String) {
        if ok {
            println!("ok - {name}");
        } else {
            self.failed += 1;
            println!("not ok - {name}: {}", why());
        }
    }
}

thread_local! {
    static MINE: Cell<u32> = const { Cell::new(0) };
}

fn threads(c: &mut Checks) {
    let counter = Arc::new(Mutex::new(0u32));
    let pair = Arc::new((Mutex::new(0u32), Condvar::new()));
    let (tx, rx) = mpsc::channel();
    let handles: Vec<_> = (0..8)
        .map(|i| {
            let (counter, pair, tx) = (counter.clone(), pair.clone(), tx.clone());
            std::thread::spawn(move || {
                MINE.with(|m| m.set(i + 1));
                for _ in 0..1000 {
                    *counter.lock().unwrap() += 1;
                }
                let (lock, cond) = &*pair;
                *lock.lock().unwrap() += 1;
                cond.notify_one();
                tx.send(i).unwrap();
                MINE.with(Cell::get)
            })
        })
        .collect();
    drop(tx);
    let mine: Vec<u32> = handles.into_iter().map(|h| h.join().unwrap_or(0)).collect();
    let (lock, cond) = &*pair;
    let done = cond.wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(10), |n| *n < 8).unwrap();
    c.check("threads", *done.0 == 8, || format!("{} done", *done.0));
    c.check("mutex", *counter.lock().unwrap() == 8000, || format!("{}", counter.lock().unwrap()));
    c.check("channel", rx.iter().count() == 8, || "messages missing".into());
    c.check("thread-local storage", mine == (1..=8).collect::<Vec<_>>() && MINE.with(Cell::get) == 0, || {
        format!("{mine:?}")
    });
}

fn files(c: &mut Checks) {
    let r = (|| -> std::io::Result<bool> {
        std::fs::write("a.txt", b"hello")?;
        std::fs::rename("a.txt", "b.txt")?;
        let text = std::fs::read_to_string("b.txt")?;
        std::fs::create_dir_all("d/e")?;
        let names: Vec<String> = std::fs::read_dir(".")?.filter_map(|e| e.ok()?.file_name().into_string().ok()).collect();
        let meta = std::fs::metadata("b.txt")?;
        std::fs::remove_file("b.txt")?;
        std::fs::remove_dir_all("d")?;
        Ok(text == "hello" && names.contains(&"b.txt".to_string()) && names.contains(&"d".to_string()) && meta.len() == 5)
    })();
    c.check("files", matches!(r, Ok(true)), || format!("{r:?}"));
    let exe = std::env::current_exe();
    c.check("current_exe", exe.as_ref().is_ok_and(|p| p.is_absolute()), || format!("{exe:?}"));
    let cwd = std::env::current_dir();
    c.check("current_dir", cwd.as_ref().is_ok_and(|p| p.is_absolute()), || format!("{cwd:?}"));
}

fn time(c: &mut Checks) {
    let start = Instant::now();
    std::thread::sleep(Duration::from_millis(20));
    let slept = start.elapsed();
    c.check("sleep", slept >= Duration::from_millis(20) && slept < Duration::from_secs(5), || format!("{slept:?}"));
    let since = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    c.check("system time", since > 1_700_000_000, || format!("{since}"));
}

fn memory(c: &mut Checks) {
    let mut big = vec![0u8; 64 << 20];
    big[(64 << 20) - 1] = 7;
    big.extend_from_slice(&[1; 4096]);
    c.check("memory", big[(64 << 20) - 1] == 7 && big.len() == (64 << 20) + 4096, || "lost".into());
    // Unwound and caught, and said nowhere: the test scripts take a panic's
    // message in the log for a failure.
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let caught = std::panic::catch_unwind(|| panic!("on purpose"));
    std::panic::set_hook(hook);
    c.check("panic unwinds", caught.is_err(), || "not caught".into());
}

fn network(c: &mut Checks) {
    let r = (|| -> std::io::Result<Vec<u8>> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        let server = std::thread::spawn(move || -> std::io::Result<()> {
            let (mut s, _) = listener.accept()?;
            let mut buf = [0u8; 5];
            s.read_exact(&mut buf)?;
            s.write_all(&buf)
        });
        let mut client = TcpStream::connect(addr)?;
        client.write_all(b"hello")?;
        let mut back = vec![0u8; 5];
        client.read_exact(&mut back)?;
        server.join().map_err(|_| std::io::Error::other("server panicked"))??;
        Ok(back)
    })();
    c.check("tcp loopback", r.as_deref().is_ok_and(|b| b == b"hello"), || format!("{r:?}"));
}

/// A program started (the standard library's `posix_spawn`, the C
/// library's vfork and execve), its output and status taken.
fn processes(c: &mut Checks) {
    let exe = std::env::current_exe().unwrap_or_default();
    let out = std::process::Command::new(&exe).arg("child").output();
    let ok = out.as_ref().is_ok_and(|o| o.status.code() == Some(5) && o.stdout == b"child\n");
    c.check("command", ok, || format!("{out:?}"));
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("child") {
        println!("child");
        std::process::exit(5);
    }
    let mut c = Checks { failed: 0 };
    c.check("arguments", std::env::args().count() >= 1, || "none".into());
    threads(&mut c);
    files(&mut c);
    time(&mut c);
    memory(&mut c);
    network(&mut c);
    processes(&mut c);
    println!("PASS: {} failed", c.failed);
    std::process::exit(c.failed as i32);
}
