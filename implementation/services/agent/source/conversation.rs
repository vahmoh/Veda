// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Conversations: resting and listening for the agent's name, waking,
//! what Deepgram says during a conversation, the microphone through the
//! echo gate, notices, and ending.
//!
//! * After an error the agent shows it for a moment ([`ERROR_SHOWN_NS`]),
//!   then rests, listening for its name again: a network that failed once
//!   does not leave it deaf.
//! * A conversation must be confirmed by Deepgram in time
//!   ([`crate::session::Session::overdue`]); one that is not ends, with the
//!   microphone closed and other sound back up.
//! * A conversation that a notice started (a due reminder) is not attended
//!   until the user speaks: until then, nothing that would wake the agent
//!   again runs (`agent::tools::allowed_unattended`).
//! * The user cancelling calls by speaking again takes them out of the
//!   worker's queue; those that had started run to the end, and what they
//!   did is told (see [`crate::approvals`]).
//! * While the user talks in a conversation they asked for, and while the
//!   agent's voice answers, the window system hears that the user is there
//!   ([`Agent::user_active`]), so that the screen does not lock in the
//!   middle of a conversation held hands-free. Nothing the agent hears
//!   while it only listens for its name counts, nor does a conversation a
//!   notice started (`agent::presence` says why).

use agent::deepgram::{self, Role, ServerMessage};
use agent::gate::{self, Verdict};
use agent::notice::Notice;
use agent::prompt::{self, PromptContext};
use agent::tools;
use proto::agent::AgentState;
use runtime::println;

use crate::session::{Connector, Incoming, Session};
use crate::voice::{MIC_PACKET, Voice};
use crate::worker::{self, Job};
use crate::{Agent, listen};

/// How the last conversation ended, shown to the next one for reference
/// (not as turns to continue) ...
const RECAP_CHARS: usize = 800;
/// ... if it was within the last few hours.
const RECAP_AGE_S: u64 = 6 * 3600;
/// How long an error is shown before the agent rests and listens again.
pub const ERROR_SHOWN_NS: u64 = 10_000_000_000;
/// What the agent shows when the connection failed.
const LOST: &str = "The connection to Deepgram was lost.";

impl Agent {
    /// The resting state: asleep, or off without a key.
    pub(crate) fn rest(&mut self) {
        self.rest_at = None;
        let (configured, enabled) = {
            let s = self.shared.lock();
            (s.key.is_some() || !deepgram::is_deepgram(&self.endpoint), s.config.enabled)
        };
        if !enabled {
            self.set_state(AgentState::Off, "The agent is switched off in Settings.");
        } else if !configured {
            self.set_state(AgentState::Off, "Add your Deepgram API key in Settings to talk with me.");
        } else {
            self.set_state(AgentState::Asleep, "");
        }
        self.update_listening();
    }

    /// Listens for the agent's name while it is asleep (when the user
    /// wants that and the microphone is on), and stops otherwise.
    pub(crate) fn update_listening(&mut self) {
        let (key, name, wanted) = {
            let s = self.shared.lock();
            (s.key.clone(), s.config.name.clone(), s.config.enabled && s.config.listen_for_name)
        };
        let url = match &self.listen_endpoint {
            Some(e) => e.clone(),
            None => deepgram::listen_url(&[name.as_str()]),
        };
        // The key goes only to Deepgram.
        let key = if deepgram::is_deepgram(&url) { key } else { Some(String::from("test")) };
        let listen = wanted
            && key.is_some()
            && !self.muted
            && !self.locked
            && self.state == AgentState::Asleep
            && self.session.is_none()
            && self.connector.is_none();
        if !listen {
            if self.listener.take().is_some()
                && self.session.is_none()
                && let Some(v) = &mut self.voice
            {
                v.close_mic();
            }
            return;
        }
        if self.listener.is_some() {
            return;
        }
        let Some(v) = &mut self.voice else { return };
        if !v.open_mic() {
            return;
        }
        let now = runtime::time::now_ns();
        self.listener = Some(listen::Listener::new(&name, &key.unwrap_or_default(), url, now, self.budget.clone()));
    }

