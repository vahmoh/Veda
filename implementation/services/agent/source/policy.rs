// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! When the agent must ask the user first.
//!
//! * [`Risk::Routine`] actions run without asking.
//! * [`Risk::Sensitive`] actions ask, unless the user chose "always allow"
//!   for that action ([`Permissions`]).
//! * [`Risk::Destructive`] actions always ask.
//!
//! The decision is the agent service's, enforced before anything runs; the
//! language model cannot skip it.
//!
//! The permissions are stored with the agent's other state; what is read
//! back is checked like anything read from a file ([`known_key`]): only a
//! sensitive action the agent knows of can be allowed for good.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use proto::agent::AppAgentInfo;
use serde_json::{Value, json};

use crate::tools::{self, Risk};

/// The permission key of an action: `"files.write"`, `"editor.save"`.
pub fn key(app: &str, action: &str) -> String {
    alloc::format!("{app}.{action}")
}

/// Who acts for the system functions in permission keys
/// (`system.files.write`).
pub const SYSTEM: &str = "system";

/// Whether `key` names an action that can be allowed for good: a sensitive
/// operation of a system function, or a sensitive action that an
/// application (in `apps`, what each offers) describes.
pub fn known_key(key: &str, apps: &BTreeMap<String, AppAgentInfo>) -> bool {
    let Some((app, action)) = key.split_once('.') else { return false };
    if app == SYSTEM {
        return action.split_once('.').is_some_and(|(name, op)| tools::may_always_allow(name, op));
    }
    apps.get(app).is_some_and(|info| info.actions.iter().any(|a| a.name == action && a.risk == Risk::Sensitive))
}

/// How often one process may offer the agent its abilities
/// (`register_app`). An application registers once, and again only after
/// the agent service restarted; any program may register, and each
/// registration costs the agent a question to the program and may rewrite
/// the catalogue of applications, so a process registering again and again
/// is refused for the rest of a window. Processes are told apart by their
/// id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistrationLimit {
    max: u32,
    window_ns: u64,
    /// By process: when its window began, and its registrations in it.
    seen: BTreeMap<u64, (u64, u32)>,
}

impl RegistrationLimit {
    /// At most `max` registrations of one process in `window_ns`.
    pub fn new(max: u32, window_ns: u64) -> RegistrationLimit {
        RegistrationLimit { max, window_ns, seen: BTreeMap::new() }
    }

    /// Whether process `koid` may register at `now` (counted if so).
    pub fn admit(&mut self, koid: u64, now: u64) -> bool {
        // Windows that are over are forgotten, which bounds what is kept.
        let window = self.window_ns;
        self.seen.retain(|_, (start, _)| now.saturating_sub(*start) < window);
        let (_, count) = self.seen.entry(koid).or_insert((now, 0));
        if *count >= self.max {
            return false;
        }
        *count += 1;
        true
    }
}

/// Actions the user always allows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Permissions {
    /// Key to label ("Files: replace files").
    always: BTreeMap<String, String>,
}

impl Permissions {
    pub fn new() -> Permissions {
        Permissions::default()
    }

    /// Whether running the action needs the user's approval.
    pub fn needs_approval(&self, key: &str, risk: Risk) -> bool {
        match risk {
            Risk::Routine => false,
            Risk::Sensitive => !self.always.contains_key(key),
            Risk::Destructive => true,
        }
    }

    /// Remembers "always allow" (only sensitive actions can be).
    pub fn allow_always(&mut self, key: &str, label: &str, risk: Risk) -> bool {
        if risk != Risk::Sensitive || key.is_empty() || key.len() > 120 {
            return false;
        }
        self.always.insert(key.to_string(), label.chars().take(120).collect());
        true
    }

    pub fn revoke(&mut self, key: &str) -> bool {
        self.always.remove(key).is_some()
    }

    /// (key, label) of every permission.
    pub fn list(&self) -> Vec<(String, String)> {
        self.always.iter().map(|(k, l)| (k.clone(), l.clone())).collect()
    }

    /// The permissions as stored: indented JSON.
    pub fn to_json(&self) -> String {
        let items: Vec<Value> =
            self.always.iter().map(|(k, l)| json!({ "key": k.as_str(), "label": l.as_str() })).collect();
        // Strings always serialize.
        serde_json::to_string_pretty(&json!({ "always": items })).unwrap_or_default()
    }

