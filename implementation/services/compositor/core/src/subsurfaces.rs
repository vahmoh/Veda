// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Subsurfaces: pictures of their own that a window shows in parts of it
//! (see `proto::display`), presented apart from its frames. Here: a
//! subsurface's buffers, each in a memory object of its own and checked as
//! it is attached, which of them is shown, and what the GPU's copy of it
//! lacks; and where a window's frame places its subsurfaces.
//!
//! A subsurface shows one buffer at a time. Presenting another gives the
//! one shown before back to the client at once: the compositor reads a
//! buffer only while it is the one shown (the screen, or the GPU's copy,
//! keeps what was drawn from it), so the client may draw into it again
//! while the new one waits to be composed. Where the GPU reads buffers in
//! place rather than copies of them, a frame it has yet to finish may still
//! be reading the one shown before: that one goes back once the frame's
//! fence has passed ([`Wm::gpu_passed`]), with a
//! [`SubsurfaceEvent::Released`]. A buffer the client detaches while it is
//! shown is shown still, until another is presented, so that a client whose
//! buffers go (a page made larger) leaves no hole meanwhile.

use alloc::vec::Vec;

use gfx::{Damage, Rect};
use proto::display::{self as dp, DisplayError, SubsurfaceEvent};

use crate::buffers::{Buffers, Layout};
use crate::limits;
use crate::outbox::Write;
use crate::wm::Wm;

/// The most buffers a subsurface has attached (WebKit's pages use four at
/// most), those detached but shown still among them.
pub const BUFFERS_MAX: usize = 8;

/// A buffer of a subsurface, as its client names it.
struct Attached<M> {
    name: u32,
    /// Which buffer it is, of all ever attached (names come again).
    serial: u64,
    buffers: Buffers<M>,
    /// Detached by its client: kept only while it is shown.
    detached: bool,
    /// The fence of the last frame the GPU composed by reading it in place
    /// (0: none): the GPU may read it until that fence has passed.
    gpu: u64,
}

/// What presenting a buffer did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Presented {
    /// The buffer shown before, which the client may have back: none if
    /// it was this one, if none was, if it was detached, or if the GPU may
    /// still be reading it (`releasing`).
    pub released: Option<u32>,
    /// The buffer shown before, which the GPU may still be reading, and the
    /// fence after which it is not: the client has it back then.
    pub releasing: Option<(u32, u64)>,
    /// The buffer shown is not as large as the one before (or none was):
    /// what shows of the background changed too.
    pub resized: bool,
}

/// A subsurface of window `window`, presented by the client on connection
/// `client`. `M` is the memory its buffers are in.
pub struct Subsurface<M> {
    pub id: u32,
    pub window: u32,
    pub client: u64,
    attached: Vec<Attached<M>>,
    /// The name of the buffer shown.
    shown: Option<u32>,
    /// A frame was presented and is to be acknowledged (`FrameDone`) once it
    /// has been composed and can be seen.
    pub(crate) frame_owed: bool,
    /// What changed in the shown buffer since the GPU's copy of it was
    /// brought up to date; `upload_all`: everything did.
    pub upload: Damage,
    pub upload_all: bool,
    /// Buffers presented over while the GPU might still read them, each
    /// with the fence after which it does not; and those it no longer does,
    /// which the client is to be told it has back.
    releasing: Vec<(u32, u64)>,
    pub(crate) released: Vec<u32>,
    /// An acknowledgement of a frame waits for room on its channel.
    pub(crate) frame_unsent: bool,
    /// The serials of the buffers gone since the compositor last took them
    /// (what it made of them, such as the GPU's view, goes too).
    gone: Vec<u64>,
}

impl<M> Subsurface<M> {
    /// A subsurface of `window` for the client on `client`, without buffers.
    pub fn new(id: u32, window: u32, client: u64) -> Subsurface<M> {
        Subsurface {
            id,
            window,
            client,
            attached: Vec::new(),
            shown: None,
            frame_owed: false,
            upload: Damage::new(),
            upload_all: true,
            releasing: Vec::new(),
            released: Vec::new(),
            frame_unsent: false,
            gone: Vec::new(),
        }
    }

