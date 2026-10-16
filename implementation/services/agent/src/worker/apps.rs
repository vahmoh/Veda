// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Applications: `list_apps`, `open_app`, `app_actions`, `use_app` and
//! `read_app`, and the links to the running ones.
//!
//! An application registers with the agent service, which asks it what it
//! offers (off the path of calls: [`crate::registrar`]) and hands the
//! worker a link to it ([`NewLink`]). A process registering again replaces
//! its previous link; another instance of the application keeps its own,
//! as dropping a link makes that instance register again and take over,
//! and two instances would take turns for ever. The agent works with the
//! newest.

use serde_json::{Value, json};
use vabi::RawHandle;
use vagent::policy;
use vagent::tools::{self, names};
use vipc::IpcError;
use vproto::agent::{AppAgentInfo, agentapp};
use vproto::init::AppInfo;
use vrt::time::{DateTime, Duration};

use super::{Ctx, Outcome, Pending, launcher, show};

/// How long an application may take to answer.
pub const APP_TIMEOUT_NS: u64 = 3_000_000_000;
/// How long an application's action may take (copying a big folder, say);
/// it runs on the application's main thread like the user's own commands.
const INVOKE_TIMEOUT_NS: u64 = 30_000_000_000;
/// How long a starting application has to register.
const START_TIMEOUT: Duration = Duration::from_secs(6);
/// Running instances of one application the agent keeps links to.
const MAX_INSTANCES: usize = 8;

/// An application that registered and described itself.
pub struct NewLink {
    /// The application's id (the registering process's name).
    pub id: String,
    /// The registering process.
    pub koid: u64,
    pub client: agentapp::Client,
}

/// A registered application.
pub(super) struct AppLink {
    client: agentapp::Client,
    channel: RawHandle,
    koid: u64,
}

impl AppLink {
    fn alive(&self) -> bool {
        let mut item =
            vabi::WaitItem { handle: self.channel, signals: vabi::signals::PEER_CLOSED, ..Default::default() };
        let _ = vrt::object::wait_many(core::slice::from_mut(&mut item), 0);
        item.observed & vabi::signals::PEER_CLOSED == 0
    }
}

/// What an application offers, asked of it and made safe to use
/// ([`tools::sanitize`]).
pub fn describe(client: &agentapp::Client) -> Result<AppAgentInfo, IpcError> {
    client.describe().map(tools::sanitize)
}

impl Ctx {
    /// Takes the applications that registered since the last look (see the
    /// module documentation).
    pub(super) fn take_links(&mut self) {
        let new: Vec<NewLink> = core::mem::take(&mut *self.q.links.lock());
        for l in new {
            let channel = l.client.channel().raw();
            let links = self.apps.entry(l.id).or_default();
            links.retain(|x| x.koid != l.koid && x.alive());
            if links.len() == MAX_INSTANCES {
                links.remove(0);
            }
            links.push(AppLink { client: l.client, channel, koid: l.koid });
        }
    }

    /// The link to the newest running instance of application `id`.
    fn link(&self, id: &str) -> Option<&AppLink> {
        self.apps.get(id)?.iter().rev().find(|l| l.alive())
    }

    /// The installed applications.
    fn installed(&self) -> Vec<AppInfo> {
        super::installed_apps()
    }

    /// Finds an installed application by id or name.
    pub(super) fn find_app(&self, query: &str) -> Option<AppInfo> {
        let q = query.trim().to_lowercase();
        let apps = self.installed();
        apps.iter()
            .find(|a| a.id.to_lowercase() == q || a.name.to_lowercase() == q)
            .or_else(|| apps.iter().find(|a| a.name.to_lowercase().contains(&q) || q.contains(&a.name.to_lowercase())))
            .cloned()
    }

    /// The live link to an application, starting it if needed.
    fn ensure_running(&mut self, app: &AppInfo) -> Result<(), String> {
        self.take_links();
        if self.link(&app.id).is_some() {
            return Ok(());
        }
        self.apps.remove(&app.id);
        // An open application that has not registered yet (the agent
        // restarted) registers again on its own: wait for it rather than
        // starting a second copy.
        if self.find_window(&app.id).is_none() {
            let launcher = launcher().ok_or("the launcher is not available")?;
            match launcher.launch_app(app.id.clone(), Vec::new()) {
                Ok(Ok(_)) => {}
                _ => return Err(format!("{} could not be started", app.name)),
            }
            self.note_use(&app.id);
        }
        let end = vrt::time::deadline_after(START_TIMEOUT);
        while vrt::time::now_ns() < end {
            vrt::time::sleep(Duration::from_millis(50));
            self.take_links();
            if self.link(&app.id).is_some() {
                return Ok(());
            }
        }
        Err(format!("{} started but does not take requests from the agent", app.name))
    }

    /// Counts application `app` being opened now (for the agent's memory of
    /// the user's habits).
    pub(super) fn note_use(&self, app: &str) {
        let t = DateTime::now();
        let mut s = self.shared.lock();
        s.memory.note_app_use(app, vrt::time::unix_time_ns() / 1_000_000_000, t.hour as u32);
        s.save_memory();
    }

