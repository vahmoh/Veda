// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! EGL 1.5: the display, its configurations, contexts, pbuffers and fences,
//! and each thread's error and API.
//!
//! Handles are numbers: the display's is that of the one [`Display`], the
//! configurations' their index from 1, and contexts', surfaces' and syncs'
//! the numbers they are kept under (none taken twice).

use std::cell::Cell;
use std::collections::HashMap;
use std::ffi::{c_char, c_void};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use crate::current::{self, Context, Group, Surface, lock};
use crate::*;

pub const EGL_FALSE: EGLBoolean = 0;
pub const EGL_TRUE: EGLBoolean = 1;
pub const EGL_DONT_CARE: EGLint = -1;
pub const EGL_UNKNOWN: EGLint = -1;

pub const EGL_SUCCESS: EGLint = 0x3000;
pub const EGL_NOT_INITIALIZED: EGLint = 0x3001;
pub const EGL_BAD_ACCESS: EGLint = 0x3002;
pub const EGL_BAD_ALLOC: EGLint = 0x3003;
pub const EGL_BAD_ATTRIBUTE: EGLint = 0x3004;
pub const EGL_BAD_CONFIG: EGLint = 0x3005;
pub const EGL_BAD_CONTEXT: EGLint = 0x3006;
pub const EGL_BAD_CURRENT_SURFACE: EGLint = 0x3007;
pub const EGL_BAD_DISPLAY: EGLint = 0x3008;
pub const EGL_BAD_MATCH: EGLint = 0x3009;
pub const EGL_BAD_NATIVE_PIXMAP: EGLint = 0x300A;
pub const EGL_BAD_NATIVE_WINDOW: EGLint = 0x300B;
pub const EGL_BAD_PARAMETER: EGLint = 0x300C;
pub const EGL_BAD_SURFACE: EGLint = 0x300D;

pub const EGL_BUFFER_SIZE: EGLint = 0x3020;
pub const EGL_ALPHA_SIZE: EGLint = 0x3021;
pub const EGL_BLUE_SIZE: EGLint = 0x3022;
pub const EGL_GREEN_SIZE: EGLint = 0x3023;
pub const EGL_RED_SIZE: EGLint = 0x3024;
pub const EGL_DEPTH_SIZE: EGLint = 0x3025;
pub const EGL_STENCIL_SIZE: EGLint = 0x3026;
pub const EGL_CONFIG_CAVEAT: EGLint = 0x3027;
pub const EGL_CONFIG_ID: EGLint = 0x3028;
pub const EGL_LEVEL: EGLint = 0x3029;
pub const EGL_MAX_PBUFFER_HEIGHT: EGLint = 0x302A;
pub const EGL_MAX_PBUFFER_PIXELS: EGLint = 0x302B;
pub const EGL_MAX_PBUFFER_WIDTH: EGLint = 0x302C;
pub const EGL_NATIVE_RENDERABLE: EGLint = 0x302D;
pub const EGL_NATIVE_VISUAL_ID: EGLint = 0x302E;
pub const EGL_NATIVE_VISUAL_TYPE: EGLint = 0x302F;
pub const EGL_SAMPLES: EGLint = 0x3031;
pub const EGL_SAMPLE_BUFFERS: EGLint = 0x3032;
pub const EGL_SURFACE_TYPE: EGLint = 0x3033;
pub const EGL_TRANSPARENT_TYPE: EGLint = 0x3034;
pub const EGL_TRANSPARENT_BLUE_VALUE: EGLint = 0x3035;
pub const EGL_TRANSPARENT_GREEN_VALUE: EGLint = 0x3036;
pub const EGL_TRANSPARENT_RED_VALUE: EGLint = 0x3037;
pub const EGL_NONE: EGLint = 0x3038;
pub const EGL_BIND_TO_TEXTURE_RGB: EGLint = 0x3039;
pub const EGL_BIND_TO_TEXTURE_RGBA: EGLint = 0x303A;
pub const EGL_MIN_SWAP_INTERVAL: EGLint = 0x303B;
pub const EGL_MAX_SWAP_INTERVAL: EGLint = 0x303C;
pub const EGL_LUMINANCE_SIZE: EGLint = 0x303D;
pub const EGL_ALPHA_MASK_SIZE: EGLint = 0x303E;
pub const EGL_COLOR_BUFFER_TYPE: EGLint = 0x303F;
pub const EGL_RENDERABLE_TYPE: EGLint = 0x3040;
pub const EGL_MATCH_NATIVE_PIXMAP: EGLint = 0x3041;
pub const EGL_CONFORMANT: EGLint = 0x3042;
pub const EGL_SLOW_CONFIG: EGLint = 0x3050;
pub const EGL_NON_CONFORMANT_CONFIG: EGLint = 0x3051;
pub const EGL_RGB_BUFFER: EGLint = 0x308E;

pub const EGL_PBUFFER_BIT: EGLint = 0x0001;
pub const EGL_PIXMAP_BIT: EGLint = 0x0002;
pub const EGL_WINDOW_BIT: EGLint = 0x0004;
pub const EGL_OPENGL_ES2_BIT: EGLint = 0x0004;
pub const EGL_OPENGL_ES3_BIT: EGLint = 0x0040;

pub const EGL_VENDOR: EGLint = 0x3053;
pub const EGL_VERSION: EGLint = 0x3054;
pub const EGL_EXTENSIONS: EGLint = 0x3055;
pub const EGL_CLIENT_APIS: EGLint = 0x308D;

pub const EGL_HEIGHT: EGLint = 0x3056;
pub const EGL_WIDTH: EGLint = 0x3057;
pub const EGL_LARGEST_PBUFFER: EGLint = 0x3058;
pub const EGL_DRAW: EGLint = 0x3059;
pub const EGL_READ: EGLint = 0x305A;
pub const EGL_NO_TEXTURE: EGLint = 0x305C;
pub const EGL_TEXTURE_FORMAT: EGLint = 0x3080;
pub const EGL_TEXTURE_TARGET: EGLint = 0x3081;
pub const EGL_MIPMAP_TEXTURE: EGLint = 0x3082;
pub const EGL_MIPMAP_LEVEL: EGLint = 0x3083;
pub const EGL_BACK_BUFFER: EGLint = 0x3084;
pub const EGL_RENDER_BUFFER: EGLint = 0x3086;
pub const EGL_VG_COLORSPACE: EGLint = 0x3087;
pub const EGL_VG_ALPHA_FORMAT: EGLint = 0x3088;
pub const EGL_GL_COLORSPACE_LINEAR: EGLint = 0x308A;
pub const EGL_HORIZONTAL_RESOLUTION: EGLint = 0x3090;
pub const EGL_VERTICAL_RESOLUTION: EGLint = 0x3091;
pub const EGL_PIXEL_ASPECT_RATIO: EGLint = 0x3092;
pub const EGL_SWAP_BEHAVIOR: EGLint = 0x3093;
pub const EGL_BUFFER_DESTROYED: EGLint = 0x3095;
pub const EGL_MULTISAMPLE_RESOLVE: EGLint = 0x3099;
pub const EGL_MULTISAMPLE_RESOLVE_DEFAULT: EGLint = 0x309A;
pub const EGL_GL_COLORSPACE: EGLint = 0x309D;

