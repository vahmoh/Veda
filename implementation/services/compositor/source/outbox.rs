// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The events on their way to a window's client.
//!
//! A window's events go out on its event channel, which holds a few hundred
//! messages. A client that falls behind (busy, or hung) fills it; what does
//! not fit waits here, and goes out as the client reads, in order. Nothing
//! that carries state is lost meanwhile, and what waits stays small:
//!
//! * the latest size and state (`Configure`), focus and frame
//!   acknowledgement replace any of their kind still waiting, and a
//!   notification already waiting (`CloseRequested`, `WindowsChanged`, ...)
//!   is not queued twice;
//! * pointer motion and scrolling are merged into the motion or scroll
//!   waiting last;
//! * once [`LIMIT`] events wait, a key or button pressed is dropped, and its
//!   release with it, while a release whose press went out always follows
//!   it: a client never sees a key or a button held for ever, nor a release
//!   without its press. Repeats are dropped as soon as anything waits.

use alloc::collections::{BTreeSet, VecDeque};

use proto::display::WindowEvent;

/// How many events wait before presses are dropped.
pub const LIMIT: usize = 128;

/// What became of an event written to the client's channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Write {
    /// It is on the channel.
    Sent,
    /// The channel is full: it waits.
    Full,
    /// The client closed the channel: nothing more goes out.
    Gone,
}

/// A window's events that did not fit on its channel yet.
#[derive(Debug, Default)]
pub struct Outbox {
    queue: VecDeque<WindowEvent>,
    /// Keys and buttons whose presses were kept: their releases are too.
    keys: BTreeSet<u16>,
    buttons: BTreeSet<u8>,
    /// Keys and buttons whose presses were dropped: so are their releases.
    dropped_keys: BTreeSet<u16>,
    dropped_buttons: BTreeSet<u8>,
    /// The client closed its channel.
    gone: bool,
}

impl Outbox {
    /// Whether nothing waits.
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// How many events wait.
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    /// The events waiting, oldest first.
    pub fn events(&self) -> impl Iterator<Item = &WindowEvent> {
        self.queue.iter()
    }

    /// Queues `ev` behind what waits, as the module's comment says.
    pub fn push(&mut self, ev: WindowEvent) {
        if self.gone {
            return;
        }
        let full = self.queue.len() >= LIMIT;
        match ev {
            WindowEvent::Key { code, pressed: true, repeat, .. } => {
                if repeat {
                    if self.queue.is_empty() {
                        self.queue.push_back(ev);
                    }
                } else if full {
                    self.dropped_keys.insert(code);
                } else {
                    self.dropped_keys.remove(&code);
                    self.keys.insert(code);
                    self.queue.push_back(ev);
                }
            }
            WindowEvent::Key { code, pressed: false, .. } => {
                if !self.dropped_keys.remove(&code) {
                    self.keys.remove(&code);
                    self.queue.push_back(ev);
                }
            }
            WindowEvent::PointerButton { button, pressed: true, .. } => {
                if full {
                    self.dropped_buttons.insert(button);
                } else {
                    self.dropped_buttons.remove(&button);
                    self.buttons.insert(button);
                    self.queue.push_back(ev);
                }
            }
            WindowEvent::PointerButton { button, pressed: false, .. } => {
                if !self.dropped_buttons.remove(&button) {
                    self.buttons.remove(&button);
                    self.queue.push_back(ev);
                }
            }
            WindowEvent::PointerMove { .. } => match self.queue.back_mut() {
                Some(last @ WindowEvent::PointerMove { .. }) => *last = ev,
                _ if !full => self.queue.push_back(ev),
                _ => {}
            },
            WindowEvent::Scroll { dx, dy } => match self.queue.back_mut() {
                Some(WindowEvent::Scroll { dx: x, dy: y }) => {
                    *x = x.saturating_add(dx);
                    *y = y.saturating_add(dy);
                }
                _ if !full => self.queue.push_back(ev),
                _ => {}
            },
            WindowEvent::PointerLeave {} => {
                if self.queue.back() != Some(&ev) {
                    self.queue.push_back(ev);
                }
            }
            WindowEvent::Configure { .. } | WindowEvent::Focus { .. } | WindowEvent::FrameDone { .. } => {
                let kind = core::mem::discriminant(&ev);
                self.queue.retain(|e| core::mem::discriminant(e) != kind);
                self.queue.push_back(ev);
            }
            WindowEvent::CloseRequested {}
            | WindowEvent::WindowsChanged {}
            | WindowEvent::StartMenuKey {}
            | WindowEvent::AgentKey {}
            | WindowEvent::Appearing {} => {
                if !self.queue.contains(&ev) {
                    self.queue.push_back(ev);
                }
            }
        }
    }

