// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The web engine (`engine/engine.h`) as the browser's thread uses it:
//! WPE WebKit on a thread of its own, told what to do, telling what
//! happened ([`Event`]), and drawing each view's page into a frame the
//! browser shows ([`Frame`]).
//!
//! The engine wakes the browser with a kernel event ([`Engine::wake_handle`]),
//! which its window's loop waits on with its own events; the browser then
//! takes every event that came ([`Engine::events`]).
//!
//! WebKit's processes are this program started again
//! ([`auxiliary_process`]): `main` asks first whether it is one.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::marker::PhantomData;
use std::os::unix::ffi::OsStrExt;
use std::ptr;

use abi::RawHandle;

/// A view: one page, as a tab shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ViewId(u32);

/// A request of a page's, which waits for the browser's answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Request(u64);

/// OpenGL ES and EGL for the engine: its libepoxy finds every function
/// through `eglGetProcAddress`, which it refers to only weakly, which takes
/// nothing into the program; this reference takes the `egl` library in.
#[used]
static EGL_GET_PROC_ADDRESS: unsafe extern "C" fn(*const c_char) -> *const c_void = egl::procs::eglGetProcAddress;

mod ffi {
    use super::*;

    #[repr(C)]
    pub struct Config {
        pub data_dir: *const c_char,
        pub cache_dir: *const c_char,
        pub downloads_dir: *const c_char,
        pub temporary_dir: *const c_char,
        pub application: *const c_char,
        pub block_third_party_cookies: c_int,
        pub accelerated: c_int,
        pub screen_width: c_int,
        pub screen_height: c_int,
        pub refresh_rate: c_int,
        pub scale: f64,
        pub wake: Option<unsafe extern "C" fn(*mut c_void)>,
        pub wake_context: *mut c_void,
    }

    #[repr(C)]
    pub struct Event {
        pub kind: u32,
        pub view: u32,
        pub request: u64,
        pub number: [i64; 4],
        pub value: f64,
        pub text: *mut c_char,
        pub text2: *mut c_char,
        pub text3: *mut c_char,
        pub data: *mut u8,
        pub data_len: usize,
    }

    #[repr(C)]
    pub struct Frame {
        pub pixels: *const u32,
        pub width: c_int,
        pub height: c_int,
        pub stride: c_int,
        pub damage: [c_int; 4],
        pub serial: u64,
    }

    unsafe extern "C" {
        pub fn engine_auxiliary_main(argc: c_int, argv: *mut *mut c_char) -> c_int;
        pub fn engine_start(config: *const Config) -> c_int;
        pub fn engine_stop();
        pub fn engine_view_create(ephemeral: c_int, related: u32) -> u32;
        pub fn engine_view_destroy(view: u32);
        pub fn engine_view_load(view: u32, uri: *const c_char);
        pub fn engine_view_reload(view: u32, bypass_cache: c_int);
        pub fn engine_view_stop(view: u32);
        pub fn engine_view_go(view: u32, steps: c_int);
        pub fn engine_view_resize(view: u32, width: c_int, height: c_int, scale: f64);
        pub fn engine_view_show(view: u32, shown: c_int);
        pub fn engine_view_focus(view: u32, active: c_int, focused: c_int);
        pub fn engine_view_zoom(view: u32, level: f64);
        pub fn engine_view_fullscreen(view: u32, fullscreen: c_int);
        pub fn engine_view_pointer(view: u32, kind: u32, x: f64, y: f64, button: u32, clicks: u32, modifiers: u32);
        pub fn engine_view_scroll(view: u32, x: f64, y: f64, dx: f64, dy: f64, modifiers: u32);
        pub fn engine_view_key(view: u32, pressed: c_int, keycode: u32, keysym: u32, modifiers: u32);
        pub fn engine_view_find(view: u32, text: *const c_char, flags: u32);
        pub fn engine_view_find_next(view: u32, backwards: c_int);
        pub fn engine_view_run_script(view: u32, request: u64, script: *const c_char);
        pub fn engine_clipboard(text: *const c_char);
        pub fn engine_clear_data(what: u32, seconds: i64);
        pub fn engine_dialog_reply(request: u64, confirmed: c_int, text: *const c_char);
        pub fn engine_permission_reply(request: u64, allow: c_int);
        pub fn engine_auth_reply(request: u64, user: *const c_char, password: *const c_char);
        pub fn engine_files_reply(request: u64, paths: *const *const c_char, count: usize);
        pub fn engine_menu_reply(request: u64, index: c_int);
        pub fn engine_tls_reply(request: u64, proceed: c_int);
        pub fn engine_download_reply(request: u64, path: *const c_char);
        pub fn engine_download_cancel(request: u64);
        pub fn engine_next_event(event: *mut Event) -> c_int;
        pub fn engine_event_clear(event: *mut Event);
        pub fn engine_frame_lock(view: u32, frame: *mut Frame) -> c_int;
        pub fn engine_frame_unlock(view: u32, presented: u64);
    }
}

