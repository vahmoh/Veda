// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Vertex arrays, vertex attributes and drawing. Vertex and index data come
//! from buffer objects: a pointer into the program's memory instead (a
//! client-side array) is refused with `GL_INVALID_OPERATION`.

use std::ffi::c_void;

use gl::gl;

use super::{boolean, count, gl_boolean, put, put_all, slice, slice_mut};
use crate::current::with_gl;
use crate::*;

/// `glGenVertexArrays`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGenVertexArrays(n: GLsizei, arrays: *mut GLuint) {
    with_gl(|c| match count(n) {
        // SAFETY: the caller has room for `n` names.
        Some(n) => c.gen_vertex_arrays(unsafe { slice_mut(arrays, n) }),
        None => c.record_error(gl::INVALID_VALUE),
    });
}

/// `glDeleteVertexArrays`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glDeleteVertexArrays(n: GLsizei, arrays: *const GLuint) {
    with_gl(|c| match count(n) {
        // SAFETY: the caller gives `n` names.
        Some(n) => c.delete_vertex_arrays(unsafe { slice(arrays, n) }),
        None => c.record_error(gl::INVALID_VALUE),
    });
}

/// `glIsVertexArray`.
#[unsafe(no_mangle)]
pub extern "C" fn glIsVertexArray(array: GLuint) -> GLboolean {
    gl_boolean(with_gl(|c| c.is_vertex_array(array)))
}

/// `glBindVertexArray`.
#[unsafe(no_mangle)]
pub extern "C" fn glBindVertexArray(array: GLuint) {
    with_gl(|c| c.bind_vertex_array(array));
}

/// `glVertexAttribPointer`: `pointer` is an offset into the bound array
/// buffer.
#[unsafe(no_mangle)]
pub extern "C" fn glVertexAttribPointer(
    index: GLuint,
    size: GLint,
    ty: GLenum,
    normalized: GLboolean,
    stride: GLsizei,
    pointer: *const c_void,
) {
    with_gl(|c| {
        if c.get_integer(gl::ARRAY_BUFFER_BINDING) == 0 && !pointer.is_null() {
            return c.record_error(gl::INVALID_OPERATION);
        }
        c.vertex_attrib_pointer(index, size, ty, boolean(normalized), stride, pointer as usize);
    });
}

/// `glVertexAttribIPointer`.
#[unsafe(no_mangle)]
pub extern "C" fn glVertexAttribIPointer(
    index: GLuint,
    size: GLint,
    ty: GLenum,
    stride: GLsizei,
    pointer: *const c_void,
) {
    with_gl(|c| {
        if c.get_integer(gl::ARRAY_BUFFER_BINDING) == 0 && !pointer.is_null() {
            return c.record_error(gl::INVALID_OPERATION);
        }
        c.vertex_attrib_i_pointer(index, size, ty, stride, pointer as usize);
    });
}

/// `glEnableVertexAttribArray`.
#[unsafe(no_mangle)]
pub extern "C" fn glEnableVertexAttribArray(index: GLuint) {
    with_gl(|c| c.enable_vertex_attrib_array(index));
}

/// `glDisableVertexAttribArray`.
#[unsafe(no_mangle)]
pub extern "C" fn glDisableVertexAttribArray(index: GLuint) {
    with_gl(|c| c.disable_vertex_attrib_array(index));
}

/// `glVertexAttribDivisor`.
#[unsafe(no_mangle)]
pub extern "C" fn glVertexAttribDivisor(index: GLuint, divisor: GLuint) {
    with_gl(|c| c.vertex_attrib_divisor(index, divisor));
}

/// `glVertexAttrib1f`.
#[unsafe(no_mangle)]
pub extern "C" fn glVertexAttrib1f(index: GLuint, x: GLfloat) {
    with_gl(|c| c.vertex_attrib1f(index, x));
}

