// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The commands for programs and the system: `ps`, `kill`, `run`, `apps`,
//! `uptime`, `free`, `sysinfo`, `date`, `dmesg`, `logger`, `whoami` and
//! `hostname`.

use apps_common::format::{cstr, uptime};
use apps_common::process::{self, Classifier};
use terminal_core::screen::{Style, color};
use vfiles::format::human_size;

use super::{line_count, options, pad, read_stdin, usage};
use crate::shell::{Io, Shell};

/// The most of its input `logger` writes to the system log (bytes).
const LOGGED: usize = 64 * 1024;

/// Which processes are the system's (see [`apps_common::process`]), from
/// all the processes listed and what the launcher, which started the
/// applications, says.
fn classifier(sh: &mut Shell, all: &[vabi::ProcessInfo]) -> Classifier {
    let tasks = sh.launcher().and_then(|l| l.tasks().ok());
    Classifier::from_tasks(all, tasks.as_deref())
}

/// CPU time as `m:ss.cc`.
fn format_cpu(ns: u64) -> String {
    let cs = ns / 10_000_000;
    let (m, s, c) = (cs / 6000, (cs / 100) % 60, cs % 100);
    format!("{m}:{s:02}.{c:02}")
}

fn system_info(io: &mut Io, cmd: &str) -> Option<vabi::SystemInfo> {
    match vrt::object::system_info() {
        Ok(i) => Some(i),
        Err(e) => {
            io.error(cmd, &format!("cannot read system information: {e}"));
            None
        }
    }
}

/// A text progress bar `[████░░░░]`.
fn bar(fraction: f32, width: usize) -> (String, String) {
    let filled = ((fraction.clamp(0.0, 1.0) * width as f32) + 0.5) as usize;
    let a: String = core::iter::repeat_n('█', filled.min(width)).collect();
    let b: String = core::iter::repeat_n('░', width - filled.min(width)).collect();
    (a, b)
}

pub(super) fn cmd_ps(sh: &mut Shell, io: &mut Io, args: &[String]) -> i32 {
    let (opts, _) = match options(args, "aef") {
        Ok(v) => v,
        Err(e) => return io.error(&args[0], &e),
    };
    let all = opts.contains(&'a');
    let mut procs = process::list();
    let is_system = classifier(sh, &procs);
    if !all {
        procs.retain(|p| p.state == vabi::process_state::RUNNING);
    }
    procs.sort_by_key(|p| p.koid);
    io.styled(
        &format!(
            "{:>6}  {}  {:>7}  {:>10}  {:>9}{}\n",
            "PID",
            pad("NAME", 16),
            "THREADS",
            "MEMORY",
            "CPU TIME",
            if all { "  STATE" } else { "" }
        ),
        Style::BOLD,
    );
    let mut mem = 0;
    for p in &procs {
        io.styled(&format!("{:>6}  ", p.koid), Style::DIM);
        let style = if is_system.is_system(p.koid) { Style::fg(color::CYAN) } else { Style::fg(color::BRIGHT_GREEN) };
        io.styled(&pad(p.name(), 16), style);
        io.print(&format!("  {:>7}  {:>10}  {:>9}", p.threads, human_size(p.memory_bytes), format_cpu(p.cpu_time_ns)));
        if all {
            let state = match p.state {
                vabi::process_state::RUNNING => "running",
                vabi::process_state::EXITED => "exited",
                vabi::process_state::KILLED => "killed",
                vabi::process_state::CRASHED => "crashed",
                _ => "?",
            };
            io.print(&format!("  {state}"));
        }
        io.print("\n");
        mem += p.memory_bytes;
    }
    io.styled(&format!("{} processes, {} in use\n", procs.len(), human_size(mem)), Style::DIM);
    0
}