/// What this process is to be, if it is one of WebKit's (its web content
/// or network process): it runs it, and returns its exit code. `None` for
/// the browser itself.
pub fn auxiliary_process() -> Option<i32> {
    let args: Vec<CString> = std::env::args_os().filter_map(|a| CString::new(a.as_bytes()).ok()).collect();
    let mut argv: Vec<*mut c_char> = args.iter().map(|a| a.as_ptr() as *mut c_char).collect();
    argv.push(ptr::null_mut());
    // SAFETY: argv is the program's arguments, NUL-terminated strings that
    // live through the call, and a null pointer after them.
    let code = unsafe { ffi::engine_auxiliary_main(args.len() as c_int, argv.as_mut_ptr()) };
    (code >= 0).then_some(code)
}

/// What the engine starts with.
pub struct Config<'a> {
    pub data_dir: &'a str,
    pub cache_dir: &'a str,
    /// Where downloads go, and the engine's temporary files: the network
    /// process, sandboxed, reaches these and the two above, and no other
    /// of the user's files.
    pub downloads_dir: &'a str,
    pub temporary_dir: &'a str,
    /// What the user agent adds of the browser's ("Name/version").
    pub application: &'a str,
    pub block_third_party_cookies: bool,
    /// Pages composited with OpenGL ES (see `engine.h`).
    pub accelerated: bool,
    pub screen: (i32, i32),
    pub refresh_rate: i32,
    pub scale: f64,
}

/// A C string of `s`, its NULs dropped (none is in what a page could use).
fn c(s: &str) -> CString {
    CString::new(s.replace('\0', "")).unwrap_or_default()
}

/// The engine, started; stopped as it is dropped.
pub struct Engine {
    /// What wakes the browser, which the engine signals (its thread holds
    /// a pointer to it, so it never moves).
    wake: Box<rt::object::Event>,
}

unsafe extern "C" fn wake(context: *mut c_void) {
    // SAFETY: the context is the engine's wake event, which lives as long
    // as the engine runs (Engine owns it and stops the engine first).
    let event = unsafe { &*(context as *const rt::object::Event) };
    let _ = event.signal();
}

impl Engine {
    pub fn start(config: &Config) -> Result<Engine, String> {
        let event = rt::object::Event::create().map_err(|e| format!("an event to wake the browser: {e:?}"))?;
        let wake_event = Box::new(event);
        let (data, cache, application) = (c(config.data_dir), c(config.cache_dir), c(config.application));
        let (downloads, temporary) = (c(config.downloads_dir), c(config.temporary_dir));
        let raw = ffi::Config {
            data_dir: data.as_ptr(),
            cache_dir: cache.as_ptr(),
            downloads_dir: downloads.as_ptr(),
            temporary_dir: temporary.as_ptr(),
            application: application.as_ptr(),
            block_third_party_cookies: c_int::from(config.block_third_party_cookies),
            accelerated: c_int::from(config.accelerated),
            screen_width: config.screen.0,
            screen_height: config.screen.1,
            refresh_rate: config.refresh_rate,
            scale: config.scale,
            wake: Some(wake),
            wake_context: &*wake_event as *const rt::object::Event as *mut c_void,
        };
        // SAFETY: the configuration's strings live through the call (the
        // engine copies them), and its wake context as long as the engine.
        let error = unsafe { ffi::engine_start(&raw) };
        if error != 0 {
            return Err(format!("the web engine did not start (error {error})"));
        }
        Ok(Engine { wake: wake_event })
    }

