// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The worker thread: runs what the agent decided to do.
//!
//! Function calls from the language model arrive as [`Job`]s. The worker
//! classifies each call (its [`Risk`], from the system function or from
//! the application's description of the action) and either runs it or
//! hands it back as an [`Outcome::Approval`] — nothing that needs the
//! user's consent runs before the user gave it. A call is made definite
//! before it is classified, so that what the user allows is what runs: a
//! request to forget names its memories by id (and the approval lists
//! them), a file's destination is worked out as it is then used.
//!
//! Everything slow lives here, off the conversation's thread: calls into
//! applications (with timeouts, as an application may hang), the file
//! system, the launcher, the shell and the network service. Each system
//! function has a module of its own: [`status`] (`get_status`), [`apps`]
//! (`list_apps`, `open_app`, `app_actions`, `use_app`, `read_app`),
//! [`windows`] (`window`), [`files`], [`volume`], [`wifi`],
//! [`wallpaper`], [`reminders`] (`notify`, `timer`), [`memory`] and
//! [`system`] (`system`, `tasks`).
//!
//! The user's files are reached through a connection to the file system of
//! the worker's own ([`user_files`]), never through the one of the agent's
//! private store.

mod apps;
mod files;
mod memory;
mod reminders;
mod status;
mod system;
mod volume;
mod wallpaper;
mod wifi;
mod windows;

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use serde_json::Value;
use vabi::RawHandle;
use vagent::policy;
use vagent::tools::{self, Risk, names};
use vproto::display::display;
use vproto::init::launcher;
use vproto::shell::shell;
use vrt::object::{Channel, Event};
use vrt::sync::{Condvar, Mutex};
use vrt::time::DateTime;

use crate::shared::Shared;

pub use apps::{APP_TIMEOUT_NS, NewLink, describe};
pub use windows::screen_summary;

/// How long the window system and the audio service may take to answer
/// (the conversation's own thread asks the window system what is on the
/// screen) ...
const SERVICE_TIMEOUT_NS: u64 = 2_000_000_000;
/// ... and the desktop shell, which may load a picture first (a new
/// wallpaper).
const DESKTOP_TIMEOUT_NS: u64 = 10_000_000_000;

/// A function call from the language model (`approved` once the user
/// allowed it).
pub struct Job {
    pub id: String,
    pub name: String,
    pub args: Value,
    pub approved: bool,
}

/// An action waiting for the user's consent.
#[derive(Debug, Clone)]
pub struct Pending {
    pub name: String,
    /// The call as it will run once allowed (made definite: see the module
    /// documentation).
    pub args: Value,
    /// Who acts ("Files", an application's name).
    pub app: String,
    pub action: String,
    pub detail: String,
    pub risk: Risk,
    /// Permission key for "always allow".
    pub key: String,
}

/// What became of a call.
pub enum Outcome {
    /// The result for the language model (JSON text).
    Result(String),
    /// The call needs the user's approval first.
    Approval(Pending),
    /// The agent wants to end the conversation.
    End,
    /// The agent locks the screen. The main loop does, then gives the
    /// conversation the result, which reaches it before the lock ends it.
    Lock,
}

/// A finished call.
pub struct Done {
    pub id: String,
    pub name: String,
    pub args: Value,
    pub outcome: Outcome,
}

struct Queue {
    jobs: Mutex<VecDeque<Job>>,
    wake: Condvar,
    done: Mutex<VecDeque<Done>>,
    /// Applications that registered and described themselves, for the
    /// worker to take.
    links: Mutex<Vec<NewLink>>,
}

/// The main loop's handle on the worker.
pub struct Worker {
    q: Arc<Queue>,
    event: Event,
}