pub const EGL_CONTEXT_CLIENT_TYPE: EGLint = 0x3097;
pub const EGL_CONTEXT_MAJOR_VERSION: EGLint = 0x3098;
pub const EGL_CONTEXT_MINOR_VERSION: EGLint = 0x30FB;
pub const EGL_CONTEXT_FLAGS_KHR: EGLint = 0x30FC;
pub const EGL_CONTEXT_OPENGL_PROFILE_MASK: EGLint = 0x30FD;
pub const EGL_CONTEXT_OPENGL_DEBUG: EGLint = 0x31B0;
pub const EGL_CONTEXT_OPENGL_FORWARD_COMPATIBLE: EGLint = 0x31B1;
pub const EGL_CONTEXT_OPENGL_ROBUST_ACCESS: EGLint = 0x31B2;
pub const EGL_CONTEXT_OPENGL_RESET_NOTIFICATION_STRATEGY: EGLint = 0x31BD;
pub const EGL_NO_RESET_NOTIFICATION: EGLint = 0x31BE;
/// `EGL_CONTEXT_FLAGS_KHR`'s debug bit (and `EGL_CONTEXT_OPENGL_DEBUG`).
pub const EGL_CONTEXT_OPENGL_DEBUG_BIT_KHR: EGLint = 0x0001;

pub const EGL_OPENGL_ES_API: EGLenum = 0x30A0;

pub const EGL_PLATFORM_SURFACELESS_MESA: EGLenum = 0x31DD;

pub const EGL_SYNC_PRIOR_COMMANDS_COMPLETE: EGLint = 0x30F0;
pub const EGL_SYNC_STATUS: EGLint = 0x30F1;
pub const EGL_SIGNALED: EGLint = 0x30F2;
pub const EGL_UNSIGNALED: EGLint = 0x30F3;
pub const EGL_TIMEOUT_EXPIRED: EGLint = 0x30F5;
pub const EGL_CONDITION_SATISFIED: EGLint = 0x30F6;
pub const EGL_SYNC_TYPE: EGLint = 0x30F7;
pub const EGL_SYNC_CONDITION: EGLint = 0x30F8;
pub const EGL_SYNC_FENCE: EGLenum = 0x30F9;
pub const EGL_SYNC_FLUSH_COMMANDS_BIT: EGLint = 0x0001;
pub const EGL_FOREVER: EGLTime = u64::MAX;

/// The largest pbuffer side.
const MAX_PBUFFER: EGLint = 16384;

/// What EGL is, for any display (`EGL_EXT_client_extensions`).
const CLIENT_EXTENSIONS: &std::ffi::CStr =
    c"EGL_EXT_client_extensions EGL_EXT_platform_base EGL_KHR_platform_base EGL_MESA_platform_surfaceless";
/// What the display has.
const DISPLAY_EXTENSIONS: &std::ffi::CStr =
    c"EGL_KHR_create_context EGL_KHR_fence_sync EGL_KHR_get_all_proc_addresses \
EGL_KHR_surfaceless_context EGL_KHR_wait_sync";

/// The display.
pub struct Display {
    initialized: AtomicBool,
}

static DISPLAY: Display = Display { initialized: AtomicBool::new(false) };

/// The display's handle.
fn display() -> EGLDisplay {
    &DISPLAY as *const Display as EGLDisplay
}

/// A configuration: the buffers a context's default framebuffer has, and
/// pbuffers' with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    pub red: EGLint,
    pub green: EGLint,
    pub blue: EGLint,
    pub alpha: EGLint,
    pub depth: EGLint,
    pub stencil: EGLint,
    pub samples: EGLint,
}

/// The configurations, in their identifiers' order (from 1): color with
/// and without alpha, with depth and stencil buffers, a stencil buffer
/// alone, or neither, and four samples a pixel.
pub const CONFIGS: [Config; 7] = [
    Config { red: 8, green: 8, blue: 8, alpha: 8, depth: 24, stencil: 8, samples: 0 },
    Config { red: 8, green: 8, blue: 8, alpha: 8, depth: 0, stencil: 8, samples: 0 },
    Config { red: 8, green: 8, blue: 8, alpha: 8, depth: 0, stencil: 0, samples: 0 },
    Config { red: 8, green: 8, blue: 8, alpha: 0, depth: 24, stencil: 8, samples: 0 },
    Config { red: 8, green: 8, blue: 8, alpha: 0, depth: 0, stencil: 0, samples: 0 },
    Config { red: 8, green: 8, blue: 8, alpha: 8, depth: 24, stencil: 8, samples: 4 },
    Config { red: 8, green: 8, blue: 8, alpha: 0, depth: 24, stencil: 8, samples: 4 },
];

impl Config {
    /// An attribute's value (`eglGetConfigAttrib`), of configuration `index`.
    pub fn attribute(&self, index: usize, attribute: EGLint) -> Option<EGLint> {
        Some(match attribute {
            EGL_BUFFER_SIZE => self.red + self.green + self.blue + self.alpha,
            EGL_RED_SIZE => self.red,
            EGL_GREEN_SIZE => self.green,
            EGL_BLUE_SIZE => self.blue,
            EGL_ALPHA_SIZE => self.alpha,
            EGL_DEPTH_SIZE => self.depth,
            EGL_STENCIL_SIZE => self.stencil,
            EGL_SAMPLES => self.samples,
            EGL_SAMPLE_BUFFERS => EGLint::from(self.samples > 0),
            EGL_CONFIG_ID => index as EGLint + 1,
            EGL_CONFIG_CAVEAT | EGL_NATIVE_VISUAL_TYPE | EGL_TRANSPARENT_TYPE => EGL_NONE,
            EGL_COLOR_BUFFER_TYPE => EGL_RGB_BUFFER,
            EGL_CONFORMANT | EGL_RENDERABLE_TYPE => EGL_OPENGL_ES2_BIT | EGL_OPENGL_ES3_BIT,
            EGL_SURFACE_TYPE => EGL_PBUFFER_BIT,
            EGL_MAX_PBUFFER_WIDTH | EGL_MAX_PBUFFER_HEIGHT => MAX_PBUFFER,
            EGL_MAX_PBUFFER_PIXELS => MAX_PBUFFER * MAX_PBUFFER,
            EGL_MAX_SWAP_INTERVAL => 1,
            EGL_MIN_SWAP_INTERVAL
            | EGL_LEVEL
            | EGL_LUMINANCE_SIZE
            | EGL_ALPHA_MASK_SIZE
            | EGL_NATIVE_VISUAL_ID
            | EGL_NATIVE_RENDERABLE
            | EGL_BIND_TO_TEXTURE_RGB
            | EGL_BIND_TO_TEXTURE_RGBA
            | EGL_TRANSPARENT_RED_VALUE
            | EGL_TRANSPARENT_GREEN_VALUE
            | EGL_TRANSPARENT_BLUE_VALUE => 0,
            _ => return None,
        })
    }

