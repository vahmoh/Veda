// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The shell's built-in commands.
//!
//! Every command has the signature `fn(&mut Shell, &mut Io, &[String]) ->
//! i32` (arguments include the command name; the result is the exit
//! status) and is listed in [`BUILTINS`] with its aliases and help text.
//! The commands for files and folders are here; those for programs and the
//! system are in [`system`], those of the shell itself in [`session`], and
//! the network's in `netcmds.rs`.
//!
//! Commands read their inputs a chunk at a time ([`Reader`]) and write
//! bytes exactly as they are, so that what goes into a file or a pipe is
//! the data itself; only what is shown on the terminal is limited (1 MiB
//! of a file) or refused (binary data).

mod session;
mod system;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use terminal_core::screen::{Style, color};
use terminal_core::text::{Counts, LastLines, LineSplitter, is_binary, trim_line_end};
use vproto::fs::{DirEntry, MAX_IO, file, open_flags};

use crate::shell::{Io, Shell, Stdin};
use vfiles::HOME;
use vfiles::format::{format_time, human_size};
use vfiles::kind::{EDITOR, FILES, FileKind, default_app, file_kind};
use vfiles::path::{SYSTEM, display_path, extension, file_name, glob_match, is_read_only, join};

/// A built-in command.
pub struct Builtin {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub group: Group,
    pub usage: &'static str,
    pub help: &'static str,
    pub run: fn(&mut Shell, &mut Io, &[String]) -> i32,
}

/// Sections of the `help` listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Files,
    System,
    Network,
    Shell,
}

