// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! A conversation with the Deepgram Voice Agent.
//!
//! [`Connector`] opens the WebSocket on a helper thread (name lookup, TCP,
//! TLS and the upgrade take a while, and the agent must keep serving its
//! interface meanwhile), and tries again while the network is coming up.
//! [`Session`] then carries the conversation: the `Settings` message goes
//! out first; microphone audio recorded before Deepgram confirms the
//! settings is held and sent right after, so the first words are not lost;
//! messages and voice come back through [`Session::poll`], which never
//! blocks. Deepgram has a few seconds to confirm the settings
//! ([`Session::overdue`]).
//!
//! Sending happens on the agent's one loop, which also serves the
//! interface, approvals and timers: a write may wait for the network only
//! briefly ([`bound_writes`]), and one that cannot finish in that time
//! fails, and the connection with it.

use std::collections::{BTreeSet, VecDeque};
use std::sync::Arc;

use abi::RawHandle;
use agent::deepgram::{self, ServerMessage};
use agent::presence::Presence;
use net::NetError;
use runtime::object::Event;
use runtime::sync::Mutex;
use runtime::time::Duration;
use serde_json::Value;
use web::ws::{Message, WebSocket};
use web::{Conn, WebError};

/// Microphone audio kept while the conversation is being set up.
const MAX_PENDING_MIC: usize = 4 * crate::voice::MIC_RATE as usize;
/// Deepgram closes a conversation that hears nothing for about ten
/// seconds; a keep-alive goes out after this long without audio.
const KEEP_ALIVE_NS: u64 = 5_000_000_000;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// How long Deepgram has to confirm a conversation's settings.
const READY_TIMEOUT_NS: u64 = 10_000_000_000;
/// How long one write may wait for the network to take it.
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
/// A connection that fails for want of a network is tried again after this
/// long ...
const NETWORK_RETRY: Duration = Duration::from_millis(250);
/// ... while the network changed less than this long ago, as it may still
/// be coming up (at boot, the agent can wake before DHCP has given the
/// machine an address), and for this long at most.
const NETWORK_SETTLE_NS: u64 = 10_000_000_000;

/// Something Deepgram sent.
pub enum Incoming {
    Message(ServerMessage),
    /// Voice: 24 kHz mono samples.
    Audio(Vec<i16>),
    /// The server closed the conversation.
    Closed {
        code: u16,
        reason: String,
    },
}

/// The outcome of opening a conversation, once there is one.
type Opened = Arc<Mutex<Option<Result<WebSocket<Conn>, WebError>>>>;

/// Opens a conversation's WebSocket on a helper thread.
pub struct Connector {
    result: Opened,
    event: Event,
}

impl Connector {
    /// Starts connecting to `url`, authenticated with `key`.
    pub fn start(url: String, key: String) -> Option<Connector> {
        let event = Event::create().ok()?;
        let signal = Event::from_handle(event.0.duplicate(None).ok()?);
        let result = Arc::new(Mutex::new(None));
        let slot = result.clone();
        runtime::thread::Builder::new()
            .name("connect")
            .spawn(move || {
                let auth = format!("Token {key}");
                let first = runtime::time::now_ns();
                let mut waited = false;
                let r = loop {
                    let r = WebSocket::connect(&url, &[("Authorization", &auth)], CONNECT_TIMEOUT);
                    // Nobody takes the result once the conversation ended.
                    let wanted = Arc::strong_count(&slot) > 1;
                    match &r {
                        Err(e) if wanted && coming_up(e, first) => {
                            if !waited {
                                println!("cannot reach Deepgram yet ({e}): the network is still coming up");
                                waited = true;
                            }
                            runtime::time::sleep(NETWORK_RETRY);
                        }
                        _ => break r,
                    }
                };
                *slot.lock() = Some(r);
                let _ = signal.signal();
            })
            .ok()?;
        Some(Connector { result, event })
    }

    /// Signalled when the connection is open, or failed.
    pub fn handle(&self) -> RawHandle {
        self.event.raw()
    }

    /// The connection, once the helper finished.
    pub fn take(&self) -> Option<Result<WebSocket<Conn>, WebError>> {
        let _ = self.event.clear();
        self.result.lock().take()
    }
}