impl Worker {
    /// Starts the worker thread.
    pub fn start(shared: Arc<Mutex<Shared>>) -> Option<Worker> {
        let event = Event::create().ok()?;
        let signal = Event::from_handle(event.0.duplicate(None).ok()?);
        let q = Arc::new(Queue {
            jobs: Mutex::new(VecDeque::new()),
            wake: Condvar::new(),
            done: Mutex::new(VecDeque::new()),
            links: Mutex::new(Vec::new()),
        });
        let wq = q.clone();
        vrt::thread::Builder::new()
            .name("actions")
            .spawn(move || {
                let mut ctx = Ctx { shared, q: wq, signal, apps: BTreeMap::new(), fs: user_files() };
                ctx.run();
            })
            .ok()?;
        Some(Worker { q, event })
    }

    /// Queues a call.
    pub fn submit(&self, job: Job) {
        self.q.jobs.lock().push_back(job);
        self.q.wake.notify_one();
    }

    /// Takes the calls with these ids out of the queue before they start;
    /// returns the ids taken (the others had started, or are not known).
    pub fn cancel(&self, ids: &[String]) -> Vec<String> {
        let mut taken = Vec::new();
        self.q.jobs.lock().retain(|j| {
            let cancelled = ids.contains(&j.id);
            if cancelled {
                taken.push(j.id.clone());
            }
            !cancelled
        });
        taken
    }

    /// Finished work (clears the event).
    pub fn take(&self) -> Vec<Done> {
        let _ = self.event.clear();
        self.q.done.lock().drain(..).collect()
    }

    /// Signalled when work is done.
    pub fn handle(&self) -> RawHandle {
        self.event.raw()
    }

    /// Where applications that registered go once they described
    /// themselves.
    pub fn links(&self) -> Links {
        Links(self.q.clone())
    }
}

/// Hands applications that registered and described themselves to the
/// worker (from the registrar's thread).
#[derive(Clone)]
pub struct Links(Arc<Queue>);

impl Links {
    /// Hands over an application's link.
    pub fn push(&self, link: NewLink) {
        self.0.links.lock().push(link);
    }
}

struct Ctx {
    shared: Arc<Mutex<Shared>>,
    q: Arc<Queue>,
    signal: Event,
    /// Registered applications by id: every running instance, the newest
    /// last, which the agent works with (see [`Ctx::link`]).
    apps: BTreeMap<String, Vec<apps::AppLink>>,
    /// The user's files (see [`user_files`]).
    fs: vfiles::Fs,
}

/// The worker's connection to the file system, for the user's files. It is
/// apart from the private store's ([`crate::store`]): the paths the worker
/// acts on come from the language model, from documents and from the
/// Trash's records, and although every one is checked
/// ([`vagent::files`]), this connection needs no access to private
/// directories at all, so it gives that access up, for good: the file
/// system then treats it as an application's. A connection that could not
/// give it up is not used: file actions then fail.
fn user_files() -> vfiles::Fs {
    let fs = vfiles::Fs::connect();
    match fs.renounce_private() {
        Ok(()) => fs,
        Err(e) => {
            println!("the file system did not take the worker's private access away ({e}): no file actions");
            vfiles::Fs::unavailable()
        }
    }
}

/// A service's client.
fn connect<T>(name: &str, make: fn(Channel) -> T) -> Option<T> {
    vproto::connect(name).ok().map(make)
}

/// The window system, which must answer in time.
fn window_system() -> Option<display::Client> {
    let d = connect(display::NAME, display::Client::new)?;
    d.set_timeout(SERVICE_TIMEOUT_NS);
    Some(d)
}

/// The desktop shell, which must answer in time.
fn desktop() -> Option<shell::Client> {
    let s = connect(shell::NAME, shell::Client::new)?;
    s.set_timeout(DESKTOP_TIMEOUT_NS);
    Some(s)
}

/// The launcher (in init).
fn launcher() -> Option<launcher::Client> {
    connect(launcher::NAME, launcher::Client::new)
}

/// How a path reads aloud ("~/Documents/notes.txt").
fn show(path: &str) -> String {
    vfiles::path::display_path(path)
}

