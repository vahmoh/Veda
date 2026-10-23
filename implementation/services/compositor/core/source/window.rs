// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! A window as the compositor keeps it: its kind, where it is, its state,
//! its client's buffers and the events on their way to it, and what part
//! of it is under a point.

use alloc::string::String;

use gfx::Rect;
use proto::display::{Cursor, WindowKind, WindowState};

use crate::buffers::Surface;
use crate::outbox::Outbox;
use crate::subsurfaces::Placements;

/// Height of the title bar of decorated windows.
pub const TITLE_HEIGHT: i32 = 36;
/// Corner radius of decorated windows.
pub const CORNER_RADIUS: i32 = 10;
/// Invisible margin around a window that acts as a resize handle.
pub const RESIZE_MARGIN: i32 = 6;
/// Width of each caption button.
pub const BUTTON_WIDTH: i32 = 46;

/// An animation affecting how a window is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimKind {
    Open,
    Close,
    Minimize,
    Restore,
}

/// An animation, from `start` for `duration` (ns).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anim {
    pub kind: AnimKind,
    pub start: u64,
    pub duration: u64,
}

impl Anim {
    /// Progress 0.0 → 1.0 with ease-out.
    pub fn progress(&self, now: u64) -> f32 {
        let t = ((now.saturating_sub(self.start)) as f32 / self.duration.max(1) as f32).clamp(0.0, 1.0);
        1.0 - (1.0 - t) * (1.0 - t) * (1.0 - t)
    }

    /// Whether it is over at `now`.
    pub fn done(&self, now: u64) -> bool {
        now >= self.start.saturating_add(self.duration)
    }
}

/// Which part of a window the pointer is over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    Client,
    Title,
    Minimize,
    Maximize,
    Close,
    /// Resize edge: (dx, dy) each -1, 0 or 1.
    Edge(i8, i8),
}

/// Where a dragged window snaps when released at a screen edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Snap {
    Maximize,
    Left,
    Right,
}

impl Snap {
    /// The part of the work area `area` that a window snapped here fills
    /// (title bar included).
    pub fn rect(self, area: Rect) -> Rect {
        let half = area.w / 2;
        match self {
            Snap::Maximize => area,
            Snap::Left => Rect::new(area.x, area.y, half, area.h),
            Snap::Right => Rect::new(area.x + half, area.y, area.w - half, area.h),
        }
    }
}

/// A window. `M` is the memory its client's buffers are in (a mapping in
/// the compositor, nothing in tests).
pub struct Window<M> {
    pub id: u32,
    /// The connection of the client it belongs to.
    pub client: u64,
    pub kind: WindowKind,
    pub title: String,
    pub app_id: String,
    pub state: WindowState,
    /// Client area in screen coordinates.
    pub client_rect: Rect,
    /// Client rectangle to return to when un-maximising.
    pub restore_rect: Rect,
    /// The least size the user may make it.
    pub min_w: i32,
    pub min_h: i32,
    pub resizable: bool,
    pub cursor: Cursor,
    pub anim: Option<Anim>,
    /// The client asked to close; destroyed after the close animation.
    pub closing: bool,
    /// Snapped to half of the screen (dragging restores `restore_rect`).
    pub snapped: Option<Snap>,
    /// Placed automatically (centred) and not moved by the user since: the
    /// window is placed again when the work area changes.
    pub auto_placed: bool,
    /// A popup's window: its client's, which had the focus when it
    /// opened (none for the shell's popups).
    pub parent: Option<u32>,
    /// A popup that lost the focus: hidden until its client destroys it.
    pub dismissed: bool,
    /// Its client's buffers.
    pub surface: Surface<M>,
    /// Where its frame shown places its subsurfaces, and what of its buffer
    /// is drawn over them.
    pub placements: Placements,
    /// Events waiting for room on its channel.
    pub(crate) outbox: Outbox,
    /// The state it returns to when it is no longer minimised.
    pub(crate) unminimized: WindowState,
    /// The size and state its client was last told (`Configure`).
    pub(crate) told: Option<(i32, i32, WindowState)>,
    /// When the user last asked its client to close it.
    pub(crate) close_asked: Option<u64>,
}

impl<M> Window<M> {
    /// A window of `kind` with its client area at `rect`.
    pub fn new(id: u32, client: u64, kind: WindowKind, rect: Rect) -> Window<M> {
        Window {
            id,
            client,
            kind,
            title: String::new(),
            app_id: String::new(),
            state: WindowState::Normal,
            client_rect: rect,
            restore_rect: rect,
            min_w: 1,
            min_h: 1,
            resizable: false,
            cursor: Cursor::Arrow,
            anim: None,
            closing: false,
            snapped: None,
            auto_placed: false,
            parent: None,
            dismissed: false,
            surface: Surface::default(),
            placements: Placements::default(),
            outbox: Outbox::default(),
            unminimized: WindowState::Normal,
            told: None,
            close_asked: None,
        }
    }

    /// Whether it has a title bar and a frame.
    pub fn decorated(&self) -> bool {
        self.kind == WindowKind::Normal && self.state != WindowState::Fullscreen
    }

    /// Outer rectangle (client area plus title bar).
    pub fn frame(&self) -> Rect {
        if self.decorated() {
            let c = self.client_rect;
            Rect::new(c.x, c.y - TITLE_HEIGHT, c.w, c.h + TITLE_HEIGHT)
        } else {
            self.client_rect
        }
    }