/// Makes every write on `ws` give up after [`WRITE_TIMEOUT`] without
/// progress (`WebError::TimedOut`), rather than wait for a stalled network.
/// A write cut short leaves the connection unusable: whoever sends treats
/// the error as a lost connection.
pub fn bound_writes(ws: &mut WebSocket<Conn>) {
    match ws.connection_mut() {
        Conn::Plain(tcp) => tcp.set_write_timeout(Some(WRITE_TIMEOUT)),
        Conn::Tls(tls) => tls.transport_mut().set_write_timeout(Some(WRITE_TIMEOUT)),
    }
}

/// Whether a connection that failed with `e` may succeed in a moment: it
/// found no route, no network, no name server or no network service, the
/// network changed a moment ago (or its service is not running yet), and
/// the first attempt, at `first`, was not long ago either.
fn coming_up(e: &WebError, first: u64) -> bool {
    let early = matches!(
        e,
        WebError::Net(
            NetError::NoRoute
                | NetError::NetworkDown
                | NetError::HostUnreachable
                | NetError::DnsFailure
                | NetError::Unavailable
        )
    );
    let now = runtime::time::now_ns();
    if !early || now - first >= NETWORK_SETTLE_NS {
        return false;
    }
    match net::status() {
        Ok(s) => now.saturating_sub(u64::from(s.changed_s) * 1_000_000_000) < NETWORK_SETTLE_NS,
        Err(_) => true,
    }
}

/// An open conversation.
pub struct Session {
    ws: WebSocket<Conn>,
    /// Deepgram applied the settings: audio may flow.
    pub ready: bool,
    pub started_ns: u64,
    /// The last time someone spoke (for ending quiet conversations).
    pub last_activity_ns: u64,
    /// The agent is in its turn: from its first audio until all of it has
    /// been sent (`AgentAudioDone`) and played.
    pub agent_turn: bool,
    /// `AgentAudioDone` arrived for the current turn.
    pub audio_done: bool,
    /// When the agent asked to end the conversation (after its goodbye).
    pub end_requested_at: Option<u64>,
    /// Its goodbye started (voice after that request) ...
    pub goodbye_started: bool,
    /// ... and was completely sent.
    pub goodbye_sent: bool,
    /// Message types not understood, each reported once.
    pub unknown: BTreeSet<String>,
    /// Function calls the user cancelled by speaking again after they had
    /// started: their results are not wanted as results any more.
    pub cancelled: BTreeSet<String>,
    /// The user is there: they started the conversation, or have spoken in
    /// it. A conversation that a notice started (a due reminder) is not
    /// attended until the user speaks.
    pub attended: bool,
    /// What it has shown of the user, for the window system's idle lock
    /// (see `agent::presence`).
    pub presence: Presence,
    pending_mic: VecDeque<i16>,
    pending_inject: Vec<String>,
    last_sent_ns: u64,
    /// A byte of voice left over from an odd-sized message.
    carry: Option<u8>,
}

impl Session {
    /// The agent asked to end the conversation.
    pub fn request_end(&mut self, now: u64) {
        self.end_requested_at = Some(now);
        self.goodbye_started = false;
        self.goodbye_sent = false;
    }

    /// The conversation goes on after all.
    pub fn cancel_end(&mut self) {
        if self.end_requested_at.take().is_some() {
            println!("the conversation goes on");
        }
        self.goodbye_started = false;
        self.goodbye_sent = false;
    }

    /// The conversation on `ws`, which opened at `now`; `attended` if the
    /// user started it.
    pub fn new(mut ws: WebSocket<Conn>, now: u64, attended: bool) -> Session {
        bound_writes(&mut ws);
        Session {
            ws,
            ready: false,
            started_ns: now,
            last_activity_ns: now,
            agent_turn: false,
            audio_done: false,
            end_requested_at: None,
            goodbye_started: false,
            goodbye_sent: false,
            unknown: BTreeSet::new(),
            cancelled: BTreeSet::new(),
            attended,
            presence: Presence::new(attended),
            pending_mic: VecDeque::new(),
            pending_inject: Vec::new(),
            last_sent_ns: now,
            carry: None,
        }
    }

