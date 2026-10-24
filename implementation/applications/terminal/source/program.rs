// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Programs run in the terminal.
//!
//! A program typed at the prompt (found on `PATH`, or named by a path) runs
//! as a child of the terminal, and the shell waits for it. Its standard
//! streams are the terminal: one end of a socket pair whose other end the
//! terminal keeps, plus the terminal's state page (`proto::tty`), which
//! makes the socket a terminal to the program (`isatty`, its size, its
//! modes). While it runs, its output comes to the screen and the keys typed
//! go to it — a line at a time, edited in the window, in canonical mode;
//! key by key otherwise. What is typed or pasted waits in the [`Console`]
//! until the program has room for it, so that nothing is lost when it reads
//! slowly. Ctrl+C sends it `SIGINT`, with the programs it started, as a
//! Unix terminal sends its foreground job: those that do not handle it end
//! (a program it ends stops the command line, as a shell stops once its
//! child dies of it). Redirections give it files instead of the
//! terminal, and text piped from or to a built-in command travels through a
//! socket of its own; what it pipes on is kept up to [`PIPE_LIMIT`].

use core::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use abi::signals::{PEER_CLOSED, PEER_WRITE_DISABLED, READABLE, SIGNALED, TERMINATED, WRITABLE};
use abi::startup::role;
use abi::{Error, map_flags};
use ipc::WaitSet;
use proto::fs::vfs;
use proto::tty::{self, Tty};
use rt::object::{Channel, Event, Handle, Process, Socket};
use rt::process::{Spawn, SpawnError, is_executable};
use rt::sync::Mutex;
use terminal_core::text::{Capture, Utf8Stream};

use crate::job::Output;
use crate::shell::PIPE_LIMIT;

/// `SIGINT`, which Ctrl+C sends.
const SIGINT: usize = 2;

/// What was typed for the foreground program and not yet written to it.
#[derive(Default)]
struct Typed {
    bytes: Vec<u8>,
    /// The end of the input (Ctrl+D) comes after them.
    end: bool,
}

/// The terminal side of the programs this window runs.
pub struct Console {
    /// The terminal's state, shared with its programs.
    pub tty: Tty,
    /// Whether a program in the foreground reads the terminal.
    foreground: AtomicBool,
    /// Wakes the shell waiting for a program (Ctrl+C was pressed, or input
    /// was typed).
    wake: Event,
    /// Typed for the foreground program, waiting for room in its input.
    typed: Mutex<Typed>,
    /// Input typed for a program that it did not read: the next program
    /// gets it, or else the shell, as from a Unix terminal's buffer.
    unread: Mutex<Vec<u8>>,
}

impl Console {
    pub fn new(rows: u16, cols: u16) -> Option<Arc<Console>> {
        Some(Arc::new(Console {
            tty: Tty::create(0, rows, cols)?,
            foreground: AtomicBool::new(false),
            wake: Event::create().ok()?,
            typed: Mutex::new(Typed::default()),
            unread: Mutex::new(Vec::new()),
        }))
    }

    /// Takes the input no program read (for the shell, once the command
    /// line is done).
    pub fn take_unread(&self) -> Vec<u8> {
        core::mem::take(&mut *self.unread.lock())
    }

    /// Whether a program in the foreground reads the terminal.
    pub fn program_running(&self) -> bool {
        self.foreground.load(Ordering::Acquire)
    }

    /// Sends what was typed to the foreground program: it waits here until
    /// the program has room for it, never blocking the window.
    pub fn send(&self, bytes: &[u8]) {
        if !self.program_running() || bytes.is_empty() {
            return;
        }
        self.typed.lock().bytes.extend_from_slice(bytes);
        let _ = self.wake.signal();
    }

    /// Ends the foreground program's input (Ctrl+D): it reads the end of
    /// the file once it has read what is there.
    pub fn end_input(&self) {
        if self.program_running() {
            self.typed.lock().end = true;
            let _ = self.wake.signal();
        }
    }

    /// Wakes the shell waiting for a program (after setting the interrupt
    /// flag).
    pub fn wake(&self) {
        let _ = self.wake.signal();
    }

    /// Whether typed input goes a line at a time (else key by key).
    pub fn canonical(&self) -> bool {
        self.tty.lflag() & tty::lflag::ICANON != 0
    }

    /// Whether typed input is shown.
    pub fn echo(&self) -> bool {
        self.tty.lflag() & tty::lflag::ECHO != 0
    }