pub(super) fn cmd_kill(sh: &mut Shell, io: &mut Io, args: &[String]) -> i32 {
    let (opts, targets) = match options(args, "f9") {
        Ok(v) => v,
        Err(e) => return io.error(&args[0], &e),
    };
    if targets.is_empty() {
        return usage(io, "kill");
    }
    let force = opts.contains(&'f') || opts.contains(&'9');
    let mut procs = process::list();
    let is_system = classifier(sh, &procs);
    procs.retain(|p| p.state == vabi::process_state::RUNNING);
    let mut status = 0;
    for t in &targets {
        let matches: Vec<(u64, String)> = match t.parse::<u64>() {
            Ok(pid) => procs.iter().filter(|p| p.koid == pid).map(|p| (p.koid, p.name().to_string())).collect(),
            Err(_) => procs
                .iter()
                .filter(|p| p.name().eq_ignore_ascii_case(t))
                .map(|p| (p.koid, p.name().to_string()))
                .collect(),
        };
        if matches.is_empty() {
            status = io.error("kill", &format!("{t}: no such process"));
            continue;
        }
        for (koid, name) in matches {
            if is_system.is_system(koid) && !force {
                status = io
                    .error("kill", &format!("{name} (PID {koid}) is a system process; use 'kill -f' to end it anyway"));
                continue;
            }
            let Some(l) = sh.launcher() else { return io.error("kill", "the launcher is unavailable") };
            match l.kill(koid) {
                Ok(Ok(())) => io.styled(&format!("Ended {name} (PID {koid})\n"), Style::DIM),
                Ok(Err(e)) => status = io.error("kill", &format!("{name} (PID {koid}): {}", e)),
                Err(_) => status = io.error("kill", "the launcher is not responding"),
            }
        }
    }
    status
}

pub(super) fn cmd_run(sh: &mut Shell, io: &mut Io, args: &[String]) -> i32 {
    if args.len() < 2 {
        return usage(io, &args[0]);
    }
    let rest: Vec<String> = args[2..].iter().map(|a| sh.absolute_arg(a)).collect();
    match sh.launch(&args[1], rest) {
        Ok((name, koid)) => {
            io.styled(&format!("Started {name} (PID {koid})\n"), Style::DIM);
            0
        }
        Err(e) => io.error(&args[0], &e),
    }
}

pub(super) fn cmd_apps(sh: &mut Shell, io: &mut Io, _args: &[String]) -> i32 {
    let apps = sh.apps();
    if apps.is_empty() {
        return io.error("apps", "no applications found (is the launcher running?)");
    }
    let idw = apps.iter().map(|a| a.id.chars().count()).max().unwrap_or(2).max(2);
    let namew = apps.iter().map(|a| a.name.chars().count()).max().unwrap_or(4).max(4);
    io.styled(
        &format!("{}  {}  {}  {}\n", pad("ID", idw), pad("NAME", namew), pad("CATEGORY", 12), "DESCRIPTION"),
        Style::BOLD,
    );
    for a in &apps {
        io.styled(&pad(&a.id, idw), Style::fg(color::BRIGHT_GREEN).bold());
        io.print("  ");
        io.print(&pad(&a.name, namew));
        io.print("  ");
        io.styled(&pad(&a.category, 12), Style::fg(color::CYAN));
        io.print("  ");
        io.styled(&a.description, Style::DIM);
        io.print("\n");
    }
    io.styled("Start one with 'run ID' (or just type its ID).\n", Style::DIM);
    0
}

pub(super) fn cmd_uptime(_sh: &mut Shell, io: &mut Io, _args: &[String]) -> i32 {
    let Some(i) = system_info(io, "uptime") else { return 1 };
    io.print("up ");
    io.styled(&uptime(i.uptime_ns), Style::BOLD);
    io.println(&format!(", {} processes, {} threads, {} CPUs", i.process_count, i.thread_count, i.cpu_count));
    0
}

