// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Serving Settings: the agent's configuration, the Deepgram key, the
//! models and voices to choose from, voice previews, memory and
//! permissions.
//!
//! Only the Settings application gets here (the main loop checks the
//! registry's identity of every connection). Its requests may wait on the
//! network — checking the key, fetching catalogues, a voice preview — so
//! they are served off the main loop, on one thread for all of Settings'
//! connections (Settings opens one for each change it makes for the
//! agent), in turn, sharing the agent's state.

use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;

use vabi::{WaitItem, signals};
use vagent::config::{self, Config};
use vagent::deepgram;
use vproto::agent::{
    AgentConfig, AgentError, AgentStatus, MemoryItem, ModelCatalog, ModelChoice, Permission, UiLink, agent,
};
use vproto::audio::{OutputStream, StreamSpec, audio, ring};
use vproto::init::ClientIdentity;
use vrt::object::{Channel, Event};
use vrt::sync::Mutex;
use vrt::time::Duration;
use vweb::WebError;

use crate::registrar::Registrar;
use crate::shared::Shared;

const NET_TIMEOUT: Duration = Duration::from_secs(15);
/// What a voice preview says.
const PREVIEW: &str = "Hi, this is how I sound. I'm here whenever you need me.";
/// Settings' connections served at once; one beyond them is closed.
const MAX_CONNECTIONS: usize = 16;

/// Logs how long a request to Deepgram took (the path only: queries can
/// be long).
fn log_fetch(url: &str, start: u64, bytes: Option<usize>) {
    let ms = (vrt::time::now_ns() - start) / 1_000_000;
    let path = url.split('?').next().unwrap_or(url);
    match bytes {
        Some(b) => vrt::println!("{path}: {b} bytes in {ms} ms"),
        None => vrt::println!("{path}: failed after {ms} ms"),
    }
}

/// New Settings connections, handed from the main loop to Settings'
/// thread.
struct Incoming {
    channels: Mutex<Vec<(Channel, ClientIdentity)>>,
    /// Signalled when one is added.
    added: Event,
}

/// The main loop's handle on Settings' thread.
pub struct SettingsServer(Arc<Incoming>);

impl SettingsServer {
    /// Starts the thread that serves Settings.
    pub fn start(
        shared: Arc<Mutex<Shared>>,
        status: Arc<Mutex<AgentStatus>>,
        registrar: Registrar,
    ) -> Option<SettingsServer> {
        let added = Event::create().ok()?;
        let incoming = Arc::new(Incoming {
            channels: Mutex::new(Vec::new()),
            added: Event::from_handle(added.0.duplicate(None).ok()?),
        });
        let theirs = incoming.clone();
        let admin = Admin { shared, status, registrar };
        vrt::thread::Builder::new().name("settings").spawn(move || serve(&theirs, &admin, &added)).ok()?;
        Some(SettingsServer(incoming))
    }

    /// Serves a connection from Settings (`who`).
    pub fn add(&self, channel: Channel, who: ClientIdentity) {
        self.0.channels.lock().push((channel, who));
        let _ = self.0.added.signal();
    }
}

/// Settings' thread: serves every request of every connection, in turn.
fn serve(incoming: &Incoming, admin: &Admin, added: &Event) {
    let mut conns: Vec<(Channel, ClientIdentity)> = Vec::new();
    let mut items: Vec<WaitItem> = Vec::new();
    loop {
        // Cleared before the connections are taken, so that one added
        // meanwhile signals again.
        let _ = added.clear();
        for (channel, who) in incoming.channels.lock().drain(..) {
            if conns.len() < MAX_CONNECTIONS {
                conns.push((channel, who));
            } else {
                vrt::println!("too many connections from Settings; one is closed");
            }
        }
        items.clear();
        items.push(WaitItem { handle: added.raw(), signals: signals::SIGNALED, ..Default::default() });
        items.extend(conns.iter().map(|(c, _)| WaitItem {
            handle: c.raw(),
            signals: signals::READABLE | signals::PEER_CLOSED,
            ..Default::default()
        }));
        let _ = vrt::object::wait_many(&mut items, vabi::DEADLINE_INFINITE);
        // The connections' items follow the event's, in order.
        let mut item = items.iter().skip(1);
        conns.retain(|(channel, who)| {
            let observed = item.next().map_or(0, |it| it.observed);
            let open = observed == 0 || serve_connection(admin, channel, who);
            open && (observed & signals::PEER_CLOSED == 0 || observed & signals::READABLE != 0)
        });
    }
}

/// Answers every request waiting on one connection; `false` once it closed.
fn serve_connection(admin: &Admin, channel: &Channel, who: &ClientIdentity) -> bool {
    loop {
        match channel.read() {
            Ok(msg) => {
                if let Ok(reply) = agent::dispatch(&mut Session { admin, who }, msg) {
                    let _ = reply.send(channel);
                }
            }
            Err(vabi::Error::ShouldWait) => return true,
            Err(_) => return false,
        }
    }
}