    /// `list_apps`: the installed applications, whether they run, and their
    /// actions.
    pub(super) fn list_apps(&mut self) -> Value {
        let infos = self.shared.lock().apps.clone();
        let apps: Vec<Value> = self
            .installed()
            .iter()
            .map(|a| {
                let mut v = json!({
                    "id": a.id.as_str(),
                    "name": a.name.as_str(),
                    "about": a.description.as_str(),
                    "running": self.link(&a.id).is_some(),
                });
                if let Some(info) = infos.get(&a.id) {
                    v["actions"] = json!(info.actions.iter().map(|x| Value::from(x.name.as_str())).collect::<Vec<_>>());
                }
                v
            })
            .collect();
        Value::Array(apps)
    }

    fn app_arg(&self, args: &Value) -> Result<AppInfo, String> {
        let q = args["app"].as_str().ok_or("which application?")?;
        self.find_app(q).ok_or_else(|| format!("there is no application called {q}"))
    }

    /// `open_app`: starts an application (with a file), or brings it to the
    /// front.
    pub(super) fn open_app(&mut self, args: &Value) -> Result<Value, String> {
        let app = self.app_arg(args)?;
        let file = args["file"].as_str().filter(|f| !f.trim().is_empty()).map(vagent::files::resolve).transpose()?;
        // An open application without a file to open comes to the front.
        if file.is_none()
            && let Some(id) = self.find_window(&app.id)
        {
            self.window_action(id, "focus")?;
            return Ok(json!({ "brought_to_front": app.name.as_str() }));
        }
        let launcher = launcher().ok_or("the launcher is not available")?;
        let argv: Vec<String> = file.iter().cloned().collect();
        match launcher.launch_app(app.id.clone(), argv) {
            Ok(Ok(_)) => {
                self.note_use(&app.id);
                Ok(json!({ "opened": app.name.as_str(), "file": file.as_deref().map(show) }))
            }
            _ => Err(format!("{} could not be started", app.name)),
        }
    }

    /// `app_actions`: what an application offers, asked of it again.
    pub(super) fn app_actions(&mut self, args: &Value) -> Result<Value, String> {
        let app = self.app_arg(args)?;
        self.ensure_running(&app)?;
        let link = self.link(&app.id).ok_or("the application went away")?;
        let info = describe(&link.client).map_err(|_| format!("{} is not answering", app.name))?;
        let described = tools::describe_actions(&info);
        self.shared.lock().set_app(&app.id, info);
        Ok(described)
    }

    /// `use_app`: runs an action of an application, or asks the user first
    /// if its risk says so.
    pub(super) fn use_app(&mut self, args: &Value, approved: bool) -> Outcome {
        let r = (|| -> Result<Outcome, String> {
            let app = self.app_arg(args)?;
            let action = args["action"].as_str().ok_or("which action?")?.to_string();
            let call_args = match args.get("arguments") {
                Some(v @ Value::Object(_)) => v.clone(),
                Some(Value::String(s)) => serde_json::from_str::<Value>(s)
                    .ok()
                    .filter(|v| v.as_object().is_some())
                    .unwrap_or_else(|| json!({})),
                _ => json!({}),
            };
            self.ensure_running(&app)?;
            let info = self.shared.lock().apps.get(&app.id).cloned();
            let spec = info.as_ref().and_then(|i| i.actions.iter().find(|a| a.name == action)).cloned();
            let Some(spec) = spec else {
                let known: Vec<&str> = info.iter().flat_map(|i| i.actions.iter().map(|a| a.name.as_str())).collect();
                return Err(format!("{} has no action called {action}; it offers: {}", app.name, known.join(", ")));
            };
            let key = policy::key(&app.id, &action);
            if !approved && self.needs_approval(&key, spec.risk) {
                return Ok(Outcome::Approval(Pending {
                    name: names::USE_APP.into(),
                    args: args.clone(),
                    app: app.name.clone(),
                    action: spec.description.clone(),
                    detail: tools::describe_args(&call_args),
                    risk: spec.risk,
                    key,
                }));
            }
            let link = self.link(&app.id).ok_or("the application went away")?;
            link.client.set_timeout(INVOKE_TIMEOUT_NS);
            let result = link.client.invoke(action.clone(), call_args.to_string());
            link.client.set_timeout(APP_TIMEOUT_NS);
            let result = result.map_err(|_| format!("{} did not answer", app.name))?;
            Ok(Outcome::Result(if result.ok {
                tools::ok(serde_json::from_str::<Value>(&result.result).unwrap_or(Value::String(result.result)))
            } else {
                tools::error(&result.result)
            }))
        })();
        r.unwrap_or_else(|e| Outcome::Result(tools::error(&e)))
    }

    /// `read_app`: what an open application shows.
    pub(super) fn read_app(&mut self, args: &Value) -> Result<Value, String> {
        let app = self.app_arg(args)?;
        let link = self.link(&app.id).ok_or_else(|| format!("{} is not open", app.name))?;
        let state = link.client.state().map_err(|_| format!("{} did not answer", app.name))?;
        Ok(serde_json::from_str::<Value>(&state).unwrap_or(Value::String(state)))
    }
}
