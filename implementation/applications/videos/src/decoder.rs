// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Starting a decoder: its program read once, then a process with nothing
//! but its control channel (no registry, so no file, service or network but
//! what it is sent), a client to speak `vproto::media` with it, and its
//! heartbeat.
//!
//! A decoder is a process of its own, which the kernel does not end with
//! Videos: every decoder started is noted here until it is dropped, and
//! [`end_all`] ends those still running when Videos ends (one that hangs in
//! FFmpeg would otherwise run on for good).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use vabi::map_flags;
use vproto::fs::{FsError, vfs};
use vproto::media::{self, Heartbeat, media::Client};
use vrt::object::{Channel, Process};
use vrt::process::Spawn;
use vrt::sync::Mutex;
use vrt::vm::Mapping;

/// The decoder's program, read once (a read-only mapping of the file
/// system's copy).
pub struct Image {
    path: String,
    map: Mapping,
}

impl Image {
    /// Reads the program at `path`.
    pub fn read(vfs: &vfs::Client, path: &str) -> Result<Image, String> {
        let (vmo, size) = match vfs.read_file(path.into()) {
            Ok(Ok(r)) => r,
            Ok(Err(FsError::NotFound)) if path == crate::engine::DECODER => {
                return Err(format!("{path} is not installed (the image was built without the C toolchain)"));
            }
            Ok(Err(FsError::NotFound)) => return Err(format!("{path} does not exist")),
            Ok(Err(e)) => return Err(format!("{path}: {e}")),
            Err(e) => return Err(format!("{path}: {e:?}")),
        };
        let map = Mapping::new(vmo, size as usize, map_flags::READ).map_err(|e| format!("{path}: {e:?}"))?;
        Ok(Image { path: path.into(), map })
    }
}

/// The decoders running, so that they end with Videos.
struct Running {
    /// Videos is ending: no decoder starts any more.
    ending: bool,
    /// Each decoder's number and its process (a handle of our own).
    processes: Vec<(u64, Process)>,
    next: u64,
}

static RUNNING: Mutex<Running> = Mutex::new(Running { ending: false, processes: Vec::new(), next: 0 });

/// Ends every decoder still running, and starts no more: Videos ends.
pub fn end_all() {
    let mut running = RUNNING.lock();
    running.ending = true;
    for (_, p) in running.processes.drain(..) {
        let _ = p.kill_tree();
    }
}

/// A running decoder, killed (with what it started) when dropped.
pub struct Decoder {
    pub client: Client,
    pub process: Process,
    pub build: String,
    /// What it counts its rounds in.
    pub heartbeat: Heartbeat,
    /// Its number among the decoders running.
    number: u64,
}

impl Drop for Decoder {
    fn drop(&mut self) {
        let _ = self.process.kill_tree();
        RUNNING.lock().processes.retain(|(n, _)| *n != self.number);
    }
}

/// Starts a decoder from `image` as process `name`; each call waits at most
/// `timeout_ns` for its answer.
pub fn start(image: &Image, name: &'static str, timeout_ns: u64) -> Result<Decoder, String> {
    let (ours, theirs) = Channel::create().map_err(|e| format!("{e:?}"))?;
    if RUNNING.lock().ending {
        return Err("Videos is ending".into());
    }
    // SAFETY: a read-only mapping of the program, alive while it is loaded.
    let process = Spawn::new(name)
        .path(&image.path)
        .handle(media::CONTROL_ROLE, theirs.into_handle())
        .start(unsafe { image.map.as_slice() })
        .map_err(|e| format!("{}: {e}", image.path))?;
    // Noted at once, so that it ends with Videos even if it never answers.
    let number = {
        let mut running = RUNNING.lock();
        let copy = process.0.duplicate(None).map(Process::from_handle);
        match copy {
            Ok(copy) if !running.ending => {
                running.next += 1;
                let number = running.next;
                running.processes.push((number, copy));
                number
            }
            _ => {
                // Videos is ending (or the handle cannot be copied, so it
                // could not be ended then): not started after all.
                let _ = process.kill_tree();
                return Err("the decoder cannot be kept".into());
            }
        }
    };
    let (heartbeat, page) = match Heartbeat::create() {
        Ok(h) => h,
        Err(e) => {
            forget(number, &process);
            return Err(format!("no memory for its heartbeat ({e:?})"));
        }
    };
    let mut decoder = Decoder { client: Client::new(ours), process, build: String::new(), heartbeat, number };
    decoder.client.set_timeout(timeout_ns);
    // Dropped (and so killed) on any error from here on.
    let hello = decoder.client.hello(media::VERSION).map_err(|e| format!("the decoder does not answer ({e:?})"))?;
    if hello.version != media::VERSION {
        return Err(format!("the decoder speaks version {}, not {}", hello.version, media::VERSION));
    }
    match decoder.client.set_heartbeat(page) {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(format!("the decoder refused its heartbeat ({e})")),
        Err(e) => return Err(format!("the decoder does not answer ({e:?})")),
    }
    decoder.build = hello.build;
    Ok(decoder)
}

/// Ends a decoder that could not be made a [`Decoder`] of.
fn forget(number: u64, process: &Process) {
    let _ = process.kill_tree();
    RUNNING.lock().processes.retain(|(n, _)| *n != number);
}