    /// The default framebuffer `gl` makes of it (none until a surface).
    fn gl(&self) -> gl::Config {
        gl::Config {
            width: 0,
            height: 0,
            color: if self.alpha > 0 { gl::gl::RGBA8 } else { gl::gl::RGB8 },
            depth_bits: self.depth as u8,
            stencil_bits: self.stencil as u8,
            samples: self.samples as u32,
            surfaceless: true,
        }
    }
}

/// The configuration `config` names, by index.
fn config_index(config: EGLConfig) -> Option<usize> {
    (config as usize).checked_sub(1).filter(|&i| i < CONFIGS.len())
}

/// A fence of a share group's renderer.
pub struct Sync {
    group: Arc<Mutex<Group>>,
    /// The renderer's fence (0: the renderer can render no more, and it
    /// has as good as signaled).
    fence: u64,
}

impl Sync {
    /// Whether it has signaled, waiting for it up to `timeout_ns`.
    fn wait(&self, timeout_ns: u64) -> bool {
        self.fence == 0 || lock(&self.group).gl.wait_fence(self.fence, timeout_ns)
    }
}

/// The display's contexts, surfaces and syncs, by handle.
#[derive(Default)]
struct Objects {
    next: u64,
    contexts: HashMap<u64, Arc<Context>>,
    surfaces: HashMap<u64, Arc<Surface>>,
    syncs: HashMap<u64, Arc<Sync>>,
}

impl Objects {
    /// A handle no object has had.
    fn handle(&mut self) -> u64 {
        // Away from 0 and the configurations' small numbers.
        self.next = self.next.max(0x1000) + 1;
        self.next
    }
}

fn objects() -> MutexGuard<'static, Objects> {
    static OBJECTS: OnceLock<Mutex<Objects>> = OnceLock::new();
    OBJECTS.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

fn handle(id: u64) -> *mut c_void {
    id as usize as *mut c_void
}

fn id_of(handle: *mut c_void) -> u64 {
    handle as usize as u64
}

thread_local! {
    /// The error of the thread's last EGL call.
    static ERROR: Cell<EGLint> = const { Cell::new(EGL_SUCCESS) };
}

/// The call succeeds: `v`.
fn ok<T>(v: T) -> T {
    ERROR.with(|e| e.set(EGL_SUCCESS));
    v
}

/// The call fails with `error`: `v` (`EGL_FALSE`, a null handle).
fn fail<T>(error: EGLint, v: T) -> T {
    ERROR.with(|e| e.set(error));
    v
}

/// Ends a call with what `r` says.
fn done<T>(r: Result<T, EGLint>, failed: T) -> T {
    match r {
        Ok(v) => ok(v),
        Err(e) => fail(e, failed),
    }
}

/// The display `dpy` is ours and initialized.
fn check_display(dpy: EGLDisplay) -> Result<(), EGLint> {
    if dpy != display() {
        return Err(EGL_BAD_DISPLAY);
    }
    if !DISPLAY.initialized.load(Ordering::Acquire) {
        return Err(EGL_NOT_INITIALIZED);
    }
    Ok(())
}

/// The pairs of an attribute list (up to `EGL_NONE`; none for a null one).
///
/// # Safety
/// `list` is null or a list ending with `EGL_NONE`.
unsafe fn attributes<T: Copy + Into<i64>>(list: *const T) -> Vec<(i64, i64)> {
    let mut out = Vec::new();
    if list.is_null() {
        return out;
    }
    let mut i = 0;
    loop {
        // SAFETY: the list goes on to its EGL_NONE, which ends this.
        let key: i64 = unsafe { *list.add(i) }.into();
        if key == i64::from(EGL_NONE) {
            return out;
        }
        // SAFETY: as above: every attribute has a value.
        out.push((key, unsafe { *list.add(i + 1) }.into()));
        i += 2;
    }
}

/// Writes `v` to `p`, if it is not null.
///
/// # Safety
/// `p` is null or valid for a write.
unsafe fn put<T>(p: *mut T, v: T) {
    if !p.is_null() {
        // SAFETY: as the caller says.
        unsafe { p.write(v) };
    }
}

// ---- Errors, the display ---------------------------------------------------------------

/// `eglGetError`.
#[unsafe(no_mangle)]
pub extern "C" fn eglGetError() -> EGLint {
    ERROR.with(|e| e.replace(EGL_SUCCESS))
}

/// `eglGetDisplay`: the default display, which is the surfaceless one.
#[unsafe(no_mangle)]
pub extern "C" fn eglGetDisplay(native: EGLNativeDisplayType) -> EGLDisplay {
    if native.is_null() { ok(display()) } else { fail(EGL_BAD_PARAMETER, std::ptr::null_mut()) }
}

/// The platform display `eglGetPlatformDisplay` asks for: the surfaceless
/// platform's default display, without attributes.
fn platform_display(platform: EGLenum, native: *mut c_void, no_attributes: bool) -> EGLDisplay {
    if platform != EGL_PLATFORM_SURFACELESS_MESA {
        return fail(EGL_BAD_PARAMETER, std::ptr::null_mut());
    }
    if !native.is_null() {
        return fail(EGL_BAD_PARAMETER, std::ptr::null_mut());
    }
    if !no_attributes {
        return fail(EGL_BAD_ATTRIBUTE, std::ptr::null_mut());
    }
    ok(display())
}

/// `eglGetPlatformDisplay`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn eglGetPlatformDisplay(
    platform: EGLenum,
    native: *mut c_void,
    attrib_list: *const EGLAttrib,
) -> EGLDisplay {
    // SAFETY: the caller gives an attribute list, or none.
    let none = unsafe { attributes(attrib_list.cast::<i64>()) }.is_empty();
    platform_display(platform, native, none)
}

/// `eglGetPlatformDisplayEXT` (`EGL_EXT_platform_base`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn eglGetPlatformDisplayEXT(
    platform: EGLenum,
    native: *mut c_void,
    attrib_list: *const EGLint,
) -> EGLDisplay {
    // SAFETY: the caller gives an attribute list, or none.
    let none = unsafe { attributes(attrib_list) }.is_empty();
    platform_display(platform, native, none)
}

/// `eglInitialize`: EGL 1.5.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn eglInitialize(dpy: EGLDisplay, major: *mut EGLint, minor: *mut EGLint) -> EGLBoolean {
    if dpy != display() {
        return fail(EGL_BAD_DISPLAY, EGL_FALSE);
    }
    DISPLAY.initialized.store(true, Ordering::Release);
    // SAFETY: the caller gives somewhere for the version, or nowhere.
    unsafe {
        put(major, 1);
        put(minor, 5);
    }
    ok(EGL_TRUE)
}

