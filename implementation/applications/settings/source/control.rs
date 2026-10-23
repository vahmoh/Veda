// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Settings for the voice agent: which page is shown, and the agent's own
//! voice, speaking rate, name and language model — so the user can say
//! "talk a little slower" and the change is made where it belongs, in the
//! system settings. The Deepgram key is never offered.
//!
//! The agent's settings change through the agent service like the Agent
//! page's do (only Settings may change them), one field at a time over
//! what the agent has; voices and models come from Deepgram's catalogue.
//!
//! The actions are answered on the window's thread (the toolkit asks for an
//! answer at once), so what can wait is kept short: one connection to the
//! agent service, kept ([`AgentLink`]: each new one would start a thread in
//! the service), and the catalogue fetched once (it waits on the network)
//! and kept.

use core::cell::RefCell;

use proto::agent::{AgentConfig, AgentError, ModelCatalog, ModelChoice, agent};
use ui::agent::{self as api, Action, AppAgentInfo, Risk, Value, arg_f64, arg_opt_str, arg_str, json};

use crate::{SECTIONS, Section, Settings};

/// How long a call about the agent's own settings may take (the service
/// answers it at once), and fetching Deepgram's catalogue (on the network).
const CALL_TIMEOUT_NS: u64 = 3_000_000_000;
const CATALOG_TIMEOUT_NS: u64 = 20_000_000_000;

const PAGES: [&str; 7] = ["personalization", "lock", "agent", "network", "display", "system", "about"];

pub fn info() -> AppAgentInfo {
    api::info(
        "System settings: the wallpaper, the lock screen's password (which only the user types), the agent's \
         voice, name and language model, network, display, system details and version.",
        vec![
            Action::new("show_page", "Shows a page of Settings").choice("page", "The page", true, &PAGES).build(),
            Action::new("set_agent_voice", "Changes how the agent (you) sounds: its voice, its speaking rate, or both")
                .param("voice", "string", "A voice name from list_voices, such as Helena or Orion", false)
                .param("speed", "number", "Speaking rate from 0.7 (slow) to 1.5 (fast); 1 is normal", false)
                .build(),
            Action::new("list_voices", "Lists the voices the agent can speak with")
                .param("accent", "string", "Only voices with this accent, such as British or Australian", false)
                .build(),
            Action::new("list_models", "Lists the language models the agent can think with, and their price tier")
                .build(),
            Action::new(
                "set_agent_model",
                "Changes the language model the agent thinks with (Advanced models cost more per minute)",
            )
            .param("model", "string", "A model's name or id from list_models", true)
            .risk(Risk::Sensitive)
            .build(),
            Action::new("rename_agent", "Changes the agent's name, which it also answers to")
                .param("name", "string", "The new name (1 to 24 characters)", true)
                .risk(Risk::Sensitive)
                .build(),
        ],
    )
}

fn section_name(s: Section) -> &'static str {
    match s {
        Section::Personalization => "personalization",
        Section::Lock => "lock",
        Section::Agent => "agent",
        Section::Network => "network",
        Section::Display => "display",
        Section::System => "system",
        Section::About => "about",
    }
}

fn refused(e: AgentError) -> String {
    match e {
        AgentError::BadConfig => "that setting is not valid".into(),
        e => e.to_string(),
    }
}

/// The agent service as Settings' actions reach it: one connection, kept
/// while it answers, and Deepgram's catalogue once fetched.
#[derive(Default)]
pub struct AgentLink {
    client: RefCell<Option<agent::Client>>,
    catalog: RefCell<Option<ModelCatalog>>,
}