    fn find(&self, name: u32) -> Option<&Attached<M>> {
        self.attached.iter().find(|a| a.name == name)
    }

    /// Attaches `buffers` (one buffer, checked) as `name`, the `serial`th
    /// buffer attached: `Invalid` for 0 or a name in use (a buffer detached
    /// but shown still among them), `NoMemory` past [`BUFFERS_MAX`].
    pub fn attach(&mut self, name: u32, serial: u64, buffers: Buffers<M>) -> Result<(), DisplayError> {
        if name == 0 || buffers.layout.count != 1 || self.find(name).is_some() {
            return Err(DisplayError::Invalid);
        }
        if self.attached.len() >= BUFFERS_MAX {
            return Err(DisplayError::NoMemory);
        }
        self.attached.push(Attached { name, serial, buffers, detached: false, gpu: 0 });
        Ok(())
    }

    /// Removes attached buffer `i`, noting that it went.
    fn remove(&mut self, i: usize) {
        let a = self.attached.swap_remove(i);
        self.gone.push(a.serial);
    }

    /// The client has done with buffer `name`: it goes, unless it is shown,
    /// which it is until another is presented. (A buffer the GPU may still
    /// read goes too: what the GPU reads stays its own until it is done.)
    pub fn detach(&mut self, name: u32) -> Result<(), DisplayError> {
        let Some(i) = self.attached.iter().position(|a| a.name == name && !a.detached) else {
            return Err(DisplayError::BadBuffer);
        };
        if self.shown == Some(name) {
            self.attached[i].detached = true;
        } else {
            self.remove(i);
            self.forget_release(name);
        }
        Ok(())
    }

    /// Buffer `name` is no longer to be given back (shown again, or gone).
    fn forget_release(&mut self, name: u32) {
        self.releasing.retain(|&(n, _)| n != name);
        self.released.retain(|&n| n != name);
    }

    /// The client presents buffer `name`, `damage` (in its pixels) having
    /// changed since the buffer presented before; everything if it is empty.
    /// The GPU has finished its frames up to fence `signaled`: a buffer it
    /// may still read goes back later ([`Subsurface::gpu_passed`]).
    pub fn present(&mut self, name: u32, damage: &[Rect], signaled: u64) -> Result<Presented, DisplayError> {
        let Some(new) = self.find(name).filter(|a| !a.detached) else { return Err(DisplayError::BadBuffer) };
        let size = (new.buffers.layout.width, new.buffers.layout.height);
        let before = self.size();
        self.forget_release(name);
        let (mut released, mut releasing) = (None, None);
        match self.shown.replace(name) {
            Some(old) if old != name => match self.attached.iter().position(|a| a.name == old) {
                Some(i) if self.attached[i].detached => self.remove(i),
                Some(i) if self.attached[i].gpu > signaled => {
                    let fence = self.attached[i].gpu;
                    self.releasing.push((old, fence));
                    releasing = Some((old, fence));
                }
                Some(_) => released = Some(old),
                None => {}
            },
            _ => {}
        }
        let resized = before != Some(size);
        // What the GPU's copy lacks now: the new buffer holds the whole
        // picture, of which only `damage` differs from the one before.
        if resized || damage.is_empty() {
            self.upload.clear();
            self.upload_all = true;
        } else {
            let all = Rect::new(0, 0, size.0, size.1);
            for d in damage {
                self.upload.add(d.intersect(&all));
            }
        }
        self.frame_owed = true;
        Ok(Presented { released, releasing, resized })
    }

    /// The GPU composed a frame by reading the shown buffer in place, which
    /// it may do until fence `fence` has passed.
    pub fn sampled(&mut self, fence: u64) {
        if let Some(name) = self.shown
            && let Some(a) = self.attached.iter_mut().find(|a| a.name == name)
        {
            a.gpu = a.gpu.max(fence);
        }
    }

