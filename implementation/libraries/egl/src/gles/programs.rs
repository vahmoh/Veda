// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Shaders and programs: sources, linking, what programs say of their
//! inputs and uniforms, and uniforms' values.

use std::ffi::c_void;

use gl::gl;

use super::{c_str, c_strings, count, gl_boolean, put, put_all, put_str, slice, slice_mut};
use crate::current::with_gl;
use crate::*;

/// `glCreateShader`.
#[unsafe(no_mangle)]
pub extern "C" fn glCreateShader(ty: GLenum) -> GLuint {
    with_gl(|c| c.create_shader(ty))
}

/// `glShaderSource`: `n` strings, each of its length (or up to its NUL).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glShaderSource(
    shader: GLuint,
    n: GLsizei,
    string: *const *const GLchar,
    length: *const GLint,
) {
    let Some(n) = count(n) else { return with_gl(|c| c.record_error(gl::INVALID_VALUE)) };
    // SAFETY: the caller gives `n` strings and their lengths, or none.
    let sources = unsafe { c_strings(string, length, n) };
    let sources: Vec<&str> = sources.iter().map(String::as_str).collect();
    with_gl(|c| c.shader_source(shader, &sources));
}

/// `glCompileShader`.
#[unsafe(no_mangle)]
pub extern "C" fn glCompileShader(shader: GLuint) {
    with_gl(|c| c.compile_shader(shader));
}

/// `glDeleteShader`.
#[unsafe(no_mangle)]
pub extern "C" fn glDeleteShader(shader: GLuint) {
    with_gl(|c| c.delete_shader(shader));
}

/// `glIsShader`.
#[unsafe(no_mangle)]
pub extern "C" fn glIsShader(shader: GLuint) -> GLboolean {
    gl_boolean(with_gl(|c| c.is_shader(shader)))
}

/// `glGetShaderiv`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGetShaderiv(shader: GLuint, pname: GLenum, params: *mut GLint) {
    let v = with_gl(|c| c.get_shaderiv(shader, pname));
    // SAFETY: the caller has room for the value.
    unsafe { put(params, v) };
}

/// A string to give the caller (`bufSize`, `length`, the string), with a
/// negative size an error.
///
/// # Safety
/// As [`put_str`]'s.
unsafe fn give_string(c: &mut ::gl::Context, s: &str, buf_size: GLsizei, length: *mut GLsizei, out: *mut GLchar) {
    if buf_size < 0 {
        return c.record_error(gl::INVALID_VALUE);
    }
    // SAFETY: forwarded from the caller.
    unsafe { put_str(s, buf_size, length, out) };
}

/// `glGetShaderInfoLog`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGetShaderInfoLog(
    shader: GLuint,
    buf_size: GLsizei,
    length: *mut GLsizei,
    info_log: *mut GLchar,
) {
    with_gl(|c| {
        let log = c.get_shader_info_log(shader);
        // SAFETY: the caller has room for `buf_size` bytes.
        unsafe { give_string(c, &log, buf_size, length, info_log) };
    });
}

/// `glGetShaderSource`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGetShaderSource(
    shader: GLuint,
    buf_size: GLsizei,
    length: *mut GLsizei,
    source: *mut GLchar,
) {
    with_gl(|c| {
        let s = c.get_shader_source(shader);
        // SAFETY: the caller has room for `buf_size` bytes.
        unsafe { give_string(c, &s, buf_size, length, source) };
    });
}

/// `glGetShaderPrecisionFormat`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGetShaderPrecisionFormat(
    shadertype: GLenum,
    precisiontype: GLenum,
    range: *mut GLint,
    precision: *mut GLint,
) {
    let (r, p) = with_gl(|c| c.get_shader_precision_format(shadertype, precisiontype));
    // SAFETY: the caller has room for the range's two values and the
    // precision.
    unsafe {
        put_all(&r, range);
        put(precision, p);
    }
}

