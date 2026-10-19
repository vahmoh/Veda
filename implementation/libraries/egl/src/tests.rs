// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The C interface as programs use it: WebKit's way through EGL (the
//! surfaceless display, a configuration, contexts that share, fences), and
//! drawing and reading pixels with OpenGL ES. The display is the process's
//! one: no test terminates it.

use std::ffi::{CStr, c_void};

use gl::gl;

use crate::egl::*;
use crate::gles::buffers::*;
use crate::gles::draw::*;
use crate::gles::framebuffers::*;
use crate::gles::state::*;
use crate::gles::textures::*;
use crate::procs::eglGetProcAddress;
use crate::*;

/// The surfaceless display, initialized.
fn display() -> EGLDisplay {
    // SAFETY: no attributes.
    let d = unsafe { eglGetPlatformDisplayEXT(EGL_PLATFORM_SURFACELESS_MESA, std::ptr::null_mut(), std::ptr::null()) };
    assert!(!d.is_null());
    let (mut major, mut minor) = (0, 0);
    // SAFETY: room for the version.
    assert_eq!(unsafe { eglInitialize(d, &mut major, &mut minor) }, EGL_TRUE);
    assert_eq!((major, minor), (1, 5));
    d
}

/// The configuration WebKit asks for: OpenGL ES 2, RGBA 8888 with a stencil
/// buffer, for pbuffers (surfaceless contexts).
fn webkit_config(d: EGLDisplay) -> EGLConfig {
    let attributes = [
        EGL_RENDERABLE_TYPE,
        EGL_OPENGL_ES2_BIT,
        EGL_RED_SIZE,
        8,
        EGL_GREEN_SIZE,
        8,
        EGL_BLUE_SIZE,
        8,
        EGL_ALPHA_SIZE,
        8,
        EGL_STENCIL_SIZE,
        8,
        EGL_SURFACE_TYPE,
        EGL_PBUFFER_BIT,
        EGL_DEPTH_SIZE,
        0,
        EGL_NONE,
    ];
    let mut configs = [std::ptr::null_mut(); 8];
    let mut n = 0;
    // SAFETY: a list ending with EGL_NONE; room for 8 configurations.
    let r = unsafe { eglChooseConfig(d, attributes.as_ptr(), configs.as_mut_ptr(), 8, &mut n) };
    assert_eq!(r, EGL_TRUE);
    assert!(n >= 1);
    configs[0]
}

/// A context of `config`, sharing `share`'s objects (or none).
fn context(d: EGLDisplay, config: EGLConfig, share: EGLContext) -> EGLContext {
    let attributes = [EGL_CONTEXT_MAJOR_VERSION, 2, EGL_NONE];
    // SAFETY: a list ending with EGL_NONE.
    let c = unsafe { eglCreateContext(d, config, share, attributes.as_ptr()) };
    assert!(!c.is_null(), "error {:#x}", eglGetError());
    c
}

/// A framebuffer with a 2 x 2 RGBA8 texture, bound.
fn framebuffer() -> GLuint {
    let (mut t, mut f) = (0, 0);
    // SAFETY: room for one name each.
    unsafe {
        glGenTextures(1, &mut t);
        glBindTexture(gl::TEXTURE_2D, t);
        glTexStorage2D(gl::TEXTURE_2D, 1, gl::RGBA8, 2, 2);
        glGenFramebuffers(1, &mut f);
    }
    glBindFramebuffer(gl::FRAMEBUFFER, f);
    glFramebufferTexture2D(gl::FRAMEBUFFER, gl::COLOR_ATTACHMENT0, gl::TEXTURE_2D, t, 0);
    assert_eq!(glCheckFramebufferStatus(gl::FRAMEBUFFER), gl::FRAMEBUFFER_COMPLETE);
    t
}

/// The bound framebuffer's pixel (0, 0), as BGRA bytes.
fn pixel_bgra() -> [u8; 4] {
    let mut p = [0u8; 4];
    // SAFETY: room for one BGRA pixel.
    unsafe { glReadPixels(0, 0, 1, 1, gl::BGRA_EXT, gl::UNSIGNED_BYTE, p.as_mut_ptr().cast()) };
    p
}

