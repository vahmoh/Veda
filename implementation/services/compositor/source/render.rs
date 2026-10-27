// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Composition: damaged regions are redrawn bottom to top, by the GPU
//! straight into the picture the display shows next (`gpu`) where it can,
//! or by the processor into the back buffer, which is then copied to the
//! screen (`screen`).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

use compositor::Wm;
use compositor::window::AnimKind;
use gfx::{Canvas, Damage, Rect};
use proto::display::WindowEvent;
use rt::println;
use rt::vm::Mapping;

use crate::decor::{self, Layer, LayerTexture, Look, Window};
use crate::gpu::{Gpu, Slot};
use crate::pixels::Pixels;
use crate::startup::{Moved, Progress};
use crate::state::Compositor;

/// How long the GPU may take to draw a frame before it counts as stopped.
const GPU_TIMEOUT_NS: u64 = 2_000_000_000;
/// How many times a GPU that stopped is set up again (once its service is
/// back): a renderer that ended is started again, but one that keeps
/// stopping is left alone.
const GPU_RETRIES: u32 = 3;
/// How long handing a frame to the GPU may take before it is logged, and
/// how many frames took that long.
const SLOW_FRAME_NS: u64 = 50_000_000;
static SLOW_FRAMES: AtomicU32 = AtomicU32::new(0);

/// How a window is drawn at `now`: its opacity, and how far below its
/// place (while it animates).
fn animation(w: &Window, now: u64) -> (f32, i32) {
    match w.anim {
        Some(a) => {
            let p = a.progress(now);
            match a.kind {
                AnimKind::Open | AnimKind::Restore => (p, ((1.0 - p) * 14.0) as i32),
                AnimKind::Close => (1.0 - p, (p * 10.0) as i32),
                AnimKind::Minimize => (1.0 - p, (p * 40.0) as i32),
            }
        }
        None => (1.0, 0),
    }
}

/// How window `w` of `wm` is drawn at `now`.
fn look(wm: &Wm<Mapping>, w: &Window, now: u64) -> Look {
    let (opacity, dy) = animation(w, now);
    Look { focused: wm.focused() == Some(w.id), decor: wm.decor(), unresponsive: wm.unresponsive(w.id), opacity, dy }
}

/// Window `w`'s subsurfaces, where its frame places them, for the
/// processor.
pub(crate) fn layers<'a>(wm: &'a Wm<Mapping>, w: &Window) -> Vec<Layer<'a>> {
    w.placements
        .placed
        .iter()
        .map(|p| Layer {
            rect: p.rect,
            background: p.background,
            pixels: wm.placed_subsurface(w.id, p.subsurface).and_then(|s| s.shown()).and_then(|b| Pixels::of(b, 0)),
        })
        .collect()
}

impl Compositor {
    // ---- rendering ---------------------------------------------------------

    /// Whether something moves on the screen: the startup sequence, or a
    /// window's animation.
    pub(crate) fn animating(&self) -> bool {
        self.startup.as_ref().is_some_and(|s| !s.holding()) || self.wm.animating()
    }

    /// Whether a frame is to be composed (when the screen is ready for one).
    pub(crate) fn wants_frame(&self) -> bool {
        !self.wm.undamaged()
            || self.animating()
            || self.screen.needs_frame()
            || self.gpu_ready.is_some()
            || self.wm.configures_due()
    }

    /// Whether a frame is to be composed at `now`.
    pub(crate) fn frame_due(&self, now: u64) -> bool {
        self.wants_frame() && now >= self.screen.next_frame(self.last_frame)
    }