/// Every built-in command.
pub static BUILTINS: &[Builtin] = &[
    // Files and folders.
    Builtin {
        name: "ls",
        aliases: &["dir"],
        group: Group::Files,
        usage: "ls [-a] [-w] [PATH...]",
        help: "List a folder (-a: include hidden files, -w: names only)",
        run: cmd_ls,
    },
    Builtin {
        name: "cd",
        aliases: &[],
        group: Group::Files,
        usage: "cd [DIR | - | ~]",
        help: "Change the current folder",
        run: cmd_cd,
    },
    Builtin {
        name: "pwd",
        aliases: &[],
        group: Group::Files,
        usage: "pwd",
        help: "Print the current folder",
        run: cmd_pwd,
    },
    Builtin {
        name: "cat",
        aliases: &["type"],
        group: Group::Files,
        usage: "cat FILE...",
        help: "Print the contents of files",
        run: cmd_cat,
    },
    Builtin {
        name: "head",
        aliases: &[],
        group: Group::Files,
        usage: "head [-n N] [FILE]",
        help: "Print the first lines of a file (default 10)",
        run: cmd_head,
    },
    Builtin {
        name: "tail",
        aliases: &[],
        group: Group::Files,
        usage: "tail [-n N] [FILE]",
        help: "Print the last lines of a file (default 10)",
        run: cmd_tail,
    },
    Builtin {
        name: "wc",
        aliases: &[],
        group: Group::Files,
        usage: "wc [FILE...]",
        help: "Count lines, words and bytes",
        run: cmd_wc,
    },
    Builtin {
        name: "grep",
        aliases: &[],
        group: Group::Files,
        usage: "grep [-i] [-n] [-v] [-c] [-r] [-H] TEXT [FILE...]",
        help: "Print lines that contain TEXT",
        run: cmd_grep,
    },
    Builtin {
        name: "find",
        aliases: &[],
        group: Group::Files,
        usage: "find [DIR] [-name PATTERN] [-type f|d]",
        help: "Search for files below a folder",
        run: cmd_find,
    },
    Builtin {
        name: "tree",
        aliases: &[],
        group: Group::Files,
        usage: "tree [-a] [DIR]",
        help: "Show a folder hierarchy",
        run: cmd_tree,
    },
    Builtin {
        name: "mkdir",
        aliases: &["md"],
        group: Group::Files,
        usage: "mkdir [-p] DIR...",
        help: "Create folders (-p: with parents)",
        run: cmd_mkdir,
    },
    Builtin {
        name: "rmdir",
        aliases: &["rd"],
        group: Group::Files,
        usage: "rmdir DIR...",
        help: "Remove empty folders",
        run: cmd_rmdir,
    },
    Builtin {
        name: "rm",
        aliases: &["del"],
        group: Group::Files,
        usage: "rm [-r] [-f] [-d] PATH...",
        help: "Delete files (-r: folders with their contents, -d: empty folders)",
        run: cmd_rm,
    },
    Builtin {
        name: "cp",
        aliases: &["copy"],
        group: Group::Files,
        usage: "cp [-r] [-v] SOURCE... DEST",
        help: "Copy files (-r: folders too, -v: say what is copied)",
        run: cmd_cp,
    },
    Builtin {
        name: "mv",
        aliases: &["move", "ren"],
        group: Group::Files,
        usage: "mv SOURCE... DEST",
        help: "Move or rename files and folders",
        run: cmd_mv,
    },
    Builtin {
        name: "touch",
        aliases: &[],
        group: Group::Files,
        usage: "touch FILE...",
        help: "Create empty files or update their time",
        run: cmd_touch,
    },
    Builtin {
        name: "stat",
        aliases: &[],
        group: Group::Files,
        usage: "stat PATH...",
        help: "Show details about files",
        run: cmd_stat,
    },
    Builtin {
        name: "edit",
        aliases: &["notepad"],
        group: Group::Files,
        usage: "edit [FILE]",
        help: "Open a file in the Text Editor",
        run: cmd_edit,
    },
    Builtin {
        name: "open",
        aliases: &["xdg-open"],
        group: Group::Files,
        usage: "open PATH...",
        help: "Open files or folders with their default app",
        run: cmd_open,
    },
    Builtin {
        name: "sync",
        aliases: &[],
        group: Group::Files,
        usage: "sync",
        help: "Write all changes to the disk now",
        run: cmd_sync,
    },
    Builtin {
        name: "df",
        aliases: &[],
        group: Group::Files,
        usage: "df [PATH...]",
        help: "Show how much space is used and free (home, /tmp and the system)",
        run: cmd_df,
    },
    // Processes and system information.
    Builtin {
        name: "ps",
        aliases: &["tasklist"],
        group: Group::System,
        usage: "ps [-a]",
        help: "List running processes",
        run: system::cmd_ps,
    },
    Builtin {
        name: "kill",
        aliases: &["taskkill"],
        group: Group::System,
        usage: "kill [-f] PID|NAME...",
        help: "End processes",
        run: system::cmd_kill,
    },
    Builtin {
        name: "run",
        aliases: &["start"],
        group: Group::System,
        usage: "run PROGRAM [ARGS...]",
        help: "Start a program or an installed app",
        run: system::cmd_run,
    },
    Builtin {
        name: "apps",
        aliases: &[],
        group: Group::System,
        usage: "apps",
        help: "List the installed applications",
        run: system::cmd_apps,
    },
    // Network.
    Builtin {
        name: "wifi",
        aliases: &[],
        group: Group::Network,
        usage: "wifi [status|scan|list|connect SSID [PASSWORD]|disconnect|saved|forget SSID|on|off|aps|log]",
        help: "Wi-Fi: show the connection, find and join networks",
        run: crate::netcmds::cmd_wifi,
    },
    Builtin {
        name: "ifconfig",
        aliases: &["ip", "ipconfig"],
        group: Group::Network,
        usage: "ifconfig [-a]",
        help: "Show network interfaces and their addresses (-a: with loopback)",
        run: crate::netcmds::cmd_ifconfig,
    },
    Builtin {
        name: "ping",
        aliases: &[],
        group: Group::Network,
        usage: "ping HOST [-c COUNT]",
        help: "Check that a host answers (ICMP echo)",
        run: crate::netcmds::cmd_ping,
    },
    Builtin {
        name: "nslookup",
        aliases: &["host"],
        group: Group::Network,
        usage: "nslookup NAME",
        help: "Look up the addresses of a name (DNS)",
        run: crate::netcmds::cmd_nslookup,
    },
    Builtin {
        name: "netstat",
        aliases: &[],
        group: Group::Network,
        usage: "netstat",
        help: "List open network connections",
        run: crate::netcmds::cmd_netstat,
    },
    Builtin {
        name: "route",
        aliases: &[],
        group: Group::Network,
        usage: "route",
        help: "Show the routing table",
        run: crate::netcmds::cmd_route,
    },
    Builtin {
        name: "curl",
        aliases: &["wget"],
        group: Group::Network,
        usage: "curl [-I] URL",
        help: "Fetch a web page over HTTP or HTTPS (-I: headers only)",
        run: crate::netcmds::cmd_curl,
    },
    Builtin {
        name: "uptime",
        aliases: &[],
        group: Group::System,
        usage: "uptime",
        help: "Show how long the system has been running",
        run: system::cmd_uptime,
    },
    Builtin {
        name: "free",
        aliases: &["mem"],
        group: Group::System,
        usage: "free",
        help: "Show memory usage",
        run: system::cmd_free,
    },
    Builtin {
        name: "sysinfo",
        aliases: &["neofetch", "ver"],
        group: Group::System,
        usage: "sysinfo",
        help: "Show system information",
        run: system::cmd_sysinfo,
    },
    Builtin {
        name: "date",
        aliases: &["time"],
        group: Group::System,
        usage: "date",
        help: "Show the date and time",
        run: system::cmd_date,
    },
    Builtin {
        name: "dmesg",
        aliases: &["log"],
        group: Group::System,
        usage: "dmesg [-n N | -a] [TEXT]",
        help: "Show the system log (last 100 lines, optionally filtered)",
        run: system::cmd_dmesg,
    },
    Builtin {
        name: "logger",
        aliases: &[],
        group: Group::System,
        usage: "logger [MESSAGE...]",
        help: "Write a message (or the piped input) to the system log",
        run: system::cmd_logger,
    },
    Builtin {
        name: "whoami",
        aliases: &[],
        group: Group::System,
        usage: "whoami",
        help: "Print the user name",
        run: system::cmd_whoami,
    },
    Builtin {
        name: "hostname",
        aliases: &[],
        group: Group::System,
        usage: "hostname",
        help: "Print the computer name",
        run: system::cmd_hostname,
    },
    // The shell itself.
    Builtin {
        name: "help",
        aliases: &["?"],
        group: Group::Shell,
        usage: "help [COMMAND]",
        help: "Show this help, or details about a command",
        run: session::cmd_help,
    },
    Builtin {
        name: "echo",
        aliases: &[],
        group: Group::Shell,
        usage: "echo [-n] [-e] TEXT...",
        help: "Print text (-e: interpret \\n, \\t and \\e)",
        run: session::cmd_echo,
    },
    Builtin {
        name: "clear",
        aliases: &["cls"],
        group: Group::Shell,
        usage: "clear",
        help: "Clear the screen (Ctrl+L)",
        run: session::cmd_clear,
    },
    Builtin {
        name: "sleep",
        aliases: &[],
        group: Group::Shell,
        usage: "sleep SECONDS",
        help: "Wait a while (Ctrl+C stops it)",
        run: session::cmd_sleep,
    },
    Builtin {
        name: "history",
        aliases: &[],
        group: Group::Shell,
        usage: "history [-c]",
        help: "Show (or clear) the command history",
        run: session::cmd_history,
    },
    Builtin {
        name: "env",
        aliases: &[],
        group: Group::Shell,
        usage: "env",
        help: "List environment variables",
        run: session::cmd_env,
    },
    Builtin {
        name: "export",
        aliases: &["set"],
        group: Group::Shell,
        usage: "export NAME=VALUE...",
        help: "Set environment variables",
        run: session::cmd_export,
    },
    Builtin {
        name: "unset",
        aliases: &[],
        group: Group::Shell,
        usage: "unset NAME...",
        help: "Remove environment variables",
        run: session::cmd_unset,
    },
    Builtin {
        name: "which",
        aliases: &["where"],
        group: Group::Shell,
        usage: "which NAME...",
        help: "Show what a command name refers to",
        run: session::cmd_which,
    },
    Builtin {
        name: "exit",
        aliases: &["quit"],
        group: Group::Shell,
        usage: "exit",
        help: "Close the terminal",
        run: session::cmd_exit,
    },
];

/// Looks up a built-in by name or alias.
pub fn find(name: &str) -> Option<&'static Builtin> {
    BUILTINS.iter().find(|b| b.name == name || b.aliases.contains(&name))
}

/// All command names and aliases (for completion).
pub fn names() -> Vec<&'static str> {
    let mut v = Vec::new();
    for b in BUILTINS {
        v.push(b.name);
        v.extend_from_slice(b.aliases);
    }
    v
}

// ---- helpers ---------------------------------------------------------------

// ---- helpers ---------------------------------------------------------------

/// Splits `-abc` options from operands; `allowed` lists valid letters.
pub(crate) fn options(args: &[String], allowed: &str) -> Result<(Vec<char>, Vec<String>), String> {
    let mut opts = Vec::new();
    let mut rest = Vec::new();
    let mut operands_only = false;
    for a in &args[1..] {
        if !operands_only && a == "--" {
            operands_only = true;
        } else if !operands_only && a.len() > 1 && a.starts_with('-') {
            for c in a[1..].chars() {
                if !allowed.contains(c) {
                    return Err(format!("unknown option '-{c}'"));
                }
                opts.push(c);
            }
        } else {
            rest.push(a.clone());
        }
    }
    Ok((opts, rest))
}