    /// What the browser waits on for the engine's events.
    pub fn wake_handle(&self) -> RawHandle {
        self.wake.raw()
    }

    /// Every event that came.
    pub fn events(&self) -> Vec<Event> {
        // Cleared first: an event that comes from now on signals it again.
        let _ = self.wake.clear();
        let mut out = Vec::new();
        loop {
            let mut raw = ffi::Event {
                kind: 0,
                view: 0,
                request: 0,
                number: [0; 4],
                value: 0.0,
                text: ptr::null_mut(),
                text2: ptr::null_mut(),
                text3: ptr::null_mut(),
                data: ptr::null_mut(),
                data_len: 0,
            };
            // SAFETY: an event to fill, which is cleared below.
            if unsafe { ffi::engine_next_event(&mut raw) } == 0 {
                return out;
            }
            out.extend(Event::of(&raw));
            // SAFETY: the event engine_next_event filled.
            unsafe { ffi::engine_event_clear(&mut raw) };
        }
    }

    pub fn create_view(&self, ephemeral: bool, related: Option<ViewId>) -> Option<ViewId> {
        // SAFETY: plain values.
        let id = unsafe { ffi::engine_view_create(c_int::from(ephemeral), related.map_or(0, |v| v.0)) };
        (id != 0).then_some(ViewId(id))
    }

    /// Ends `view`, which is never used again.
    pub fn destroy_view(&self, view: ViewId) {
        // SAFETY: plain values.
        unsafe { ffi::engine_view_destroy(view.0) }
    }

    pub fn load(&self, view: ViewId, uri: &str) {
        let uri = c(uri);
        // SAFETY: a string that lives through the call.
        unsafe { ffi::engine_view_load(view.0, uri.as_ptr()) }
    }

    pub fn reload(&self, view: ViewId, bypass_cache: bool) {
        // SAFETY: plain values.
        unsafe { ffi::engine_view_reload(view.0, c_int::from(bypass_cache)) }
    }

    pub fn stop(&self, view: ViewId) {
        // SAFETY: plain values.
        unsafe { ffi::engine_view_stop(view.0) }
    }

    pub fn go(&self, view: ViewId, steps: i32) {
        // SAFETY: plain values.
        unsafe { ffi::engine_view_go(view.0, steps) }
    }

    pub fn resize(&self, view: ViewId, width: i32, height: i32, scale: f64) {
        // SAFETY: plain values.
        unsafe { ffi::engine_view_resize(view.0, width, height, scale) }
    }

    pub fn show(&self, view: ViewId, shown: bool) {
        // SAFETY: plain values.
        unsafe { ffi::engine_view_show(view.0, c_int::from(shown)) }
    }

    pub fn focus(&self, view: ViewId, active: bool, focused: bool) {
        // SAFETY: plain values.
        unsafe { ffi::engine_view_focus(view.0, c_int::from(active), c_int::from(focused)) }
    }

    pub fn zoom(&self, view: ViewId, level: f64) {
        // SAFETY: plain values.
        unsafe { ffi::engine_view_zoom(view.0, level) }
    }

    pub fn fullscreen(&self, view: ViewId, fullscreen: bool) {
        // SAFETY: plain values.
        unsafe { ffi::engine_view_fullscreen(view.0, c_int::from(fullscreen)) }
    }

    pub fn pointer(&self, view: ViewId, kind: Pointer, x: f64, y: f64, modifiers: u32) {
        let (kind, button, clicks) = match kind {
            Pointer::Move => (1, 0, 0),
            Pointer::Down { button, clicks } => (2, button, clicks),
            Pointer::Up { button } => (3, button, 0),
            Pointer::Leave => (4, 0, 0),
        };
        // SAFETY: plain values.
        unsafe { ffi::engine_view_pointer(view.0, kind, x, y, button, clicks, modifiers) }
    }

