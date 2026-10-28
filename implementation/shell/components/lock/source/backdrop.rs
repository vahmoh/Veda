// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The picture behind the lock screen: the wallpaper the user chose (the
//! desktop's), at the screen's size, in two forms made once as it loads:
//!
//! * **resting**: the wallpaper itself, under a light veil, shaded more at
//!   the top, where the clock is, and at the bottom, where the hint is, so
//!   that white text reads on any picture;
//! * **asking**: its blurred copy, dimmed (more towards the bottom, behind
//!   the card).
//!
//! As the card rises, the asking form is drawn over the resting one, more
//! opaque as it comes. Until the wallpaper is loaded, and without one,
//! both are the splash's gradient (`splash`), what the screen showed as
//! the system started; a wallpaper chosen anew comes in over the one shown
//! ([`Backdrop::draw_over`]).
//!
//! Decoding, scaling and blurring a picture takes seconds under emulation,
//! so a [`Painter`] does it on a thread of its own: as the lock service
//! starts, and whenever the screen is locked with another wallpaper chosen
//! since.

use std::sync::Arc;

use abi::RawHandle;
use gfx::color::lerp_px;
use gfx::{Bitmap, Canvas};
use runtime::object::Event;
use runtime::sync::{Condvar, Mutex};
use shell_common::wallpaper::{self, Wallpaper};

/// What the backdrop is shaded with: the splash's deep blue.
const SHADE: u32 = 0xFF05_0711;
/// How long the painter waits for the file system.
const VFS_TIMEOUT_NS: u64 = 10_000_000_000;

/// The lock screen's backdrop (see the module's comment).
pub struct Backdrop {
    /// The wallpaper it was made from (`None`: the splash's gradient).
    pub source: Option<String>,
    rest: Bitmap,
    ask: Bitmap,
}