impl AgentLink {
    /// Calls the service (connected first if need be), waiting at most
    /// `timeout_ns`; a connection that does not answer is dropped.
    fn call<T>(
        &self,
        timeout_ns: u64,
        f: impl FnOnce(&agent::Client) -> Result<Result<T, AgentError>, ipc::IpcError>,
    ) -> Result<T, String> {
        let mut slot = self.client.borrow_mut();
        if slot.is_none() {
            // A connection to a service not registered would wait for it.
            if !crate::agent::agent_running() {
                return Err("the agent service is not running".into());
            }
            let ch = proto::connect(agent::NAME).map_err(|_| "the agent service is not running")?;
            *slot = Some(agent::Client::new(ch));
        }
        let Some(c) = slot.as_ref() else { return Err("the agent service is not running".into()) };
        c.set_timeout(timeout_ns);
        match f(c) {
            Ok(r) => r.map_err(refused),
            Err(_) => {
                *slot = None;
                Err("the agent service did not answer".into())
            }
        }
    }

    /// The agent's settings as they are.
    pub fn config(&self) -> Result<AgentConfig, String> {
        self.call(CALL_TIMEOUT_NS, |c| c.config())
    }

    /// Changes the agent's settings by `f`, over what it has now.
    fn change(&self, f: impl FnOnce(&mut AgentConfig)) -> Result<AgentConfig, String> {
        let mut cfg = self.config()?;
        f(&mut cfg);
        let saved = cfg.clone();
        self.call(CALL_TIMEOUT_NS, |c| c.set_config(cfg))?;
        Ok(saved)
    }

    /// Deepgram's catalogue: fetched once (that waits on the network), then
    /// kept.
    pub fn catalog(&self) -> Result<ModelCatalog, String> {
        if let Some(c) = self.catalog.borrow().as_ref() {
            return Ok(c.clone());
        }
        let fetched = self.call(CATALOG_TIMEOUT_NS, |c| c.catalog())?;
        *self.catalog.borrow_mut() = Some(fetched.clone());
        Ok(fetched)
    }

    /// The catalogue, fetched elsewhere (by the Agent page).
    pub fn keep_catalog(&self, catalog: &ModelCatalog) {
        let mut slot = self.catalog.borrow_mut();
        if slot.is_none() {
            *slot = Some(catalog.clone());
        }
    }
}

/// The choice called `query` (its name or id, ignoring case).
fn find<'a>(choices: &'a [ModelChoice], query: &str) -> Option<&'a ModelChoice> {
    let q = query.trim().to_lowercase();
    choices
        .iter()
        .find(|c| c.id.to_lowercase() == q || c.name.to_lowercase() == q)
        .or_else(|| choices.iter().find(|c| c.id.to_lowercase().contains(&q)))
}

fn voice_value(v: &ModelChoice) -> Value {
    json!({ "name": v.name.as_str(), "about": v.detail.as_str() })
}

pub fn state(s: &Settings) -> Value {
    let agent = s.link.config().ok().map(|c| {
        json!({
            "name": c.name.as_str(),
            "voice": c.voice.as_str(),
            "speaking_rate": api::number(c.speed),
            "language_model": c.think_model.as_str(),
            "speech_recognition": c.listen_model.as_str(),
            "answers_to_its_name": c.listen_for_name,
            "enabled": c.enabled,
            "has_deepgram_key": c.has_key,
        })
    });
    json!({
        "page": section_name(s.section),
        "pages": Value::from(PAGES.iter().map(|p| Value::from(*p)).collect::<Vec<Value>>()),
        "wallpaper": (!s.current.is_empty()).then(|| Settings::title_of(&s.current)),
        "screen": s.screen.map(|i| format!("{} x {}", i.width, i.height)),
        "agent": agent,
    })
}

