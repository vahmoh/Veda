// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `packages` — the packages service: installs, upgrades and removes
//! Veda's packages, from repositories whose keys sign their indexes, and
//! alone changes what `/home/packages` holds (the VFS lets no one else).
//! [Packages](../../../../documentation/PACKAGES.md) describes the design;
//! `packages` (`implementation/libraries/packages`) holds what it decides,
//! tested on the host.
//!
//! * **At its start** it recovers from a change cut short (power lost, the
//!   service ended): rolls it back or finishes it
//!   ([`packages::transaction`]); then reads what is installed, the
//!   system image's repositories and the user's, and the index of each,
//!   checking every one again.
//! * **Who may change what.** Anyone may ask what is installed and offered
//!   (`proto::packages`); only the Terminal and the programs it runs (the
//!   `pkg` command), Settings and the programs the kernel command line
//!   asks for may install, remove, update, and change the repositories, as
//!   the registry identifies them. No package runs anything as it installs:
//!   what the service writes is files, checked against their repository's
//!   signed index.
//! * **Answering in time.** Updating the repositories and carrying a
//!   change out take time, so the worker ([`worker`]) does them on a thread
//!   of its own, one at a time, and the service answers them once done,
//!   answering every other call meanwhile.
//! * **Bounds.** At most [`MAX_CLIENTS`] connections, [`MAX_CLIENTS_EACH`]
//!   of one process; lists come in pages, and plans and reports list a
//!   bounded number of entries.

mod fetch;
mod state;
mod worker;

use std::sync::Arc;

use abi::signals;
use ipc::{Decode, Encode, WaitSet};
use proto::init::ClientIdentity;
use proto::packages::{
    self as protocol, Failure, Outcome, PackageError, PackageInfo, PackageSummary, Plan, Refreshed, Report,
    RepositoryChange, RepositoryInfo, Request, Scope, Status, packages,
};
use rt::object::{Channel, Message};
use rt::println;

use crate::state::{State, failure};
use crate::worker::{Done, Job, Worker};

/// Connections served at once, and of one process.
const MAX_CLIENTS: usize = 32;
const MAX_CLIENTS_EACH: usize = 8;

/// The applications that change the installed software: the Terminal
/// (whose programs speak for it) and Settings.
const MANAGERS: [&str; 2] = ["terminal", "settings"];

/// The methods answered once the worker is done.
const UPDATE: u32 = 4;
const APPLY: u32 = 8;
const VERIFY: u32 = 10;

const LISTENER_KEY: u64 = u64::MAX;
const WORKER_KEY: u64 = u64::MAX - 1;

/// Whether `who` may change packages and repositories.
fn manages(who: &ClientIdentity) -> bool {
    who.requested || (who.app && !who.service && MANAGERS.contains(&who.name.as_str()))
}

struct Client {
    id: u64,
    channel: Channel,
    who: ClientIdentity,
}

/// The call the worker's job answers: whose, its transaction and method.
struct Waiting {
    client: u64,
    txid: u32,
    ordinal: u32,
}

struct Service {
    state: State,
    worker: Option<Worker>,
    listener: Option<Channel>,
    clients: Vec<Client>,
    next_client: u64,
    /// The job under way, and the call it answers.
    waiting: Option<Waiting>,
}

/// Sends the answer to the call `txid` of method `ordinal`.
fn answer<T: Encode>(channel: &Channel, ordinal: u32, txid: u32, result: T) {
    let mut e = ipc::Encoder::with_header(ordinal, txid, ipc::FLAG_RESPONSE);
    result.encode(&mut e);
    let _ = e.send(channel);
}

impl Service {
    fn busy(&self) -> bool {
        self.waiting.is_some()
    }

    /// Accepts connections, as many as allowed.
    fn accept(&mut self) {
        let Some(l) = &self.listener else { return };
        while let Some((channel, who)) = proto::accept_with_identity(l) {
            let own = self.clients.iter().filter(|c| c.who.koid == who.koid).count();
            if self.clients.len() >= MAX_CLIENTS || own >= MAX_CLIENTS_EACH {
                println!("{} has too many connections; one refused", who.name);
                continue;
            }
            self.next_client += 1;
            self.clients.push(Client { id: self.next_client, channel, who });
        }
    }

    /// Answers the requests waiting on connection `id`, and forgets it if
    /// it closed.
    fn serve(&mut self, id: u64) {
        let Some(i) = self.clients.iter().position(|c| c.id == id) else { return };
        let client = self.clients.swap_remove(i);
        let alive = loop {
            match client.channel.read() {
                Ok(msg) => self.request(&client, msg),
                Err(abi::Error::ShouldWait) => break true,
                Err(_) => break false,
            }
        };
        if alive {
            self.clients.push(client);
        }
    }

