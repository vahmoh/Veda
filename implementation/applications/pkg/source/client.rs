// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The commands that ask the packages service (`proto::packages`): what is
//! installed and offered, changes to both, and checks. A change is shown
//! as the service plans it, and carried out once the user agrees (or at
//! once, with `-y`), by the plan's digest: what is done is what was shown.
//! How it goes is told as it goes, from the service's progress events,
//! which a thread of the command's reads.

use std::io::{BufRead, Write};

use ipc::Bytes;
use proto::packages::{
    Action, Failure, PAGE, PROGRESS, PackageSummary, Plan, Progress, RepositoryChange, Request, Scope, Stage, packages,
};
use rt::object::Channel;

use ::packages::count;

use crate::{Exit, bytes, fail};

/// How long a question of the service may take, and a change (none: as
/// long as it takes; the service ends its downloads that stall).
const ASK_NS: u64 = 30_000_000_000;

/// A connection to the service.
pub struct Service(packages::Client);

impl Service {
    pub fn connect() -> Result<Service, Exit> {
        let channel = proto::connect(packages::NAME).map_err(|_| fail("the packages service is not running"))?;
        let client = packages::Client::new(channel);
        client.set_timeout(ASK_NS);
        Ok(Service(client))
    }
}

/// The service's answer, or what to say of its failure.
fn answered<T>(r: Result<Result<T, Failure>, ipc::IpcError>) -> Result<T, Exit> {
    match r {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(f)) => Err(fail(&f.to_string())),
        Err(ipc::IpcError::Kernel(abi::Error::TimedOut)) => Err(fail("the packages service did not answer in time")),
        Err(e) => Err(fail(&format!("the packages service failed: {e}"))),
    }
}

fn plain<T>(r: Result<T, ipc::IpcError>) -> Result<T, Exit> {
    answered(r.map(Ok))
}

/// Every page of `scope` matching `query`.
fn all(service: &Service, query: &str, scope: Scope) -> Result<Vec<PackageSummary>, Exit> {
    let mut found: Vec<PackageSummary> = Vec::new();
    loop {
        let after = found.last().map(|s| s.name.clone()).unwrap_or_default();
        let page = answered(service.0.find(query.into(), scope, after, PAGE))?;
        let last = page.len() < PAGE as usize;
        found.extend(page);
        if last {
            return Ok(found);
        }
    }
}

/// Prints rows in columns, the first row (the heads) as it is.
fn table(rows: &[Vec<String>]) {
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    let widths: Vec<usize> = (0..columns)
        .map(|c| rows.iter().filter_map(|r| r.get(c)).map(|s| s.chars().count()).max().unwrap_or(0))
        .collect();
    for row in rows {
        let mut line = String::new();
        for (c, cell) in row.iter().enumerate() {
            if c + 1 == row.len() {
                line.push_str(cell);
            } else {
                line.push_str(&format!("{cell:<width$}  ", width = widths[c]));
            }
        }
        println!("{}", line.trim_end());
    }
}

pub fn status(service: &Service) -> Result<(), Exit> {
    let s = plain(service.0.status())?;
    let busy = if s.busy { "; a change is under way" } else { "" };
    println!(
        "{} installed, {}{busy}",
        count(s.installed.into(), "package", "packages"),
        count(s.repositories.into(), "repository", "repositories")
    );
    if !s.problem.is_empty() {
        println!("Nothing can be installed or removed: {}", s.problem);
    }
    Ok(())
}

pub fn repositories(service: &Service) -> Result<(), Exit> {
    let list = plain(service.0.repositories())?;
    let mut rows = vec![vec!["NAME".into(), "PACKAGES".into(), "SERIAL".into(), "STATE".into(), "ADDRESS".into()]];
    for r in &list {
        let state = if !r.enabled {
            String::from("disabled")
        } else if r.problem.is_empty() {
            String::from("ok")
        } else {
            String::from("unusable")
        };
        let name = if r.system { format!("{} (system)", r.name) } else { r.name.clone() };
        rows.push(vec![name, r.packages.to_string(), r.serial.to_string(), state, r.url.clone()]);
    }
    table(&rows);
    for r in list.iter().filter(|r| r.enabled && !r.problem.is_empty()) {
        println!("{}: {}", r.name, r.problem);
    }
    Ok(())
}