impl Settings {
    pub(crate) fn agent_invoke(&mut self, action: &str, args: &Value) -> Result<Value, String> {
        match action {
            "show_page" => {
                let page = arg_str(args, "page")?.trim().to_lowercase();
                let section = SECTIONS
                    .iter()
                    .map(|s| s.0)
                    .find(|&s| section_name(s) == page)
                    .ok_or_else(|| format!("Settings has no page called {page}; it has {}", PAGES.join(", ")))?;
                self.section = section;
                Ok(json!({ "showing": section_name(section) }))
            }
            "set_agent_voice" => {
                let voice = arg_opt_str(args, "voice");
                let speed = match args.get("speed") {
                    Some(_) => Some(arg_f64(args, "speed")? as f32),
                    None => None,
                };
                if voice.is_none() && speed.is_none() {
                    return Err("say which voice or speaking rate".into());
                }
                let voice = match voice {
                    Some(v) => {
                        let voices = self.catalog()?.voices;
                        let found = find(&voices, v).or_else(|| find(&voices, &format!("aura-2-{v}")));
                        let found = found.ok_or_else(|| {
                            let names: Vec<&str> = voices.iter().map(|v| v.name.as_str()).collect();
                            format!("there is no voice called {v}; voices: {}", names.join(", "))
                        })?;
                        Some(found.id.clone())
                    }
                    None => None,
                };
                let speed = match speed {
                    Some(sp) if !(0.7..=1.5).contains(&sp) => {
                        return Err("the speaking rate goes from 0.7 to 1.5".into());
                    }
                    Some(sp) => Some((sp * 20.0 + 0.5) as i32 as f32 / 20.0),
                    None => None,
                };
                let cfg = self.link.change(|cfg| {
                    if let Some(v) = voice {
                        cfg.voice = v;
                    }
                    if let Some(sp) = speed {
                        cfg.speed = sp;
                    }
                })?;
                self.reload_agent_page();
                Ok(json!({ "voice": cfg.voice, "speaking_rate": api::number(cfg.speed) }))
            }
            "list_voices" => {
                let voices = self.catalog()?.voices;
                let accent = arg_opt_str(args, "accent").map(str::to_lowercase);
                let list: Vec<Value> = voices
                    .iter()
                    .filter(|v| accent.as_ref().is_none_or(|a| v.detail.to_lowercase().contains(a.as_str())))
                    .take(60)
                    .map(voice_value)
                    .collect();
                Ok(json!({ "voices": list, "of": voices.len() }))
            }
            "list_models" => {
                let models = self.catalog()?.think;
                let list: Vec<Value> = models
                    .iter()
                    .map(|m| json!({ "name": m.name.as_str(), "id": m.id.as_str(), "tier": m.detail.as_str() }))
                    .collect();
                Ok(json!({ "models": list }))
            }
            "set_agent_model" => {
                let q = arg_str(args, "model")?;
                let models = self.catalog()?.think;
                let m = find(&models, q).ok_or_else(|| format!("there is no language model called {q}"))?;
                let (provider, id) = (m.provider.clone(), m.id.clone());
                self.link.change(|cfg| {
                    cfg.think_provider = provider;
                    cfg.think_model = id;
                })?;
                self.reload_agent_page();
                Ok(json!({ "model": m.name.as_str(), "tier": m.detail.as_str() }))
            }
            "rename_agent" => {
                let name = arg_str(args, "name")?.trim().to_string();
                if name.is_empty() || name.chars().count() > 24 {
                    return Err("the name must be 1 to 24 characters".into());
                }
                let to = name.clone();
                self.link.change(|cfg| cfg.name = to)?;
                self.reload_agent_page();
                Ok(json!({ "name": name }))
            }
            other => Err(format!("Settings has no action called {other}")),
        }
    }

    /// The Agent page shows the agent's settings as they are now.
    fn reload_agent_page(&mut self) {
        if let Some(p) = &mut self.agent {
            p.reload();
        }
    }

    /// Deepgram's catalogue: the Agent page's if it has it, else fetched
    /// once.
    fn catalog(&self) -> Result<ModelCatalog, String> {
        if let Some(c) = self.agent.as_ref().and_then(|p| p.catalog()) {
            self.link.keep_catalog(c);
        }
        self.link.catalog()
    }
}
