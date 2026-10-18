// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `egl` — EGL 1.5 and OpenGL ES 3.0 for C programs: their C functions over
//! [`gl`], Veda's OpenGL ES.
//!
//! * EGL has one display, the surfaceless platform's
//!   (`EGL_MESA_platform_surfaceless`), which is the default display too:
//!   contexts draw into framebuffer objects (`EGL_KHR_surfaceless_context`)
//!   or into pbuffers. There are no window surfaces yet.
//! * A context is OpenGL ES 3.0, whether 2.0 or 3.0 is asked for (3.0 is
//!   2.0 and more, as EGL allows). Contexts that share objects
//!   (`eglCreateContext`'s `share_context`) are one [`gl::Context`] with a
//!   [`gl::Local`] for each, behind a lock: their calls, from any threads,
//!   take turns ([`current`]).
//! * Each thread has its current context (`eglMakeCurrent`); a GL call
//!   without one does nothing (and returns 0).
//! * `eglGetProcAddress` finds every function by its name, core ones too
//!   (`EGL_KHR_get_all_proc_addresses`): how libepoxy finds them in a
//!   program that links this library, as every program on Veda links its
//!   libraries ([`procs`]).
//! * In Veda, a share group renders on the GPU (virgl, through the `gpu`
//!   service) if there is one, and on a worker thread per processor
//!   otherwise; elsewhere (the host's tests) on the processor.
//!
//! What OpenGL ES's C interface has that `gl` does not take: vertex and
//! index data in the program's memory (client-side arrays), which fail with
//! `GL_INVALID_OPERATION` as they do with a vertex array object bound. A
//! pbuffer's pixels are each context's own.
//!
//! The functions are called from C, with the pointers the EGL and OpenGL
//! ES specifications describe: what they read and write is the caller's to
//! make valid, as in C.
//!
//! Unlike Veda's other libraries, `egl` is on the standard library, as the
//! programs whose C code calls it are (POSIX programs), and keeps each
//! thread's current context in a thread-local.

pub mod current;
pub mod egl;
pub mod gles;
pub mod procs;

/// The GPU's renderer, if contexts made now render on one ([`current`]);
/// otherwise they render in software, which a program may rather not.
pub fn gpu_renderer() -> Option<String> {
    #[cfg(target_os = "veda")]
    return gl::veda::gpu_renderer();
    #[cfg(not(target_os = "veda"))]
    None
}

#[cfg(test)]
mod tests;

use core::ffi::c_void;

// EGL's types (EGL/egl.h, EGL/eglplatform.h).
pub type EGLBoolean = u32;
pub type EGLint = i32;
pub type EGLenum = u32;
pub type EGLAttrib = isize;
pub type EGLTime = u64;
pub type EGLDisplay = *mut c_void;
pub type EGLConfig = *mut c_void;
pub type EGLContext = *mut c_void;
pub type EGLSurface = *mut c_void;
pub type EGLSync = *mut c_void;
pub type EGLImage = *mut c_void;
pub type EGLClientBuffer = *mut c_void;
pub type EGLNativeDisplayType = *mut c_void;
pub type EGLNativeWindowType = *mut c_void;
pub type EGLNativePixmapType = *mut c_void;

// OpenGL ES's (GLES3/gl3.h).
pub type GLenum = u32;
pub type GLboolean = u8;
pub type GLbitfield = u32;
pub type GLbyte = i8;
pub type GLubyte = u8;
pub type GLshort = i16;
pub type GLushort = u16;
pub type GLint = i32;
pub type GLuint = u32;
pub type GLsizei = i32;
pub type GLfloat = f32;
pub type GLintptr = isize;
pub type GLsizeiptr = isize;
pub type GLint64 = i64;
pub type GLuint64 = u64;
pub type GLchar = core::ffi::c_char;
pub type GLsync = *mut c_void;
