// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What becomes of function calls: their results go back to the language
//! model; calls that need the user's consent wait for the shell's answer;
//! the outcome of an allowed action is told as a notice, with what the
//! action said as data ([`agent_core::notice`]).
//!
//! Calls the user cancelled by speaking again (`FunctionCallCancelled`) are
//! taken out of the worker's queue if they have not started; one that had
//! started runs to the end, is not answered as a call (Deepgram no longer
//! waits for it), and if it may have changed something, what it did is
//! told. A cancelled call that would have asked the user, or ended the
//! conversation, does nothing.

use agent_core::deepgram;
use agent_core::notice::Notice;
use agent_core::tools::{self, Risk};
use proto::agent::{AgentError, AgentEvent, AgentState, ApprovalRequest};
use rt::println;
use serde_json::{Value, json};

use crate::Agent;
use crate::voice::Voice;
use crate::worker::{Done, Job, Outcome, Pending};

/// An approval request expires after this long.
const APPROVAL_TTL_NS: u64 = 180_000_000_000;

/// An action waiting for the user.
pub struct Approval {
    pub id: u64,
    pub pending: Pending,
    pub created: u64,
}

impl Approval {
    /// The request as the shell shows it.
    pub fn request(&self) -> ApprovalRequest {
        ApprovalRequest {
            id: self.id,
            app: self.pending.app.clone(),
            action: self.pending.action.clone(),
            detail: self.pending.detail.clone(),
            risk: self.pending.risk,
            allow_always: self.pending.risk == Risk::Sensitive,
        }
    }
}

/// A call's result (JSON text) as a value; one that is not JSON is the
/// text.
fn outcome_value(result: &str) -> Value {
    serde_json::from_str(result).unwrap_or_else(|_| Value::String(result.to_string()))
}

impl Agent {
    /// A call finished: its result goes to the language model, an approval
    /// is asked for, or an allowed action's outcome is told.
    pub(crate) fn finished(&mut self, done: Done) {
        let Done { id, name, args, outcome } = done;
        if let Some(label) = self.approved.remove(&id) {
            // An approved action: tell the agent how it went (a result can
            // be a failure: the file was gone by then, say).
            match &outcome {
                Outcome::Result(r) => {
                    let v = outcome_value(r);
                    if v["ok"].as_bool() == Some(false) {
                        let why = v["error"].as_str().unwrap_or("it failed");
                        println!("approved action failed: {why}");
                        self.notice(&Notice::Failed { action: &label, error: why });
                    } else {
                        self.notice(&Notice::Done { action: &label, result: &v["result"] });
                    }
                }
                _ => self.notice(&Notice::NotDone { action: &label }),
            }
            return;
        }
        self.calls_in_flight = self.calls_in_flight.saturating_sub(1);
        if self.session.as_ref().is_some_and(|s| s.cancelled.contains(&id)) {
            if let Outcome::Result(r) = &outcome
                && tools::changes_something(&name, &args)
            {
                println!("{name} ran although the user had moved on");
                self.notice(&Notice::RanAnyway { function: &name, outcome: &outcome_value(r) });
            }
            return;
        }
        let content = match outcome {
            Outcome::Result(r) => r,
            Outcome::End => {
                if let Some(s) = &mut self.session {
                    s.request_end(rt::time::now_ns());
                }
                tools::ok(Value::Null)
            }
            Outcome::Approval(p) => self.ask(p),
            Outcome::Lock => match self.lock_screen() {
                Ok(()) => tools::ok(serde_json::json!({ "locked": true })),
                Err(e) => tools::error(e),
            },
        };
        println!("result {} {}", name, content.chars().take(200).collect::<String>());
        self.send(&deepgram::function_response(&id, &name, &content));
        if self.calls_in_flight == 0 && self.state == AgentState::Thinking {
            // What the agent said before the call may still be playing.
            let speaking = self.voice.as_ref().is_some_and(Voice::speaking);
            self.set_state(if speaking { AgentState::Speaking } else { AgentState::Listening }, "");
        }
    }

    /// The user cancelled these calls by speaking again (see the module
    /// documentation).
    pub(crate) fn cancel_calls(&mut self, ids: Vec<String>) {
        let taken = self.worker.cancel(&ids);
        if !taken.is_empty() {
            println!("{} call(s) cancelled before they ran", taken.len());
        }
        self.calls_in_flight = self.calls_in_flight.saturating_sub(taken.len());
        if let Some(s) = &mut self.session {
            s.cancelled.extend(ids.into_iter().filter(|id| !taken.contains(id)));
        }
    }

    /// Asks the user (through the shell) to approve an action; returns the
    /// function result that tells the agent to wait.
    fn ask(&mut self, p: Pending) -> String {
        if self.ui.is_none() {
            return tools::error("this needs the user's approval on screen, but the screen is not available");
        }
        let id = self.next_approval;
        self.next_approval += 1;
        let label = p.action.clone();
        println!("approval {} requested: {} ({})", id, label, p.detail);
        let approval = Approval { id, pending: p, created: rt::time::now_ns() };
        let request = approval.request();
        self.approvals.push(approval);
        if let Some(ui) = &self.ui
            && !ui.send(AgentEvent::Approval { request })
        {
            self.ui = None;
        }
        self.publish();
        json!({
            "ok": false,
            "waiting_for_approval": true,
            "message": "The system is asking the user to allow this on screen. Tell them briefly; the outcome will follow.",
            "action": label,
        })
        .to_string()
    }

    /// The user's answer to approval request `id` (`always`: allow such
    /// actions for good, if they are sensitive).
    pub(crate) fn decide(&mut self, id: u64, allow: bool, always: bool) -> Result<(), AgentError> {
        let i = self.approvals.iter().position(|a| a.id == id).ok_or(AgentError::NotFound)?;
        let a = self.approvals.remove(i);
        if let Some(ui) = &self.ui {
            let _ = ui.send(AgentEvent::ApprovalDone { id });
        }
        self.publish();
        let label = a.pending.action.clone();
        println!("approval {} {}: {}", id, if allow { "allowed" } else { "declined" }, label);
        if !allow {
            self.notice(&Notice::Declined { action: &label });
            return Ok(());
        }
        if always && a.pending.risk == Risk::Sensitive {
            let mut s = self.shared.lock();
            if s.permissions.allow_always(&a.pending.key, &format!("{}: {}", a.pending.app, label), Risk::Sensitive) {
                s.save_permissions();
            }
        }
        let call_id = format!("approved-{id}");
        self.approved.insert(call_id.clone(), label);
        self.worker.submit(Job { id: call_id, name: a.pending.name, args: a.pending.args, approved: true });
        Ok(())
    }

    /// Withdraws the requests nobody answered in time.
    pub(crate) fn expire_approvals(&mut self, now: u64) {
        let expired: Vec<u64> =
            self.approvals.iter().filter(|a| now - a.created > APPROVAL_TTL_NS).map(|a| a.id).collect();
        for id in expired {
            if let Some(i) = self.approvals.iter().position(|a| a.id == id) {
                let a = self.approvals.remove(i);
                if let Some(ui) = &self.ui {
                    let _ = ui.send(AgentEvent::ApprovalDone { id });
                }
                if self.session.is_some() {
                    self.notice(&Notice::Expired { action: &a.pending.action });
                }
            }
        }
    }
}
