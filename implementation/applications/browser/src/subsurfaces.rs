// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Pages' subsurfaces: each view's page is a subsurface of the browser's
//! window (see `proto::display`), which the engine's thread
//! (`engine/engine.c`) presents WebKit's frames to, in the memory WebKit's
//! page processes drew them in: the window system shows each as it comes,
//! without the browser copying it or drawing its window again. The
//! window's frames place the shown tab's subsurface ([`crate::page`]).
//!
//! The engine's thread calls these functions alone, on a connection of the
//! process's own to the window system, made as the first subsurface is.
//! A thread of their own hears that the frames presented are on the screen,
//! from all the subsurfaces' events, which come on one channel, and tells
//! the engine (`engine_subsurface_shown`), whose WebKit then draws the
//! next: pages are paced by the screen, as the window is. It hears too of
//! the buffers given back late, which the window system's GPU read in place
//! while a frame it was drawing was not done (`engine_subsurface_released`).

use std::ffi::c_int;
use std::sync::{Mutex, PoisonError};

use abi::signals;
use proto::display::{self as dp, SubsurfaceEvent, display};
use rt::object::{Channel, Handle, Vmo};

unsafe extern "C" {
    fn engine_subsurface_shown(subsurface: u32);
    fn engine_subsurface_released(subsurface: u32, buffer: u32);
    /// `<veda/memory.h>`: the memory of the memory file on `fd`.
    fn veda_memory_file(fd: c_int, vmo: *mut abi::RawHandle) -> c_int;
}

/// `<errno.h>`'s, as the bridge reads them.
const ENOMEM: c_int = 12;
const EINVAL: c_int = 22;

/// The engine's connection to the window system, and the window system's
/// end of the channel its subsurfaces' events come on.
struct Link {
    display: display::Client,
    events: Channel,
}

static LINK: Mutex<Option<Link>> = Mutex::new(None);

/// Logs `what` the first few times, then every hundredth (`count`
/// counts them), so that a page that keeps failing cannot flood the log.
fn now_and_then(count: &Mutex<u32>, what: impl FnOnce() -> String) {
    let mut n = count.lock().unwrap_or_else(PoisonError::into_inner);
    if *n < 3 || n.is_multiple_of(100) {
        eprintln!("browser: {}", what());
    }
    *n = n.wrapping_add(1);
}

static FAILURES: Mutex<u32> = Mutex::new(0);

/// Runs `call` with the connection, made first if there is none (and the
/// thread that hears the frames shown started): `None` if there is none.
fn with_link<T>(call: impl FnOnce(&Link) -> T) -> Option<T> {
    let mut link = LINK.lock().unwrap_or_else(PoisonError::into_inner);
    if link.is_none() {
        let display = proto::connect(display::NAME).ok().map(display::Client::new)?;
        // The window system's end is duplicated for each subsurface.
        let (ours, theirs) = Channel::create_shared().ok()?;
        std::thread::Builder::new().name("page frames".into()).spawn(move || hear(ours)).ok()?;
        *link = Some(Link { display, events: theirs });
    }
    link.as_ref().map(call)
}

/// Hears the subsurfaces' frames shown and buffers given back, as long as
/// the process runs (the window system's end is kept: the channel never
/// closes).
fn hear(events: Channel) {
    loop {
        let Ok(observed) = events.wait(signals::READABLE | signals::PEER_CLOSED, abi::DEADLINE_INFINITE) else {
            return;
        };
        while let Ok(msg) = events.read() {
            match ipc::decode_event::<SubsurfaceEvent>(msg) {
                Ok((_, SubsurfaceEvent::FrameDone { subsurface, .. })) => {
                    // SAFETY: plain values; the engine takes them from any
                    // thread.
                    unsafe { engine_subsurface_shown(subsurface) }
                }
                Ok((_, SubsurfaceEvent::Released { subsurface, buffer })) => {
                    // SAFETY: as above.
                    unsafe { engine_subsurface_released(subsurface, buffer) }
                }
                Err(_) => {}
            }
        }
        if observed & signals::READABLE == 0 && observed & signals::PEER_CLOSED != 0 {
            return;
        }
    }
}

/// What a call to the window system said, for the log: `None` if it did
/// what was asked.
fn failed<T>(what: &str, result: Option<Result<Result<T, dp::DisplayError>, ipc::IpcError>>) -> Option<T> {
    match result {
        Some(Ok(Ok(v))) => Some(v),
        Some(Ok(Err(e))) => {
            now_and_then(&FAILURES, || format!("a page's subsurface: the window system refused to {what}: {e:?}"));
            None
        }
        Some(Err(e)) => {
            now_and_then(&FAILURES, || format!("a page's subsurface: cannot {what}: {e:?}"));
            None
        }
        None => {
            now_and_then(&FAILURES, || format!("a page's subsurface: cannot {what}: no window system"));
            None
        }
    }
}