    /// Answers one request.
    fn request(&mut self, client: &Client, mut msg: Message) {
        let Ok(header) = ipc::Header::parse(&msg.bytes) else { return };
        if matches!(header.ordinal, UPDATE | APPLY | VERIFY) {
            let refused = |why| {
                let _ = ipc::refuse(&client.channel, header, why);
            };
            let Ok((_, mut d)) = ipc::open(&mut msg) else { return refused(ipc::Refusal::Malformed) };
            let started = match header.ordinal {
                UPDATE => Option::<Channel>::decode(&mut d)
                    .and_then(|p| d.finish().map(|()| p))
                    .map(|progress| self.update(client, progress).map_err(Encoded::Refreshed)),
                APPLY => (|| -> Result<_, ipc::DecodeError> {
                    let request = Request::decode(&mut d)?;
                    let digest = ipc::Bytes::decode(&mut d)?;
                    let progress = Option::<Channel>::decode(&mut d)?;
                    d.finish()?;
                    Ok((request, digest, progress))
                })()
                .map(|(request, digest, progress)| {
                    self.apply(client, request, &digest.0, progress).map_err(Encoded::Outcome)
                }),
                _ => String::decode(&mut d)
                    .and_then(|n| d.finish().map(|()| n))
                    .map(|name| self.verify(client, name).map_err(Encoded::Report)),
            };
            match started {
                Err(_) => refused(ipc::Refusal::Malformed),
                Ok(Ok(())) => {
                    self.waiting = Some(Waiting { client: client.id, txid: header.txid, ordinal: header.ordinal })
                }
                Ok(Err(refusal)) => refusal.send(&client.channel, header.ordinal, header.txid),
            }
            return;
        }
        let manager = manages(&client.who);
        match packages::dispatch(&mut Call { service: self, manager }, msg) {
            Ok(reply) => {
                let _ = reply.send(&client.channel);
            }
            Err(e) => println!("bad request: {e}"),
        }
    }

    /// Refuses a change by `client` that it may not make, or that cannot be
    /// made now.
    fn may_change(&self, client: &Client) -> Result<(), Failure> {
        if !manages(&client.who) {
            println!("{} may not change the installed software", client.who.name);
            return Err(failure(PackageError::Denied, ""));
        }
        if self.busy() {
            return Err(failure(PackageError::Busy, ""));
        }
        Ok(())
    }

    fn update(&mut self, client: &Client, progress: Option<Channel>) -> Result<(), Result<Vec<Refreshed>, Failure>> {
        self.may_change(client).map_err(Err)?;
        let worker = self.worker.as_ref().ok_or_else(|| Err(failure(PackageError::Unavailable, "")))?;
        worker.give(Job::Update {
            repositories: self.state.repositories.clone(),
            previous: self.state.indexes.clone(),
            progress,
        });
        Ok(())
    }

    fn apply(
        &mut self,
        client: &Client,
        request: Request,
        digest: &[u8],
        progress: Option<Channel>,
    ) -> Result<(), Result<Outcome, Failure>> {
        self.may_change(client).map_err(Err)?;
        let resolved = self.state.resolve(&request).map_err(Err)?;
        if !State::agreed(&resolved, digest) {
            return Err(Err(failure(PackageError::PlanChanged, "")));
        }
        let worker = self.worker.as_ref().ok_or_else(|| Err(failure(PackageError::Unavailable, "")))?;
        worker.give(Job::Apply {
            resolved: Box::new(resolved),
            db: self.state.db.clone(),
            layout: self.state.layout.clone(),
            progress,
        });
        Ok(())
    }

    fn verify(&mut self, client: &Client, name: String) -> Result<(), Result<Report, Failure>> {
        self.may_change(client).map_err(Err)?;
        if !name.is_empty() && self.state.db.get(&name).is_none() {
            return Err(Err(failure(PackageError::NotFound, format!("{name} is not installed"))));
        }
        let worker = self.worker.as_ref().ok_or_else(|| Err(failure(PackageError::Unavailable, "")))?;
        let name = if name.is_empty() { None } else { Some(name) };
        worker.give(Job::Verify { name, db: self.state.db.clone(), layout: self.state.layout.clone() });
        Ok(())
    }

    /// Takes what the worker did, and answers the call that asked for it.
    /// The caller's answer goes to it if it is still connected; what the
    /// job found is taken either way.
    fn take_done(&mut self) {
        let Some(worker) = &self.worker else { return };
        for done in worker.take() {
            let waiting = self.waiting.take();
            let caller = waiting.as_ref().and_then(|w| {
                let client = self.clients.iter().find(|c| c.id == w.client)?;
                Some((&client.channel, w.ordinal, w.txid))
            });
            match done {
                Done::Updated { refreshed, taken } => {
                    self.state.take_updates(taken);
                    if let Some((ch, ordinal, txid)) = caller {
                        answer(ch, ordinal, txid, Ok::<_, Failure>(refreshed));
                    }
                }
                Done::Applied { result, db } => {
                    if let Some(db) = db {
                        self.state.db = Arc::new(db);
                    }
                    if let Some((ch, ordinal, txid)) = caller {
                        answer(ch, ordinal, txid, result);
                    }
                }
                Done::Verified(result) => {
                    if let Some((ch, ordinal, txid)) = caller {
                        answer(ch, ordinal, txid, result);
                    }
                }
            }
        }
    }