    /// Starts a conversation (if none is running); `attended` if the user
    /// asked for it (a click, its name), not a notice. None starts while the
    /// screen is locked: it waits for it to be unlocked (see [`crate::lock`]).
    pub(crate) fn wake(&mut self, reason: &str, attended: bool) {
        if self.locked {
            println!("not waking ({reason}): the screen is locked");
            self.wake_on_unlock |= attended;
            return;
        }
        if let Some(s) = &mut self.session {
            s.attended |= attended;
            if attended {
                s.presence.ask();
            }
            return;
        }
        if self.connector.is_some() {
            self.attended |= attended;
            return;
        }
        let (key, enabled) = {
            let s = self.shared.lock();
            (s.key.clone(), s.config.enabled)
        };
        if !enabled {
            self.rest();
            return;
        }
        // The key goes only to Deepgram.
        let key = if !deepgram::is_deepgram(&self.endpoint) {
            String::from("test")
        } else if let Some(k) = key {
            k
        } else {
            self.rest();
            return;
        };
        println!("waking ({reason})");
        self.attended = attended;
        self.listener = None;
        self.rest_at = None;
        // Music and other sound play quieter during the conversation, so
        // that the agent hears the user (and the user the agent).
        if let Some(v) = &self.voice {
            v.duck_others(true);
        }
        if let Some(v) = &mut self.voice
            && !self.muted
            && !v.open_mic()
        {
            println!("listening without a microphone");
        }
        match Connector::start(self.endpoint.clone(), key) {
            Some(c) => {
                self.connector = Some(c);
                self.set_state(AgentState::Waking, "");
            }
            None => self.end_conversation(Some("The agent could not start a conversation.")),
        }
    }

    /// The prompt for a conversation.
    pub(crate) fn build_prompt(&self) -> String {
        let installed = worker::installed_apps();
        let since = (runtime::time::unix_time_ns() / 1_000_000_000).saturating_sub(RECAP_AGE_S);
        let (memory, recap, infos, name, first) = {
            let s = self.shared.lock();
            let recap: String = s
                .memory
                .recent_turns(RECAP_CHARS, since)
                .iter()
                .map(|(role, text)| format!("{}: {}\n", if *role == Role::User { "User" } else { "You" }, text))
                .collect();
            (
                s.memory.prompt_section(),
                recap,
                s.apps.clone(),
                s.config.name.clone(),
                s.memory.facts.is_empty() && s.memory.turns.is_empty(),
            )
        };
        let apps = prompt::catalog(&installed, &infos);
        let screen = worker::screen_summary();
        let now = worker::now_text();
        prompt::system_prompt(&PromptContext {
            agent_name: &name,
            now: &now,
            screen: &screen,
            memory: &memory,
            last_conversation: &recap,
            apps: &apps,
            first_meeting: first,
        })
    }

    /// The `Settings` message for a new conversation.
    fn settings(&self) -> serde_json::Value {
        let c = self.config();
        deepgram::settings(&deepgram::SessionSettings {
            listen_model: c.listen_model.clone(),
            keyterms: vec![c.name.clone()],
            think_provider: c.think_provider.clone(),
            think_model: c.think_model.clone(),
            prompt: self.build_prompt(),
            functions: tools::definitions(),
            voice: c.voice.clone(),
            speed: c.speed,
        })
    }

    /// The conversation's connection is open: its settings go out, with
    /// the notices that woke the agent.
    pub(crate) fn connected(&mut self, ws: web::ws::WebSocket<web::Conn>) {
        let now = runtime::time::now_ns();
        let mut s = Session::new(ws, now, core::mem::take(&mut self.attended));
        let settings = self.settings();
        if let Err(e) = s.send(&settings) {
            println!("cannot configure the conversation: {}", e);
            self.session = Some(s);
            self.end_conversation(Some("The conversation could not be set up."));
            return;
        }
        for notice in core::mem::take(&mut self.on_wake) {
            let _ = s.inject(&notice);
        }
        let (gc, ga) = {
            let sh = self.shared.lock();
            (sh.config_generation, sh.apps_generation)
        };
        self.seen_config = gc;
        self.seen_apps = ga;
        self.session = Some(s);
    }