/// `glReleaseShaderCompiler`.
#[unsafe(no_mangle)]
pub extern "C" fn glReleaseShaderCompiler() {
    with_gl(|c| c.release_shader_compiler());
}

/// `glShaderBinary`: there are no binary formats.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glShaderBinary(
    n: GLsizei,
    shaders: *const GLuint,
    binaryformat: GLenum,
    binary: *const c_void,
    length: GLsizei,
) {
    with_gl(|c| match (count(n), count(length)) {
        // SAFETY: the caller gives `n` shaders and `length` bytes.
        (Some(n), Some(l)) => {
            c.shader_binary(unsafe { slice(shaders, n) }, binaryformat, unsafe { slice(binary.cast::<u8>(), l) })
        }
        _ => c.record_error(gl::INVALID_VALUE),
    });
}

/// `glCreateProgram`.
#[unsafe(no_mangle)]
pub extern "C" fn glCreateProgram() -> GLuint {
    with_gl(|c| c.create_program())
}

/// `glAttachShader`.
#[unsafe(no_mangle)]
pub extern "C" fn glAttachShader(program: GLuint, shader: GLuint) {
    with_gl(|c| c.attach_shader(program, shader));
}

/// `glDetachShader`.
#[unsafe(no_mangle)]
pub extern "C" fn glDetachShader(program: GLuint, shader: GLuint) {
    with_gl(|c| c.detach_shader(program, shader));
}

/// `glGetAttachedShaders`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGetAttachedShaders(
    program: GLuint,
    max_count: GLsizei,
    n: *mut GLsizei,
    shaders: *mut GLuint,
) {
    with_gl(|c| {
        let Some(max) = count(max_count) else { return c.record_error(gl::INVALID_VALUE) };
        let all = c.get_attached_shaders(program);
        let given = &all[..all.len().min(max)];
        // SAFETY: the caller has room for `max_count` names and the count.
        unsafe {
            put_all(given, shaders);
            put(n, given.len() as GLsizei);
        }
    });
}

/// `glBindAttribLocation`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glBindAttribLocation(program: GLuint, index: GLuint, name: *const GLchar) {
    // SAFETY: the caller gives a NUL-terminated name.
    let name = unsafe { c_str(name) };
    with_gl(|c| c.bind_attrib_location(program, index, &name));
}

/// `glLinkProgram`.
#[unsafe(no_mangle)]
pub extern "C" fn glLinkProgram(program: GLuint) {
    with_gl(|c| c.link_program(program));
}

/// `glUseProgram`.
#[unsafe(no_mangle)]
pub extern "C" fn glUseProgram(program: GLuint) {
    with_gl(|c| c.use_program(program));
}

/// `glDeleteProgram`.
#[unsafe(no_mangle)]
pub extern "C" fn glDeleteProgram(program: GLuint) {
    with_gl(|c| c.delete_program(program));
}

/// `glIsProgram`.
#[unsafe(no_mangle)]
pub extern "C" fn glIsProgram(program: GLuint) -> GLboolean {
    gl_boolean(with_gl(|c| c.is_program(program)))
}

/// `glValidateProgram`.
#[unsafe(no_mangle)]
pub extern "C" fn glValidateProgram(program: GLuint) {
    with_gl(|c| c.validate_program(program));
}

/// `glGetProgramiv`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGetProgramiv(program: GLuint, pname: GLenum, params: *mut GLint) {
    let v = with_gl(|c| c.get_programiv(program, pname));
    // SAFETY: the caller has room for the value.
    unsafe { put(params, v) };
}

/// `glGetProgramInfoLog`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGetProgramInfoLog(
    program: GLuint,
    buf_size: GLsizei,
    length: *mut GLsizei,
    info_log: *mut GLchar,
) {
    with_gl(|c| {
        let log = c.get_program_info_log(program);
        // SAFETY: the caller has room for `buf_size` bytes.
        unsafe { give_string(c, &log, buf_size, length, info_log) };
    });
}

