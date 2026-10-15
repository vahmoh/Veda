// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Desktop wallpapers: loading from image files, a procedural default, and
//! the blurred copy used for translucent ("mica") shell surfaces.
//!
//! Decoding, scaling and blurring a large picture takes seconds, so once the
//! desktop is up a [`Loader`] does it on a thread of its own: the taskbar,
//! the menus and the agent's approvals stay responsive meanwhile, whoever
//! asked for the picture.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use num_traits::Float;
use vabi::RawHandle;
use vgfx::Bitmap;
use vproto::shell::ShellError;
use vrt::object::Event;
use vrt::sync::{Condvar, Mutex};

/// Name of the built-in procedural wallpaper (used in place of a path).
pub const PROCEDURAL: &str = "procedural";

/// The largest picture file taken for a wallpaper (bytes).
const MAX_FILE: u64 = 64 << 20;

/// The wallpaper at screen resolution plus a heavily blurred version.
pub struct Wallpaper {
    /// File it came from, or [`PROCEDURAL`].
    pub path: String,
    pub image: Bitmap,
    pub blurred: Bitmap,
}

/// Cheap, smooth blur: shrink 8x, blur, then scale back up.
fn soft_blur(src: &Bitmap) -> Bitmap {
    let (w, h) = ((src.width / 8).max(1), (src.height / 8).max(1));
    let mut small = src.resized(w, h);
    small.blur(3, 3);
    small.resized(src.width, src.height)
}

impl Wallpaper {
    pub fn from_bitmap(path: &str, image: Bitmap) -> Wallpaper {
        let blurred = soft_blur(&image);
        Wallpaper { path: path.into(), image, blurred }
    }

    /// Decodes an image file and scales it to cover a `w` x `h` screen.
    pub fn load(path: &str, bytes: &[u8], w: i32, h: i32) -> Option<Wallpaper> {
        let img = vimage::decode(bytes).ok()?;
        let src = Bitmap::from_straight(img.width as i32, img.height as i32, img.pixels);
        Some(Wallpaper::from_bitmap(path, src.cover(w, h)))
    }

    /// Reads and decodes the picture at `path` for a `w` x `h` screen (or
    /// makes the procedural one).
    pub fn open(vfs: Option<&vproto::vfs::Client>, path: &str, w: i32, h: i32) -> Result<Wallpaper, ShellError> {
        if path == PROCEDURAL {
            return Ok(Wallpaper::procedural(w, h));
        }
        let vfs = vfs.ok_or(ShellError::Unavailable)?;
        let st = match vfs.stat(path.into()) {
            Ok(Ok(st)) => st,
            Ok(Err(_)) => return Err(ShellError::NotFound),
            Err(_) => return Err(ShellError::Unavailable),
        };
        if st.is_dir {
            return Err(ShellError::NotFound);
        }
        if st.size > MAX_FILE {
            return Err(ShellError::BadImage);
        }
        let bytes = crate::read_file(vfs, path, MAX_FILE).ok_or(ShellError::NotFound)?;
        Wallpaper::load(path, &bytes, w, h).ok_or(ShellError::BadImage)
    }

    /// Deep blue with luminous blooms and a ribbon of light, used when no
    /// wallpaper files are installed. Rendered at half resolution (it is
    /// all soft gradients) and dithered at full resolution.
    pub fn procedural(w: i32, h: i32) -> Wallpaper {
        let (hw, hh) = ((w / 2).max(1), (h / 2).max(1));
        let (fw, fh) = (hw as f32, hh as f32);
        // (centre x, centre y, radius, r, g, b, strength), in screen fractions.
        let blooms: [(f32, f32, f32, f32, f32, f32, f32); 4] = [
            (0.22, 0.30, 0.65, 59.0, 91.0, 255.0, 0.50),
            (0.80, 0.62, 0.60, 164.0, 59.0, 255.0, 0.42),
            (0.58, 0.12, 0.42, 31.0, 209.0, 255.0, 0.28),
            (0.45, 0.95, 0.50, 255.0, 70.0, 160.0, 0.18),
        ];
        // The ribbon's centre line depends only on x.
        let wave: Vec<f32> = (0..hw)
            .map(|x| x as f32 / fw)
            .map(|fx| 0.56 + 0.10 * (fx * 5.2 + 0.8).sin() + 0.04 * (fx * 13.0).sin())
            .collect();
        let mut small = Bitmap::new(hw, hh);
        for y in 0..hh {
            let fy = y as f32 / fh;
            let base = (11.0 + 15.0 * fy, 16.0 + 4.0 * fy, 38.0 + 26.0 * fy);
            for x in 0..hw {
                let fx = x as f32 / fw;
                let (mut r, mut g, mut b) = base;
                for &(cx, cy, rad, cr, cg, cb, s) in &blooms {
                    let dx = (fx - cx) * fw / fh;
                    let dy = fy - cy;
                    let d2 = (dx * dx + dy * dy) / (rad * rad);
                    if d2 < 1.0 {
                        let k = (1.0 - d2) * (1.0 - d2) * s;
                        r += (cr - r) * k;
                        g += (cg - g) * k;
                        b += (cb - b) * k;
                    }
                }
                let d = (fy - wave[x as usize]) / 0.045;
                let rk = 0.55 / (1.0 + d * d);
                r += (90.0 + 120.0 * fx - r) * rk;
                g += (200.0 - 60.0 * fx - g) * rk;
                b += (255.0 - b) * rk;
                // A thin bright core.
                let core = 0.35 / (1.0 + 16.0 * d * d);
                r += (255.0 - r) * core;
                g += (255.0 - g) * core;
                b += (255.0 - b) * core;
                let px = |v: f32| v.clamp(0.0, 255.0) as u32;
                small.pixels[(y * hw + x) as usize] = 0xFF00_0000 | px(r) << 16 | px(g) << 8 | px(b);
            }
        }
        let mut image = small.resized(w, h);
        // Dither to hide banding in the gradients.
        let mut seed = 0x1234_5678u32;
        for p in image.pixels.iter_mut() {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let n = (seed & 3) as i32 - 1;
            let ch = |v: u32| (v as i32 + n).clamp(0, 255) as u32;
            *p = 0xFF00_0000 | ch(*p >> 16 & 0xFF) << 16 | ch(*p >> 8 & 0xFF) << 8 | ch(*p & 0xFF);
        }
        Wallpaper::from_bitmap(PROCEDURAL, image)
    }
}