    /// Ends the conversation (or the attempt to start one); `error` is
    /// shown for a moment before the agent rests.
    pub(crate) fn end_conversation(&mut self, error: Option<&str>) {
        if let Some(mut s) = self.session.take() {
            s.close();
            println!("conversation ended after {} s", (runtime::time::now_ns() - s.started_ns) / 1_000_000_000);
            let g = &mut self.gate;
            if g.held > 0 {
                let (cancelled, recorded) = g.coupling_db();
                println!(
                    "kept {:.1} s of the agent's own voice from the recogniser (its echo up to {:.0} dB after cancellation, {:.0} dB before), the user talked over it {} time(s)",
                    g.held as f32 * 0.02,
                    cancelled,
                    recorded,
                    g.openings
                );
                g.held = 0;
                g.openings = 0;
            }
        }
        self.mic_pending.clear();
        self.playing_pending.clear();
        self.recorded_pending.clear();
        self.preroll.clear();
        self.connector = None;
        self.attended = false;
        self.calls_in_flight = 0;
        if let Some(v) = &mut self.voice {
            v.close_mic();
            v.close_speaker();
            v.duck_others(false);
        }
        self.shared.lock().save_memory();
        match error {
            Some(e) => {
                self.set_state(AgentState::Error, e);
                self.rest_at = Some(runtime::time::now_ns() + ERROR_SHOWN_NS);
            }
            None => self.rest(),
        }
    }

    /// Sends a message to Deepgram; a failure ends the conversation.
    pub(crate) fn send(&mut self, v: &serde_json::Value) {
        let failed = match &mut self.session {
            Some(s) => s.send(v).is_err(),
            None => false,
        };
        if failed {
            self.end_conversation(Some(LOST));
        }
    }

    /// Tells the agent something that happened (as a turn of its own),
    /// waking it if no conversation is on: the user is not there then, so
    /// the conversation is not attended.
    pub(crate) fn notice(&mut self, notice: &Notice) {
        let text = notice.text();
        match &mut self.session {
            Some(s) => {
                if s.inject(&text).is_err() {
                    self.end_conversation(Some(LOST));
                }
            }
            None => {
                self.on_wake.push(text);
                self.wake("a notice", false);
            }
        }
    }

