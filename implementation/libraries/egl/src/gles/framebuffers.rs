// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Framebuffers and renderbuffers, clearing, reading pixels and blitting.

use std::ffi::c_void;

use ::gl::pixels::PixelsMut;
use gl::gl;

use super::{count, gl_boolean, put, slice, slice_mut};
use crate::current::with_gl;
use crate::*;

/// `glGenFramebuffers`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGenFramebuffers(n: GLsizei, framebuffers: *mut GLuint) {
    with_gl(|c| match count(n) {
        // SAFETY: the caller has room for `n` names.
        Some(n) => c.gen_framebuffers(unsafe { slice_mut(framebuffers, n) }),
        None => c.record_error(gl::INVALID_VALUE),
    });
}

/// `glDeleteFramebuffers`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glDeleteFramebuffers(n: GLsizei, framebuffers: *const GLuint) {
    with_gl(|c| match count(n) {
        // SAFETY: the caller gives `n` names.
        Some(n) => c.delete_framebuffers(unsafe { slice(framebuffers, n) }),
        None => c.record_error(gl::INVALID_VALUE),
    });
}

/// `glIsFramebuffer`.
#[unsafe(no_mangle)]
pub extern "C" fn glIsFramebuffer(framebuffer: GLuint) -> GLboolean {
    gl_boolean(with_gl(|c| c.is_framebuffer(framebuffer)))
}

/// `glBindFramebuffer`.
#[unsafe(no_mangle)]
pub extern "C" fn glBindFramebuffer(target: GLenum, framebuffer: GLuint) {
    with_gl(|c| c.bind_framebuffer(target, framebuffer));
}

/// `glCheckFramebufferStatus`.
#[unsafe(no_mangle)]
pub extern "C" fn glCheckFramebufferStatus(target: GLenum) -> GLenum {
    with_gl(|c| c.check_framebuffer_status(target))
}

/// `glFramebufferTexture2D`.
#[unsafe(no_mangle)]
pub extern "C" fn glFramebufferTexture2D(
    target: GLenum,
    attachment: GLenum,
    textarget: GLenum,
    texture: GLuint,
    level: GLint,
) {
    with_gl(|c| c.framebuffer_texture_2d(target, attachment, textarget, texture, level));
}

/// `glFramebufferTextureLayer`.
#[unsafe(no_mangle)]
pub extern "C" fn glFramebufferTextureLayer(
    target: GLenum,
    attachment: GLenum,
    texture: GLuint,
    level: GLint,
    layer: GLint,
) {
    with_gl(|c| c.framebuffer_texture_layer(target, attachment, texture, level, layer));
}

/// `glFramebufferRenderbuffer`.
#[unsafe(no_mangle)]
pub extern "C" fn glFramebufferRenderbuffer(
    target: GLenum,
    attachment: GLenum,
    renderbuffertarget: GLenum,
    renderbuffer: GLuint,
) {
    with_gl(|c| c.framebuffer_renderbuffer(target, attachment, renderbuffertarget, renderbuffer));
}

/// `glGetFramebufferAttachmentParameteriv`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGetFramebufferAttachmentParameteriv(
    target: GLenum,
    attachment: GLenum,
    pname: GLenum,
    params: *mut GLint,
) {
    let v = with_gl(|c| c.get_framebuffer_attachment_parameteri(target, attachment, pname));
    // SAFETY: the caller has room for the value.
    unsafe { put(params, v) };
}

/// `glGenRenderbuffers`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGenRenderbuffers(n: GLsizei, renderbuffers: *mut GLuint) {
    with_gl(|c| match count(n) {
        // SAFETY: the caller has room for `n` names.
        Some(n) => c.gen_renderbuffers(unsafe { slice_mut(renderbuffers, n) }),
        None => c.record_error(gl::INVALID_VALUE),
    });
}

/// `glDeleteRenderbuffers`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glDeleteRenderbuffers(n: GLsizei, renderbuffers: *const GLuint) {
    with_gl(|c| match count(n) {
        // SAFETY: the caller gives `n` names.
        Some(n) => c.delete_renderbuffers(unsafe { slice(renderbuffers, n) }),
        None => c.record_error(gl::INVALID_VALUE),
    });
}

/// `glIsRenderbuffer`.
#[unsafe(no_mangle)]
pub extern "C" fn glIsRenderbuffer(renderbuffer: GLuint) -> GLboolean {
    gl_boolean(with_gl(|c| c.is_renderbuffer(renderbuffer)))
}

/// `glBindRenderbuffer`.
#[unsafe(no_mangle)]
pub extern "C" fn glBindRenderbuffer(target: GLenum, renderbuffer: GLuint) {
    with_gl(|c| c.bind_renderbuffer(target, renderbuffer));
}

/// `glRenderbufferStorage`.
#[unsafe(no_mangle)]
pub extern "C" fn glRenderbufferStorage(target: GLenum, internalformat: GLenum, width: GLsizei, height: GLsizei) {
    with_gl(|c| c.renderbuffer_storage(target, internalformat, width, height));
}