/// `glVertexAttrib2f`.
#[unsafe(no_mangle)]
pub extern "C" fn glVertexAttrib2f(index: GLuint, x: GLfloat, y: GLfloat) {
    with_gl(|c| c.vertex_attrib2f(index, x, y));
}

/// `glVertexAttrib3f`.
#[unsafe(no_mangle)]
pub extern "C" fn glVertexAttrib3f(index: GLuint, x: GLfloat, y: GLfloat, z: GLfloat) {
    with_gl(|c| c.vertex_attrib3f(index, x, y, z));
}

/// `glVertexAttrib4f`.
#[unsafe(no_mangle)]
pub extern "C" fn glVertexAttrib4f(index: GLuint, x: GLfloat, y: GLfloat, z: GLfloat, w: GLfloat) {
    with_gl(|c| c.vertex_attrib4f(index, x, y, z, w));
}

/// `glVertexAttrib{1,2,3,4}fv`: `n` values.
///
/// # Safety
/// `v` points at `n` values.
unsafe fn vertex_attribfv(index: GLuint, v: *const GLfloat, n: usize) {
    // SAFETY: as the caller says.
    let v = unsafe { slice(v, n) };
    with_gl(|c| if v.len() == n { c.vertex_attribfv(index, v) } else { c.record_error(gl::INVALID_VALUE) });
}

/// `glVertexAttrib1fv`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glVertexAttrib1fv(index: GLuint, v: *const GLfloat) {
    // SAFETY: forwarded from the caller.
    unsafe { vertex_attribfv(index, v, 1) };
}

/// `glVertexAttrib2fv`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glVertexAttrib2fv(index: GLuint, v: *const GLfloat) {
    // SAFETY: forwarded from the caller.
    unsafe { vertex_attribfv(index, v, 2) };
}

/// `glVertexAttrib3fv`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glVertexAttrib3fv(index: GLuint, v: *const GLfloat) {
    // SAFETY: forwarded from the caller.
    unsafe { vertex_attribfv(index, v, 3) };
}

/// `glVertexAttrib4fv`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glVertexAttrib4fv(index: GLuint, v: *const GLfloat) {
    // SAFETY: forwarded from the caller.
    unsafe { vertex_attribfv(index, v, 4) };
}

/// `glVertexAttribI4i`.
#[unsafe(no_mangle)]
pub extern "C" fn glVertexAttribI4i(index: GLuint, x: GLint, y: GLint, z: GLint, w: GLint) {
    with_gl(|c| c.vertex_attrib_i4i(index, x, y, z, w));
}

/// `glVertexAttribI4ui`.
#[unsafe(no_mangle)]
pub extern "C" fn glVertexAttribI4ui(index: GLuint, x: GLuint, y: GLuint, z: GLuint, w: GLuint) {
    with_gl(|c| c.vertex_attrib_i4ui(index, x, y, z, w));
}

/// `glVertexAttribI4iv`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glVertexAttribI4iv(index: GLuint, v: *const GLint) {
    // SAFETY: the caller gives 4 values.
    let Ok(v) = <&[GLint; 4]>::try_from(unsafe { slice(v, 4) }) else { return };
    with_gl(|c| c.vertex_attrib_i4iv(index, v));
}

/// `glVertexAttribI4uiv`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glVertexAttribI4uiv(index: GLuint, v: *const GLuint) {
    // SAFETY: the caller gives 4 values.
    let Ok(v) = <&[GLuint; 4]>::try_from(unsafe { slice(v, 4) }) else { return };
    with_gl(|c| c.vertex_attrib_i4uiv(index, v));
}

/// How many values `glGetVertexAttrib*v` gives for `pname`.
fn attrib_values(pname: GLenum) -> usize {
    if pname == gl::CURRENT_VERTEX_ATTRIB { 4 } else { 1 }
}

/// `glGetVertexAttribfv`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGetVertexAttribfv(index: GLuint, pname: GLenum, params: *mut GLfloat) {
    let mut v = [0.0; 4];
    with_gl(|c| c.get_vertex_attribfv(index, pname, &mut v));
    // SAFETY: the caller has room for the values.
    unsafe { put_all(&v[..attrib_values(pname)], params) };
}