    fn handle_message(&mut self, m: ServerMessage) {
        let now = runtime::time::now_ns();
        let unix = runtime::time::unix_time_ns() / 1_000_000_000;
        if let Some(s) = &mut self.session {
            s.presence.heard(&m);
        }
        match m {
            ServerMessage::Welcome { request_id } => println!("connected to Deepgram (request {request_id})"),
            ServerMessage::SettingsApplied => {
                if let Some(s) = &mut self.session
                    && s.on_ready().is_err()
                {
                    self.end_conversation(Some(LOST));
                    return;
                }
                self.set_state(AgentState::Listening, "");
            }
            ServerMessage::ConversationText { role, content } => {
                println!("{}: {}", if role == Role::User { "user" } else { "agent" }, content);
                if let Some(s) = &mut self.session {
                    s.last_activity_ns = now;
                }
                self.shared.lock().memory.add_turn(role, &content, unix);
            }
            ServerMessage::History { role, content } => {
                self.shared.lock().memory.add_turn(role, &content, unix);
            }
            ServerMessage::UserStartedSpeaking => {
                if let Some(v) = &mut self.voice
                    && v.speaking()
                {
                    let gate = if self.gate.is_open() {
                        "open"
                    } else if self.gate.echoing() {
                        "holding"
                    } else {
                        "passing"
                    };
                    println!("interrupted: voice stopped (echo gate {gate})");
                    v.stop_speaking();
                }
                if let Some(s) = &mut self.session {
                    s.last_activity_ns = now;
                    s.agent_turn = false;
                    // Someone is there.
                    s.attended = true;
                    // Talking over the goodbye: the conversation goes on.
                    // (Before it, the user may just be finishing the
                    // sentence the agent answered early.)
                    if s.goodbye_started {
                        s.cancel_end();
                    }
                }
                self.set_state(AgentState::Listening, "");
            }
            ServerMessage::AgentThinking { .. } => self.set_state(AgentState::Thinking, ""),
            ServerMessage::FunctionCallRequest { calls } => {
                // More work after asking to end: the conversation goes on.
                if calls.iter().any(|c| c.name != tools::names::END_CONVERSATION)
                    && let Some(s) = &mut self.session
                {
                    s.cancel_end();
                }
                let attended = self.session.as_ref().is_some_and(|s| s.attended);
                for c in calls.into_iter().filter(|c| c.client_side) {
                    println!("call {} {}", c.name, c.arguments.chars().take(200).collect::<String>());
                    let args = c.args();
                    if !attended && !tools::allowed_unattended(&c.name, &args) {
                        println!("refused: nobody has spoken in this conversation");
                        let refusal =
                            tools::error("only the user can ask for that; tell them, and wait until they say so");
                        self.send(&deepgram::function_response(&c.id, &c.name, &refusal));
                        continue;
                    }
                    self.calls_in_flight += 1;
                    self.worker.submit(Job { args, id: c.id, name: c.name, approved: false });
                }
                self.set_state(AgentState::Thinking, "");
            }
            ServerMessage::FunctionCallCancelled { ids } => self.cancel_calls(ids),
            ServerMessage::AgentStartedSpeaking { latency } => {
                if let Some(l) = latency {
                    println!("speaking (latency {:.2} s)", l);
                }
                self.agent_speaks();
            }
            ServerMessage::AgentAudioDone => {
                if let Some(v) = &mut self.voice {
                    v.answer_complete();
                }
                if let Some(s) = &mut self.session {
                    s.audio_done = true;
                    s.last_activity_ns = now;
                    if s.goodbye_started {
                        s.goodbye_sent = true;
                    }
                }
            }
            ServerMessage::LatencyReport { total, think } => match (total, think) {
                (Some(t), Some(m)) if m > 0.0 => println!("answered in {:.2} s (language model {:.2} s)", t, m),
                (Some(t), _) => println!("answered in {:.2} s", t),
                _ => {}
            },
            ServerMessage::Updated { what } => println!("{what}"),
            ServerMessage::InjectionRefused { message } => println!("notice refused: {message}"),
            ServerMessage::Error { code, description } => {
                println!("Deepgram error {code}: {description}");
                let lower = description.to_lowercase();
                if code.contains("AUTH") || lower.contains("auth") || lower.contains("credential") {
                    self.end_conversation(Some("Deepgram did not accept the API key. Check it in Settings."));
                }
            }
            ServerMessage::Warning { code, description } => println!("Deepgram warning {code}: {description}"),
            // Echoes of what this side sent (kept by Deepgram's history),
            // and Flux's turn-taking events (its decisions are acted on by
            // Deepgram already).
            ServerMessage::Other { kind }
                if matches!(
                    kind.as_str(),
                    "FunctionCallResponse" | "History" | "StartOfTurn" | "EndOfTurn" | "EagerEndOfTurn" | "TurnResumed"
                ) => {}
            ServerMessage::Other { kind } => {
                if let Some(s) = &mut self.session
                    && s.unknown.insert(kind.clone())
                {
                    println!("Deepgram sent a message not handled here: {kind}");
                }
            }
        }
    }

    /// The agent's voice is coming: its turn starts (once per turn).
    fn agent_speaks(&mut self) {
        let Some(s) = &mut self.session else { return };
        s.last_activity_ns = runtime::time::now_ns();
        if s.end_requested_at.is_some() {
            s.goodbye_started = true;
        }
        if s.agent_turn && !s.audio_done {
            return;
        }
        s.agent_turn = true;
        s.audio_done = false;
        self.set_state(AgentState::Speaking, "");
    }

