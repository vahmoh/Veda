// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `window`: the open windows, arranged through the desktop shell; and
//! what is on the screen, for the agent's instructions. The window system
//! and the shell must answer in time ([`super::SERVICE_TIMEOUT_NS`],
//! [`super::DESKTOP_TIMEOUT_NS`]): one that hangs does not hold up the
//! agent.

use serde_json::{Value, json};
use vproto::display::{WindowInfo, WindowKind};

use super::{Ctx, desktop, window_system};

/// The application windows (not panels or menus), if the window system
/// answers.
fn app_windows() -> Option<Vec<WindowInfo>> {
    let list = window_system()?.list_windows().ok()?;
    Some(list.into_iter().filter(|w| matches!(w.kind, WindowKind::Normal | WindowKind::Borderless)).collect())
}

/// The open windows as the agent reads them.
pub(super) fn windows_json() -> Option<Value> {
    let windows: Vec<Value> = app_windows()?
        .iter()
        .map(|w| {
            json!({
                "app": w.app_id.as_str(),
                "title": w.title.as_str(),
                "in_front": w.focused,
                "state": format!("{:?}", w.state).to_lowercase(),
            })
        })
        .collect();
    Some(Value::Array(windows))
}

/// What is on the screen, for the prompt.
pub fn screen_summary() -> String {
    let Some(list) = app_windows() else { return String::new() };
    let wins: Vec<String> =
        list.iter().map(|w| if w.focused { format!("{} (in front)", w.title) } else { w.title.clone() }).collect();
    if wins.is_empty() { "No windows are open.".into() } else { format!("Open windows: {}.", wins.join(", ")) }
}

impl Ctx {
    /// The id of a window of `app` (or with `app` in its title).
    pub(super) fn find_window(&self, app: &str) -> Option<u32> {
        let q = app.to_lowercase();
        app_windows()?
            .iter()
            .rev()
            .find(|w| w.app_id.to_lowercase() == q || w.title.to_lowercase().contains(&q))
            .map(|w| w.id)
    }

    /// Has the shell do `op` to window `id`.
    pub(super) fn window_action(&self, id: u32, op: &str) -> Result<(), String> {
        let s = desktop().ok_or("the desktop is not available")?;
        match s.window_action(id, op.into()) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(format!("the desktop refused ({e:?})")),
            Err(_) => Err("the desktop did not answer".into()),
        }
    }

    /// `window` (see the module documentation).
    pub(super) fn window(&mut self, args: &Value) -> Result<Value, String> {
        let op = args["operation"].as_str().unwrap_or("list");
        match op {
            "list" => windows_json().ok_or_else(|| "the window system did not answer".into()),
            "show_desktop" => {
                self.window_action(0, "show_desktop")?;
                Ok(Value::Null)
            }
            _ => {
                let target = args["window"].as_str().ok_or("which window?")?;
                let id = self
                    .find_app(target)
                    .and_then(|a| self.find_window(&a.id))
                    .or_else(|| self.find_window(target))
                    .ok_or_else(|| format!("no open window matches {target}"))?;
                self.window_action(id, op)?;
                Ok(Value::Null)
            }
        }
    }
}
