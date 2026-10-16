// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! File operations that can take long — copying, moving, moving to the
//! Trash and back, deleting, emptying the Trash, measuring a folder — run
//! on a thread of their own, one after another in the order they were
//! asked for, so that the window stays responsive: it shows what is being
//! done, how far it is, and can stop it.
//!
//! The thread has a connection of its own to the file system. It works an
//! item at a time (each item of the selection), saying which it is on and
//! how many are done, and looks at the cancel flag between items. Within an
//! item, `vfiles`' recursive walks (`Fs::copy`, `Fs::move_to`,
//! `Fs::remove_all`) run to their end; once they report progress and take a
//! cancel hook, [`Context::step`] is where that goes.

use core::sync::atomic::{AtomicBool, Ordering};
use std::collections::VecDeque;
use std::sync::Arc;

use vabi::RawHandle;
use vfiles::fs::Error;
use vfiles::path::{file_name, is_within, join, parent};
use vfiles::{Fs, trash};
use vrt::object::Event;
use vrt::sync::{Condvar, Mutex};

/// Something to do with files.
#[derive(Debug, Clone)]
pub enum Job {
    /// Copies (or moves, without `copy`) items into the folder `dir`, under
    /// free names there. An item leaving the Trash gets back the name it
    /// was deleted with, and what the Trash knew of it goes.
    Transfer { paths: Vec<String>, dir: String, copy: bool },
    /// Moves items to the Trash.
    Trash { paths: Vec<String> },
    /// Puts items of the Trash (by their names there) back where they were
    /// deleted from; with an origin, only if the item still came from
    /// there (undoing a move to the Trash).
    Restore { items: Vec<(String, Option<String>)> },
    /// Deletes items for good (items of the Trash with what it knows of
    /// them).
    Delete { paths: Vec<String> },
    /// Deletes everything in the Trash.
    EmptyTrash,
    /// Counts the files and folders in a folder and their bytes.
    Measure { path: String },
}

impl Job {
    /// What it does, as the window says it ("Copying").
    pub fn verb(&self) -> &'static str {
        match self {
            Job::Transfer { copy: true, .. } => "Copying",
            Job::Transfer { copy: false, .. } => "Moving",
            Job::Trash { .. } => "Moving to the Trash",
            Job::Restore { .. } => "Restoring",
            Job::Delete { .. } => "Deleting",
            Job::EmptyTrash => "Emptying the Trash",
            Job::Measure { .. } => "Measuring",
        }
    }
}

/// How far the job being done is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    pub id: u64,
    pub verb: &'static str,
    /// Items done, of `total`.
    pub done: usize,
    pub total: usize,
    /// The item being worked on (a name).
    pub current: String,
    /// Jobs waiting after it.
    pub queued: usize,
    /// It was asked to stop.
    pub cancelling: bool,
}

/// What a job did.
#[derive(Debug, Clone)]
pub enum Outcome {
    /// The names the items have in `dir` now, and the items that could not
    /// be copied or moved (by name).
    Transferred {
        dir: String,
        copy: bool,
        arrived: Vec<String>,
        failed: Vec<(String, Error)>,
    },
    /// Each item's name in the Trash and where it was; those for which
    /// there is no room in the Trash; the others that failed.
    Trashed {
        moved: Vec<(String, String)>,
        no_room: Vec<String>,
        failed: Vec<(String, Error)>,
    },
    /// Where the items are now.
    Restored {
        restored: Vec<String>,
        failed: Vec<(String, Error)>,
    },
    Deleted {
        paths: Vec<String>,
        failed: Vec<(String, Error)>,
    },
    Emptied(Result<usize, Error>),
    Measured {
        path: String,
        files: u64,
        folders: u64,
        bytes: u64,
    },
}

/// A job done (or stopped: `cancelled`, with what it did until then).
#[derive(Debug, Clone)]
pub struct Finished {
    pub id: u64,
    pub outcome: Outcome,
    pub cancelled: bool,
}

struct Shared {
    queue: Mutex<VecDeque<(u64, Job)>>,
    wake: Condvar,
    progress: Mutex<Option<Progress>>,
    finished: Mutex<Vec<Finished>>,
    cancel: AtomicBool,
}

/// The thread that does the file operations, and what it reports.
pub struct Worker {
    shared: Arc<Shared>,
    /// Signalled when a job progresses or finishes.
    event: Event,
    next_id: u64,
    /// Without a thread, jobs are done when they are given (the window
    /// waits).
    inline: Option<Fs>,
}