/// `glProgramParameteri`.
#[unsafe(no_mangle)]
pub extern "C" fn glProgramParameteri(program: GLuint, pname: GLenum, value: GLint) {
    with_gl(|c| c.program_parameteri(program, pname, value));
}

/// `glGetProgramBinary`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGetProgramBinary(
    program: GLuint,
    buf_size: GLsizei,
    length: *mut GLsizei,
    binary_format: *mut GLenum,
    binary: *mut c_void,
) {
    with_gl(|c| {
        let Some(room) = count(buf_size) else { return c.record_error(gl::INVALID_VALUE) };
        let Some((format, bytes)) = c.get_program_binary(program) else { return };
        if bytes.len() > room {
            return c.record_error(gl::INVALID_OPERATION);
        }
        // SAFETY: the caller has room for `buf_size` bytes, the length and
        // the format.
        unsafe {
            put_all(&bytes, binary.cast::<u8>());
            put(length, bytes.len() as GLsizei);
            put(binary_format, format);
        }
    });
}

/// `glProgramBinary`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glProgramBinary(
    program: GLuint,
    binary_format: GLenum,
    binary: *const c_void,
    length: GLsizei,
) {
    with_gl(|c| match count(length) {
        // SAFETY: the caller gives `length` bytes.
        Some(l) => c.program_binary(program, binary_format, unsafe { slice(binary.cast::<u8>(), l) }),
        None => c.record_error(gl::INVALID_VALUE),
    });
}

/// `glGetAttribLocation`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGetAttribLocation(program: GLuint, name: *const GLchar) -> GLint {
    // SAFETY: the caller gives a NUL-terminated name.
    let name = unsafe { c_str(name) };
    with_gl(|c| c.get_attrib_location(program, &name))
}

/// `glGetFragDataLocation`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGetFragDataLocation(program: GLuint, name: *const GLchar) -> GLint {
    // SAFETY: the caller gives a NUL-terminated name.
    let name = unsafe { c_str(name) };
    with_gl(|c| c.get_frag_data_location(program, &name))
}

/// What `glGetActiveAttrib` and its kind give: the size, the type and the
/// name of an input or uniform (`found`, if there is one).
///
/// # Safety
/// As `glGetActiveAttrib`'s arguments.
unsafe fn give_active(
    c: &mut ::gl::Context,
    found: Option<(i32, u32, String)>,
    buf_size: GLsizei,
    length: *mut GLsizei,
    size: *mut GLint,
    ty: *mut GLenum,
    name: *mut GLchar,
) {
    let Some((s, t, n)) = found else { return };
    // SAFETY: forwarded from the caller.
    unsafe {
        give_string(c, &n, buf_size, length, name);
        put(size, s);
        put(ty, t);
    }
}

/// `glGetActiveAttrib`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGetActiveAttrib(
    program: GLuint,
    index: GLuint,
    buf_size: GLsizei,
    length: *mut GLsizei,
    size: *mut GLint,
    ty: *mut GLenum,
    name: *mut GLchar,
) {
    with_gl(|c| {
        let found = c.get_active_attrib(program, index);
        // SAFETY: forwarded from the caller.
        unsafe { give_active(c, found, buf_size, length, size, ty, name) };
    });
}

/// `glGetActiveUniform`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGetActiveUniform(
    program: GLuint,
    index: GLuint,
    buf_size: GLsizei,
    length: *mut GLsizei,
    size: *mut GLint,
    ty: *mut GLenum,
    name: *mut GLchar,
) {
    with_gl(|c| {
        let found = c.get_active_uniform(program, index);
        // SAFETY: forwarded from the caller.
        unsafe { give_active(c, found, buf_size, length, size, ty, name) };
    });
}

/// `glGetTransformFeedbackVarying`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGetTransformFeedbackVarying(
    program: GLuint,
    index: GLuint,
    buf_size: GLsizei,
    length: *mut GLsizei,
    size: *mut GLsizei,
    ty: *mut GLenum,
    name: *mut GLchar,
) {
    with_gl(|c| {
        let found = c.get_transform_feedback_varying(program, index);
        // SAFETY: forwarded from the caller.
        unsafe { give_active(c, found, buf_size, length, size, ty, name) };
    });
}