/// Reads a key as given: `ed25519:...`, or the file holding one.
fn read_key(text: &str) -> Result<String, Exit> {
    if text.starts_with("ed25519:") {
        return Ok(text.to_string());
    }
    let read = std::fs::read_to_string(text).map_err(|e| fail(&format!("{text}: {e}")))?;
    read.lines()
        .map(str::trim)
        .find(|l| l.starts_with("ed25519:"))
        .map(String::from)
        .ok_or_else(|| fail(&format!("{text} holds no public key (ed25519:...)")))
}

pub fn add_repository(service: &Service, name: &str, url: &str, keys: &[String], title: &str) -> Result<(), Exit> {
    let keys = keys.iter().map(|k| read_key(k)).collect::<Result<Vec<_>, _>>()?;
    if url.starts_with("http://") {
        println!(
            "Note: {url} is not encrypted: the packages are checked against the index's signatures, but others on the network can see which ones this computer fetches."
        );
    }
    let change = RepositoryChange::Add { name: name.into(), title: title.into(), url: url.into(), keys };
    answered(service.0.configure(change))?;
    println!("Added the repository {name}: update the repositories to use it (pkg update).");
    Ok(())
}

pub fn remove_repository(service: &Service, name: &str) -> Result<(), Exit> {
    answered(service.0.configure(RepositoryChange::Remove { name: name.into() }))?;
    println!("Removed the repository {name}.");
    Ok(())
}

pub fn enable_repository(service: &Service, name: &str, enabled: bool) -> Result<(), Exit> {
    answered(service.0.configure(RepositoryChange::Enable { name: name.into(), enabled }))?;
    println!("{} the repository {name}.", if enabled { "Enabled" } else { "Disabled" });
    Ok(())
}

/// Shows the service's progress events on `events` until it closes.
fn show_progress(events: Channel) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut last: Option<(Stage, String)> = None;
        while let Ok(msg) = events.read_blocking(abi::DEADLINE_INFINITE) {
            let Ok((PROGRESS, p)) = ipc::decode_event::<Progress>(msg) else { continue };
            let Progress { stage, name, done, total } = p;
            // A line for each stage of each package, and when a download
            // ends.
            let new = last.as_ref().is_none_or(|(s, n)| *s != stage || *n != name);
            let said = match stage {
                Stage::Index if new => Some(format!("Fetching the index of {name}")),
                Stage::Download if new => Some(format!("Fetching {name} ({})", bytes(total))),
                Stage::Unpack if new => Some(format!("Unpacking {name}")),
                Stage::Commit if new => Some(String::from("Putting the files in place")),
                _ => None,
            };
            if let Some(line) = said {
                println!("{line}");
            }
            let _ = done;
            last = Some((stage, name));
        }
    })
}

/// A channel for the service's progress events, and the thread that shows
/// them.
fn progress() -> Result<(Channel, std::thread::JoinHandle<()>), Exit> {
    let (ours, theirs) = Channel::create().map_err(|_| fail("no channel for the progress"))?;
    Ok((theirs, show_progress(ours)))
}

pub fn update(service: &Service) -> Result<(), Exit> {
    let (theirs, shower) = progress()?;
    service.0.set_timeout(0);
    let refreshed = answered(service.0.update(Some(theirs)));
    let _ = shower.join();
    let mut failed = false;
    for r in refreshed? {
        if r.problem.is_empty() {
            let unchanged = if r.unchanged { ", unchanged" } else { "" };
            println!(
                "{}: {} (serial {}){unchanged}",
                r.repository,
                count(r.packages.into(), "package", "packages"),
                r.serial
            );
        } else {
            failed = true;
            println!("{}: not updated: {}", r.repository, r.problem);
        }
    }
    if failed { Err(Exit::Failed) } else { Ok(()) }
}

