// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Reading and decoding pictures on background threads.
//!
//! Two workers, each with its own VFS connection: one decodes full pictures for the viewer (the
//! picture on screen is always decoded before any prefetching), the other makes thumbnails for
//! the library and the filmstrip. Results come back through a mutex and an [`Event`] that the
//! UI waits on (see `App::wait_handles`), so decoding never blocks drawing.
//!
//! Pictures take much memory (a 50-megapixel photo some 270 MB with its smaller copies), so what
//! they may take follows the machine's ([`Budget`]): the decoders refuse pictures larger than a
//! share of it, and the viewer keeps neighbours decoded only while they fit. A file is mapped,
//! never copied, and its size checked before it is read; a picture that is decoded for the
//! viewer gives its thumbnails too, rather than being decoded a second time for them.

use core::sync::atomic::{AtomicU64, Ordering};
use std::collections::VecDeque;
use std::sync::Arc;

use abi::RawHandle;
use gfx::Bitmap;
use image::{DecodeOptions, Filter, Format, Image, ImageError, Orientation};
use proto::fs::FsError;
use proto::vfs;
use runtime::object::Event;
use runtime::sync::{Condvar, Mutex};
use runtime::vm::Mapping;

/// Files larger than this are not opened.
const MAX_FILE: u64 = 128 << 20;
/// The decoders take pictures of at least this many pixels, whatever the memory.
const MIN_PIXELS: u64 = 4 << 20;

/// What decoded pictures may take of the machine's memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// The decoders refuse pictures of more pixels: one decoded picture with its smaller copies
    /// takes at most a fifth of the memory.
    pub max_pixels: u64,
    /// The pictures kept decoded (the one on screen and its neighbours) take at most this many
    /// bytes together: a third of the memory.
    pub kept_bytes: u64,
}

impl Budget {
    /// The budget of a machine with `total` bytes of memory.
    pub fn for_memory(total: u64) -> Budget {
        let max_pixels = (total / 5).saturating_mul(3) / 16;
        Budget { max_pixels: max_pixels.clamp(MIN_PIXELS, image::MAX_PIXELS), kept_bytes: total / 3 }
    }

    /// Bytes a decoded picture of `pixels` pixels takes with its smaller copies (4 bytes a pixel,
    /// and a third more for the halves).
    pub fn picture_bytes(pixels: u64) -> u64 {
        pixels.saturating_mul(16) / 3
    }

    /// Neighbours of the picture on screen (of `pixels` pixels) to keep decoded on each side:
    /// both while three such pictures fit, the next one while two do, none otherwise.
    pub fn neighbours(&self, pixels: u64) -> usize {
        let fit = self.kept_bytes / Budget::picture_bytes(pixels).max(1);
        fit.saturating_sub(1).min(2) as usize
    }
}
/// Size of a library card thumbnail.
pub const CARD_W: u32 = 216;
pub const CARD_H: u32 = 144;
/// Size of a filmstrip thumbnail.
pub const STRIP_W: u32 = 76;
pub const STRIP_H: u32 = 52;
/// The low-resolution preview (shown while the full picture loads) fits in this box.
const PREVIEW_W: u32 = 400;
const PREVIEW_H: u32 = 300;
/// Thumbnails are cropped from a copy of the picture that fits in this square.
const BASE_SIZE: u32 = 640;
/// The mip chain stops at this size.
const SMALLEST_LEVEL: i32 = 96;

/// Facts about a picture file, from its headers.
#[derive(Debug, Clone, Copy)]
pub struct Meta {
    pub format: Format,
    /// Size as displayed (after the EXIF orientation).
    pub width: u32,
    pub height: u32,
    pub has_alpha: bool,
    pub orientation: Orientation,
    /// Progressive JPEG or interlaced PNG.
    pub progressive: bool,
    pub file_size: u64,
}

/// A decoded picture, ready for drawing.
pub struct Picture {
    /// Its own number: no other picture decoded has it (what the view cache knows it by).
    pub id: u64,
    /// Mip chain of premultiplied bitmaps: the full picture first, then halves.
    pub levels: Vec<Bitmap>,
    /// Some pixels are not opaque (drawn over a checkerboard).
    pub has_alpha: bool,
    /// The file failed its checksums and was decoded without verifying them.
    pub damaged: bool,
}

impl Picture {
    pub fn width(&self) -> u32 {
        self.levels[0].width as u32
    }

    pub fn height(&self) -> u32 {
        self.levels[0].height as u32
    }
}