/// `glTransformFeedbackVaryings`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glTransformFeedbackVaryings(
    program: GLuint,
    n: GLsizei,
    varyings: *const *const GLchar,
    buffer_mode: GLenum,
) {
    let Some(n) = count(n) else { return with_gl(|c| c.record_error(gl::INVALID_VALUE)) };
    // SAFETY: the caller gives `n` NUL-terminated names.
    let names = unsafe { c_strings(varyings, std::ptr::null(), n) };
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    with_gl(|c| c.transform_feedback_varyings(program, &names, buffer_mode));
}

/// `glGetUniformLocation`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGetUniformLocation(program: GLuint, name: *const GLchar) -> GLint {
    // SAFETY: the caller gives a NUL-terminated name.
    let name = unsafe { c_str(name) };
    with_gl(|c| c.get_uniform_location(program, &name))
}

/// `glGetUniformIndices`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGetUniformIndices(
    program: GLuint,
    n: GLsizei,
    names: *const *const GLchar,
    indices: *mut GLuint,
) {
    let Some(n) = count(n) else { return with_gl(|c| c.record_error(gl::INVALID_VALUE)) };
    // SAFETY: the caller gives `n` NUL-terminated names.
    let all = unsafe { c_strings(names, std::ptr::null(), n) };
    let all: Vec<&str> = all.iter().map(String::as_str).collect();
    let found = with_gl(|c| c.get_uniform_indices(program, &all));
    if found.len() == n {
        // SAFETY: the caller has room for `n` indices.
        unsafe { put_all(&found, indices) };
    }
}

/// `glGetActiveUniformsiv`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGetActiveUniformsiv(
    program: GLuint,
    n: GLsizei,
    indices: *const GLuint,
    pname: GLenum,
    params: *mut GLint,
) {
    with_gl(|c| match count(n) {
        // SAFETY: the caller gives `n` indices and has room for `n` values.
        Some(n) => {
            c.get_active_uniformsiv(program, unsafe { slice(indices, n) }, pname, unsafe { slice_mut(params, n) })
        }
        None => c.record_error(gl::INVALID_VALUE),
    });
}

/// `glGetUniformBlockIndex`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGetUniformBlockIndex(program: GLuint, name: *const GLchar) -> GLuint {
    // SAFETY: the caller gives a NUL-terminated name.
    let name = unsafe { c_str(name) };
    with_gl_invalid(|c| c.get_uniform_block_index(program, &name))
}

/// [`with_gl`], `GL_INVALID_INDEX` without a context.
fn with_gl_invalid(f: impl FnOnce(&mut ::gl::Context) -> GLuint) -> GLuint {
    crate::current::with_gl_or(gl::INVALID_INDEX, f)
}

/// `glGetActiveUniformBlockiv`: the block's uniforms' indices for
/// `GL_UNIFORM_BLOCK_ACTIVE_UNIFORM_INDICES`, one value otherwise.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGetActiveUniformBlockiv(program: GLuint, index: GLuint, pname: GLenum, params: *mut GLint) {
    with_gl(|c| {
        let n = if pname == gl::UNIFORM_BLOCK_ACTIVE_UNIFORM_INDICES {
            let mut k = [0];
            c.get_active_uniform_blockiv(program, index, gl::UNIFORM_BLOCK_ACTIVE_UNIFORMS, &mut k);
            usize::try_from(k[0]).unwrap_or(0)
        } else {
            1
        };
        // SAFETY: the caller has room for the values.
        c.get_active_uniform_blockiv(program, index, pname, unsafe { slice_mut(params, n) });
    });
}