/// `glRenderbufferStorageMultisample`.
#[unsafe(no_mangle)]
pub extern "C" fn glRenderbufferStorageMultisample(
    target: GLenum,
    samples: GLsizei,
    internalformat: GLenum,
    width: GLsizei,
    height: GLsizei,
) {
    with_gl(|c| c.renderbuffer_storage_multisample(target, samples, internalformat, width, height));
}

/// `glRenderbufferStorageMemoryFileVEDA`, Veda's own (found by
/// `eglGetProcAddress`): makes the bound renderbuffer's storage the memory
/// file on `fd` (`memfd_create`), which the GPU then renders into in place,
/// as a renderbuffer of an EGL image of a window's buffer would be: a
/// picture of `width` x `height` pixels of `internalformat` (`GL_RGBA8`, or
/// `GL_RGB8`, whose pixels' fourth byte is never read) from the file's
/// start, rows `stride` bytes apart, each pixel's bytes in `format`'s order
/// (`GL_BGRA_EXT`, blue first, as Veda's pictures and WebKit's buffers
/// are, or `GL_RGBA`). So WebKit renders a page into the memory the window
/// system shows it from, rather than reading each frame back. Once a fence
/// after the drawing has signaled, the file holds what was drawn. The
/// descriptor stays the caller's.
///
/// Fails with `GL_INVALID_OPERATION` where the GPU cannot take such memory
/// (contexts in software, virgl's host under QEMU, a GPU whose caches are
/// not the processor's), `GL_OUT_OF_MEMORY` if not now (past the
/// renderer's limits), and `GL_INVALID_VALUE` for a descriptor of no memory
/// file or a picture larger than the file.
#[unsafe(no_mangle)]
pub extern "C" fn glRenderbufferStorageMemoryFileVEDA(
    target: GLenum,
    internalformat: GLenum,
    width: GLsizei,
    height: GLsizei,
    fd: GLint,
    format: GLenum,
    stride: GLsizei,
) {
    with_gl(|c| {
        let bgr = match format {
            gl::BGRA_EXT => true,
            gl::RGBA => false,
            _ => return c.record_error(gl::INVALID_ENUM),
        };
        let Ok(stride) = u32::try_from(stride) else { return c.record_error(gl::INVALID_VALUE) };
        match memory_file(fd) {
            // The renderer takes a handle of its own (`External::handle`).
            Some(file) => {
                let memory = ::gl::backend::External::handle(file.handle, file.size, stride, bgr);
                c.renderbuffer_storage_external(target, internalformat, width, height, &memory);
            }
            None => c.record_error(gl::INVALID_VALUE),
        }
    });
}

/// The memory of a memory file, as `veda_memory_file` gives it: a handle,
/// closed as this goes, and the file's size.
struct MemoryFile {
    handle: u32,
    size: usize,
}

#[cfg(target_os = "veda")]
unsafe extern "C" {
    /// `<veda/memory.h>`: a handle to the memory of the memory file on `fd`.
    fn veda_memory_file(fd: std::ffi::c_int, vmo: *mut u32) -> std::ffi::c_int;
    /// `<veda/ipc.h>`.
    fn veda_close(handle: u32) -> std::ffi::c_int;
}

/// The memory file on `fd`, if it is one (none outside Veda).
#[cfg(target_os = "veda")]
fn memory_file(fd: GLint) -> Option<MemoryFile> {
    use std::os::fd::FromRawFd;
    if fd < 0 {
        return None;
    }
    // SAFETY: the caller's open descriptor, borrowed: never closed here.
    let file = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(fd) });
    let size = file.metadata().ok()?.len();
    let mut handle = 0;
    // SAFETY: room for the handle.
    if unsafe { veda_memory_file(fd, &mut handle) } != 0 {
        return None;
    }
    Some(MemoryFile { handle, size: usize::try_from(size).ok()? })
}

#[cfg(not(target_os = "veda"))]
fn memory_file(_fd: GLint) -> Option<MemoryFile> {
    None
}

#[cfg(target_os = "veda")]
impl Drop for MemoryFile {
    fn drop(&mut self) {
        // SAFETY: the handle `veda_memory_file` gave, closed once.
        unsafe { veda_close(self.handle) };
    }
}

/// `glGetRenderbufferParameteriv`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGetRenderbufferParameteriv(target: GLenum, pname: GLenum, params: *mut GLint) {
    let v = with_gl(|c| c.get_renderbuffer_parameteri(target, pname));
    // SAFETY: the caller has room for the value.
    unsafe { put(params, v) };
}

/// `glGetInternalformativ`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGetInternalformativ(
    target: GLenum,
    internalformat: GLenum,
    pname: GLenum,
    buf_size: GLsizei,
    params: *mut GLint,
) {
    with_gl(|c| match count(buf_size) {
        // SAFETY: the caller has room for `buf_size` values.
        Some(n) => c.get_internalformativ(target, internalformat, pname, unsafe { slice_mut(params, n) }),
        None => c.record_error(gl::INVALID_VALUE),
    });
}

