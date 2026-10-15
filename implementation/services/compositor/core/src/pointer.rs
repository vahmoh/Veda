// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The pointer: what it is over, clicks and focus, moving and resizing
//! windows, and the grab that holds it from a button's press to its
//! release.
//!
//! While a button is held the pointer is grabbed: by the window manager
//! when the press began a move, a resize or a caption button's press, by
//! the client's window it was pressed in otherwise. A client's grab hears
//! every button pressed or released until none is held, wherever the
//! pointer is; the window manager's ends when the button that began it is
//! released, other buttons doing nothing meanwhile. A press while the
//! pointer is grabbed is never taken for a new one elsewhere.

use vproto::display::{Cursor, WindowEvent, WindowKind, WindowState};

use crate::window::{Part, Snap, TITLE_HEIGHT};
use crate::wm::{Click, Drag, Wm};

/// Presses closer together than this count as a double (or triple) click.
pub const DOUBLE_CLICK_NS: u64 = 400_000_000;

impl<M> Wm<M> {
    /// The topmost window the pointer reaches at the point, and the part of
    /// it there.
    pub fn window_at(&self, x: i32, y: i32) -> Option<(u32, Part)> {
        self.order.iter().rev().find_map(|id| {
            let w = self.windows.get(id).filter(|w| w.interactive())?;
            w.hit(x, y).map(|part| (*id, part))
        })
    }

    /// The pointer moved to (`x`, `y`) (kept on the screen).
    pub fn pointer_moved(&mut self, x: i32, y: i32) {
        let s = self.screen();
        let (x, y) = (x.clamp(s.x, s.right() - 1), y.clamp(s.y, s.bottom() - 1));
        if (x, y) == self.pointer {
            return;
        }
        self.pointer = (x, y);
        match self.drag {
            Some(Drag::Move { id, button, origin, restore: true, .. }) => {
                if (x - origin.0).abs() + (y - origin.1).abs() >= 6 {
                    self.unsnap_for_drag(id, button, origin, (x, y));
                }
            }
            Some(Drag::Move { id, dx, dy, .. }) => {
                let area = self.work_area;
                if let Some(w) = self.windows.get_mut(&id) {
                    let before = w.paint_bounds();
                    let ny = (y - dy).max(area.y + TITLE_HEIGHT).min(area.bottom() - 8);
                    w.client_rect = vgfx::Rect::new(x - dx, ny, w.client_rect.w, w.client_rect.h);
                    w.auto_placed = false;
                    let after = w.paint_bounds();
                    self.damage.add(before);
                    self.damage.add(after);
                }
                self.update_snap(x, y);
            }
            Some(Drag::Resize { id, edge, start, px, py, .. }) => self.resize(id, edge, start, (x - px, y - py)),
            Some(Drag::Client { id }) => {
                if let Some(c) = self.windows.get(&id).map(|w| w.client_rect) {
                    self.send(id, WindowEvent::PointerMove { x: x - c.x, y: y - c.y });
                }
            }
            Some(Drag::Button { .. }) | None => self.update_hover(true),
        }
    }

    /// Window `id`, resized from `start` by `edge`, the pointer having
    /// moved by `delta`: no smaller than its client allows, no larger than
    /// the screen.
    fn resize(&mut self, id: u32, edge: (i8, i8), start: vgfx::Rect, (ddx, ddy): (i32, i32)) {
        let max = self.limits.window();
        let Some(w) = self.windows.get_mut(&id) else { return };
        let before = w.paint_bounds();
        let mut r = start;
        if edge.0 < 0 {
            let nw = (start.w - ddx).max(w.min_w).min(max.0);
            r.x = start.right() - nw;
            r.w = nw;
        } else if edge.0 > 0 {
            r.w = (start.w + ddx).max(w.min_w).min(max.0);
        }
        if edge.1 < 0 {
            let nh = (start.h - ddy).max(w.min_h).min(max.1);
            r.y = start.bottom() - nh;
            r.h = nh;
        } else if edge.1 > 0 {
            r.h = (start.h + ddy).max(w.min_h).min(max.1);
        }
        if r == w.client_rect {
            return;
        }
        w.client_rect = r;
        w.auto_placed = false;
        let after = w.paint_bounds();
        self.damage.add(before);
        self.damage.add(after);
        self.configure(id);
    }

