// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The lock: while the screen is locked, the window system shows only the
//! lock screen's windows (`WindowKind::Lock`, the lock service's), and only
//! they hear the keyboard and the pointer.
//!
//! * **Locking** is at once, whoever asks: the shell, the agent or the
//!   lock service (`display::lock`), the user with Super+L, the window
//!   system as it starts ([`Wm::lock_at_start`]), or no input for a while
//!   ([`crate::idle`]). The pointer's grab, the switcher and the popups
//!   go, and the keyboard goes to the lock screen, or to nobody until one
//!   appears.
//! * **While it is locked**, nothing but a lock screen is drawn
//!   ([`Wm::shown`], [`Wm::covering`]: the window system's backdrop until
//!   one has drawn itself), the pointer reaches nothing else
//!   ([`Wm::window_at`]), nothing else takes the focus, and the window
//!   system's shortcuts do nothing. A lock screen that goes (its service
//!   crashed) leaves the screen locked, and the next one takes the focus.
//! * **Unlocking** is the lock service's alone; the keyboard goes back to
//!   the window that had it, and the idle time starts again.
//!
//! The system's services hear of it (`display::watch_lock`): the lock
//! service shows its screen, the agent stops listening.

use gfx::Rect;
use proto::display::{DisplayError, WindowKind};

use crate::clients::Role;
use crate::window::Window;
use crate::wm::Wm;

impl<M> Wm<M> {
    /// Whether the screen is locked.
    pub fn locked(&self) -> bool {
        self.locked
    }

    /// The window system starts locked, until the lock service unlocks it
    /// (at once if no password is set): a window system restarted after a
    /// crash shows nothing that a lock would hide.
    pub fn lock_at_start(&mut self) {
        self.lock_now();
    }

    /// `client` locks the screen: the shell, the agent and the lock
    /// service may.
    pub fn lock(&mut self, client: u64, now: u64) -> Result<(), DisplayError> {
        self.now = now;
        if !matches!(self.role(client), Role::Shell | Role::Agent | Role::Lock) {
            return Err(DisplayError::Denied);
        }
        self.lock_now();
        Ok(())
    }

    /// `client` unlocks the screen: only the lock service may. The keyboard
    /// goes back to the window that had it (or to the topmost), and the
    /// idle time starts again.
    pub fn unlock(&mut self, client: u64, now: u64) -> Result<(), DisplayError> {
        self.now = now;
        if self.role(client) != Role::Lock {
            return Err(DisplayError::Denied);
        }
        if !self.locked {
            return Ok(());
        }
        self.locked = false;
        self.active(now);
        let screen = self.screen();
        self.damage.add(screen);
        let back = self
            .unlocked_focus
            .take()
            .filter(|id| self.windows.get(id).is_some_and(|w| w.kind != WindowKind::Lock && w.interactive()));
        match back {
            Some(id) => self.focus(Some(id)),
            None => self.focus_top(),
        }
        self.update_hover(false);
        Ok(())
    }

    /// Whether `client` may watch the lock (`display::watch_lock`): the
    /// system's services may.
    pub fn may_watch_lock(&self, client: u64) -> Result<(), DisplayError> {
        if self.clients.get(&client).is_some_and(|c| c.identity.service) { Ok(()) } else { Err(DisplayError::Denied) }
    }

    /// Locks the screen now (see the module's comment).
    pub(crate) fn lock_now(&mut self) {
        if self.locked {
            return;
        }
        self.unlocked_focus = self.focus_root();
        self.locked = true;
        self.close_switcher(false);
        self.end_drag();
        let screen = self.screen();
        self.damage.add(screen);
        // The focus moves (the popups close with it) to the lock screen, or
        // to nobody until one appears.
        let lock = self.top_lock();
        self.focus(lock);
        self.update_hover(false);
    }

    /// The topmost lock screen the keyboard can reach.
    pub(crate) fn top_lock(&self) -> Option<u32> {
        self.order
            .iter()
            .rev()
            .copied()
            .find(|id| self.windows.get(id).is_some_and(|w| w.kind == WindowKind::Lock && w.interactive()))
    }

    /// Whether window `w` is on the screen: drawn, and while the screen is
    /// locked, a lock screen.
    pub fn shown(&self, w: &Window<M>) -> bool {
        w.visible() && (!self.locked || w.kind == WindowKind::Lock)
    }

    /// Whether window `w` may have the keyboard and the pointer: while the
    /// screen is locked, only a lock screen may.
    pub(crate) fn reachable(&self, w: &Window<M>) -> bool {
        !self.locked || w.kind == WindowKind::Lock
    }

    /// Where in the paint order drawing `r` starts: at the topmost window
    /// shown that hides all of it (nothing below it can be seen), or, for
    /// `None`, at the backdrop.
    pub fn covering(&self, r: Rect) -> Option<usize> {
        self.order
            .iter()
            .rposition(|id| self.windows.get(id).is_some_and(|w| self.shown(w) && w.opaque_rect().contains_rect(&r)))
    }

    /// Whether what the screen shows has drawn itself: a lock screen while
    /// it is locked, the desktop otherwise ([`Wm::desktop_drawn`]).
    pub fn screen_drawn(&self) -> bool {
        if self.locked {
            self.windows.values().any(|w| w.kind == WindowKind::Lock && w.surface.size().is_some())
        } else {
            self.desktop_drawn()
        }
    }
}
