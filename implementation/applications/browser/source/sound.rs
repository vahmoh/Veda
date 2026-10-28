// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Pages' sound: the audio service's streams that the engine's GStreamer
//! sink (`engine/media.c`) plays media and Web Audio through, in WebKit's
//! web processes (this program, started as one), where pages play. Each
//! sink has a stream of its own, opened as its first samples come, on one
//! connection of the process's: the service keeps no more than 16 streams
//! a connection, and 48 connections in all.
//!
//! The sink calls these functions from GStreamer's threads: its ring
//! buffer's writes come on one, while others pause, flush and ask how much
//! is still to be heard; each [`Sound`] takes turns under its lock, and the
//! connection under its own.

use std::ffi::c_int;
use std::sync::{Mutex, MutexGuard, PoisonError};

use proto::audio::{OpenError, OutputStream, StreamSpec, audio};
use runtime::object::Event;

/// The process's connection to the audio service, made as the first
/// stream opens, and again once it broke.
static SERVICE: Mutex<Option<audio::Client>> = Mutex::new(None);

/// Runs `call` with the connection: `None` if there is none or the call
/// failed (the connection is then dropped, for the next call to make anew).
fn with_service<T>(call: impl FnOnce(&audio::Client) -> Result<T, ipc::IpcError>) -> Option<T> {
    let mut service = SERVICE.lock().unwrap_or_else(PoisonError::into_inner);
    if service.is_none() {
        *service = proto::connect(audio::NAME).ok().map(audio::Client::new);
    }
    let result = call(service.as_ref()?);
    if result.is_err() {
        *service = None;
    }
    result.ok()
}

/// A stream of the audio service, as a sink plays into it.
pub struct Sound {
    stream: Mutex<OutputStream>,
    /// The stream's event, which the service signals as room frees up: a
    /// handle of its own, waited on outside the stream's lock.
    room: Event,
    id: u32,
}

impl Sound {
    fn stream(&self) -> MutexGuard<'_, OutputStream> {
        self.stream.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A stream of `channels` (1 or 2) interleaved 16-bit samples at `rate`,
/// with room for about `buffer_frames` frames; null if none could be
/// opened.
#[unsafe(no_mangle)]
pub extern "C" fn browser_sound_open(rate: u32, channels: u32, buffer_frames: u32) -> *mut Sound {
    let spec = StreamSpec { channels, buffer_frames, ..StreamSpec::stereo(rate, 0, "Browser") };
    let opened = with_service(|client| match OutputStream::open(client, spec) {
        Err(OpenError::Ipc(e)) => Err(e),
        other => Ok(other),
    });
    let stream = match opened {
        Some(Ok(stream)) => stream,
        Some(Err(OpenError::Audio(e))) => {
            eprintln!("browser: a page's sound has no stream: {e}");
            return std::ptr::null_mut();
        }
        Some(Err(_)) | None => {
            eprintln!("browser: a page's sound has no stream: the audio service cannot be reached");
            return std::ptr::null_mut();
        }
    };
    let id = stream.id;
    let Ok(room) = stream.event().0.duplicate(None).map(Event::from_handle) else {
        let _ = with_service(|client| client.close(id));
        return std::ptr::null_mut();
    };
    Box::into_raw(Box::new(Sound { stream: Mutex::new(stream), room, id }))
}

/// Writes what fits of `frames` frames from `samples`: how many it wrote.
///
/// # Safety
/// `sound` is one that `browser_sound_open` gave and that is not closed;
/// `samples` holds `frames` frames.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn browser_sound_write(sound: *const Sound, samples: *const i16, frames: usize) -> usize {
    // SAFETY: per the caller.
    let stream = unsafe { &*sound }.stream();
    let len = frames.saturating_mul(stream.channels() as usize);
    // SAFETY: per the caller, `samples` holds `frames` frames of the
    // stream's channels.
    let samples = unsafe { std::slice::from_raw_parts(samples, len) };
    stream.write(samples)
}

/// Waits until `frames` frames fit in the stream, or `timeout_ms` passes.
///
/// # Safety
/// `sound` is one that `browser_sound_open` gave and that is not closed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn browser_sound_wait(sound: *const Sound, frames: u32, timeout_ms: u32) {
    // SAFETY: per the caller.
    let sound = unsafe { &*sound };
    let deadline = runtime::time::now_ns().saturating_add(u64::from(timeout_ms) * 1_000_000);
    let _ = sound.room.clear();
    let short = {
        let stream = sound.stream();
        stream.free() < frames.min(stream.ring().capacity())
    };
    if short {
        let _ = sound.room.wait(abi::signals::SIGNALED, deadline);
    }
}

/// The frames written that have not been heard yet.
///
/// # Safety
/// `sound` is one that `browser_sound_open` gave and that is not closed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn browser_sound_delay(sound: *const Sound) -> u32 {
    // SAFETY: per the caller.
    let stream = unsafe { &*sound }.stream();
    stream.written().saturating_sub(stream.played_now()).min(u64::from(u32::MAX)) as u32
}

/// Pauses or resumes the stream, keeping what it holds.
///
/// # Safety
/// `sound` is one that `browser_sound_open` gave and that is not closed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn browser_sound_pause(sound: *const Sound, paused: c_int) {
    // SAFETY: per the caller.
    let id = unsafe { &*sound }.id;
    let _ = with_service(|client| client.set_paused(id, paused != 0));
}

/// Drops what the stream holds that has not been heard.
///
/// # Safety
/// `sound` is one that `browser_sound_open` gave and that is not closed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn browser_sound_flush(sound: *const Sound) {
    // SAFETY: per the caller.
    let id = unsafe { &*sound }.id;
    let _ = with_service(|client| client.flush(id));
}

/// Closes the stream.
///
/// # Safety
/// `sound` is null, or one that `browser_sound_open` gave and that is not
/// closed, which no other thread uses any more.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn browser_sound_close(sound: *mut Sound) {
    if sound.is_null() {
        return;
    }
    // SAFETY: per the caller, it is ours again.
    let sound = unsafe { Box::from_raw(sound) };
    let _ = with_service(|client| client.close(sound.id));
}