    /// Handles what Deepgram sent. Returns false if more may be waiting
    /// (it stops after a while, so the rest of the loop keeps running).
    pub(crate) fn poll_session(&mut self) -> bool {
        for _ in 0..200 {
            let Some(s) = &mut self.session else { return true };
            match s.poll() {
                Ok(None) => return true,
                Ok(Some(Incoming::Message(m))) => self.handle_message(m),
                Ok(Some(Incoming::Audio(pcm))) => {
                    self.agent_speaks();
                    if let Some(v) = &mut self.voice {
                        v.speak(&pcm);
                    }
                }
                Ok(Some(Incoming::Closed { code, reason })) => {
                    println!("Deepgram closed the conversation ({code} {reason})");
                    let error =
                        if code == 1000 || code == 1005 { None } else { Some("Deepgram ended the conversation.") };
                    self.end_conversation(error);
                    return true;
                }
                Err(e) => {
                    println!("connection to Deepgram lost: {}", e);
                    self.end_conversation(Some(LOST));
                    return true;
                }
            }
        }
        false
    }

    /// Takes what the microphone recorded: to the listener while asleep, to
    /// Deepgram (through the echo gate) in a conversation.
    pub(crate) fn mic(&mut self) {
        let Some(v) = &mut self.voice else { return };
        self.mic_buf.clear();
        self.playing_buf.clear();
        self.recorded_buf.clear();
        if v.read_mic(&mut self.mic_buf, &mut self.playing_buf, &mut self.recorded_buf) == 0 {
            return;
        }
        if self.muted {
            return;
        }
        if self.session.is_none()
            && let Some(l) = &mut self.listener
        {
            l.hear(&self.mic_buf, &self.playing_buf, &self.recorded_buf, runtime::time::now_ns());
            return;
        }
        if self.session.is_none() {
            return;
        }
        let out = self.gate_mic();
        let failed = match &mut self.session {
            Some(s) => s.send_mic(&out).is_err(),
            None => false,
        };
        if failed {
            self.end_conversation(Some(LOST));
        }
    }

    /// The microphone as Deepgram should hear it: 20 ms at a time, through
    /// the echo gate, so that what is left of the agent's own voice after
    /// echo cancellation is silence to the recogniser (see
    /// `agent::gate`).
    fn gate_mic(&mut self) -> Vec<i16> {
        use audio::level::rms_dbfs;
        self.mic_pending.extend_from_slice(&self.mic_buf);
        self.playing_pending.extend_from_slice(&self.playing_buf);
        self.recorded_pending.extend_from_slice(&self.recorded_buf);
        let whole = self.mic_pending.len() / MIC_PACKET * MIC_PACKET;
        // The gate works while the agent's voice is audible (and against the
        // echo of everything playing; of the voice alone if the audio
        // service does not say what plays).
        let voice_db = self.voice.as_ref().map_or(-100.0, Voice::output_db);
        let mut out = Vec::with_capacity(whole + gate::PREROLL_PACKETS * MIC_PACKET);
        for (k, p) in self.mic_pending[..whole].chunks(MIC_PACKET).enumerate() {
            let at = k * MIC_PACKET..(k + 1) * MIC_PACKET;
            let mic_db = rms_dbfs(p);
            let playing_db = self.playing_pending.get(at.clone()).map_or(voice_db, rms_dbfs);
            let recorded_db = self.recorded_pending.get(at).map_or(mic_db, rms_dbfs);
            match self.gate.packet(mic_db, recorded_db, playing_db, voice_db) {
                Verdict::Pass => {
                    self.preroll.clear();
                    out.extend_from_slice(p);
                }
                Verdict::Through => out.extend_from_slice(p),
                Verdict::Hold => {
                    if self.preroll.len() == gate::PREROLL_PACKETS {
                        self.preroll.pop_front();
                    }
                    self.preroll.push_back(p.to_vec());
                    out.extend(core::iter::repeat_n(0i16, p.len()));
                }
                Verdict::Open => {
                    println!(
                        "the user talks over the agent (microphone at {:.0} dBFS, its echo at most {:.0})",
                        mic_db,
                        self.gate.expected_db()
                    );
                    for q in self.preroll.drain(..) {
                        out.extend_from_slice(&q);
                    }
                    out.extend_from_slice(p);
                }
            }
        }
        self.mic_pending.drain(..whole);
        let side = whole.min(self.playing_pending.len());
        self.playing_pending.drain(..side);
        let side = whole.min(self.recorded_pending.len());
        self.recorded_pending.drain(..side);
        out
    }

