// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The compositor's state: the window manager (`compositor_core::Wm`), and
//! what goes with it in this process: the clients' channels, the screen,
//! how windows are drawn, and the GPU.
//!
//! After everything that may change the window manager (a request, an input
//! driver's events, a frame), [`Compositor::settle`] carries out what it
//! decided: its events go out on the windows' channels, the channels and
//! textures of windows that are gone go too, connections it gave up on are
//! closed, and the cursor follows the pointer.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use compositor_core::Wm;
use compositor_core::outbox::Write;
use vgfx::{Bitmap, Rect};
use vproto::display::{self as dp, Cursor, WindowEvent};
use vrt::object::Channel;
use vrt::println;
use vrt::vm::Mapping;

use crate::decor::Decor;
use crate::gpu::{Gpu, Pending};
use crate::pixels::{self, Pixels};
use crate::screen::Screen;
use crate::startup::Startup;

/// The window system (see the module's comment).
pub(crate) struct Compositor {
    pub(crate) wm: Wm<Mapping>,
    pub(crate) screen: Screen,
    pub(crate) decor: Decor,
    /// Each window's event channel (the compositor's end).
    pub(crate) events: BTreeMap<u32, Channel>,
    /// Each display client's connection.
    pub(crate) clients: BTreeMap<u64, Channel>,
    /// The cursor as drawn: its shape, where the pointer was, and what it
    /// covered.
    pub(crate) cursor: (Cursor, (i32, i32)),
    pub(crate) cursor_rect: Rect,
    /// Thumbnails of the switcher's windows, for the switcher they were
    /// taken for (its serial).
    pub(crate) thumbs: Option<(u64, Vec<Option<Bitmap>>)>,
    pub(crate) last_frame: u64,
    /// The startup sequence, while it runs (see [`Startup`]).
    pub(crate) startup: Option<Startup>,
    /// The GPU, while it composes (see `gpu`); one set up and waiting for
    /// its check; its setup under way.
    pub(crate) gpu: Option<Gpu>,
    pub(crate) gpu_ready: Option<Gpu>,
    pub(crate) gpu_setup: Option<Pending>,
    /// The driver whose pictures the GPU was set up for (its connection).
    pub(crate) gpu_tried: Option<u64>,
    /// How many times the GPU stopped while it composed: it is set up
    /// again after the first few (its renderer ended and started again).
    pub(crate) gpu_losses: u32,
}

/// Writes `ev` on `channel`, a window's event channel.
fn write(channel: Option<&Channel>, ev: &WindowEvent) -> Write {
    let Some(channel) = channel else { return Write::Gone };
    match vipc::send_event(channel, dp::EVENT, ev.clone()) {
        Ok(()) => Write::Sent,
        Err(vipc::IpcError::Kernel(vabi::Error::ShouldWait)) => Write::Full,
        Err(_) => Write::Gone,
    }
}

impl Compositor {
    /// The screen.
    pub(crate) fn screen_rect(&self) -> Rect {
        self.screen.rect()
    }

    /// Carries out what the window manager decided (see the module's
    /// comment).
    pub(crate) fn settle(&mut self) {
        let events = &self.events;
        self.wm.deliver(|id, ev| write(events.get(&id), ev));
        for id in self.wm.take_destroyed() {
            self.events.remove(&id);
            if let Some(g) = &mut self.gpu {
                g.forget_window(id);
            }
        }
        for key in self.wm.take_dropped() {
            // Its windows are gone: their channels closed above.
            println!("a client did not answer and was asked again: its connection is closed");
            self.clients.remove(&key);
        }
        self.update_cursor();
        self.update_thumbnails();
    }

    /// Window `id`'s channel has room again.
    pub(crate) fn writable(&mut self, id: u32) {
        let events = &self.events;
        self.wm.writable(id, |id, ev| write(events.get(&id), ev));
    }

    /// The client on connection `key` went away, or is sent away (it does
    /// not read its answers): its windows go.
    pub(crate) fn drop_client(&mut self, key: u64) {
        self.clients.remove(&key);
        self.wm.disconnect(key);
        self.settle();
    }

    /// The cursor follows the pointer and the shape the window manager
    /// chose: what it covered and covers now is drawn again.
    pub(crate) fn update_cursor(&mut self) {
        let now = (self.wm.cursor(), self.wm.pointer());
        if now == self.cursor && !self.cursor_rect.is_empty() {
            return;
        }
        self.cursor = now;
        self.wm.add_damage(self.cursor_rect);
        if let Some((b, hx, hy)) = self.decor.cursor(now.0) {
            self.cursor_rect = Rect::new(now.1.0 - hx, now.1.1 - hy, b.width, b.height);
        }
        self.wm.add_damage(self.cursor_rect);
    }

    /// The switcher's thumbnails, taken as it opens (and dropped as it
    /// closes).
    fn update_thumbnails(&mut self) {
        let Some(s) = self.wm.switcher() else {
            self.thumbs = None;
            return;
        };
        if self.thumbs.as_ref().is_some_and(|(serial, _)| *serial == s.serial) {
            return;
        }
        let thumbs = s
            .windows
            .iter()
            .map(|id| self.wm.window(*id).and_then(|w| Pixels::shown(&w.surface)).and_then(|p| pixels::thumbnail(&p)))
            .collect();
        self.thumbs = Some((s.serial, thumbs));
    }
}