pub(super) fn cmd_free(_sh: &mut Shell, io: &mut Io, _args: &[String]) -> i32 {
    let Some(i) = system_info(io, "free") else { return 1 };
    let used = i.total_memory.saturating_sub(i.free_memory);
    let mib = |b: u64| format!("{} MiB", b >> 20);
    io.styled(&format!("{:>9} {:>12} {:>12} {:>12}\n", "", "total", "used", "free"), Style::BOLD);
    io.println(&format!("{:>9} {:>12} {:>12} {:>12}", "Memory:", mib(i.total_memory), mib(used), mib(i.free_memory)));
    let f = if i.total_memory > 0 { used as f32 / i.total_memory as f32 } else { 0.0 };
    let (a, b) = bar(f, 32);
    io.print(&format!("{:>9} ", ""));
    io.styled(&a, Style::fg(if f > 0.85 { color::BRIGHT_RED } else { color::BRIGHT_GREEN }));
    io.styled(&b, Style::DIM);
    io.println(&format!(" {:.1}% in use", f * 100.0));
    0
}

pub(super) fn cmd_sysinfo(_sh: &mut Shell, io: &mut Io, _args: &[String]) -> i32 {
    let Some(i) = system_info(io, "sysinfo") else { return 1 };
    let screen = vproto::connect(vproto::display::display::NAME)
        .ok()
        .and_then(|ch| vproto::display::display::Client::new(ch).screen_info().ok());
    let version = cstr(&i.version);
    let used = i.total_memory.saturating_sub(i.free_memory);
    let mut info: Vec<(&str, String)> = vec![
        ("OS", format!("Veda {} {}", version.trim_start_matches("Veda").trim(), vrt::arch::MACHINE)),
        ("Kernel", format!("vkernel {} (microkernel)", version.trim_start_matches("Veda").trim())),
        ("Uptime", uptime(i.uptime_ns)),
        ("CPU", format!("{} ({} cores)", cstr(&i.cpu_brand), i.cpu_count)),
        ("Memory", format!("{} MiB / {} MiB", used >> 20, i.total_memory >> 20)),
        ("Processes", format!("{} ({} threads)", i.process_count, i.thread_count)),
    ];
    if let Some(s) = screen {
        info.push(("Display", format!("{}x{}", s.width, s.height)));
    }
    info.push(("Shell", "vsh 0.1".to_string()));
    info.push(("Terminal", "Veda Terminal".to_string()));
    info.push(("Font", "JetBrains Mono".to_string()));
    // The logo: the ring, like the About window's (round in cells about
    // twice as tall as they are wide).
    const RING: [&str; 7] = [
        "    ████████    ",
        "  ███      ███  ",
        "███          ███",
        "██            ██",
        "███          ███",
        "  ███      ███  ",
        "    ████████    ",
    ];
    let rows = (info.len() + 2).max(RING.len());
    for r in 0..rows {
        io.print("  ");
        match RING.get(r) {
            Some(line) => io.styled(line, Style::fg(color::BRIGHT_WHITE)),
            None => io.print(&pad("", 16)),
        }
        io.print("    ");
        match r {
            0 => {
                io.styled("user", Style::fg(color::BRIGHT_GREEN).bold());
                io.print("@");
                io.styled("veda", Style::fg(color::BRIGHT_GREEN).bold());
            }
            1 => io.styled("─────────", Style::DIM),
            r if r - 2 < info.len() => {
                let (k, v) = &info[r - 2];
                io.styled(&format!("{k}: "), Style::fg(color::BRIGHT_BLUE).bold());
                io.print(v);
            }
            _ => {}
        }
        io.print("\n");
    }
    io.print("\n  ");
    for c in 0..16u8 {
        io.styled("███", Style::fg(c));
        if c == 7 {
            io.print("\n  ");
        }
    }
    io.print("\n");
    0
}

pub(super) fn cmd_date(_sh: &mut Shell, io: &mut Io, _args: &[String]) -> i32 {
    let d = vrt::time::DateTime::now();
    io.println(&format!(
        "{}, {} {} {}, {:02}:{:02}:{:02}",
        d.weekday_name(),
        d.day,
        d.month_name(),
        d.year,
        d.hour,
        d.minute,
        d.second
    ));
    0
}