    /// Whether Ctrl+C ends the program (else it is sent to it).
    pub fn signals(&self) -> bool {
        self.tty.lflag() & tty::lflag::ISIG != 0
    }

    /// Writes what was typed to the program's input `term`, as much as
    /// fits; returns whether some is left to write.
    fn feed(&self, term: &Socket) -> bool {
        let mut typed = self.typed.lock();
        while !typed.bytes.is_empty() {
            match term.write(&typed.bytes) {
                Ok(n) => {
                    typed.bytes.drain(..n);
                }
                Err(Error::ShouldWait) => return true,
                // The program closed its input: nothing more goes there.
                Err(_) => {
                    typed.bytes.clear();
                    typed.end = false;
                    return false;
                }
            }
        }
        if typed.end {
            typed.end = false;
            let _ = term.shutdown();
        }
        false
    }
}

/// A program's standard input.
pub enum Input {
    Terminal,
    /// Text from a built-in command or a pipe.
    Text(Vec<u8>),
    /// A file (`< FILE`).
    File(Channel),
}

/// Where a program's standard output or error goes.
pub enum Destination {
    Terminal,
    /// Kept for the next command of a pipe.
    Capture,
    /// A file (`> FILE`, `>> FILE`).
    File(Channel),
}

/// What to run.
pub struct Run<'a> {
    pub path: &'a str,
    /// Arguments, the first being the name the program was invoked by.
    pub args: &'a [String],
    pub env: Vec<String>,
    pub cwd: &'a str,
    pub stdin: Input,
    pub stdout: Destination,
    pub stderr: Destination,
}

/// How a program ended.
pub struct Ended {
    /// The shell's exit status: the program's (0-255), or 128 plus the
    /// signal that ended it.
    pub status: i32,
    /// What ended it, if it did not exit by itself.
    pub reason: Option<&'static str>,
    /// What it wrote for the next command (cut at [`PIPE_LIMIT`]).
    pub captured: Capture,
}

/// The message for a program that could not start.
fn spawn_error(path: &str, e: SpawnError) -> String {
    match e {
        SpawnError::NotExecutable => format!("{path}: cannot run this file (it is not a program)"),
        SpawnError::TooLarge => format!("{path}: the argument list is too long"),
        e => format!("{path}: {e}"),
    }
}

/// Reads what a socket has without waiting into `f`: `Some` with the bytes
/// read, `None` at the end of the stream.
fn drain(s: &Socket, mut f: impl FnMut(&[u8])) -> Option<()> {
    let mut buf = [0u8; 16 * 1024];
    loop {
        match s.read(&mut buf) {
            Ok(n) => f(&buf[..n]),
            Err(Error::ShouldWait) => return Some(()),
            Err(_) => return None,
        }
    }
}

/// What the exit code of a program means to the shell.
fn ended(code: i64) -> (i32, Option<&'static str>) {
    match abi::exit_signal(code) {
        None => ((code & 0xff) as i32, None),
        // Ctrl+C, which the screen shows (^C).
        Some(s) if s as usize == SIGINT && code != abi::EXIT_CODE_KILLED => (128 + s as i32, None),
        Some(s) => {
            let what = match s {
                // Killed, as a shell says of SIGKILL, and why.
                9 if code == abi::EXIT_CODE_OUT_OF_MEMORY => "Killed: out of memory",
                4 => "Illegal instruction",
                6 => "Aborted",
                8 => "Floating point exception",
                9 => "Killed",
                11 => "Segmentation fault",
                13 => "Broken pipe",
                15 => "Terminated",
                _ => "Ended by a signal",
            };
            (128 + s as i32, Some(what))
        }
    }
}

/// A program to start: its file's path and image, mapped, and the
/// arguments it starts with.
struct Program {
    path: String,
    image: rt::vm::Mapping,
    size: usize,
    args: Vec<String>,
}