    pub fn scroll(&self, view: ViewId, x: f64, y: f64, dx: f64, dy: f64, modifiers: u32) {
        // SAFETY: plain values.
        unsafe { ffi::engine_view_scroll(view.0, x, y, dx, dy, modifiers) }
    }

    pub fn key(&self, view: ViewId, pressed: bool, keycode: u32, keysym: u32, modifiers: u32) {
        // SAFETY: plain values.
        unsafe { ffi::engine_view_key(view.0, c_int::from(pressed), keycode, keysym, modifiers) }
    }

    /// Finds `text` in the page (`None`: stops finding).
    pub fn find(&self, view: ViewId, text: Option<&str>, case_sensitive: bool) {
        let text = text.map(c);
        let flags = u32::from(case_sensitive);
        // SAFETY: a string (or null) that lives through the call.
        unsafe { ffi::engine_view_find(view.0, text.as_ref().map_or(ptr::null(), |t| t.as_ptr()), flags) }
    }

    pub fn find_next(&self, view: ViewId, backwards: bool) {
        // SAFETY: plain values.
        unsafe { ffi::engine_view_find_next(view.0, c_int::from(backwards)) }
    }

    /// Runs `script` in the page; an [`Event::ScriptResult`] of `request`
    /// answers.
    pub fn run_script(&self, view: ViewId, request: u64, script: &str) {
        let script = c(script);
        // SAFETY: a string that lives through the call.
        unsafe { ffi::engine_view_run_script(view.0, request, script.as_ptr()) }
    }

    /// The system's clipboard text, for the next paste in a page.
    pub fn clipboard(&self, text: &str) {
        let text = c(text);
        // SAFETY: a string that lives through the call.
        unsafe { ffi::engine_clipboard(text.as_ptr()) }
    }

    /// Clears what the session keeps of `what` ([`data`]) from the last
    /// `seconds` (0: all).
    pub fn clear_data(&self, what: u32, seconds: i64) {
        // SAFETY: plain values.
        unsafe { ffi::engine_clear_data(what, seconds) }
    }

    pub fn dialog_reply(&self, request: Request, confirmed: bool, text: Option<&str>) {
        let text = text.map(c);
        // SAFETY: a string (or null) that lives through the call.
        unsafe {
            ffi::engine_dialog_reply(
                request.0,
                c_int::from(confirmed),
                text.as_ref().map_or(ptr::null(), |t| t.as_ptr()),
            )
        }
    }

    pub fn permission_reply(&self, request: Request, allow: bool) {
        // SAFETY: plain values.
        unsafe { ffi::engine_permission_reply(request.0, c_int::from(allow)) }
    }

    /// Credentials, or `None` to give up.
    pub fn auth_reply(&self, request: Request, credentials: Option<(&str, &str)>) {
        let credentials = credentials.map(|(u, p)| (c(u), c(p)));
        let (user, password) =
            credentials.as_ref().map_or((ptr::null(), ptr::null()), |(u, p)| (u.as_ptr(), p.as_ptr()));
        // SAFETY: strings (or nulls) that live through the call.
        unsafe { ffi::engine_auth_reply(request.0, user, password) }
    }

    pub fn files_reply(&self, request: Request, paths: &[String]) {
        let paths: Vec<CString> = paths.iter().map(|p| c(p)).collect();
        let pointers: Vec<*const c_char> = paths.iter().map(|p| p.as_ptr()).collect();
        // SAFETY: `pointers.len()` strings that live through the call.
        unsafe { ffi::engine_files_reply(request.0, pointers.as_ptr(), pointers.len()) }
    }

    /// The item chosen of a menu, or `None`.
    pub fn menu_reply(&self, request: Request, index: Option<usize>) {
        let index = index.and_then(|i| c_int::try_from(i).ok()).unwrap_or(-1);
        // SAFETY: plain values.
        unsafe { ffi::engine_menu_reply(request.0, index) }
    }

    pub fn tls_reply(&self, request: Request, proceed: bool) {
        // SAFETY: plain values.
        unsafe { ffi::engine_tls_reply(request.0, c_int::from(proceed)) }
    }

