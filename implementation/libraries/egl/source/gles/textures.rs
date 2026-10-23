// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Textures and samplers. An upload's pixels are the bytes it reads from
//! `pixels` (as many as the unpack storage modes say), or an offset into
//! the bound pixel unpack buffer.

use std::ffi::c_void;

use ::gl::pixels::Pixels;
use gl::gl;

use super::{count, gl_boolean, put, slice, slice_mut};
use crate::current::with_gl;
use crate::*;

/// An upload's pixels: `w` x `h` x `d` of `(format, ty)` at `pixels`, an
/// offset into the pixel unpack buffer if one is bound, or none.
///
/// # Safety
/// `pixels` is null, an offset, or points at the pixels the upload reads.
unsafe fn upload<'a>(
    c: &mut ::gl::Context,
    pixels: *const c_void,
    w: GLsizei,
    h: GLsizei,
    d: GLsizei,
    format: GLenum,
    ty: GLenum,
    three_d: bool,
) -> Pixels<'a> {
    if c.get_integer(gl::PIXEL_UNPACK_BUFFER_BINDING) != 0 {
        return Pixels::Offset(pixels as usize);
    }
    if pixels.is_null() {
        return Pixels::None;
    }
    let n = c.unpack_size(w, h, d, format, ty, three_d).unwrap_or(0);
    // SAFETY: as the caller says.
    Pixels::Data(unsafe { slice(pixels.cast::<u8>(), n) })
}

/// A compressed upload's `size` bytes at `data`, or an offset into the
/// pixel unpack buffer if one is bound: `None` for a negative size.
///
/// # Safety
/// `data` is null, an offset, or points at `size` bytes.
unsafe fn compressed<'a>(c: &mut ::gl::Context, data: *const c_void, size: GLsizei) -> Option<(usize, Pixels<'a>)> {
    let Some(n) = count(size) else {
        c.record_error(gl::INVALID_VALUE);
        return None;
    };
    if c.get_integer(gl::PIXEL_UNPACK_BUFFER_BINDING) != 0 {
        return Some((n, Pixels::Offset(data as usize)));
    }
    // SAFETY: as the caller says.
    Some((n, if data.is_null() { Pixels::None } else { Pixels::Data(unsafe { slice(data.cast::<u8>(), n) }) }))
}

/// `glActiveTexture`.
#[unsafe(no_mangle)]
pub extern "C" fn glActiveTexture(texture: GLenum) {
    with_gl(|c| c.active_texture(texture));
}

/// `glGenTextures`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGenTextures(n: GLsizei, textures: *mut GLuint) {
    with_gl(|c| match count(n) {
        // SAFETY: the caller has room for `n` names.
        Some(n) => c.gen_textures(unsafe { slice_mut(textures, n) }),
        None => c.record_error(gl::INVALID_VALUE),
    });
}

/// `glDeleteTextures`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glDeleteTextures(n: GLsizei, textures: *const GLuint) {
    with_gl(|c| match count(n) {
        // SAFETY: the caller gives `n` names.
        Some(n) => c.delete_textures(unsafe { slice(textures, n) }),
        None => c.record_error(gl::INVALID_VALUE),
    });
}

/// `glIsTexture`.
#[unsafe(no_mangle)]
pub extern "C" fn glIsTexture(texture: GLuint) -> GLboolean {
    gl_boolean(with_gl(|c| c.is_texture(texture)))
}

/// `glBindTexture`.
#[unsafe(no_mangle)]
pub extern "C" fn glBindTexture(target: GLenum, texture: GLuint) {
    with_gl(|c| c.bind_texture(target, texture));
}

/// `glTexImage2D`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glTexImage2D(
    target: GLenum,
    level: GLint,
    internalformat: GLint,
    width: GLsizei,
    height: GLsizei,
    border: GLint,
    format: GLenum,
    ty: GLenum,
    pixels: *const c_void,
) {
    with_gl(|c| {
        // SAFETY: the caller gives the pixels the upload reads.
        let p = unsafe { upload(c, pixels, width, height, 1, format, ty, false) };
        c.tex_image_2d(target, level, internalformat as GLenum, width, height, border, format, ty, p);
    });
}