/// Parses `-n N`, `-nN` or `-N` line counts; returns the count and the rest.
pub(crate) fn line_count(args: &[String], default: usize) -> Result<(usize, Vec<String>), String> {
    let mut n = default;
    let mut rest = Vec::new();
    let mut i = 1;
    while i < args.len() {
        let a = &args[i];
        if a == "-n" {
            let v = args.get(i + 1).ok_or("option -n needs a number")?;
            n = v.parse().map_err(|_| format!("'{v}' is not a number"))?;
            i += 1;
        } else if let Some(v) = a.strip_prefix("-n").filter(|v| !v.is_empty()) {
            n = v.parse().map_err(|_| format!("'{v}' is not a number"))?;
        } else if let Some(v) = a.strip_prefix('-').filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit())) {
            n = v.parse().unwrap_or(default);
        } else if a == "-a" {
            n = usize::MAX;
        } else {
            rest.push(a.clone());
        }
        i += 1;
    }
    Ok((n, rest))
}

/// Prints a usage error.
pub(crate) fn usage(io: &mut Io, cmd: &str) -> i32 {
    let u = find(cmd).map(|b| b.usage).unwrap_or(cmd);
    io.error(cmd, &format!("usage: {u}"))
}

/// The colour of a directory entry's name (`program` as for
/// [`vfiles::kind::file_kind`]).
fn entry_style(name: &str, is_dir: bool, program: bool) -> Style {
    if is_dir {
        return Style::fg(color::BRIGHT_BLUE).bold();
    }
    if name.starts_with('.') {
        return Style::DIM;
    }
    match file_kind(name, program) {
        FileKind::Program => Style::fg(color::BRIGHT_GREEN).bold(),
        // As GNU dircolors has them: pictures and videos alike.
        FileKind::Image | FileKind::Video => Style::fg(color::BRIGHT_MAGENTA),
        FileKind::Audio => Style::fg(color::BRIGHT_CYAN),
        FileKind::Text | FileKind::Other => Style::PLAIN,
    }
}

/// An input of a command: a file named on its command line (as named), or
/// its standard input.
enum Source {
    File(String),
    Stdin,
}

/// The inputs of a command: its file arguments or, without any, its
/// standard input. `None`, the usage shown, if it has neither.
fn sources(io: &mut Io, cmd: &str, files: &[String]) -> Option<Vec<Source>> {
    if !files.is_empty() {
        return Some(files.iter().map(|f| Source::File(f.clone())).collect());
    }
    if io.has_stdin() {
        return Some(vec![Source::Stdin]);
    }
    usage(io, cmd);
    None
}

/// What a command reads, a chunk at a time: an open file, or bytes.
pub(crate) struct Reader {
    /// How it is called in messages.
    pub name: String,
    /// Its size, if known.
    pub size: u64,
    file: Option<file::Client>,
    bytes: Option<Vec<u8>>,
}

impl Reader {
    /// Opens `src`. `None`, the error shown, if it cannot be read: missing,
    /// a folder, or a file this command writes to (which it emptied).
    fn open(sh: &Shell, io: &mut Io, cmd: &str, src: Source) -> Option<Reader> {
        let (name, path) = match src {
            Source::File(f) => {
                let path = sh.resolve(&f);
                (f, path)
            }
            Source::Stdin => match io.take_stdin() {
                Stdin::Bytes(bytes) => {
                    let size = bytes.len() as u64;
                    return Some(Reader { name: "(standard input)".into(), size, file: None, bytes: Some(bytes) });
                }
                Stdin::File(path) => (display_path(&path), path),
                Stdin::None => (String::from("(standard input)"), String::new()),
            },
        };
        if path.is_empty() {
            return Some(Reader { name, size: 0, file: None, bytes: None });
        }
        let st = match sh.fs.stat(&path) {
            Ok(st) => st,
            Err(e) => {
                io.error(cmd, &format!("{name}: {e}"));
                return None;
            }
        };
        if st.is_dir {
            io.error(cmd, &format!("{name}: is a folder"));
            return None;
        }
        if io.writes_to(&st) {
            io.error(cmd, &format!("{name}: input file is output file"));
            return None;
        }
        match sh.fs.client().map(|c| c.open_file(path, open_flags::READ)) {
            Some(Ok(Ok((ch, st)))) => {
                Some(Reader { name, size: st.size, file: Some(file::Client::new(ch)), bytes: None })
            }
            Some(Ok(Err(e))) => {
                io.error(cmd, &format!("{name}: {e}"));
                None
            }
            _ => {
                io.error(cmd, &format!("{name}: the file system is not responding"));
                None
            }
        }
    }

    /// The next chunk; `None` at the end.
    fn next(&mut self) -> Result<Option<Vec<u8>>, String> {
        let Some(f) = &self.file else { return Ok(self.bytes.take()) };
        match f.read(MAX_IO) {
            Ok(Ok(b)) if b.0.is_empty() => Ok(None),
            Ok(Ok(b)) => Ok(Some(b.0)),
            Ok(Err(e)) => Err(e.to_string()),
            Err(_) => Err("the file system is not responding".into()),
        }
    }
}

/// Reads `r` a chunk at a time into `f` until `f` returns `false`, the
/// command is interrupted or the input ends. Returns `false`, the error
/// shown, if reading failed.
fn read_all(sh: &Shell, io: &mut Io, cmd: &str, r: &mut Reader, mut f: impl FnMut(&mut Io, &[u8]) -> bool) -> bool {
    while !sh.interrupted() {
        match r.next() {
            Ok(Some(chunk)) => {
                if !f(io, &chunk) {
                    break;
                }
            }
            Ok(None) => break,
            Err(e) => {
                io.error(cmd, &format!("{}: {e}", r.name));
                return false;
            }
        }
    }
    true
}

/// At most `limit` bytes of the command's standard input. `None`, the
/// usage or error shown, if it has none or it cannot be read.
pub(crate) fn read_stdin(sh: &Shell, io: &mut Io, cmd: &str, limit: usize) -> Option<Vec<u8>> {
    let src = sources(io, cmd, &[])?.pop()?;
    let mut r = Reader::open(sh, io, cmd, src)?;
    let mut data = Vec::new();
    let ok = read_all(sh, io, cmd, &mut r, |_, chunk| {
        let n = chunk.len().min(limit - data.len());
        data.extend_from_slice(&chunk[..n]);
        data.len() < limit
    });
    ok.then_some(data)
}