/// `t` eased in and out (0 to 1).
fn smooth(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// How much of [`SHADE`] row `y` of `h` takes when resting (parts of 256):
/// a veil, deeper over the clock and the hint.
fn rest_shade(y: i32, h: i32) -> u32 {
    let f = y as f32 / h.max(1) as f32;
    let top = smooth(1.0 - f / 0.48) * 0.44;
    let bottom = smooth((f - 0.6) / 0.4) * 0.5;
    ((0.08 + top.max(bottom)) * 256.0) as u32
}

/// ... and when asking: dimmed, more towards the bottom.
fn ask_shade(y: i32, h: i32) -> u32 {
    let f = y as f32 / h.max(1) as f32;
    ((0.36 + 0.2 * f) * 256.0) as u32
}

/// `b` with each row mixed with [`SHADE`] as `shade` says.
fn shaded(mut b: Bitmap, shade: fn(i32, i32) -> u32) -> Bitmap {
    let (w, h) = (b.width.max(0) as usize, b.height);
    if w == 0 {
        return b;
    }
    for (y, row) in b.pixels.chunks_exact_mut(w).enumerate() {
        let t = shade(y as i32, h).min(256);
        for px in row {
            *px = lerp_px(*px | 0xFF00_0000, SHADE, t);
            gfx::color::keep_scalar();
        }
    }
    b
}

impl Backdrop {
    /// The splash's gradient on a `w` x `h` screen, while there is no
    /// wallpaper.
    pub fn gradient(w: i32, h: i32) -> Backdrop {
        let mut rest = Bitmap::new(w.max(1), h.max(1));
        let width = rest.width as usize;
        for (y, row) in rest.pixels.chunks_exact_mut(width).enumerate() {
            splash::background(y as u32, h.max(1) as u32, 0, row);
            for px in row {
                *px |= 0xFF00_0000;
            }
        }
        let ask = shaded(rest.clone(), |_, _| 48);
        Backdrop { source: None, rest, ask }
    }

    /// From the wallpaper `wp`, at the screen's size.
    pub fn from_wallpaper(wp: Wallpaper) -> Backdrop {
        let Wallpaper { path, image, blurred } = wp;
        Backdrop { source: Some(path), rest: shaded(image, rest_shade), ask: shaded(blurred, ask_shade) }
    }

    /// The size it was made for.
    pub fn size(&self) -> (i32, i32) {
        (self.rest.width, self.rest.height)
    }

    /// Draws it over all of `c` (a canvas of its size), `blur` of the way
    /// (0 to 1) from the resting form to the asking one.
    pub fn draw(&self, c: &mut Canvas, blur: f32) {
        let alpha = (blur.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        if alpha < 255 {
            c.blit(&self.rest, self.rest.rect(), 0, 0);
        }
        if alpha == 255 {
            c.blit(&self.ask, self.ask.rect(), 0, 0);
        } else if alpha > 0 {
            c.draw_bitmap(&self.ask, 0, 0, alpha);
        }
    }

    /// Draws it over what `c` shows, `opacity` (0 to 1) of the way in: its
    /// form nearer `blur`.
    pub fn draw_over(&self, c: &mut Canvas, blur: f32, opacity: f32) {
        let form = if blur < 0.5 { &self.rest } else { &self.ask };
        c.draw_bitmap(form, 0, 0, (opacity.clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
    }
}

/// A backdrop asked for: the screen's size, and the wallpaper the one shown
/// was made from.
struct Request {
    width: i32,
    height: i32,
    shown: Option<String>,
}

struct Shared {
    asked: Mutex<Option<Request>>,
    wake: Condvar,
    done: Mutex<Option<Backdrop>>,
}

/// Paints backdrops on a thread of its own: [`Painter::event_handle`] is
/// signalled when one is ready ([`Painter::take`]).
pub struct Painter {
    shared: Arc<Shared>,
    event: Event,
}

impl Painter {
    /// Starts the thread.
    pub fn start() -> Option<Painter> {
        let event = Event::create().ok()?;
        let theirs = Event::from_handle(event.0.duplicate(None).ok()?);
        let shared = Arc::new(Shared { asked: Mutex::new(None), wake: Condvar::new(), done: Mutex::new(None) });
        let worker = shared.clone();
        runtime::thread::Builder::new()
            .name("backdrop")
            .stack_size(256 * 1024)
            .spawn(move || work(&worker, &theirs))
            .ok()?;
        Some(Painter { shared, event })
    }

    /// Paints the backdrop for a `width` x `height` screen, unless the
    /// wallpaper chosen is still `shown`'s (the one shown was made from).
    /// A request not yet begun is replaced.
    pub fn refresh(&self, width: i32, height: i32, shown: Option<&Backdrop>) {
        let shown = shown.filter(|b| b.size() == (width, height)).and_then(|b| b.source.clone());
        *self.shared.asked.lock() = Some(Request { width, height, shown });
        self.shared.wake.notify_one();
    }

    /// The backdrop painted, if one is ready.
    pub fn take(&self) -> Option<Backdrop> {
        let _ = self.event.clear();
        self.shared.done.lock().take()
    }

    /// Signalled when a backdrop is ready.
    pub fn event_handle(&self) -> RawHandle {
        self.event.raw()
    }
}

/// The thread: paints what is asked, the latest request first.
fn work(shared: &Shared, ready: &Event) {
    let vfs = proto::connect(proto::vfs::NAME).ok().map(proto::vfs::Client::new);
    if let Some(v) = &vfs {
        v.set_timeout(VFS_TIMEOUT_NS);
    }
    loop {
        let r = {
            let mut asked = shared.asked.lock();
            loop {
                if let Some(r) = asked.take() {
                    break r;
                }
                asked = shared.wake.wait(asked);
            }
        };
        // The wallpaper the desktop shows: nothing to do if it is still the
        // one the backdrop shown was made from.
        let chosen = vfs.as_ref().and_then(wallpaper::remembered);
        if chosen.is_some() && chosen == r.shown {
            continue;
        }
        let wp = wallpaper::chosen(vfs.as_ref(), r.width, r.height);
        if r.shown.as_deref() == Some(wp.path.as_str()) {
            continue;
        }
        *shared.done.lock() = Some(Backdrop::from_wallpaper(wp));
        let _ = ready.signal();
    }
}
