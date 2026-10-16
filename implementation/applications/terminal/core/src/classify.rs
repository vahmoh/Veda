// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What a command line may do: only look, change something without losing
//! anything, or destroy something.
//!
//! The voice agent may run command lines in the Terminal with the user's
//! consent, and the user may give that consent once and for all ("always
//! allow"). Such standing consent must not cover a command line that
//! deletes files, replaces them, ends programs or forgets the history, so
//! the Terminal runs those only through an action the user approves every
//! time; [`classify`] tells them apart. It reads the line exactly as the
//! shell does, before it runs, and errs on the side of
//! [`Effect::Destroys`]: a command it does not know, a program (which can
//! do anything), a command whose name comes from a variable, and the like.
//!
//! A redirection that empties a file (`> FILE`) destroys what the file
//! held, so it only [changes](Effect::Changes) something when no file is
//! there yet; likewise `open` only changes something when what it opens
//! goes to an application, not when it is a program, which is started.
//! What a relative path names depends on the folder the command runs in,
//! which a `cd` before it in the line may change, or not, if the folder is
//! not there: every folder the command may run in is followed, and a path
//! counts as the worst of what it names in any of them.

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use crate::syntax::{Lexer, Redirect, Word, parse, tokenize};

/// What a command line may do, from the least to the most.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Effect {
    /// Only looks: lists, reads, prints, asks the network.
    Looks,
    /// Changes something without losing anything: changes folder, makes
    /// folders and new files, starts applications, joins a network,
    /// appends to a file.
    Changes,
    /// May delete, replace, move, end or forget something, or does what
    /// cannot be known beforehand (programs).
    Destroys,
}

/// What [`classify`] needs to know of the system.
pub trait Lookup {
    /// The built-in command `name` stands for (its own name for an alias),
    /// if it is one.
    fn builtin(&self, name: &str) -> Option<&'static str>;
    /// Whether `name` starts an installed application.
    fn is_app(&self, name: &str) -> bool;
    /// The absolute path that `path` names in the folder `cwd`, as the
    /// shell's commands read it.
    fn resolve(&self, cwd: &str, path: &str) -> String;
    /// Whether anything is at the absolute `path` (when that cannot be
    /// told, something may be).
    fn exists(&self, path: &str) -> bool;
    /// Whether the absolute `path` is a folder (when that cannot be told,
    /// it is not known to be one).
    fn is_dir(&self, path: &str) -> bool;
    /// Whether a file at the absolute `path` is of a type that an
    /// application opens (its name says so, whatever it holds).
    fn opens_in_app(&self, path: &str) -> bool;
}

/// The most folders followed; with more, the folder counts as not known.
const MAX_FOLDERS: usize = 8;

/// What is known while a line is read: the folders a command may run in
/// (`None`: not known), and whether its variables still stand for what
/// they do now.
struct Reading<'a> {
    lookup: &'a dyn Lookup,
    folders: Option<Vec<String>>,
    vars_known: bool,
}