/// What serving Settings shares with the agent.
struct Admin {
    shared: Arc<Mutex<Shared>>,
    status: Arc<Mutex<AgentStatus>>,
    registrar: Registrar,
}

/// One request from Settings (`who`).
struct Session<'a> {
    admin: &'a Admin,
    who: &'a ClientIdentity,
}

/// The configuration as Settings sees it (never the key itself).
pub fn to_message(c: &Config, key: Option<&str>) -> AgentConfig {
    AgentConfig {
        enabled: c.enabled,
        name: c.name.clone(),
        listen_model: c.listen_model.clone(),
        think_provider: c.think_provider.clone(),
        think_model: c.think_model.clone(),
        voice: c.voice.clone(),
        speed: c.speed,
        listen_for_name: c.listen_for_name,
        idle_timeout_s: c.idle_timeout_s,
        has_key: key.is_some(),
        key_hint: key.map(config::key_hint).unwrap_or_default(),
    }
}

impl Session<'_> {
    fn key(&self) -> Result<String, AgentError> {
        self.admin.shared.lock().key.clone().ok_or(AgentError::NoKey)
    }

    /// GET with the key; maps failures to agent errors.
    fn get(&self, url: &str) -> Result<String, AgentError> {
        let auth = alloc::format!("Token {}", self.key()?);
        let start = vrt::time::now_ns();
        let r = vweb::http::fetch("GET", url, &[("Authorization", &auth)], &[], NET_TIMEOUT, 4 << 20);
        log_fetch(url, start, r.as_ref().map(|r| r.body.len()).ok());
        match r {
            Ok(r) if r.status == 401 || r.status == 403 => Err(AgentError::BadKey),
            Ok(r) if (200..300).contains(&r.status) => Ok(r.text()),
            Ok(r) => {
                vrt::println!("Deepgram answered {} for {}", r.status, url);
                Err(AgentError::Network)
            }
            Err(e) => {
                vrt::println!("cannot reach Deepgram: {}", e);
                Err(AgentError::Network)
            }
        }
    }
}