    /// Composes a frame: what changed, as it is when the frame is seen.
    pub(crate) fn composite(&mut self) {
        let real = rt::time::now_ns();
        // Animations as they are when the frame is seen.
        let now = self.screen.frame_time(real);
        self.wm.advance(now);
        // Clients hear of their sizes once a frame, however often a drag
        // changed them.
        self.wm.flush_configures();
        self.wm.refresh_hover();
        self.settle();
        self.advance_startup(now);
        self.take_gpu();
        if self.gpu.is_some() && !self.screen.flipping() {
            self.drop_gpu("the display no longer flips");
        }
        if self.gpu.is_some() {
            self.composite_gpu(now, real);
        } else {
            self.composite_cpu(now, real);
        }
        // Clients may now draw their next frame: theirs is seen when this
        // one is (`now`), the next a period later at the earliest.
        let period_ns = self.screen.period_ns();
        self.wm.frames_done(now, period_ns);
        self.settle();
        self.last_frame = real;
    }

    /// Moves the startup sequence on to `now`: the shell is told as the
    /// desktop appears, and once the sequence is over the whole screen is
    /// composed anew, the desktop alone.
    fn advance_startup(&mut self, now: u64) {
        let Some(mut s) = self.startup.take() else { return };
        let progress = Progress { settled: self.screen.settled(), drawn: self.wm.screen_drawn() };
        match s.advance(now, progress, &|| self.describe()) {
            Moved::Appearing => self.wm.send_to_desktop(WindowEvent::Appearing {}),
            Moved::Over => {
                let screen = self.screen_rect();
                self.wm.add_damage(screen);
                return;
            }
            Moved::No | Moved::Started => {}
        }
        self.startup = Some(s);
    }

    /// Where frames go, and (where a driver flips them) who draws them, for
    /// the log.
    fn describe(&self) -> String {
        match &self.gpu {
            Some(g) => format!("{}; drawn by the GPU ({})", self.screen.describe(), g.renderer),
            None if self.screen.flipping() => format!("{}; drawn by the processor", self.screen.describe()),
            None => self.screen.describe(),
        }
    }

    // ---- by the processor ----------------------------------------------------

    fn composite_cpu(&mut self, now: u64, real: u64) {
        let screen = self.screen_rect();
        let rects = self.wm.take_damage();
        self.screen.begin_frame(self.startup.as_ref().map(|s| s.layer()));
        // During the startup sequence the screen shows it instead.
        let shown = self.startup.is_none();
        let Compositor { wm, screen: out, decor, thumbs, cursor, cursor_rect, .. } = self;
        let order = wm.paint_order();
        let preview = wm.snap_preview();
        for r in rects {
            let r = r.intersect(&screen);
            if r.is_empty() {
                continue;
            }
            let (w, h) = (out.back.width, out.back.height);
            let mut c = Canvas::new(&mut out.back.pixels, w, h, w);
            c.clip_to(r);
            // Drawing starts at the topmost window shown that hides all of
            // `r`: nothing below it (the desktop under a game, say) is
            // visible. While the screen is locked, only lock screens are
            // shown, over the backdrop.
            let covering = wm.covering(r);
            if covering.is_none() {
                decor::draw_background(&mut c, screen);
            }
            for &id in &order[covering.unwrap_or(0)..] {
                // The snap preview goes just below the window being dragged.
                if let Some((dragged, preview)) = preview
                    && id == dragged
                    && preview.inflate(8).intersects(&r)
                {
                    decor::draw_snap_preview(&mut c, preview);
                }
                let Some(win) = wm.window(id) else { continue };
                if !wm.shown(win) || !win.paint_bounds().intersects(&r) {
                    continue;
                }
                decor.draw_window(&mut c, win, look(wm, win, now), &layers(wm, win));
            }
            if let Some(s) = wm.switcher()
                && s.bounds().intersects(&r)
            {
                let titles: Vec<&str> =
                    s.windows.iter().map(|id| wm.window(*id).map_or("", |w| w.title.as_str())).collect();
                let thumbs = thumbs.as_ref().map_or(&[][..], |(_, t)| t.as_slice());
                decor.draw_switcher(&mut c, s, &titles, thumbs);
            }
            // Cursor on top.
            if cursor_rect.intersects(&r)
                && let Some((b, hx, hy)) = decor.cursor(cursor.0)
            {
                c.draw_bitmap(b, cursor.1.0 - hx, cursor.1.1 - hy, 255);
            }
            drop(c);
            if shown {
                out.flush(r);
            }
        }
        // The GPU's copies of the windows and their subsurfaces, if it
        // composes again, are made anew.
        for w in self.wm.windows_mut() {
            w.surface.upload.clear();
            w.surface.upload_all = true;
        }
        for s in self.wm.subsurfaces_mut() {
            s.upload.clear();
            s.upload_all = true;
        }
        if let Some(s) = &mut self.startup {
            s.frame(&mut self.screen, now);
        }
        self.screen.end_frame(real);
    }