/// `glGetActiveUniformBlockName`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGetActiveUniformBlockName(
    program: GLuint,
    index: GLuint,
    buf_size: GLsizei,
    length: *mut GLsizei,
    name: *mut GLchar,
) {
    with_gl(|c| {
        let s = c.get_active_uniform_block_name(program, index);
        // SAFETY: the caller has room for `buf_size` bytes.
        unsafe { give_string(c, &s, buf_size, length, name) };
    });
}

/// `glUniformBlockBinding`.
#[unsafe(no_mangle)]
pub extern "C" fn glUniformBlockBinding(program: GLuint, index: GLuint, binding: GLuint) {
    with_gl(|c| c.uniform_block_binding(program, index, binding));
}

// ---- Uniforms -----------------------------------------------------------------------------------

/// `glUniform1f`.
#[unsafe(no_mangle)]
pub extern "C" fn glUniform1f(location: GLint, v0: GLfloat) {
    with_gl(|c| c.uniform1f(location, v0));
}

/// `glUniform2f`.
#[unsafe(no_mangle)]
pub extern "C" fn glUniform2f(location: GLint, v0: GLfloat, v1: GLfloat) {
    with_gl(|c| c.uniform2f(location, v0, v1));
}

/// `glUniform3f`.
#[unsafe(no_mangle)]
pub extern "C" fn glUniform3f(location: GLint, v0: GLfloat, v1: GLfloat, v2: GLfloat) {
    with_gl(|c| c.uniform3f(location, v0, v1, v2));
}

/// `glUniform4f`.
#[unsafe(no_mangle)]
pub extern "C" fn glUniform4f(location: GLint, v0: GLfloat, v1: GLfloat, v2: GLfloat, v3: GLfloat) {
    with_gl(|c| c.uniform4f(location, v0, v1, v2, v3));
}

/// `glUniform1i`.
#[unsafe(no_mangle)]
pub extern "C" fn glUniform1i(location: GLint, v0: GLint) {
    with_gl(|c| c.uniform1i(location, v0));
}

/// `glUniform2i`.
#[unsafe(no_mangle)]
pub extern "C" fn glUniform2i(location: GLint, v0: GLint, v1: GLint) {
    with_gl(|c| c.uniform2i(location, v0, v1));
}

/// `glUniform3i`.
#[unsafe(no_mangle)]
pub extern "C" fn glUniform3i(location: GLint, v0: GLint, v1: GLint, v2: GLint) {
    with_gl(|c| c.uniform3i(location, v0, v1, v2));
}

/// `glUniform4i`.
#[unsafe(no_mangle)]
pub extern "C" fn glUniform4i(location: GLint, v0: GLint, v1: GLint, v2: GLint, v3: GLint) {
    with_gl(|c| c.uniform4i(location, v0, v1, v2, v3));
}

/// `glUniform1ui`.
#[unsafe(no_mangle)]
pub extern "C" fn glUniform1ui(location: GLint, v0: GLuint) {
    with_gl(|c| c.uniform1ui(location, v0));
}

/// `glUniform2ui`.
#[unsafe(no_mangle)]
pub extern "C" fn glUniform2ui(location: GLint, v0: GLuint, v1: GLuint) {
    with_gl(|c| c.uniform2ui(location, v0, v1));
}

/// `glUniform3ui`.
#[unsafe(no_mangle)]
pub extern "C" fn glUniform3ui(location: GLint, v0: GLuint, v1: GLuint, v2: GLuint) {
    with_gl(|c| c.uniform3ui(location, v0, v1, v2));
}

/// `glUniform4ui`.
#[unsafe(no_mangle)]
pub extern "C" fn glUniform4ui(location: GLint, v0: GLuint, v1: GLuint, v2: GLuint, v3: GLuint) {
    with_gl(|c| c.uniform4ui(location, v0, v1, v2, v3));
}

/// The `n` values of `n / size` uniforms of `size` components at `value`.
///
/// # Safety
/// `value` points at `count * size` values.
unsafe fn uniform_values<'a, T>(c: &mut ::gl::Context, value: *const T, n: GLsizei, size: usize) -> Option<&'a [T]> {
    let Some(n) = count(n) else {
        c.record_error(gl::INVALID_VALUE);
        return None;
    };
    // SAFETY: as the caller says.
    Some(unsafe { slice(value, n * size) })
}