    /// Where a download goes, or `None`: it is cancelled.
    pub fn download_reply(&self, request: Request, path: Option<&str>) {
        let path = path.map(c);
        // SAFETY: a string (or null) that lives through the call.
        unsafe { ffi::engine_download_reply(request.0, path.as_ref().map_or(ptr::null(), |p| p.as_ptr())) }
    }

    pub fn download_cancel(&self, request: Request) {
        // SAFETY: plain values.
        unsafe { ffi::engine_download_cancel(request.0) }
    }

    /// `view`'s frame, which WebKit leaves as it is while it is held.
    pub fn frame(&self, view: ViewId) -> Option<Frame<'_>> {
        let mut raw = ffi::Frame { pixels: ptr::null(), width: 0, height: 0, stride: 0, damage: [0; 4], serial: 0 };
        // SAFETY: a frame to fill; on success the lock is the Frame's.
        if unsafe { ffi::engine_frame_lock(view.0, &mut raw) } == 0 {
            return None;
        }
        Some(Frame { raw, view, presented: 0, _engine: PhantomData })
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        // SAFETY: the engine started, and stops before its wake event goes.
        unsafe { ffi::engine_stop() }
    }
}

/// What the pointer did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pointer {
    Move,
    /// A button (1 primary, 2 middle, 3 secondary) went down, the
    /// `clicks`th in a row.
    Down {
        button: u32,
        clicks: u32,
    },
    Up {
        button: u32,
    },
    Leave,
}

/// What the session keeps, to clear ([`Engine::clear_data`]).
pub mod data {
    pub const COOKIES: u32 = 1;
    pub const CACHE: u32 = 2;
    pub const STORAGE: u32 = 4;
}

/// A view's page, as WebKit drew it last; it stays put while held.
pub struct Frame<'a> {
    raw: ffi::Frame,
    view: ViewId,
    presented: u64,
    _engine: PhantomData<&'a Engine>,
}

impl Frame<'_> {
    pub fn width(&self) -> i32 {
        self.raw.width
    }

    pub fn height(&self) -> i32 {
        self.raw.height
    }

    /// Its pixels, premultiplied 0xAARRGGBB, [`Frame::width`] a row.
    pub fn pixels(&self) -> &[u32] {
        let len = self.raw.width.max(0) as usize * self.raw.height.max(0) as usize;
        // SAFETY: the engine holds `stride * height` pixels at `pixels`
        // (stride is the width), which stay put while the frame is locked.
        unsafe { std::slice::from_raw_parts(self.raw.pixels, len) }
    }

    /// It reaches the screen: WebKit may draw the next.
    pub fn presented(&mut self) {
        self.presented = self.raw.serial;
    }
}

impl Drop for Frame<'_> {
    fn drop(&mut self) {
        // SAFETY: the frame was locked by engine_frame_lock.
        unsafe { ffi::engine_frame_unlock(self.view.0, self.presented) }
    }
}

/// How a load goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Load {
    Started,
    Redirected,
    Committed,
    Finished,
}

/// Why a load failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    Network,
    /// Stopped: nothing to tell.
    Cancelled,
    /// No such host.
    Host,
    /// It was downloaded, not shown.
    Content,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Security {
    /// The browser's own pages, files, data.
    #[default]
    Local,
    Insecure,
    Secure,
    /// A certificate not trusted, but allowed.
    Broken,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Crash {
    Crashed,
    Memory,
    Terminated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialog {
    Alert,
    Confirm,
    Prompt,
    /// Leaving a page that asks whether to.
    Leave,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    Geolocation,
    Notifications,
    Storage,
    DeviceInfo,
    Media,
    Other,
}

/// An item of a page's menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuItem {
    pub label: String,
    pub enabled: bool,
    pub selected: bool,
    pub separator: bool,
    pub group_label: bool,
    pub group_child: bool,
}

/// How a download ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadEnd {
    Done,
    Failed(String),
    Cancelled,
}

