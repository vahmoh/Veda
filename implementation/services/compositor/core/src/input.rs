// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Input drivers' events (`proto::input`), and what each driver's
//! connection holds down. Each event starts the idle time again (see
//! [`crate::idle`]).
//!
//! A driver that goes away (the driver VM restarting, say) cannot release
//! what it held: the keys and buttons it held are released for it, so that
//! no key repeats for ever and no modifier stays down.

use alloc::collections::BTreeMap;

use proto::input::InputEvent;

use crate::wm::Wm;

/// What a driver's connection holds down: how many times each key was
/// pressed and not released (a key held on two of its keyboards), and
/// whether each button is.
#[derive(Debug, Default)]
pub struct Source {
    keys: BTreeMap<u16, u32>,
    buttons: u8,
}

impl<M> Wm<M> {
    /// Driver connection `source` reports `ev` at `now`.
    pub fn input(&mut self, source: u64, ev: InputEvent, now: u64) {
        self.now = now;
        self.active(now);
        match ev {
            InputEvent::Key { code, pressed } => {
                let held = self.sources.entry(source).or_default();
                let n = held.keys.entry(code).or_insert(0);
                *n = if pressed { n.saturating_add(1) } else { n.saturating_sub(1) };
                if *n == 0 {
                    held.keys.remove(&code);
                }
                self.key(code, pressed, now);
            }
            InputEvent::Button { button, pressed } => {
                let bit = 1u8 << button.min(7);
                let held = self.sources.entry(source).or_default();
                if pressed {
                    held.buttons |= bit;
                } else {
                    held.buttons &= !bit;
                }
                self.pointer_button(button, pressed, now);
            }
            InputEvent::Motion { dx, dy } => {
                let (x, y) = self.pointer;
                self.pointer_moved(x.saturating_add(dx), y.saturating_add(dy));
            }
            InputEvent::Absolute { x, y, max_x, max_y } => {
                let s = self.screen();
                let at = |v: u32, max: u32, size: i32| (v.min(max) as i64 * size as i64 / max.max(1) as i64) as i32;
                self.pointer_moved(s.x + at(x, max_x, s.w), s.y + at(y, max_y, s.h));
            }
            InputEvent::Scroll { dx, dy } => self.scroll(dx, dy),
        }
    }

    /// Driver connection `source` went away at `now`: what it held is
    /// released (the switcher closing without a choice, Super's release
    /// opening nothing).
    pub fn source_gone(&mut self, source: u64, now: u64) {
        let Some(held) = self.sources.remove(&source) else { return };
        self.now = now;
        if held.keys.is_empty() && held.buttons == 0 {
            return;
        }
        self.close_switcher(false);
        self.keyboard.forget_tap();
        for (code, n) in held.keys {
            // The keymap counts a modifier held twice at most.
            for _ in 0..n.min(2) {
                self.key(code, false, now);
            }
        }
        for button in 0..8u8 {
            if held.buttons & (1 << button) != 0 {
                self.pointer_button(button, false, now);
            }
        }
    }
}