/// `glGetVertexAttribiv`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGetVertexAttribiv(index: GLuint, pname: GLenum, params: *mut GLint) {
    let mut v = [0; 4];
    with_gl(|c| c.get_vertex_attribiv(index, pname, &mut v));
    // SAFETY: the caller has room for the values.
    unsafe { put_all(&v[..attrib_values(pname)], params) };
}

/// `glGetVertexAttribIiv`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGetVertexAttribIiv(index: GLuint, pname: GLenum, params: *mut GLint) {
    let mut v = [0; 4];
    with_gl(|c| c.get_vertex_attrib_iiv(index, pname, &mut v));
    // SAFETY: the caller has room for the values.
    unsafe { put_all(&v[..attrib_values(pname)], params) };
}

/// `glGetVertexAttribIuiv`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGetVertexAttribIuiv(index: GLuint, pname: GLenum, params: *mut GLuint) {
    let mut v = [0; 4];
    with_gl(|c| c.get_vertex_attrib_iuiv(index, pname, &mut v));
    // SAFETY: the caller has room for the values.
    unsafe { put_all(&v[..attrib_values(pname)], params) };
}

/// `glGetVertexAttribPointerv`: the offset, as a pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGetVertexAttribPointerv(index: GLuint, pname: GLenum, pointer: *mut *mut c_void) {
    let offset = with_gl(|c| c.get_vertex_attrib_pointerv(index, pname));
    // SAFETY: the caller has room for the pointer.
    unsafe { put(pointer, offset as *mut c_void) };
}

/// `glDrawArrays`.
#[unsafe(no_mangle)]
pub extern "C" fn glDrawArrays(mode: GLenum, first: GLint, count: GLsizei) {
    with_gl(|c| c.draw_arrays(mode, first, count));
}

/// `glDrawArraysInstanced`.
#[unsafe(no_mangle)]
pub extern "C" fn glDrawArraysInstanced(mode: GLenum, first: GLint, count: GLsizei, instances: GLsizei) {
    with_gl(|c| c.draw_arrays_instanced(mode, first, count, instances));
}

/// Whether the indices of a draw of `count` are in the program's memory
/// (no element array buffer bound), which is refused.
fn client_indices(c: &mut ::gl::Context, count: GLsizei) -> bool {
    count > 0 && c.get_integer(gl::ELEMENT_ARRAY_BUFFER_BINDING) == 0
}

/// `glDrawElements`: `indices` is an offset into the bound element array
/// buffer.
#[unsafe(no_mangle)]
pub extern "C" fn glDrawElements(mode: GLenum, count: GLsizei, ty: GLenum, indices: *const c_void) {
    with_gl(|c| {
        if client_indices(c, count) {
            return c.record_error(gl::INVALID_OPERATION);
        }
        c.draw_elements(mode, count, ty, indices as usize);
    });
}

/// `glDrawElementsInstanced`.
#[unsafe(no_mangle)]
pub extern "C" fn glDrawElementsInstanced(
    mode: GLenum,
    count: GLsizei,
    ty: GLenum,
    indices: *const c_void,
    instances: GLsizei,
) {
    with_gl(|c| {
        if client_indices(c, count) {
            return c.record_error(gl::INVALID_OPERATION);
        }
        c.draw_elements_instanced(mode, count, ty, indices as usize, instances);
    });
}

/// `glDrawRangeElements`.
#[unsafe(no_mangle)]
pub extern "C" fn glDrawRangeElements(
    mode: GLenum,
    start: GLuint,
    end: GLuint,
    count: GLsizei,
    ty: GLenum,
    indices: *const c_void,
) {
    with_gl(|c| {
        if client_indices(c, count) {
            return c.record_error(gl::INVALID_OPERATION);
        }
        c.draw_range_elements(mode, start, end, count, ty, indices as usize);
    });
}