/// Local date and time as people say it.
pub fn now_text() -> String {
    let t = DateTime::now();
    format!("{} {} {} {}, {:02}:{:02}", t.weekday_name(), t.day, t.month_name(), t.year, t.hour, t.minute)
}

/// The installed applications (for the prompt).
pub fn installed_apps() -> Vec<vproto::init::AppInfo> {
    launcher().and_then(|l| l.apps().ok()).unwrap_or_default()
}

impl Ctx {
    fn run(&mut self) {
        loop {
            let job = {
                let mut jobs = self.q.jobs.lock();
                loop {
                    if let Some(j) = jobs.pop_front() {
                        break j;
                    }
                    jobs = self.q.wake.wait(jobs);
                }
            };
            let outcome = self.call(&job.name, &job.args, job.approved);
            self.q.done.lock().push_back(Done { id: job.id, name: job.name, args: job.args, outcome });
            let _ = self.signal.signal();
        }
    }

    /// Runs (or classifies) one call.
    fn call(&mut self, name: &str, args: &Value, approved: bool) -> Outcome {
        self.take_links();
        if name == names::USE_APP {
            return self.use_app(args, approved);
        }
        if !tools::is_system(name) {
            return Outcome::Result(tools::error(&format!("there is no function called {name}")));
        }
        let pinned = match self.pin(name, args, approved) {
            Ok(p) => p,
            Err(e) => return Outcome::Result(tools::error(&e)),
        };
        let args = pinned.as_ref().unwrap_or(args);
        let fs = &self.fs;
        let exists = |p: &str| vagent::files::resolve(p).map(|p| fs.exists(&p)).unwrap_or(false);
        let risk = tools::risk(name, args, &exists);
        let key = policy::key(policy::SYSTEM, &format!("{name}.{}", args["operation"].as_str().unwrap_or("")));
        if !approved && self.needs_approval(&key, risk) {
            let (action, detail) = tools::describe(name, args);
            let app = match name {
                names::FILES => "Files",
                names::WIFI => "Wi-Fi",
                names::MEMORY => "Memory",
                _ => "System",
            };
            return Outcome::Approval(Pending {
                name: name.into(),
                args: args.clone(),
                app: app.into(),
                action,
                detail,
                risk,
                key,
            });
        }
        let result = match name {
            names::GET_STATUS => Ok(self.status()),
            names::LIST_APPS => Ok(self.list_apps()),
            names::OPEN_APP => self.open_app(args),
            names::APP_ACTIONS => self.app_actions(args),
            names::READ_APP => self.read_app(args),
            names::WINDOW => self.window(args),
            names::FILES => self.files(args),
            names::VOLUME => self.volume(args),
            names::WIFI => self.wifi(args),
            names::WALLPAPER => self.wallpaper(args),
            names::NOTIFY => self.notify(args),
            names::TIMER => self.timer(args),
            names::MEMORY => self.memory(args),
            names::SYSTEM if args["operation"].as_str() == Some("lock") => return Outcome::Lock,
            names::SYSTEM => self.system(args),
            names::TASKS => self.tasks(args),
            names::END_CONVERSATION => return Outcome::End,
            _ => Err("not implemented".into()),
        };
        Outcome::Result(match result {
            Ok(v) => tools::ok(v),
            Err(e) => tools::error(&e),
        })
    }

    /// The call made definite before it is classified, if it needs to be: a
    /// request to forget names its memories by id. An allowed call was made
    /// definite when it was asked about.
    fn pin(&self, name: &str, args: &Value, approved: bool) -> Result<Option<Value>, String> {
        if name == names::MEMORY && args["operation"].as_str() == Some("forget") && !approved {
            return self.shared.lock().memory.pin_forget(args).map(Some).map_err(String::from);
        }
        Ok(None)
    }

    fn needs_approval(&self, key: &str, risk: Risk) -> bool {
        self.shared.lock().permissions.needs_approval(key, risk)
    }
}
