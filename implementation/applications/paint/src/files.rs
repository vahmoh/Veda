// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Opening and saving pictures on a thread of their own: decoding or encoding a large picture
//! takes seconds under emulation, and the window stays responsive meanwhile. Results come back
//! through a mutex and an [`Event`] that the window waits on (see `App::wait_handles`).

use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;

use paint_core::codec::{self, Opened};
use vabi::RawHandle;
use vgfx::Bitmap;
use vimage::Format;
use vrt::object::Event;
use vrt::sync::{Condvar, Mutex};

/// The largest file opened (bytes).
const MAX_FILE: usize = 256 << 20;
/// A JPEG's quality.
const JPEG_QUALITY: u8 = 92;

/// What the worker is asked to do.
enum Job {
    Open { path: String, max_pixels: u64 },
    Save { path: String, picture: Bitmap, format: Format, version: u64 },
}

/// What it did.
pub enum Done {
    Opened { path: String, result: Result<Opened, String> },
    Saved { path: String, format: Format, version: u64, result: Result<usize, String> },
}

struct Shared {
    jobs: Mutex<VecDeque<Job>>,
    wake: Condvar,
    done: Mutex<Vec<Done>>,
    /// Jobs asked for and not yet taken back.
    pending: Mutex<usize>,
}

/// The window's side of the worker.
pub struct Files {
    shared: Arc<Shared>,
    event: Event,
}

impl Files {
    /// Starts the worker.
    pub fn start() -> Option<Files> {
        let event = Event::create().ok()?;
        let signal = Event::from_handle(event.0.duplicate(None).ok()?);
        let shared = Arc::new(Shared {
            jobs: Mutex::new(VecDeque::new()),
            wake: Condvar::new(),
            done: Mutex::new(Vec::new()),
            pending: Mutex::new(0),
        });
        let worker = shared.clone();
        vrt::thread::Builder::new()
            .name("paint-files")
            .stack_size(512 * 1024)
            .priority(vabi::priority::NORMAL - 1)
            .spawn(move || work(&worker, &signal))
            .ok()?;
        Some(Files { shared, event })
    }

    fn ask(&self, job: Job) {
        *self.shared.pending.lock() += 1;
        self.shared.jobs.lock().push_back(job);
        self.shared.wake.notify_one();
    }

    /// Reads and decodes `path` (a picture of at most `max_pixels`).
    pub fn open(&self, path: &str, max_pixels: u64) {
        self.ask(Job::Open { path: path.into(), max_pixels });
    }

    /// Encodes `picture` (the document's `version`) in `format` and writes it to `path`.
    pub fn save(&self, path: &str, picture: Bitmap, format: Format, version: u64) {
        self.ask(Job::Save { path: path.into(), picture, format, version });
    }

    /// What finished since the last call.
    pub fn take(&self) -> Vec<Done> {
        let _ = self.event.clear();
        let done = core::mem::take(&mut *self.shared.done.lock());
        let mut pending = self.shared.pending.lock();
        *pending = pending.saturating_sub(done.len());
        done
    }

    /// Whether a job is under way.
    pub fn busy(&self) -> bool {
        *self.shared.pending.lock() > 0
    }

    /// Signalled when a job has finished.
    pub fn event_handle(&self) -> RawHandle {
        self.event.raw()
    }
}

fn work(shared: &Shared, signal: &Event) {
    let fs = vfiles::Fs::connect();
    loop {
        let job = {
            let mut q = shared.jobs.lock();
            loop {
                if let Some(j) = q.pop_front() {
                    break j;
                }
                q = shared.wake.wait(q);
            }
        };
        let started = vrt::time::now_ns();
        let done = match job {
            Job::Open { path, max_pixels } => {
                let result = fs
                    .read_limited(&path, MAX_FILE)
                    .map_err(|e| match e {
                        vfiles::fs::Error::TooLarge => format!("it is larger than {} MB", MAX_FILE >> 20),
                        e => e.to_string(),
                    })
                    .and_then(|bytes| codec::decode(&bytes, max_pixels).map_err(|e| e.to_string()));
                match &result {
                    Ok(o) => vrt::println!(
                        "opened {path} ({}x{} {}) in {} ms",
                        o.picture.width,
                        o.picture.height,
                        o.format.name(),
                        (vrt::time::now_ns() - started) / 1_000_000
                    ),
                    Err(e) => vrt::println!("cannot open {path}: {e}"),
                }
                Done::Opened { path, result }
            }
            Job::Save { path, picture, format, version } => {
                let result = codec::encode(&picture, format, JPEG_QUALITY)
                    .map_err(|e| e.to_string())
                    .and_then(|bytes| fs.write(&path, &bytes).map(|()| bytes.len()).map_err(|e| e.to_string()));
                match &result {
                    Ok(n) => vrt::println!(
                        "saved {path} ({} bytes, {}) in {} ms",
                        n,
                        format.name(),
                        (vrt::time::now_ns() - started) / 1_000_000
                    ),
                    Err(e) => vrt::println!("cannot save {path}: {e}"),
                }
                Done::Saved { path, format, version, result }
            }
        };
        shared.done.lock().push(done);
        let _ = signal.signal();
    }
}