/// `eglTerminate`: the display's handles go; contexts and surfaces current
/// in threads live on until those let go of them.
#[unsafe(no_mangle)]
pub extern "C" fn eglTerminate(dpy: EGLDisplay) -> EGLBoolean {
    if dpy != display() {
        return fail(EGL_BAD_DISPLAY, EGL_FALSE);
    }
    DISPLAY.initialized.store(false, Ordering::Release);
    // (Not the handle count: no handle is given twice.)
    let gone = {
        let mut o = objects();
        (std::mem::take(&mut o.contexts), std::mem::take(&mut o.surfaces), std::mem::take(&mut o.syncs))
    };
    drop(gone);
    ok(EGL_TRUE)
}

/// `eglQueryString`.
#[unsafe(no_mangle)]
pub extern "C" fn eglQueryString(dpy: EGLDisplay, name: EGLint) -> *const c_char {
    if dpy.is_null() && name == EGL_EXTENSIONS {
        return ok(CLIENT_EXTENSIONS.as_ptr());
    }
    if let Err(e) = check_display(dpy) {
        return fail(e, std::ptr::null());
    }
    let s: &std::ffi::CStr = match name {
        EGL_VENDOR => c"Veda",
        EGL_VERSION => c"1.5 Veda",
        EGL_CLIENT_APIS => c"OpenGL_ES",
        EGL_EXTENSIONS => DISPLAY_EXTENSIONS,
        _ => return fail(EGL_BAD_PARAMETER, std::ptr::null()),
    };
    ok(s.as_ptr())
}

// ---- Configurations -----------------------------------------------------------------------

/// Writes up to `size` configurations of `chosen` to `configs` (or, if it
/// is null, none) and how many to `num_config`.
///
/// # Safety
/// As `eglGetConfigs`' arguments.
unsafe fn give_configs(chosen: &[usize], configs: *mut EGLConfig, size: EGLint, num_config: *mut EGLint) -> EGLBoolean {
    if num_config.is_null() {
        return fail(EGL_BAD_PARAMETER, EGL_FALSE);
    }
    let n = if configs.is_null() { chosen.len() } else { chosen.len().min(size.max(0) as usize) };
    if !configs.is_null() {
        for (i, &c) in chosen.iter().take(n).enumerate() {
            // SAFETY: the caller has room for `size` configurations.
            unsafe { configs.add(i).write((c + 1) as EGLConfig) };
        }
    }
    // SAFETY: checked not null.
    unsafe { num_config.write(n as EGLint) };
    ok(EGL_TRUE)
}

/// `eglGetConfigs`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn eglGetConfigs(
    dpy: EGLDisplay,
    configs: *mut EGLConfig,
    config_size: EGLint,
    num_config: *mut EGLint,
) -> EGLBoolean {
    if let Err(e) = check_display(dpy) {
        return fail(e, EGL_FALSE);
    }
    let all: Vec<usize> = (0..CONFIGS.len()).collect();
    // SAFETY: forwarded from the caller.
    unsafe { give_configs(&all, configs, config_size, num_config) }
}

/// How an attribute of `eglChooseConfig` matches (EGL 1.5, table 3.4).
enum Criterion {
    AtLeast,
    Exact,
    Mask,
}

/// The configurations that match `wanted` (attribute, value), in EGL's
/// order (EGL 1.5, section 3.4.1.2): an error for an attribute that is no
/// configuration's.
pub fn choose(wanted: &[(i64, i64)]) -> Result<Vec<usize>, EGLint> {
    // Attributes' defaults, then what is asked for.
    let mut asks: HashMap<EGLint, EGLint> = HashMap::from([
        (EGL_RENDERABLE_TYPE, 1), // EGL_OPENGL_ES_BIT
        (EGL_SURFACE_TYPE, EGL_WINDOW_BIT),
        (EGL_COLOR_BUFFER_TYPE, EGL_RGB_BUFFER),
        (EGL_LEVEL, 0),
    ]);
    for &(k, v) in wanted {
        let (k, v) = (k as EGLint, v as EGLint);
        // What choosing ignores (EGL 1.5, table 3.4).
        if matches!(
            k,
            EGL_MATCH_NATIVE_PIXMAP
                | EGL_MAX_PBUFFER_WIDTH
                | EGL_MAX_PBUFFER_HEIGHT
                | EGL_MAX_PBUFFER_PIXELS
                | EGL_NATIVE_VISUAL_ID
        ) {
            continue;
        }
        if CONFIGS[0].attribute(0, k).is_none() {
            return Err(EGL_BAD_ATTRIBUTE);
        }
        asks.insert(k, v);
    }
    // An identifier chooses that configuration, whatever else is asked.
    if let Some(&id) = asks.get(&EGL_CONFIG_ID).filter(|&&id| id != EGL_DONT_CARE) {
        return Ok((1..=CONFIGS.len() as EGLint).filter(|&i| i == id).map(|i| i as usize - 1).collect());
    }
    let criterion = |k: EGLint| match k {
        EGL_CONFORMANT | EGL_RENDERABLE_TYPE | EGL_SURFACE_TYPE => Criterion::Mask,
        EGL_BUFFER_SIZE | EGL_RED_SIZE | EGL_GREEN_SIZE | EGL_BLUE_SIZE | EGL_ALPHA_SIZE | EGL_DEPTH_SIZE
        | EGL_STENCIL_SIZE | EGL_SAMPLES | EGL_SAMPLE_BUFFERS | EGL_LUMINANCE_SIZE | EGL_ALPHA_MASK_SIZE => {
            Criterion::AtLeast
        }
        _ => Criterion::Exact,
    };
    let mut chosen: Vec<usize> = (0..CONFIGS.len())
        .filter(|&i| {
            asks.iter().all(|(&k, &v)| {
                let have = CONFIGS[i].attribute(i, k).unwrap_or(0);
                v == EGL_DONT_CARE
                    || match criterion(k) {
                        Criterion::AtLeast => have >= v,
                        Criterion::Exact => have == v,
                        Criterion::Mask => have & v == v,
                    }
            })
        })
        .collect();
    // More bits of the colors asked for first, then fewer of everything
    // else; the identifiers last.
    let asked = |k: EGLint| asks.get(&k).is_some_and(|&v| v != 0 && v != EGL_DONT_CARE);
    chosen.sort_by_key(|&i| {
        let c = &CONFIGS[i];
        let color: EGLint =
            [(EGL_RED_SIZE, c.red), (EGL_GREEN_SIZE, c.green), (EGL_BLUE_SIZE, c.blue), (EGL_ALPHA_SIZE, c.alpha)]
                .iter()
                .filter(|(k, _)| asked(*k))
                .map(|(_, v)| v)
                .sum();
        (-color, c.red + c.green + c.blue + c.alpha, EGLint::from(c.samples > 0), c.samples, c.depth, c.stencil, i)
    });
    Ok(chosen)
}