/// `glDrawBuffers`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glDrawBuffers(n: GLsizei, bufs: *const GLenum) {
    with_gl(|c| match count(n) {
        // SAFETY: the caller gives `n` buffers.
        Some(n) => c.draw_buffers(unsafe { slice(bufs, n) }),
        None => c.record_error(gl::INVALID_VALUE),
    });
}

/// `glReadBuffer`.
#[unsafe(no_mangle)]
pub extern "C" fn glReadBuffer(src: GLenum) {
    with_gl(|c| c.read_buffer(src));
}

/// `glInvalidateFramebuffer`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glInvalidateFramebuffer(target: GLenum, n: GLsizei, attachments: *const GLenum) {
    with_gl(|c| match count(n) {
        // SAFETY: the caller gives `n` attachments.
        Some(n) => c.invalidate_framebuffer(target, unsafe { slice(attachments, n) }),
        None => c.record_error(gl::INVALID_VALUE),
    });
}

/// `glInvalidateSubFramebuffer`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glInvalidateSubFramebuffer(
    target: GLenum,
    n: GLsizei,
    attachments: *const GLenum,
    x: GLint,
    y: GLint,
    width: GLsizei,
    height: GLsizei,
) {
    with_gl(|c| match count(n) {
        // SAFETY: the caller gives `n` attachments.
        Some(n) => c.invalidate_sub_framebuffer(target, unsafe { slice(attachments, n) }, x, y, width, height),
        None => c.record_error(gl::INVALID_VALUE),
    });
}

/// `glClear`.
#[unsafe(no_mangle)]
pub extern "C" fn glClear(mask: GLbitfield) {
    with_gl(|c| c.clear(mask));
}

/// How many values `glClearBuffer*v` takes for `buffer`: a color's four, a
/// depth or stencil value.
fn clear_values(buffer: GLenum) -> usize {
    if buffer == gl::COLOR { 4 } else { 1 }
}

/// `glClearBufferiv`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glClearBufferiv(buffer: GLenum, drawbuffer: GLint, value: *const GLint) {
    // SAFETY: the caller gives the buffer's values.
    let v = unsafe { slice(value, clear_values(buffer)) };
    with_gl(|c| c.clear_bufferiv(buffer, drawbuffer, v));
}

/// `glClearBufferuiv`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glClearBufferuiv(buffer: GLenum, drawbuffer: GLint, value: *const GLuint) {
    // SAFETY: the caller gives the buffer's values.
    let v = unsafe { slice(value, clear_values(buffer)) };
    with_gl(|c| c.clear_bufferuiv(buffer, drawbuffer, v));
}

/// `glClearBufferfv`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glClearBufferfv(buffer: GLenum, drawbuffer: GLint, value: *const GLfloat) {
    // SAFETY: the caller gives the buffer's values.
    let v = unsafe { slice(value, clear_values(buffer)) };
    with_gl(|c| c.clear_bufferfv(buffer, drawbuffer, v));
}

/// `glClearBufferfi`.
#[unsafe(no_mangle)]
pub extern "C" fn glClearBufferfi(buffer: GLenum, drawbuffer: GLint, depth: GLfloat, stencil: GLint) {
    with_gl(|c| c.clear_bufferfi(buffer, drawbuffer, depth, stencil));
}

/// `glReadPixels`: into the bytes the read writes at `pixels` (as many as
/// the pack storage modes say), or at an offset into the bound pixel pack
/// buffer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glReadPixels(
    x: GLint,
    y: GLint,
    width: GLsizei,
    height: GLsizei,
    format: GLenum,
    ty: GLenum,
    pixels: *mut c_void,
) {
    with_gl(|c| {
        let out = if c.get_integer(gl::PIXEL_PACK_BUFFER_BINDING) != 0 {
            PixelsMut::Offset(pixels as usize)
        } else {
            let n = c.pack_size(width, height, format, ty).unwrap_or(0);
            // SAFETY: the caller has room for the pixels the read writes.
            PixelsMut::Data(unsafe { slice_mut(pixels.cast::<u8>(), n) })
        };
        c.read_pixels(x, y, width, height, format, ty, out);
    });
}

/// `glBlitFramebuffer`.
#[unsafe(no_mangle)]
pub extern "C" fn glBlitFramebuffer(
    src_x0: GLint,
    src_y0: GLint,
    src_x1: GLint,
    src_y1: GLint,
    dst_x0: GLint,
    dst_y0: GLint,
    dst_x1: GLint,
    dst_y1: GLint,
    mask: GLbitfield,
    filter: GLenum,
) {
    with_gl(|c| c.blit_framebuffer(src_x0, src_y0, src_x1, src_y1, dst_x0, dst_y0, dst_x1, dst_y1, mask, filter));
}