/// Reads the whole kernel log ring.
fn read_log() -> String {
    let mut out = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut offset = 0u64;
    while let Ok((n, next)) = vrt::object::log_read(offset, &mut buf) {
        if n == 0 || out.len() > 4 << 20 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
        offset = next;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn message_style(msg: &str) -> Style {
    let lower = msg.to_ascii_lowercase();
    if lower.contains("panic") || lower.contains("error") || lower.contains("fail") || lower.contains("crash") {
        Style::fg(color::BRIGHT_RED)
    } else if lower.contains("warn") {
        Style::fg(color::BRIGHT_YELLOW)
    } else {
        Style::PLAIN
    }
}

fn print_log_line(io: &mut Io, line: &str) {
    if !io.is_terminal() || !line.starts_with('[') {
        io.println(line);
        return;
    }
    let Some(close) = line.find(']') else {
        io.println(line);
        return;
    };
    io.styled(&line[..=close], Style::DIM);
    let rest = &line[close + 1..];
    let trimmed = rest.trim_start();
    if let Some(after_cpu) = trimmed.strip_prefix("cpu") {
        // Kernel record: "cpuN level message".
        let mut parts = after_cpu.splitn(2, ' ');
        let cpu = parts.next().unwrap_or("");
        let rest = parts.next().unwrap_or("").trim_start();
        let (level, msg) = rest.split_once(' ').unwrap_or((rest, ""));
        io.styled(&format!(" cpu{cpu} "), Style::DIM);
        let ls = match level {
            "ERROR" => Style::fg(color::BRIGHT_RED).bold(),
            "WARN" => Style::fg(color::BRIGHT_YELLOW).bold(),
            "debug" => Style::DIM,
            _ => Style::fg(color::GREEN),
        };
        io.styled(&pad(level, 5), ls);
        io.print(" ");
        let msg = msg.trim_start();
        io.styled(msg, message_style(msg));
    } else if let Some((name, msg)) = rest.split_once(": ") {
        io.styled(&format!("{name}:"), Style::fg(color::CYAN));
        io.print(" ");
        io.styled(msg, message_style(msg));
    } else {
        io.print(rest);
    }
    io.print("\n");
}

pub(super) fn cmd_dmesg(_sh: &mut Shell, io: &mut Io, args: &[String]) -> i32 {
    let (n, filters) = match line_count(args, 100) {
        Ok(v) => v,
        Err(e) => return io.error(&args[0], &e),
    };
    let filter = filters.join(" ").to_ascii_lowercase();
    let log = read_log();
    let lines: Vec<&str> =
        log.lines().filter(|l| filter.is_empty() || l.to_ascii_lowercase().contains(&filter)).collect();
    let start = lines.len().saturating_sub(n);
    if start > 0 && io.is_terminal() {
        io.styled(&format!("… {} earlier lines (use '{} -a' to see everything)\n", start, args[0]), Style::DIM);
    }
    for l in &lines[start..] {
        print_log_line(io, l);
    }
    if lines.is_empty() && io.is_terminal() {
        io.styled("No matching log lines.\n", Style::DIM);
    }
    0
}

pub(super) fn cmd_logger(sh: &mut Shell, io: &mut Io, args: &[String]) -> i32 {
    let text = if args.len() > 1 {
        args[1..].join(" ")
    } else {
        match read_stdin(sh, io, "logger", LOGGED) {
            Some(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            None => return 1,
        }
    };
    let mut any = false;
    for line in text.lines().map(str::trim_end).filter(|l| !l.is_empty()) {
        // The kernel prefixes each line with the process name ("terminal").
        println!("{line}");
        any = true;
    }
    if any { 0 } else { usage(io, "logger") }
}

pub(super) fn cmd_whoami(_sh: &mut Shell, io: &mut Io, _args: &[String]) -> i32 {
    io.println("user");
    0
}

pub(super) fn cmd_hostname(_sh: &mut Shell, io: &mut Io, _args: &[String]) -> i32 {
    io.println("veda");
    0
}