/// Thumbnails of one picture (premultiplied bitmaps).
pub struct Thumbs {
    /// Its own number (as [`Picture::id`]).
    pub id: u64,
    /// Library card: `CARD_W` x `CARD_H`, cropped to fill, with rounded corners.
    pub card: Bitmap,
    /// Filmstrip: `STRIP_W` x `STRIP_H`, cropped to fill, with rounded corners.
    pub strip: Bitmap,
    /// The whole picture at low resolution.
    pub preview: Bitmap,
    pub has_alpha: bool,
}

/// A finished job.
pub enum Done {
    Picture { path: String, result: Result<Arc<Picture>, String>, meta: Option<Meta> },
    Thumbs { path: String, result: Result<Thumbs, String>, meta: Option<Meta> },
}

struct Shared {
    pictures: Mutex<VecDeque<String>>,
    pictures_wake: Condvar,
    thumbs: Mutex<VecDeque<String>>,
    thumbs_wake: Condvar,
    done: Mutex<Vec<Done>>,
    /// The picture the decoder is working on.
    active: Mutex<Option<String>>,
    /// Thumbnails asked for pictures the picture decoder is decoding or will decode: it makes
    /// them from what it decoded. (Locked after `pictures`, never before.)
    with_picture: Mutex<Vec<String>>,
    budget: Budget,
    /// The next picture's or thumbnails' number.
    next_id: AtomicU64,
}

impl Shared {
    fn id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }
}

/// The UI side of the decoding threads.
pub struct Loader {
    shared: Arc<Shared>,
    event: Event,
}

impl Loader {
    /// Starts the two worker threads, decoding within `budget`.
    pub fn start(budget: Budget) -> Option<Loader> {
        let event = Event::create().ok()?;
        let shared = Arc::new(Shared {
            pictures: Mutex::new(VecDeque::new()),
            pictures_wake: Condvar::new(),
            thumbs: Mutex::new(VecDeque::new()),
            thumbs_wake: Condvar::new(),
            done: Mutex::new(Vec::new()),
            active: Mutex::new(None),
            with_picture: Mutex::new(Vec::new()),
            budget,
            next_id: AtomicU64::new(1),
        });
        // The picture on screen matters more than thumbnails; both yield to the UI thread.
        let workers =
            [("picture-decoder", false, abi::priority::NORMAL - 1), ("thumbnailer", true, abi::priority::NORMAL - 2)];
        for (name, thumbs, priority) in workers {
            let s = shared.clone();
            let ev = Event::from_handle(event.0.duplicate(None).ok()?);
            runtime::thread::Builder::new()
                .name(name)
                .stack_size(512 * 1024)
                .priority(priority)
                .spawn(move || worker(s, ev, thumbs))
                .ok()?;
        }
        Some(Loader { shared, event })
    }

    /// Asks for the full picture at `path`. An `urgent` request (the picture on screen) is
    /// decoded next; others wait behind the queued requests.
    pub fn request_picture(&self, path: &str, urgent: bool) {
        let mut q = self.shared.pictures.lock();
        if self.shared.active.lock().as_deref() == Some(path) {
            return;
        }
        if let Some(i) = q.iter().position(|p| p == path) {
            if urgent && i > 0 {
                let p = q.remove(i).unwrap_or_default();
                q.push_front(p);
            }
            return;
        }
        if urgent {
            q.push_front(path.to_string());
        } else {
            q.push_back(path.to_string());
        }
        drop(q);
        self.shared.pictures_wake.notify_one();
    }

    /// What decoded pictures may take.
    pub fn budget(&self) -> Budget {
        self.shared.budget
    }

    /// Drops queued picture requests for paths not in `keep`. Thumbnails the picture decoder was
    /// to make of them go back to the thumbnailer.
    pub fn retain_pictures(&self, keep: &[String]) {
        let mut q = self.shared.pictures.lock();
        q.retain(|p| keep.contains(p));
        let mut with = self.shared.with_picture.lock();
        let active = self.shared.active.lock().clone();
        let orphans: Vec<String> = with.extract_if(.., |p| !q.contains(p) && active.as_ref() != Some(p)).collect();
        drop((with, q));
        for p in orphans {
            self.request_thumbs(&p, false);
        }
    }

    /// Asks for the thumbnails of `path`; `first` puts the request at the head of the queue. A
    /// picture the picture decoder decodes (or will) gives them itself.
    pub fn request_thumbs(&self, path: &str, first: bool) {
        {
            let q = self.shared.pictures.lock();
            if self.shared.active.lock().as_deref() == Some(path) || q.iter().any(|p| p == path) {
                let mut with = self.shared.with_picture.lock();
                if !with.iter().any(|p| p == path) {
                    with.push(path.to_string());
                }
                return;
            }
        }
        let mut q = self.shared.thumbs.lock();
        if let Some(i) = q.iter().position(|p| p == path) {
            if !first {
                return;
            }
            q.remove(i);
        }
        if first {
            q.push_front(path.to_string());
        } else {
            q.push_back(path.to_string());
        }
        drop(q);
        self.shared.thumbs_wake.notify_one();
    }