impl Worker {
    /// Starts the thread (or, without one, does jobs at once).
    pub fn start() -> Option<Worker> {
        let event = Event::create().ok()?;
        let shared = Arc::new(Shared {
            queue: Mutex::new(VecDeque::new()),
            wake: Condvar::new(),
            progress: Mutex::new(None),
            finished: Mutex::new(Vec::new()),
            cancel: AtomicBool::new(false),
        });
        let theirs = Event::from_handle(event.0.duplicate(None).ok()?);
        let worker = shared.clone();
        let thread = vrt::thread::Builder::new()
            .name("file-operations")
            .stack_size(256 * 1024)
            .spawn(move || work(&worker, &theirs));
        let inline = thread.is_err().then(Fs::connect);
        Some(Worker { shared, event, next_id: 1, inline })
    }

    /// Queues `job`; returns its number.
    pub fn submit(&mut self, job: Job) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        if let Some(fs) = &self.inline {
            let mut cx = Context { shared: &self.shared, event: &self.event, id, verb: job.verb() };
            let (outcome, cancelled) = cx.run(fs, job);
            *self.shared.progress.lock() = None;
            self.shared.finished.lock().push(Finished { id, outcome, cancelled });
            let _ = self.event.signal();
            return id;
        }
        self.shared.queue.lock().push_back((id, job));
        self.shared.wake.notify_one();
        id
    }

    /// How far the job being done is (`None` when idle).
    pub fn progress(&self) -> Option<Progress> {
        let mut p = self.shared.progress.lock().clone()?;
        p.queued = self.shared.queue.lock().len();
        Some(p)
    }

    /// Stops the job being done after the item it is on (what it did stays
    /// done).
    pub fn cancel(&self) {
        if let Some(p) = self.shared.progress.lock().as_mut() {
            p.cancelling = true;
            self.shared.cancel.store(true, Ordering::Release);
        }
    }

    /// Forgets the jobs that wait, if `which` says so; returns them.
    pub fn drop_queued(&self, which: impl Fn(&Job) -> bool) -> Vec<(u64, Job)> {
        let mut q = self.shared.queue.lock();
        let (gone, kept): (Vec<_>, Vec<_>) = q.drain(..).partition(|(_, j)| which(j));
        q.extend(kept);
        gone
    }

    /// The jobs finished since the last call.
    pub fn take_finished(&self) -> Vec<Finished> {
        let _ = self.event.clear();
        core::mem::take(&mut *self.shared.finished.lock())
    }

    /// Waits until job `id` has finished or `deadline` (monotonic ns)
    /// passes; returns whether it finished. What else finished meanwhile
    /// stays for [`Worker::take_finished`].
    pub fn wait_for(&self, id: u64, deadline: u64) -> bool {
        loop {
            if self.shared.finished.lock().iter().any(|f| f.id == id) {
                return true;
            }
            if vrt::time::now_ns() >= deadline {
                return false;
            }
            let _ = self.event.wait(vabi::signals::SIGNALED, deadline);
            let _ = self.event.clear();
        }
    }

    /// Signalled when a job progresses or finishes.
    pub fn event_handle(&self) -> RawHandle {
        self.event.raw()
    }
}

/// The thread: takes jobs in order and does them.
fn work(shared: &Shared, event: &Event) {
    let fs = Fs::connect();
    loop {
        let (id, job) = {
            let mut q = shared.queue.lock();
            loop {
                if let Some(j) = q.pop_front() {
                    break j;
                }
                q = shared.wake.wait(q);
            }
        };
        shared.cancel.store(false, Ordering::Release);
        let mut cx = Context { shared, event, id, verb: job.verb() };
        let (outcome, cancelled) = cx.run(&fs, job);
        *shared.progress.lock() = None;
        shared.finished.lock().push(Finished { id, outcome, cancelled });
        let _ = event.signal();
    }
}

/// A job being done: reports its progress and hears whether to stop.
struct Context<'a> {
    shared: &'a Shared,
    event: &'a Event,
    id: u64,
    verb: &'static str,
}