/// What a certificate's problem is (several at once may be).
pub mod tls {
    pub const UNKNOWN_CA: u32 = 1 << 0;
    pub const BAD_IDENTITY: u32 = 1 << 1;
    pub const NOT_ACTIVATED: u32 = 1 << 2;
    pub const EXPIRED: u32 = 1 << 3;
    pub const REVOKED: u32 = 1 << 4;
    pub const INSECURE: u32 = 1 << 5;
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Frame(ViewId),
    Title(ViewId, String),
    Uri(ViewId, String),
    Load(ViewId, Load),
    Progress(ViewId, f64),
    History {
        view: ViewId,
        back: bool,
        forward: bool,
    },
    LoadFailed {
        view: ViewId,
        uri: String,
        message: String,
        failure: Failure,
    },
    TlsError {
        view: ViewId,
        request: Request,
        uri: String,
        host: String,
        subject: String,
        errors: u32,
    },
    Security(ViewId, Security),
    Cursor(ViewId, String),
    /// The page's icon, `width` x `height` premultiplied pixels (none: no
    /// icon).
    Icon {
        view: ViewId,
        width: i32,
        height: i32,
        pixels: Vec<u32>,
    },
    Hover(ViewId, Option<String>),
    NewView {
        view: ViewId,
        opener: ViewId,
    },
    Close(ViewId),
    Crashed(ViewId, Crash),
    Fullscreen(ViewId, bool),
    Dialog {
        view: ViewId,
        request: Request,
        kind: Dialog,
        message: String,
        default: String,
    },
    Permission {
        view: ViewId,
        request: Request,
        kind: Permission,
        site: String,
    },
    Auth {
        view: ViewId,
        request: Request,
        host: String,
        realm: String,
        proxy: bool,
    },
    FileChooser {
        view: ViewId,
        request: Request,
        multiple: bool,
        types: Vec<String>,
    },
    /// A `<select>`'s options, where it is in the page.
    OptionMenu {
        view: ViewId,
        request: Request,
        items: Vec<MenuItem>,
        at: (i32, i32, i32, i32),
    },
    ContextMenu {
        view: ViewId,
        request: Request,
        items: Vec<MenuItem>,
        link: Option<String>,
        image: Option<String>,
    },
    MenuClosed(Request),
    Download {
        request: Request,
        uri: String,
        suggested: String,
        size: Option<u64>,
    },
    DownloadProgress {
        request: Request,
        received: u64,
        fraction: f64,
    },
    /// It ended; `path` where it went, once done.
    DownloadDone {
        request: Request,
        end: DownloadEnd,
        path: Option<String>,
    },
    Found(ViewId, u32),
    ScriptResult {
        view: ViewId,
        request: u64,
        result: Result<String, String>,
    },
    Clipboard(String),
    InsecureContent(ViewId),
}