    /// Forgets thumbnail requests that have not started yet; returns their paths.
    pub fn cancel_thumbs(&self) -> Vec<String> {
        let mut paths: Vec<String> = self.shared.thumbs.lock().drain(..).collect();
        paths.extend(self.shared.with_picture.lock().drain(..));
        paths
    }

    /// Finished jobs since the last call.
    pub fn take(&self) -> Vec<Done> {
        let _ = self.event.clear();
        core::mem::take(&mut *self.shared.done.lock())
    }

    /// Signalled when jobs have finished.
    pub fn event_handle(&self) -> RawHandle {
        self.event.raw()
    }
}

fn worker(shared: Arc<Shared>, event: Event, thumbs: bool) {
    let (queue, wake) =
        if thumbs { (&shared.thumbs, &shared.thumbs_wake) } else { (&shared.pictures, &shared.pictures_wake) };
    let mut vfs: Option<vfs::Client> = None;
    loop {
        let path = {
            let mut q = queue.lock();
            loop {
                if let Some(p) = q.pop_front() {
                    if !thumbs {
                        // Set under the queue lock, so that a request always finds the path in
                        // one of the two.
                        *shared.active.lock() = Some(p.clone());
                    }
                    break p;
                }
                q = wake.wait(q);
            }
        };
        if vfs.is_none() {
            vfs = proto::connect(vfs::NAME).ok().map(vfs::Client::new);
        }
        let data = match &vfs {
            Some(client) => read_file(client, &path),
            None => Err(Failure::Service),
        };
        if matches!(data, Err(Failure::Service)) {
            // Reconnect for the next job.
            vfs = None;
        }
        let max_pixels = shared.budget.max_pixels;
        let done = match data {
            Err(e) => {
                let e = e.to_string();
                if thumbs {
                    Done::Thumbs { path, result: Err(e), meta: None }
                } else {
                    take_with_picture(&shared, &path);
                    Done::Picture { path, result: Err(e), meta: None }
                }
            }
            Ok(data) => {
                let bytes = data.bytes();
                let meta = image::read_info(bytes).ok().map(|i| Meta {
                    format: i.format,
                    width: i.width,
                    height: i.height,
                    has_alpha: i.has_alpha,
                    orientation: i.orientation,
                    progressive: i.progressive,
                    file_size: bytes.len() as u64,
                });
                let started = runtime::time::now_ns();
                let decoded = decode(bytes, max_pixels);
                let decode_ms = (runtime::time::now_ns() - started) / 1_000_000;
                drop(data);
                let name = path.rsplit('/').next().unwrap_or("");
                if thumbs {
                    let result = decoded.map(|(img, _)| make_thumbs(&img, shared.id()));
                    let total_ms = (runtime::time::now_ns() - started) / 1_000_000;
                    match &result {
                        Ok(_) => println!("thumbnail of {name}: decoded in {decode_ms} ms, {total_ms} ms in all"),
                        Err(e) => println!("no thumbnail for {name}: {e}"),
                    }
                    Done::Thumbs { path, result, meta }
                } else {
                    // The thumbnails asked for meanwhile, from what was decoded: before the picture's
                    // smaller copies are made, so that the preview shows at once.
                    if take_with_picture(&shared, &path) {
                        let result = match &decoded {
                            Ok((img, _)) => Ok(make_thumbs(img, shared.id())),
                            Err(e) => Err(e.clone()),
                        };
                        shared.done.lock().push(Done::Thumbs { path: path.clone(), result, meta });
                        let _ = event.signal();
                    }
                    let result = decoded.map(|(img, damaged)| Arc::new(make_picture(img, damaged, shared.id())));
                    let total_ms = (runtime::time::now_ns() - started) / 1_000_000;
                    match &result {
                        Ok(p) => println!(
                            "opened {name} ({}x{}): decoded in {decode_ms} ms, ready in {total_ms} ms",
                            p.width(),
                            p.height()
                        ),
                        Err(e) => println!("cannot open {name}: {e}"),
                    }
                    Done::Picture { path, result, meta }
                }
            }
        };
        shared.done.lock().push(done);
        if !thumbs {
            *shared.active.lock() = None;
        }
        let _ = event.signal();
    }
}

