// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! State shared by the agent's threads: settings, the Deepgram key, memory,
//! permissions, the catalogue of applications and timers — all persisted in
//! the private directory. What is read back is checked like any file: the
//! catalogue is bounded as applications' own descriptions are when they
//! register, and only permissions for sensitive actions the agent knows of
//! are kept.

use std::collections::BTreeMap;

use serde_json::{Value, json};
use vagent::config::Config;
use vagent::memory::Memory;
use vagent::policy::{self, Permissions};
use vagent::{notice, tools};
use vproto::agent::{ActionSpec, AppAgentInfo, ParamSpec, Risk};

use vrt::object::Event;

use crate::store::{self, Store};

/// A timer or reminder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Timer {
    pub id: u64,
    pub label: String,
    /// When it is due (Unix seconds, local time).
    pub due: u64,
}

/// The longest label of a timer.
const MAX_LABEL: usize = 120;

/// What the agent's threads share (see the module documentation).
pub struct Shared {
    pub store: Store,
    pub config: Config,
    pub key: Option<String>,
    pub memory: Memory,
    pub permissions: Permissions,
    /// What each application offered when it last ran (by application id).
    pub apps: BTreeMap<String, AppAgentInfo>,
    pub timers: Vec<Timer>,
    next_timer: u64,
    /// Incremented whenever the settings change.
    pub config_generation: u64,
    /// Incremented whenever the applications' abilities change.
    pub apps_generation: u64,
    /// Signalled when either changes, so the main loop looks (Settings and
    /// the worker change them on their own threads).
    pub changed: Option<Event>,
}

fn risk_name(r: Risk) -> &'static str {
    match r {
        Risk::Routine => "routine",
        Risk::Sensitive => "sensitive",
        Risk::Destructive => "destructive",
    }
}

fn risk_from(s: &str) -> Risk {
    match s {
        "sensitive" => Risk::Sensitive,
        "destructive" => Risk::Destructive,
        _ => Risk::Routine,
    }
}

/// An application's abilities as stored.
fn info_to_json(info: &AppAgentInfo) -> Value {
    let actions: Vec<Value> = info
        .actions
        .iter()
        .map(|a| {
            let params: Vec<Value> = a
                .params
                .iter()
                .map(|p| {
                    json!({
                        "name": p.name.as_str(),
                        "kind": p.kind.as_str(),
                        "description": p.description.as_str(),
                        "required": p.required,
                        "choices": p.choices.iter().map(|c| Value::from(c.as_str())).collect::<Vec<_>>(),
                    })
                })
                .collect();
            json!({ "name": a.name.as_str(), "description": a.description.as_str(), "risk": risk_name(a.risk), "params": params })
        })
        .collect();
    json!({ "summary": info.summary.as_str(), "has_state": info.has_state, "actions": actions })
}