/// `glTexSubImage2D`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glTexSubImage2D(
    target: GLenum,
    level: GLint,
    xoffset: GLint,
    yoffset: GLint,
    width: GLsizei,
    height: GLsizei,
    format: GLenum,
    ty: GLenum,
    pixels: *const c_void,
) {
    with_gl(|c| {
        // SAFETY: the caller gives the pixels the upload reads.
        let p = unsafe { upload(c, pixels, width, height, 1, format, ty, false) };
        c.tex_sub_image_2d(target, level, xoffset, yoffset, width, height, format, ty, p);
    });
}

/// `glTexImage3D`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glTexImage3D(
    target: GLenum,
    level: GLint,
    internalformat: GLint,
    width: GLsizei,
    height: GLsizei,
    depth: GLsizei,
    border: GLint,
    format: GLenum,
    ty: GLenum,
    pixels: *const c_void,
) {
    with_gl(|c| {
        // SAFETY: the caller gives the pixels the upload reads.
        let p = unsafe { upload(c, pixels, width, height, depth, format, ty, true) };
        c.tex_image_3d(target, level, internalformat as GLenum, width, height, depth, border, format, ty, p);
    });
}

/// `glTexSubImage3D`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glTexSubImage3D(
    target: GLenum,
    level: GLint,
    xoffset: GLint,
    yoffset: GLint,
    zoffset: GLint,
    width: GLsizei,
    height: GLsizei,
    depth: GLsizei,
    format: GLenum,
    ty: GLenum,
    pixels: *const c_void,
) {
    with_gl(|c| {
        // SAFETY: the caller gives the pixels the upload reads.
        let p = unsafe { upload(c, pixels, width, height, depth, format, ty, true) };
        c.tex_sub_image_3d(target, level, xoffset, yoffset, zoffset, width, height, depth, format, ty, p);
    });
}

/// `glCompressedTexImage2D`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glCompressedTexImage2D(
    target: GLenum,
    level: GLint,
    internalformat: GLenum,
    width: GLsizei,
    height: GLsizei,
    border: GLint,
    image_size: GLsizei,
    data: *const c_void,
) {
    with_gl(|c| {
        // SAFETY: the caller gives `image_size` bytes.
        let Some((n, p)) = (unsafe { compressed(c, data, image_size) }) else { return };
        c.compressed_tex_image_2d(target, level, internalformat, width, height, border, n, p);
    });
}

/// `glCompressedTexSubImage2D`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glCompressedTexSubImage2D(
    target: GLenum,
    level: GLint,
    xoffset: GLint,
    yoffset: GLint,
    width: GLsizei,
    height: GLsizei,
    format: GLenum,
    image_size: GLsizei,
    data: *const c_void,
) {
    with_gl(|c| {
        // SAFETY: the caller gives `image_size` bytes.
        let Some((n, p)) = (unsafe { compressed(c, data, image_size) }) else { return };
        c.compressed_tex_sub_image_2d(target, level, xoffset, yoffset, width, height, format, n, p);
    });
}

/// `glCompressedTexImage3D`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glCompressedTexImage3D(
    target: GLenum,
    level: GLint,
    internalformat: GLenum,
    width: GLsizei,
    height: GLsizei,
    depth: GLsizei,
    border: GLint,
    image_size: GLsizei,
    data: *const c_void,
) {
    with_gl(|c| {
        // SAFETY: the caller gives `image_size` bytes.
        let Some((n, p)) = (unsafe { compressed(c, data, image_size) }) else { return };
        c.compressed_tex_image_3d(target, level, internalformat, width, height, depth, border, n, p);
    });
}

/// `glCompressedTexSubImage3D`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glCompressedTexSubImage3D(
    target: GLenum,
    level: GLint,
    xoffset: GLint,
    yoffset: GLint,
    zoffset: GLint,
    width: GLsizei,
    height: GLsizei,
    depth: GLsizei,
    format: GLenum,
    image_size: GLsizei,
    data: *const c_void,
) {
    with_gl(|c| {
        // SAFETY: the caller gives `image_size` bytes.
        let Some((n, p)) = (unsafe { compressed(c, data, image_size) }) else { return };
        c.compressed_tex_sub_image_3d(target, level, xoffset, yoffset, zoffset, width, height, depth, format, n, p);
    });
}