/// Whether thumbnails of `path` were asked of the picture decoder (they are then no longer).
fn take_with_picture(shared: &Shared, path: &str) -> bool {
    let mut with = shared.with_picture.lock();
    let asked = with.iter().any(|p| p == path);
    with.retain(|p| p != path);
    asked
}

/// Why a file could not be read.
enum Failure {
    Fs(FsError),
    Service,
    TooLarge,
    NoMemory,
}

impl core::fmt::Display for Failure {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Failure::Fs(FsError::NotFound) => f.write_str("The file no longer exists."),
            Failure::Fs(e) => write!(f, "The file could not be read ({e})."),
            Failure::Service => f.write_str("The file system is not responding."),
            Failure::TooLarge => f.write_str("The file is too large to open."),
            Failure::NoMemory => f.write_str("There is not enough memory to open this picture."),
        }
    }
}

/// A file as read: the file system's copy of it, mapped (never copied again).
struct FileData {
    map: Mapping,
    len: usize,
}

impl FileData {
    fn bytes(&self) -> &[u8] {
        // SAFETY: a read-only mapping of at least `len` bytes that lives as long as `self`; the
        // file system does not write the copy it handed over.
        unsafe { &self.map.as_slice()[..self.len] }
    }
}

/// Reads the file at `path`, if it is not too large (its size is asked first).
fn read_file(vfs: &vfs::Client, path: &str) -> Result<FileData, Failure> {
    match vfs.stat(path.into()) {
        Ok(Ok(st)) if st.size > MAX_FILE => return Err(Failure::TooLarge),
        Ok(Ok(_)) => {}
        Ok(Err(e)) => return Err(Failure::Fs(e)),
        Err(_) => return Err(Failure::Service),
    }
    let (vmo, len) = match vfs.read_file(path.into()) {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => return Err(Failure::Fs(e)),
        Err(_) => return Err(Failure::Service),
    };
    // The file may have grown since it was looked at.
    if len > MAX_FILE {
        return Err(Failure::TooLarge);
    }
    let size = vmo.size().map_err(|_| Failure::Service)?;
    if len as usize > size {
        return Err(Failure::Service);
    }
    let map = Mapping::new(vmo, size.max(4096), abi::map_flags::READ).map_err(|_| Failure::NoMemory)?;
    Ok(FileData { map, len: len as usize })
}

/// Decodes a picture of at most `max_pixels` pixels. Files whose only fault is a checksum
/// (common with PNGs written by tools that do not update CRCs) are decoded without verifying
/// checksums; the flag reports that.
fn decode(data: &[u8], max_pixels: u64) -> Result<(Image, bool), String> {
    let options = DecodeOptions { max_pixels, ..DecodeOptions::DEFAULT };
    match image::decode_with(data, &options) {
        Err(ImageError::ChecksumMismatch(_)) => {
            let lax = DecodeOptions { verify_checksums: false, ..options };
            image::decode_with(data, &lax).map(|img| (img, true))
        }
        other => other.map(|img| (img, false)),
    }
    .map_err(describe)
}

/// A decoding error in words for the user.
fn describe(e: ImageError) -> String {
    match e {
        ImageError::UnknownFormat => "This file is not a picture Photos can open.".into(),
        ImageError::Truncated => "The file is incomplete: it ends before the picture does.".into(),
        ImageError::Invalid(why) => format!("The file is damaged ({why})."),
        ImageError::Unsupported(what) => format!("This picture uses a feature Photos does not support ({what})."),
        ImageError::TooLarge { width, height } => {
            format!("The picture is too large for this computer's memory ({width} × {height} pixels).")
        }
        ImageError::ChecksumMismatch(_) => "The file is damaged (checksum mismatch).".into(),
        ImageError::OutOfMemory => "There is not enough memory to open this picture.".into(),
        e => format!("The picture could not be decoded ({e})."),
    }
}

/// Builds the mip chain of a decoded picture (as far as there is memory for it: the picture
/// shows from the levels made).
fn make_picture(img: Image, damaged: bool, id: u64) -> Picture {
    let has_alpha = img.pixels.iter().any(|p| p >> 24 != 0xFF);
    let mut levels = vec![Bitmap::from_straight(img.width as i32, img.height as i32, img.pixels)];
    loop {
        let last = &levels[levels.len() - 1];
        if last.width.max(last.height) <= SMALLEST_LEVEL || last.width < 2 || last.height < 2 {
            break;
        }
        let Some(next) = half(last) else { break };
        levels.push(next);
    }
    Picture { id, levels, has_alpha, damaged }
}

