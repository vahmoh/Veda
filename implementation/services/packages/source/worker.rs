// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The worker: does what takes time (fetching indexes, downloading and
//! installing packages, checking installed files) on a thread of its own,
//! one job at a time, below the priority of the applications' threads, so
//! that the service answers its other callers meanwhile and the desktop
//! stays quick. Each job is given what it needs (the repositories, the
//! indexes and the database as they were when it began, shared, not
//! copied), and hands back what it found, which the service then takes.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use abi::RawHandle;
use packages::config::Repository;
use packages::database::Database;
use packages::index;
use packages::stdfs::StdFiles;
use packages::transaction::{self, Change, Failure as FilesFailure, Incoming, Layout};
use proto::packages::{
    Action, Failure, Outcome, PROGRESS, PackageError, Problem, Progress, REPORT_PROBLEMS, Refreshed, Report, Stage,
};
use rt::object::{Channel, Event};
use rt::println;
use rt::sync::{Condvar, Mutex};

use crate::fetch;
use crate::state::{self, Resolved, Taken, failure, now};

/// Something to do.
pub enum Job {
    /// Fetch every repository's index anew; `previous` holds those taken
    /// before, which a new one must not be older than.
    Update { repositories: Vec<Repository>, previous: BTreeMap<String, Arc<Taken>>, progress: Option<Channel> },
    /// Carry out a request worked out.
    Apply { resolved: Box<Resolved>, db: Arc<Database>, layout: Layout, progress: Option<Channel> },
    /// Check the installed files (of `name`, or every package's).
    Verify { name: Option<String>, db: Arc<Database>, layout: Layout },
}

/// A job done.
pub enum Done {
    Updated {
        refreshed: Vec<Refreshed>,
        taken: Vec<(String, Arc<Taken>)>,
    },
    /// The outcome, and the database after if the change was made.
    Applied {
        result: Result<Outcome, Failure>,
        db: Option<Database>,
    },
    Verified(Result<Report, Failure>),
}

struct Shared {
    jobs: Mutex<VecDeque<Job>>,
    wake: Condvar,
    done: Mutex<Vec<Done>>,
}

/// The service's handle on the worker: [`Worker::event_handle`] is
/// signalled when a job is done ([`Worker::take`]).
pub struct Worker {
    shared: Arc<Shared>,
    event: Event,
}

impl Worker {
    pub fn start() -> Option<Worker> {
        let event = Event::create().ok()?;
        let theirs = Event::from_handle(event.0.duplicate(None).ok()?);
        let shared =
            Arc::new(Shared { jobs: Mutex::new(VecDeque::new()), wake: Condvar::new(), done: Mutex::new(Vec::new()) });
        let worker = shared.clone();
        rt::thread::Builder::new()
            .name("packages-worker")
            .stack_size(1 << 20)
            .priority(abi::priority::LOW)
            .spawn(move || work(&worker, &theirs))
            .ok()?;
        Some(Worker { shared, event })
    }

    pub fn give(&self, job: Job) {
        self.shared.jobs.lock().push_back(job);
        self.shared.wake.notify_one();
    }

    pub fn take(&self) -> Vec<Done> {
        let _ = self.event.clear();
        core::mem::take(&mut *self.shared.done.lock())
    }

    pub fn event_handle(&self) -> RawHandle {
        self.event.raw()
    }
}

fn work(shared: &Shared, done: &Event) {
    loop {
        let job = {
            let mut q = shared.jobs.lock();
            loop {
                if let Some(j) = q.pop_front() {
                    break j;
                }
                q = shared.wake.wait(q);
            }
        };
        let result = match job {
            Job::Update { repositories, previous, progress } => update(&repositories, &previous, progress.as_ref()),
            Job::Apply { resolved, db, layout, progress } => apply(&resolved, &db, &layout, progress.as_ref()),
            Job::Verify { name, db, layout } => Done::Verified(verify(name.as_deref(), &db, &layout)),
        };
        shared.done.lock().push(result);
        let _ = done.signal();
    }
}

