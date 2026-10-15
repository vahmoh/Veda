// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `agent` — the voice agent that lives in Veda.
//!
//! A system service, started at boot and restarted if it fails. It is the
//! presence the user talks to: it listens (or sleeps until called), holds
//! conversations through the Deepgram Voice Agent API, speaks, and acts on
//! the system and its applications on the user's behalf.
//!
//! * **Conversations** ([`conversation`], over a [`session`]): waking (from
//!   the tray, a shortcut, its name, a due reminder) opens a Voice Agent
//!   WebSocket with the agent's character, what it knows and the functions
//!   it may call; the microphone streams to Deepgram and the voice comes
//!   back. When the user starts talking, the voice stops at once. A
//!   conversation ends after a quiet spell (or when the agent says
//!   goodbye), to save cost; the next one starts afresh, with how the last
//!   one ended in the instructions only for reference (asked about it, the
//!   agent knows). After an error the agent shows it for a moment, then
//!   listens for its name again.
//! * **Actions** ([`worker`]): function calls run on the worker thread.
//!   Anything above routine risk waits for the user's approval
//!   ([`approvals`]), which the shell asks for — in the agent's window
//!   when it is open, otherwise in a notification — and the outcome is told
//!   to the agent ([`vagent::notice`]). Timers wake it when they are due
//!   ([`timers`]).
//! * **Who may do what** is decided by the registry's identity of each
//!   connection ([`server`]): any program may register its abilities
//!   ([`registrar`]); only the shell attaches the interface and answers
//!   approvals; only Settings changes the configuration ([`admin`]).
//! * **State** ([`shared`]): settings, the key, memory, permissions and
//!   timers live in the agent's private directory ([`store`]).

#![no_std]
#![no_main]

extern crate alloc;

mod admin;
mod approvals;
mod conversation;
mod listen;
mod registrar;
mod server;
mod session;
mod shared;
mod store;
mod timers;
mod ui;
mod voice;
mod worker;

use alloc::collections::{BTreeMap, VecDeque};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;

use vabi::{WaitItem, signals};
use vagent::deepgram;
use vagent::gate::EchoGate;
use vproto::agent::{AgentEvent, AgentState, AgentStatus, agent};
use vrt::object::Channel;
use vrt::println;
use vrt::sync::Mutex;

use admin::SettingsServer;
use approvals::Approval;
use registrar::Registrar;
use server::Conn;
use session::{Connector, Session};
use shared::Shared;
use ui::Ui;
use voice::Voice;
use worker::Worker;

vrt::entry!(main);

/// How often the interface's live levels are refreshed.
const LIVE_PERIOD_NS: u64 = 33_000_000;

struct Agent {
    shared: Arc<Mutex<Shared>>,
    /// The status as published (for Settings' thread).
    published: Arc<Mutex<AgentStatus>>,
    state: AgentState,
    detail: String,
    muted: bool,
    window_open: bool,
    conns: BTreeMap<u64, Conn>,
    next_key: u64,
    ui: Option<Ui>,
    voice: Option<Voice>,
    worker: Worker,
    registrar: Registrar,
    settings: SettingsServer,
    connector: Option<Connector>,
    session: Option<Session>,
    /// The conversation being opened was started by the user (not by a
    /// notice).
    attended: bool,
    approvals: Vec<Approval>,
    next_approval: u64,
    /// Results of approved actions go back to the conversation as a notice
    /// (the function call that asked has long been answered).
    approved: BTreeMap<String, String>,
    /// Calls the language model is waiting on.
    calls_in_flight: usize,
    mic_buf: Vec<i16>,
    /// What was playing while `mic_buf` was recorded, and `mic_buf` before
    /// echo cancellation (empty if the audio service does not say).
    playing_buf: Vec<i16>,
    recorded_buf: Vec<i16>,
    /// Microphone audio short of a whole packet (and what goes with it).
    mic_pending: Vec<i16>,
    playing_pending: Vec<i16>,
    recorded_pending: Vec<i16>,
    /// Keeps the agent's own voice from the recogniser.
    gate: EchoGate,
    /// Packets the gate held back, the newest last (sent if the user turns
    /// out to be talking).
    preroll: VecDeque<Vec<i16>>,
    /// Notices to give the agent when the next conversation starts.
    on_wake: Vec<String>,
    seen_config: u64,
    seen_apps: u64,
    last_live_ns: u64,
    /// The Voice Agent endpoint (a local simulator in tests).
    endpoint: String,
    /// Start a conversation as soon as the agent runs (tests).
    wake_at_start: bool,
    /// Listening for the agent's name while it is asleep.
    listener: Option<listen::Listener>,
    /// Where speech is recognised then (a local simulator in tests).
    listen_endpoint: Option<String>,
    listen_retry_at: u64,
    /// How much recognition listening for the name may still use: the
    /// agent's, as every listener draws on it.
    budget: listen::SharedBudget,
    /// When the error shown is over, and the agent rests again.
    rest_at: Option<u64>,
}