#[test]
fn the_surfaceless_display_is_what_webkit_looks_for() {
    // SAFETY: EGL's strings are NUL-terminated.
    let client = unsafe { CStr::from_ptr(eglQueryString(std::ptr::null_mut(), EGL_EXTENSIONS)) }.to_str().unwrap();
    assert!(client.split(' ').any(|e| e == "EGL_MESA_platform_surfaceless"));
    assert!(client.split(' ').any(|e| e == "EGL_EXT_platform_base"));
    let d = display();
    // SAFETY: as above.
    let ext = unsafe { CStr::from_ptr(eglQueryString(d, EGL_EXTENSIONS)) }.to_str().unwrap();
    assert!(ext.split(' ').any(|e| e == "EGL_KHR_surfaceless_context"));
    assert_eq!(eglGetDisplay(std::ptr::null_mut()), d);
    // The fewest buffers first: no depth buffer, as none was asked for.
    let config = webkit_config(d);
    let mut v = 0;
    for (attribute, want) in [(EGL_STENCIL_SIZE, 8), (EGL_ALPHA_SIZE, 8), (EGL_DEPTH_SIZE, 0), (EGL_SAMPLES, 0)] {
        // SAFETY: room for the value.
        assert_eq!(unsafe { eglGetConfigAttrib(d, config, attribute, &mut v) }, EGL_TRUE);
        assert_eq!(v, want, "attribute {attribute:#x}");
    }
    // Windows are no configuration's: none matches the default surface
    // type.
    let mut n = -1;
    let attributes = [EGL_RENDERABLE_TYPE, EGL_OPENGL_ES2_BIT, EGL_NONE];
    // SAFETY: a list ending with EGL_NONE; no room asked for.
    unsafe { eglChooseConfig(d, attributes.as_ptr(), std::ptr::null_mut(), 0, &mut n) };
    assert_eq!(n, 0);
    // A context of OpenGL ES 1 is none of ours.
    let attributes = [EGL_CONTEXT_MAJOR_VERSION, 1, EGL_NONE];
    // SAFETY: a list ending with EGL_NONE.
    let c = unsafe { eglCreateContext(d, config, std::ptr::null_mut(), attributes.as_ptr()) };
    assert!(c.is_null());
    assert_eq!(eglGetError(), EGL_BAD_MATCH);
}

#[test]
fn a_surfaceless_context_draws_into_framebuffer_objects() {
    let d = display();
    let config = webkit_config(d);
    let c = context(d, config, std::ptr::null_mut());
    assert_eq!(eglMakeCurrent(d, std::ptr::null_mut(), std::ptr::null_mut(), c), EGL_TRUE);
    assert_eq!(eglGetCurrentContext(), c);
    // SAFETY: OpenGL ES's strings are NUL-terminated.
    let version = unsafe { CStr::from_ptr(glGetString(gl::VERSION).cast()) }.to_str().unwrap();
    assert!(version.starts_with("OpenGL ES 3.0"), "{version}");
    assert_eq!(glCheckFramebufferStatus(gl::FRAMEBUFFER), gl::FRAMEBUFFER_UNDEFINED);
    framebuffer();
    glViewport(0, 0, 2, 2);
    glClearColor(1.0, 0.5, 0.0, 1.0);
    glClear(gl::COLOR_BUFFER_BIT);
    assert_eq!(pixel_bgra(), [0, 128, 255, 255]);
    let mut v = [0; 4];
    // SAFETY: room for the viewport's four values.
    unsafe { glGetIntegerv(gl::VIEWPORT, v.as_mut_ptr()) };
    assert_eq!(v, [0, 0, 2, 2]);
    assert_eq!(glGetError(), gl::NO_ERROR);
    // Client-side arrays are not taken.
    let data = [0.0f32; 4];
    glVertexAttribPointer(0, 2, gl::FLOAT, 0, 0, data.as_ptr().cast());
    assert_eq!(glGetError(), gl::INVALID_OPERATION);
    assert_eq!(eglMakeCurrent(d, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()), EGL_TRUE);
    assert!(eglGetCurrentContext().is_null());
    // Without a context, calls do nothing.
    assert_eq!(glGetError(), gl::NO_ERROR);
    assert!(glGetString(gl::VERSION).is_null());
    assert_eq!(eglDestroyContext(d, c), EGL_TRUE);
}