/// A subsurface of window `window` for a page: its id, or 0 if none can
/// be made (none is, quietly, for a window that is no more).
#[unsafe(no_mangle)]
pub extern "C" fn browser_subsurface_create(window: u32) -> u32 {
    let made = with_link(|link| {
        let events = link.events.0.duplicate(None).map(Channel::from_handle);
        events.map(|events| link.display.create_subsurface(window, events))
    });
    match made {
        Some(Ok(Ok(Err(dp::DisplayError::NoSuchWindow)))) => 0,
        Some(Err(e)) => {
            now_and_then(&FAILURES, || format!("a page's subsurface: cannot share its events' channel: {e:?}"));
            0
        }
        Some(Ok(result)) => failed("make one", Some(result)).unwrap_or(0),
        None => failed::<u32>("make one", None).unwrap_or(0),
    }
}

/// What a call that lets go of something said, a subsurface that is no
/// more counting as done: it went with the window, as the browser closed.
fn gone(
    result: Option<Result<Result<(), dp::DisplayError>, ipc::IpcError>>,
) -> Option<Result<Result<(), dp::DisplayError>, ipc::IpcError>> {
    match result {
        Some(Ok(Err(dp::DisplayError::NoSuchSubsurface))) => Some(Ok(Ok(()))),
        other => other,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn browser_subsurface_destroy(subsurface: u32) {
    failed("destroy it", gone(with_link(|link| link.display.destroy_subsurface(subsurface))));
}

/// Attaches the memory file on `fd`, `height` rows of `stride` bytes,
/// `width` pixels each, to `subsurface` as buffer `name`: 0, ENOMEM if the
/// window system has no room for it, EINVAL if it is refused.
#[unsafe(no_mangle)]
pub extern "C" fn browser_subsurface_attach(
    subsurface: u32,
    name: u32,
    fd: c_int,
    width: c_int,
    height: c_int,
    stride: c_int,
) -> c_int {
    if width <= 0 || height <= 0 || stride < width.saturating_mul(4) || stride % 4 != 0 {
        return EINVAL;
    }
    let mut raw = abi::INVALID_HANDLE;
    // SAFETY: a handle to write.
    if unsafe { veda_memory_file(fd, &mut raw) } != 0 {
        return EINVAL;
    }
    // SAFETY: the handle veda_memory_file gave is ours alone.
    let memory = Vmo::from_handle(unsafe { Handle::from_raw(raw) });
    let (w, h, stride) = (width as u32, height as u32, (stride / 4) as u32);
    match with_link(|link| link.display.attach_subsurface_buffer(subsurface, name, memory, w, h, stride)) {
        Some(Ok(Ok(()))) => 0,
        Some(Ok(Err(dp::DisplayError::NoMemory))) => ENOMEM,
        other => {
            failed("attach a buffer", other);
            EINVAL
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn browser_subsurface_detach(subsurface: u32, name: u32) {
    failed("detach a buffer", gone(with_link(|link| link.display.detach_subsurface_buffer(subsurface, name))));
}

/// Presents buffer `name` of `subsurface`, the `n` rectangles at `damage`
/// (x, y, width, height each; none: all of it) having changed: the buffer
/// shown before (0: none, or not yet: `engine_subsurface_released` gives it
/// back once the window system's GPU has done with it), -1 if it could not
/// be presented, or -2 if the subsurface is no more (it went with the
/// window, as the browser closes).
///
/// # Safety
/// `damage` holds `4 * n` numbers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn browser_subsurface_present(subsurface: u32, name: u32, damage: *const c_int, n: u32) -> i64 {
    let rects = if n == 0 || damage.is_null() {
        &[][..]
    } else {
        // SAFETY: as the caller promises.
        unsafe { std::slice::from_raw_parts(damage, 4 * n as usize) }
    };
    let rects: Vec<dp::Rect> = rects
        .as_chunks::<4>()
        .0
        .iter()
        .map(|r| dp::Rect::new(r[0], r[1], r[2].max(0) as u32, r[3].max(0) as u32))
        .collect();
    match with_link(|link| link.display.present_subsurface(subsurface, name, rects)) {
        Some(Ok(Err(dp::DisplayError::NoSuchSubsurface))) => -2,
        result => match failed("present a frame", result) {
            Some(released) => i64::from(released.unwrap_or(0)),
            None => -1,
        },
    }
}