    /// The GPU has finished its frames up to fence `signaled`: the buffers
    /// it read that were presented over are the client's again (`released`).
    /// Whether any were.
    pub fn gpu_passed(&mut self, signaled: u64) -> bool {
        let before = self.released.len();
        let released = &mut self.released;
        self.releasing.retain(|&(name, fence)| {
            if fence <= signaled {
                released.push(name);
            }
            fence > signaled
        });
        self.released.len() > before
    }

    /// The buffer shown, if any is.
    pub fn shown(&self) -> Option<&Buffers<M>> {
        self.shown.and_then(|name| self.find(name)).map(|a| &a.buffers)
    }

    /// The buffer shown, and its serial (which buffer it is, of all ever
    /// attached).
    pub fn shown_serial(&self) -> Option<(&Buffers<M>, u64)> {
        self.shown.and_then(|name| self.find(name)).map(|a| (&a.buffers, a.serial))
    }

    /// The size of the buffer shown.
    pub fn size(&self) -> Option<(i32, i32)> {
        self.shown().map(|b| (b.layout.width, b.layout.height))
    }

    /// The memory its buffers take.
    pub fn bytes(&self) -> usize {
        self.attached.iter().map(|a| a.buffers.layout.bytes()).sum()
    }
}

/// Checks a buffer for a subsurface: one buffer of `width` x `height`
/// pixels at most `max`, rows `stride` pixels apart, at the start of
/// `memory` bytes.
pub fn layout(width: u32, height: u32, stride: u32, max: (i32, i32), memory: usize) -> Result<Layout, DisplayError> {
    Layout::new(width, height, stride, 1, max, memory)
}

/// Where a window's frame shows one of its subsurfaces (checked).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placed {
    pub subsurface: u32,
    /// In the window's client area.
    pub rect: Rect,
    /// `0xFF_RRGGBB`: what shows where the subsurface's buffer does not
    /// reach.
    pub background: u32,
}

/// Where a window's frame places its subsurfaces, and where its buffer is
/// drawn over them (its overlays, in its client area).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Placements {
    pub placed: Vec<Placed>,
    pub overlays: Vec<Rect>,
}

impl Placements {
    /// Where subsurface `id` is placed.
    pub fn of(&self, id: u32) -> Option<&Placed> {
        self.placed.iter().find(|p| p.subsurface == id)
    }

    /// What `self` and `other` show differently, in the client area: the
    /// rectangles of the placements that differ, and the overlays of both
    /// if they differ (the window's buffer is shown there, or no longer).
    pub fn differences(&self, other: &Placements) -> Vec<Rect> {
        let mut out = Vec::new();
        for p in &self.placed {
            if !other.placed.contains(p) {
                out.push(p.rect);
            }
        }
        for p in &other.placed {
            if !self.placed.contains(p) {
                out.push(p.rect);
            }
        }
        if self.overlays != other.overlays {
            out.extend(self.overlays.iter().chain(&other.overlays).copied());
        }
        out
    }
}

impl<M> Wm<M> {
    /// Subsurface `id`.
    pub fn subsurface(&self, id: u32) -> Option<&Subsurface<M>> {
        self.subsurfaces.get(&id)
    }

    /// Subsurface `id` as window `window`'s frame places it: none if it is
    /// no more, or another window's.
    pub fn placed_subsurface(&self, window: u32, id: u32) -> Option<&Subsurface<M>> {
        self.subsurfaces.get(&id).filter(|s| s.window == window)
    }

    /// Subsurface `id`, to change how it is drawn (the GPU's copy of it).
    pub fn subsurface_mut(&mut self, id: u32) -> Option<&mut Subsurface<M>> {
        self.subsurfaces.get_mut(&id)
    }