#[test]
fn contexts_share_objects_across_threads() {
    let d = display();
    let config = webkit_config(d);
    let a = context(d, config, std::ptr::null_mut());
    let b = context(d, config, a);
    assert_eq!(eglMakeCurrent(d, std::ptr::null_mut(), std::ptr::null_mut(), a), EGL_TRUE);
    let texture = framebuffer();
    glClearColor(0.0, 0.0, 1.0, 1.0);
    glClear(gl::COLOR_BUFFER_BIT);
    glFinish();
    // Another thread, with the other context, reads the same texture.
    let (a_addr, b_addr, d_addr) = (a as usize, b as usize, d as usize);
    let seen = std::thread::spawn(move || {
        let (a, b, d) = (a_addr as EGLContext, b_addr as EGLContext, d_addr as EGLDisplay);
        // The first context is the other thread's.
        assert_eq!(eglMakeCurrent(d, std::ptr::null_mut(), std::ptr::null_mut(), a), EGL_FALSE);
        assert_eq!(eglGetError(), EGL_BAD_ACCESS);
        assert_eq!(eglMakeCurrent(d, std::ptr::null_mut(), std::ptr::null_mut(), b), EGL_TRUE);
        let mut f = 0;
        // SAFETY: room for one name.
        unsafe { glGenFramebuffers(1, &mut f) };
        glBindFramebuffer(gl::FRAMEBUFFER, f);
        glFramebufferTexture2D(gl::FRAMEBUFFER, gl::COLOR_ATTACHMENT0, gl::TEXTURE_2D, texture, 0);
        let p = pixel_bgra();
        eglReleaseThread();
        p
    })
    .join()
    .unwrap();
    assert_eq!(seen, [255, 0, 0, 255]);
    // The first context's own state is as it left it.
    let mut bound = 0;
    // SAFETY: room for one value.
    unsafe { glGetIntegerv(gl::TEXTURE_BINDING_2D, &mut bound) };
    assert_eq!(bound as GLuint, texture);
    eglReleaseThread();
    eglDestroyContext(d, b);
    eglDestroyContext(d, a);
}

#[test]
fn a_context_without_a_renderer_fails_rather_than_render_in_software() {
    let d = display();
    let config = webkit_config(d);
    let a = context(d, config, std::ptr::null_mut());
    // The GPU gives no context, as in Veda when it is at its limits: one of
    // a share group of its own fails, as WebKit hears, and is not made.
    crate::current::NO_RENDERER.set(true);
    let attributes = [EGL_CONTEXT_MAJOR_VERSION, 2, EGL_NONE];
    // SAFETY: a list ending with EGL_NONE.
    let refused = unsafe { eglCreateContext(d, config, std::ptr::null_mut(), attributes.as_ptr()) };
    assert!(refused.is_null());
    assert_eq!(eglGetError(), EGL_BAD_ALLOC);
    // One that shares a group renders with the group's renderer, which it
    // has already.
    let b = context(d, config, a);
    assert_eq!(eglMakeCurrent(d, std::ptr::null_mut(), std::ptr::null_mut(), b), EGL_TRUE);
    framebuffer();
    glClearColor(0.0, 1.0, 0.0, 1.0);
    glClear(gl::COLOR_BUFFER_BIT);
    assert_eq!(pixel_bgra(), [0, 255, 0, 255]);
    eglReleaseThread();
    // With the renderer back, so are contexts of groups of their own.
    crate::current::NO_RENDERER.set(false);
    let c = context(d, config, std::ptr::null_mut());
    for context in [c, b, a] {
        assert_eq!(eglDestroyContext(d, context), EGL_TRUE);
    }
}

#[test]
fn fences_signal_once_the_work_is_done() {
    let d = display();
    let config = webkit_config(d);
    let c = context(d, config, std::ptr::null_mut());
    // No context, no fence.
    // SAFETY: no attributes.
    assert!(unsafe { eglCreateSync(d, EGL_SYNC_FENCE, std::ptr::null()) }.is_null());
    assert_eq!(eglGetError(), EGL_BAD_MATCH);
    eglMakeCurrent(d, std::ptr::null_mut(), std::ptr::null_mut(), c);
    framebuffer();
    glClear(gl::COLOR_BUFFER_BIT);
    // SAFETY: no attributes.
    let sync = unsafe { eglCreateSync(d, EGL_SYNC_FENCE, std::ptr::null()) };
    assert!(!sync.is_null());
    assert_eq!(eglClientWaitSync(d, sync, EGL_SYNC_FLUSH_COMMANDS_BIT, EGL_FOREVER), EGL_CONDITION_SATISFIED);
    let mut status = 0;
    // SAFETY: room for the value.
    unsafe { eglGetSyncAttribKHR(d, sync, EGL_SYNC_STATUS, &mut status) };
    assert_eq!(status, EGL_SIGNALED);
    assert_eq!(eglDestroySync(d, sync), EGL_TRUE);
    assert_eq!(eglDestroySync(d, sync), EGL_FALSE);
    eglReleaseThread();
    eglDestroyContext(d, c);
}