    /// Writes what waits, oldest first, while `write` takes it. Whether all
    /// of it went out (or the client is gone).
    pub fn flush(&mut self, mut write: impl FnMut(&WindowEvent) -> Write) -> bool {
        while let Some(ev) = self.queue.front() {
            match write(ev) {
                Write::Sent => {
                    self.queue.pop_front();
                }
                Write::Full => return false,
                Write::Gone => {
                    self.gone = true;
                    self.queue.clear();
                }
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;
    use alloc::vec::Vec;
    use proto::display::WindowState;

    fn key(code: u16, pressed: bool) -> WindowEvent {
        WindowEvent::Key { code, pressed, repeat: false, modifiers: 0, text: String::new() }
    }

    fn button(button: u8, pressed: bool) -> WindowEvent {
        WindowEvent::PointerButton { x: 0, y: 0, button, pressed, clicks: 1 }
    }

    fn drain(o: &mut Outbox) -> Vec<WindowEvent> {
        let mut out = Vec::new();
        assert!(o.flush(|e| {
            out.push(e.clone());
            Write::Sent
        }));
        out
    }

    #[test]
    fn events_go_out_in_order_once_there_is_room() {
        let mut o = Outbox::default();
        o.push(key(30, true));
        o.push(key(30, false));
        assert!(!o.flush(|_| Write::Full));
        assert_eq!(o.len(), 2);
        assert_eq!(drain(&mut o), [key(30, true), key(30, false)]);
        assert!(o.is_empty());
    }

    #[test]
    fn motion_and_state_are_merged() {
        let mut o = Outbox::default();
        o.push(WindowEvent::PointerMove { x: 1, y: 1 });
        o.push(WindowEvent::PointerMove { x: 2, y: 3 });
        o.push(WindowEvent::Configure { width: 10, height: 10, state: WindowState::Normal });
        o.push(WindowEvent::Scroll { dx: 0, dy: 1 });
        o.push(WindowEvent::Scroll { dx: 0, dy: 2 });
        o.push(WindowEvent::Configure { width: 20, height: 10, state: WindowState::Maximized });
        o.push(WindowEvent::CloseRequested {});
        o.push(WindowEvent::CloseRequested {});
        assert_eq!(
            drain(&mut o),
            [
                WindowEvent::PointerMove { x: 2, y: 3 },
                WindowEvent::Scroll { dx: 0, dy: 3 },
                WindowEvent::Configure { width: 20, height: 10, state: WindowState::Maximized },
                WindowEvent::CloseRequested {},
            ]
        );
    }

    #[test]
    fn a_client_far_behind_loses_presses_but_never_releases() {
        let mut o = Outbox::default();
        o.push(button(0, true));
        for i in 0..LIMIT as u16 {
            o.push(key(100 + i % 2, i % 2 == 0));
        }
        assert!(o.len() >= LIMIT);
        // Its queue is full: a new press is dropped, and its release too.
        o.push(key(30, true));
        o.push(key(30, false));
        // A release whose press went out still goes out.
        o.push(button(0, false));
        let out = drain(&mut o);
        assert!(!out.contains(&key(30, true)) && !out.contains(&key(30, false)));
        assert_eq!(out.last(), Some(&button(0, false)));
        // Room again: presses go out.
        o.push(key(30, true));
        assert_eq!(drain(&mut o), [key(30, true)]);
    }

    #[test]
    fn repeats_wait_for_nobody() {
        let mut o = Outbox::default();
        let repeat = WindowEvent::Key { code: 30, pressed: true, repeat: true, modifiers: 0, text: "a".into() };
        o.push(repeat.clone());
        o.push(repeat.clone());
        assert_eq!(drain(&mut o), [repeat]);
    }

    #[test]
    fn nothing_goes_to_a_client_that_left() {
        let mut o = Outbox::default();
        o.push(key(30, true));
        assert!(o.flush(|_| Write::Gone));
        o.push(key(31, true));
        assert!(o.is_empty());
    }
}