    /// The subsurfaces, to change how they are drawn.
    pub fn subsurfaces_mut(&mut self) -> impl Iterator<Item = &mut Subsurface<M>> {
        self.subsurfaces.values_mut()
    }

    /// Subsurface `id`, if `client` presents it.
    fn own_subsurface(&self, client: u64, id: u32) -> Result<&Subsurface<M>, DisplayError> {
        self.subsurfaces.get(&id).filter(|s| s.client == client).ok_or(DisplayError::NoSuchSubsurface)
    }

    /// The subsurfaces presented by connections of `koid`'s process.
    fn process_subsurfaces(&self, koid: u64) -> impl Iterator<Item = &Subsurface<M>> {
        self.subsurfaces.values().filter(move |s| self.clients.get(&s.client).is_some_and(|c| c.identity.koid == koid))
    }

    /// `client` makes a subsurface for `window`, a window of its process
    /// (see `display::create_subsurface`); its id.
    pub fn create_subsurface(&mut self, client: u64, window: u32) -> Result<u32, DisplayError> {
        let caller = self.clients.get(&client).ok_or(DisplayError::Denied)?;
        let w = self.windows.get(&window).ok_or(DisplayError::NoSuchWindow)?;
        let owner = self.clients.get(&w.client).ok_or(DisplayError::NoSuchWindow)?;
        if w.client != client && !owner.identity.same_process(&caller.identity) {
            return Err(DisplayError::NoSuchWindow);
        }
        if self.process_subsurfaces(caller.identity.koid).count() >= limits::SUBSURFACES_PER_PROCESS {
            return Err(DisplayError::NoMemory);
        }
        let id = loop {
            self.next_subsurface = self.next_subsurface.wrapping_add(1);
            let id = self.next_subsurface;
            if id != 0 && !self.subsurfaces.contains_key(&id) && !self.destroyed_subsurfaces.contains(&id) {
                break id;
            }
        };
        self.subsurfaces.insert(id, Subsurface::new(id, window, client));
        Ok(id)
    }

    /// Checks a buffer `client` wants to attach to subsurface `id`: one
    /// buffer of `size`, rows `stride` pixels apart, in `memory` bytes (see
    /// [`layout`]), within its process's limit.
    pub fn subsurface_layout(
        &self,
        client: u64,
        id: u32,
        size: (u32, u32),
        stride: u32,
        memory: usize,
    ) -> Result<Layout, DisplayError> {
        self.own_subsurface(client, id)?;
        let layout = layout(size.0, size.1, stride, self.limits.window(), memory)?;
        let koid = self.clients.get(&client).map_or(0, |c| c.identity.koid);
        let used: usize = self.process_subsurfaces(koid).map(|s| s.bytes()).sum();
        if used.saturating_add(layout.bytes()) > self.limits.subsurface_bytes_per_process() {
            return Err(DisplayError::NoMemory);
        }
        Ok(layout)
    }

    /// `client` attaches `buffers` (checked by [`Wm::subsurface_layout`]) to
    /// subsurface `id` as `name`.
    pub fn attach_subsurface(
        &mut self,
        client: u64,
        id: u32,
        name: u32,
        buffers: Buffers<M>,
    ) -> Result<(), DisplayError> {
        self.own_subsurface(client, id)?;
        let serial = self.next_buffer_serial + 1;
        self.subsurfaces.get_mut(&id).ok_or(DisplayError::NoSuchSubsurface)?.attach(name, serial, buffers)?;
        self.next_buffer_serial = serial;
        Ok(())
    }

    /// `client` has done with buffer `name` of subsurface `id`.
    pub fn detach_subsurface(&mut self, client: u64, id: u32, name: u32) -> Result<(), DisplayError> {
        self.own_subsurface(client, id)?;
        self.subsurfaces.get_mut(&id).ok_or(DisplayError::NoSuchSubsurface)?.detach(name)
    }

