// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The command interpreter ("vsh").
//!
//! A command line is read by `terminal_core::syntax` (words, quotes,
//! `$VAR` and `~` expansion, lists of pipelines with redirections) and run
//! here: `*`/`?` wildcards are matched against file names, and each command
//! is built in (see `commands.rs`) or a program or installed application,
//! which are started as children of the Terminal or through the launcher.
//!
//! Commands write through [`Io`]: their output and their errors each go to
//! the terminal (with colours, through the [`Output`] queue, as commands
//! run on a thread of their own), to the next command of a pipeline, or to
//! a file, as the redirections say. What goes to a pipe or a file is the
//! bytes exactly as written; a file is written as the command goes, a pipe
//! holds at most [`PIPE_LIMIT`]. Ctrl+C sets the shell's interrupt flag:
//! the rest of the command line is skipped, and commands that take long
//! check it as they go.

use core::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use proto::fs::{FsError, MAX_IO, Stat, file, open_flags};
use proto::init::{AppInfo, launcher};
use rt::object::Channel;
use terminal_core::classify::{self, Effect, Lookup};
use terminal_core::edit;
use terminal_core::screen::{Cell, Style, cells, color};
use terminal_core::syntax::{self, Command, Lexer, Routes, Sink, Word};
use terminal_core::text::{Capture, Utf8Stream};

use crate::commands;
use crate::job::Output;
use crate::program::{self, Console, Destination, Input, Run};
use files::path::{display_path, file_name, glob_match, has_wildcards, is_read_only, normalize, resolve};
use files::{Fs, HOME, default_app};

/// Shown after an operation failed because the disk is full.
const NO_SPACE_HINT: &str = "The disk is full: delete files you no longer need ('df' shows how much space is left).";

/// The command history file (kept across sessions).
const HISTORY_FILE: &str = "/home/user/.vsh_history";
/// Commands kept in the history.
const MAX_HISTORY: usize = 500;
/// The most of the history file read when the Terminal starts (its end):
/// far more than [`MAX_HISTORY`] commands typed by hand take, and little
/// enough memory whatever wrote the file.
const HISTORY_BYTES: u64 = 1 << 20;
/// The exit status of a command stopped with Ctrl+C (as in Unix shells).
pub const INTERRUPTED: i32 = 130;
/// The most a pipe between two commands holds (bytes): the commands of a
/// pipeline run one after the other, so what the first writes is kept until
/// the next one runs.
pub const PIPE_LIMIT: usize = 16 << 20;

/// A command's standard input.
pub enum Stdin {
    /// None: it is the first of its pipeline, without `< FILE`.
    None,
    /// What the command before it in the pipeline wrote.
    Bytes(Vec<u8>),
    /// A file (`< FILE`): its absolute path.
    File(String),
}

/// A file a command's output goes to, written a chunk at a time.
struct FileOut {
    /// The redirection's target as typed (for messages).
    name: String,
    /// The open file; `None` once writing it failed.
    file: Option<file::Client>,
    /// What was written and not sent yet.
    buf: Vec<u8>,
    /// Why writing failed.
    error: Option<files::Error>,
    /// The file (device, inode): a command must not read it while it
    /// writes it.
    id: (u64, u64),
}

impl FileOut {
    fn write(&mut self, bytes: &[u8]) {
        if self.file.is_none() {
            return;
        }
        self.buf.extend_from_slice(bytes);
        if self.buf.len() >= MAX_IO as usize {
            self.flush(false);
        }
    }

    /// Sends what was written: whole chunks only, unless `all`.
    fn flush(&mut self, all: bool) {
        let Some(f) = &self.file else { return };
        let least = if all { 1 } else { MAX_IO as usize };
        let mut sent = 0;
        while self.buf.len() - sent >= least {
            let n = (self.buf.len() - sent).min(MAX_IO as usize);
            match f.write(ipc::Bytes(self.buf[sent..sent + n].to_vec())) {
                Ok(Ok(w)) if w > 0 => sent += w as usize,
                Ok(Ok(_)) => {
                    self.fail(files::Error::Fs(FsError::Io));
                    return;
                }
                Ok(Err(e)) => {
                    self.fail(files::Error::Fs(e));
                    return;
                }
                Err(_) => {
                    self.fail(files::Error::Ipc);
                    return;
                }
            }
        }
        self.buf.drain(..sent);
    }

    fn fail(&mut self, e: files::Error) {
        self.error = Some(e);
        self.file = None;
        self.buf = Vec::new();
    }
}