/// `eglChooseConfig`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn eglChooseConfig(
    dpy: EGLDisplay,
    attrib_list: *const EGLint,
    configs: *mut EGLConfig,
    config_size: EGLint,
    num_config: *mut EGLint,
) -> EGLBoolean {
    if let Err(e) = check_display(dpy) {
        return fail(e, EGL_FALSE);
    }
    // SAFETY: the caller gives an attribute list, or none.
    let wanted = unsafe { attributes(attrib_list) };
    match choose(&wanted) {
        // SAFETY: forwarded from the caller.
        Ok(chosen) => unsafe { give_configs(&chosen, configs, config_size, num_config) },
        Err(e) => fail(e, EGL_FALSE),
    }
}

/// `eglGetConfigAttrib`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn eglGetConfigAttrib(
    dpy: EGLDisplay,
    config: EGLConfig,
    attribute: EGLint,
    value: *mut EGLint,
) -> EGLBoolean {
    if let Err(e) = check_display(dpy) {
        return fail(e, EGL_FALSE);
    }
    let Some(i) = config_index(config) else { return fail(EGL_BAD_CONFIG, EGL_FALSE) };
    let Some(v) = CONFIGS[i].attribute(i, attribute) else { return fail(EGL_BAD_ATTRIBUTE, EGL_FALSE) };
    // SAFETY: the caller gives somewhere for the value.
    unsafe { put(value, v) };
    ok(EGL_TRUE)
}

// ---- The API ---------------------------------------------------------------------------------

/// `eglBindAPI`: OpenGL ES is the one there is.
#[unsafe(no_mangle)]
pub extern "C" fn eglBindAPI(api: EGLenum) -> EGLBoolean {
    if api == EGL_OPENGL_ES_API { ok(EGL_TRUE) } else { fail(EGL_BAD_PARAMETER, EGL_FALSE) }
}

/// `eglQueryAPI`.
#[unsafe(no_mangle)]
pub extern "C" fn eglQueryAPI() -> EGLenum {
    ok(EGL_OPENGL_ES_API)
}

// ---- Contexts ------------------------------------------------------------------------------

/// A context's attributes (`EGL_KHR_create_context`): OpenGL ES 2.0 or 3.0,
/// which a 3.0 context is; debugging, which is accepted (`gl` checks every
/// call anyway); no robust access.
fn context_attributes(list: &[(i64, i64)]) -> Result<(), EGLint> {
    let (mut major, mut minor) = (1, 0);
    for &(k, v) in list {
        match k as EGLint {
            EGL_CONTEXT_MAJOR_VERSION => major = v,
            EGL_CONTEXT_MINOR_VERSION => minor = v,
            EGL_CONTEXT_OPENGL_DEBUG => {}
            EGL_CONTEXT_FLAGS_KHR if v & !i64::from(EGL_CONTEXT_OPENGL_DEBUG_BIT_KHR) == 0 => {}
            EGL_CONTEXT_OPENGL_ROBUST_ACCESS if v == 0 => {}
            EGL_CONTEXT_OPENGL_RESET_NOTIFICATION_STRATEGY if v == i64::from(EGL_NO_RESET_NOTIFICATION) => {}
            EGL_CONTEXT_FLAGS_KHR
            | EGL_CONTEXT_OPENGL_ROBUST_ACCESS
            | EGL_CONTEXT_OPENGL_RESET_NOTIFICATION_STRATEGY => {
                return Err(EGL_BAD_MATCH);
            }
            EGL_CONTEXT_OPENGL_PROFILE_MASK | EGL_CONTEXT_OPENGL_FORWARD_COMPATIBLE => return Err(EGL_BAD_ATTRIBUTE),
            _ => return Err(EGL_BAD_ATTRIBUTE),
        }
    }
    match (major, minor) {
        (2, 0) | (3, 0) => Ok(()),
        _ => Err(EGL_BAD_MATCH),
    }
}

/// `eglCreateContext`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn eglCreateContext(
    dpy: EGLDisplay,
    config: EGLConfig,
    share_context: EGLContext,
    attrib_list: *const EGLint,
) -> EGLContext {
    let none = std::ptr::null_mut();
    if let Err(e) = check_display(dpy) {
        return fail(e, none);
    }
    let Some(i) = config_index(config) else { return fail(EGL_BAD_CONFIG, none) };
    // SAFETY: the caller gives an attribute list, or none.
    if let Err(e) = context_attributes(&unsafe { attributes(attrib_list) }) {
        return fail(e, none);
    }
    let mut o = objects();
    let share = if share_context.is_null() {
        None
    } else {
        match o.contexts.get(&id_of(share_context)) {
            Some(c) => Some(c.clone()),
            None => return fail(EGL_BAD_CONTEXT, none),
        }
    };
    let id = o.handle();
    let context = Arc::new(Context::new(id, i, CONFIGS[i].gl(), share.as_deref()));
    o.contexts.insert(id, context);
    ok(handle(id))
}

/// `eglDestroyContext`: its handle goes; it lives on while current.
#[unsafe(no_mangle)]
pub extern "C" fn eglDestroyContext(dpy: EGLDisplay, ctx: EGLContext) -> EGLBoolean {
    if let Err(e) = check_display(dpy) {
        return fail(e, EGL_FALSE);
    }
    let gone = objects().contexts.remove(&id_of(ctx));
    match gone {
        Some(context) => {
            drop(context);
            ok(EGL_TRUE)
        }
        None => fail(EGL_BAD_CONTEXT, EGL_FALSE),
    }
}

/// `eglMakeCurrent`: draw and read are the same surface, or none.
#[unsafe(no_mangle)]
pub extern "C" fn eglMakeCurrent(dpy: EGLDisplay, draw: EGLSurface, read: EGLSurface, ctx: EGLContext) -> EGLBoolean {
    // Letting go of the current context needs no display (EGL 1.5).
    if ctx.is_null() && draw.is_null() && read.is_null() {
        if !dpy.is_null() && dpy != display() {
            return fail(EGL_BAD_DISPLAY, EGL_FALSE);
        }
        current::make_current(None, None);
        return ok(EGL_TRUE);
    }
    done(make_current(dpy, draw, read, ctx), EGL_FALSE)
}

fn make_current(dpy: EGLDisplay, draw: EGLSurface, read: EGLSurface, ctx: EGLContext) -> Result<EGLBoolean, EGLint> {
    check_display(dpy)?;
    if ctx.is_null() || draw != read {
        return Err(EGL_BAD_MATCH);
    }
    let (context, surface) = {
        let o = objects();
        let context = o.contexts.get(&id_of(ctx)).cloned().ok_or(EGL_BAD_CONTEXT)?;
        let surface =
            if draw.is_null() { None } else { Some(o.surfaces.get(&id_of(draw)).cloned().ok_or(EGL_BAD_SURFACE)?) };
        (context, surface)
    };
    let me = std::thread::current().id();
    if context.current_thread().is_some_and(|t| t != me) {
        return Err(EGL_BAD_ACCESS);
    }
    if let Some(s) = &surface {
        if s.config != context.config {
            return Err(EGL_BAD_MATCH);
        }
        // Another context's, unless the thread's own, which it lets go.
        let mine = current::current().map(|c| c.id);
        let theirs = *s.context.lock().unwrap_or_else(|e| e.into_inner());
        if theirs.is_some_and(|t| t != context.id && Some(t) != mine) {
            return Err(EGL_BAD_ACCESS);
        }
    }
    current::make_current(Some(context), surface);
    Ok(EGL_TRUE)
}

