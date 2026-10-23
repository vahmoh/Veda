// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The context's state: capabilities, the fixed-function state, the state
//! queries, strings and errors, and finishing work.

use std::ffi::CString;

use gl::gl;

use super::{boolean, gl_boolean, put, put_all};
use crate::current::{with_gl, with_group_or};
use crate::*;

/// `glEnable`.
#[unsafe(no_mangle)]
pub extern "C" fn glEnable(cap: GLenum) {
    with_gl(|c| c.enable(cap));
}

/// `glDisable`.
#[unsafe(no_mangle)]
pub extern "C" fn glDisable(cap: GLenum) {
    with_gl(|c| c.disable(cap));
}

/// `glIsEnabled`.
#[unsafe(no_mangle)]
pub extern "C" fn glIsEnabled(cap: GLenum) -> GLboolean {
    gl_boolean(with_gl(|c| c.is_enabled(cap)))
}

/// `glViewport`.
#[unsafe(no_mangle)]
pub extern "C" fn glViewport(x: GLint, y: GLint, width: GLsizei, height: GLsizei) {
    with_gl(|c| c.viewport(x, y, width, height));
}

/// `glScissor`.
#[unsafe(no_mangle)]
pub extern "C" fn glScissor(x: GLint, y: GLint, width: GLsizei, height: GLsizei) {
    with_gl(|c| c.scissor(x, y, width, height));
}

/// `glDepthRangef`.
#[unsafe(no_mangle)]
pub extern "C" fn glDepthRangef(n: GLfloat, f: GLfloat) {
    with_gl(|c| c.depth_rangef(n, f));
}

/// `glCullFace`.
#[unsafe(no_mangle)]
pub extern "C" fn glCullFace(mode: GLenum) {
    with_gl(|c| c.cull_face(mode));
}

/// `glFrontFace`.
#[unsafe(no_mangle)]
pub extern "C" fn glFrontFace(mode: GLenum) {
    with_gl(|c| c.front_face(mode));
}

/// `glLineWidth`.
#[unsafe(no_mangle)]
pub extern "C" fn glLineWidth(width: GLfloat) {
    with_gl(|c| c.line_width(width));
}

/// `glPolygonOffset`.
#[unsafe(no_mangle)]
pub extern "C" fn glPolygonOffset(factor: GLfloat, units: GLfloat) {
    with_gl(|c| c.polygon_offset(factor, units));
}

/// `glSampleCoverage`.
#[unsafe(no_mangle)]
pub extern "C" fn glSampleCoverage(value: GLfloat, invert: GLboolean) {
    with_gl(|c| c.sample_coverage(value, boolean(invert)));
}

/// `glStencilFunc`.
#[unsafe(no_mangle)]
pub extern "C" fn glStencilFunc(func: GLenum, reference: GLint, mask: GLuint) {
    with_gl(|c| c.stencil_func(func, reference, mask));
}

/// `glStencilFuncSeparate`.
#[unsafe(no_mangle)]
pub extern "C" fn glStencilFuncSeparate(face: GLenum, func: GLenum, reference: GLint, mask: GLuint) {
    with_gl(|c| c.stencil_func_separate(face, func, reference, mask));
}

/// `glStencilMask`.
#[unsafe(no_mangle)]
pub extern "C" fn glStencilMask(mask: GLuint) {
    with_gl(|c| c.stencil_mask(mask));
}

/// `glStencilMaskSeparate`.
#[unsafe(no_mangle)]
pub extern "C" fn glStencilMaskSeparate(face: GLenum, mask: GLuint) {
    with_gl(|c| c.stencil_mask_separate(face, mask));
}

/// `glStencilOp`.
#[unsafe(no_mangle)]
pub extern "C" fn glStencilOp(fail: GLenum, zfail: GLenum, zpass: GLenum) {
    with_gl(|c| c.stencil_op(fail, zfail, zpass));
}

/// `glStencilOpSeparate`.
#[unsafe(no_mangle)]
pub extern "C" fn glStencilOpSeparate(face: GLenum, sfail: GLenum, dpfail: GLenum, dppass: GLenum) {
    with_gl(|c| c.stencil_op_separate(face, sfail, dpfail, dppass));
}

/// `glDepthFunc`.
#[unsafe(no_mangle)]
pub extern "C" fn glDepthFunc(func: GLenum) {
    with_gl(|c| c.depth_func(func));
}

/// `glDepthMask`.
#[unsafe(no_mangle)]
pub extern "C" fn glDepthMask(flag: GLboolean) {
    with_gl(|c| c.depth_mask(boolean(flag)));
}

/// `glBlendEquation`.
#[unsafe(no_mangle)]
pub extern "C" fn glBlendEquation(mode: GLenum) {
    with_gl(|c| c.blend_equation(mode));
}

/// `glBlendEquationSeparate`.
#[unsafe(no_mangle)]
pub extern "C" fn glBlendEquationSeparate(mode_rgb: GLenum, mode_alpha: GLenum) {
    with_gl(|c| c.blend_equation_separate(mode_rgb, mode_alpha));
}

/// `glBlendFunc`.
#[unsafe(no_mangle)]
pub extern "C" fn glBlendFunc(sfactor: GLenum, dfactor: GLenum) {
    with_gl(|c| c.blend_func(sfactor, dfactor));
}

/// `glBlendFuncSeparate`.
#[unsafe(no_mangle)]
pub extern "C" fn glBlendFuncSeparate(src_rgb: GLenum, dst_rgb: GLenum, src_alpha: GLenum, dst_alpha: GLenum) {
    with_gl(|c| c.blend_func_separate(src_rgb, dst_rgb, src_alpha, dst_alpha));
}