/// The first place of `needle` in `hay` at or after `from`.
fn find_bytes(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() {
        return Some(from.min(hay.len()));
    }
    hay.get(from..)?.windows(needle.len()).position(|w| w == needle).map(|p| p + from)
}

/// Pads `s` with spaces to `w` characters.
pub(crate) fn pad(s: &str, w: usize) -> String {
    let n = s.chars().count();
    let mut out = String::from(s);
    for _ in n..w {
        out.push(' ');
    }
    out
}

/// Right-aligns `s` in `w` characters.
fn rpad(s: &str, w: usize) -> String {
    let n = s.chars().count();
    let mut out = String::new();
    for _ in n..w {
        out.push(' ');
    }
    out.push_str(s);
    out
}

// ---- files -----------------------------------------------------------------

fn print_entry_long(io: &mut Io, e: &DirEntry) {
    io.styled(&format_time(e.modified), Style::DIM);
    io.print("  ");
    if e.is_dir {
        let items = if e.size == 1 { "1 item".to_string() } else { format!("{} items", e.size) };
        io.styled(&rpad(&items, 10), Style::DIM);
    } else {
        io.print(&rpad(&human_size(e.size), 10));
    }
    io.print("  ");
    io.styled(&e.name, entry_style(&e.name, e.is_dir, e.executable));
    if e.is_dir {
        io.styled("/", entry_style(&e.name, true, false));
    }
    io.print("\n");
}

fn print_wide(sh: &Shell, io: &mut Io, entries: &[DirEntry]) {
    if entries.is_empty() {
        return;
    }
    let names: Vec<String> =
        entries.iter().map(|e| if e.is_dir { format!("{}/", e.name) } else { e.name.clone() }).collect();
    let colw = names.iter().map(|n| n.chars().count()).max().unwrap_or(1) + 2;
    let per_row = (sh.cols.max(colw) / colw).max(1);
    let rows = names.len().div_ceil(per_row);
    for r in 0..rows {
        for c in 0..per_row {
            let i = c * rows + r;
            let Some(n) = names.get(i) else { continue };
            let e = &entries[i];
            io.styled(n, entry_style(&e.name, e.is_dir, e.executable));
            if (c + 1) * rows + r < names.len() {
                io.print(&pad("", colw - n.chars().count()));
            }
        }
        io.print("\n");
    }
}

fn cmd_ls(sh: &mut Shell, io: &mut Io, args: &[String]) -> i32 {
    let (opts, paths) = match options(args, "alw1h") {
        Ok(v) => v,
        Err(e) => return io.error(&args[0], &e),
    };
    let all = opts.contains(&'a');
    let wide = opts.contains(&'w');
    let targets: Vec<String> = if paths.is_empty() { vec![".".to_string()] } else { paths };
    let multi = targets.len() > 1;
    let mut status = 0;
    for (i, arg) in targets.iter().enumerate() {
        let path = sh.resolve(arg);
        let st = match sh.fs.stat(&path) {
            Ok(s) => s,
            Err(e) => {
                status = io.error(&args[0], &format!("{arg}: {e}"));
                continue;
            }
        };
        if !st.is_dir {
            let e = DirEntry {
                name: arg.clone(),
                is_dir: false,
                size: st.size,
                modified: st.modified,
                inode: st.inode,
                executable: st.executable,
            };
            print_entry_long(io, &e);
            continue;
        }
        let entries = match sh.fs.read_dir(&path) {
            Ok(v) => v,
            Err(e) => {
                status = io.error(&args[0], &format!("{arg}: {e}"));
                continue;
            }
        };
        let entries: Vec<DirEntry> = entries.into_iter().filter(|e| all || !e.name.starts_with('.')).collect();
        if multi {
            if i > 0 {
                io.print("\n");
            }
            io.styled(&format!("{}:\n", display_path(&path)), Style::BOLD);
        }
        if entries.is_empty() {
            io.styled("(empty folder)\n", Style::DIM);
            continue;
        }
        if wide {
            print_wide(sh, io, &entries);
            continue;
        }
        let (mut dirs, mut files, mut total) = (0, 0, 0u64);
        for e in &entries {
            print_entry_long(io, e);
            if e.is_dir {
                dirs += 1;
            } else {
                files += 1;
                total += e.size;
            }
        }
        let plural = |n: usize, s: &str| if n == 1 { format!("1 {s}") } else { format!("{n} {s}s") };
        io.styled(
            &format!(
                "{}, {}, {}{}\n",
                plural(dirs, "folder"),
                plural(files, "file"),
                human_size(total),
                if is_read_only(&path) { " (read-only)" } else { "" }
            ),
            Style::DIM,
        );
    }
    status
}

fn cmd_cd(sh: &mut Shell, io: &mut Io, args: &[String]) -> i32 {
    let target = match args.get(1).map(String::as_str) {
        None => HOME.to_string(),
        Some("-") => match &sh.prev_dir {
            Some(p) => {
                let p = p.clone();
                io.println(&display_path(&p));
                p
            }
            None => return io.error("cd", "no previous folder"),
        },
        Some(p) => sh.resolve(p),
    };
    if args.len() > 2 {
        return io.error("cd", "too many arguments");
    }
    let shown = args.get(1).map(String::as_str).unwrap_or("~");
    match sh.fs.stat(&target) {
        Ok(st) if st.is_dir => {
            sh.set_cwd(target);
            0
        }
        Ok(_) => io.error("cd", &format!("{shown}: not a folder")),
        Err(e) => io.error("cd", &format!("{shown}: {e}")),
    }
}

fn cmd_pwd(sh: &mut Shell, io: &mut Io, _args: &[String]) -> i32 {
    io.println(&sh.cwd);
    0
}

fn cmd_cat(sh: &mut Shell, io: &mut Io, args: &[String]) -> i32 {
    // Shown on the terminal: at most this much of each input.
    const SHOWN: usize = 1 << 20;
    let Some(inputs) = sources(io, &args[0], &args[1..]) else { return 1 };
    let terminal = io.is_terminal();
    let mut status = 0;
    for src in inputs {
        if sh.interrupted() || io.stopped() {
            break;
        }
        let Some(mut r) = Reader::open(sh, io, &args[0], src) else {
            status = 1;
            continue;
        };
        // Into a file or a pipe: the bytes, all of them.
        if !terminal {
            if !read_all(sh, io, &args[0], &mut r, |io, chunk| {
                io.write(chunk);
                !io.stopped()
            }) {
                status = 1;
            }
            continue;
        }
        // On the terminal: text, up to its limit.
        let (mut shown, mut first, mut binary, mut cut, mut last) = (0, true, false, false, b'\n');
        let ok = read_all(sh, io, &args[0], &mut r, |io, chunk| {
            if first && is_binary(chunk) {
                binary = true;
                return false;
            }
            first = false;
            let n = chunk.len().min(SHOWN - shown);
            io.write(&chunk[..n]);
            shown += n;
            if n > 0 {
                last = chunk[n - 1];
            }
            cut = n < chunk.len();
            !cut
        });
        if binary {
            status = io.error(&args[0], &format!("{}: binary file ({})", r.name, human_size(r.size)));
            continue;
        }
        if !ok {
            status = 1;
        }
        // A file that does not end its last line still ends it here.
        if last != b'\n' {
            io.print("\n");
        }
        if cut {
            io.styled("… (output truncated at 1 MiB)\n", Style::DIM);
        }
    }
    status
}

