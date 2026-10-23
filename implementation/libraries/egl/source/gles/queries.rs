// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Queries, sync objects (`GLsync`: the sync's name, as a pointer) and
//! transform feedback.

use gl::gl;

use super::{count, gl_boolean, put, slice, slice_mut};
use crate::current::with_gl;
use crate::*;

/// `glGenQueries`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGenQueries(n: GLsizei, ids: *mut GLuint) {
    with_gl(|c| match count(n) {
        // SAFETY: the caller has room for `n` names.
        Some(n) => c.gen_queries(unsafe { slice_mut(ids, n) }),
        None => c.record_error(gl::INVALID_VALUE),
    });
}

/// `glDeleteQueries`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glDeleteQueries(n: GLsizei, ids: *const GLuint) {
    with_gl(|c| match count(n) {
        // SAFETY: the caller gives `n` names.
        Some(n) => c.delete_queries(unsafe { slice(ids, n) }),
        None => c.record_error(gl::INVALID_VALUE),
    });
}

/// `glIsQuery`.
#[unsafe(no_mangle)]
pub extern "C" fn glIsQuery(id: GLuint) -> GLboolean {
    gl_boolean(with_gl(|c| c.is_query(id)))
}

/// `glBeginQuery`.
#[unsafe(no_mangle)]
pub extern "C" fn glBeginQuery(target: GLenum, id: GLuint) {
    with_gl(|c| c.begin_query(target, id));
}

/// `glEndQuery`.
#[unsafe(no_mangle)]
pub extern "C" fn glEndQuery(target: GLenum) {
    with_gl(|c| c.end_query(target));
}

/// `glGetQueryiv`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGetQueryiv(target: GLenum, pname: GLenum, params: *mut GLint) {
    let v = with_gl(|c| c.get_queryiv(target, pname));
    // SAFETY: the caller has room for the value.
    unsafe { put(params, v) };
}

/// `glGetQueryObjectuiv`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGetQueryObjectuiv(id: GLuint, pname: GLenum, params: *mut GLuint) {
    let v = with_gl(|c| c.get_query_objectuiv(id, pname));
    // SAFETY: the caller has room for the value.
    unsafe { put(params, v) };
}

/// The name of the sync object `sync` is (0 for no name of ours).
fn sync_name(sync: GLsync) -> GLuint {
    GLuint::try_from(sync as usize).unwrap_or(0)
}

/// `glFenceSync`.
#[unsafe(no_mangle)]
pub extern "C" fn glFenceSync(condition: GLenum, flags: GLbitfield) -> GLsync {
    with_gl(|c| c.fence_sync(condition, flags)) as usize as GLsync
}

/// `glIsSync`.
#[unsafe(no_mangle)]
pub extern "C" fn glIsSync(sync: GLsync) -> GLboolean {
    gl_boolean(with_gl(|c| c.is_sync(sync_name(sync))))
}

/// `glDeleteSync`.
#[unsafe(no_mangle)]
pub extern "C" fn glDeleteSync(sync: GLsync) {
    with_gl(|c| c.delete_sync(sync_name(sync)));
}

/// `glClientWaitSync`.
#[unsafe(no_mangle)]
pub extern "C" fn glClientWaitSync(sync: GLsync, flags: GLbitfield, timeout: GLuint64) -> GLenum {
    crate::current::with_gl_or(gl::WAIT_FAILED, |c| c.client_wait_sync(sync_name(sync), flags, timeout))
}

/// `glWaitSync`.
#[unsafe(no_mangle)]
pub extern "C" fn glWaitSync(sync: GLsync, flags: GLbitfield, timeout: GLuint64) {
    with_gl(|c| c.wait_sync(sync_name(sync), flags, timeout));
}

/// `glGetSynciv`: each of the sync's states is one value.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGetSynciv(
    sync: GLsync,
    pname: GLenum,
    buf_size: GLsizei,
    length: *mut GLsizei,
    values: *mut GLint,
) {
    with_gl(|c| {
        let Some(room) = count(buf_size) else { return c.record_error(gl::INVALID_VALUE) };
        let v = c.get_synciv(sync_name(sync), pname);
        let n = room.min(1);
        // SAFETY: the caller has room for `buf_size` values and the count.
        unsafe {
            if n > 0 {
                put(values, v);
            }
            put(length, n as GLsizei);
        }
    });
}

// ---- Transform feedback ---------------------------------------------------------------------

/// `glGenTransformFeedbacks`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGenTransformFeedbacks(n: GLsizei, ids: *mut GLuint) {
    with_gl(|c| match count(n) {
        // SAFETY: the caller has room for `n` names.
        Some(n) => c.gen_transform_feedbacks(unsafe { slice_mut(ids, n) }),
        None => c.record_error(gl::INVALID_VALUE),
    });
}

/// `glDeleteTransformFeedbacks`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glDeleteTransformFeedbacks(n: GLsizei, ids: *const GLuint) {
    with_gl(|c| match count(n) {
        // SAFETY: the caller gives `n` names.
        Some(n) => c.delete_transform_feedbacks(unsafe { slice(ids, n) }),
        None => c.record_error(gl::INVALID_VALUE),
    });
}

/// `glIsTransformFeedback`.
#[unsafe(no_mangle)]
pub extern "C" fn glIsTransformFeedback(id: GLuint) -> GLboolean {
    gl_boolean(with_gl(|c| c.is_transform_feedback(id)))
}

/// `glBindTransformFeedback`.
#[unsafe(no_mangle)]
pub extern "C" fn glBindTransformFeedback(target: GLenum, id: GLuint) {
    with_gl(|c| c.bind_transform_feedback(target, id));
}

/// `glBeginTransformFeedback`.
#[unsafe(no_mangle)]
pub extern "C" fn glBeginTransformFeedback(primitive_mode: GLenum) {
    with_gl(|c| c.begin_transform_feedback(primitive_mode));
}

/// `glEndTransformFeedback`.
#[unsafe(no_mangle)]
pub extern "C" fn glEndTransformFeedback() {
    with_gl(|c| c.end_transform_feedback());
}

/// `glPauseTransformFeedback`.
#[unsafe(no_mangle)]
pub extern "C" fn glPauseTransformFeedback() {
    with_gl(|c| c.pause_transform_feedback());
}

/// `glResumeTransformFeedback`.
#[unsafe(no_mangle)]
pub extern "C" fn glResumeTransformFeedback() {
    with_gl(|c| c.resume_transform_feedback());
}