    /// Reads stored permissions, keeping those whose key `known` accepts
    /// (see [`known_key`]).
    pub fn from_json(text: &str, known: &dyn Fn(&str) -> bool) -> Permissions {
        let mut p = Permissions::new();
        if let Ok(v) = serde_json::from_str::<Value>(text)
            && let Some(items) = v["always"].as_array()
        {
            for i in items {
                if let Some(k) = i["key"].as_str().filter(|k| known(k)) {
                    p.allow_always(k, i["label"].as_str().unwrap_or(k), Risk::Sensitive);
                }
            }
        }
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decides_by_risk_and_permission() {
        let mut p = Permissions::new();
        let k = key("files", "write");
        assert!(!p.needs_approval(&k, Risk::Routine));
        assert!(p.needs_approval(&k, Risk::Sensitive));
        assert!(p.needs_approval(&k, Risk::Destructive));
        assert!(p.allow_always(&k, "Files: replace files", Risk::Sensitive));
        assert!(!p.needs_approval(&k, Risk::Sensitive));
        // Destructive actions can never be allowed for good.
        assert!(!p.allow_always("files.delete", "x", Risk::Destructive));
        assert!(p.needs_approval("files.delete", Risk::Destructive));
        let back = Permissions::from_json(&p.to_json(), &|_| true);
        assert_eq!(back, p);
        assert!(p.revoke(&k));
        assert!(p.needs_approval(&k, Risk::Sensitive));
        assert_eq!(Permissions::from_json("nonsense", &|_| true), Permissions::new());
    }

    #[test]
    fn a_process_registering_again_and_again_is_refused() {
        const MIN: u64 = 60_000_000_000;
        let mut l = RegistrationLimit::new(3, MIN);
        assert!(l.admit(7, 0) && l.admit(7, 1) && l.admit(7, 2));
        assert!(!l.admit(7, 3) && !l.admit(7, MIN - 1));
        // Others are not held up, and a new window starts afresh.
        assert!(l.admit(8, 4));
        assert!(l.admit(7, MIN) && l.admit(7, MIN + 1));
        // Over windows are forgotten.
        assert!(l.admit(9, 10 * MIN));
        assert_eq!(l.seen.len(), 1);
    }

    #[test]
    fn only_known_sensitive_actions_are_read_back() {
        use proto::agent::ActionSpec;
        let action = |name: &str, risk| ActionSpec {
            name: name.into(),
            description: String::new(),
            params: alloc::vec![],
            risk,
        };
        let mut apps = BTreeMap::new();
        apps.insert(
            "terminal".to_string(),
            AppAgentInfo {
                summary: String::new(),
                actions: alloc::vec![action("run_command", Risk::Sensitive), action("clear", Risk::Routine)],
                has_state: true,
            },
        );
        apps.insert(
            "taskmgr".to_string(),
            AppAgentInfo {
                summary: String::new(),
                actions: alloc::vec![action("end_task", Risk::Destructive)],
                has_state: true,
            },
        );
        assert!(known_key("terminal.run_command", &apps));
        assert!(known_key("system.files.write", &apps) && known_key("system.wifi.off", &apps));
        for k in [
            "terminal.clear",
            "taskmgr.end_task",
            "editor.save",
            "system.files.empty_trash",
            "system.system.shutdown",
            "system.memory.forget_all",
            "system.nonsense.write",
            "system.files",
            "nonsense",
            "",
        ] {
            assert!(!known_key(k, &apps), "{k}");
        }
        // A planted file allows nothing it should not.
        let planted = r#"{"always":[{"key":"terminal.run_command","label":"Terminal"},
            {"key":"taskmgr.end_task","label":"End tasks"},{"key":"system.files.empty_trash","label":"Empty"},
            {"key":"anything.at_all","label":"x"}]}"#;
        let p = Permissions::from_json(planted, &|k| known_key(k, &apps));
        assert_eq!(p.list(), alloc::vec![("terminal.run_command".to_string(), "Terminal".to_string())]);
    }
}