/// Tells the caller how a job goes; a caller that falls behind misses some.
fn tell(progress: Option<&Channel>, stage: Stage, name: &str, done: u64, total: u64) {
    if let Some(ch) = progress {
        let _ = ipc::send_event(ch, PROGRESS, Progress { stage, name: name.to_string(), done, total });
    }
}

/// Fetches and checks each enabled repository's index; keeps those of
/// repositories not in the system image.
fn update(repositories: &[Repository], previous: &BTreeMap<String, Arc<Taken>>, progress: Option<&Channel>) -> Done {
    let mut refreshed = Vec::new();
    let mut taken = Vec::new();
    for r in repositories.iter().filter(|r| r.enabled) {
        tell(progress, Stage::Index, &r.name, 0, 0);
        let local = r.url.in_system_image();
        let before = previous.get(&r.name).filter(|_| !local);
        let policy = index::Policy {
            keys: &r.keys,
            repository: &r.name,
            now: now(),
            must_expire: r.url.is_web(),
            previous: before.map(|t| (t.verified.contents.serial, t.verified.contents_sha256)),
        };
        let fetched = fetch::index(&r.url).and_then(|bytes| {
            let verified = index::verify(&bytes, &policy).map_err(|e| e.to_string())?;
            if !local {
                let path = format!("{}/indexes/{}.vidx", state::STATE, r.name);
                state::write_whole(&path, &bytes).map_err(|e| format!("cannot keep its index: {e}"))?;
            }
            Ok(verified)
        });
        match fetched {
            Ok(verified) => {
                let c = &verified.contents;
                let unchanged = before.is_some_and(|t| t.verified.contents_sha256 == verified.contents_sha256);
                let unchanged_said = if unchanged { ", unchanged" } else { "" };
                println!(
                    "{}: {} (serial {}){unchanged_said}",
                    r.name,
                    packages::count(c.packages.len() as u64, "package", "packages"),
                    c.serial
                );
                refreshed.push(Refreshed {
                    repository: r.name.clone(),
                    packages: c.packages.len() as u32,
                    serial: c.serial,
                    unchanged,
                    problem: String::new(),
                });
                taken.push((r.name.clone(), Arc::new(Taken { verified, at: now() })));
            }
            Err(problem) => {
                println!("{} could not be updated: {problem}", r.name);
                refreshed.push(Refreshed {
                    repository: r.name.clone(),
                    packages: 0,
                    serial: 0,
                    unchanged: false,
                    problem,
                });
            }
        }
    }
    Done::Updated { refreshed, taken }
}

/// The failure of a change, as the caller is told it.
fn change_failure(e: &transaction::Error) -> Failure {
    let error = match e {
        transaction::Error::Files { error, .. } if error.failure == FilesFailure::NoSpace => PackageError::NoSpace,
        transaction::Error::Files { .. } | transaction::Error::Interrupted => PackageError::Unavailable,
        transaction::Error::Archive { .. }
        | transaction::Error::Mismatch(_)
        | transaction::Error::Database(_)
        | transaction::Error::Journal(_) => PackageError::Damaged,
        transaction::Error::Conflict { .. } => PackageError::Conflict,
        transaction::Error::NotInstalled(_) => PackageError::NotFound,
    };
    failure(error, e.to_string())
}