pub fn search(service: &Service, query: &str) -> Result<(), Exit> {
    let found = all(service, query, Scope::Available)?;
    if found.is_empty() {
        println!("{}", if query.is_empty() { "No package is offered." } else { "No package offered matches." });
        return Ok(());
    }
    let mut rows = vec![vec!["NAME".into(), "VERSION".into(), "REPOSITORY".into(), "SUMMARY".into()]];
    for s in found {
        let version =
            if s.installed.is_empty() { s.version } else { format!("{} (installed {})", s.version, s.installed) };
        rows.push(vec![s.name, version, s.repository, s.summary]);
    }
    table(&rows);
    Ok(())
}

pub fn list(service: &Service, upgradable: bool) -> Result<(), Exit> {
    let found = all(service, "", if upgradable { Scope::Upgradable } else { Scope::Installed })?;
    if found.is_empty() {
        println!("{}", if upgradable { "Everything installed is up to date." } else { "No package is installed." });
        return Ok(());
    }
    let mut rows = vec![if upgradable {
        vec!["NAME".into(), "INSTALLED".into(), "NEWER".into(), "REPOSITORY".into()]
    } else {
        vec!["NAME".into(), "VERSION".into(), "REPOSITORY".into(), "SUMMARY".into()]
    }];
    for s in found {
        if upgradable {
            rows.push(vec![s.name, s.installed, s.version, s.repository]);
        } else {
            let name = if s.explicit { s.name } else { format!("{} (for others)", s.name) };
            rows.push(vec![name, s.version, s.repository, s.summary]);
        }
    }
    table(&rows);
    Ok(())
}

pub fn info(service: &Service, name: &str) -> Result<(), Exit> {
    let p = answered(service.0.describe(name.into()))?;
    println!("{}", p.name);
    let show = |what: &str, d: &proto::packages::Details| {
        println!("  {what:<11}{} ({}, {})", d.version, d.architecture, d.repository);
        println!("  {:<11}{}", "summary", d.summary);
        println!("  {:<11}{}", "licence", d.licence);
        if !d.homepage.is_empty() {
            println!("  {:<11}{}", "homepage", d.homepage);
        }
        for (label, list) in [("depends", &d.depends), ("conflicts", &d.conflicts), ("provides", &d.provides)] {
            if !list.is_empty() {
                println!("  {label:<11}{}", list.join(", "));
            }
        }
        if d.size > 0 {
            println!("  {:<11}{} ({} installed)", "size", bytes(d.size), bytes(d.unpacked));
        } else {
            println!("  {:<11}{}", "size", bytes(d.unpacked));
        }
        for line in d.description.lines() {
            println!("    {line}");
        }
    };
    match &p.installed {
        Some(d) => {
            show("installed", d);
            println!(
                "  {:<11}{} files, on {}{}",
                "files",
                p.files,
                ::packages::date(p.installed_at),
                if p.explicit { "" } else { "; installed for other packages" }
            );
        }
        None => println!("  not installed"),
    }
    if let Some(c) = &p.candidate
        && p.installed.as_ref().is_none_or(|i| i.version != c.version)
    {
        show(if p.installed.is_some() { "newer" } else { "offered" }, c);
    }
    if !p.versions.is_empty() {
        println!("  {:<11}{}", "versions", p.versions.join(", "));
    }
    Ok(())
}

pub fn files(service: &Service, name: &str) -> Result<(), Exit> {
    let mut after = String::new();
    loop {
        let page = answered(service.0.files(name.into(), after.clone(), PAGE))?;
        for f in &page {
            println!("{f}");
        }
        match page.last() {
            Some(last) if page.len() == PAGE as usize => after = last.clone(),
            _ => return Ok(()),
        }
    }
}