/// `glBlendColor`.
#[unsafe(no_mangle)]
pub extern "C" fn glBlendColor(red: GLfloat, green: GLfloat, blue: GLfloat, alpha: GLfloat) {
    with_gl(|c| c.blend_color(red, green, blue, alpha));
}

/// `glColorMask`.
#[unsafe(no_mangle)]
pub extern "C" fn glColorMask(red: GLboolean, green: GLboolean, blue: GLboolean, alpha: GLboolean) {
    with_gl(|c| c.color_mask(boolean(red), boolean(green), boolean(blue), boolean(alpha)));
}

/// `glClearColor`.
#[unsafe(no_mangle)]
pub extern "C" fn glClearColor(red: GLfloat, green: GLfloat, blue: GLfloat, alpha: GLfloat) {
    with_gl(|c| c.clear_color(red, green, blue, alpha));
}

/// `glClearDepthf`.
#[unsafe(no_mangle)]
pub extern "C" fn glClearDepthf(d: GLfloat) {
    with_gl(|c| c.clear_depthf(d));
}

/// `glClearStencil`.
#[unsafe(no_mangle)]
pub extern "C" fn glClearStencil(s: GLint) {
    with_gl(|c| c.clear_stencil(s));
}

/// `glHint`.
#[unsafe(no_mangle)]
pub extern "C" fn glHint(target: GLenum, mode: GLenum) {
    with_gl(|c| c.hint(target, mode));
}

/// `glPixelStorei`.
#[unsafe(no_mangle)]
pub extern "C" fn glPixelStorei(pname: GLenum, param: GLint) {
    with_gl(|c| c.pixel_storei(pname, param));
}

/// `glFlush`.
#[unsafe(no_mangle)]
pub extern "C" fn glFlush() {
    with_gl(|c| c.flush());
}

/// `glFinish`.
#[unsafe(no_mangle)]
pub extern "C" fn glFinish() {
    with_gl(|c| c.finish());
}

/// `glGetError`.
#[unsafe(no_mangle)]
pub extern "C" fn glGetError() -> GLenum {
    with_gl(|c| c.get_error())
}

// ---- State queries --------------------------------------------------------------------------

/// The most values a state has (`GL_COMPRESSED_TEXTURE_FORMATS`'s list,
/// a matrix).
const MAX_VALUES: usize = 16;

/// `glGetBooleanv`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGetBooleanv(pname: GLenum, data: *mut GLboolean) {
    let mut v = [false; MAX_VALUES];
    let n = with_gl(|c| c.get_booleanv(pname, &mut v));
    let v: Vec<GLboolean> = v[..n].iter().map(|&b| gl_boolean(b)).collect();
    // SAFETY: the caller has room for the state's values.
    unsafe { put_all(&v, data) };
}

/// `glGetIntegerv`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGetIntegerv(pname: GLenum, data: *mut GLint) {
    let mut v = [0; MAX_VALUES];
    let n = with_gl(|c| c.get_integerv(pname, &mut v));
    // SAFETY: the caller has room for the state's values.
    unsafe { put_all(&v[..n], data) };
}

/// `glGetInteger64v`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGetInteger64v(pname: GLenum, data: *mut GLint64) {
    let mut v = [0; MAX_VALUES];
    let n = with_gl(|c| c.get_integer64v(pname, &mut v));
    // SAFETY: the caller has room for the state's values.
    unsafe { put_all(&v[..n], data) };
}

/// `glGetFloatv`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGetFloatv(pname: GLenum, data: *mut GLfloat) {
    let mut v = [0.0; MAX_VALUES];
    let n = with_gl(|c| c.get_floatv(pname, &mut v));
    // SAFETY: the caller has room for the state's values.
    unsafe { put_all(&v[..n], data) };
}

/// `glGetIntegeri_v`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGetIntegeri_v(target: GLenum, index: GLuint, data: *mut GLint) {
    let v = with_gl(|c| c.get_integeri(target, index));
    // SAFETY: the caller has room for the value.
    unsafe { put(data, v) };
}

/// `glGetInteger64i_v`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGetInteger64i_v(target: GLenum, index: GLuint, data: *mut GLint64) {
    let v = with_gl(|c| c.get_integer64i(target, index));
    // SAFETY: the caller has room for the value.
    unsafe { put(data, v) };
}

/// `glGetString`: the context's strings live as long as it does.
#[unsafe(no_mangle)]
pub extern "C" fn glGetString(name: GLenum) -> *const GLubyte {
    with_group_or(std::ptr::null(), |g| {
        if !g.strings.contains_key(&name) {
            let Some(s) = g.gl.get_string(name) else { return std::ptr::null() };
            g.strings.insert(name, CString::new(s).unwrap_or_default());
        }
        g.strings[&name].as_ptr().cast()
    })
}

/// `glGetStringi`: `GL_EXTENSIONS`' names one by one.
#[unsafe(no_mangle)]
pub extern "C" fn glGetStringi(name: GLenum, index: GLuint) -> *const GLubyte {
    with_group_or(std::ptr::null(), |g| {
        let Some(s) = g.gl.get_stringi(name, index) else { return std::ptr::null() };
        if name != gl::EXTENSIONS {
            return std::ptr::null();
        }
        g.extensions.entry(index).or_insert_with(|| CString::new(s).unwrap_or_default()).as_ptr().cast()
    })
}