/// Where a command's output and errors go, and its input.
pub struct Io<'a> {
    /// The terminal.
    pub out: &'a Output,
    stdout: Sink,
    stderr: Sink,
    /// What goes to the next command of the pipeline.
    pipe: Capture,
    /// The files its streams go to (see `Routes::files`).
    files: Vec<FileOut>,
    /// Bytes shown on the terminal, as text.
    screen: Utf8Stream,
    stdin: Stdin,
}

impl Io<'_> {
    /// True if the output goes to the terminal (so colours are visible and
    /// what is long or binary may be left out).
    pub fn is_terminal(&self) -> bool {
        self.stdout == Sink::Terminal
    }

    fn send(&mut self, sink: Sink, bytes: &[u8]) {
        match sink {
            Sink::Terminal => {
                let text = self.screen.push(bytes);
                self.out.raw(&text);
            }
            Sink::Pipe => {
                self.pipe.push(bytes);
            }
            Sink::File(i) => {
                if let Some(f) = self.files.get_mut(i) {
                    f.write(bytes);
                }
            }
        }
    }

    /// Writes text in a style (the style only shows on the terminal).
    pub fn styled(&mut self, s: &str, style: Style) {
        match self.stdout {
            Sink::Terminal => self.out.styled(s, style),
            sink => self.send(sink, s.as_bytes()),
        }
    }

    /// Writes plain text.
    pub fn print(&mut self, s: &str) {
        self.styled(s, Style::PLAIN);
    }

    /// Writes text and a newline.
    pub fn println(&mut self, s: &str) {
        self.styled(s, Style::PLAIN);
        self.styled("\n", Style::PLAIN);
    }

    /// Writes text that may contain ANSI colour sequences.
    pub fn raw(&mut self, s: &str) {
        match self.stdout {
            Sink::Terminal => self.out.raw(s),
            sink => self.send(sink, s.as_bytes()),
        }
    }

    /// Writes bytes exactly as they are (shown as text on the terminal).
    pub fn write(&mut self, bytes: &[u8]) {
        let sink = self.stdout;
        self.send(sink, bytes);
    }

    /// Writes a line to the error stream: in `style` on the terminal.
    fn error_line(&mut self, text: &str, style: Style) {
        match self.stderr {
            Sink::Terminal => {
                self.out.finish_line();
                self.out.styled(text, style);
            }
            sink => self.send(sink, text.as_bytes()),
        }
    }

    /// Writes `cmd: message` to the error stream (red on the terminal);
    /// returns exit status 1.
    pub fn error(&mut self, cmd: &str, msg: &str) -> i32 {
        self.error_line(&format!("{cmd}: {msg}\n"), Style::ERROR);
        1
    }

    /// Writes a hint to the error stream (grey on the terminal).
    pub fn hint(&mut self, msg: &str) {
        self.error_line(&format!("{msg}\n"), Style::DIM);
    }

    /// Writes `cmd: what: error` for a failed file operation (with a hint
    /// when the disk is full); returns exit status 1.
    pub fn fs_error(&mut self, cmd: &str, what: &str, e: &files::Error) -> i32 {
        let status = self.error(cmd, &format!("{what}: {e}"));
        if e.is_no_space() {
            self.hint(NO_SPACE_HINT);
        }
        status
    }

    /// Whether the command has a standard input: a pipe or `< FILE`.
    pub fn has_stdin(&self) -> bool {
        !matches!(self.stdin, Stdin::None)
    }

    /// Takes standard input.
    pub fn take_stdin(&mut self) -> Stdin {
        core::mem::replace(&mut self.stdin, Stdin::None)
    }

    /// Writing more is in vain: the pipe is full, or the file failed.
    pub fn stopped(&self) -> bool {
        match self.stdout {
            Sink::Terminal => false,
            Sink::Pipe => self.pipe.cut(),
            Sink::File(i) => self.files.get(i).is_none_or(|f| f.file.is_none()),
        }
    }

    /// Whether the file `st` is one this command writes to.
    pub fn writes_to(&self, st: &Stat) -> bool {
        self.files.iter().any(|f| f.id == (st.device, st.inode))
    }

    /// Ends the command's output: sends what is left to its files, and
    /// returns what goes to the next command and the files that could not
    /// be written (named as typed).
    fn finish(mut self) -> (Capture, Vec<(String, files::Error)>) {
        let rest = self.screen.finish();
        if !rest.is_empty() {
            self.out.raw(&rest);
        }
        let mut failed = Vec::new();
        for f in &mut self.files {
            f.flush(true);
            if let Some(e) = f.error {
                failed.push((f.name.clone(), e));
            }
        }
        (self.pipe, failed)
    }
}