    /// `client` presents buffer `name` of subsurface `id`, `damage` (the
    /// buffer's pixels) having changed since the one presented before, or
    /// everything if it is empty: the buffer shown before, which the
    /// client may have back (none if the GPU may still read it: a
    /// [`SubsurfaceEvent::Released`] gives it back once it does not).
    pub fn present_subsurface(
        &mut self,
        client: u64,
        id: u32,
        name: u32,
        damage: Vec<dp::Rect>,
    ) -> Result<Option<u32>, DisplayError> {
        self.own_subsurface(client, id)?;
        if damage.len() > dp::DAMAGE_MAX {
            return Err(DisplayError::Invalid);
        }
        let rects = damage.into_iter().map(limits::rect).collect::<Result<Vec<Rect>, DisplayError>>()?;
        let signaled = self.gpu_signaled;
        let p = self.subsurfaces.get_mut(&id).ok_or(DisplayError::NoSuchSubsurface)?.present(name, &rects, signaled)?;
        self.subsurfaces_owed.insert(id);
        let changed = if p.resized || rects.is_empty() { None } else { Some(rects.as_slice()) };
        self.damage_subsurface(id, changed);
        Ok(p.released)
    }

    /// `client` destroys subsurface `id`.
    pub fn destroy_subsurface(&mut self, client: u64, id: u32) -> Result<(), DisplayError> {
        self.own_subsurface(client, id)?;
        self.remove_subsurface(id);
        Ok(())
    }

    /// Removes subsurface `id`: where its window's frame places it, the
    /// placement's background shows.
    pub(crate) fn remove_subsurface(&mut self, id: u32) {
        self.damage_subsurface(id, None);
        let Some(mut s) = self.subsurfaces.remove(&id) else { return };
        self.gone_buffers.append(&mut s.gone);
        self.gone_buffers.extend(s.attached.iter().map(|a| a.serial));
        self.subsurfaces_owed.remove(&id);
        self.subsurfaces_due.remove(&id);
        self.subsurfaces_released.remove(&id);
        self.subsurfaces_blocked.remove(&id);
        self.destroyed_subsurfaces.push(id);
    }

    /// The GPU composed a frame reading subsurfaces `ids`' shown buffers in
    /// place, which it may do until fence `fence` has passed.
    pub fn subsurfaces_sampled(&mut self, ids: &[u32], fence: u64) {
        for id in ids {
            if let Some(s) = self.subsurfaces.get_mut(id) {
                s.sampled(fence);
            }
        }
    }

    /// The GPU has finished its frames up to fence `signaled` (`u64::MAX`:
    /// it reads nothing more, as when it went): the buffers it read that
    /// were presented over go back to their clients
    /// ([`SubsurfaceEvent::Released`], with [`Wm::deliver_subsurfaces`]).
    pub fn gpu_passed(&mut self, signaled: u64) {
        self.gpu_signaled = self.gpu_signaled.max(signaled);
        for (&id, s) in &mut self.subsurfaces {
            if s.gpu_passed(signaled) && !self.subsurfaces_blocked.contains(&id) {
                self.subsurfaces_released.insert(id);
            }
        }
    }

    /// The GPU composing frames from now on is a new one (or none): its
    /// fences count from the start again.
    pub fn gpu_changed(&mut self) {
        self.gpu_passed(u64::MAX);
        self.gpu_signaled = 0;
        for s in self.subsurfaces.values_mut() {
            for a in &mut s.attached {
                a.gpu = 0;
            }
        }
    }

    /// Subsurfaces gone since the last call: their channels and textures go.
    pub fn take_destroyed_subsurfaces(&mut self) -> Vec<u32> {
        core::mem::take(&mut self.destroyed_subsurfaces)
    }

    /// The serials of subsurfaces' buffers gone since the last call (those
    /// of subsurfaces gone among them): what was made of them goes too.
    pub fn take_gone_buffers(&mut self) -> Vec<u64> {
        let mut gone = core::mem::take(&mut self.gone_buffers);
        for s in self.subsurfaces.values_mut() {
            gone.append(&mut s.gone);
        }
        gone
    }

