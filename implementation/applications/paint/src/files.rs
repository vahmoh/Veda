// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Opening and saving pictures on a thread of their own: decoding or encoding a large picture
//! takes seconds under emulation, and the window stays responsive meanwhile. The thread takes
//! jobs from one channel and gives back what it did on another, signalling an [`Event`] that
//! the window waits on (see `App::wait_handles`); it ends with the window's side.
//!
//! Pictures move through `files`, in one piece of shared memory each way.

use std::cell::Cell;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Instant;

use abi::RawHandle;
use gfx::Bitmap;
use image::Format;
use paint_core::codec::{self, Opened};
use rt::object::Event;

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

/// The window's side of the worker.
pub struct Files {
    jobs: Sender<Job>,
    done: Receiver<Done>,
    event: Event,
    /// Jobs asked for and not yet taken back.
    pending: Cell<usize>,
}

impl Files {
    /// Starts the worker.
    pub fn start() -> Option<Files> {
        let event = Event::create().ok()?;
        let signal = Event::from_handle(event.0.duplicate(None).ok()?);
        let (jobs, todo) = mpsc::channel();
        let (finished, done) = mpsc::channel();
        thread::Builder::new()
            .name("paint-files".into())
            .stack_size(512 * 1024)
            .spawn(move || {
                // Behind the window, which draws as it works.
                let _ = rt::thread::set_priority(abi::priority::NORMAL - 1);
                let fs = files::Fs::connect();
                for job in todo {
                    if finished.send(work(&fs, job)).is_err() {
                        break;
                    }
                    let _ = signal.signal();
                }
            })
            .ok()?;
        Some(Files { jobs, done, event, pending: Cell::new(0) })
    }

    fn ask(&self, job: Job) {
        if self.jobs.send(job).is_ok() {
            self.pending.set(self.pending.get() + 1);
        }
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
        // Cleared first: what finishes meanwhile signals it again.
        let _ = self.event.clear();
        let done: Vec<Done> = self.done.try_iter().collect();
        self.pending.set(self.pending.get().saturating_sub(done.len()));
        done
    }

    /// Whether a job is under way.
    pub fn busy(&self) -> bool {
        self.pending.get() > 0
    }

    /// Signalled when a job has finished.
    pub fn event_handle(&self) -> RawHandle {
        self.event.raw()
    }
}

/// Does `job`, saying in the log what it did and how long it took.
fn work(fs: &files::Fs, job: Job) -> Done {
    let started = Instant::now();
    let ms = || started.elapsed().as_millis();
    match job {
        Job::Open { path, max_pixels } => {
            let result = fs
                .read_limited(&path, MAX_FILE)
                .map_err(|e| match e {
                    files::fs::Error::TooLarge => format!("it is larger than {} MB", MAX_FILE >> 20),
                    e => e.to_string(),
                })
                .and_then(|bytes| codec::decode(&bytes, max_pixels).map_err(|e| e.to_string()));
            match &result {
                Ok(o) => println!(
                    "opened {path} ({}x{} {}) in {} ms",
                    o.picture.width,
                    o.picture.height,
                    o.format.name(),
                    ms()
                ),
                Err(e) => println!("cannot open {path}: {e}"),
            }
            Done::Opened { path, result }
        }
        Job::Save { path, picture, format, version } => {
            let result = codec::encode(&picture, format, JPEG_QUALITY)
                .map_err(|e| e.to_string())
                .and_then(|bytes| fs.write(&path, &bytes).map(|()| bytes.len()).map_err(|e| e.to_string()));
            match &result {
                Ok(n) => println!("saved {path} ({n} bytes, {}) in {} ms", format.name(), ms()),
                Err(e) => println!("cannot save {path}: {e}"),
            }
            Done::Saved { path, format, version, result }
        }
    }
}