fn cmd_head(sh: &mut Shell, io: &mut Io, args: &[String]) -> i32 {
    head_tail(sh, io, args, true)
}

fn cmd_tail(sh: &mut Shell, io: &mut Io, args: &[String]) -> i32 {
    head_tail(sh, io, args, false)
}

/// The first (`head`) or last `n` lines of each input; the first are taken
/// without reading on.
fn head_tail(sh: &mut Shell, io: &mut Io, args: &[String], head: bool) -> i32 {
    let (n, files) = match line_count(args, 10) {
        Ok(v) => v,
        Err(e) => return io.error(&args[0], &e),
    };
    let Some(inputs) = sources(io, &args[0], &files) else { return 1 };
    let multi = inputs.len() > 1;
    let terminal = io.is_terminal();
    let mut status = 0;
    for (i, src) in inputs.into_iter().enumerate() {
        if sh.interrupted() || io.stopped() {
            break;
        }
        let Some(mut r) = Reader::open(sh, io, &args[0], src) else {
            status = 1;
            continue;
        };
        if multi {
            io.styled(&format!("{}==> {} <==\n", if i > 0 { "\n" } else { "" }, r.name), Style::BOLD);
        }
        let mut lines = LineSplitter::new();
        let mut last = LastLines::new(if head { 0 } else { n });
        let (mut taken, mut first, mut binary) = (0, true, false);
        let mut take = |io: &mut Io, line: &[u8]| {
            if !head {
                last.push(line);
                return true;
            }
            if taken < n {
                io.write(line);
                taken += 1;
            }
            taken < n
        };
        let ok = read_all(sh, io, &args[0], &mut r, |io, chunk| {
            if first && terminal && is_binary(chunk) {
                binary = true;
                return false;
            }
            first = false;
            lines.feed(chunk, &mut |line| take(io, line))
        });
        if binary {
            status = io.error(&args[0], &format!("{}: binary file ({})", r.name, human_size(r.size)));
            continue;
        }
        if !ok {
            status = 1;
        }
        lines.finish(&mut |line| take(io, line));
        for line in last.lines() {
            io.write(line);
        }
    }
    status
}

fn cmd_wc(sh: &mut Shell, io: &mut Io, args: &[String]) -> i32 {
    let Some(inputs) = sources(io, &args[0], &args[1..]) else { return 1 };
    let mut total = (0, 0, 0);
    let mut counted = 0;
    let mut status = 0;
    for src in inputs {
        if sh.interrupted() {
            break;
        }
        let shown = match &src {
            Source::File(f) => f.clone(),
            Source::Stdin => String::new(),
        };
        let Some(mut r) = Reader::open(sh, io, &args[0], src) else {
            status = 1;
            continue;
        };
        let mut c = Counts::default();
        if !read_all(sh, io, &args[0], &mut r, |_, chunk| {
            c.add(chunk);
            true
        }) {
            status = 1;
            continue;
        }
        let (l, w, b) = c.total();
        total = (total.0 + l, total.1 + w, total.2 + b);
        counted += 1;
        io.println(&format!("{l:>7} {w:>7} {b:>9} {shown}"));
    }
    if counted > 1 {
        io.println(&format!("{:>7} {:>7} {:>9} total", total.0, total.1, total.2));
    }
    status
}

/// Collects the files below `dir` (for `grep -r`).
fn walk_files(sh: &Shell, dir: &str, out: &mut Vec<String>) {
    for e in sh.fs.read_dir(dir).unwrap_or_default() {
        if sh.interrupted() {
            return;
        }
        let p = join(dir, &e.name);
        if e.is_dir {
            walk_files(sh, &p, out);
        } else {
            out.push(p);
        }
    }
}

fn cmd_grep(sh: &mut Shell, io: &mut Io, args: &[String]) -> i32 {
    let (opts, rest) = match options(args, "invcrH") {
        Ok(v) => v,
        Err(e) => return io.error("grep", &e),
    };
    let Some(pattern) = rest.first() else { return usage(io, "grep") };
    let (ci, numbers, invert, count, recursive) =
        (opts.contains(&'i'), opts.contains(&'n'), opts.contains(&'v'), opts.contains(&'c'), opts.contains(&'r'));
    // Files found below a folder, and those named.
    let mut files: Vec<(String, bool)> = Vec::new();
    for f in &rest[1..] {
        let abs = sh.resolve(f);
        if recursive && sh.fs.is_dir(&abs) {
            let mut found = Vec::new();
            walk_files(sh, &abs, &mut found);
            let prefix = abs.len();
            files.extend(found.into_iter().map(|p| (format!("{}{}", f.trim_end_matches('/'), &p[prefix..]), true)));
        } else {
            files.push((f.clone(), false));
        }
    }
    if recursive && rest.len() == 1 {
        let mut found = Vec::new();
        walk_files(sh, &sh.cwd.clone(), &mut found);
        let prefix = sh.cwd.len() + if sh.cwd == "/" { 0 } else { 1 };
        files.extend(found.into_iter().map(|p| (p[prefix..].to_string(), true)));
    }
    let named: Vec<String> = files.iter().map(|(f, _)| f.clone()).collect();
    // Searching folders that hold no files finds nothing (it is no reading
    // of the standard input).
    let inputs = if recursive && named.is_empty() {
        Vec::new()
    } else {
        match sources(io, "grep", &named) {
            Some(inputs) => inputs,
            None => return 2,
        }
    };
    let names = inputs.len() > 1 || recursive || opts.contains(&'H');
    let needle: Vec<u8> = if ci { pattern.to_ascii_lowercase().into_bytes() } else { pattern.clone().into_bytes() };
    let terminal = io.is_terminal();
    let mut any = false;
    for (i, src) in inputs.into_iter().enumerate() {
        if sh.interrupted() || io.stopped() {
            break;
        }
        // Files found by `-r` that are binary are passed over in silence.
        let walked = files.get(i).is_some_and(|f| f.1);
        let Some(mut r) = Reader::open(sh, io, "grep", src) else { continue };
        let name = r.name.clone();
        let mut lines = LineSplitter::new();
        let (mut ln, mut n, mut first, mut binary) = (0, 0, true, false);
        let mut line_of = |io: &mut Io, line: &[u8]| {
            ln += 1;
            let text = trim_line_end(line);
            let lower;
            let hay = if ci {
                lower = text.to_ascii_lowercase();
                &lower[..]
            } else {
                text
            };
            if find_bytes(hay, &needle, 0).is_some() == invert {
                return true;
            }
            n += 1;
            if count {
                return true;
            }
            if names {
                io.styled(&name, Style::fg(color::MAGENTA));
                io.styled(":", Style::DIM);
            }
            if numbers {
                io.styled(&format!("{ln}"), Style::fg(color::GREEN));
                io.styled(":", Style::DIM);
            }
            if invert || needle.is_empty() || !terminal {
                io.write(text);
            } else {
                // The matches stand out.
                let mut at = 0;
                while let Some(pos) = find_bytes(hay, &needle, at) {
                    io.print(&String::from_utf8_lossy(&text[at..pos]));
                    let end = pos + needle.len();
                    io.styled(&String::from_utf8_lossy(&text[pos..end]), Style::fg(color::BRIGHT_RED).bold());
                    at = end;
                }
                io.print(&String::from_utf8_lossy(&text[at..]));
            }
            io.print("\n");
            !io.stopped()
        };
        read_all(sh, io, "grep", &mut r, |io, chunk| {
            if first && is_binary(chunk) {
                binary = true;
                return false;
            }
            first = false;
            lines.feed(chunk, &mut |line| line_of(io, line))
        });
        if binary {
            if !walked {
                io.error("grep", &format!("{name}: binary file ({})", human_size(r.size)));
            }
            continue;
        }
        lines.finish(&mut |line| line_of(io, line));
        any |= n > 0;
        if count {
            if names {
                io.styled(&format!("{name}:"), Style::fg(color::MAGENTA));
            }
            io.println(&n.to_string());
        }
    }
    if any { 0 } else { 1 }
}

