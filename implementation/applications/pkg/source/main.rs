// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `pkg` — Veda's packages, in the Terminal: installs, upgrades and removes
//! them, and lists and checks what is installed and offered, through the
//! package manager, which alone changes them (the Terminal's programs may
//! ask it to); and makes keys, packages and repositories, for whoever
//! publishes them ([`author`]). [Packages](../../../../documentation/PACKAGES.md)
//! has the whole of it.

mod author;
mod client;

use std::path::Path;

use proto::packages::Request;

use crate::client::Service;

const USAGE: &str = "\
usage: pkg COMMAND [ARGUMENTS]

Packages:
  pkg update                       fetch the repositories' indexes anew
  pkg search [TEXT]                list the packages offered (whose name or summary holds TEXT)
  pkg list [--upgradable]          list the packages installed (or those with newer versions)
  pkg info NAME                    what is known of a package
  pkg files NAME                   the files of a package installed
  pkg install [-y] PACKAGE...      install packages (NAME, NAME=VERSION, or 'NAME (>= VERSION)')
  pkg remove [-y] NAME...          remove packages, and those installed only for them
  pkg upgrade [-y] [NAME...]       upgrade packages (every one, if none is named)
  pkg verify [NAME]                check the files installed (of one package, or of all)
  pkg clean                        remove the downloaded package files kept
  pkg status                       what the package manager is doing

Repositories:
  pkg repo list
  pkg repo add NAME URL KEY... [--title TITLE]
                                   add a repository: KEY is ed25519:... or a file holding one
  pkg repo remove NAME
  pkg repo enable NAME
  pkg repo disable NAME

Publishing:
  pkg key FILE                     make a key pair: FILE (secret) and FILE.pub
  pkg build DIR [-o FILE]          make a package of DIR (DIR/manifest, DIR/files/...)
  pkg index DIR --name NAME --key FILE [--serial N] [--expires DAYS|never]
                                   sign an index of the packages in DIR, making it a repository
";

/// How the command ends.
#[derive(Debug, PartialEq, Eq)]
pub enum Exit {
    /// It was used wrongly (status 2).
    Usage,
    /// It failed, having said why (status 1).
    Failed,
}

/// Says why the command failed.
pub fn fail(why: &str) -> Exit {
    eprintln!("pkg: {why}");
    Exit::Failed
}

fn usage(why: &str) -> Exit {
    eprintln!("pkg: {why}\n\n{USAGE}");
    Exit::Usage
}

/// `bytes` as people read them: `512 B`, `12 KiB`, `3.4 MiB`.
pub fn bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if value < 10.0 { format!("{value:.1} {}", UNITS[unit]) } else { format!("{value:.0} {}", UNITS[unit]) }
}

/// The time now (Unix time, s, UTC).
pub fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// A package as named on the command line, as a requirement:
/// `NAME=VERSION` is `NAME (= VERSION)`.
fn requirement(text: &str) -> String {
    match text.split_once('=') {
        Some((name, version)) if !name.contains(['(', ' ', '<', '>']) && !version.is_empty() => {
            format!("{name} (= {version})")
        }
        _ => text.to_string(),
    }
}

/// The value of option `name` in `args`, taken out of them.
fn option(args: &mut Vec<String>, name: &str) -> Result<Option<String>, Exit> {
    let Some(i) = args.iter().position(|a| a == name) else { return Ok(None) };
    if i + 1 >= args.len() {
        return Err(usage(&format!("{name} takes a value")));
    }
    let value = args.remove(i + 1);
    args.remove(i);
    Ok(Some(value))
}

/// Whether flag `name` is in `args`, taken out of them.
fn flag(args: &mut Vec<String>, names: &[&str]) -> bool {
    let before = args.len();
    args.retain(|a| !names.contains(&a.as_str()));
    args.len() != before
}