impl Agent {
    fn config(&self) -> vagent::config::Config {
        self.shared.lock().config.clone()
    }

    fn status(&self) -> AgentStatus {
        let s = self.shared.lock();
        AgentStatus {
            state: self.state,
            name: s.config.name.clone(),
            muted: self.muted,
            configured: s.key.is_some(),
            detail: self.detail.clone(),
            pending: self.approvals.len() as u32,
        }
    }

    /// Changes the state and tells the interface.
    fn set_state(&mut self, state: AgentState, detail: &str) {
        if self.state == state && self.detail == detail {
            return;
        }
        if self.state != state {
            println!("state: {:?}{}", state, if detail.is_empty() { String::new() } else { format!(" ({detail})") });
        }
        self.state = state;
        self.detail = detail.to_string();
        self.publish();
    }

    fn publish(&mut self) {
        let status = self.status();
        *self.published.lock() = status.clone();
        if let Some(ui) = &self.ui {
            ui.live().state.store(status.state as u32, core::sync::atomic::Ordering::Relaxed);
            if !ui.send(AgentEvent::Status { status }) {
                self.ui = None;
            }
        }
    }

    fn update_live(&mut self, now: u64) {
        if now.saturating_sub(self.last_live_ns) < LIVE_PERIOD_NS {
            return;
        }
        self.last_live_ns = now;
        if let (Some(ui), Some(v)) = (&self.ui, &mut self.voice) {
            v.pump();
            ui.live().set_levels(v.output_level(), v.mic_level());
        }
    }

    fn run(&mut self, listener: Channel) -> ! {
        const LISTENER: u64 = 1;
        const WORKER: u64 = 2;
        const CONNECTOR: u64 = 3;
        const SESSION: u64 = 4;
        const VOICE: u64 = 5;
        // Settings or applications changed (housekeeping looks every pass).
        const CHANGED: u64 = 6;
        const CONN: u64 = 1 << 32;
        self.rest();
        if self.wake_at_start {
            self.wake("start-up option", true);
        }
        // Deepgram's messages are waiting beyond what one pass handled.
        let mut backlog = false;
        loop {
            let now = vrt::time::now_ns();
            let mut items: Vec<WaitItem> = Vec::new();
            let mut keys: Vec<u64> = Vec::new();
            let mut add = |h, s, k| {
                items.push(WaitItem { handle: h, signals: s, ..Default::default() });
                keys.push(k);
            };
            add(listener.raw(), signals::READABLE, LISTENER);
            if let Some(e) = &self.shared.lock().changed {
                add(e.raw(), signals::SIGNALED, CHANGED);
            }
            add(self.worker.handle(), signals::SIGNALED, WORKER);
            if let Some(c) = &self.connector {
                add(c.handle(), signals::SIGNALED, CONNECTOR);
            }
            if let Some(s) = &self.session {
                add(s.handle(), signals::READABLE | signals::PEER_CLOSED, SESSION);
            }
            if let Some(v) = &self.voice {
                for (h, s) in v.wait_handles() {
                    add(h, s, VOICE);
                }
            }
            if let Some(l) = &self.listener {
                for (h, s) in l.wait_handles() {
                    add(h, s, VOICE);
                }
            }
            for (&k, c) in &self.conns {
                add(c.channel.raw(), signals::READABLE | signals::PEER_CLOSED, CONN + k);
            }
            let active = self.session.is_some() || self.connector.is_some();
            let mut deadline = self.next_timer_ns(now);
            if active {
                deadline = deadline.min(now + if self.ui.is_some() { LIVE_PERIOD_NS } else { 250_000_000 });
            }
            if !self.approvals.is_empty() {
                deadline = deadline.min(now + 1_000_000_000);
            }
            if let Some(at) = self.rest_at {
                deadline = deadline.min(at);
            }
            if backlog {
                deadline = now;
            }
            let _ = vrt::object::wait_many(&mut items, deadline);
            let mut closed = Vec::new();
            for (it, &k) in items.iter().zip(&keys) {
                if it.observed == 0 {
                    continue;
                }
                match k {
                    LISTENER => self.accept(&listener),
                    WORKER => {
                        for d in self.worker.take() {
                            self.finished(d);
                        }
                    }
                    CONNECTOR => {
                        if let Some(r) = self.connector.as_ref().and_then(Connector::take) {
                            self.connector = None;
                            match r {
                                Ok(ws) => self.connected(ws),
                                Err(e) => {
                                    println!("cannot reach Deepgram: {}", e);
                                    let msg = match e {
                                        vweb::WebError::Status { code: 401 | 403, .. } => {
                                            "Deepgram did not accept the API key. Check it in Settings."
                                        }
                                        _ => "Deepgram cannot be reached. Check the network connection.",
                                    };
                                    self.end_conversation(Some(msg));
                                }
                            }
                        }
                    }
                    // Polled below on every pass: TLS may hold decrypted
                    // data the socket no longer signals.
                    SESSION => {}
                    // Cleared before housekeeping looks at what changed.
                    CHANGED => {
                        if let Some(e) = &self.shared.lock().changed {
                            let _ = e.clear();
                        }
                    }
                    VOICE => {}
                    k => {
                        let key = k - CONN;
                        if !self.serve(key)
                            || (it.observed & signals::PEER_CLOSED != 0 && it.observed & signals::READABLE == 0)
                        {
                            closed.push(key);
                        }
                    }
                }
            }
            backlog = self.session.is_some() && !self.poll_session();
            if let Some(l) = &mut self.listener
                && let Some(said) = l.poll(vrt::time::now_ns())
            {
                // What woke it is the first thing the user said.
                self.on_wake.push(said);
                self.wake("its name", true);
            }
            for k in closed {
                self.disconnect(k);
            }
            // The microphone and the voice are serviced every round.
            self.mic();
            if let Some(v) = &mut self.voice {
                v.pump();
            }
            let now = vrt::time::now_ns();
            self.housekeeping(now);
            self.check_timers();
            self.expire_approvals(now);
            self.update_live(now);
        }
    }
}