/// `eglGetCurrentContext`.
#[unsafe(no_mangle)]
pub extern "C" fn eglGetCurrentContext() -> EGLContext {
    ok(current::current().map_or(std::ptr::null_mut(), |c| handle(c.id)))
}

/// `eglGetCurrentDisplay`.
#[unsafe(no_mangle)]
pub extern "C" fn eglGetCurrentDisplay() -> EGLDisplay {
    ok(if current::current().is_some() { display() } else { std::ptr::null_mut() })
}

/// `eglGetCurrentSurface`.
#[unsafe(no_mangle)]
pub extern "C" fn eglGetCurrentSurface(readdraw: EGLint) -> EGLSurface {
    if readdraw != EGL_DRAW && readdraw != EGL_READ {
        return fail(EGL_BAD_PARAMETER, std::ptr::null_mut());
    }
    let Some(context) = current::current() else { return ok(std::ptr::null_mut()) };
    let surface = context.surface.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let id = surface.and_then(|s| objects().surfaces.iter().find(|(_, t)| Arc::ptr_eq(t, &s)).map(|(&id, _)| id));
    ok(id.map_or(std::ptr::null_mut(), handle))
}

/// `eglQueryContext`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn eglQueryContext(
    dpy: EGLDisplay,
    ctx: EGLContext,
    attribute: EGLint,
    value: *mut EGLint,
) -> EGLBoolean {
    if let Err(e) = check_display(dpy) {
        return fail(e, EGL_FALSE);
    }
    let Some(context) = objects().contexts.get(&id_of(ctx)).cloned() else { return fail(EGL_BAD_CONTEXT, EGL_FALSE) };
    let v = match attribute {
        EGL_CONFIG_ID => context.config as EGLint + 1,
        EGL_CONTEXT_CLIENT_TYPE => EGL_OPENGL_ES_API as EGLint,
        EGL_CONTEXT_MAJOR_VERSION => 3,
        EGL_RENDER_BUFFER => {
            let surfaced = context.surface.lock().unwrap_or_else(|e| e.into_inner()).is_some();
            if surfaced { EGL_BACK_BUFFER } else { EGL_NONE }
        }
        _ => return fail(EGL_BAD_ATTRIBUTE, EGL_FALSE),
    };
    // SAFETY: the caller gives somewhere for the value.
    unsafe { put(value, v) };
    ok(EGL_TRUE)
}

/// `eglReleaseThread`: the thread lets go of its context.
#[unsafe(no_mangle)]
pub extern "C" fn eglReleaseThread() -> EGLBoolean {
    current::make_current(None, None);
    ok(EGL_TRUE)
}

/// `eglWaitClient`: the current context's work done.
#[unsafe(no_mangle)]
pub extern "C" fn eglWaitClient() -> EGLBoolean {
    current::with_gl(|c| c.finish());
    ok(EGL_TRUE)
}

/// `eglWaitGL`.
#[unsafe(no_mangle)]
pub extern "C" fn eglWaitGL() -> EGLBoolean {
    eglWaitClient()
}

/// `eglWaitNative`: there is no native rendering to wait for.
#[unsafe(no_mangle)]
pub extern "C" fn eglWaitNative(_engine: EGLint) -> EGLBoolean {
    ok(EGL_TRUE)
}

// ---- Surfaces ------------------------------------------------------------------------------

/// `eglCreatePbufferSurface`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn eglCreatePbufferSurface(
    dpy: EGLDisplay,
    config: EGLConfig,
    attrib_list: *const EGLint,
) -> EGLSurface {
    // SAFETY: the caller gives an attribute list, or none.
    let list = unsafe { attributes(attrib_list) };
    done(create_pbuffer(dpy, config, &list), std::ptr::null_mut())
}

fn create_pbuffer(dpy: EGLDisplay, config: EGLConfig, list: &[(i64, i64)]) -> Result<EGLSurface, EGLint> {
    check_display(dpy)?;
    let i = config_index(config).ok_or(EGL_BAD_CONFIG)?;
    let (mut width, mut height) = (0, 0);
    for &(k, v) in list {
        match k as EGLint {
            EGL_WIDTH => width = v,
            EGL_HEIGHT => height = v,
            EGL_LARGEST_PBUFFER | EGL_MIPMAP_TEXTURE | EGL_VG_COLORSPACE | EGL_VG_ALPHA_FORMAT => {}
            EGL_TEXTURE_FORMAT | EGL_TEXTURE_TARGET if v != i64::from(EGL_NO_TEXTURE) => return Err(EGL_BAD_MATCH),
            EGL_TEXTURE_FORMAT | EGL_TEXTURE_TARGET => {}
            EGL_GL_COLORSPACE if v != i64::from(EGL_GL_COLORSPACE_LINEAR) => return Err(EGL_BAD_MATCH),
            EGL_GL_COLORSPACE => {}
            _ => return Err(EGL_BAD_ATTRIBUTE),
        }
    }
    if width < 0 || height < 0 {
        return Err(EGL_BAD_PARAMETER);
    }
    if width > i64::from(MAX_PBUFFER) || height > i64::from(MAX_PBUFFER) {
        return Err(EGL_BAD_MATCH);
    }
    let surface = Surface { config: i, width: width as u32, height: height as u32, context: Mutex::new(None) };
    let mut o = objects();
    let id = o.handle();
    o.surfaces.insert(id, Arc::new(surface));
    Ok(handle(id))
}

/// `eglCreateWindowSurface`: no configuration has windows (yet).
#[unsafe(no_mangle)]
pub extern "C" fn eglCreateWindowSurface(
    dpy: EGLDisplay,
    _config: EGLConfig,
    _win: EGLNativeWindowType,
    _attrib_list: *const EGLint,
) -> EGLSurface {
    let e = check_display(dpy).err().unwrap_or(EGL_BAD_MATCH);
    fail(e, std::ptr::null_mut())
}

/// `eglCreatePlatformWindowSurface`.
#[unsafe(no_mangle)]
pub extern "C" fn eglCreatePlatformWindowSurface(
    dpy: EGLDisplay,
    config: EGLConfig,
    _native_window: *mut c_void,
    _attrib_list: *const EGLAttrib,
) -> EGLSurface {
    eglCreateWindowSurface(dpy, config, std::ptr::null_mut(), std::ptr::null())
}

/// `eglCreatePixmapSurface`: no configuration has pixmaps.
#[unsafe(no_mangle)]
pub extern "C" fn eglCreatePixmapSurface(
    dpy: EGLDisplay,
    config: EGLConfig,
    _pixmap: EGLNativePixmapType,
    _attrib_list: *const EGLint,
) -> EGLSurface {
    eglCreateWindowSurface(dpy, config, std::ptr::null_mut(), std::ptr::null())
}