/// Wallpaper files shipped with the system, sorted by name.
pub fn list(vfs: &vproto::vfs::Client) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(Ok(entries)) = vfs.read_dir("/system/wallpapers".into()) {
        for e in entries.iter().filter(|e| !e.is_dir && vfiles::kind::is_image(&e.name)) {
            out.push(alloc::format!("/system/wallpapers/{}", e.name));
        }
    }
    out.sort();
    out
}

/// A wallpaper to load: what asked for it (`ticket`), the picture and the
/// screen it covers.
struct Request {
    ticket: u64,
    path: String,
    width: i32,
    height: i32,
}

/// A wallpaper loaded (or not) for a [`Loader::load`] request.
pub struct Loaded {
    pub ticket: u64,
    pub result: Result<Wallpaper, ShellError>,
}

struct Shared {
    requests: Mutex<VecDeque<Request>>,
    wake: Condvar,
    done: Mutex<Vec<Loaded>>,
}

/// Loads wallpapers on a thread of its own; [`Loader::event_handle`] is
/// signalled when one is ready ([`Loader::take`]).
pub struct Loader {
    shared: Arc<Shared>,
    event: Event,
}

impl Loader {
    /// Starts the worker.
    pub fn start() -> Option<Loader> {
        let event = Event::create().ok()?;
        let theirs = Event::from_handle(event.0.duplicate(None).ok()?);
        let shared = Arc::new(Shared {
            requests: Mutex::new(VecDeque::new()),
            wake: Condvar::new(),
            done: Mutex::new(Vec::new()),
        });
        let worker = shared.clone();
        vrt::thread::Builder::new()
            .name("wallpaper")
            .stack_size(256 * 1024)
            .spawn(move || work(&worker, &theirs))
            .ok()?;
        Some(Loader { shared, event })
    }

    /// Loads the wallpaper at `path` (or [`PROCEDURAL`]) for a `width` x
    /// `height` screen; the result comes back with `ticket`.
    pub fn load(&self, ticket: u64, path: &str, width: i32, height: i32) {
        self.shared.requests.lock().push_back(Request { ticket, path: path.into(), width, height });
        self.shared.wake.notify_one();
    }

    /// The wallpapers loaded since the last call.
    pub fn take(&self) -> Vec<Loaded> {
        let _ = self.event.clear();
        core::mem::take(&mut *self.shared.done.lock())
    }

    /// Signalled when a wallpaper is ready.
    pub fn event_handle(&self) -> RawHandle {
        self.event.raw()
    }
}

/// The worker: loads what is asked, one at a time.
fn work(shared: &Shared, ready: &Event) {
    let vfs = vproto::connect(vproto::vfs::NAME).ok().map(vproto::vfs::Client::new);
    if let Some(c) = &vfs {
        c.set_timeout(crate::SERVICE_TIMEOUT_NS);
    }
    loop {
        let r = {
            let mut q = shared.requests.lock();
            loop {
                if let Some(r) = q.pop_front() {
                    break r;
                }
                q = shared.wake.wait(q);
            }
        };
        let result = Wallpaper::open(vfs.as_ref(), &r.path, r.width, r.height);
        shared.done.lock().push(Loaded { ticket: r.ticket, result });
        let _ = ready.signal();
    }
}
