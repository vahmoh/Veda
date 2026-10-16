// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The library's worker: what each video is (its length, size and codecs)
//! and a picture of it for its card, and the previews above the seek bar.
//!
//! It has a decoder of its own, opened for pictures only (`THUMBNAILS`), so
//! that making them never holds playback up; it starts the decoder when
//! asked something and lets it go after a while with nothing to do. Seek
//! bar previews go before cards (the pointer waits for them), and the file
//! they come from stays open between them.

use std::collections::VecDeque;
use std::sync::Arc;

use vabi::map_flags;
use vproto::fs::{open_flags, vfs};
use vproto::media::{MediaInfo, OpenOptions, open_flags as media_open};
use vrt::object::Event;
use vrt::println;
use vrt::sync::Mutex;
use vrt::vm::Mapping;

use crate::decoder::{self, Decoder, Image};

/// A card's picture fits in this.
pub const CARD: (u32, u32) = (240, 135);
/// A seek preview's.
pub const PREVIEW: (u32, u32) = (192, 108);
/// The worker's decoder goes after this long with nothing to do (ns).
const IDLE_NS: u64 = 20_000_000_000;
const TIMEOUT_NS: u64 = 10_000_000_000;

/// Opaque `0xAARRGGBB` pixels.
#[derive(Debug, Clone)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u32>,
}

pub enum Request {
    /// What `path` is, and its card's picture.
    Card { path: String },
    /// A picture of `path` at `at_us` (the seek bar's preview `id`).
    Preview { path: String, at_us: i64, id: u64 },
}

pub enum Done {
    Card {
        path: String,
        info: Result<MediaInfo, String>,
        picture: Option<Picture>,
    },
    Preview {
        id: u64,
        at_us: i64,
        picture: Picture,
    },
    /// The decoder cannot be started (said once; cards are not made).
    Unavailable(String),
}

pub struct Shared {
    queue: Mutex<VecDeque<Request>>,
    wake: Event,
    done: Mutex<Vec<Done>>,
    /// Signalled when something is done.
    pub ready: Event,
}

impl Shared {
    /// Asks for a card's picture (after those asked for before).
    pub fn card(&self, path: String) {
        self.queue.lock().push_back(Request::Card { path });
        let _ = self.wake.signal();
    }

    /// Asks for a preview, before any card, replacing a preview not made
    /// yet.
    pub fn preview(&self, path: String, at_us: i64, id: u64) {
        let mut q = self.queue.lock();
        q.retain(|r| !matches!(r, Request::Preview { .. }));
        q.push_front(Request::Preview { path, at_us, id });
        drop(q);
        let _ = self.wake.signal();
    }

    /// Forgets the cards not made yet (another folder).
    pub fn clear_cards(&self) {
        self.queue.lock().retain(|r| !matches!(r, Request::Card { .. }));
    }

    pub fn take(&self) -> Vec<Done> {
        core::mem::take(&mut *self.done.lock())
    }
}

/// Starts the worker; `decoder` is the decoder's program.
pub fn start(decoder: String) -> Option<Arc<Shared>> {
    let shared = Arc::new(Shared {
        queue: Mutex::new(VecDeque::new()),
        wake: Event::create().ok()?,
        done: Mutex::new(Vec::new()),
        ready: Event::create().ok()?,
    });
    let ours = shared.clone();
    vrt::thread::Builder::new()
        .name("thumbnails")
        .priority(vabi::priority::NORMAL - 2)
        .stack_size(256 << 10)
        .spawn(move || {
            Worker { shared: ours, path: decoder, image: None, missing: None, decoder: None, vfs: None, open: None }
                .run()
        })
        .ok()?;
    Some(shared)
}

struct Worker {
    shared: Arc<Shared>,
    path: String,
    image: Option<Image>,
    /// Why the decoder cannot be started.
    missing: Option<String>,
    decoder: Option<Decoder>,
    vfs: Option<vfs::Client>,
    /// The file open in the decoder, and what it is.
    open: Option<(String, Result<MediaInfo, String>)>,
}

