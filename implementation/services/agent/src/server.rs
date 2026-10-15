// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The `agent` protocol for programs other than Settings, served on the
//! main loop. Who may do what is decided by the registry's identity of each
//! connection: any program may ask for the status and register its
//! abilities (through the [`crate::registrar`]); only the desktop shell
//! attaches the interface, answers approvals, wakes the agent, puts it to
//! sleep and mutes it. Settings' connections are handed to Settings' own
//! thread ([`crate::admin`]).

use alloc::string::String;
use alloc::vec::Vec;

use vagent::deepgram;
use vproto::agent::{
    AgentConfig, AgentError, AgentEvent, AgentStatus, MemoryItem, ModelCatalog, Permission, UiLink, agent,
};
use vproto::init::ClientIdentity;
use vrt::object::Channel;
use vrt::println;

use crate::Agent;
use crate::ui::Ui;

/// A client connection.
pub struct Conn {
    pub channel: Channel,
    who: ClientIdentity,
}

impl Agent {
    /// Takes the new connections (Settings' to its own thread).
    pub(crate) fn accept(&mut self, listener: &Channel) {
        while let Some((channel, who)) = vproto::accept_with_identity(listener) {
            if who.app && who.name == "settings" {
                self.settings.add(channel, who);
                continue;
            }
            self.next_key += 1;
            self.conns.insert(self.next_key, Conn { channel, who });
        }
    }

    /// Serves every request waiting on connection `key`; `false` once it
    /// closed.
    pub(crate) fn serve(&mut self, key: u64) -> bool {
        loop {
            let Some(c) = self.conns.get(&key) else { return false };
            match c.channel.read() {
                Ok(msg) => {
                    let who = c.who.clone();
                    let reply = agent::dispatch(&mut Client { agent: self, who }, msg);
                    if let (Ok(reply), Some(c)) = (reply, self.conns.get(&key)) {
                        let _ = reply.send(&c.channel);
                    }
                }
                Err(vabi::Error::ShouldWait) => return true,
                Err(_) => return false,
            }
        }
    }

    /// Connection `key` closed.
    pub(crate) fn disconnect(&mut self, key: u64) {
        if let Some(c) = self.conns.remove(&key)
            && c.who.name == "shell"
            && c.who.service
        {
            self.ui = None;
        }
    }
}

/// Requests from programs other than Settings.
struct Client<'a> {
    agent: &'a mut Agent,
    who: ClientIdentity,
}

impl Client<'_> {
    fn shell(&self) -> Result<(), AgentError> {
        if self.who.service && self.who.name == "shell" { Ok(()) } else { Err(AgentError::Denied) }
    }
}

impl agent::Server for Client<'_> {
    fn status(&mut self) -> AgentStatus {
        self.agent.status()
    }

    fn register_app(&mut self, app: Channel) -> Result<(), AgentError> {
        self.agent.registrar.register(&self.who, app)
    }

    fn attach_ui(&mut self) -> Result<UiLink, AgentError> {
        self.shell()?;
        let (ui, link) = Ui::attach()?;
        self.agent.ui = Some(ui);
        println!("interface attached");
        self.agent.publish();
        // Approvals that were waiting when the shell (re)started.
        let requests: Vec<_> = self.agent.approvals.iter().map(|a| a.request()).collect();
        if let Some(ui) = &self.agent.ui {
            for request in requests {
                ui.send(AgentEvent::Approval { request });
            }
        }
        Ok(link)
    }

    fn decide(&mut self, id: u64, allow: bool, always: bool) -> Result<(), AgentError> {
        self.shell()?;
        self.agent.decide(id, allow, always)
    }

    fn wake(&mut self) -> Result<(), AgentError> {
        self.shell()?;
        if self.agent.shared.lock().key.is_none() && deepgram::is_deepgram(&self.agent.endpoint) {
            self.agent.rest();
            return Err(AgentError::NoKey);
        }
        self.agent.wake("asked", true);
        Ok(())
    }

    fn sleep(&mut self) -> Result<(), AgentError> {
        self.shell()?;
        self.agent.end_conversation(None);
        Ok(())
    }

    fn set_window_open(&mut self, open: bool) -> Result<(), AgentError> {
        self.shell()?;
        self.agent.window_open = open;
        Ok(())
    }

    fn set_muted(&mut self, muted: bool) -> Result<(), AgentError> {
        self.shell()?;
        self.agent.muted = muted;
        println!("microphone {}", if muted { "muted" } else { "on" });
        if let Some(v) = &mut self.agent.voice {
            if muted {
                v.close_mic();
            } else if self.agent.session.is_some() {
                v.open_mic();
            }
        }
        self.agent.update_listening();
        self.agent.publish();
        Ok(())
    }

    fn config(&mut self) -> Result<AgentConfig, AgentError> {
        Err(AgentError::Denied)
    }

    fn set_config(&mut self, _config: AgentConfig) -> Result<(), AgentError> {
        Err(AgentError::Denied)
    }

    fn set_api_key(&mut self, _key: String) -> Result<(), AgentError> {
        Err(AgentError::Denied)
    }

    fn check_key(&mut self) -> Result<String, AgentError> {
        Err(AgentError::Denied)
    }

    fn catalog(&mut self) -> Result<ModelCatalog, AgentError> {
        Err(AgentError::Denied)
    }

    fn preview_voice(&mut self, _voice: String) -> Result<(), AgentError> {
        Err(AgentError::Denied)
    }

    fn memories(&mut self) -> Result<Vec<MemoryItem>, AgentError> {
        Err(AgentError::Denied)
    }

    fn forget(&mut self, _id: u64) -> Result<(), AgentError> {
        Err(AgentError::Denied)
    }

    fn forget_all(&mut self) -> Result<(), AgentError> {
        Err(AgentError::Denied)
    }

    fn permissions(&mut self) -> Result<Vec<Permission>, AgentError> {
        Err(AgentError::Denied)
    }

    fn revoke(&mut self, _key: String) -> Result<(), AgentError> {
        Err(AgentError::Denied)
    }
}