impl Reading<'_> {
    /// Whether the command will get `w` as it reads now (a pattern may
    /// stand for any number of names).
    fn known(&self, w: &Word) -> bool {
        (self.vars_known || !w.expanded) && !w.glob
    }

    /// Every place the path `w` may name when its command runs (`None`:
    /// not known). Patterns are not expanded here.
    fn places(&self, w: &Word) -> Option<Vec<String>> {
        if w.expanded && !self.vars_known {
            return None;
        }
        if w.text.starts_with('/') {
            return Some(vec![self.lookup.resolve("/", &w.text)]);
        }
        let folders = self.folders.as_ref()?;
        let mut places: Vec<String> = folders.iter().map(|c| self.lookup.resolve(c, &w.text)).collect();
        places.sort();
        places.dedup();
        Some(places)
    }

    /// What a redirection may do: `> FILE` empties the file, so it
    /// destroys unless there is none (wherever the command may run);
    /// `>> FILE` only adds to it; `/dev/null` loses nothing.
    fn redirect(&self, r: &Redirect) -> Effect {
        let (path, append) = match r {
            Redirect::Input(_) | Redirect::Dup { .. } => return Effect::Looks,
            Redirect::Output { path, append, .. } => (path, *append),
        };
        match self.places(path) {
            Some(places) if places.iter().all(|p| p == "/dev/null") => Effect::Looks,
            _ if append => Effect::Changes,
            Some(places) if places.iter().all(|p| !self.lookup.exists(p)) => Effect::Changes,
            _ => Effect::Destroys,
        }
    }

    /// Follows `cd` with `args`: the folder changes, or stays if the new
    /// one may not be there.
    fn cd(&mut self, args: &[Option<&str>], home: &str) {
        let Some(folders) = self.folders.take() else { return };
        let next = match args {
            [] => vec![home.to_string()],
            [Some(a)] if *a != "-" => {
                let mut next: Vec<String> = Vec::new();
                for c in folders {
                    let t = self.lookup.resolve(&c, a);
                    if !self.lookup.is_dir(&t) {
                        next.push(c);
                    }
                    next.push(t);
                }
                next.sort();
                next.dedup();
                next
            }
            // Too many arguments: it fails, and the folder stays.
            [_, _, ..] if args.iter().all(Option::is_some) => folders,
            // Back to the folder before (`cd -`), or one not known here.
            _ => return,
        };
        self.folders = (next.len() <= MAX_FOLDERS).then_some(next);
    }
}

/// What `line` may do when the shell runs it in the folder `cwd`. A line
/// that does not parse runs nothing, so it only [looks](Effect::Looks).
pub fn classify(line: &str, lx: &Lexer, cwd: &str, lookup: &dyn Lookup) -> Effect {
    let Ok(toks) = tokenize(line, lx) else { return Effect::Looks };
    let Ok(list) = parse(toks.toks) else { return Effect::Looks };
    let mut effect = Effect::Looks;
    let mut reading = Reading { lookup, folders: Some(vec![cwd.to_string()]), vars_known: true };
    for (_, pipeline) in &list {
        for cmd in pipeline {
            // Redirections are made first, even if the command then fails.
            for r in &cmd.redirects {
                effect = effect.max(reading.redirect(r));
            }
            let Some(first) = cmd.words.first() else { continue };
            let args: Vec<Option<&str>> =
                cmd.words[1..].iter().map(|w| reading.known(w).then_some(w.text.as_str())).collect();
            let name = if first.expanded || first.glob { None } else { lookup.builtin(&first.text) };
            let e = match name {
                Some(name) => builtin_effect(name, &args, &cmd.words[1..], &reading),
                None if !first.expanded && !first.glob && lookup.is_app(&first.text) => Effect::Changes,
                None => Effect::Destroys,
            };
            effect = effect.max(e);
            if name == Some("cd") {
                reading.cd(&args, lx.home);
            }
        }
        // The next item's variables expand when its turn comes, after this
        // one ran: `$?`, `$PWD` and what `export` sets may then stand for
        // something else than now.
        reading.vars_known = false;
    }
    effect
}