/// The result of Tab completion.
#[derive(Debug, Default)]
pub struct Completion {
    /// Byte offset of the word being completed.
    pub start: usize,
    /// Replacement for the input between `start` and the cursor.
    pub replacement: Option<String>,
    /// Every match (shown when the completion is ambiguous).
    pub candidates: Vec<String>,
}

/// The interpreter state.
pub struct Shell {
    pub cwd: String,
    pub prev_dir: Option<String>,
    pub env: Vec<(String, String)>,
    pub history: Vec<String>,
    /// Exit status of the last command.
    pub status: i32,
    pub fs: Fs,
    launcher: Option<launcher::Client>,
    apps: Option<Vec<AppInfo>>,
    /// The `exit` command ran.
    pub exit_requested: bool,
    /// Terminal width in columns (for formatting tables).
    pub cols: usize,
    /// Set (by Ctrl+C) to stop the command that is running.
    pub interrupt: Arc<AtomicBool>,
    /// The terminal programs run in (none without a window).
    pub console: Option<Arc<Console>>,
}

/// The shell's view of the system, for [`classify::classify`].
struct ShellLookup<'a>(&'a Shell);

impl Lookup for ShellLookup<'_> {
    fn builtin(&self, name: &str) -> Option<&'static str> {
        commands::find(name).map(|b| b.name)
    }

    fn is_app(&self, name: &str) -> bool {
        self.0.apps.as_ref().is_some_and(|apps| find_app(apps, name).is_some())
    }

    fn resolve(&self, cwd: &str, path: &str) -> String {
        resolve(cwd, path)
    }

    fn exists(&self, path: &str) -> bool {
        // A file system that does not answer may hold anything.
        !matches!(self.0.fs.stat(path), Err(files::Error::Fs(FsError::NotFound)))
    }

    fn is_dir(&self, path: &str) -> bool {
        self.0.fs.stat(path).is_ok_and(|st| st.is_dir)
    }

    fn opens_in_app(&self, path: &str) -> bool {
        default_app(path).is_some()
    }
}

/// The installed application that the command name `name` starts (by id
/// or name, ignoring case).
fn find_app<'a>(apps: &'a [AppInfo], name: &str) -> Option<&'a AppInfo> {
    let lower = name.to_ascii_lowercase();
    apps.iter().find(|a| a.id == lower || a.name.to_ascii_lowercase() == lower)
}

impl Shell {
    pub fn new(cwd: &str) -> Shell {
        let fs = Fs::connect();
        let cwd = if fs.is_dir(cwd) { normalize(cwd) } else { HOME.to_string() };
        let env = vec![
            ("HOME".to_string(), HOME.to_string()),
            ("USER".to_string(), "user".to_string()),
            ("HOSTNAME".to_string(), "veda".to_string()),
            ("SHELL".to_string(), "vsh".to_string()),
            ("PATH".to_string(), "/system/bin".to_string()),
            ("PWD".to_string(), cwd.clone()),
            ("TERM".to_string(), "veda-terminal".to_string()),
        ];
        Shell {
            cwd,
            prev_dir: None,
            env,
            history: Vec::new(),
            status: 0,
            fs,
            launcher: None,
            apps: None,
            exit_requested: false,
            cols: 80,
            interrupt: Arc::new(AtomicBool::new(false)),
            console: None,
        }
    }

    /// The command that is running should stop (Ctrl+C was pressed).
    pub fn interrupted(&self) -> bool {
        self.interrupt.load(Ordering::Relaxed)
    }

    /// The prompt: `user@veda:~/Documents$ `.
    pub fn prompt(&self) -> Vec<Cell> {
        let mut c = cells("user@veda", Style::fg(color::BRIGHT_GREEN).bold());
        c.extend(cells(":", Style::PLAIN));
        c.extend(cells(&display_path(&self.cwd), Style::fg(color::BRIGHT_BLUE).bold()));
        c.extend(cells("$ ", Style::PLAIN));
        c
    }

    pub fn var(&self, key: &str) -> Option<String> {
        if key == "?" {
            return Some(self.status.to_string());
        }
        if key == "PWD" {
            return Some(self.cwd.clone());
        }
        self.env.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    }

    pub fn set_var(&mut self, key: &str, value: &str) {
        match self.env.iter_mut().find(|(k, _)| k == key) {
            Some(e) => e.1 = value.to_string(),
            None => self.env.push((key.to_string(), value.to_string())),
        }
    }

    pub fn unset_var(&mut self, key: &str) {
        self.env.retain(|(k, _)| k != key);
    }