/// What runs the file at `path`, started with `args`: the file, or for a
/// script the interpreter its first line names (`#!`, from the directory
/// `cwd`), given its argument and the script's path before the arguments
/// but the first, as `posix_spawn` runs one. An interpreter may be a script
/// too, `rt::process::SCRIPT_DEPTH` deep.
fn program(fs: &vfs::Client, cwd: &str, path: &str, args: &[String]) -> Result<Program, String> {
    let (mut path, mut args) = (path.to_string(), args.to_vec());
    let mut depth = 0;
    loop {
        let (vmo, size) = match fs.read_file(path.clone()) {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => return Err(format!("{path}: {e}")),
            Err(_) => return Err("the file system is not responding".into()),
        };
        let size = size as usize;
        let image = rt::vm::Mapping::new(vmo, size.max(1), map_flags::READ).map_err(|e| format!("{path}: {e}"))?;
        // SAFETY: a read-only mapping of the file, which nothing changes:
        // the system image's pages, or a copy made for us alone.
        let bytes = unsafe { &image.as_slice()[..size] };
        if is_executable(bytes) {
            return Ok(Program { path, image, size, args });
        }
        if !bytes.starts_with(b"#!") {
            return Err(spawn_error(&path, SpawnError::NotExecutable));
        }
        if depth == rt::process::SCRIPT_DEPTH {
            return Err(format!("{path}: its interpreter is a script, and so on, too deep"));
        }
        depth += 1;
        let interpreter = rt::process::interpreter(bytes)
            .and_then(|i| Some((core::str::from_utf8(i.path).ok()?, i.argument.map(core::str::from_utf8))))
            .ok_or_else(|| format!("{path}: its first line names no interpreter that can run it"))?;
        let (interpreter, argument) = match interpreter {
            (interpreter, Some(Ok(argument))) => (interpreter, Some(argument)),
            (interpreter, None) => (interpreter, None),
            (_, Some(Err(_))) => return Err(format!("{path}: its interpreter's argument is not UTF-8")),
        };
        let mut started = vec![interpreter.to_string()];
        started.extend(argument.map(str::to_string));
        started.push(core::mem::replace(&mut path, files::path::resolve(cwd, interpreter)));
        started.extend(args.drain(1.min(args.len())..));
        args = started;
    }
}