/// What the built-in command `name` may do with `args` (`None` for an
/// argument not known here), `words` as written.
fn builtin_effect(name: &str, args: &[Option<&str>], words: &[Word], reading: &Reading) -> Effect {
    match name {
        "ls" | "pwd" | "cat" | "head" | "tail" | "wc" | "grep" | "find" | "tree" | "stat" | "df" | "ps" | "apps"
        | "uptime" | "free" | "sysinfo" | "date" | "dmesg" | "whoami" | "hostname" | "help" | "echo" | "clear"
        | "sleep" | "env" | "which" | "ifconfig" | "ping" | "nslookup" | "netstat" | "route" | "curl" | "firewall" => {
            Effect::Looks
        }
        "cd" | "mkdir" | "touch" | "edit" | "sync" | "logger" | "export" | "unset" => Effect::Changes,
        // `history -c` forgets the history.
        "history" if args.iter().any(|a| a.is_none_or(|a| a == "-c")) => Effect::Destroys,
        "history" => Effect::Looks,
        "wifi" => match args.first() {
            None => Effect::Looks,
            Some(None) => Effect::Destroys,
            Some(Some(sub)) => match *sub {
                "status" | "scan" | "list" | "networks" | "saved" | "aps" | "log" | "diag" | "diagnostics" | "help"
                | "-h" | "--help" => Effect::Looks,
                "connect" | "disconnect" | "on" | "off" | "auto" => Effect::Changes,
                // `forget`, and whatever else.
                _ => Effect::Destroys,
            },
        },
        // Applications are started; any other program can do anything.
        "run" => match args.first() {
            None => Effect::Looks,
            Some(Some(p)) if reading.lookup.is_app(p) => Effect::Changes,
            Some(_) => Effect::Destroys,
        },
        // Folders and files open in their applications, but a program is
        // started: anything else may be one when the command runs.
        "open" => {
            let opened = |p: &String| reading.lookup.is_dir(p) || reading.lookup.opens_in_app(p);
            for w in words {
                match reading.places(w) {
                    Some(places) if !w.glob && places.iter().all(opened) => {}
                    _ => return Effect::Destroys,
                }
            }
            if words.is_empty() { Effect::Looks } else { Effect::Changes }
        }
        // `rm`, `rmdir`, `cp` and `mv` (which replace files), `kill`,
        // `exit`, and any command not known here.
        _ => Effect::Destroys,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;
    use alloc::string::String;

    /// vsh's built-in commands, some of their aliases, two applications,
    /// and a few folders and files.
    struct Vsh;

    const FOLDERS: &[&str] = &[
        "/",
        "/dev",
        "/home",
        "/home/user",
        "/home/user/Documents",
        "/home/user/Downloads",
        "/home/user/Pictures",
        "/system",
        "/system/bin",
        "/tmp",
    ];
    const FILES: &[&str] = &[
        "/dev/null",
        "/home/user/a.txt",
        "/home/user/list.txt",
        "/home/user/notes.txt",
        "/home/user/Downloads/tool",
        "/home/user/Pictures/cat.png",
        "/system/bin/systest",
        "/tmp/existing",
    ];

    impl Lookup for Vsh {
        fn builtin(&self, name: &str) -> Option<&'static str> {
            const NAMES: &[&str] = &[
                "ls", "cd", "pwd", "cat", "head", "tail", "wc", "grep", "find", "tree", "mkdir", "rmdir", "rm", "cp",
                "mv", "touch", "stat", "edit", "open", "sync", "df", "ps", "kill", "run", "apps", "wifi", "ifconfig",
                "ping", "nslookup", "netstat", "route", "curl", "firewall", "uptime", "free", "sysinfo", "date",
                "dmesg", "logger", "whoami", "hostname", "help", "echo", "clear", "sleep", "history", "env", "export",
                "unset", "which", "exit",
            ];
            match name {
                "del" => Some("rm"),
                "set" => Some("export"),
                "start" => Some("run"),
                "wget" => Some("curl"),
                n => NAMES.iter().find(|b| **b == n).copied(),
            }
        }

        fn is_app(&self, name: &str) -> bool {
            matches!(name, "editor" | "photos")
        }

        fn resolve(&self, cwd: &str, path: &str) -> String {
            let joined = if path.starts_with('/') { String::from(path) } else { format!("{cwd}/{path}") };
            let mut parts: Vec<&str> = Vec::new();
            for part in joined.split('/') {
                match part {
                    "" | "." => {}
                    ".." => {
                        parts.pop();
                    }
                    p => parts.push(p),
                }
            }
            format!("/{}", parts.join("/"))
        }

        fn exists(&self, path: &str) -> bool {
            FOLDERS.contains(&path) || FILES.contains(&path)
        }

        fn is_dir(&self, path: &str) -> bool {
            FOLDERS.contains(&path)
        }

        fn opens_in_app(&self, path: &str) -> bool {
            [".txt", ".png", ".mkv"].iter().any(|ext| path.ends_with(ext))
        }
    }

    fn vars(name: &str) -> Option<String> {
        match name {
            "X" => Some("rm".into()),
            "D" => Some("/tmp".into()),
            "?" => Some("0".into()),
            _ => None,
        }
    }

    fn effect(line: &str) -> Effect {
        classify(line, &Lexer { vars: Some(&vars), home: "/home/user", lenient: false }, "/home/user", &Vsh)
    }

    #[test]
    fn looking() {
        for line in [
            "ls ~/Documents",
            "cat notes.txt | grep milk | wc -l",
            "df; free; uptime",
            "wifi",
            "wifi status",
            "wifi scan",
            "history",
            "ping -c 2 example.org",
            "curl example.org",
            // The Terminal only reads the firewall: Settings changes it.
            "firewall",
            "firewall log",
            "ls > /dev/null 2>&1",
            "ls x 2>/dev/null",
            "ls x 2> ../../dev/null",
            "wc < notes.txt",
            "echo 'rm -r ~'",
            "open",
            // Runs nothing.
            "rm 'unfinished",
            "ls &",
            "rm -r ~; echo >",
        ] {
            assert_eq!(effect(line), Effect::Looks, "{line:?}");
        }
    }

    #[test]
    fn changing_without_losing_anything() {
        for line in [
            "cd /tmp",
            "mkdir -p ~/Projects/new",
            "touch a.txt",
            "edit notes.txt",
            "editor notes.txt",
            "run photos",
            "start editor",
            "open ~/Pictures/cat.png",
            "open Documents Pictures/cat.png",
            "wifi connect Home secret",
            "wifi off",
            "echo milk >> list.txt",
            "export A=1",
            // New files, wherever the command may run.
            "echo hi > new.txt",
            "ls 2> errors.txt",
            "echo hi > $D/new.txt",
            "cd $D; echo hi > notes.txt",
            "mkdir -p ~/agent && cd ~/agent && echo hello from the agent > note.txt && ls -w",
            "cd /tmp; echo hi > notes.txt",
            "cd Downloads; cd ..; echo hi > existing",
            "cd /tmp extra; echo hi > existing",
            "cd /dev && ls > null",
            "cd nowhere; cd /tmp && echo hi > notes.txt",
        ] {
            assert_eq!(effect(line), Effect::Changes, "{line:?}");
        }
    }

    #[test]
    fn destroying() {
        for line in [
            "rm notes.txt",
            "del notes.txt",
            "ls; rm -r ~/Documents",
            "ls && rmdir old",
            "cp a.txt b.txt",
            "mv a.txt b.txt",
            "kill 12",
            "kill -f compositor",
            "history -c",
            "wifi forget Home",
            "wifi reset",
            "exit",
            // Redirections that empty a file.
            "echo hi > notes.txt",
            "echo hi > ~/notes.txt",
            "ls 2> list.txt",
            "ls &> a.txt",
            "echo hi > Documents",
            "echo hi > $D/existing",
            "cat a | sort > a",
            "cd Pictures && echo > cat.png",
            "cd /tmp && echo hi > existing",
            // ... wherever the command may run: `cd` fails if there is no
            // such folder, and the one before may hold the file.
            "cd nowhere; echo hi > notes.txt",
            "mkdir -p /tmp/new && cd /tmp/new && echo hi > notes.txt",
            // ... or where it runs is not known.
            "cd -; echo hi > new.txt",
            "export D=/home/user; cd $D; echo hi > new.txt",
            "cd D*; echo hi > new.txt",
            "cd a; cd b; cd c; cd d; echo hi > new.txt",
            "echo hi > $D/new.txt; echo hi > $D/new.txt",
            "ls; echo hi > $?.txt",
            // Programs can do anything; unknown commands too.
            "systest",
            "./script",
            "/system/bin/gcc a.c",
            "run systest",
            "open /system/bin/systest",
            "open Downloads/tool",
            "mkdir new && open new",
            "open *.png",
            // Where the folder or the variables may differ when it runs.
            "cd /system/bin; open systest",
            "$X notes.txt",
            "export X=photos; run $X",
            "l* notes",
        ] {
            assert_eq!(effect(line), Effect::Destroys, "{line:?}");
        }
    }
}
