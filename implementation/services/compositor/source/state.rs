// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The compositor's state: the window manager (`compositor::Wm`), and
//! what goes with it in this process: the clients' channels, the screen,
//! how windows are drawn, and the GPU.
//!
//! After everything that may change the window manager (a request, an input
//! driver's events, a frame), [`Compositor::settle`] carries out what it
//! decided: its events go out on the windows' channels, the channels and
//! textures of windows that are gone go too, connections it gave up on are
//! closed, the services watching the lock hear whether it changed, and the
//! cursor follows the pointer.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use abi::signals;
use compositor::Wm;
use compositor::outbox::Write;
use gfx::{Bitmap, Rect};
use proto::display::{self as dp, Cursor, DisplayError, LockNotice, SubsurfaceEvent, WindowEvent};
use rt::object::Channel;
use rt::println;
use rt::vm::Mapping;

use crate::decor::{self, Decor};
use crate::gpu::{Gpu, Pending, Slot};
use crate::pixels::{self, Pixels};
use crate::screen::Screen;
use crate::startup::Startup;

/// How many channels may watch the lock at once (the system's services
/// that do: the lock service, the agent).
const LOCK_WATCHERS_MAX: usize = 8;

/// The window system (see the module's comment).
pub(crate) struct Compositor {
    pub(crate) wm: Wm<Mapping>,
    pub(crate) screen: Screen,
    pub(crate) decor: Decor,
    /// Each window's event channel (the compositor's end).
    pub(crate) events: BTreeMap<u32, Channel>,
    /// Each subsurface's event channel (the compositor's end).
    pub(crate) subsurface_events: BTreeMap<u32, Channel>,
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
    /// The channels on which system services watch the lock, and whether
    /// the screen was locked when they were last told.
    pub(crate) lock_watchers: Vec<Channel>,
    pub(crate) told_locked: bool,
}

/// Tells a lock watcher whether the screen is locked: false if it cannot
/// be told (it has gone, or does not read what it is told), and is dropped,
/// so that it watches again and hears how it is then.
fn tell_lock(channel: &Channel, locked: bool) -> bool {
    ipc::send_event(channel, dp::LOCK_NOTICE, LockNotice::Changed { locked }).is_ok()
}

/// Whether the service watching on `channel` has gone (closed it).
fn gone(channel: &Channel) -> bool {
    channel.wait(signals::PEER_CLOSED, 0).is_ok_and(|s| s & signals::PEER_CLOSED != 0)
}

/// Writes `ev` on `channel`, a window's event channel.
fn write(channel: Option<&Channel>, ev: &WindowEvent) -> Write {
    let Some(channel) = channel else { return Write::Gone };
    written(ipc::send_event(channel, dp::EVENT, ev.clone()))
}

/// Writes `ev` on `channel`, a subsurface's event channel.
fn write_subsurface(channel: Option<&Channel>, ev: &SubsurfaceEvent) -> Write {
    let Some(channel) = channel else { return Write::Gone };
    written(ipc::send_event(channel, dp::SUBSURFACE_EVENT, ev.clone()))
}

/// What became of an event sent.
fn written(sent: Result<(), ipc::IpcError>) -> Write {
    match sent {
        Ok(()) => Write::Sent,
        Err(ipc::IpcError::Kernel(abi::Error::ShouldWait)) => Write::Full,
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
        let events = &self.subsurface_events;
        self.wm.deliver_subsurfaces(|id, ev| write_subsurface(events.get(&id), ev));
        for id in self.wm.take_destroyed() {
            self.events.remove(&id);
            if let Some(g) = &mut self.gpu {
                g.forget_window(id);
            }
        }
        for id in self.wm.take_destroyed_subsurfaces() {
            self.subsurface_events.remove(&id);
            if let Some(g) = &mut self.gpu {
                g.forget(Slot::Subsurface(id));
            }
        }
        let gone = self.wm.take_gone_buffers();
        if let Some(g) = &mut self.gpu {
            g.forget_buffers(&gone);
        }
        for key in self.wm.take_dropped() {
            // Its windows are gone: their channels closed above.
            println!("a client did not answer and was asked again: its connection is closed");
            self.clients.remove(&key);
        }
        if self.wm.locked() != self.told_locked {
            let locked = self.wm.locked();
            self.told_locked = locked;
            println!("the screen is {}", if locked { "locked" } else { "unlocked" });
            self.lock_watchers.retain(|c| tell_lock(c, locked));
        }
        self.update_cursor();
        self.update_thumbnails();
    }

    /// A system service watches the lock on `events`: it hears how it is
    /// now, then every change ([`Compositor::settle`]).
    pub(crate) fn watch_lock(&mut self, events: Channel) -> Result<(), DisplayError> {
        if self.lock_watchers.len() >= LOCK_WATCHERS_MAX {
            // Those whose services went away make room.
            self.lock_watchers.retain(|c| !gone(c));
        }
        if self.lock_watchers.len() >= LOCK_WATCHERS_MAX {
            return Err(DisplayError::NoMemory);
        }
        if tell_lock(&events, self.told_locked) {
            self.lock_watchers.push(events);
        }
        Ok(())
    }

    /// Window `id`'s channel has room again.
    pub(crate) fn writable(&mut self, id: u32) {
        let events = &self.events;
        self.wm.writable(id, |id, ev| write(events.get(&id), ev));
    }

    /// Subsurface `id`'s channel has room again.
    pub(crate) fn subsurface_writable(&mut self, id: u32) {
        let events = &self.subsurface_events;
        self.wm.subsurface_writable(id, |id, ev| write_subsurface(events.get(&id), ev));
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
        let wm = &self.wm;
        let thumbs = s
            .windows
            .iter()
            .map(|id| {
                let w = wm.window(*id)?;
                let p = Pixels::shown(&w.surface)?;
                let layers = crate::render::layers(wm, w);
                let overlays = &w.placements.overlays;
                pixels::thumbnail(p.width(), p.height(), |x, y| decor::composite_pixel(&p, &layers, overlays, x, y))
            })
            .collect();
        self.thumbs = Some((s.serial, thumbs));
    }
}