/// The text of a C string, if there is one.
fn text(p: *const c_char) -> Option<String> {
    // SAFETY: the engine's strings are NUL-terminated or null.
    (!p.is_null()).then(|| unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
}

fn items(labels: Option<String>, flags: &[u8]) -> Vec<MenuItem> {
    let labels = labels.unwrap_or_default();
    let mut labels = labels.split('\n');
    flags
        .iter()
        .map(|&f| MenuItem {
            label: labels.next().unwrap_or("").to_string(),
            enabled: f & 1 != 0,
            selected: f & 2 != 0,
            separator: f & 4 != 0,
            group_label: f & 8 != 0,
            group_child: f & 16 != 0,
        })
        .collect()
}

impl Event {
    fn of(e: &ffi::Event) -> Option<Event> {
        let view = ViewId(e.view);
        let request = Request(e.request);
        let n = e.number;
        let data: &[u8] = if e.data.is_null() {
            &[]
        } else {
            // SAFETY: the engine gives `data_len` bytes at `data`.
            unsafe { std::slice::from_raw_parts(e.data, e.data_len) }
        };
        let s = || text(e.text).unwrap_or_default();
        let s2 = || text(e.text2).unwrap_or_default();
        Some(match e.kind {
            1 => Event::Frame(view),
            2 => Event::Title(view, s()),
            3 => Event::Uri(view, s()),
            4 => Event::Load(
                view,
                match n[0] {
                    1 => Load::Started,
                    2 => Load::Redirected,
                    3 => Load::Committed,
                    _ => Load::Finished,
                },
            ),
            5 => Event::Progress(view, e.value.clamp(0.0, 1.0)),
            6 => Event::History { view, back: n[0] != 0, forward: n[1] != 0 },
            7 => Event::LoadFailed {
                view,
                uri: s(),
                message: s2(),
                failure: match n[0] {
                    1 => Failure::Network,
                    2 => Failure::Cancelled,
                    3 => Failure::Host,
                    4 => Failure::Content,
                    _ => Failure::Other,
                },
            },
            8 => Event::TlsError {
                view,
                request,
                uri: s(),
                host: s2(),
                subject: text(e.text3).unwrap_or_default(),
                errors: n[0] as u32,
            },
            9 => Event::Security(
                view,
                match n[0] {
                    2 => Security::Insecure,
                    3 => Security::Secure,
                    4 => Security::Broken,
                    _ => Security::Local,
                },
            ),
            10 => Event::Cursor(view, s()),
            11 => {
                let (w, h) = (n[0] as i32, n[1] as i32);
                let len = w.max(0) as usize * h.max(0) as usize;
                let pixels = if data.len() >= len * 4 && len > 0 {
                    data.as_chunks::<4>().0.iter().take(len).map(|p| u32::from_ne_bytes(*p)).collect()
                } else {
                    Vec::new()
                };
                let (width, height) = if pixels.is_empty() { (0, 0) } else { (w, h) };
                Event::Icon { view, width, height, pixels }
            }
            12 => Event::Hover(view, text(e.text)),
            13 => Event::NewView { view, opener: ViewId(n[0] as u32) },
            14 => Event::Close(view),
            15 => Event::Crashed(
                view,
                match n[0] {
                    2 => Crash::Memory,
                    3 => Crash::Terminated,
                    _ => Crash::Crashed,
                },
            ),
            16 => Event::Fullscreen(view, n[0] != 0),
            17 => Event::Dialog {
                view,
                request,
                kind: match n[0] {
                    2 => Dialog::Confirm,
                    3 => Dialog::Prompt,
                    4 => Dialog::Leave,
                    _ => Dialog::Alert,
                },
                message: s(),
                default: s2(),
            },
            18 => Event::Permission {
                view,
                request,
                kind: match n[0] {
                    1 => Permission::Geolocation,
                    2 => Permission::Notifications,
                    3 => Permission::Storage,
                    4 => Permission::DeviceInfo,
                    5 => Permission::Media,
                    _ => Permission::Other,
                },
                site: s(),
            },
            19 => Event::Auth { view, request, host: s(), realm: s2(), proxy: n[0] != 0 },
            20 => Event::FileChooser {
                view,
                request,
                multiple: n[0] != 0,
                types: text(e.text).map_or_else(Vec::new, |t| t.split('\n').map(str::to_string).collect()),
            },
            21 => Event::OptionMenu {
                view,
                request,
                items: items(text(e.text), data),
                at: (n[0] as i32, n[1] as i32, n[2] as i32, n[3] as i32),
            },
            22 => Event::ContextMenu {
                view,
                request,
                items: items(text(e.text), data),
                link: text(e.text2),
                image: text(e.text3),
            },
            23 => Event::MenuClosed(request),
            24 => Event::Download {
                request,
                uri: s(),
                suggested: s2(),
                size: u64::try_from(n[0]).ok().filter(|&s| s > 0),
            },
            25 => Event::DownloadProgress { request, received: n[0].max(0) as u64, fraction: e.value.clamp(0.0, 1.0) },
            26 => {
                let end = match n[0] {
                    0 => DownloadEnd::Done,
                    2 => DownloadEnd::Cancelled,
                    _ => DownloadEnd::Failed(s()),
                };
                let path = if end == DownloadEnd::Done { text(e.text) } else { None };
                Event::DownloadDone { request, end, path }
            }
            27 => Event::Found(view, n[0].clamp(0, i64::from(u32::MAX)) as u32),
            28 => Event::ScriptResult {
                view,
                request: e.request,
                result: match text(e.text) {
                    Some(json) => Ok(json),
                    None => Err(s2()),
                },
            },
            29 => Event::Clipboard(s()),
            30 => Event::InsecureContent(view),
            _ => return None,
        })
    }
}