    /// Where subsurface `s` shows on the screen: its window's frame places
    /// it, and the window can be seen; the rectangle its buffer starts at,
    /// and its place, cut to the window's client area.
    pub fn subsurface_place(&self, s: &Subsurface<M>) -> Option<(Rect, Rect)> {
        let w = self.windows.get(&s.window).filter(|w| self.shown(w))?;
        let p = w.placements.of(s.id)?;
        let c = w.client_rect;
        let at = p.rect.translate(c.x, c.y);
        Some((at, at.intersect(&c)))
    }

    /// Marks what subsurface `id` shows to be composed again: `rects` of its
    /// buffer, or (`None`) all of its place.
    fn damage_subsurface(&mut self, id: u32, rects: Option<&[Rect]>) {
        let Some(s) = self.subsurfaces.get(&id) else { return };
        let Some((at, place)) = self.subsurface_place(s) else { return };
        // A window that animates is drawn again whole.
        if let Some(w) = self.windows.get(&s.window).filter(|w| w.anim.is_some()) {
            let r = w.paint_bounds();
            self.damage.add(r);
            return;
        }
        match rects {
            None => self.damage.add(place),
            Some(rects) => {
                for r in rects {
                    self.damage.add(r.translate(at.x, at.y).intersect(&place));
                }
            }
        }
    }

    /// After a frame: the subsurfaces that can be seen hear that their
    /// frames were shown (the others once they can be seen again).
    pub(crate) fn subsurface_frames_done(&mut self) {
        let seen: Vec<u32> = self
            .subsurfaces_owed
            .iter()
            .copied()
            .filter(|id| self.subsurfaces.get(id).is_some_and(|s| self.subsurface_place(s).is_some()))
            .collect();
        for id in seen {
            self.subsurfaces_owed.remove(&id);
            let Some(s) = self.subsurfaces.get_mut(&id) else { continue };
            if core::mem::take(&mut s.frame_owed) {
                if self.subsurfaces_blocked.contains(&id) {
                    s.frame_unsent = true;
                } else {
                    self.subsurfaces_due.insert(id);
                }
            }
        }
    }

    /// Writes what subsurfaces' clients are to hear since the last call,
    /// `write` putting each on its subsurface's channel: the buffers the GPU
    /// no longer reads, then the acknowledgements of frames. What does not
    /// fit on a full channel is written once it has room
    /// ([`Wm::subsurface_writable`]); one frame's acknowledgement stands for
    /// those that came meanwhile.
    pub fn deliver_subsurfaces(&mut self, mut write: impl FnMut(u32, &SubsurfaceEvent) -> Write) {
        let frames = core::mem::take(&mut self.subsurfaces_due);
        let released = core::mem::take(&mut self.subsurfaces_released);
        for id in frames.union(&released).copied() {
            if let Some(s) = self.subsurfaces.get_mut(&id) {
                s.frame_unsent |= frames.contains(&id);
            }
            self.write_subsurface(id, &mut write);
        }
    }

    /// Subsurface `id`'s channel has room again: what waits is written.
    pub fn subsurface_writable(&mut self, id: u32, mut write: impl FnMut(u32, &SubsurfaceEvent) -> Write) {
        if self.subsurfaces_blocked.remove(&id) {
            self.write_subsurface(id, &mut write);
        }
    }

    /// Writes what subsurface `id`'s client waits to hear: the buffers it
    /// has back first (it draws its next frame into one), then the frame's
    /// acknowledgement. A full channel keeps the rest for when it has room.
    fn write_subsurface(&mut self, id: u32, write: &mut impl FnMut(u32, &SubsurfaceEvent) -> Write) {
        let (seen_ns, period_ns) = self.frame;
        let Some(s) = self.subsurfaces.get_mut(&id) else { return };
        while let Some(&buffer) = s.released.first() {
            if write(id, &SubsurfaceEvent::Released { subsurface: id, buffer }) == Write::Full {
                self.subsurfaces_blocked.insert(id);
                return;
            }
            s.released.remove(0);
        }
        if s.frame_unsent {
            let ev = SubsurfaceEvent::FrameDone { subsurface: id, seen_ns, period_ns };
            if write(id, &ev) == Write::Full {
                self.subsurfaces_blocked.insert(id);
                return;
            }
            s.frame_unsent = false;
        }
    }