    /// Updates where the window being moved snaps if released with the
    /// pointer at (`x`, `y`), and its preview.
    fn update_snap(&mut self, x: i32, y: i32) {
        let area = self.work_area;
        let Some(Drag::Move { id, snap: old, .. }) = self.drag else { return };
        let resizable = self.windows.get(&id).is_some_and(|w| w.resizable);
        let zone = match () {
            _ if !resizable => None,
            _ if y <= area.y + 1 => Some(Snap::Maximize),
            _ if x <= area.x + 1 => Some(Snap::Left),
            _ if x >= area.right() - 2 => Some(Snap::Right),
            _ => None,
        };
        let target = zone.map(|z| (z, z.rect(area)));
        if target == old {
            return;
        }
        for (_, r) in [old, target].into_iter().flatten() {
            self.damage.add(r.inflate(8));
        }
        if let Some(Drag::Move { snap, .. }) = &mut self.drag {
            *snap = target;
        }
    }

    /// Works out what the pointer is over: the window it is over hears
    /// where it is if it `moved`, or if that window (or part of it) has
    /// just come under it, and the cursor becomes that window's.
    pub(crate) fn update_hover(&mut self, moved: bool) {
        let (x, y) = self.pointer;
        let hit = self.window_at(x, y);
        let entered = hit != self.hover_hit;
        self.hover_hit = hit;
        let window = hit.map(|(id, _)| id);
        if window != self.hover_window {
            if let Some(old) = self.hover_window {
                self.send(old, WindowEvent::PointerLeave {});
            }
            self.hover_window = window;
        }
        let button = hit.filter(|(_, p)| matches!(p, Part::Close | Part::Maximize | Part::Minimize));
        if button != self.decor.hover {
            for (id, _) in [self.decor.hover, button].into_iter().flatten() {
                self.damage_title(id);
            }
            self.decor.hover = button;
        }
        self.cursor = match hit {
            Some((id, Part::Client)) => match self.windows.get(&id).map(|w| (w.client_rect, w.cursor)) {
                Some((c, cursor)) => {
                    if moved || entered {
                        self.send(id, WindowEvent::PointerMove { x: x - c.x, y: y - c.y });
                    }
                    cursor
                }
                None => Cursor::Arrow,
            },
            Some((_, Part::Edge(dx, dy))) => match (dx, dy) {
                (0, _) => Cursor::ResizeVertical,
                (_, 0) => Cursor::ResizeHorizontal,
                (a, b) if a == b => Cursor::ResizeDiagonal,
                _ => Cursor::ResizeAntiDiagonal,
            },
            _ => Cursor::Arrow,
        };
    }

    /// A window that came or went under the pointer, or changed, while the
    /// pointer stayed put: the hover and the cursor follow it (a game
    /// opening full screen under the pointer hides it).
    pub fn refresh_hover(&mut self) {
        let (x, y) = self.pointer;
        if self.drag.is_none() && self.window_at(x, y) != self.hover_hit {
            self.update_hover(false);
        }
    }

    /// Button `button` (0 left, 1 right, 2 middle) went down or up at `now`.
    pub fn pointer_button(&mut self, button: u8, pressed: bool, now: u64) {
        self.now = now;
        let button = button.min(7);
        let bit = 1u8 << button;
        if pressed {
            self.buttons |= bit;
        } else {
            self.buttons &= !bit;
        }
        if let Some(drag) = self.drag {
            self.grabbed(drag, button, pressed);
        } else if pressed {
            self.press(button, now);
        } else {
            // A release nothing grabbed (its press was over nothing).
            self.update_hover(true);
        }
    }