/// `glCopyTexImage2D`.
#[unsafe(no_mangle)]
pub extern "C" fn glCopyTexImage2D(
    target: GLenum,
    level: GLint,
    internalformat: GLenum,
    x: GLint,
    y: GLint,
    width: GLsizei,
    height: GLsizei,
    border: GLint,
) {
    with_gl(|c| c.copy_tex_image_2d(target, level, internalformat, x, y, width, height, border));
}

/// `glCopyTexSubImage2D`.
#[unsafe(no_mangle)]
pub extern "C" fn glCopyTexSubImage2D(
    target: GLenum,
    level: GLint,
    xoffset: GLint,
    yoffset: GLint,
    x: GLint,
    y: GLint,
    width: GLsizei,
    height: GLsizei,
) {
    with_gl(|c| c.copy_tex_sub_image_2d(target, level, xoffset, yoffset, x, y, width, height));
}

/// `glCopyTexSubImage3D`.
#[unsafe(no_mangle)]
pub extern "C" fn glCopyTexSubImage3D(
    target: GLenum,
    level: GLint,
    xoffset: GLint,
    yoffset: GLint,
    zoffset: GLint,
    x: GLint,
    y: GLint,
    width: GLsizei,
    height: GLsizei,
) {
    with_gl(|c| c.copy_tex_sub_image_3d(target, level, xoffset, yoffset, zoffset, x, y, width, height));
}

/// `glTexStorage2D`.
#[unsafe(no_mangle)]
pub extern "C" fn glTexStorage2D(
    target: GLenum,
    levels: GLsizei,
    internalformat: GLenum,
    width: GLsizei,
    height: GLsizei,
) {
    with_gl(|c| c.tex_storage_2d(target, levels, internalformat, width, height));
}

/// `glTexStorage3D`.
#[unsafe(no_mangle)]
pub extern "C" fn glTexStorage3D(
    target: GLenum,
    levels: GLsizei,
    internalformat: GLenum,
    width: GLsizei,
    height: GLsizei,
    depth: GLsizei,
) {
    with_gl(|c| c.tex_storage_3d(target, levels, internalformat, width, height, depth));
}

/// `glGenerateMipmap`.
#[unsafe(no_mangle)]
pub extern "C" fn glGenerateMipmap(target: GLenum) {
    with_gl(|c| c.generate_mipmap(target));
}

/// `glTexParameteri`.
#[unsafe(no_mangle)]
pub extern "C" fn glTexParameteri(target: GLenum, pname: GLenum, param: GLint) {
    with_gl(|c| c.tex_parameteri(target, pname, param));
}

/// `glTexParameterf`.
#[unsafe(no_mangle)]
pub extern "C" fn glTexParameterf(target: GLenum, pname: GLenum, param: GLfloat) {
    with_gl(|c| c.tex_parameterf(target, pname, param));
}

/// `glTexParameteriv`: every parameter of OpenGL ES 3.0's is one value.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glTexParameteriv(target: GLenum, pname: GLenum, params: *const GLint) {
    // SAFETY: the caller gives the value.
    let Some(&v) = (unsafe { slice(params, 1) }).first() else { return };
    with_gl(|c| c.tex_parameteri(target, pname, v));
}

/// `glTexParameterfv`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glTexParameterfv(target: GLenum, pname: GLenum, params: *const GLfloat) {
    // SAFETY: the caller gives the value.
    let Some(&v) = (unsafe { slice(params, 1) }).first() else { return };
    with_gl(|c| c.tex_parameterf(target, pname, v));
}

/// `glGetTexParameteriv`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGetTexParameteriv(target: GLenum, pname: GLenum, params: *mut GLint) {
    let v = with_gl(|c| c.get_tex_parameteri(target, pname));
    // SAFETY: the caller has room for the value.
    unsafe { put(params, v) };
}