impl agent::Server for Session<'_> {
    fn status(&mut self) -> AgentStatus {
        self.admin.status.lock().clone()
    }

    /// Settings offers the agent its own actions like any application.
    fn register_app(&mut self, app: Channel) -> Result<(), AgentError> {
        self.admin.registrar.register(self.who, app)
    }

    fn attach_ui(&mut self) -> Result<UiLink, AgentError> {
        Err(AgentError::Denied)
    }

    fn decide(&mut self, _id: u64, _allow: bool, _always: bool) -> Result<(), AgentError> {
        Err(AgentError::Denied)
    }

    fn wake(&mut self) -> Result<(), AgentError> {
        Err(AgentError::Denied)
    }

    fn sleep(&mut self) -> Result<(), AgentError> {
        Err(AgentError::Denied)
    }

    fn set_window_open(&mut self, _open: bool) -> Result<(), AgentError> {
        Err(AgentError::Denied)
    }

    fn set_muted(&mut self, _muted: bool) -> Result<(), AgentError> {
        Err(AgentError::Denied)
    }

    fn config(&mut self) -> Result<AgentConfig, AgentError> {
        let s = self.admin.shared.lock();
        Ok(to_message(&s.config, s.key.as_deref()))
    }

    fn set_config(&mut self, c: AgentConfig) -> Result<(), AgentError> {
        let mut s = self.admin.shared.lock();
        let new = Config {
            enabled: c.enabled,
            name: c.name.trim().to_string(),
            listen_model: c.listen_model,
            think_provider: c.think_provider,
            think_model: c.think_model,
            voice: c.voice,
            speed: c.speed,
            listen_for_name: c.listen_for_name,
            idle_timeout_s: c.idle_timeout_s,
        };
        if let Err(e) = new.validate() {
            vrt::println!("refused settings: {}", e);
            return Err(AgentError::BadConfig);
        }
        s.config = new;
        s.save_config();
        vrt::println!("settings changed");
        Ok(())
    }

    fn set_api_key(&mut self, key: String) -> Result<(), AgentError> {
        let key = key.trim().to_string();
        let mut s = self.admin.shared.lock();
        if key.is_empty() {
            s.key = None;
            s.save_key();
            vrt::println!("Deepgram key removed");
            return Ok(());
        }
        if !config::valid_key(&key) {
            return Err(AgentError::BadConfig);
        }
        s.key = Some(key);
        s.save_key();
        vrt::println!("Deepgram key stored");
        Ok(())
    }

    fn check_key(&mut self) -> Result<String, AgentError> {
        let text = self.get(deepgram::PROJECTS_URL)?;
        let v = serde_json::from_str::<serde_json::Value>(&text).unwrap_or_default();
        let name = v.pointer("/projects/0/name").and_then(|n| n.as_str()).unwrap_or("your project");
        Ok(alloc::format!("The key works ({name})."))
    }

    fn catalog(&mut self) -> Result<ModelCatalog, AgentError> {
        let think = deepgram::parse_think_models(&self.get(deepgram::THINK_MODELS_URL)?);
        let voices = deepgram::parse_voices(&self.get(deepgram::MODELS_URL)?);
        Ok(ModelCatalog {
            think: think
                .into_iter()
                .map(|m| ModelChoice { id: m.id, name: m.name, provider: m.provider, detail: m.tier.to_string() })
                .collect(),
            voices: voices
                .into_iter()
                .map(|v| ModelChoice {
                    id: v.id,
                    name: v.name,
                    provider: String::new(),
                    detail: if v.accent.is_empty() {
                        v.description
                    } else {
                        alloc::format!("{}, {}", v.accent, v.description)
                    },
                })
                .collect(),
            listen: deepgram::LISTEN_MODELS
                .iter()
                .map(|(id, name, detail)| ModelChoice {
                    id: id.to_string(),
                    name: name.to_string(),
                    provider: String::new(),
                    detail: detail.to_string(),
                })
                .collect(),
        })
    }

    fn preview_voice(&mut self, voice: String) -> Result<(), AgentError> {
        if voice.is_empty() || voice.len() > 80 || !voice.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            return Err(AgentError::BadConfig);
        }
        let auth = alloc::format!("Token {}", self.key()?);
        let body = deepgram::speak_body(PREVIEW);
        let url = deepgram::speak_url(&voice);
        let start = vrt::time::now_ns();
        let r = vweb::http::fetch(
            "POST",
            &url,
            &[("Authorization", &auth), ("Content-Type", "application/json")],
            body.as_bytes(),
            NET_TIMEOUT,
            8 << 20,
        );
        log_fetch(&url, start, r.as_ref().map(|r| r.body.len()).ok());
        let r = r.map_err(|e: WebError| {
            vrt::println!("voice preview failed: {}", e);
            AgentError::Network
        })?;
        match r.status {
            200..=299 => {}
            401 | 403 => return Err(AgentError::BadKey),
            _ => return Err(AgentError::Network),
        }
        let pcm: Vec<i16> = r.body.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b)).collect();
        play(&pcm);
        Ok(())
    }

    fn memories(&mut self) -> Result<Vec<MemoryItem>, AgentError> {
        let s = self.admin.shared.lock();
        Ok(s.memory
            .facts
            .iter()
            .rev()
            .map(|f| MemoryItem { id: f.id, text: f.text.clone(), kind: f.kind.clone(), created: f.created })
            .collect())
    }

    fn forget(&mut self, id: u64) -> Result<(), AgentError> {
        let mut s = self.admin.shared.lock();
        if !s.memory.forget(id) {
            return Err(AgentError::NotFound);
        }
        s.save_memory();
        Ok(())
    }

    fn forget_all(&mut self) -> Result<(), AgentError> {
        let mut s = self.admin.shared.lock();
        s.memory.forget_all();
        s.save_memory();
        vrt::println!("memory erased");
        Ok(())
    }

    fn permissions(&mut self) -> Result<Vec<Permission>, AgentError> {
        let s = self.admin.shared.lock();
        Ok(s.permissions.list().into_iter().map(|(key, label)| Permission { key, label }).collect())
    }

    fn revoke(&mut self, key: String) -> Result<(), AgentError> {
        let mut s = self.admin.shared.lock();
        if !s.permissions.revoke(&key) {
            return Err(AgentError::NotFound);
        }
        s.save_permissions();
        Ok(())
    }
}

/// Plays 24 kHz mono PCM and waits until it has been heard.
fn play(pcm: &[i16]) {
    let Some(client) = vproto::connect(audio::NAME).ok().map(audio::Client::new) else { return };
    let spec = StreamSpec {
        rate: deepgram::OUTPUT_RATE,
        channels: 1,
        buffer_frames: deepgram::OUTPUT_RATE * 2,
        notify_frames: 0,
        name: "Voice preview".into(),
        paused: false,
        volume: 1.0,
    };
    let Ok(stream) = OutputStream::open(&client, spec) else { return };
    let mut pos = 0;
    let end = vrt::time::deadline_after(Duration::from_secs(20));
    while pos < pcm.len() && vrt::time::now_ns() < end {
        stream.wait_writable(2400, vrt::time::deadline_after(Duration::from_millis(200)));
        pos += stream.write(&pcm[pos..]);
    }
    // Nothing follows: played out, the stream has ended, not run short.
    stream.ring().set_producer_flags(ring::flags::END);
    while stream.played_now() < stream.written() && vrt::time::now_ns() < end {
        vrt::time::sleep(Duration::from_millis(50));
    }
    let _ = client.close(stream.id);
}