    fn run(&mut self) -> ! {
        let mut ws = WaitSet::new();
        loop {
            ws.clear();
            if let Some(l) = &self.listener {
                ws.add(l.raw(), signals::READABLE | signals::PEER_CLOSED, LISTENER_KEY);
            }
            if let Some(w) = &self.worker {
                ws.add(w.event_handle(), signals::SIGNALED, WORKER_KEY);
            }
            for c in &self.clients {
                ws.add(c.channel.raw(), signals::READABLE | signals::PEER_CLOSED, c.id);
            }
            let ready = match ws.wait(abi::DEADLINE_INFINITE) {
                Ok(r) => r,
                Err(e) => {
                    println!("wait failed: {e}");
                    continue;
                }
            };
            for (key, _) in ready {
                match key {
                    LISTENER_KEY => self.accept(),
                    WORKER_KEY => self.take_done(),
                    id => self.serve(id),
                }
            }
        }
    }
}

/// An answer refused before the worker began, encoded as its method's.
enum Encoded {
    Refreshed(Result<Vec<Refreshed>, Failure>),
    Outcome(Result<Outcome, Failure>),
    Report(Result<Report, Failure>),
}

impl Encoded {
    fn send(self, channel: &Channel, ordinal: u32, txid: u32) {
        match self {
            Encoded::Refreshed(r) => answer(channel, ordinal, txid, r),
            Encoded::Outcome(r) => answer(channel, ordinal, txid, r),
            Encoded::Report(r) => answer(channel, ordinal, txid, r),
        }
    }
}

/// A request answered at once, by a caller who may or may not change
/// packages.
struct Call<'a> {
    service: &'a mut Service,
    manager: bool,
}

impl packages::Server for Call<'_> {
    fn status(&mut self) -> Status {
        self.service.state.status(self.service.busy())
    }

    fn repositories(&mut self) -> Vec<RepositoryInfo> {
        self.service.state.repository_infos()
    }

    fn configure(&mut self, change: RepositoryChange) -> Result<(), Failure> {
        if !self.manager {
            return Err(failure(PackageError::Denied, ""));
        }
        if self.service.busy() {
            return Err(failure(PackageError::Busy, ""));
        }
        let said = match &change {
            RepositoryChange::Add { name, url, .. } => format!("added the repository {name} ({url})"),
            RepositoryChange::Remove { name } => format!("removed the repository {name}"),
            RepositoryChange::Enable { name, enabled: true } => format!("enabled the repository {name}"),
            RepositoryChange::Enable { name, enabled: false } => format!("disabled the repository {name}"),
        };
        self.service.state.configure(change)?;
        println!("{said}");
        Ok(())
    }

    /// Answered by the worker (`Service::update`).
    fn update(&mut self, _progress: Option<Channel>) -> Result<Vec<Refreshed>, Failure> {
        Err(failure(PackageError::Unavailable, ""))
    }

    fn find(&mut self, query: String, scope: Scope, after: String, max: u32) -> Result<Vec<PackageSummary>, Failure> {
        Ok(self.service.state.find(&query, scope, &after, max))
    }

    fn describe(&mut self, name: String) -> Result<PackageInfo, Failure> {
        self.service.state.describe(&name)
    }

    fn plan(&mut self, request: Request) -> Result<Plan, Failure> {
        self.service.state.resolve(&request).map(|r| r.plan)
    }

    /// Answered by the worker (`Service::apply`).
    fn apply(&mut self, _: Request, _: ipc::Bytes, _: Option<Channel>) -> Result<Outcome, Failure> {
        Err(failure(PackageError::Unavailable, ""))
    }

    fn files(&mut self, name: String, after: String, max: u32) -> Result<Vec<String>, Failure> {
        self.service.state.files(&name, &after, max)
    }

    /// Answered by the worker (`Service::verify`).
    fn verify(&mut self, _name: String) -> Result<Report, Failure> {
        Err(failure(PackageError::Unavailable, ""))
    }

    fn clean(&mut self) -> Result<u64, Failure> {
        if !self.manager {
            return Err(failure(PackageError::Denied, ""));
        }
        if self.service.busy() {
            return Err(failure(PackageError::Busy, ""));
        }
        let mut freed = 0;
        for entry in std::fs::read_dir(format!("{}/cache", state::STATE)).into_iter().flatten().flatten() {
            let size = entry.metadata().map_or(0, |m| m.len());
            if std::fs::remove_file(entry.path()).is_ok() {
                freed += size;
            }
        }
        println!("the downloaded package files kept are removed ({freed} bytes)");
        Ok(freed)
    }
}

fn main() {
    let state = State::load();
    let worker = Worker::start();
    if worker.is_none() {
        println!("cannot start the worker: nothing can be installed");
    }
    let listener = proto::register(protocol::packages::NAME).ok();
    if listener.is_none() {
        println!("cannot register the packages service");
    }
    let names: Vec<&str> = state.repositories.iter().map(|r| r.name.as_str()).collect();
    println!(
        "ready ({} installed; repositories: {})",
        state.db.packages.len(),
        if names.is_empty() { String::from("none") } else { names.join(", ") }
    );
    let mut service = Service { state, worker, listener, clients: Vec::new(), next_client: 0, waiting: None };
    service.run()
}