    /// Ends quiet (or unconfirmed) conversations, finishes the agent's
    /// turns, tells the window system when the user is there
    /// (`agent::presence`), brings settings that changed into the
    /// conversation, and rests again once an error has been shown.
    pub(crate) fn housekeeping(&mut self, now: u64) {
        /// How long to wait for a goodbye after the agent asked to end.
        const END_GRACE_NS: u64 = 5_000_000_000;
        /// The longest goodbye.
        const END_LIMIT_NS: u64 = 30_000_000_000;
        let idle_ns = self.config().idle_timeout_s as u64 * 1_000_000_000;
        let speaking = self.voice.as_ref().is_some_and(Voice::speaking);
        let mut end = false;
        let mut error = None;
        let mut present = false;
        if let Some(s) = &mut self.session {
            present = s.presence.tell(now, speaking);
            if s.overdue(now) {
                println!("Deepgram did not confirm the conversation's settings in time");
                error = Some("Deepgram did not answer. Check the network connection.");
            }
            if s.agent_turn && s.audio_done && !speaking {
                s.agent_turn = false;
                s.last_activity_ns = now;
            }
            // Asked to end: once the goodbye has been said, or if none
            // comes (it was said before the call) while all is quiet.
            if let Some(at) = s.end_requested_at {
                let quiet = !s.agent_turn && !speaking;
                let none_coming = !s.goodbye_started && now - at.max(s.last_activity_ns) > END_GRACE_NS;
                if (quiet && (s.goodbye_sent || none_coming)) || now - at > END_LIMIT_NS {
                    end = true;
                }
            }
            if speaking {
                s.last_activity_ns = now;
            }
            if s.ready
                && !s.agent_turn
                && self.calls_in_flight == 0
                && self.approvals.is_empty()
                && now - s.last_activity_ns > idle_ns
            {
                println!("quiet for a while; going to sleep");
                end = true;
            }
            if s.keep_alive(now).is_err() {
                error = Some(LOST);
            }
        }
        if present {
            self.user_active();
        }
        if error.is_some() {
            self.end_conversation(error);
        } else if end {
            self.end_conversation(None);
        } else if self.session.as_ref().is_some_and(|s| s.ready && !s.agent_turn) && self.state == AgentState::Speaking
        {
            self.set_state(AgentState::Listening, "");
        }
        // Settings or applications changed during a conversation.
        let (gc, ga) = {
            let s = self.shared.lock();
            (s.config_generation, s.apps_generation)
        };
        if self.session.as_ref().is_some_and(|s| s.ready) {
            if gc != self.seen_config {
                let c = self.config();
                self.send(&deepgram::update_speak(&c.voice, c.speed));
                self.send(&deepgram::update_think(&c.think_provider, &c.think_model));
            }
            if ga != self.seen_apps || gc != self.seen_config {
                let p = self.build_prompt();
                self.send(&deepgram::update_prompt(&p));
            }
        }
        if gc != self.seen_config {
            if self.session.is_none() && self.connector.is_none() {
                // A new name is listened for from now on.
                self.listener = None;
                self.rest();
            }
            // The interface shows the agent's name.
            self.publish();
        }
        self.seen_config = gc;
        self.seen_apps = ga;
        // An error has been shown long enough.
        if self.state == AgentState::Error && self.rest_at.is_some_and(|at| now >= at) {
            self.rest();
        }
        // The microphone may not have been there when the agent fell asleep.
        if self.listener.is_none() && self.state == AgentState::Asleep && now >= self.listen_retry_at {
            self.listen_retry_at = now + 5_000_000_000;
            self.update_listening();
        }
    }
}
