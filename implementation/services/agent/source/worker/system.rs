// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `system` and `tasks`: what the computer is, restarting and shutting it
//! down, and the running programs (of which the agent may end only
//! applications). Locking the screen is the main loop's
//! ([`super::Outcome::Lock`]).

use serde_json::{Value, json};

use super::{Ctx, launcher};

impl Ctx {
    /// `system`: information, restarting and shutting down.
    pub(super) fn system(&self, args: &Value) -> Result<Value, String> {
        let launcher = launcher().ok_or("the launcher is not available")?;
        match args["operation"].as_str().unwrap_or("info") {
            "info" => {
                let info = rt::object::system_info().map_err(|_| "no system information")?;
                Ok(json!({
                    "os": "Veda 0.1",
                    "cpus": info.cpu_count,
                    "memory_total": files::format::human_size(info.total_memory),
                    "memory_free": files::format::human_size(info.free_memory),
                    "uptime_minutes": rt::time::now_ns() / 60_000_000_000,
                }))
            }
            "restart" => {
                let _ = launcher.power(proto::init::power::REBOOT);
                Ok(Value::Null)
            }
            "shutdown" => {
                let _ = launcher.power(proto::init::power::SHUTDOWN);
                Ok(Value::Null)
            }
            other => Err(format!("unknown system operation {other}")),
        }
    }

    /// `tasks`: the running programs, and ending an application.
    pub(super) fn tasks(&self, args: &Value) -> Result<Value, String> {
        let launcher = launcher().ok_or("the launcher is not available")?;
        let tasks = launcher.tasks().map_err(|_| "the launcher did not answer")?;
        match args["operation"].as_str().unwrap_or("list") {
            "list" => Ok(Value::Array(
                tasks
                    .iter()
                    .map(|t| {
                        json!({
                            "name": t.name.as_str(),
                            "id": t.koid,
                            "memory": files::format::human_size(t.memory),
                            "app": t.is_app,
                        })
                    })
                    .collect(),
            )),
            "end" => {
                // Only applications: system services are not the agent's to
                // stop. The program is named exactly (and by its id when
                // several share the name): the approval may come later.
                let name = args["name"].as_str().map(str::to_lowercase);
                let id = args["id"].as_u64();
                let matching: Vec<_> = tasks
                    .iter()
                    .filter(|t| t.is_app)
                    .filter(|t| name.as_ref().is_none_or(|n| t.name.to_lowercase() == *n))
                    .filter(|t| id.is_none_or(|i| t.koid == i))
                    .collect();
                let t = match matching.as_slice() {
                    [] => return Err(format!("no running application called {}", name.as_deref().unwrap_or("that"))),
                    [t] => *t,
                    several => {
                        let ids: Vec<String> = several.iter().map(|t| t.koid.to_string()).collect();
                        return Err(format!("several are running (ids {}); say which by its id", ids.join(", ")));
                    }
                };
                match launcher.kill(t.koid) {
                    Ok(Ok(())) => Ok(json!({ "ended": t.name.as_str() })),
                    _ => Err(format!("{} could not be ended", t.name)),
                }
            }
            other => Err(format!("unknown task operation {other}")),
        }
    }
}
