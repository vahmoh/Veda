// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Program arguments, environment and the handles received at startup.
//!
//! A Veda program reads them from the startup message on the bootstrap
//! channel ([`adopt`], which `vrt`'s entry point calls). A program on the
//! standard library (`target_os = "veda"`) does not: the C library's POSIX
//! layer has read it, and keeps the descriptors, the terminal and the
//! registry for itself. Its arguments, environment and working directory
//! are then the standard library's, a handle of an application's role
//! (`role::USER` and above) is the layer's to give, and a registry channel
//! is one of the program's own, which the layer asks `init` for: what goes
//! through it is the program's, as through the layer's.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::object::Handle;
#[cfg(not(target_os = "veda"))]
use vabi::RawHandle;
#[cfg(not(target_os = "veda"))]
use vabi::startup::StartupView;

#[cfg(not(target_os = "veda"))]
use crate::object::Channel;
#[cfg(not(target_os = "veda"))]
use crate::sync::Mutex;

#[cfg(not(target_os = "veda"))]
#[derive(Default)]
pub(crate) struct Startup {
    // A guest's arguments are Linux's (`std::env::args`).
    #[cfg_attr(veda_guest, allow(dead_code))]
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: String,
    pub handles: Vec<(u32, Handle)>,
}

#[cfg(not(target_os = "veda"))]
pub(crate) static STARTUP: Mutex<Option<Startup>> = Mutex::new(None);

/// Command-line arguments; `args()[0]` is the program name.
#[cfg(not(any(veda_guest, target_os = "veda")))]
pub fn args() -> Vec<String> {
    STARTUP.lock().as_ref().map(|s| s.args.clone()).unwrap_or_default()
}

/// Command-line arguments; `args()[0]` is the program name.
#[cfg(any(veda_guest, target_os = "veda"))]
pub fn args() -> Vec<String> {
    std::env::args_os().map(|a| a.to_string_lossy().into_owned()).collect()
}

/// The working directory the program was started in (absolute).
#[cfg(not(target_os = "veda"))]
pub fn cwd() -> String {
    STARTUP.lock().as_ref().map(|s| s.cwd.clone()).filter(|c| !c.is_empty()).unwrap_or_else(|| "/".to_string())
}

/// The program's working directory (absolute).
#[cfg(target_os = "veda")]
pub fn cwd() -> String {
    std::env::current_dir()
        .ok()
        .and_then(|d| d.into_os_string().into_string().ok())
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| "/".to_string())
}

/// Looks up an environment variable.
#[cfg(not(target_os = "veda"))]
pub fn var(key: &str) -> Option<String> {
    STARTUP.lock().as_ref()?.env.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
}

/// Looks up an environment variable.
#[cfg(target_os = "veda")]
pub fn var(key: &str) -> Option<String> {
    std::env::var_os(key).map(|v| v.to_string_lossy().into_owned())
}

/// All environment variables.
#[cfg(not(target_os = "veda"))]
pub fn vars() -> Vec<(String, String)> {
    STARTUP.lock().as_ref().map(|s| s.env.clone()).unwrap_or_default()
}

/// All environment variables.
#[cfg(target_os = "veda")]
pub fn vars() -> Vec<(String, String)> {
    std::env::vars_os().map(|(k, v)| (k.to_string_lossy().into_owned(), v.to_string_lossy().into_owned())).collect()
}

/// Takes the first startup handle with the given role
/// (see `vabi::startup::role`). In a guest of the driver VM, the guest's
/// own (a new one each time).
#[cfg(veda_guest)]
pub fn take_handle(role: u32) -> Option<Handle> {
    // SAFETY: the bridge just gave us this handle.
    crate::guest::bootstrap(role).map(|h| unsafe { Handle::from_raw(h) })
}

/// Takes the first startup handle with the given role
/// (see `vabi::startup::role`).
#[cfg(not(any(veda_guest, target_os = "veda")))]
pub fn take_handle(role: u32) -> Option<Handle> {
    let mut guard = STARTUP.lock();
    let s = guard.as_mut()?;
    let i = s.handles.iter().position(|(r, _)| *r == role)?;
    Some(s.handles.remove(i).1)
}

/// Takes the handle the program started with in `role` (`role::USER` and
/// above), each once, from the POSIX layer; for `role::REGISTRY`, a new
/// registry channel of the program's own (see the module's documentation).
#[cfg(target_os = "veda")]
pub fn take_handle(role: u32) -> Option<Handle> {
    use vabi::startup::role;
    let mut raw = vabi::INVALID_HANDLE;
    // SAFETY: the layer writes a handle to `raw` when it succeeds.
    let status = unsafe {
        if role == role::REGISTRY { layer::veda_registry(&mut raw) } else { layer::veda_startup_handle(role, &mut raw) }
    };
    // SAFETY: the layer gave us this handle.
    (status == 0).then(|| unsafe { Handle::from_raw(raw) })
}

/// The POSIX layer's calls, in the C library (`<veda/ipc.h>`).
#[cfg(target_os = "veda")]
mod layer {
    use core::ffi::c_int;

    use vabi::RawHandle;

    unsafe extern "C" {
        /// Takes the handle the program started with in `role`.
        pub fn veda_startup_handle(role: u32, out: *mut RawHandle) -> c_int;
        /// A registry channel of the program's own.
        pub fn veda_registry(out: *mut RawHandle) -> c_int;
    }
}

/// Roles of the handles that have not been taken yet.
#[cfg(not(target_os = "veda"))]
pub fn handle_roles() -> Vec<u32> {
    STARTUP.lock().as_ref().map(|s| s.handles.iter().map(|(r, _)| *r).collect()).unwrap_or_default()
}

/// Reads the startup message from the bootstrap channel the kernel passed
/// the process and keeps what it says for [`args`], [`vars`], [`cwd`] and
/// [`take_handle`]. `vrt`'s entry point does this for Rust programs; C
/// programs' POSIX layer, which has an entry point of its own, calls it.
#[cfg(not(target_os = "veda"))]
pub fn adopt(bootstrap: RawHandle) {
    *STARTUP.lock() = Some(read_startup(bootstrap));
}

/// Decodes the startup message on the bootstrap channel.
#[cfg(not(target_os = "veda"))]
fn read_startup(bootstrap: RawHandle) -> Startup {
    if bootstrap == vabi::INVALID_HANDLE {
        return Startup::default();
    }
    // SAFETY: the kernel passes us ownership of the bootstrap channel.
    let ch = Channel(unsafe { Handle::from_raw(bootstrap) });
    let Ok(msg) = ch.read() else { return Startup::default() };
    let Ok(view) = StartupView::parse(&msg.bytes) else { return Startup::default() };
    let args = view.args().map(|s| s.to_string()).collect();
    let env = view
        .env()
        .map(|kv| match kv.split_once('=') {
            Some((k, v)) => (k.to_string(), v.to_string()),
            None => (kv.to_string(), String::new()),
        })
        .collect();
    let handles: Vec<(u32, Handle)> =
        msg.handles.into_iter().enumerate().map(|(i, h)| (view.role(i).unwrap_or(0), h)).collect();
    Startup { args, env, cwd: view.cwd().to_string(), handles }
}