    // ---- by the GPU ------------------------------------------------------------

    /// Composes a frame with the GPU into the picture the display shows
    /// next: what changed, what that picture lacks (it was not on the
    /// screen while frames went into the others), and during the startup
    /// sequence what of it moves, or (as the splash fades in and as the
    /// desktop comes in) everything.
    /// The picture is asked for once the GPU has drawn it.
    fn composite_gpu(&mut self, now: u64, real: u64) {
        let screen = self.screen_rect();
        let Some(mut g) = self.gpu.take() else { return };
        let Some((target, shown, stale)) = self.screen.gpu_frame() else {
            self.gpu = Some(g);
            return;
        };
        let mut region = Damage::new();
        for r in self.wm.take_damage() {
            region.add(r.intersect(&screen));
        }
        // What the picture lacks of the screen is copied from the picture
        // that shows it, or (while the firmware's is shown) drawn.
        if shown.is_none() {
            for &r in &stale {
                region.add(r.intersect(&screen));
            }
        }
        let startup = self.startup.as_ref().map(|s| (s.revealing(), s.whole_screen(), s.region()));
        match startup {
            Some((_, true, _)) => region.add(screen),
            Some((_, false, moving)) => region.add(moving),
            None => {}
        }
        let clip = region.rects().iter().fold(Rect::default(), |a, r| if a.is_empty() { *r } else { a.union(r) });
        let desktop = startup.is_none_or(|(revealing, _, _)| revealing);
        let started = rt::time::now_ns();
        // What the frame draws again (all of `clip`, where the desktop is
        // drawn) is not copied first: a full-screen game changes all of the
        // screen every frame, which would be copied whole, then drawn over.
        let redrawn = if desktop { clip } else { Rect::default() };
        let stale: Vec<Rect> = stale.into_iter().filter(|r| !redrawn.contains_rect(r)).collect();
        if let Some(from) = shown
            && !stale.is_empty()
        {
            g.copy(from, target, &stale);
        }
        g.begin(target, clip);
        // The subsurfaces whose buffers the frame reads in place.
        let mut sampled = Vec::new();
        if !clip.is_empty() && desktop {
            sampled = self.draw_scene(&mut g, clip, now);
        }
        if let Some(s) = &mut self.startup {
            s.frame_gpu(&mut g, clip, now);
        }
        self.screen.gpu_wrote(clip);
        let fence = g.end();
        // The GPU's driver takes a frame in a millisecond or two: say when
        // it takes much longer (the first few times, then now and then).
        let took = rt::time::now_ns() - started;
        if took >= SLOW_FRAME_NS {
            let n = SLOW_FRAMES.fetch_add(1, Ordering::Relaxed);
            if n < 3 || n.is_multiple_of(100) {
                println!(
                    "{} took {} ms to take a frame ({}x{} drawn; {} such frames)",
                    g.renderer,
                    took / 1_000_000,
                    clip.w,
                    clip.h,
                    n + 1
                );
            }
        }
        self.gpu = Some(g);
        match fence {
            Some(fence) => {
                // Their buffers are read until the frame is done.
                self.wm.subsurfaces_sampled(&sampled, fence);
                self.screen.end_gpu_frame(real, fence)
            }
            None => self.lose_gpu("the GPU stopped answering"),
        }
    }