/// `glGetTexParameterfv`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGetTexParameterfv(target: GLenum, pname: GLenum, params: *mut GLfloat) {
    let v = with_gl(|c| c.get_tex_parameterf(target, pname));
    // SAFETY: the caller has room for the value.
    unsafe { put(params, v) };
}

// ---- Samplers ---------------------------------------------------------------------------

/// `glGenSamplers`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGenSamplers(n: GLsizei, samplers: *mut GLuint) {
    with_gl(|c| match count(n) {
        // SAFETY: the caller has room for `n` names.
        Some(n) => c.gen_samplers(unsafe { slice_mut(samplers, n) }),
        None => c.record_error(gl::INVALID_VALUE),
    });
}

/// `glDeleteSamplers`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glDeleteSamplers(n: GLsizei, samplers: *const GLuint) {
    with_gl(|c| match count(n) {
        // SAFETY: the caller gives `n` names.
        Some(n) => c.delete_samplers(unsafe { slice(samplers, n) }),
        None => c.record_error(gl::INVALID_VALUE),
    });
}

/// `glIsSampler`.
#[unsafe(no_mangle)]
pub extern "C" fn glIsSampler(sampler: GLuint) -> GLboolean {
    gl_boolean(with_gl(|c| c.is_sampler(sampler)))
}

/// `glBindSampler`.
#[unsafe(no_mangle)]
pub extern "C" fn glBindSampler(unit: GLuint, sampler: GLuint) {
    with_gl(|c| c.bind_sampler(unit, sampler));
}

/// `glSamplerParameteri`.
#[unsafe(no_mangle)]
pub extern "C" fn glSamplerParameteri(sampler: GLuint, pname: GLenum, param: GLint) {
    with_gl(|c| c.sampler_parameteri(sampler, pname, param));
}

/// `glSamplerParameterf`.
#[unsafe(no_mangle)]
pub extern "C" fn glSamplerParameterf(sampler: GLuint, pname: GLenum, param: GLfloat) {
    with_gl(|c| c.sampler_parameterf(sampler, pname, param));
}

/// `glSamplerParameteriv`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glSamplerParameteriv(sampler: GLuint, pname: GLenum, params: *const GLint) {
    // SAFETY: the caller gives the value.
    let Some(&v) = (unsafe { slice(params, 1) }).first() else { return };
    with_gl(|c| c.sampler_parameteri(sampler, pname, v));
}

/// `glSamplerParameterfv`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glSamplerParameterfv(sampler: GLuint, pname: GLenum, params: *const GLfloat) {
    // SAFETY: the caller gives the value.
    let Some(&v) = (unsafe { slice(params, 1) }).first() else { return };
    with_gl(|c| c.sampler_parameterf(sampler, pname, v));
}

/// `glGetSamplerParameteriv`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGetSamplerParameteriv(sampler: GLuint, pname: GLenum, params: *mut GLint) {
    let v = with_gl(|c| c.get_sampler_parameteri(sampler, pname));
    // SAFETY: the caller has room for the value.
    unsafe { put(params, v) };
}

/// `glGetSamplerParameterfv`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glGetSamplerParameterfv(sampler: GLuint, pname: GLenum, params: *mut GLfloat) {
    let v = with_gl(|c| c.get_sampler_parameterf(sampler, pname));
    // SAFETY: the caller has room for the value.
    unsafe { put(params, v) };
}

// ---- GL_OES_texture_3D's names for 3.0's ------------------------------------------------

/// `glFramebufferTexture3DOES`: a 3D texture's layer, as
/// `glFramebufferTextureLayer` attaches it.
#[unsafe(no_mangle)]
pub extern "C" fn glFramebufferTexture3DOES(
    target: GLenum,
    attachment: GLenum,
    textarget: GLenum,
    texture: GLuint,
    level: GLint,
    zoffset: GLint,
) {
    with_gl(|c| {
        if textarget != gl::TEXTURE_3D && texture != 0 {
            return c.record_error(gl::INVALID_ENUM);
        }
        c.framebuffer_texture_layer(target, attachment, texture, level, zoffset);
    });
}