fn run(mut args: Vec<String>) -> Result<(), Exit> {
    if args.is_empty() || flag(&mut args, &["-h", "--help", "help"]) {
        print!("{USAGE}");
        return Ok(());
    }
    let command = args.remove(0);
    let yes = flag(&mut args, &["-y", "--yes"]);
    let none =
        |args: &[String]| if args.is_empty() { Ok(()) } else { Err(usage(&format!("{command} takes no {}", args[0]))) };
    match command.as_str() {
        "key" => match args.as_slice() {
            [file] => author::key(Path::new(file)).map_err(|e| fail(&e)),
            _ => Err(usage("pkg key FILE")),
        },
        "build" => {
            let out = option(&mut args, "-o")?;
            match args.as_slice() {
                [dir] => author::build(Path::new(dir), out.as_deref().map(Path::new)).map(drop).map_err(|e| fail(&e)),
                _ => Err(usage("pkg build DIR [-o FILE]")),
            }
        }
        "index" => {
            let name = option(&mut args, "--name")?.ok_or_else(|| usage("pkg index needs --name NAME"))?;
            let key = option(&mut args, "--key")?.ok_or_else(|| usage("pkg index needs --key FILE"))?;
            let serial = match option(&mut args, "--serial")? {
                Some(s) => Some(s.parse().map_err(|_| usage("--serial takes a number"))?),
                None => None,
            };
            let expires = match option(&mut args, "--expires")?.as_deref() {
                Some("never") => None,
                Some(days) => Some(days.parse().map_err(|_| usage("--expires takes a number of days, or never"))?),
                None => Some(30),
            };
            let [dir] = args.as_slice() else { return Err(usage("pkg index DIR --name NAME --key FILE")) };
            let options =
                author::IndexOptions { dir: Path::new(dir), repository: &name, key: Path::new(&key), serial, expires };
            author::index(&options).map_err(|e| fail(&e))
        }
        "repo" => {
            let service = Service::connect()?;
            let title = option(&mut args, "--title")?.unwrap_or_default();
            match args.as_slice() {
                [] => client::repositories(&service),
                [what] if what == "list" => client::repositories(&service),
                [what, name, url, keys @ ..] if what == "add" && !keys.is_empty() => {
                    client::add_repository(&service, name, url, keys, &title)
                }
                [what, name] if what == "remove" => client::remove_repository(&service, name),
                [what, name] if what == "enable" => client::enable_repository(&service, name, true),
                [what, name] if what == "disable" => client::enable_repository(&service, name, false),
                _ => Err(usage("pkg repo list|add|remove|enable|disable")),
            }
        }
        "update" => {
            none(&args)?;
            client::update(&Service::connect()?)
        }
        "search" => client::search(&Service::connect()?, &args.join(" ")),
        "list" => {
            let upgradable = flag(&mut args, &["--upgradable", "-u"]);
            none(&args)?;
            client::list(&Service::connect()?, upgradable)
        }
        "info" | "files" => {
            let [name] = args.as_slice() else { return Err(usage(&format!("pkg {command} NAME"))) };
            let service = Service::connect()?;
            if command == "info" { client::info(&service, name) } else { client::files(&service, name) }
        }
        "install" | "remove" | "upgrade" => {
            let packages: Vec<String> = args.iter().map(|a| requirement(a)).collect();
            let request = match command.as_str() {
                "install" if !packages.is_empty() => Request::Install { packages },
                "remove" if !packages.is_empty() => Request::Remove { packages },
                "upgrade" => Request::Upgrade { packages },
                _ => return Err(usage(&format!("pkg {command} names the packages"))),
            };
            client::change(&Service::connect()?, request, yes)
        }
        "verify" => match args.as_slice() {
            [] => client::verify(&Service::connect()?, ""),
            [name] => client::verify(&Service::connect()?, name),
            _ => Err(usage("pkg verify [NAME]")),
        },
        "clean" => {
            none(&args)?;
            client::clean(&Service::connect()?)
        }
        "status" => {
            none(&args)?;
            client::status(&Service::connect()?)
        }
        other => Err(usage(&format!("there is no command {other}"))),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(match run(args) {
        Ok(()) => 0,
        Err(Exit::Failed) => 1,
        Err(Exit::Usage) => 2,
    });
}
