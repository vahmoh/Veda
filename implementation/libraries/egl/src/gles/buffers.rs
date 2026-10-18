// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Buffer objects: their names, data, mapping and indexed bindings.

use std::ffi::c_void;

use gl::gl;

use super::{count, gl_boolean, put, slice, slice_mut};
use crate::current::{with_gl, with_gl_or};
use crate::*;

/// `glGenBuffers`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGenBuffers(n: GLsizei, buffers: *mut GLuint) {
    with_gl(|c| match count(n) {
        // SAFETY: the caller has room for `n` names.
        Some(n) => c.gen_buffers(unsafe { slice_mut(buffers, n) }),
        None => c.record_error(gl::INVALID_VALUE),
    });
}

/// `glDeleteBuffers`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glDeleteBuffers(n: GLsizei, buffers: *const GLuint) {
    with_gl(|c| match count(n) {
        // SAFETY: the caller gives `n` names.
        Some(n) => c.delete_buffers(unsafe { slice(buffers, n) }),
        None => c.record_error(gl::INVALID_VALUE),
    });
}

/// `glIsBuffer`.
#[unsafe(no_mangle)]
pub extern "C" fn glIsBuffer(buffer: GLuint) -> GLboolean {
    gl_boolean(with_gl(|c| c.is_buffer(buffer)))
}

/// `glBindBuffer`.
#[unsafe(no_mangle)]
pub extern "C" fn glBindBuffer(target: GLenum, buffer: GLuint) {
    with_gl(|c| c.bind_buffer(target, buffer));
}

/// `glBufferData`: `size` bytes of `data`, or of zeros for none.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glBufferData(target: GLenum, size: GLsizeiptr, data: *const c_void, usage: GLenum) {
    with_gl(|c| match usize::try_from(size) {
        Ok(size) if data.is_null() => c.buffer_data_size(target, size, usage),
        // SAFETY: the caller gives `size` bytes.
        Ok(size) => c.buffer_data(target, unsafe { slice(data.cast::<u8>(), size) }, usage),
        Err(_) => c.record_error(gl::INVALID_VALUE),
    });
}

/// `glBufferSubData`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glBufferSubData(target: GLenum, offset: GLintptr, size: GLsizeiptr, data: *const c_void) {
    with_gl(|c| match (usize::try_from(offset), usize::try_from(size)) {
        (Ok(_), Ok(size)) if data.is_null() && size > 0 => c.record_error(gl::INVALID_VALUE),
        // SAFETY: the caller gives `size` bytes.
        (Ok(offset), Ok(size)) => c.buffer_sub_data(target, offset, unsafe { slice(data.cast::<u8>(), size) }),
        _ => c.record_error(gl::INVALID_VALUE),
    });
}

/// `glCopyBufferSubData`.
#[unsafe(no_mangle)]
pub extern "C" fn glCopyBufferSubData(
    read_target: GLenum,
    write_target: GLenum,
    read_offset: GLintptr,
    write_offset: GLintptr,
    size: GLsizeiptr,
) {
    with_gl(|c| match (usize::try_from(read_offset), usize::try_from(write_offset), usize::try_from(size)) {
        (Ok(r), Ok(w), Ok(n)) => c.copy_buffer_sub_data(read_target, write_target, r, w, n),
        _ => c.record_error(gl::INVALID_VALUE),
    });
}

/// `glMapBufferRange`: the mapping is the context's memory, valid until
/// the buffer is unmapped.
#[unsafe(no_mangle)]
pub extern "C" fn glMapBufferRange(
    target: GLenum,
    offset: GLintptr,
    length: GLsizeiptr,
    access: GLbitfield,
) -> *mut c_void {
    with_gl_or(std::ptr::null_mut(), |c| match (usize::try_from(offset), usize::try_from(length)) {
        (Ok(o), Ok(l)) => {
            c.map_buffer_range(target, o, l, access).map_or(std::ptr::null_mut(), |m| m.as_mut_ptr().cast())
        }
        _ => {
            c.record_error(gl::INVALID_VALUE);
            std::ptr::null_mut()
        }
    })
}

/// `glFlushMappedBufferRange`.
#[unsafe(no_mangle)]
pub extern "C" fn glFlushMappedBufferRange(target: GLenum, offset: GLintptr, length: GLsizeiptr) {
    with_gl(|c| match (usize::try_from(offset), usize::try_from(length)) {
        (Ok(o), Ok(l)) => c.flush_mapped_buffer_range(target, o, l),
        _ => c.record_error(gl::INVALID_VALUE),
    });
}

/// `glUnmapBuffer`.
#[unsafe(no_mangle)]
pub extern "C" fn glUnmapBuffer(target: GLenum) -> GLboolean {
    gl_boolean(with_gl(|c| c.unmap_buffer(target)))
}

/// `glGetBufferPointerv`: a mapped buffer's mapping.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGetBufferPointerv(target: GLenum, pname: GLenum, params: *mut *mut c_void) {
    let p = with_gl_or(std::ptr::null_mut(), |c| {
        if pname != gl::BUFFER_MAP_POINTER {
            c.record_error(gl::INVALID_ENUM);
            return std::ptr::null_mut();
        }
        c.mapped_buffer(target).map_or(std::ptr::null_mut(), |m| m.as_mut_ptr().cast())
    });
    // SAFETY: the caller has room for the pointer.
    unsafe { put(params, p) };
}

/// `glGetBufferParameteriv`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGetBufferParameteriv(target: GLenum, pname: GLenum, params: *mut GLint) {
    let v = with_gl(|c| c.get_buffer_parameteri(target, pname));
    // SAFETY: the caller has room for the value.
    unsafe { put(params, v) };
}

/// `glGetBufferParameteri64v`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGetBufferParameteri64v(target: GLenum, pname: GLenum, params: *mut GLint64) {
    let v = with_gl(|c| c.get_buffer_parameteri64(target, pname));
    // SAFETY: the caller has room for the value.
    unsafe { put(params, v) };
}

/// `glBindBufferBase`.
#[unsafe(no_mangle)]
pub extern "C" fn glBindBufferBase(target: GLenum, index: GLuint, buffer: GLuint) {
    with_gl(|c| c.bind_buffer_base(target, index, buffer));
}

/// `glBindBufferRange`.
#[unsafe(no_mangle)]
pub extern "C" fn glBindBufferRange(target: GLenum, index: GLuint, buffer: GLuint, offset: GLintptr, size: GLsizeiptr) {
    with_gl(|c| match (usize::try_from(offset), usize::try_from(size)) {
        (Ok(o), Ok(s)) => c.bind_buffer_range(target, index, buffer, o, s),
        _ => c.record_error(gl::INVALID_VALUE),
    });
}