/// `eglCreatePlatformPixmapSurface`.
#[unsafe(no_mangle)]
pub extern "C" fn eglCreatePlatformPixmapSurface(
    dpy: EGLDisplay,
    config: EGLConfig,
    _native_pixmap: *mut c_void,
    _attrib_list: *const EGLAttrib,
) -> EGLSurface {
    eglCreateWindowSurface(dpy, config, std::ptr::null_mut(), std::ptr::null())
}

/// `eglCreatePbufferFromClientBuffer`: there are no client buffers.
#[unsafe(no_mangle)]
pub extern "C" fn eglCreatePbufferFromClientBuffer(
    dpy: EGLDisplay,
    _buftype: EGLenum,
    _buffer: EGLClientBuffer,
    _config: EGLConfig,
    _attrib_list: *const EGLint,
) -> EGLSurface {
    let e = check_display(dpy).err().unwrap_or(EGL_BAD_PARAMETER);
    fail(e, std::ptr::null_mut())
}

/// `eglDestroySurface`: its handle goes; it lives on while current.
#[unsafe(no_mangle)]
pub extern "C" fn eglDestroySurface(dpy: EGLDisplay, surface: EGLSurface) -> EGLBoolean {
    if let Err(e) = check_display(dpy) {
        return fail(e, EGL_FALSE);
    }
    let gone = objects().surfaces.remove(&id_of(surface));
    if gone.is_some() { ok(EGL_TRUE) } else { fail(EGL_BAD_SURFACE, EGL_FALSE) }
}

/// The surface `surface` names.
fn surface_of(dpy: EGLDisplay, surface: EGLSurface) -> Result<Arc<Surface>, EGLint> {
    check_display(dpy)?;
    objects().surfaces.get(&id_of(surface)).cloned().ok_or(EGL_BAD_SURFACE)
}

/// `eglQuerySurface`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn eglQuerySurface(
    dpy: EGLDisplay,
    surface: EGLSurface,
    attribute: EGLint,
    value: *mut EGLint,
) -> EGLBoolean {
    let s = match surface_of(dpy, surface) {
        Ok(s) => s,
        Err(e) => return fail(e, EGL_FALSE),
    };
    let v = match attribute {
        EGL_WIDTH => s.width as EGLint,
        EGL_HEIGHT => s.height as EGLint,
        EGL_CONFIG_ID => s.config as EGLint + 1,
        EGL_LARGEST_PBUFFER | EGL_MIPMAP_TEXTURE | EGL_MIPMAP_LEVEL => 0,
        EGL_TEXTURE_FORMAT | EGL_TEXTURE_TARGET => EGL_NO_TEXTURE,
        EGL_RENDER_BUFFER => EGL_BACK_BUFFER,
        EGL_SWAP_BEHAVIOR => EGL_BUFFER_DESTROYED,
        EGL_MULTISAMPLE_RESOLVE => EGL_MULTISAMPLE_RESOLVE_DEFAULT,
        EGL_HORIZONTAL_RESOLUTION | EGL_VERTICAL_RESOLUTION | EGL_PIXEL_ASPECT_RATIO => EGL_UNKNOWN,
        EGL_GL_COLORSPACE => EGL_GL_COLORSPACE_LINEAR,
        _ => return fail(EGL_BAD_ATTRIBUTE, EGL_FALSE),
    };
    // SAFETY: the caller gives somewhere for the value.
    unsafe { put(value, v) };
    ok(EGL_TRUE)
}

/// `eglSurfaceAttrib`: what pbuffers have as they are.
#[unsafe(no_mangle)]
pub extern "C" fn eglSurfaceAttrib(
    dpy: EGLDisplay,
    surface: EGLSurface,
    attribute: EGLint,
    value: EGLint,
) -> EGLBoolean {
    if let Err(e) = surface_of(dpy, surface) {
        return fail(e, EGL_FALSE);
    }
    match (attribute, value) {
        (EGL_MIPMAP_LEVEL, _) | (EGL_SWAP_BEHAVIOR, EGL_BUFFER_DESTROYED) => ok(EGL_TRUE),
        (EGL_MULTISAMPLE_RESOLVE, EGL_MULTISAMPLE_RESOLVE_DEFAULT) => ok(EGL_TRUE),
        (EGL_SWAP_BEHAVIOR | EGL_MULTISAMPLE_RESOLVE, _) => fail(EGL_BAD_MATCH, EGL_FALSE),
        _ => fail(EGL_BAD_ATTRIBUTE, EGL_FALSE),
    }
}

/// `eglBindTexImage`: no pbuffer is bound to textures.
#[unsafe(no_mangle)]
pub extern "C" fn eglBindTexImage(dpy: EGLDisplay, surface: EGLSurface, _buffer: EGLint) -> EGLBoolean {
    let e = surface_of(dpy, surface).err().unwrap_or(EGL_BAD_MATCH);
    fail(e, EGL_FALSE)
}

/// `eglReleaseTexImage`.
#[unsafe(no_mangle)]
pub extern "C" fn eglReleaseTexImage(dpy: EGLDisplay, surface: EGLSurface, buffer: EGLint) -> EGLBoolean {
    eglBindTexImage(dpy, surface, buffer)
}

/// `eglSwapInterval`: pbuffers are not shown.
#[unsafe(no_mangle)]
pub extern "C" fn eglSwapInterval(dpy: EGLDisplay, _interval: EGLint) -> EGLBoolean {
    if let Err(e) = check_display(dpy) {
        return fail(e, EGL_FALSE);
    }
    if current::current().is_none() {
        return fail(EGL_BAD_CONTEXT, EGL_FALSE);
    }
    ok(EGL_TRUE)
}

/// `eglSwapBuffers`: a pbuffer's has no effect.
#[unsafe(no_mangle)]
pub extern "C" fn eglSwapBuffers(dpy: EGLDisplay, surface: EGLSurface) -> EGLBoolean {
    match surface_of(dpy, surface) {
        Ok(_) => ok(EGL_TRUE),
        Err(e) => fail(e, EGL_FALSE),
    }
}

/// `eglCopyBuffers`: there are no native pixmaps.
#[unsafe(no_mangle)]
pub extern "C" fn eglCopyBuffers(dpy: EGLDisplay, surface: EGLSurface, _target: EGLNativePixmapType) -> EGLBoolean {
    let e = surface_of(dpy, surface).err().unwrap_or(EGL_BAD_NATIVE_PIXMAP);
    fail(e, EGL_FALSE)
}

// ---- Fences -------------------------------------------------------------------------------