/// Shows a plan, as the user is asked to agree to it.
fn show(plan: &Plan) {
    for note in &plan.notes {
        println!("Note: {note}.");
    }
    for (action, heading) in [
        (Action::Remove, "To remove:"),
        (Action::Downgrade, "To downgrade:"),
        (Action::Upgrade, "To upgrade:"),
        (Action::Install, "To install:"),
    ] {
        let steps: Vec<_> = plan.steps.iter().filter(|s| s.action == action).collect();
        if steps.is_empty() {
            continue;
        }
        println!("{heading}");
        for s in steps {
            let what = match s.action {
                Action::Remove => format!("{} {}", s.name, s.from),
                Action::Install => format!("{} {} ({})", s.name, s.to, s.repository),
                _ => format!("{} {} -> {} ({})", s.name, s.from, s.to, s.repository),
            };
            println!("  {what}");
        }
    }
    if plan.more > 0 {
        println!("  and {} more", plan.more);
    }
    let growth = if plan.growth >= 0 {
        format!("{} more installed", bytes(plan.growth as u64))
    } else {
        format!("{} less installed", bytes(plan.growth.unsigned_abs()))
    };
    println!("{} to download; {growth}.", bytes(plan.download));
}

/// Whether the user agrees, asked on the terminal.
fn agree() -> bool {
    print!("Go ahead? [y/N] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).is_ok() && matches!(line.trim(), "y" | "Y" | "yes" | "Yes")
}

/// Plans `request`, shows the plan, and carries it out once agreed to.
pub fn change(service: &Service, request: Request, yes: bool) -> Result<(), Exit> {
    let plan = answered(service.0.plan(request.clone()))?;
    if plan.steps.is_empty() && plan.more == 0 {
        for note in &plan.notes {
            println!("Note: {note}.");
        }
        // Nothing installed or removed; but what was asked for by name may
        // still change, which is kept at once.
        if !plan.world {
            println!("Nothing to do.");
            return Ok(());
        }
    } else {
        show(&plan);
        if !yes && !agree() {
            println!("Nothing was changed.");
            return Ok(());
        }
    }
    let (theirs, shower) = progress()?;
    service.0.set_timeout(0);
    let outcome = answered(service.0.apply(request, Bytes(plan.digest.0.clone()), Some(theirs)));
    let _ = shower.join();
    let o = outcome?;
    let mut done = Vec::new();
    for (n, what) in
        [(o.installed, "installed"), (o.upgraded, "upgraded"), (o.downgraded, "downgraded"), (o.removed, "removed")]
    {
        if n > 0 {
            done.push(format!("{n} {what}"));
        }
    }
    if done.is_empty() {
        println!("Done.");
    } else {
        println!("Done: {}.", done.join(", "));
    }
    Ok(())
}

pub fn verify(service: &Service, name: &str) -> Result<(), Exit> {
    service.0.set_timeout(0);
    let r = answered(service.0.verify(name.into()))?;
    for p in &r.problems {
        if p.package.is_empty() {
            println!("{}: {}", p.path, p.what);
        } else {
            println!("{}: {} (of {})", p.path, p.what, p.package);
        }
    }
    if r.more > 0 {
        println!("and {} more problems", r.more);
    }
    let problems = r.problems.len() + r.more as usize;
    let checked =
        format!("{} of {}", count(r.files.into(), "file", "files"), count(r.packages.into(), "package", "packages"));
    println!(
        "Checked {checked}: {}.",
        match problems {
            0 => String::from("all as installed"),
            1 => String::from("1 problem"),
            n => format!("{n} problems"),
        }
    );
    if problems == 0 { Ok(()) } else { Err(Exit::Failed) }
}

pub fn clean(service: &Service) -> Result<(), Exit> {
    let freed = answered(service.0.clean())?;
    println!("Removed the downloaded package files kept ({}).", bytes(freed));
    Ok(())
}