    /// Changes the working directory.
    pub fn set_cwd(&mut self, dir: String) {
        if dir != self.cwd {
            self.prev_dir = Some(core::mem::replace(&mut self.cwd, dir));
            let cwd = self.cwd.clone();
            self.set_var("PWD", &cwd);
        }
    }

    /// Absolute form of a path argument.
    pub fn resolve(&self, p: &str) -> String {
        resolve(&self.cwd, p)
    }

    /// Adds a line to the history (skipping blanks and repeats) and to the
    /// history file, Wi-Fi passwords hidden.
    pub fn remember(&mut self, line: &str) {
        let redacted = syntax::redact(line.trim());
        let line = redacted.as_str();
        if line.is_empty() || self.history.last().is_some_and(|l| l == line) {
            return;
        }
        self.history.push(line.to_string());
        if self.history.len() > MAX_HISTORY {
            self.history.remove(0);
        }
        let _ = self.fs.append(HISTORY_FILE, format!("{line}\n").as_bytes());
    }

    /// Loads the history saved by earlier sessions (keeping the file short).
    pub fn load_history(&mut self) {
        let Some((data, cut)) = self.read_end(HISTORY_FILE, HISTORY_BYTES) else { return };
        let text = String::from_utf8_lossy(&data);
        let mut lines = text.lines();
        if cut {
            // Read from the middle of the file: the first line is a part.
            lines.next();
        }
        // Commands as they could have been typed.
        let lines: Vec<&str> = lines.filter(|l| !l.trim().is_empty() && l.len() <= edit::MAX_TEXT).collect();
        let start = lines.len().saturating_sub(MAX_HISTORY);
        self.history = lines[start..].iter().map(|l| l.to_string()).collect();
        if start > 0 || cut {
            let mut trimmed = self.history.join("\n");
            trimmed.push('\n');
            let _ = self.fs.write(HISTORY_FILE, trimmed.as_bytes());
        }
    }

    /// The last `max` bytes of the file at `path`, and whether there is
    /// more before them; `None` if it cannot be read.
    fn read_end(&self, path: &str, max: u64) -> Option<(Vec<u8>, bool)> {
        let (ch, st) = self.fs.client()?.open_file(path.into(), open_flags::READ).ok()?.ok()?;
        let f = file::Client::new(ch);
        let from = st.size.saturating_sub(max);
        let mut data = Vec::new();
        while (data.len() as u64) < st.size - from {
            let at = from + data.len() as u64;
            let chunk = f.read_at(at, (st.size - at).min(u64::from(MAX_IO)) as u32).ok()?.ok()?.0;
            if chunk.is_empty() {
                break;
            }
            data.extend_from_slice(&chunk);
        }
        Some((data, from > 0))
    }

    /// Forgets the history, also on disk.
    pub fn clear_history(&mut self) {
        self.history.clear();
        let _ = self.fs.remove(HISTORY_FILE);
    }