/// A fence after the current context's work.
fn create_sync(dpy: EGLDisplay, ty: EGLenum, list: &[(i64, i64)]) -> Result<EGLSync, EGLint> {
    check_display(dpy)?;
    if ty != EGL_SYNC_FENCE || !list.is_empty() {
        return Err(EGL_BAD_ATTRIBUTE);
    }
    let context = current::current().ok_or(EGL_BAD_MATCH)?;
    let fence = current::with_gl(|c| c.fence());
    let sync = Sync { group: context.group.clone(), fence };
    let mut o = objects();
    let id = o.handle();
    o.syncs.insert(id, Arc::new(sync));
    Ok(handle(id))
}

/// `eglCreateSync`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn eglCreateSync(dpy: EGLDisplay, ty: EGLenum, attrib_list: *const EGLAttrib) -> EGLSync {
    // SAFETY: the caller gives an attribute list, or none.
    let list = unsafe { attributes(attrib_list.cast::<i64>()) };
    done(create_sync(dpy, ty, &list), std::ptr::null_mut())
}

/// `eglCreateSyncKHR` (`EGL_KHR_fence_sync`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn eglCreateSyncKHR(dpy: EGLDisplay, ty: EGLenum, attrib_list: *const EGLint) -> EGLSync {
    // SAFETY: the caller gives an attribute list, or none.
    let list = unsafe { attributes(attrib_list) };
    done(create_sync(dpy, ty, &list), std::ptr::null_mut())
}

/// The sync `sync` names.
fn sync_of(dpy: EGLDisplay, sync: EGLSync) -> Result<Arc<Sync>, EGLint> {
    check_display(dpy)?;
    objects().syncs.get(&id_of(sync)).cloned().ok_or(EGL_BAD_PARAMETER)
}

/// `eglDestroySync`.
#[unsafe(no_mangle)]
pub extern "C" fn eglDestroySync(dpy: EGLDisplay, sync: EGLSync) -> EGLBoolean {
    if let Err(e) = check_display(dpy) {
        return fail(e, EGL_FALSE);
    }
    let gone = objects().syncs.remove(&id_of(sync));
    if gone.is_some() { ok(EGL_TRUE) } else { fail(EGL_BAD_PARAMETER, EGL_FALSE) }
}

/// `eglDestroySyncKHR`.
#[unsafe(no_mangle)]
pub extern "C" fn eglDestroySyncKHR(dpy: EGLDisplay, sync: EGLSync) -> EGLBoolean {
    eglDestroySync(dpy, sync)
}

/// `eglClientWaitSync`.
#[unsafe(no_mangle)]
pub extern "C" fn eglClientWaitSync(dpy: EGLDisplay, sync: EGLSync, flags: EGLint, timeout: EGLTime) -> EGLint {
    let s = match sync_of(dpy, sync) {
        Ok(s) => s,
        Err(e) => return fail(e, EGL_FALSE as EGLint),
    };
    if flags & EGL_SYNC_FLUSH_COMMANDS_BIT != 0 {
        lock(&s.group).gl.flush();
    }
    ok(if s.wait(timeout) { EGL_CONDITION_SATISFIED } else { EGL_TIMEOUT_EXPIRED })
}

/// `eglClientWaitSyncKHR`.
#[unsafe(no_mangle)]
pub extern "C" fn eglClientWaitSyncKHR(dpy: EGLDisplay, sync: EGLSync, flags: EGLint, timeout: EGLTime) -> EGLint {
    eglClientWaitSync(dpy, sync, flags, timeout)
}

/// `eglWaitSync` (`EGL_KHR_wait_sync`): the current context's next work
/// after the fence's, which this waits for (a group's renderer is not
/// another's).
#[unsafe(no_mangle)]
pub extern "C" fn eglWaitSync(dpy: EGLDisplay, sync: EGLSync, flags: EGLint) -> EGLBoolean {
    let s = match sync_of(dpy, sync) {
        Ok(s) => s,
        Err(e) => return fail(e, EGL_FALSE),
    };
    if flags != 0 {
        return fail(EGL_BAD_PARAMETER, EGL_FALSE);
    }
    if current::current().is_none() {
        return fail(EGL_BAD_MATCH, EGL_FALSE);
    }
    s.wait(EGL_FOREVER);
    ok(EGL_TRUE)
}

/// `eglWaitSyncKHR`.
#[unsafe(no_mangle)]
pub extern "C" fn eglWaitSyncKHR(dpy: EGLDisplay, sync: EGLSync, flags: EGLint) -> EGLint {
    eglWaitSync(dpy, sync, flags) as EGLint
}

/// A sync's attribute.
fn sync_attribute(dpy: EGLDisplay, sync: EGLSync, attribute: EGLint) -> Result<EGLint, EGLint> {
    let s = sync_of(dpy, sync)?;
    Ok(match attribute {
        EGL_SYNC_TYPE => EGL_SYNC_FENCE as EGLint,
        EGL_SYNC_STATUS => {
            if s.wait(0) {
                EGL_SIGNALED
            } else {
                EGL_UNSIGNALED
            }
        }
        EGL_SYNC_CONDITION => EGL_SYNC_PRIOR_COMMANDS_COMPLETE,
        _ => return Err(EGL_BAD_ATTRIBUTE),
    })
}

/// `eglGetSyncAttrib`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn eglGetSyncAttrib(
    dpy: EGLDisplay,
    sync: EGLSync,
    attribute: EGLint,
    value: *mut EGLAttrib,
) -> EGLBoolean {
    match sync_attribute(dpy, sync, attribute) {
        Ok(v) => {
            // SAFETY: the caller gives somewhere for the value.
            unsafe { put(value, v as EGLAttrib) };
            ok(EGL_TRUE)
        }
        Err(e) => fail(e, EGL_FALSE),
    }
}

/// `eglGetSyncAttribKHR`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn eglGetSyncAttribKHR(
    dpy: EGLDisplay,
    sync: EGLSync,
    attribute: EGLint,
    value: *mut EGLint,
) -> EGLBoolean {
    match sync_attribute(dpy, sync, attribute) {
        Ok(v) => {
            // SAFETY: the caller gives somewhere for the value.
            unsafe { put(value, v) };
            ok(EGL_TRUE)
        }
        Err(e) => fail(e, EGL_FALSE),
    }
}

// ---- Images ----------------------------------------------------------------------------------

/// `eglCreateImage`: no target is an image's (there is no
/// `EGL_KHR_image_base`, nor its targets').
#[unsafe(no_mangle)]
pub extern "C" fn eglCreateImage(
    dpy: EGLDisplay,
    _ctx: EGLContext,
    _target: EGLenum,
    _buffer: EGLClientBuffer,
    _attrib_list: *const EGLAttrib,
) -> EGLImage {
    let e = check_display(dpy).err().unwrap_or(EGL_BAD_PARAMETER);
    fail(e, std::ptr::null_mut())
}

/// `eglDestroyImage`: there are no images.
#[unsafe(no_mangle)]
pub extern "C" fn eglDestroyImage(dpy: EGLDisplay, _image: EGLImage) -> EGLBoolean {
    let e = check_display(dpy).err().unwrap_or(EGL_BAD_PARAMETER);
    fail(e, EGL_FALSE)
}