    /// The subsurfaces whose channels are full, an acknowledgement waiting.
    pub fn blocked_subsurfaces(&self) -> impl Iterator<Item = u32> + '_ {
        self.subsurfaces_blocked.iter().copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAX: (i32, i32) = (1920, 1080);

    fn buffer(w: u32, h: u32) -> Buffers<()> {
        Buffers { layout: layout(w, h, w, MAX, usize::MAX).unwrap(), memory: () }
    }

    #[test]
    fn buffers_are_named_once() {
        let mut s = Subsurface::new(1, 2, 3);
        assert_eq!(s.attach(0, 0, buffer(10, 10)), Err(DisplayError::Invalid));
        s.attach(7, 7, buffer(10, 10)).unwrap();
        assert_eq!(s.attach(7, 7, buffer(10, 10)), Err(DisplayError::Invalid));
        // One buffer to a memory object, no more.
        let two = Buffers { layout: Layout::new(10, 10, 10, 2, MAX, usize::MAX).unwrap(), memory: () };
        assert_eq!(s.attach(8, 8, two), Err(DisplayError::Invalid));
        for name in 8..8 + BUFFERS_MAX as u32 - 1 {
            s.attach(name, u64::from(name), buffer(10, 10)).unwrap();
        }
        assert_eq!(s.attach(99, 99, buffer(10, 10)), Err(DisplayError::NoMemory));
        assert_eq!(s.bytes(), 10 * 10 * 4 * BUFFERS_MAX);
    }

    #[test]
    fn presenting_gives_the_buffer_shown_before_back() {
        let mut s = Subsurface::new(1, 2, 3);
        s.attach(1, 1, buffer(100, 50)).unwrap();
        s.attach(2, 2, buffer(100, 50)).unwrap();
        assert_eq!(s.present(3, &[], 0), Err(DisplayError::BadBuffer));
        assert_eq!(s.present(1, &[], 0), Ok(Presented { released: None, releasing: None, resized: true }));
        assert_eq!(
            s.present(2, &[Rect::new(0, 0, 5, 5)], 0),
            Ok(Presented { released: Some(1), releasing: None, resized: false })
        );
        // The same buffer again (the client drew into it while it was shown):
        // nothing to give back.
        assert_eq!(s.present(2, &[], 0).unwrap().released, None);
        assert_eq!(s.present(1, &[], 0).unwrap().released, Some(2));
        assert_eq!(s.size(), Some((100, 50)));
    }

    #[test]
    fn a_buffer_the_gpu_may_still_read_goes_back_once_it_does_not() {
        let mut s = Subsurface::new(1, 2, 3);
        for name in 1..=3 {
            s.attach(name, u64::from(name), buffer(100, 50)).unwrap();
        }
        s.present(1, &[], 0).unwrap();
        // A frame read buffer 1 in place, its fence 7 not yet passed: 1 goes
        // back once it has, not as 2 is presented.
        s.sampled(7);
        assert_eq!(s.present(2, &[], 6), Ok(Presented { released: None, releasing: Some((1, 7)), resized: false }));
        assert!(!s.gpu_passed(6) && s.released.is_empty());
        assert!(s.gpu_passed(7));
        assert_eq!(s.released, [1]);
        // One whose last frame is done goes back at once.
        s.sampled(8);
        assert_eq!(s.present(3, &[], 8).unwrap().released, Some(2));
        // Shown again, or detached, before it went back: nothing to give.
        s.sampled(9);
        assert_eq!(s.present(1, &[], 8).unwrap().releasing, Some((3, 9)));
        s.detach(3).unwrap();
        assert!(!s.gpu_passed(u64::MAX));
        s.released.clear();
        s.sampled(10);
        s.present(2, &[], 0).unwrap();
        s.present(1, &[], 0).unwrap();
        assert!(s.gpu_passed(u64::MAX) && s.released == [2]);
    }