    /// What `line` may do if it runs now (see `terminal_core::classify`).
    pub fn classify(&mut self, line: &str) -> Effect {
        // The applications, for telling them from programs.
        self.apps();
        let env = self.env.clone();
        let (status, cwd) = (self.status, self.cwd.clone());
        let vars = move |key: &str| match key {
            "?" => Some(status.to_string()),
            "PWD" => Some(cwd.clone()),
            _ => env.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone()),
        };
        let lx = Lexer { vars: Some(&vars), home: HOME, lenient: false };
        classify::classify(line, &lx, &self.cwd, &ShellLookup(self))
    }

    // ---- services ----------------------------------------------------------

    pub fn launcher(&mut self) -> Option<&launcher::Client> {
        if self.launcher.is_none() {
            self.launcher = proto::connect(launcher::NAME).ok().map(launcher::Client::new);
        }
        self.launcher.as_ref()
    }

    /// Installed applications (cached).
    pub fn apps(&mut self) -> Vec<AppInfo> {
        if self.apps.is_none() {
            let list = self.launcher().and_then(|l| l.apps().ok());
            if let Some(list) = list {
                self.apps = Some(list);
            }
        }
        self.apps.clone().unwrap_or_default()
    }

    /// Names of the programs in `/system/bin`.
    pub fn programs(&self) -> Vec<String> {
        self.fs
            .read_dir("/system/bin")
            .map(|v| v.into_iter().filter(|e| e.executable && !e.is_dir).map(|e| e.name).collect())
            .unwrap_or_default()
    }

    /// Starts a program or application. `prog` is an application id, a
    /// program name from `/system/bin` or a path to a program. Returns a
    /// display name and the process id.
    pub fn launch(&mut self, prog: &str, args: Vec<String>) -> Result<(String, u64), String> {
        let as_path = prog.contains('/');
        if !as_path {
            let apps = self.apps();
            if let Some(app) = find_app(&apps, prog) {
                let (id, name) = (app.id.clone(), app.name.clone());
                let r = self.launcher().ok_or("the launcher is unavailable")?.launch_app(id, args);
                return launch_result(r).map(|koid| (name, koid));
            }
        }
        let path = if as_path { self.resolve(prog) } else { format!("/system/bin/{prog}") };
        match self.fs.stat(&path) {
            Ok(st) if !st.is_dir => {}
            _ => return Err(format!("{prog}: command not found")),
        }
        if !is_read_only(&path) {
            return Err(format!("{prog}: only programs installed in /system/bin can be started"));
        }
        let name = file_name(&path).to_string();
        let r = self.launcher().ok_or("the launcher is unavailable")?.launch(path, args);
        launch_result(r).map(|koid| (name, koid))
    }

    /// The program a command name runs as a child of the terminal: a path
    /// (with a `/`) to a file, or a file of that name in a directory of
    /// `PATH`. Built-in commands and installed applications come first:
    /// they are not programs here.
    pub fn program_for(&mut self, name: &str) -> Option<String> {
        if commands::find(name).is_some() {
            return None;
        }
        let is_file = |fs: &Fs, p: &str| fs.stat(p).is_ok_and(|s| !s.is_dir);
        if name.contains('/') {
            let path = self.resolve(name);
            return is_file(&self.fs, &path).then_some(path);
        }
        if find_app(&self.apps(), name).is_some() {
            return None;
        }
        let dirs = self.var("PATH").unwrap_or_default();
        for dir in dirs.split(':').filter(|d| d.starts_with('/')) {
            let candidate = format!("{dir}/{name}");
            if is_file(&self.fs, &candidate) {
                return Some(normalize(&candidate));
            }
        }
        None
    }

    // ---- execution ---------------------------------------------------------

    /// Runs a command line, writing output to `out`. After Ctrl+C the rest
    /// of the line is skipped.
    pub fn execute(&mut self, line: &str, out: &Output) {
        // The whole line is checked first; then each item of the list is
        // expanded when its turn comes, so that it sees what the items
        // before it did (`prog; echo $?`).
        let items = match syntax::split_list(line, HOME) {
            Ok(items) => items,
            Err(e) => {
                out.styled(&format!("vsh: {e}\n"), Style::ERROR);
                self.status = 2;
                return;
            }
        };
        for (conn, text) in items {
            if self.interrupted() {
                break;
            }
            match conn {
                syntax::Connector::And if self.status != 0 => continue,
                syntax::Connector::Or if self.status == 0 => continue,
                _ => {}
            }
            let parsed = {
                let vars = |key: &str| self.var(key);
                syntax::parse_item(text, &Lexer { vars: Some(&vars), home: HOME, lenient: false })
            };
            let pipeline = match parsed {
                Ok(p) => p,
                Err(e) => {
                    out.styled(&format!("vsh: {e}\n"), Style::ERROR);
                    self.status = 2;
                    return;
                }
            };
            let Some(pipeline) = pipeline else { continue };
            self.status = self.run_pipeline(pipeline, out);
            out.finish_line();
            if self.exit_requested {
                break;
            }
        }
        if self.interrupted() {
            self.status = INTERRUPTED;
        }
    }

    fn run_pipeline(&mut self, cmds: Vec<Command>, out: &Output) -> i32 {
        let n = cmds.len();
        let mut piped: Option<Vec<u8>> = None;
        let mut status = 0;
        for (i, cmd) in cmds.into_iter().enumerate() {
            if self.interrupted() {
                return INTERRUPTED;
            }
            let last = i + 1 == n;
            let routes = syntax::route(&cmd, last);
            let args = self.expand(&cmd.words);
            let input = piped.take();
            // Redirections first, as POSIX shells do them: a file written
            // to is emptied even if the command then fails; a command
            // whose files cannot be opened does not run.
            let Some((files, stdin_path)) = self.open_redirections(&routes, out) else {
                status = 1;
                continue;
            };
            // Programs get their redirections as files and pipes.
            if let Some(name) = args.first()
                && let Some(path) = self.program_for(name)
            {
                match self.run_program(&path, &args, &routes, files, input, stdin_path, out) {
                    Ok((s, captured)) => {
                        status = s;
                        piped = captured;
                    }
                    Err(s) => return s,
                }
                if self.exit_requested {
                    break;
                }
                continue;
            }
            let mut io = Io {
                out,
                stdout: routes.out,
                stderr: routes.err,
                pipe: Capture::new(PIPE_LIMIT),
                files: files
                    .into_iter()
                    .map(|(name, ch, st)| FileOut {
                        name,
                        file: Some(file::Client::new(ch)),
                        buf: Vec::new(),
                        error: None,
                        id: (st.device, st.inode),
                    })
                    .collect(),
                screen: Utf8Stream::new(),
                stdin: match (stdin_path, input) {
                    (Some(path), _) => Stdin::File(path),
                    (None, Some(bytes)) => Stdin::Bytes(bytes),
                    (None, None) => Stdin::None,
                },
            };
            status = if args.is_empty() { 0 } else { self.run_command(&args, &mut io) };
            let (pipe, failed) = io.finish();
            for (name, e) in failed {
                out.finish_line();
                out.styled(&format!("vsh: {name}: {e}\n"), Style::ERROR);
                if e.is_no_space() {
                    out.styled(&format!("{NO_SPACE_HINT}\n"), Style::DIM);
                }
                status = 1;
            }
            if pipe.cut()
                && let Some(name) = args.first()
            {
                pipe_cut(out, name);
            }
            if routes.out == Sink::Pipe || routes.err == Sink::Pipe {
                piped = Some(pipe.into_bytes());
            }
            if self.exit_requested {
                break;
            }
        }
        status
    }

    /// Opens the files a command's redirections name: those written to
    /// (emptied unless appended to), each with its name as typed and its
    /// details, and checks the one read from (`< FILE`), returning its
    /// absolute path. `None`, the errors shown, if one cannot be opened.
    #[allow(clippy::type_complexity)]
    fn open_redirections(
        &mut self,
        routes: &Routes,
        out: &Output,
    ) -> Option<(Vec<(String, Channel, Stat)>, Option<String>)> {
        let fail = |what: &Word, e: &dyn core::fmt::Display| {
            out.finish_line();
            out.styled(&format!("vsh: {}: {e}\n", what.text), Style::ERROR);
        };
        let mut files = Vec::with_capacity(routes.files.len());
        for (target, append) in &routes.files {
            let abs = self.resolve(&target.text);
            let flags = open_flags::WRITE
                | open_flags::CREATE
                | if *append { open_flags::APPEND } else { open_flags::TRUNCATE };
            match self.fs.client().map(|c| c.open_file(abs, flags)) {
                Some(Ok(Ok((ch, st)))) => files.push((target.text.clone(), ch, st)),
                Some(Ok(Err(e))) => {
                    fail(target, &e);
                    return None;
                }
                _ => {
                    fail(target, &"the file system is not responding");
                    return None;
                }
            }
        }
        let input = match &routes.input {
            Some(w) => {
                let abs = self.resolve(&w.text);
                match self.fs.stat(&abs) {
                    Ok(st) if st.is_dir => {
                        fail(w, &"is a folder");
                        return None;
                    }
                    Ok(_) => Some(abs),
                    Err(e) => {
                        fail(w, &e);
                        return None;
                    }
                }
            }
            None => None,
        };
        Some((files, input))
    }

    /// Runs the program at `path` for one command of a pipeline, its
    /// streams as `routes` say (to `files`, the files opened for it), and
    /// waits for it. Its input is the terminal, `piped` (the output of the
    /// command before) or the file at `stdin_path`. Returns its exit status
    /// and what it wrote for the next command, or the status of a failure
    /// to start it.
    #[allow(clippy::too_many_arguments)]
    fn run_program(
        &mut self,
        path: &str,
        args: &[String],
        routes: &Routes,
        files: Vec<(String, Channel, Stat)>,
        piped: Option<Vec<u8>>,
        stdin_path: Option<String>,
        out: &Output,
    ) -> Result<(i32, Option<Vec<u8>>), i32> {
        let Some(console) = self.console.clone() else {
            out.styled("vsh: programs cannot run here (no terminal)\n", Style::ERROR);
            return Err(1);
        };
        let stdin = match (stdin_path, piped) {
            (Some(p), _) => match self.fs.client().map(|c| c.open_file(p.clone(), open_flags::READ)) {
                Some(Ok(Ok((ch, _)))) => Input::File(ch),
                Some(Ok(Err(e))) => {
                    out.styled(&format!("vsh: {}: {e}\n", display_path(&p)), Style::ERROR);
                    return Err(1);
                }
                _ => {
                    out.styled("vsh: the file system is not responding\n", Style::ERROR);
                    return Err(1);
                }
            },
            (None, Some(text)) => Input::Text(text),
            (None, None) => Input::Terminal,
        };
        // Each file the program writes to, a connection of its own for
        // each stream that goes there (sharing its offset, as a duplicated
        // descriptor does).
        let mut channels: Vec<Option<Channel>> = files.into_iter().map(|(_, ch, _)| Some(ch)).collect();
        let mut destination = |sink: Sink| -> Result<Destination, i32> {
            Ok(match sink {
                Sink::Terminal => Destination::Terminal,
                Sink::Pipe => Destination::Capture,
                Sink::File(i) => match channels.get_mut(i).and_then(Option::take) {
                    Some(ch) => {
                        let client = file::Client::new(ch);
                        let dup = client.duplicate();
                        channels[i] = Some(client.into_channel());
                        match dup {
                            Ok(Ok(ch)) => Destination::File(ch),
                            _ => {
                                out.styled("vsh: the file system is not responding\n", Style::ERROR);
                                return Err(1);
                            }
                        }
                    }
                    None => Destination::Terminal,
                },
            })
        };
        let stdout = destination(routes.out)?;
        let stderr = destination(routes.err)?;
        let mut env: Vec<String> = self.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
        env.retain(|e| !e.starts_with("PWD="));
        env.push(format!("PWD={}", self.cwd));
        let run = Run { path, args, env, cwd: &self.cwd, stdin, stdout, stderr };
        match program::run(&console, run, out, &self.interrupt) {
            Ok(ended) => {
                if let Some(reason) = ended.reason {
                    out.finish_line();
                    out.styled(&format!("{reason}\n"), Style::ERROR);
                }
                if ended.captured.cut() {
                    pipe_cut(out, &args[0]);
                }
                let capturing = routes.out == Sink::Pipe || routes.err == Sink::Pipe;
                Ok((ended.status, capturing.then(|| ended.captured.into_bytes())))
            }
            Err(msg) => {
                out.finish_line();
                out.styled(&format!("vsh: {msg}\n"), Style::ERROR);
                Err(126)
            }
        }
    }

    fn run_command(&mut self, args: &[String], io: &mut Io) -> i32 {
        let name = args[0].as_str();
        if let Some(b) = commands::find(name) {
            return (b.run)(self, io, args);
        }
        // Not a built-in: maybe an application or program.
        let rest: Vec<String> = args[1..].iter().map(|a| self.absolute_arg(a)).collect();
        match self.launch(name, rest) {
            Ok((app, koid)) => {
                io.styled(&format!("Started {app} (PID {koid})\n"), Style::DIM);
                0
            }
            Err(e) if e.ends_with("command not found") => {
                io.error("vsh", &format!("command not found: {name}"));
                io.hint("Type 'help' to see the available commands, or 'apps' for the installed applications.");
                127
            }
            Err(e) => io.error("vsh", &e),
        }
    }

    /// Turns an argument that names an existing file into an absolute path
    /// (programs do not share the terminal's working directory).
    pub fn absolute_arg(&self, a: &str) -> String {
        if a.starts_with('-') {
            return a.to_string();
        }
        let abs = self.resolve(a);
        if !a.starts_with('/') && self.fs.exists(&abs) { abs } else { a.to_string() }
    }

    /// Expands wildcards in words (patterns without matches stay as typed).
    fn expand(&self, words: &[Word]) -> Vec<String> {
        let mut out = Vec::new();
        for word in words {
            let w = &word.text;
            if !word.glob || !has_wildcards(w) {
                out.push(w.clone());
                continue;
            }
            let (dir_typed, pattern) = match w.rfind('/') {
                Some(i) => (&w[..=i], &w[i + 1..]),
                None => ("", w.as_str()),
            };
            if has_wildcards(dir_typed) {
                out.push(w.clone());
                continue;
            }
            let dir = if dir_typed.is_empty() { self.cwd.clone() } else { self.resolve(dir_typed) };
            let mut matches: Vec<String> = self
                .fs
                .read_dir(&dir)
                .unwrap_or_default()
                .into_iter()
                .filter(|e| (!e.name.starts_with('.') || pattern.starts_with('.')) && glob_match(pattern, &e.name))
                .map(|e| format!("{dir_typed}{}", e.name))
                .collect();
            if matches.is_empty() {
                out.push(w.clone());
            } else {
                matches.sort();
                out.extend(matches);
            }
        }
        out
    }

    // ---- completion --------------------------------------------------------

    /// Completes the word before byte offset `cursor` of `input`.
    pub fn complete(&mut self, input: &str, cursor: usize) -> Completion {
        let before = &input[..cursor];
        // Find the start of the current word (unquoted whitespace or operator).
        let mut start = 0;
        let mut quote: Option<char> = None;
        let mut escaped = false;
        for (i, c) in before.char_indices() {
            if escaped {
                escaped = false;
                continue;
            }
            match (quote, c) {
                (_, '\\') if quote != Some('\'') => escaped = true,
                (None, '\'' | '"') => quote = Some(c),
                (Some(q), c) if c == q => quote = None,
                (None, ' ' | '\t' | '|' | ';' | '&' | '<' | '>') => start = i + c.len_utf8(),
                _ => {}
            }
        }
        let typed = unescape(&before[start..]);
        let prefix_text = before[..start].trim_end();
        let command_position = prefix_text.is_empty() || prefix_text.ends_with(['|', ';', '&']);
        let mut out = Completion { start, ..Default::default() };

        if command_position && !typed.contains('/') && !typed.starts_with('.') && !typed.starts_with('~') {
            let mut names: Vec<String> = commands::names().into_iter().map(|s| s.to_string()).collect();
            names.extend(self.apps().into_iter().map(|a| a.id));
            names.extend(self.programs());
            names.sort();
            names.dedup();
            let matches: Vec<String> = names.into_iter().filter(|n| n.starts_with(typed.as_str())).collect();
            match matches.len() {
                0 => {}
                1 => out.replacement = Some(format!("{} ", syntax::escape(&matches[0]))),
                _ => {
                    let lcp = common_prefix(&matches);
                    if lcp.len() > typed.len() {
                        out.replacement = Some(syntax::escape(&lcp));
                    }
                }
            }
            out.candidates = matches;
            return out;
        }

        // Path completion.
        let (dir_typed, prefix) = match typed.rfind('/') {
            Some(i) => (typed[..=i].to_string(), typed[i + 1..].to_string()),
            None => (String::new(), typed.clone()),
        };
        let dir = if dir_typed.is_empty() { self.cwd.clone() } else { self.resolve(&dir_typed) };
        let entries = self.fs.read_dir(&dir).unwrap_or_default();
        let visible = |name: &str| !name.starts_with('.') || prefix.starts_with('.');
        let mut matches: Vec<(String, bool)> = entries
            .iter()
            .filter(|e| visible(&e.name) && e.name.starts_with(prefix.as_str()))
            .map(|e| (e.name.clone(), e.is_dir))
            .collect();
        if matches.is_empty() {
            // Fall back to a case-insensitive match.
            let lower = prefix.to_lowercase();
            matches = entries
                .iter()
                .filter(|e| visible(&e.name) && e.name.to_lowercase().starts_with(&lower))
                .map(|e| (e.name.clone(), e.is_dir))
                .collect();
        }
        out.candidates = matches.iter().map(|(n, d)| if *d { format!("{n}/") } else { n.clone() }).collect();
        match matches.len() {
            0 => {}
            1 => {
                let (name, is_dir) = &matches[0];
                let suffix = if *is_dir { "/" } else { " " };
                out.replacement = Some(format!("{}{}", syntax::escape(&format!("{dir_typed}{name}")), suffix));
            }
            _ => {
                let names: Vec<String> = matches.iter().map(|(n, _)| n.clone()).collect();
                let lcp = common_prefix(&names);
                if lcp.chars().count() > prefix.chars().count() || (lcp.len() == prefix.len() && lcp != prefix) {
                    out.replacement = Some(syntax::escape(&format!("{dir_typed}{lcp}")));
                }
            }
        }
        out
    }
}

/// Says that the output of the command `name` was cut because a pipe holds
/// at most [`PIPE_LIMIT`].
fn pipe_cut(out: &Output, name: &str) {
    out.finish_line();
    out.styled(
        &format!("vsh: {name}: the output was cut at {} MiB, the most a pipe holds\n", PIPE_LIMIT >> 20),
        Style::ERROR,
    );
}

fn launch_result(r: Result<Result<u64, proto::init::LaunchError>, ipc::IpcError>) -> Result<u64, String> {
    match r {
        Ok(Ok(koid)) => Ok(koid),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err("the launcher is not responding".to_string()),
    }
}

/// Removes quotes and backslash escapes from a partial word.
fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (None, '\'' | '"') => quote = Some(c),
            (q, '\\') if q != Some('\'') => {
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// Longest common prefix of several strings.
fn common_prefix(items: &[String]) -> String {
    let Some(first) = items.first() else { return String::new() };
    let mut len = first.len();
    for s in &items[1..] {
        len = len.min(s.len());
        for (i, (a, b)) in first.bytes().zip(s.bytes()).enumerate() {
            if a != b {
                len = len.min(i);
                break;
            }
        }
    }
    while !first.is_char_boundary(len) {
        len -= 1;
    }
    first[..len].into()
}