fn cmd_find(sh: &mut Shell, io: &mut Io, args: &[String]) -> i32 {
    let mut start = ".".to_string();
    let mut name: Option<(String, bool)> = None;
    let mut kind: Option<bool> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-name" | "-iname" => {
                let Some(p) = args.get(i + 1) else { return usage(io, "find") };
                name = Some((p.clone(), args[i] == "-iname"));
                i += 1;
            }
            "-type" => match args.get(i + 1).map(String::as_str) {
                Some("d") => {
                    kind = Some(true);
                    i += 1;
                }
                Some("f") => {
                    kind = Some(false);
                    i += 1;
                }
                _ => return io.error("find", "-type expects 'f' or 'd'"),
            },
            a if a.starts_with('-') => return io.error("find", &format!("unknown option '{a}'")),
            a => start = a.to_string(),
        }
        i += 1;
    }
    let abs = sh.resolve(&start);
    if !sh.fs.is_dir(&abs) {
        return io.error("find", &format!("{start}: not a folder"));
    }
    let shown = start.trim_end_matches('/').to_string();
    let mut found = 0;
    find_rec(sh, io, &abs, if shown.is_empty() { "/" } else { &shown }, &name, kind, &mut found);
    if found == 0 && io.is_terminal() {
        io.styled("No matches.\n", Style::DIM);
    }
    0
}

fn find_rec(
    sh: &Shell,
    io: &mut Io,
    dir: &str,
    shown: &str,
    name: &Option<(String, bool)>,
    kind: Option<bool>,
    found: &mut usize,
) {
    for e in sh.fs.read_dir(dir).unwrap_or_default() {
        if sh.interrupted() {
            return;
        }
        let path = join(dir, &e.name);
        let display = join(shown, &e.name);
        let name_ok = match name {
            Some((p, true)) => glob_match(&p.to_lowercase(), &e.name.to_lowercase()),
            Some((p, false)) => glob_match(p, &e.name),
            None => true,
        };
        if name_ok && kind.is_none_or(|d| d == e.is_dir) {
            io.styled(&display, entry_style(&e.name, e.is_dir, e.executable));
            io.print("\n");
            *found += 1;
        }
        if e.is_dir {
            find_rec(sh, io, &path, &display, name, kind, found);
        }
    }
}

fn cmd_tree(sh: &mut Shell, io: &mut Io, args: &[String]) -> i32 {
    let (opts, rest) = match options(args, "a") {
        Ok(v) => v,
        Err(e) => return io.error("tree", &e),
    };
    let arg = rest.first().cloned().unwrap_or_else(|| ".".into());
    let abs = sh.resolve(&arg);
    if !sh.fs.is_dir(&abs) {
        return io.error("tree", &format!("{arg}: not a folder"));
    }
    io.styled(&display_path(&abs), Style::fg(color::BRIGHT_BLUE).bold());
    io.print("\n");
    let mut counts = (0usize, 0usize);
    tree_rec(sh, io, &abs, "", opts.contains(&'a'), 0, &mut counts);
    let plural = |n: usize, s: &str| if n == 1 { format!("1 {s}") } else { format!("{n} {s}s") };
    io.styled(&format!("\n{}, {}\n", plural(counts.0, "folder"), plural(counts.1, "file")), Style::DIM);
    0
}

fn tree_rec(sh: &Shell, io: &mut Io, dir: &str, prefix: &str, all: bool, depth: usize, counts: &mut (usize, usize)) {
    let entries: Vec<DirEntry> =
        sh.fs.read_dir(dir).unwrap_or_default().into_iter().filter(|e| all || !e.name.starts_with('.')).collect();
    for (i, e) in entries.iter().enumerate() {
        if sh.interrupted() {
            return;
        }
        let last = i + 1 == entries.len();
        io.styled(prefix, Style::DIM);
        io.styled(if last { "└── " } else { "├── " }, Style::DIM);
        io.styled(&e.name, entry_style(&e.name, e.is_dir, e.executable));
        io.print("\n");
        if e.is_dir {
            counts.0 += 1;
            if depth < 12 {
                let p = format!("{prefix}{}", if last { "    " } else { "│   " });
                tree_rec(sh, io, &join(dir, &e.name), &p, all, depth + 1, counts);
            }
        } else {
            counts.1 += 1;
        }
    }
}

fn cmd_mkdir(sh: &mut Shell, io: &mut Io, args: &[String]) -> i32 {
    let (opts, dirs) = match options(args, "p") {
        Ok(v) => v,
        Err(e) => return io.error("mkdir", &e),
    };
    if dirs.is_empty() {
        return usage(io, "mkdir");
    }
    let mut status = 0;
    for d in &dirs {
        let abs = sh.resolve(d);
        let r = if opts.contains(&'p') { sh.fs.mkdir_all(&abs) } else { sh.fs.mkdir(&abs) };
        if let Err(e) = r {
            status = io.fs_error("mkdir", d, &e);
        }
    }
    status
}