impl Worker {
    fn run(&mut self) {
        loop {
            let next = self.shared.queue.lock().pop_front();
            let Some(request) = next else {
                let deadline =
                    if self.decoder.is_some() { vrt::time::now_ns() + IDLE_NS } else { vabi::DEADLINE_INFINITE };
                if self.shared.wake.wait(vabi::signals::SIGNALED, deadline).is_err() {
                    // Nothing to do for a while: the decoder goes.
                    self.decoder = None;
                    self.open = None;
                }
                let _ = self.shared.wake.clear();
                continue;
            };
            if self.missing.is_some() {
                continue;
            }
            let done = match request {
                Request::Card { path } => {
                    let info = self.open_file(&path);
                    let picture = info.as_ref().ok().and_then(|i| {
                        let at = i.duration_us.map_or(2_000_000, |d| (d / 10).clamp(0, 30_000_000));
                        self.picture(at, CARD)
                    });
                    Done::Card { path, info, picture }
                }
                Request::Preview { path, at_us, id } => {
                    if self.open_file(&path).is_err() {
                        continue;
                    }
                    let Some(picture) = self.picture(at_us, PREVIEW) else { continue };
                    Done::Preview { id, at_us, picture }
                }
            };
            if self.missing.is_some() {
                // Not the video's fault: its card waits.
                continue;
            }
            self.shared.done.lock().push(done);
            let _ = self.shared.ready.signal();
        }
    }

    /// The decoder, started if need be.
    fn decoder(&mut self) -> Result<&Decoder, String> {
        if self.vfs.is_none() {
            self.vfs = vproto::connect(vfs::NAME).ok().map(vfs::Client::new);
        }
        if self.image.is_none() {
            let vfs = self.vfs.as_ref().ok_or("the file system is not running")?;
            match Image::read(vfs, &self.path) {
                Ok(image) => self.image = Some(image),
                Err(e) => {
                    self.missing = Some(e.clone());
                    self.shared.done.lock().push(Done::Unavailable(e.clone()));
                    let _ = self.shared.ready.signal();
                    return Err(e);
                }
            }
        }
        if self.decoder.as_ref().is_some_and(|d| d.process.wait(vabi::signals::TERMINATED, 0).is_ok()) {
            self.decoder = None;
            self.open = None;
        }
        if self.decoder.is_none() {
            self.decoder = Some(decoder::start(self.image.as_ref().unwrap(), "mediadec-thumbs", TIMEOUT_NS)?);
        }
        Ok(self.decoder.as_ref().unwrap())
    }

    /// Opens `path` in the decoder unless it is open already: what it is.
    fn open_file(&mut self, path: &str) -> Result<MediaInfo, String> {
        if let Some((p, info)) = &self.open
            && p == path
        {
            return info.clone();
        }
        let info = self.open_now(path);
        if let Err(e) = &info
            && self.missing.is_none()
        {
            println!("library: {path}: {e}");
        }
        self.open = Some((path.into(), info.clone()));
        info
    }

    fn open_now(&mut self, path: &str) -> Result<MediaInfo, String> {
        self.decoder()?;
        let vfs = self.vfs.as_ref().ok_or("the file system is not running")?;
        let channel = match vfs.open_file(path.into(), open_flags::READ) {
            Ok(Ok((ch, _))) => ch,
            Ok(Err(e)) => return Err(format!("{e}")),
            Err(e) => return Err(format!("{e:?}")),
        };
        let d = self.decoder.as_ref().ok_or("no decoder")?;
        let options = OpenOptions { threads: 1, max_width: 0, max_height: 0, flags: media_open::THUMBNAILS };
        match d.client.open(channel, options) {
            Ok(Ok(info)) if info.is_valid() => Ok(info),
            Ok(Ok(_)) => Err("described wrongly".into()),
            Ok(Err(e)) => Err(crate::engine::sentence(e).into()),
            Err(e) => {
                // A decoder that fails here is started again next time.
                self.decoder = None;
                Err(format!("the decoder stopped ({e:?})"))
            }
        }
    }

    /// A picture of the open file at `at_us` fitting in `size`.
    fn picture(&mut self, at_us: i64, size: (u32, u32)) -> Option<Picture> {
        let d = self.decoder.as_ref()?;
        let t = match d.client.thumbnail(at_us, size.0, size.1) {
            Ok(Ok(t)) => t,
            Ok(Err(_)) => return None,
            Err(_) => {
                self.decoder = None;
                self.open = None;
                return None;
            }
        };
        let (w, h, stride) = (t.width.min(size.0 * 2), t.height.min(size.1 * 2), t.stride);
        if w == 0 || h == 0 || stride < w {
            return None;
        }
        let len = (stride as usize * h as usize * 4).next_multiple_of(4096);
        let map = Mapping::new(t.pixels, len, map_flags::READ).ok()?;
        let mut pixels = vec![0u32; w as usize * h as usize];
        for y in 0..h as usize {
            // SAFETY: the mapping holds `h` rows of `stride` pixels; they are
            // copied, never borrowed (the decoder could still write them).
            unsafe {
                core::ptr::copy_nonoverlapping(
                    (map.as_ptr() as *const u32).add(y * stride as usize),
                    pixels.as_mut_ptr().add(y * w as usize),
                    w as usize,
                )
            };
        }
        for p in &mut pixels {
            *p |= 0xFF00_0000;
        }
        Some(Picture { width: w, height: h, pixels })
    }
}
