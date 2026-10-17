// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The idle lock: the screen locks once no input has come for the time the
//! lock service sets (`display::set_idle_timeout`; never until it does),
//! unless a window keeps it awake.
//!
//! * **The time** counts from the last input from a driver (a key, a
//!   button, the pointer moving, the wheel), the last unlock, or the moment
//!   the screen stopped being kept awake: a film that ends leaves the user
//!   the whole time before the screen locks, not what was left of it.
//!   Setting it again (as the lock service does whenever it reconnects)
//!   does not start it again, so that a service that keeps restarting
//!   cannot hold the lock off.
//! * **The agent** says when the user is there although no input came
//!   (`display::user_active`: they talk with it, hands-free), which counts
//!   as input; nobody else may, since a program could otherwise keep an
//!   unattended computer unlocked for ever. What counts as the user there
//!   is the agent's to decide (`vagent::presence`).
//! * **A window keeps the screen awake** while its client asks
//!   (`display::keep_awake`: a video playing, a slideshow, a game in play)
//!   and while it can be seen: neither minimised nor closing, and not
//!   hidden whole by a window above it. So no application keeps the screen
//!   unlocked from a window the user cannot see; and being full screen is
//!   not enough, so that a film paused and left full screen, or a picture,
//!   does not keep an unattended computer unlocked.
//!
//! The compositor waits until [`Wm::idle_deadline`], and calls
//! [`Wm::check_idle`] after everything it does: it notes when the screen
//! stops being kept awake, and locks it when the time is up.

use vproto::display::{DisplayError, WindowKind};

use crate::clients::Role;
use crate::window::Window;
use crate::wm::Wm;

impl<M> Wm<M> {
    /// `client` sets how long the screen waits without input before it
    /// locks: `seconds` (0: never). Only the lock service may.
    pub fn set_idle_timeout(&mut self, client: u64, seconds: u32) -> Result<(), DisplayError> {
        if self.role(client) != Role::Lock {
            return Err(DisplayError::Denied);
        }
        self.idle_timeout = u64::from(seconds) * 1_000_000_000;
        Ok(())
    }

    /// How long the screen waits without input before it locks (ns; 0:
    /// never).
    pub fn idle_timeout(&self) -> u64 {
        self.idle_timeout
    }

    /// `client` asks that its window `id` keep the screen awake, or no
    /// longer: its normal and borderless windows may.
    pub fn keep_awake(&mut self, client: u64, id: u32, awake: bool) -> Result<(), DisplayError> {
        let w = self.own(client, id)?;
        if !matches!(w.kind, WindowKind::Normal | WindowKind::Borderless) {
            return Err(DisplayError::Invalid);
        }
        if awake {
            self.awake.insert(id);
        } else {
            self.awake.remove(&id);
        }
        Ok(())
    }

    /// Whether a window keeps the screen awake now (see the module's
    /// comment).
    pub fn kept_awake(&self) -> bool {
        self.awake.iter().any(|id| self.windows.get(id).is_some_and(|w| self.seen(w)))
    }

    /// Whether window `w` can be seen: on the screen, neither minimised,
    /// closing nor behind the lock, and not hidden whole by a window above
    /// it.
    fn seen(&self, w: &Window<M>) -> bool {
        if !w.interactive() || !self.shown(w) {
            return false;
        }
        let r = w.client_rect.intersect(&self.screen());
        let Some(at) = self.order.iter().position(|&id| id == w.id) else { return false };
        // The topmost window that hides all of it is itself or one below.
        !r.is_empty() && self.covering(r).is_none_or(|top| top <= at)
    }

    /// When the screen locks for want of input, unless input comes first:
    /// none while it never does, is locked, or is kept awake (whatever ends
    /// that wakes the compositor); at once when it is no longer kept awake,
    /// for [`Wm::check_idle`] to note when that was.
    pub fn idle_deadline(&self) -> Option<u64> {
        if self.idle_timeout == 0 || self.locked || self.kept_awake() {
            None
        } else if self.idle_held {
            Some(0)
        } else {
            Some(self.idle_since.saturating_add(self.idle_timeout))
        }
    }

    /// Locks the screen if no input has come for the idle timeout by
    /// `now`, and nothing keeps it awake; whether it did.
    pub fn check_idle(&mut self, now: u64) -> bool {
        if self.idle_timeout == 0 || self.locked {
            return false;
        }
        if self.kept_awake() {
            self.idle_held = true;
            return false;
        }
        if core::mem::take(&mut self.idle_held) {
            // It stopped being kept awake since the last look: the time
            // counts from now.
            self.active(now);
            return false;
        }
        if now < self.idle_since.saturating_add(self.idle_timeout) {
            return false;
        }
        self.now = now;
        self.lock_now();
        true
    }

    /// `client` says the user is there at `now`, as input would: only the
    /// agent may (see the module's comment). While the screen is locked it
    /// does nothing: the time starts again when the user unlocks it.
    pub fn user_active(&mut self, client: u64, now: u64) -> Result<(), DisplayError> {
        if self.role(client) != Role::Agent {
            return Err(DisplayError::Denied);
        }
        if !self.locked {
            self.active(now);
        }
        Ok(())
    }

    /// The user was there at `now` (input came, they unlocked the screen,
    /// or the agent says so): the idle time starts again.
    pub(crate) fn active(&mut self, now: u64) {
        self.idle_since = self.idle_since.max(now);
    }
}