    /// The desktop within `clip`, as [`Compositor::composite_cpu`] draws
    /// it: the windows bottom to top, each window's texture brought up to
    /// date first, each subsurface's buffer sampled in place where the GPU
    /// can. The subsurfaces whose buffers it read so.
    fn draw_scene(&mut self, g: &mut Gpu, clip: Rect, now: u64) -> Vec<u32> {
        let mut sampled = Vec::new();
        let screen = self.screen_rect();
        let Compositor { wm, decor, thumbs, cursor, cursor_rect, .. } = self;
        let order: Vec<u32> = wm.paint_order().to_vec();
        let covering = wm.covering(clip);
        if covering.is_none() {
            decor::draw_background_gpu(g, screen);
        }
        let preview = wm.snap_preview();
        for &id in &order[covering.unwrap_or(0)..] {
            if let Some((dragged, preview)) = preview
                && id == dragged
                && preview.inflate(8).intersects(&clip)
            {
                decor::draw_snap_preview_gpu(g, preview);
            }
            if !wm.window(id).is_some_and(|w| wm.shown(w) && w.paint_bounds().intersects(&clip)) {
                continue;
            }
            let Some(win) = wm.window_mut(id) else { continue };
            // Its pixels, as the GPU has them.
            let changed = win.surface.upload.take();
            let all = core::mem::replace(&mut win.surface.upload_all, false);
            let content = Pixels::shown(&win.surface).and_then(|p| g.picture(Slot::Window(id), &p, all, &changed));
            if content.is_none() {
                win.surface.upload_all = true;
            }
            let placed = win.placements.placed.clone();
            // Its subsurfaces' pixels: their memory, sampled in place, or
            // (where the GPU cannot) as a texture of theirs has them.
            let mut layers = Vec::with_capacity(placed.len());
            for p in placed {
                let content = wm.subsurface_mut(p.subsurface).filter(|s| s.window == id).and_then(|s| {
                    let changed = s.upload.take();
                    let all = core::mem::replace(&mut s.upload_all, false);
                    if let Some(t) = s.shown_serial().and_then(|(b, serial)| g.sampled(serial, b)) {
                        sampled.push(p.subsurface);
                        // What a copy of it would lack is all of it.
                        s.upload_all = true;
                        return Some(t);
                    }
                    let content = s
                        .shown()
                        .and_then(|b| Pixels::of(b, 0))
                        .and_then(|px| g.picture(Slot::Subsurface(p.subsurface), &px, all, &changed));
                    if content.is_none() {
                        s.upload_all = true;
                    }
                    content
                });
                layers.push(LayerTexture { rect: p.rect, background: p.background, content });
            }
            let Some(win) = wm.window(id) else { continue };
            decor.draw_window_gpu(g, win, look(wm, win, now), content, &layers);
        }
        if let Some(s) = wm.switcher()
            && s.bounds().intersects(&clip)
        {
            // Each switcher is drawn anew; one switcher again as its
            // selection moves.
            let key = s.serial << 16 | s.selected as u64 | 1 << 63;
            let t = match g.cached(Slot::Switcher, key) {
                Some(t) => Some(t),
                None => {
                    let titles: Vec<&str> =
                        s.windows.iter().map(|id| wm.window(*id).map_or("", |w| w.title.as_str())).collect();
                    let thumbs = thumbs.as_ref().map_or(&[][..], |(_, t)| t.as_slice());
                    decor.switcher_bitmap(s, &titles, thumbs).map(|b| g.upload(Slot::Switcher, key, &b))
                }
            };
            if let Some(t) = t {
                let at = s.bounds();
                g.image(t, Rect::new(0, 0, at.w, at.h), at, 1.0, false, None);
            }
        } else if wm.switcher().is_none() && g.has(Slot::Switcher) {
            g.forget(Slot::Switcher);
        }
        // Cursor on top.
        if cursor_rect.intersects(&clip)
            && let Some((b, hx, hy)) = decor.cursor(cursor.0)
        {
            let slot = Slot::Cursor(cursor.0 as u32);
            let t = match g.cached(slot, 1) {
                Some(t) => t,
                None => g.upload(slot, 1, b),
            };
            let at = Rect::new(cursor.1.0 - hx, cursor.1.1 - hy, t.w, t.h);
            g.image(t, Rect::new(0, 0, t.w, t.h), at, 1.0, false, None);
        }
        sampled
    }