#[test]
fn a_pbuffer_is_the_default_framebuffer() {
    let d = display();
    let config = webkit_config(d);
    let attributes = [EGL_WIDTH, 3, EGL_HEIGHT, 2, EGL_NONE];
    // SAFETY: a list ending with EGL_NONE.
    let s = unsafe { eglCreatePbufferSurface(d, config, attributes.as_ptr()) };
    assert!(!s.is_null());
    let c = context(d, config, std::ptr::null_mut());
    assert_eq!(eglMakeCurrent(d, s, s, c), EGL_TRUE);
    assert_eq!(eglGetCurrentSurface(EGL_DRAW), s);
    assert_eq!(glCheckFramebufferStatus(gl::FRAMEBUFFER), gl::FRAMEBUFFER_COMPLETE);
    // Its first time: the viewport is the surface's.
    let mut v = [0; 4];
    // SAFETY: room for the viewport's four values.
    unsafe { glGetIntegerv(gl::VIEWPORT, v.as_mut_ptr()) };
    assert_eq!(v, [0, 0, 3, 2]);
    glClearColor(0.0, 1.0, 0.0, 1.0);
    glClear(gl::COLOR_BUFFER_BIT);
    assert_eq!(pixel_bgra(), [0, 255, 0, 255]);
    assert_eq!(eglSwapBuffers(d, s), EGL_TRUE);
    let mut w = 0;
    // SAFETY: room for the value.
    unsafe { eglQuerySurface(d, s, EGL_WIDTH, &mut w) };
    assert_eq!(w, 3);
    eglReleaseThread();
    assert_eq!(eglDestroySurface(d, s), EGL_TRUE);
    eglDestroyContext(d, c);
}

#[test]
fn every_function_is_found_by_its_name() {
    let find = |name: &CStr| {
        // SAFETY: a NUL-terminated name.
        unsafe { eglGetProcAddress(name.as_ptr()) }
    };
    assert_eq!(find(c"glClear"), glClear as *const c_void);
    assert_eq!(find(c"eglGetProcAddress"), eglGetProcAddress as *const c_void);
    assert_eq!(find(c"glBindVertexArrayOES"), glBindVertexArray as *const c_void);
    assert!(find(c"glNotAFunction").is_null());
    for name in crate::procs::names() {
        let name = std::ffi::CString::new(name).unwrap();
        assert!(!find(&name).is_null(), "{name:?}");
    }
    // OpenGL ES 3.0's 246 functions, EGL 1.5's 34 and extensions'.
    assert!(crate::procs::names().count() > 300);
}

#[test]
fn buffers_take_their_bytes_and_offsets() {
    let d = display();
    let config = webkit_config(d);
    let c = context(d, config, std::ptr::null_mut());
    eglMakeCurrent(d, std::ptr::null_mut(), std::ptr::null_mut(), c);
    let mut b = 0;
    // SAFETY: room for one name; 8 bytes of data; none.
    unsafe {
        glGenBuffers(1, &mut b);
        glBindBuffer(gl::ARRAY_BUFFER, b);
        glBufferData(gl::ARRAY_BUFFER, 8, [1u8, 2, 3, 4, 5, 6, 7, 8].as_ptr().cast(), gl::STATIC_DRAW);
        glBufferData(gl::ARRAY_BUFFER, -1, std::ptr::null(), gl::STATIC_DRAW);
    }
    assert_eq!(glGetError(), gl::INVALID_VALUE);
    let p = glMapBufferRange(gl::ARRAY_BUFFER, 2, 4, gl::MAP_READ_BIT);
    assert!(!p.is_null());
    // SAFETY: the mapping holds 4 bytes until the buffer is unmapped.
    assert_eq!(unsafe { std::slice::from_raw_parts(p.cast::<u8>(), 4) }, [3, 4, 5, 6]);
    assert_eq!(glUnmapBuffer(gl::ARRAY_BUFFER), 1);
    // An offset into a bound buffer is no client-side array.
    glVertexAttribPointer(0, 2, gl::FLOAT, 0, 0, std::ptr::null::<c_void>());
    assert_eq!(glGetError(), gl::NO_ERROR);
    eglReleaseThread();
    eglDestroyContext(d, c);
}
