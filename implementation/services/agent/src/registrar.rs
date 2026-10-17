// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Applications' registrations.
//!
//! Any program may offer the agent its abilities (`register_app`): it hands
//! over a channel on which it serves `agentapp`. Before the agent can use
//! it, the application is asked what it offers (`describe`), which it may
//! answer slowly or never; so a thread of its own asks, off the path of
//! function calls, and hands the worker the application's link once it
//! answered ([`crate::worker::Links`]). What it said is kept for the
//! agent's instructions (bounded first: `agent::tools::sanitize`).
//!
//! A process registers once: a new registration of the same process
//! replaces its previous one (in the worker). One registering again and
//! again is refused for a while ([`RegistrationLimit`]), and so are
//! registrations beyond those waiting to be asked.

use std::collections::VecDeque;
use std::sync::Arc;

use agent::policy::RegistrationLimit;
use proto::agent::{AgentError, agentapp};
use proto::init::ClientIdentity;
use rt::object::Channel;
use rt::sync::{Condvar, Mutex};

use crate::shared::Shared;
use crate::worker::{self, Links, NewLink};

/// Registrations of one process allowed in a minute.
const PER_MINUTE: u32 = 4;
const MINUTE_NS: u64 = 60_000_000_000;
/// Registrations waiting to be asked what they offer.
const MAX_WAITING: usize = 16;

struct State {
    waiting: VecDeque<(ClientIdentity, Channel)>,
    limit: RegistrationLimit,
}

struct Inner {
    state: Mutex<State>,
    wake: Condvar,
}

/// Takes registrations (from the main loop and Settings' thread).
#[derive(Clone)]
pub struct Registrar(Arc<Inner>);

impl Registrar {
    /// Starts the thread that asks registered applications what they offer.
    pub fn start(shared: Arc<Mutex<Shared>>, links: Links) -> Option<Registrar> {
        let inner = Arc::new(Inner {
            state: Mutex::new(State { waiting: VecDeque::new(), limit: RegistrationLimit::new(PER_MINUTE, MINUTE_NS) }),
            wake: Condvar::new(),
        });
        let r = Registrar(inner.clone());
        rt::thread::Builder::new()
            .name("registrar")
            .spawn(move || describe_registrations(&inner, &shared, &links))
            .ok()?;
        Some(r)
    }

    /// Takes a registration of `who`, the program serving `agentapp` on
    /// `channel`; refuses one that comes too often, or too many at once.
    pub fn register(&self, who: &ClientIdentity, channel: Channel) -> Result<(), AgentError> {
        if who.name.is_empty() {
            return Err(AgentError::Denied);
        }
        let mut s = self.0.state.lock();
        if s.waiting.len() >= MAX_WAITING || !s.limit.admit(who.koid, rt::time::now_ns()) {
            println!("{} registers too often; refused", who.name);
            return Err(AgentError::Busy);
        }
        s.waiting.push_back((who.clone(), channel));
        drop(s);
        self.0.wake.notify_one();
        Ok(())
    }
}

/// The registrar's thread: asks each registered application what it
/// offers, keeps it, and hands the worker its link.
fn describe_registrations(inner: &Inner, shared: &Mutex<Shared>, links: &Links) {
    loop {
        let (who, channel) = {
            let mut s = inner.state.lock();
            loop {
                if let Some(r) = s.waiting.pop_front() {
                    break r;
                }
                s = inner.wake.wait(s);
            }
        };
        let client = agentapp::Client::new(channel);
        client.set_timeout(worker::APP_TIMEOUT_NS);
        match worker::describe(&client) {
            Ok(info) => {
                println!("{} offers {} action(s)", who.name, info.actions.len());
                shared.lock().set_app(&who.name, info);
                links.push(NewLink { id: who.name, koid: who.koid, client });
            }
            Err(e) => println!("{} did not describe itself: {:?}", who.name, e),
        }
    }
}