    /// A button went down or up while `drag` holds the pointer.
    fn grabbed(&mut self, drag: Drag, button: u8, pressed: bool) {
        let (x, y) = self.pointer;
        match drag {
            Drag::Client { id } => {
                if let Some(c) = self.windows.get(&id).map(|w| w.client_rect) {
                    let clicks = self.click_count;
                    self.send(id, WindowEvent::PointerButton { x: x - c.x, y: y - c.y, button, pressed, clicks });
                }
                if self.buttons == 0 {
                    self.drag = None;
                    self.update_hover(true);
                }
            }
            Drag::Move { button: held, .. } | Drag::Resize { button: held, .. } | Drag::Button { button: held, .. } => {
                if pressed || button != held {
                    return;
                }
                self.end_drag();
                match drag {
                    Drag::Move { id, snap: Some((snap, r)), .. } => self.apply_snap(id, snap, r),
                    Drag::Button { id, part, .. } if self.window_at(x, y) == Some((id, part)) => match part {
                        Part::Close => self.request_close(id),
                        Part::Minimize => self.set_state(id, WindowState::Minimized),
                        Part::Maximize => {
                            let maximized = self.windows.get(&id).is_some_and(|w| w.state == WindowState::Maximized);
                            let next = if maximized { WindowState::Normal } else { WindowState::Maximized };
                            self.set_state(id, next);
                        }
                        _ => {}
                    },
                    _ => {}
                }
                self.update_hover(true);
            }
        }
    }

    /// A button went down with nothing grabbing the pointer.
    fn press(&mut self, button: u8, now: u64) {
        let (x, y) = self.pointer;
        let hit = self.window_at(x, y);
        let target = hit.map_or(0, |(id, _)| id);
        let last = self.last_click;
        let again = now.saturating_sub(last.time) < DOUBLE_CLICK_NS
            && last.window == target
            && last.button == button
            && (x - last.at.0).abs() < 5
            && (y - last.at.1).abs() < 5;
        self.click_count = if again { self.click_count.saturating_add(1) } else { 1 };
        self.last_click = Click { time: now, window: target, button, at: (x, y) };

        let Some((id, part)) = hit else {
            // The bare background: popups close, the keyboard goes nowhere.
            self.focus(None);
            return;
        };
        let Some(kind) = self.windows.get(&id).map(|w| w.kind) else { return };
        match kind {
            WindowKind::Normal | WindowKind::Borderless => {
                self.raise(id);
                self.focus(Some(id));
            }
            WindowKind::Popup => self.focus(Some(id)),
            // Pressing the desktop, a panel or a notification closes popups.
            WindowKind::Desktop | WindowKind::Panel | WindowKind::Notification => self.close_popups(),
        }
        let Some(w) = self.windows.get(&id) else { return };
        let (c, state, resizable, snapped) = (w.client_rect, w.state, w.resizable, w.snapped.is_some());
        match part {
            Part::Client => {
                let clicks = self.click_count;
                self.send(id, WindowEvent::PointerButton { x: x - c.x, y: y - c.y, button, pressed: true, clicks });
                self.drag = Some(Drag::Client { id });
            }
            Part::Title if button == 0 => {
                if self.click_count >= 2 && resizable {
                    let next =
                        if state == WindowState::Maximized { WindowState::Normal } else { WindowState::Maximized };
                    self.set_state(id, next);
                    return;
                }
                let restore = state == WindowState::Maximized || snapped;
                self.drag =
                    Some(Drag::Move { id, button, dx: x - c.x, dy: y - c.y, origin: (x, y), restore, snap: None });
                self.cursor = Cursor::Move;
            }
            Part::Close | Part::Minimize | Part::Maximize if button == 0 => {
                self.decor.pressed = Some((id, part));
                self.damage_title(id);
                self.drag = Some(Drag::Button { id, button, part });
            }
            Part::Edge(dx, dy) if button == 0 => {
                self.drag = Some(Drag::Resize { id, button, edge: (dx, dy), start: c, px: x, py: y });
            }
            _ => {}
        }
    }

    /// The wheel turned: the client under the pointer hears of it.
    pub fn scroll(&mut self, dx: i32, dy: i32) {
        let (x, y) = self.pointer;
        if let Some((id, Part::Client)) = self.window_at(x, y) {
            self.send(id, WindowEvent::Scroll { dx, dy });
        }
    }
}