/// Downloads what a change installs (what the cache does not hold
/// already), then carries it out.
fn apply(resolved: &Resolved, db: &Database, layout: &Layout, progress: Option<&Channel>) -> Done {
    let mut paths = Vec::new();
    let mut downloaded = 0;
    for i in &resolved.installs {
        let path = state::cached(&i.offer);
        let kept = std::fs::read(&path)
            .is_ok_and(|b| b.len() as u64 == i.offer.size && packages::Digest::of(&b) == i.offer.sha256);
        if !kept {
            let name = i.offer.manifest.name.as_str();
            tell(progress, Stage::Download, name, 0, i.offer.size);
            let fetched = fetch::package(&i.url, &i.offer)
                .and_then(|bytes| state::write_whole(&path, &bytes).map_err(|e| format!("cannot keep it: {e}")));
            if let Err(e) = fetched {
                println!("{} could not be downloaded: {e}", i.offer.manifest.label());
                let detail = format!("{} could not be downloaded: {e}", i.offer.manifest.label());
                return Done::Applied { result: Err(failure(PackageError::Download, detail)), db: None };
            }
            tell(progress, Stage::Download, name, i.offer.size, i.offer.size);
            downloaded += i.offer.size;
        }
        paths.push(path);
    }
    let change = Change {
        remove: resolved.removes.clone(),
        install: resolved
            .installs
            .iter()
            .zip(&paths)
            .map(|(i, path)| Incoming { path, offer: &i.offer, repository: &i.repository })
            .collect(),
        world: resolved.world.clone(),
        now: now(),
    };
    let mut files = StdFiles { anchor: state::STATE.into() };
    // A change of before that tidying up did not end is ended first.
    if let Err(e) = transaction::recover(&mut files, layout) {
        return Done::Applied { result: Err(change_failure(&e)), db: None };
    }
    let mut told = |step: transaction::Step| match step {
        transaction::Step::Unpacking { package, done, total } => tell(progress, Stage::Unpack, package, done, total),
        transaction::Step::Committing => tell(progress, Stage::Commit, "", 0, 0),
    };
    match transaction::apply(&mut files, layout, db, &change, &mut told) {
        Ok(after) => {
            let mut outcome = Outcome { downloaded, ..Outcome::default() };
            for s in &resolved.steps {
                let (count, said) = match s.action {
                    Action::Install => (&mut outcome.installed, format!("installed {} {}", s.name, s.to)),
                    Action::Upgrade => (&mut outcome.upgraded, format!("upgraded {} {} to {}", s.name, s.from, s.to)),
                    Action::Downgrade => {
                        (&mut outcome.downgraded, format!("downgraded {} {} to {}", s.name, s.from, s.to))
                    }
                    Action::Remove => (&mut outcome.removed, format!("removed {} {}", s.name, s.from)),
                };
                *count += 1;
                let from = if s.repository.is_empty() { String::new() } else { format!(" (from {})", s.repository) };
                println!("{said}{from}");
            }
            // The cache keeps nothing installed.
            for path in &paths {
                let _ = std::fs::remove_file(path);
            }
            Done::Applied { result: Ok(outcome), db: Some(after) }
        }
        Err(e) => {
            println!("the change was not made: {e}");
            Done::Applied { result: Err(change_failure(&e)), db: None }
        }
    }
}

/// Checks installed files against what was installed.
fn verify(name: Option<&str>, db: &Database, layout: &Layout) -> Result<Report, Failure> {
    let mut files = StdFiles { anchor: state::STATE.into() };
    let problems = transaction::verify(&mut files, layout, db, name).map_err(|e| change_failure(&e))?;
    let checked: Vec<_> = db.packages.iter().filter(|p| name.is_none_or(|n| p.manifest.name.as_str() == n)).collect();
    let more = problems.len().saturating_sub(REPORT_PROBLEMS) as u32;
    let problems = problems
        .into_iter()
        .take(REPORT_PROBLEMS)
        .map(|p| match p {
            transaction::Problem::Missing { package, path } => Problem { package, path, what: String::from("missing") },
            transaction::Problem::Changed { package, path } => {
                Problem { package, path, what: String::from("not what was installed") }
            }
            transaction::Problem::Stray(path) => {
                Problem { package: String::new(), path, what: String::from("belongs to no package") }
            }
        })
        .collect();
    Ok(Report {
        packages: checked.len() as u32,
        files: checked.iter().map(|p| p.files.len() as u32).sum(),
        problems,
        more,
    })
}