    /// Area that may be painted, including the shadow.
    pub fn paint_bounds(&self) -> Rect {
        if self.decorated() {
            let f = self.frame();
            Rect::new(f.x - 30, f.y - 26, f.w + 60, f.h + 64)
        } else if self.kind == WindowKind::Popup || self.kind == WindowKind::Notification {
            self.frame().inflate(28).translate(0, 4)
        } else {
            self.frame()
        }
    }

    /// The title bar.
    pub fn title_rect(&self) -> Rect {
        let f = self.frame();
        Rect::new(f.x, f.y, f.w, TITLE_HEIGHT)
    }

    /// Where caption button `part` is.
    pub fn button_rect(&self, part: Part) -> Rect {
        let t = self.title_rect();
        let i = match part {
            Part::Close => 1,
            Part::Maximize => 2,
            Part::Minimize => 3,
            _ => return Rect::default(),
        };
        Rect::new(t.right() - BUTTON_WIDTH * i, t.y, BUTTON_WIDTH, TITLE_HEIGHT)
    }

    /// Whether it is drawn: not minimised (or still animating there), and
    /// not a popup the compositor dismissed.
    pub fn visible(&self) -> bool {
        !self.dismissed && (self.state != WindowState::Minimized || self.anim.is_some())
    }

    /// Whether the pointer and the keyboard reach it: drawn, and neither
    /// leaving nor on its way to the taskbar.
    pub fn interactive(&self) -> bool {
        self.visible() && self.state != WindowState::Minimized && !self.closing
    }

    /// Whether the client's pixels are copied as they are (see
    /// [`WindowKind`]: normal and desktop windows are opaque).
    pub fn opaque(&self) -> bool {
        matches!(self.kind, WindowKind::Normal | WindowKind::Desktop)
    }

    /// The screen area this window hides completely: the client area of an
    /// opaque, settled window whose buffer fills it, without the rounded
    /// bottom corners. Empty if it hides nothing.
    pub fn opaque_rect(&self) -> Rect {
        let filled = self.surface.size().is_some_and(|(w, h)| w >= self.client_rect.w && h >= self.client_rect.h);
        if !self.opaque() || !filled || self.anim.is_some() || !self.interactive() {
            return Rect::default();
        }
        let c = self.client_rect;
        let rounded = self.decorated() && self.state != WindowState::Maximized;
        if rounded { Rect::new(c.x, c.y, c.w, c.h - CORNER_RADIUS) } else { c }
    }

    /// Classifies a screen point; `None` if the point misses the window.
    pub fn hit(&self, x: i32, y: i32) -> Option<Part> {
        let f = self.frame();
        if self.decorated() && self.resizable && self.state == WindowState::Normal {
            let grip = f.inflate(RESIZE_MARGIN);
            if grip.contains(x, y) && !f.inset(2, 2, 2, 2).contains(x, y) {
                let dx = if x < f.x + 4 {
                    -1
                } else if x >= f.right() - 4 {
                    1
                } else {
                    0
                };
                let dy = if y < f.y + 4 {
                    -1
                } else if y >= f.bottom() - 4 {
                    1
                } else {
                    0
                };
                if dx != 0 || dy != 0 {
                    return Some(Part::Edge(dx, dy));
                }
            }
        }
        if !f.contains(x, y) {
            return None;
        }
        if self.decorated() && y < self.client_rect.y {
            for p in [Part::Close, Part::Maximize, Part::Minimize] {
                if self.button_rect(p).contains(x, y) && (p != Part::Maximize || self.resizable) {
                    return Some(p);
                }
            }
            return Some(Part::Title);
        }
        Some(Part::Client)
    }

    /// The events waiting for room on its channel.
    pub fn outbox(&self) -> &Outbox {
        &self.outbox
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(kind: WindowKind) -> Window<()> {
        let mut w = Window::new(1, 1, kind, Rect::new(100, 100, 400, 300));
        w.resizable = true;
        w
    }

    #[test]
    fn decorated_windows_have_a_title_bar_and_handles() {
        let w = window(WindowKind::Normal);
        assert_eq!(w.frame(), Rect::new(100, 64, 400, 336));
        assert_eq!(w.hit(200, 80), Some(Part::Title));
        assert_eq!(w.hit(495, 80), Some(Part::Close));
        assert_eq!(w.hit(200, 200), Some(Part::Client));
        assert_eq!(w.hit(98, 200), Some(Part::Edge(-1, 0)));
        assert_eq!(w.hit(510, 200), None);
        let b = window(WindowKind::Borderless);
        assert_eq!(b.hit(200, 80), None);
        assert_eq!(b.hit(200, 100), Some(Part::Client));
    }

    #[test]
    fn hidden_windows_are_not_interactive() {
        let mut w = window(WindowKind::Popup);
        assert!(w.visible() && w.interactive());
        w.dismissed = true;
        assert!(!w.visible() && !w.interactive());
        let mut w = window(WindowKind::Normal);
        w.state = WindowState::Minimized;
        w.anim = Some(Anim { kind: AnimKind::Minimize, start: 0, duration: 10 });
        assert!(w.visible() && !w.interactive());
    }

    #[test]
    fn only_settled_opaque_windows_hide_what_is_below() {
        let mut w = window(WindowKind::Normal);
        assert!(w.opaque_rect().is_empty());
        w.surface.attach(crate::buffers::Buffers {
            layout: crate::buffers::Layout::new(400, 300, 400, 2, (1920, 1080), usize::MAX).unwrap(),
            memory: (),
        });
        w.surface.present(0, &[]).unwrap();
        assert_eq!(w.opaque_rect(), Rect::new(100, 100, 400, 300 - CORNER_RADIUS));
        w.anim = Some(Anim { kind: AnimKind::Open, start: 0, duration: 10 });
        assert!(w.opaque_rect().is_empty());
    }
}
