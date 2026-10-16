// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The commands of the shell itself: `help`, `echo`, `clear`, `sleep`,
//! `history`, `env`, `export`, `unset`, `which` and `exit`.

use terminal_core::screen::{Style, color};

use super::{BUILTINS, Builtin, Group, find, pad, usage};
use crate::shell::{INTERRUPTED, Io, Shell};

pub(super) fn cmd_help(_sh: &mut Shell, io: &mut Io, args: &[String]) -> i32 {
    if let Some(name) = args.get(1) {
        let Some(b) = find(name) else { return io.error("help", &format!("no such command: {name}")) };
        io.styled(b.usage, Style::BOLD);
        io.print("\n  ");
        io.println(b.help);
        if !b.aliases.is_empty() {
            io.styled(&format!("  Also available as: {}\n", b.aliases.join(", ")), Style::DIM);
        }
        return 0;
    }
    io.styled("Veda Terminal", Style::fg(color::BRIGHT_BLUE).bold());
    io.styled(" — built-in commands\n", Style::DIM);
    let label = |b: &Builtin| {
        let mut names = String::from(b.name);
        for a in b.aliases {
            names.push_str(", ");
            names.push_str(a);
        }
        names
    };
    let width = BUILTINS.iter().map(|b| label(b).chars().count()).max().unwrap_or(10) + 3;
    for (group, title) in [
        (Group::Files, "Files and folders"),
        (Group::System, "Programs and system"),
        (Group::Network, "Network"),
        (Group::Shell, "Shell"),
    ] {
        io.print("\n");
        io.styled(&format!("{title}\n"), Style::BOLD);
        for b in BUILTINS.iter().filter(|b| b.group == group) {
            io.print("  ");
            io.styled(&pad(&label(b), width), Style::fg(color::BRIGHT_GREEN));
            io.println(b.help);
        }
    }
    io.print("\n");
    io.styled("Tips\n", Style::BOLD);
    for tip in [
        "Tab completes commands and paths; Up/Down browse the history, Ctrl+R searches it.",
        "Use | to pass output to another command, > or >> to save it in a file (2> for errors).",
        "Chain commands with ; or &&. Wildcards: rm *.txt, ls ~/Pictures/*.png",
        "Type an app's id (see 'apps') to start it. 'help COMMAND' shows details.",
        "Select text with the mouse; Ctrl+Shift+C copies, Ctrl+Shift+V pastes.",
    ] {
        io.styled("  • ", Style::DIM);
        io.println(tip);
    }
    0
}

fn unescape_echo(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('e') => out.push('\x1b'),
            Some('\\') => out.push('\\'),
            Some('0') => {
                // \033 style octal escape.
                let mut v = 0u32;
                for _ in 0..3 {
                    match chars.peek() {
                        Some(d @ '0'..='7') => {
                            v = v * 8 + d.to_digit(8).unwrap_or(0);
                            chars.next();
                        }
                        _ => break,
                    }
                }
                if let Some(ch) = char::from_u32(v) {
                    out.push(ch);
                }
            }
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

pub(super) fn cmd_echo(_sh: &mut Shell, io: &mut Io, args: &[String]) -> i32 {
    let mut newline = true;
    let mut escapes = false;
    let mut i = 1;
    while let Some(a) = args.get(i) {
        match a.as_str() {
            "-n" => newline = false,
            "-e" => escapes = true,
            "-ne" | "-en" => {
                newline = false;
                escapes = true;
            }
            _ => break,
        }
        i += 1;
    }
    let text = args[i..].join(" ");
    let text = if escapes { unescape_echo(&text) } else { text };
    io.raw(&text);
    if newline {
        io.raw("\n");
    }
    0
}

pub(super) fn cmd_clear(_sh: &mut Shell, io: &mut Io, _args: &[String]) -> i32 {
    io.out.clear();
    0
}

pub(super) fn cmd_sleep(sh: &mut Shell, io: &mut Io, args: &[String]) -> i32 {
    let Some(secs) = args.get(1).and_then(|a| a.parse::<f64>().ok()).filter(|s| *s >= 0.0 && *s <= 3600.0) else {
        return io.error("sleep", "usage: sleep SECONDS (up to 3600)");
    };
    let end = vrt::time::now_ns() + (secs * 1e9) as u64;
    // In short steps, so that Ctrl+C stops it at once.
    while vrt::time::now_ns() < end {
        if sh.interrupted() {
            return INTERRUPTED;
        }
        let left = end.saturating_sub(vrt::time::now_ns());
        vrt::time::sleep(vrt::time::Duration::from_nanos(left.min(50_000_000)));
    }
    0
}

pub(super) fn cmd_history(sh: &mut Shell, io: &mut Io, args: &[String]) -> i32 {
    if args.get(1).is_some_and(|a| a == "-c") {
        sh.clear_history();
        return 0;
    }
    for (i, h) in sh.history.iter().enumerate() {
        io.styled(&format!("{:>5}  ", i + 1), Style::DIM);
        io.println(h);
    }
    0
}

pub(super) fn cmd_env(sh: &mut Shell, io: &mut Io, _args: &[String]) -> i32 {
    let mut vars = sh.env.clone();
    vars.sort();
    for (k, v) in vars {
        io.styled(&k, Style::fg(color::BRIGHT_BLUE));
        io.print("=");
        io.println(&v);
    }
    0
}

pub(super) fn cmd_export(sh: &mut Shell, io: &mut Io, args: &[String]) -> i32 {
    if args.len() < 2 {
        return cmd_env(sh, io, args);
    }
    let mut status = 0;
    for a in &args[1..] {
        match a.split_once('=') {
            Some((k, v)) if !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') => {
                sh.set_var(k, v)
            }
            _ => status = io.error(&args[0], &format!("'{a}': expected NAME=VALUE")),
        }
    }
    status
}

pub(super) fn cmd_unset(sh: &mut Shell, _io: &mut Io, args: &[String]) -> i32 {
    for a in &args[1..] {
        sh.unset_var(a);
    }
    0
}

pub(super) fn cmd_which(sh: &mut Shell, io: &mut Io, args: &[String]) -> i32 {
    if args.len() < 2 {
        return usage(io, "which");
    }
    let mut status = 0;
    for name in &args[1..] {
        if let Some(b) = find(name) {
            io.println(&format!("{name}: shell built-in ({})", b.help.to_lowercase()));
        } else if let Some(app) = sh.apps().into_iter().find(|a| &a.id == name) {
            io.println(&format!("{name}: application \"{}\" ({})", app.name, app.exe));
        } else if let Some(path) = sh.program_for(name) {
            io.println(&path);
        } else {
            status = io.error("which", &format!("{name}: not found"));
        }
    }
    status
}

pub(super) fn cmd_exit(sh: &mut Shell, _io: &mut Io, _args: &[String]) -> i32 {
    sh.exit_requested = true;
    0
}