fn info_from_json(v: &Value) -> Option<AppAgentInfo> {
    let actions = v["actions"]
        .as_array()?
        .iter()
        .filter_map(|a| {
            Some(ActionSpec {
                name: a["name"].as_str()?.to_string(),
                description: a["description"].as_str().unwrap_or("").to_string(),
                risk: risk_from(a["risk"].as_str().unwrap_or("")),
                params: a["params"]
                    .as_array()
                    .map(|ps| {
                        ps.iter()
                            .filter_map(|p| {
                                Some(ParamSpec {
                                    name: p["name"].as_str()?.to_string(),
                                    kind: p["kind"].as_str().unwrap_or("string").to_string(),
                                    description: p["description"].as_str().unwrap_or("").to_string(),
                                    required: p["required"].as_bool().unwrap_or(false),
                                    choices: p["choices"]
                                        .as_array()
                                        .map(|c| c.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                                        .unwrap_or_default(),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            })
        })
        .collect();
    Some(AppAgentInfo {
        summary: v["summary"].as_str().unwrap_or("").to_string(),
        actions,
        has_state: v["has_state"].as_bool().unwrap_or(false),
    })
}

const APPS: &str = "apps.json";

impl Shared {
    /// Loads everything from the private directory.
    pub fn load() -> Shared {
        let store = Store::open();
        let config = store.read(store::CONFIG).map(|t| Config::from_json(&t)).unwrap_or_default();
        let key = store.read(store::KEY).map(|k| k.trim().to_string()).filter(|k| vagent::config::valid_key(k));
        let memory = store.read(store::MEMORY).map(|t| Memory::from_json(&t)).unwrap_or_default();
        let mut apps = BTreeMap::new();
        if let Some(v) = store.read(APPS).and_then(|t| serde_json::from_str::<Value>(&t).ok())
            && let Some(m) = v.as_object()
        {
            for (id, info) in m.iter() {
                if let Some(info) = info_from_json(info) {
                    apps.insert(id.to_string(), tools::sanitize(info));
                }
            }
        }
        let permissions = store
            .read(store::PERMISSIONS)
            .map(|t| Permissions::from_json(&t, &|k| policy::known_key(k, &apps)))
            .unwrap_or_default();
        let mut timers = Vec::new();
        let mut next_timer = 1;
        if let Some(v) = store.read(store::TIMERS).and_then(|t| serde_json::from_str::<Value>(&t).ok())
            && let Some(list) = v.as_array()
        {
            for t in list {
                if let (Some(id), Some(due)) = (t["id"].as_u64(), t["due"].as_u64()) {
                    let label = notice::untrusted_line(t["label"].as_str().unwrap_or(""), MAX_LABEL);
                    timers.push(Timer { id, label, due });
                    next_timer = next_timer.max(id.saturating_add(1));
                }
            }
        }
        timers.sort_by_key(|t| t.due);
        Shared {
            store,
            config,
            key,
            memory,
            permissions,
            apps,
            timers,
            next_timer,
            config_generation: 0,
            apps_generation: 0,
            changed: Event::create().ok(),
        }
    }

    fn signal_change(&self) {
        if let Some(e) = &self.changed {
            let _ = e.signal();
        }
    }

    /// Stores the settings, and tells the main loop they changed.
    pub fn save_config(&mut self) {
        self.store.write(store::CONFIG, &self.config.to_json());
        self.config_generation += 1;
        self.signal_change();
    }

    /// Stores (or removes) the Deepgram key, and tells the main loop.
    pub fn save_key(&mut self) {
        match &self.key {
            Some(k) => {
                self.store.write(store::KEY, k);
            }
            None => self.store.remove(store::KEY),
        }
        self.config_generation += 1;
        self.signal_change();
    }

    /// Stores what the agent remembers.
    pub fn save_memory(&self) {
        self.store.write(store::MEMORY, &self.memory.to_json());
    }

    /// Stores the actions the user always allows.
    pub fn save_permissions(&self) {
        self.store.write(store::PERMISSIONS, &self.permissions.to_json());
    }

    /// Keeps what application `id` offers ([`tools::sanitize`]d already).
    pub fn set_app(&mut self, id: &str, info: AppAgentInfo) {
        if self.apps.get(id) == Some(&info) {
            return;
        }
        self.apps.insert(id.to_string(), info);
        let mut m = serde_json::Map::new();
        for (k, v) in &self.apps {
            m.insert(k.clone(), info_to_json(v));
        }
        self.store.write(APPS, &Value::Object(m).to_string());
        self.apps_generation += 1;
        self.signal_change();
    }

    /// Adds a timer due at `due` (Unix seconds); returns its id and its
    /// label as kept: one line, without the notices' marker (the language
    /// model chose it, from whatever it was told).
    pub fn add_timer(&mut self, label: &str, due: u64) -> (u64, String) {
        let id = self.next_timer;
        self.next_timer += 1;
        let label = notice::untrusted_line(label, MAX_LABEL);
        self.timers.push(Timer { id, label: label.clone(), due });
        self.timers.sort_by_key(|t| t.due);
        self.save_timers();
        (id, label)
    }

    /// Removes timer `id`; returns it.
    pub fn remove_timer(&mut self, id: u64) -> Option<Timer> {
        let i = self.timers.iter().position(|t| t.id == id)?;
        let t = self.timers.remove(i);
        self.save_timers();
        Some(t)
    }

    /// Stores the timers.
    pub fn save_timers(&self) {
        let list: Vec<Value> =
            self.timers.iter().map(|t| json!({ "id": t.id, "label": t.label.as_str(), "due": t.due })).collect();
        self.store.write(store::TIMERS, &Value::Array(list).to_string());
    }
}