impl Context<'_> {
    /// Says that item `done` of `total` (`current`) is next; returns
    /// whether to go on. (Once `vfiles`' walks report their progress and
    /// can be stopped, they report through here too.)
    fn step(&mut self, done: usize, total: usize, current: &str) -> bool {
        if self.shared.cancel.load(Ordering::Acquire) {
            return false;
        }
        let p = Progress {
            id: self.id,
            verb: self.verb,
            done,
            total,
            current: current.to_string(),
            // Filled in by `Worker::progress`.
            queued: 0,
            cancelling: false,
        };
        *self.shared.progress.lock() = Some(p);
        let _ = self.event.signal();
        true
    }

    /// Does `job`; returns what it did and whether it was stopped.
    fn run(&mut self, fs: &Fs, job: Job) -> (Outcome, bool) {
        let mut stopped = false;
        let outcome = match job {
            Job::Transfer { paths, dir, copy } => {
                let (mut arrived, mut failed) = (Vec::new(), Vec::new());
                for (i, src) in paths.iter().enumerate() {
                    let name = file_name(src).to_string();
                    if !self.step(i, paths.len(), &name) {
                        stopped = true;
                        break;
                    }
                    if !copy && parent(src) == dir {
                        arrived.push(name);
                        continue;
                    }
                    if is_within(&dir, src) {
                        failed.push((name, Error::IntoItself));
                        continue;
                    }
                    match transfer(fs, src, &dir, copy) {
                        Ok(n) => arrived.push(n),
                        Err(e) => failed.push((original_name(fs, src), e)),
                    }
                }
                Outcome::Transferred { dir, copy, arrived, failed }
            }
            Job::Trash { paths } => {
                let (mut moved, mut no_room, mut failed) = (Vec::new(), Vec::new(), Vec::new());
                for (i, p) in paths.iter().enumerate() {
                    if !self.step(i, paths.len(), file_name(p)) {
                        stopped = true;
                        break;
                    }
                    match trash::put(fs, p) {
                        Ok(name) => moved.push((name, p.clone())),
                        Err(e) if e.is_no_space() => no_room.push(p.clone()),
                        Err(e) => failed.push((file_name(p).to_string(), e)),
                    }
                }
                Outcome::Trashed { moved, no_room, failed }
            }
            Job::Restore { items } => {
                let (mut restored, mut failed) = (Vec::new(), Vec::new());
                for (i, (name, origin)) in items.iter().enumerate() {
                    let shown = original_name(fs, &join(trash::FILES, name));
                    if !self.step(i, items.len(), &shown) {
                        stopped = true;
                        break;
                    }
                    // The name may belong to another item by now.
                    if let Some(o) = origin
                        && !trash::info(fs, name).is_some_and(|i| i.origin == *o)
                    {
                        continue;
                    }
                    match trash::restore(fs, name) {
                        Ok(path) => restored.push(path),
                        Err(e) => failed.push((shown, e)),
                    }
                }
                Outcome::Restored { restored, failed }
            }
            Job::Delete { paths } => {
                let mut failed = Vec::new();
                let mut done = Vec::new();
                for (i, p) in paths.iter().enumerate() {
                    let shown = original_name(fs, p);
                    if !self.step(i, paths.len(), &shown) {
                        stopped = true;
                        break;
                    }
                    let r = match trash::item_name(p) {
                        Some(name) if parent(p) == trash::FILES => trash::delete(fs, name),
                        _ => fs.remove_all(p),
                    };
                    match r {
                        Ok(()) => done.push(p.clone()),
                        Err(e) => failed.push((shown, e)),
                    }
                }
                Outcome::Deleted { paths: done, failed }
            }
            Job::EmptyTrash => {
                self.step(0, 1, "Trash");
                Outcome::Emptied(trash::empty(fs))
            }
            Job::Measure { path } => {
                let (mut files, mut folders, mut bytes) = (0, 0, 0);
                stopped = !self.measure(fs, &path, &mut files, &mut folders, &mut bytes, 0);
                Outcome::Measured { path, files, folders, bytes }
            }
        };
        (outcome, stopped)
    }

    /// Adds up what is in folder `dir`; returns `false` if stopped.
    fn measure(&mut self, fs: &Fs, dir: &str, files: &mut u64, folders: &mut u64, bytes: &mut u64, depth: u32) -> bool {
        if depth > 32 {
            return true;
        }
        if !self.step(*files as usize, 0, file_name(dir)) {
            return false;
        }
        for e in fs.read_dir(dir).unwrap_or_default() {
            if e.is_dir {
                *folders += 1;
                if !self.measure(fs, &join(dir, &e.name), files, folders, bytes, depth + 1) {
                    return false;
                }
            } else {
                *files += 1;
                *bytes += e.size;
            }
        }
        true
    }
}

/// The name of the item at `path` before it went to the Trash (its own
/// name for anything else).
pub fn original_name(fs: &Fs, path: &str) -> String {
    let name = file_name(path);
    if parent(path) != trash::FILES {
        return name.to_string();
    }
    trash::info(fs, name)
        .map(|i| file_name(&i.origin).to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| name.to_string())
}

/// Moves (or copies) `src` into the folder `dir` under a free name, and
/// returns that name. An item leaving the Trash gets back the name it was
/// deleted with, and what the Trash knew of it goes.
fn transfer(fs: &Fs, src: &str, dir: &str, copy: bool) -> Result<String, Error> {
    let base = original_name(fs, src);
    if !copy && parent(src) == trash::FILES {
        return trash::restore_to(fs, file_name(src), dir, &base).map(|p| file_name(&p).to_string());
    }
    let name = fs.unique_name(dir, &base);
    let dst = join(dir, &name);
    let r = if copy { fs.copy(src, &dst) } else { fs.move_to(src, &dst) };
    r.map(|()| name)
}