    #[test]
    fn a_buffer_detached_while_shown_stays_until_another_is_presented() {
        let mut s = Subsurface::new(1, 2, 3);
        s.attach(1, 1, buffer(100, 50)).unwrap();
        s.present(1, &[], 0).unwrap();
        s.detach(1).unwrap();
        assert_eq!(s.size(), Some((100, 50)));
        // It cannot be presented again, nor detached twice.
        assert_eq!(s.present(1, &[], 0), Err(DisplayError::BadBuffer));
        assert_eq!(s.detach(1), Err(DisplayError::BadBuffer));
        // Larger buffers come: the old one goes as the first is presented,
        // and nobody is given it back.
        s.attach(2, 2, buffer(200, 80)).unwrap();
        assert_eq!(s.bytes(), 100 * 50 * 4 + 200 * 80 * 4);
        assert_eq!(s.present(2, &[], 0), Ok(Presented { released: None, releasing: None, resized: true }));
        assert_eq!(s.bytes(), 200 * 80 * 4);
        // A buffer not shown goes at once.
        s.attach(3, 3, buffer(200, 80)).unwrap();
        s.detach(3).unwrap();
        assert_eq!(s.bytes(), 200 * 80 * 4);
    }

    #[test]
    fn a_name_detached_but_shown_is_in_use_still() {
        let mut s = Subsurface::new(1, 2, 3);
        s.attach(1, 1, buffer(100, 50)).unwrap();
        s.present(1, &[], 0).unwrap();
        s.detach(1).unwrap();
        assert_eq!(s.attach(1, 1, buffer(10, 10)), Err(DisplayError::Invalid));
        // Free again once another is shown.
        s.attach(2, 2, buffer(10, 10)).unwrap();
        s.present(2, &[], 0).unwrap();
        assert_eq!(s.attach(1, 1, buffer(10, 10)), Ok(()));
    }

    #[test]
    fn what_changed_is_kept_for_the_gpu() {
        let mut s = Subsurface::new(1, 2, 3);
        s.attach(1, 1, buffer(100, 50)).unwrap();
        s.attach(2, 2, buffer(100, 50)).unwrap();
        s.present(1, &[], 0).unwrap();
        assert!(s.upload_all);
        s.upload_all = false;
        s.present(2, &[Rect::new(90, 40, 50, 50)], 0).unwrap();
        assert!(!s.upload_all);
        assert_eq!(s.upload.rects(), &[Rect::new(90, 40, 10, 10)]);
        // Everything, when the client says so.
        s.present(1, &[], 0).unwrap();
        assert!(s.upload_all && s.upload.is_empty());
    }

    #[test]
    fn placements_differ_where_they_show_differently() {
        let page = Placed { subsurface: 1, rect: Rect::new(0, 100, 800, 500), background: 0xFFFF_FFFF };
        let bar = Rect::new(400, 110, 380, 42);
        let a = Placements { placed: alloc::vec![page], overlays: alloc::vec![bar] };
        assert!(a.differences(&a.clone()).is_empty());
        // Another subsurface in the same place: there.
        let other = Placements { placed: alloc::vec![Placed { subsurface: 2, ..page }], ..a.clone() };
        assert_eq!(a.differences(&other), [page.rect, page.rect]);
        // The bar gone: where it was.
        let without = Placements { overlays: Vec::new(), ..a.clone() };
        assert_eq!(a.differences(&without), [bar]);
        assert_eq!(a.of(1), Some(&page));
        assert_eq!(a.of(2), None);
    }
}
