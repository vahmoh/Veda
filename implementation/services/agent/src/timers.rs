// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Timers and reminders coming due: a notification on the screen, and a
//! notice that wakes the agent to tell the user. The conversation such a
//! notice starts is not attended until the user speaks, so what the timer
//! says (its label, which the language model chose) cannot set another
//! timer and keep the agent waking up by itself (see
//! [`crate::conversation`]).

use vagent::notice::Notice;
use vproto::shell::shell;
use vrt::println;

use crate::Agent;
use crate::shared::Timer;

/// How long the shell may take to show a notification (the agent's loop
/// waits for it).
const SHELL_TIMEOUT_NS: u64 = 1_000_000_000;

impl Agent {
    /// Tells of the timers that came due.
    pub(crate) fn check_timers(&mut self) {
        let unix = vrt::time::unix_time_ns() / 1_000_000_000;
        let due: Vec<Timer> = {
            let mut s = self.shared.lock();
            let due: Vec<Timer> = s.timers.iter().filter(|t| t.due <= unix).cloned().collect();
            if !due.is_empty() {
                s.timers.retain(|t| t.due > unix);
                s.save_timers();
            }
            due
        };
        for t in due {
            println!("timer due: {}", t.label);
            if let Some(shell) = vproto::connect(shell::NAME).ok().map(shell::Client::new) {
                shell.set_timeout(SHELL_TIMEOUT_NS);
                let _ = shell.notify(t.label.clone(), "Your reminder is due.".into(), "agent".into());
            }
            self.notice(&Notice::TimerDue { label: &t.label });
        }
    }

    /// When the main loop should look at the timers again at the latest.
    pub(crate) fn next_timer_ns(&self, now: u64) -> u64 {
        let unix = vrt::time::unix_time_ns() / 1_000_000_000;
        match self.shared.lock().timers.first() {
            Some(t) => now + t.due.saturating_sub(unix).min(3600) * 1_000_000_000 + 50_000_000,
            None => vabi::DEADLINE_INFINITE,
        }
    }
}