fn cmd_rmdir(sh: &mut Shell, io: &mut Io, args: &[String]) -> i32 {
    if args.len() < 2 {
        return usage(io, "rmdir");
    }
    let mut status = 0;
    for d in &args[1..] {
        let abs = sh.resolve(d);
        match sh.fs.stat(&abs) {
            Ok(st) if !st.is_dir => status = io.error("rmdir", &format!("{d}: not a folder")),
            Ok(_) => {
                if let Err(e) = sh.fs.remove(&abs) {
                    status = io.error("rmdir", &format!("{d}: {e}"));
                }
            }
            Err(e) => status = io.error("rmdir", &format!("{d}: {e}")),
        }
    }
    status
}

/// Paths that must never be deleted or overwritten wholesale.
fn protected(path: &str) -> bool {
    matches!(path, "/" | "/home" | HOME | "/system" | "/tmp")
}

fn cmd_rm(sh: &mut Shell, io: &mut Io, args: &[String]) -> i32 {
    // Asking first (`-i`) is not offered: an option that did nothing would
    // delete what the user meant to be asked about.
    let (opts, paths) = match options(args, "rRfd") {
        Ok(v) => v,
        Err(e) => return io.error(&args[0], &e),
    };
    if paths.is_empty() {
        return usage(io, "rm");
    }
    let recursive = opts.contains(&'r') || opts.contains(&'R');
    let empty_folders = opts.contains(&'d');
    let force = opts.contains(&'f');
    let mut status = 0;
    for p in &paths {
        let abs = sh.resolve(p);
        if protected(&abs) {
            status = io.error(&args[0], &format!("refusing to remove '{}'", display_path(&abs)));
            continue;
        }
        let st = match sh.fs.stat(&abs) {
            Ok(s) => s,
            Err(e) => {
                if !force {
                    status = io.error(&args[0], &format!("{p}: {e}"));
                }
                continue;
            }
        };
        if st.is_dir && !recursive && !empty_folders {
            status = io.error(&args[0], &format!("{p}: is a folder (use 'rm -r' to delete it with its contents)"));
            continue;
        }
        // `-d` removes a folder only if it is empty.
        let r = if st.is_dir && recursive { sh.fs.remove_all(&abs) } else { sh.fs.remove(&abs) };
        if let Err(e) = r {
            status = io.error(&args[0], &format!("{p}: {e}"));
        }
    }
    status
}

fn cmd_cp(sh: &mut Shell, io: &mut Io, args: &[String]) -> i32 {
    // `-f` (replace without asking) is what cp always does.
    let (opts, paths) = match options(args, "rRfv") {
        Ok(v) => v,
        Err(e) => return io.error(&args[0], &e),
    };
    if paths.len() < 2 {
        return usage(io, "cp");
    }
    let recursive = opts.contains(&'r') || opts.contains(&'R');
    let verbose = opts.contains(&'v');
    let (sources, dest) = paths.split_at(paths.len() - 1);
    let dest_abs = sh.resolve(&dest[0]);
    let dest_is_dir = sh.fs.is_dir(&dest_abs);
    if sources.len() > 1 && !dest_is_dir {
        return io.error(&args[0], &format!("{}: not a folder", dest[0]));
    }
    let mut status = 0;
    for s in sources {
        let src = sh.resolve(s);
        let st = match sh.fs.stat(&src) {
            Ok(st) => st,
            Err(e) => {
                status = io.error(&args[0], &format!("{s}: {e}"));
                continue;
            }
        };
        if st.is_dir && !recursive {
            status = io.error(&args[0], &format!("{s}: is a folder (use 'cp -r' to copy it)"));
            continue;
        }
        let target = if dest_is_dir { join(&dest_abs, file_name(&src)) } else { dest_abs.clone() };
        if target == src {
            status = io.error(&args[0], &format!("{s}: source and destination are the same"));
            continue;
        }
        let r = match sh.fs.stat(&target) {
            Ok(t) if !t.is_dir && !st.is_dir => sh.fs.read(&src).and_then(|d| sh.fs.write(&target, &d)),
            _ => sh.fs.copy(&src, &target),
        };
        match r {
            Ok(()) if verbose => io.println(&format!("'{s}' -> '{}'", display_path(&target))),
            Ok(()) => {}
            Err(e) => status = io.fs_error(&args[0], s, &e),
        }
    }
    status
}

fn cmd_mv(sh: &mut Shell, io: &mut Io, args: &[String]) -> i32 {
    let (_, paths) = match options(args, "fv") {
        Ok(v) => v,
        Err(e) => return io.error(&args[0], &e),
    };
    if paths.len() < 2 {
        return usage(io, "mv");
    }
    let (sources, dest) = paths.split_at(paths.len() - 1);
    let dest_abs = sh.resolve(&dest[0]);
    let dest_is_dir = sh.fs.is_dir(&dest_abs);
    if sources.len() > 1 && !dest_is_dir {
        return io.error(&args[0], &format!("{}: not a folder", dest[0]));
    }
    let mut status = 0;
    for s in sources {
        let src = sh.resolve(s);
        if protected(&src) {
            status = io.error(&args[0], &format!("refusing to move '{}'", display_path(&src)));
            continue;
        }
        let st = match sh.fs.stat(&src) {
            Ok(st) => st,
            Err(e) => {
                status = io.error(&args[0], &format!("{s}: {e}"));
                continue;
            }
        };
        let target = if dest_is_dir { join(&dest_abs, file_name(&src)) } else { dest_abs.clone() };
        if target == src {
            continue;
        }
        // Like Unix mv, replace an existing file: in one step, so that if
        // the move fails, the file is still there.
        let moved = match sh.fs.stat(&target) {
            Ok(t) if !t.is_dir && !st.is_dir => sh.fs.replace(&src, &target),
            _ => sh.fs.move_to(&src, &target),
        };
        if let Err(e) = moved {
            status = io.fs_error(&args[0], s, &e);
        }
    }
    status
}

fn cmd_touch(sh: &mut Shell, io: &mut Io, args: &[String]) -> i32 {
    if args.len() < 2 {
        return usage(io, "touch");
    }
    let mut status = 0;
    for p in &args[1..] {
        let abs = sh.resolve(p);
        if sh.fs.is_dir(&abs) {
            continue;
        }
        if let Err(e) = sh.fs.touch(&abs) {
            status = io.fs_error("touch", p, &e);
        }
    }
    status
}