macro_rules! uniform_vectors {
    ($($name:ident: $ty:ty, $size:literal, $method:ident;)*) => {$(
        #[doc = concat!("`", stringify!($name), "`.")]
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(location: GLint, n: GLsizei, value: *const $ty) {
            with_gl(|c| {
                // SAFETY: the caller gives `n` uniforms' values.
                if let Some(v) = unsafe { uniform_values(c, value, n, $size) } {
                    c.$method(location, $size, v);
                }
            });
        }
    )*};
}

uniform_vectors! {
    glUniform1fv: GLfloat, 1, uniformfv;
    glUniform2fv: GLfloat, 2, uniformfv;
    glUniform3fv: GLfloat, 3, uniformfv;
    glUniform4fv: GLfloat, 4, uniformfv;
    glUniform1iv: GLint, 1, uniformiv;
    glUniform2iv: GLint, 2, uniformiv;
    glUniform3iv: GLint, 3, uniformiv;
    glUniform4iv: GLint, 4, uniformiv;
    glUniform1uiv: GLuint, 1, uniformuiv;
    glUniform2uiv: GLuint, 2, uniformuiv;
    glUniform3uiv: GLuint, 3, uniformuiv;
    glUniform4uiv: GLuint, 4, uniformuiv;
}

macro_rules! uniform_matrices {
    ($($name:ident: $cols:literal x $rows:literal;)*) => {$(
        #[doc = concat!("`", stringify!($name), "`.")]
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(location: GLint, n: GLsizei, transpose: GLboolean, value: *const GLfloat) {
            with_gl(|c| {
                // SAFETY: the caller gives `n` matrices.
                if let Some(v) = unsafe { uniform_values(c, value, n, $cols * $rows) } {
                    c.uniform_matrixfv(location, $cols, $rows, super::boolean(transpose), v);
                }
            });
        }
    )*};
}

uniform_matrices! {
    glUniformMatrix2fv: 2 x 2;
    glUniformMatrix3fv: 3 x 3;
    glUniformMatrix4fv: 4 x 4;
    glUniformMatrix2x3fv: 2 x 3;
    glUniformMatrix3x2fv: 3 x 2;
    glUniformMatrix2x4fv: 2 x 4;
    glUniformMatrix4x2fv: 4 x 2;
    glUniformMatrix3x4fv: 3 x 4;
    glUniformMatrix4x3fv: 4 x 3;
}

/// The most values a uniform has (a 4 x 4 matrix's).
const MAX_UNIFORM_VALUES: usize = 16;

/// `glGetUniformfv`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGetUniformfv(program: GLuint, location: GLint, params: *mut GLfloat) {
    let mut v = [0.0; MAX_UNIFORM_VALUES];
    let n = with_gl(|c| c.get_uniformfv(program, location, &mut v));
    // SAFETY: the caller has room for the uniform's values.
    unsafe { put_all(&v[..n.min(MAX_UNIFORM_VALUES)], params) };
}

/// `glGetUniformiv`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGetUniformiv(program: GLuint, location: GLint, params: *mut GLint) {
    let mut v = [0; MAX_UNIFORM_VALUES];
    let n = with_gl(|c| c.get_uniformiv(program, location, &mut v));
    // SAFETY: the caller has room for the uniform's values.
    unsafe { put_all(&v[..n.min(MAX_UNIFORM_VALUES)], params) };
}

/// `glGetUniformuiv`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGetUniformuiv(program: GLuint, location: GLint, params: *mut GLuint) {
    let mut v = [0; MAX_UNIFORM_VALUES];
    let n = with_gl(|c| c.get_uniformuiv(program, location, &mut v));
    // SAFETY: the caller has room for the uniform's values.
    unsafe { put_all(&v[..n.min(MAX_UNIFORM_VALUES)], params) };
}
