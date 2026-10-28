// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `notify` and `timer`: a notification on the screen, and timers and
//! reminders, which wake the agent when they are due (see
//! `crate::timers`). A timer's label is kept as one line without the
//! notices' marker: the language model chose it, from whatever it was told.

use serde_json::{Value, json};

use super::{Ctx, desktop};

impl Ctx {
    /// `notify`: a notification on the screen.
    pub(super) fn notify(&self, args: &Value) -> Result<Value, String> {
        let s = desktop().ok_or("the desktop is not available")?;
        let title = args["title"].as_str().unwrap_or("Reminder");
        let text = args["text"].as_str().unwrap_or("");
        s.notify(title.into(), text.into(), "agent".into()).map_err(|_| "the desktop did not answer")?;
        Ok(Value::Null)
    }

    /// `timer`: set, list and cancel timers.
    pub(super) fn timer(&self, args: &Value) -> Result<Value, String> {
        let now = runtime::time::unix_time_ns() / 1_000_000_000;
        let mut sh = self.shared.lock();
        match args["operation"].as_str().unwrap_or("list") {
            "set" => {
                let label = args["label"].as_str().unwrap_or("Timer");
                let due = if let Some(s) = args["seconds"].as_f64() {
                    // Up to a year ahead.
                    now + s.clamp(1.0, 366.0 * 86_400.0) as u64
                } else if let Some(at) = args["at"].as_str() {
                    let (h, m) = at.split_once(':').ok_or("the time must look like 17:30")?;
                    let (h, m): (u64, u64) =
                        (h.trim().parse().map_err(|_| "bad hour")?, m.trim().parse().map_err(|_| "bad minute")?);
                    if h > 23 || m > 59 {
                        return Err("the time must be between 00:00 and 23:59".into());
                    }
                    let today = now - now % 86_400 + h * 3600 + m * 60;
                    if today > now { today } else { today + 86_400 }
                } else {
                    return Err("when? give seconds or a time".into());
                };
                let (id, label) = sh.add_timer(label, due);
                Ok(json!({ "id": id, "label": label, "due_in_seconds": due - now }))
            }
            "list" => Ok(Value::Array(
                sh.timers
                    .iter()
                    .map(|t| json!({ "id": t.id, "label": t.label.as_str(), "due_in_seconds": t.due.saturating_sub(now) }))
                    .collect(),
            )),
            "cancel" => {
                let id = args["id"].as_u64().or_else(|| {
                    let l = args["label"].as_str()?.to_lowercase();
                    sh.timers.iter().find(|t| t.label.to_lowercase().contains(&l)).map(|t| t.id)
                });
                match id.and_then(|id| sh.remove_timer(id)) {
                    Some(t) => Ok(json!({ "cancelled": t.label.as_str() })),
                    None => Err("no such timer".into()),
                }
            }
            other => Err(format!("unknown timer operation {other}")),
        }
    }
}
