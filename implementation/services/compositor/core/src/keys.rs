// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The keyboard: keys go to the focused window, but for the window
//! system's own shortcuts: Alt+Tab (the switcher), Alt+F4 (close),
//! Super+arrows (snap, maximise, restore, minimise), Super+D (the desktop),
//! Super+L (the lock), Super alone (the start menu) and Super+Space (the
//! agent), the last two told to the shell. While the screen is locked, the
//! lock screen hears every key, and no shortcut does anything.

use alloc::vec::Vec;

use proto::display::{WindowEvent, WindowKind, WindowState, modifiers};
use proto::input::keys;

use crate::keymap::KeyOutput;
use crate::switcher::Switcher;
use crate::window::Snap;
use crate::wm::Wm;

impl<M> Wm<M> {
    /// Key `code` (evdev) went down or up at `now`.
    pub fn key(&mut self, code: u16, pressed: bool, now: u64) {
        self.now = now;
        let out = self.keyboard.process(code, pressed, now);
        let mods = out.modifiers;
        let alt = mods & modifiers::ALT != 0;
        let super_ = mods & modifiers::SUPER != 0;
        let tapped = self.keyboard.take_meta_tap(code, pressed);
        if self.locked {
            // The lock screen's alone, as the user typed it (but for Super
            // tapped, which the shell would hear).
            if !tapped {
                self.deliver_key(out);
            }
            return;
        }
        // Super tapped on its own: the start menu.
        if tapped {
            self.send_to_shell(WindowEvent::StartMenuKey {});
            return;
        }
        if pressed && super_ && code == keys::L {
            self.lock_now();
            return;
        }
        if pressed && alt && code == keys::TAB {
            self.switcher_step(mods & modifiers::SHIFT != 0);
            return;
        }
        if self.switcher.is_some() {
            if pressed && code == keys::ESC {
                self.close_switcher(false);
                return;
            }
            if !pressed && matches!(code, keys::LEFTALT | keys::RIGHTALT) {
                // The choice is activated; the Alt release still reaches it below.
                self.close_switcher(true);
            }
        }
        if pressed && alt && code == keys::F4 {
            if let Some(f) = self.focused {
                self.request_close(f);
            }
            return;
        }
        if pressed && super_ && code == keys::SPACE {
            self.send_to_shell(WindowEvent::AgentKey {});
            return;
        }
        if pressed && super_ && matches!(code, keys::LEFT | keys::RIGHT | keys::UP | keys::DOWN | keys::D) {
            if code == keys::D {
                self.toggle_desktop();
            } else {
                self.arrange_focused(code);
            }
            return;
        }
        self.deliver_key(out);
    }

    /// Repeats the key held, if it is due at `now`.
    pub fn repeat(&mut self, now: u64) {
        self.now = now;
        while let Some(out) = self.keyboard.poll_repeat(now) {
            self.deliver_key(out);
        }
    }

    /// When the key held repeats next.
    pub fn repeat_deadline(&self) -> Option<u64> {
        self.keyboard.repeat_deadline()
    }

    fn deliver_key(&mut self, out: KeyOutput) {
        if let Some(f) = self.focused {
            let ev = WindowEvent::Key {
                code: out.code,
                pressed: out.pressed,
                repeat: out.repeat,
                modifiers: out.modifiers,
                text: out.text,
            };
            self.send(f, ev);
        }
    }

    /// Super+arrows: Left/Right snap the focused window to that half of the
    /// screen, Up maximises it, Down restores it or, if it is already in its
    /// normal place, minimises it.
    pub(crate) fn arrange_focused(&mut self, code: u16) {
        let Some(id) = self.focused else { return };
        let Some(w) = self.windows.get(&id) else { return };
        if w.kind != WindowKind::Normal || !w.interactive() {
            return;
        }
        let (state, snapped, resizable) = (w.state, w.snapped.is_some(), w.resizable);
        let area = self.work_area;
        match code {
            keys::LEFT | keys::RIGHT if resizable && state != WindowState::Fullscreen => {
                if state == WindowState::Maximized {
                    self.set_state(id, WindowState::Normal);
                }
                let snap = if code == keys::LEFT { Snap::Left } else { Snap::Right };
                self.apply_snap(id, snap, snap.rect(area));
            }
            keys::UP if resizable => self.set_state(id, WindowState::Maximized),
            keys::DOWN if matches!(state, WindowState::Maximized | WindowState::Fullscreen) => {
                self.set_state(id, WindowState::Normal)
            }
            keys::DOWN if snapped => {
                let Some(w) = self.windows.get_mut(&id) else { return };
                let before = w.paint_bounds();
                w.snapped = None;
                w.client_rect = w.restore_rect;
                let after = w.paint_bounds();
                self.damage.add(before);
                self.damage.add(after);
                self.configure(id);
            }
            keys::DOWN => self.set_state(id, WindowState::Minimized),
            _ => {}
        }
    }

    /// Super+D: minimises every window, or brings them back if the desktop
    /// is already showing.
    pub(crate) fn toggle_desktop(&mut self) {
        let visible: Vec<u32> = self
            .order
            .iter()
            .copied()
            .filter(|id| self.windows.get(id).is_some_and(|w| w.kind == WindowKind::Normal && w.interactive()))
            .collect();
        if visible.is_empty() {
            for id in core::mem::take(&mut self.desktop_shown) {
                self.unminimize(id);
            }
        } else {
            for &id in &visible {
                self.set_state(id, WindowState::Minimized);
            }
            self.desktop_shown = visible;
        }
    }

    /// Alt+Tab: opens the window switcher or moves its selection.
    fn switcher_step(&mut self, backwards: bool) {
        if let Some(s) = &mut self.switcher {
            s.step(backwards);
            let b = s.bounds();
            self.damage.add(b);
            return;
        }
        // Most recently used first (the stacking order keeps them so).
        let windows: Vec<u32> = self
            .order
            .iter()
            .rev()
            .copied()
            .filter(|id| self.windows.get(id).is_some_and(|w| w.kind == WindowKind::Normal && !w.closing))
            .collect();
        self.switchers += 1;
        if let Some(s) = Switcher::new(windows, backwards, self.screen(), self.switchers) {
            self.damage.add(s.bounds());
            self.switcher = Some(s);
        }
    }

    /// Closes the switcher, activating the selected window if `commit`.
    pub(crate) fn close_switcher(&mut self, commit: bool) {
        let Some(s) = self.switcher.take() else { return };
        self.damage.add(s.bounds());
        let Some(id) = s.choice() else { return };
        if !commit || !self.windows.get(&id).is_some_and(|w| !w.closing) {
            return;
        }
        self.unminimize(id);
        self.raise(id);
        self.focus(Some(id));
    }
}