/// Starts `run` and waits for it, showing its output in `out`. Ctrl+C
/// (`interrupt`, with [`Console::wake`]) sends it `SIGINT` each time, with
/// the programs it started; the flag stays set once it is over only if
/// that ended it, which stops the command line.
pub fn run(console: &Console, run: Run, out: &Output, interrupt: &AtomicBool) -> Result<Ended, String> {
    // The program's image.
    let fs = proto::connect(vfs::NAME).map(vfs::Client::new).map_err(|_| "the file system is unavailable")?;
    let program = program(&fs, run.cwd, run.path, run.args)?;

    // The terminal: a fresh pair for each program, so that the end of one
    // program's input (Ctrl+D) is not the next one's.
    let (term, program_side) = Socket::create().map_err(|e| format!("{e}"))?;
    console.tty.set_socket(program_side.0.koid());
    let dup = |s: &Socket| s.duplicate(None).map_err(|e| format!("{e}"));
    // Our own handle to the program's side, to take back what it does not
    // read of the input typed for it.
    let input_side = dup(&program_side)?;
    let mut handles: Vec<(u32, Handle)> = Vec::new();
    let mut feed: Option<(Socket, Vec<u8>)> = None;
    match run.stdin {
        Input::Terminal => handles.push((role::FD, dup(&program_side)?.into_handle())),
        Input::File(ch) => handles.push((role::FD, ch.into_handle())),
        Input::Text(text) => {
            let (ours, theirs) = Socket::create().map_err(|e| format!("{e}"))?;
            handles.push((role::FD, theirs.into_handle()));
            feed = Some((ours, text));
        }
    }
    // One socket for what goes to the next command, whichever streams go
    // there.
    let mut capture: Option<Socket> = None;
    let mut capture_side: Option<Socket> = None;
    for (fd, dest) in [(role::FD + 1, run.stdout), (role::FD + 2, run.stderr)] {
        let handle = match dest {
            Destination::Terminal => dup(&program_side)?.into_handle(),
            Destination::File(ch) => ch.into_handle(),
            Destination::Capture => {
                let side = match capture_side.take() {
                    Some(side) => side,
                    None => {
                        let (ours, theirs) = Socket::create().map_err(|e| format!("{e}"))?;
                        capture = Some(ours);
                        theirs
                    }
                };
                let handle = dup(&side)?.into_handle();
                capture_side = Some(side);
                handle
            }
        };
        handles.push((fd, handle));
    }
    drop(capture_side);
    drop(program_side);
    let registry = proto::with_registry(|r| r.clone_registry());
    if let Ok(Ok(Ok(ch))) = registry {
        handles.push((role::REGISTRY, ch.into_handle()));
    }
    if let Ok(h) = console.tty.vmo().0.duplicate(None) {
        handles.push((role::TERMINAL, h));
    }

    // Named after the file run, a script's too.
    let name = run.path.rsplit('/').next().unwrap_or(run.path);
    let mut spawn = Spawn::new(name).path(&program.path).cwd(run.cwd);
    spawn.args = program.args.iter().map(|a| a.as_bytes()).collect();
    spawn.env = run.env.iter().map(|e| e.as_bytes()).collect();
    spawn.handles = handles;
    // What earlier programs left unread comes first.
    let left = core::mem::take(&mut *console.unread.lock());
    {
        let mut typed = console.typed.lock();
        let later = core::mem::take(&mut typed.bytes);
        typed.bytes = left;
        typed.bytes.extend_from_slice(&later);
        typed.end = false;
    }
    // The program runs from the file's pages where it can.
    let process: Process =
        spawn.start_file(program.image.vmo(), program.size).map_err(|e| spawn_error(&program.path, e))?;
    drop(program);

    console.foreground.store(true, Ordering::Release);
    let _ = console.wake.clear();
    let mut captured = Capture::new(PIPE_LIMIT);
    let mut screen = Utf8Stream::new();
    let mut term_open = true;
    let mut typed_left = console.feed(&term);
    const TERM: u64 = 1;
    const PROCESS: u64 = 2;
    const WAKE: u64 = 3;
    const CAPTURE: u64 = 4;
    const FEED: u64 = 5;
    loop {
        let mut ws = WaitSet::new();
        ws.add(process.raw(), TERMINATED, PROCESS);
        ws.add(console.wake.raw(), SIGNALED, WAKE);
        if term_open {
            // Its output, and room for what was typed for it.
            let room = if typed_left { WRITABLE } else { 0 };
            ws.add(term.raw(), READABLE | PEER_CLOSED | PEER_WRITE_DISABLED | room, TERM);
        }
        if let Some(c) = &capture {
            ws.add(c.raw(), READABLE | PEER_CLOSED | PEER_WRITE_DISABLED, CAPTURE);
        }
        if let Some((s, _)) = &feed {
            ws.add(s.raw(), WRITABLE | PEER_CLOSED, FEED);
        }
        let Ok(ready) = ws.wait(abi::DEADLINE_INFINITE) else { break };
        let mut exited = false;
        for (key, _) in ready {
            match key {
                TERM => {
                    term_open = drain(&term, |b| out.raw(&screen.push(b))).is_some();
                    typed_left = term_open && console.feed(&term);
                }
                CAPTURE => {
                    let mut full = false;
                    let open = capture.as_ref().and_then(|c| drain(c, |b| full |= !captured.push(b)));
                    // Full: the program's next write finds the pipe closed,
                    // and it ends (or carries on without it), as a Unix
                    // program does when the reader went away.
                    if open.is_none() || full {
                        capture = None;
                    }
                }
                FEED => {
                    if let Some((s, text)) = &mut feed {
                        match s.write(text) {
                            Ok(n) => {
                                text.drain(..n);
                            }
                            Err(Error::ShouldWait) => {}
                            Err(_) => text.clear(),
                        }
                        if text.is_empty() {
                            // The end of the input.
                            feed = None;
                        }
                    }
                }
                WAKE => {
                    let _ = console.wake.clear();
                    if interrupt.swap(false, Ordering::Relaxed) {
                        // The whole job, as on a Unix terminal: `gcc` with the
                        // `cc1` and `as` it is running.
                        let _ = process.signal(SIGINT, abi::kill_flags::DESCENDANTS);
                    }
                    typed_left = term_open && console.feed(&term);
                }
                PROCESS => exited = true,
                _ => {}
            }
        }
        if exited {
            break;
        }
    }
    console.foreground.store(false, Ordering::Release);
    // What it wrote last.
    drain(&term, |b| out.raw(&screen.push(b)));
    out.raw(&screen.finish());
    // What was typed for it and not read stays for the next reader: what it
    // left in its input, then what had no room there yet.
    let mut unread = Vec::new();
    drain(&input_side, |b| unread.extend_from_slice(b));
    unread.extend_from_slice(&core::mem::take(&mut console.typed.lock().bytes));
    console.unread.lock().extend_from_slice(&unread);
    if let Some(c) = &capture {
        drain(c, |b| {
            captured.push(b);
        });
    }
    let code = process.info().map(|i| i.exit_code).unwrap_or(abi::EXIT_CODE_CRASHED);
    let (status, mut reason) = ended(code);
    if abi::exit_signal(code) == Some(SIGINT as u32) {
        interrupt.store(true, Ordering::Relaxed);
    }
    // Ended by the pipe the shell closed: that is said already.
    if captured.cut() && reason == Some("Broken pipe") {
        reason = None;
    }
    Ok(Ended { status, reason, captured })
}