    /// Readable when Deepgram sent something.
    pub fn handle(&self) -> RawHandle {
        self.ws.handle()
    }

    /// Deepgram has not confirmed the settings in time: a server that
    /// accepted the connection but does not answer.
    pub fn overdue(&self, now: u64) -> bool {
        !self.ready && now.saturating_sub(self.started_ns) > READY_TIMEOUT_NS
    }

    /// Sends a JSON message.
    pub fn send(&mut self, v: &Value) -> Result<(), WebError> {
        self.last_sent_ns = runtime::time::now_ns();
        self.ws.send_text(&v.to_string())
    }

    /// Sends (or, before the settings apply, holds) microphone audio.
    pub fn send_mic(&mut self, pcm: &[i16]) -> Result<(), WebError> {
        if !self.ready {
            self.pending_mic.extend(pcm.iter().copied());
            let excess = self.pending_mic.len().saturating_sub(MAX_PENDING_MIC);
            self.pending_mic.drain(..excess);
            return Ok(());
        }
        if pcm.is_empty() {
            return Ok(());
        }
        let mut bytes = Vec::with_capacity(pcm.len() * 2);
        for s in pcm {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        self.last_sent_ns = runtime::time::now_ns();
        self.ws.send_binary(&bytes)
    }

    /// Asks the agent to respond to a notice from the system (sent once the
    /// conversation is ready).
    pub fn inject(&mut self, text: &str) -> Result<(), WebError> {
        if self.ready {
            self.send(&deepgram::inject_user_message(text))
        } else {
            self.pending_inject.push(text.to_string());
            Ok(())
        }
    }

    /// The settings were applied: send what was held back.
    pub fn on_ready(&mut self) -> Result<(), WebError> {
        self.ready = true;
        let held: Vec<i16> = self.pending_mic.drain(..).collect();
        for chunk in held.chunks(crate::voice::MIC_PACKET * 5) {
            self.send_mic(chunk)?;
        }
        for text in core::mem::take(&mut self.pending_inject) {
            self.send(&deepgram::inject_user_message(&text))?;
        }
        Ok(())
    }

    /// Sends a keep-alive when no audio went out for a while (the
    /// microphone is muted).
    pub fn keep_alive(&mut self, now: u64) -> Result<(), WebError> {
        if self.ready && now.saturating_sub(self.last_sent_ns) > KEEP_ALIVE_NS {
            self.send(&deepgram::keep_alive())?;
        }
        Ok(())
    }

    /// The next thing Deepgram sent, if any (never blocks).
    pub fn poll(&mut self) -> Result<Option<Incoming>, WebError> {
        loop {
            match self.ws.poll()? {
                None => return Ok(None),
                Some(Message::Text(t)) => match deepgram::parse_server_message(&t) {
                    Ok(m) => return Ok(Some(Incoming::Message(m))),
                    Err(e) => {
                        println!("ignoring a malformed message ({}): {}", e, t.chars().take(120).collect::<String>())
                    }
                },
                Some(Message::Binary(b)) => {
                    let mut pcm = Vec::with_capacity(b.len() / 2 + 1);
                    let mut bytes = &b[..];
                    if let Some(lo) = self.carry.take()
                        && let Some((&hi, rest)) = bytes.split_first()
                    {
                        pcm.push(i16::from_le_bytes([lo, hi]));
                        bytes = rest;
                    }
                    let (pairs, _) = bytes.as_chunks::<2>();
                    pcm.extend(pairs.iter().map(|p| i16::from_le_bytes(*p)));
                    if bytes.len() % 2 == 1 {
                        self.carry = bytes.last().copied();
                    }
                    return Ok(Some(Incoming::Audio(pcm)));
                }
                Some(Message::Close { code, reason }) => return Ok(Some(Incoming::Closed { code, reason })),
            }
        }
    }

    /// Ends the conversation politely.
    pub fn close(&mut self) {
        let _ = self.ws.close(web::ws::close_code::NORMAL, "");
    }
}