fn cmd_stat(sh: &mut Shell, io: &mut Io, args: &[String]) -> i32 {
    if args.len() < 2 {
        return usage(io, "stat");
    }
    let mut status = 0;
    for p in &args[1..] {
        let abs = sh.resolve(p);
        match sh.fs.stat(&abs) {
            Ok(st) => {
                let label = |io: &mut Io, l: &str| io.styled(&format!("{l:>9}: "), Style::DIM);
                label(io, "Name");
                io.styled(file_name(&abs), entry_style(file_name(&abs), st.is_dir, st.executable));
                io.print("\n");
                label(io, "Path");
                io.println(&abs);
                label(io, "Type");
                if st.is_dir {
                    io.println(&format!("folder ({} items)", st.size));
                } else {
                    let kind = match file_kind(&abs, st.executable) {
                        FileKind::Text => "text document",
                        FileKind::Image => "image",
                        FileKind::Audio => "audio",
                        FileKind::Video => "video",
                        FileKind::Program => "program",
                        FileKind::Other => "file",
                    };
                    io.println(kind);
                    label(io, "Size");
                    io.println(&format!("{} ({} bytes)", human_size(st.size), st.size));
                }
                label(io, "Modified");
                io.println(&format_time(st.modified));
                label(io, "Access");
                io.println(if st.read_only { "read-only" } else { "read and write" });
                if let Some(app) = default_app(&abs).filter(|_| !st.is_dir) {
                    label(io, "Opens in");
                    io.println(app.id);
                }
            }
            Err(e) => status = io.error("stat", &format!("{p}: {e}")),
        }
    }
    status
}

fn launch_with(sh: &mut Shell, io: &mut Io, cmd: &str, exe: &str, args: Vec<String>) -> i32 {
    let Some(l) = sh.launcher() else { return io.error(cmd, "the launcher is unavailable") };
    match l.launch(exe.into(), args) {
        Ok(Ok(_)) => 0,
        Ok(Err(e)) => io.error(cmd, &format!("{}: {e}", file_name(exe))),
        Err(_) => io.error(cmd, "the launcher is not responding"),
    }
}

fn cmd_edit(sh: &mut Shell, io: &mut Io, args: &[String]) -> i32 {
    if args.len() < 2 {
        return launch_with(sh, io, "edit", EDITOR.exe, Vec::new());
    }
    let mut status = 0;
    for p in &args[1..] {
        let abs = sh.resolve(p);
        match sh.fs.stat(&abs) {
            Ok(st) if st.is_dir => {
                status = io.error("edit", &format!("{p}: is a folder"));
                continue;
            }
            Ok(_) => {}
            Err(_) if is_read_only(&abs) => {
                status = io.error("edit", &format!("{p}: no such file in the read-only system folder"));
                continue;
            }
            Err(_) => {
                if let Err(e) = sh.fs.touch(&abs) {
                    status = io.fs_error("edit", p, &e);
                    continue;
                }
                io.styled(&format!("Created {}\n", display_path(&abs)), Style::DIM);
            }
        }
        status |= launch_with(sh, io, "edit", EDITOR.exe, vec![abs]);
    }
    status
}

fn cmd_open(sh: &mut Shell, io: &mut Io, args: &[String]) -> i32 {
    if args.len() < 2 {
        return usage(io, &args[0]);
    }
    let mut status = 0;
    for p in &args[1..] {
        let abs = sh.resolve(p);
        let st = match sh.fs.stat(&abs) {
            Ok(st) => st,
            Err(e) => {
                status = io.error(&args[0], &format!("{p}: {e}"));
                continue;
            }
        };
        if st.is_dir {
            status |= launch_with(sh, io, &args[0], FILES.exe, vec![abs]);
        } else if let Some(app) = default_app(&abs) {
            status |= launch_with(sh, io, &args[0], app.exe, vec![abs]);
        } else if file_kind(&abs, st.executable) == FileKind::Program {
            match sh.launch(&abs, Vec::new()) {
                Ok((name, koid)) => io.styled(&format!("Started {name} (PID {koid})\n"), Style::DIM),
                Err(e) => status = io.error(&args[0], &e),
            }
        } else {
            let ext = extension(&abs);
            let what =
                if ext.is_empty() { "files without an extension".to_string() } else { format!("'.{ext}' files") };
            status = io.error(&args[0], &format!("{p}: no app is set up to open {what}"));
        }
    }
    status
}

fn cmd_sync(sh: &mut Shell, io: &mut Io, _args: &[String]) -> i32 {
    match sh.fs.sync() {
        Ok(()) => {
            io.styled("All changes are saved.\n", Style::DIM);
            0
        }
        Err(e) => io.error("sync", &e.to_string()),
    }
}

fn cmd_df(sh: &mut Shell, io: &mut Io, args: &[String]) -> i32 {
    let targets: Vec<String> = if args.len() > 1 {
        args[1..].iter().map(|a| sh.resolve(a)).collect()
    } else {
        vec![HOME.to_string(), "/tmp".to_string(), SYSTEM.to_string()]
    };
    let shown: Vec<String> = targets.iter().map(|p| display_path(p)).collect();
    let w = shown.iter().map(|s| s.chars().count()).max().unwrap_or(6).max(6) + 2;
    io.styled(
        &format!("{}{:>11}{:>11}{:>11}{:>6}  {}\n", pad("FOLDER", w), "SIZE", "USED", "FREE", "USE", "KEPT ON"),
        Style::BOLD,
    );
    let mut status = 0;
    for (path, name) in targets.iter().zip(&shown) {
        let s = match sh.fs.space(path) {
            Ok(s) => s,
            Err(e) => {
                status = io.fs_error("df", name, &e);
                continue;
            }
        };
        let free = s.total.saturating_sub(s.used);
        let pct = if s.total > 0 { (s.used.min(s.total) as u128 * 100 / s.total as u128) as u64 } else { 0 };
        let kept = if is_read_only(path) {
            "system image (read-only)"
        } else if s.persistent {
            "home disk"
        } else {
            "memory (lost at restart)"
        };
        io.print(&pad(name, w));
        io.print(&format!("{:>11}{:>11}", human_size(s.total), human_size(s.used)));
        let low = !is_read_only(path) && free < s.total / 10;
        io.styled(&format!("{:>11}", human_size(free)), if low { Style::fg(color::BRIGHT_RED) } else { Style::PLAIN });
        let pct_style = match pct {
            90.. if !is_read_only(path) => Style::fg(color::BRIGHT_RED).bold(),
            75.. if !is_read_only(path) => Style::fg(color::BRIGHT_YELLOW),
            _ => Style::PLAIN,
        };
        io.styled(&format!("{:>5}%", pct), pct_style);
        io.styled(&format!("  {kept}\n"), Style::DIM);
    }
    status
}