    // ---- the GPU's arrival and departure -------------------------------------

    /// Starts using a GPU that was set up, once a picture waits for nothing
    /// (none asked for, none being drawn): first checking that what it
    /// draws reaches the display's memory.
    fn take_gpu(&mut self) {
        if self.gpu_ready.is_none() {
            return;
        }
        if !self.screen.flipping() {
            self.gpu_ready = None;
            return;
        }
        let Some(picture) = self.screen.idle_picture() else { return };
        let Some(mut g) = self.gpu_ready.take() else { return };
        let checked = g.check(&mut self.screen, picture);
        // The check drew into the picture's first pixels: they are drawn again
        // before it is shown.
        self.screen.spoil(picture, Rect::new(0, 0, 4, 1));
        match checked {
            Ok(()) => {
                println!("frames are drawn by the GPU ({}) from now on", g.renderer);
                // Its fences count from the start.
                self.wm.gpu_changed();
                self.gpu = Some(g);
            }
            Err(why) => println!("frames stay with the processor: {}", why),
        }
    }

    /// Stops using the GPU: frames are drawn by the processor from the next
    /// on, all of the screen first (the back buffer was not kept).
    pub(crate) fn drop_gpu(&mut self, why: &str) {
        if self.gpu.take().is_none() {
            return;
        }
        // It reads nothing more: what it might have been reading goes back
        // to the clients.
        self.wm.gpu_changed();
        println!("{}: frames are drawn by the processor from now on", why);
        self.screen.not_drawn();
        let screen = self.screen_rect();
        self.wm.add_damage(screen);
    }

    /// The GPU stopped while it composed: frames are drawn by the processor,
    /// and it is set up again for the display's pictures once its service is
    /// back (Linux starts a renderer that ended again), a few times.
    fn lose_gpu(&mut self, why: &str) {
        if self.gpu.is_none() {
            return;
        }
        self.drop_gpu(why);
        self.gpu_losses += 1;
        if self.gpu_losses <= GPU_RETRIES {
            println!("the GPU is set up again once its service is back ({} of {})", self.gpu_losses, GPU_RETRIES);
            self.gpu_tried = None;
        }
    }

    /// The GPU's fences moved: the frame it drew, if done, is asked for, and
    /// the subsurfaces' buffers it no longer reads go back to their clients.
    pub(crate) fn gpu_signaled(&mut self, now: u64) {
        if let Some(g) = &self.gpu {
            self.wm.gpu_passed(g.fence_signaled());
        }
        let Some((fence, since)) = self.screen.drawing() else { return };
        let done = self.gpu.as_ref().is_some_and(|g| g.signaled(fence));
        if done {
            self.screen.drawn(now);
        } else if now.saturating_sub(since) >= GPU_TIMEOUT_NS || self.gpu.as_ref().is_none_or(|g| g.lost()) {
            self.lose_gpu("the GPU did not draw a frame in time");
        }
    }

    /// When to look at the GPU's fences again without its event (a frame it
    /// draws times out).
    pub(crate) fn gpu_deadline(&self) -> Option<u64> {
        self.screen.drawing().map(|(_, since)| since + GPU_TIMEOUT_NS)
    }
}