/// Halves a premultiplied bitmap with a 2x2 box filter (an odd last row or column is dropped);
/// `None` without the memory for it.
fn half(b: &Bitmap) -> Option<Bitmap> {
    let (w, h) = (b.width / 2, b.height / 2);
    let mut pixels = Vec::new();
    pixels.try_reserve_exact(w as usize * h as usize).ok()?;
    pixels.resize(w as usize * h as usize, 0);
    let mut out = Bitmap { width: w, height: h, pixels };
    let sw = b.width as usize;
    for y in 0..h as usize {
        let r0 = &b.pixels[2 * y * sw..2 * y * sw + 2 * w as usize];
        let r1 = &b.pixels[(2 * y + 1) * sw..(2 * y + 1) * sw + 2 * w as usize];
        let dst = &mut out.pixels[y * w as usize..(y + 1) * w as usize];
        for (x, d) in dst.iter_mut().enumerate() {
            let (a, b, c, e) = (r0[2 * x], r0[2 * x + 1], r1[2 * x], r1[2 * x + 1]);
            // Two channels per lane: every sum of four 8-bit values fits in 10 bits.
            let rb = ((a & 0x00FF_00FF) + (b & 0x00FF_00FF) + (c & 0x00FF_00FF) + (e & 0x00FF_00FF) + 0x0002_0002) >> 2;
            let ag = (((a >> 8) & 0x00FF_00FF)
                + ((b >> 8) & 0x00FF_00FF)
                + ((c >> 8) & 0x00FF_00FF)
                + ((e >> 8) & 0x00FF_00FF)
                + 0x0002_0002)
                >> 2;
            *d = (rb & 0x00FF_00FF) | ((ag & 0x00FF_00FF) << 8);
        }
    }
    Some(out)
}

fn make_thumbs(img: &Image, id: u64) -> Thumbs {
    let has_alpha = img.pixels.iter().any(|p| p >> 24 != 0xFF);
    // An intermediate size with enough pixels for the crops of any aspect ratio.
    let base = image::thumbnail(img, BASE_SIZE, BASE_SIZE, Filter::Bilinear);
    let card = finish(image::cover(&base, CARD_W, CARD_H, Filter::Bilinear), has_alpha, 10.0);
    let strip = finish(image::cover(&base, STRIP_W, STRIP_H, Filter::Bilinear), has_alpha, 6.0);
    let preview = image::thumbnail(&base, PREVIEW_W, PREVIEW_H, Filter::Bilinear);
    let preview = Bitmap::from_straight(preview.width as i32, preview.height as i32, preview.pixels);
    Thumbs { id, card, strip, preview, has_alpha }
}

/// Puts a transparent thumbnail on a checkerboard and rounds its corners; returns it
/// premultiplied.
fn finish(mut img: Image, has_alpha: bool, radius: f32) -> Bitmap {
    let (w, h) = (img.width as usize, img.height as usize);
    if has_alpha {
        for (i, p) in img.pixels.iter_mut().enumerate() {
            let (x, y) = (i % w, i / w);
            let bg: u32 = if ((x / 6) ^ (y / 6)) & 1 == 0 { 0x46_464E } else { 0x38_383F };
            let a = *p >> 24;
            let mix = |s: u32| -> u32 {
                let (f, b) = ((*p >> s) & 0xFF, (bg >> s) & 0xFF);
                (f * a + b * (255 - a) + 127) / 255
            };
            *p = 0xFF00_0000 | mix(16) << 16 | mix(8) << 8 | mix(0);
        }
    }
    round_corners(&mut img.pixels, w, h, radius);
    Bitmap::from_straight(w as i32, h as i32, img.pixels)
}

/// Fades the corners of a straight-alpha image outside a rounded rectangle (anti-aliased).
pub fn round_corners(pixels: &mut [u32], w: usize, h: usize, radius: f32) {
    let r = radius.min(w as f32 / 2.0).min(h as f32 / 2.0);
    let n = r.ceil() as usize;
    for y in 0..n.min(h) {
        for x in 0..n.min(w) {
            // Distance from the corner circle's centre, for the top-left corner.
            let (dx, dy) = (r - (x as f32 + 0.5), r - (y as f32 + 0.5));
            let d = (dx * dx + dy * dy).sqrt();
            let cover = (r - d + 0.5).clamp(0.0, 1.0);
            if cover >= 1.0 {
                continue;
            }
            for (cx, cy) in [(x, y), (w - 1 - x, y), (x, h - 1 - y), (w - 1 - x, h - 1 - y)] {
                let p = &mut pixels[cy * w + cx];
                let a = ((*p >> 24) as f32 * cover + 0.5) as u32;
                *p = (*p & 0x00FF_FFFF) | a << 24;
            }
        }
    }
}