fn main() -> i32 {
    // Options from the kernel command line (handed down by init):
    // `agent.endpoint=ws://10.0.2.2:5000/` talks to a local simulator. The
    // Deepgram key goes only to Deepgram's own services, over TLS
    // (`vagent::deepgram::is_deepgram`); any other endpoint is sent the
    // word "test" in its place.
    let mut endpoint = String::from(deepgram::AGENT_URL);
    let mut wake_at_start = false;
    let mut listen_endpoint = None;
    for a in vrt::env::args().iter().skip(1) {
        wake_at_start |= a == "agent.wake";
        if let Some(e) = a.strip_prefix("agent.listen=")
            && (e.starts_with("ws://") || e.starts_with("wss://"))
        {
            println!("using the speech recognition endpoint {}", e);
            listen_endpoint = Some(String::from(e));
        }
        if let Some(e) = a.strip_prefix("agent.endpoint=")
            && (e.starts_with("ws://") || e.starts_with("wss://"))
        {
            println!("using the Voice Agent endpoint {}", e);
            endpoint = e.into();
        }
    }
    let shared = Arc::new(Mutex::new(Shared::load()));
    {
        let s = shared.lock();
        println!(
            "{} is here ({}; thinks with {}, speaks as {})",
            s.config.name,
            if s.key.is_some() { "Deepgram key set" } else { "no Deepgram key yet" },
            s.config.think_model,
            s.config.voice
        );
    }
    let listener = match vproto::register(agent::NAME) {
        Ok(l) => l,
        Err(e) => {
            println!("cannot register: {:?}", e);
            return 1;
        }
    };
    let Some(worker) = Worker::start(shared.clone()) else {
        println!("cannot start the worker");
        return 1;
    };
    let Some(registrar) = Registrar::start(shared.clone(), worker.links()) else {
        println!("cannot start the registrar");
        return 1;
    };
    let published = Arc::new(Mutex::new(AgentStatus {
        state: AgentState::Off,
        name: String::new(),
        muted: false,
        configured: false,
        detail: String::new(),
        pending: 0,
    }));
    let Some(settings) = SettingsServer::start(shared.clone(), published.clone(), registrar.clone()) else {
        println!("cannot serve Settings");
        return 1;
    };
    let voice = Voice::new();
    if voice.is_none() {
        println!("no audio service; the agent cannot speak or listen");
    }
    let mut agent = Agent {
        shared,
        published,
        state: AgentState::Off,
        detail: String::from("starting"),
        muted: false,
        window_open: false,
        conns: BTreeMap::new(),
        next_key: 0,
        ui: None,
        voice,
        worker,
        registrar,
        settings,
        connector: None,
        session: None,
        attended: false,
        approvals: Vec::new(),
        next_approval: 1,
        approved: BTreeMap::new(),
        calls_in_flight: 0,
        mic_buf: Vec::new(),
        playing_buf: Vec::new(),
        recorded_buf: Vec::new(),
        mic_pending: Vec::new(),
        playing_pending: Vec::new(),
        recorded_pending: Vec::new(),
        gate: EchoGate::new(),
        preroll: VecDeque::new(),
        on_wake: Vec::new(),
        seen_config: 0,
        seen_apps: 0,
        last_live_ns: 0,
        endpoint,
        wake_at_start,
        listener: None,
        listen_endpoint,
        listen_retry_at: 0,
        budget: listen::budget(vrt::time::now_ns()),
        rest_at: None,
    };
    agent.run(listener)
}
