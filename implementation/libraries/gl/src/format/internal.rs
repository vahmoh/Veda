// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! OpenGL ES's internal formats: the sized formats of OpenGL ES 3.0 and the
//! unsized and luminance/alpha formats of OpenGL ES 2.0, with the storage
//! format of each, and the rules that pick one for the `(internalformat,
//! format, type)` of a `TexImage*` call.

use super::Format;
use crate::gl;

/// How a sized internal format's components read in queries.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ComponentType {
    UnsignedNormalized,
    SignedNormalized,
    Float,
    Int,
    UnsignedInt,
}

impl ComponentType {
    /// The `GL_*` value `GetFramebufferAttachmentParameteriv` reports.
    pub fn gl(self) -> u32 {
        match self {
            ComponentType::UnsignedNormalized => gl::UNSIGNED_NORMALIZED,
            ComponentType::SignedNormalized => gl::SIGNED_NORMALIZED,
            ComponentType::Float => gl::FLOAT,
            ComponentType::Int => gl::INT,
            ComponentType::UnsignedInt => gl::UNSIGNED_INT,
        }
    }
}

/// A sized internal format.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Internal {
    /// The `GL_*` sized internal format.
    pub gl: u32,
    pub format: Format,
    /// The base internal format (`GL_RGBA`, `GL_DEPTH_COMPONENT`...).
    pub base: u32,
    /// Red, green, blue, alpha, depth and stencil bits.
    pub bits: [u8; 6],
    pub component: ComponentType,
    /// Color-renderable (core OpenGL ES 3.0).
    pub renderable: bool,
    /// Color-renderable with `EXT_color_buffer_float`.
    pub float_renderable: bool,
    /// Texture-filterable (32-bit floats need `OES_texture_float_linear`).
    pub filterable: bool,
}

const fn fmt(
    gl: u32,
    format: Format,
    base: u32,
    bits: [u8; 6],
    component: ComponentType,
    renderable: bool,
    filterable: bool,
) -> Internal {
    Internal { gl, format, base, bits, component, renderable, float_renderable: false, filterable }
}

const fn ffmt(gl: u32, format: Format, base: u32, bits: [u8; 6], filterable: bool) -> Internal {
    Internal {
        gl,
        format,
        base,
        bits,
        component: ComponentType::Float,
        renderable: false,
        float_renderable: true,
        filterable,
    }
}

use ComponentType as C;
use Format as F;

/// OpenGL ES 3.0's sized internal formats (tables 3.13 and 3.14).
pub const SIZED: &[Internal] = &[
    fmt(gl::R8, F::R8Unorm, gl::RED, [8, 0, 0, 0, 0, 0], C::UnsignedNormalized, true, true),
    fmt(gl::R8_SNORM, F::R8Snorm, gl::RED, [8, 0, 0, 0, 0, 0], C::SignedNormalized, false, true),
    fmt(gl::RG8, F::Rg8Unorm, gl::RG, [8, 8, 0, 0, 0, 0], C::UnsignedNormalized, true, true),
    fmt(gl::RG8_SNORM, F::Rg8Snorm, gl::RG, [8, 8, 0, 0, 0, 0], C::SignedNormalized, false, true),
    fmt(gl::RGB8, F::Rgbx8Unorm, gl::RGB, [8, 8, 8, 0, 0, 0], C::UnsignedNormalized, true, true),
    fmt(gl::RGB8_SNORM, F::Rgbx8Snorm, gl::RGB, [8, 8, 8, 0, 0, 0], C::SignedNormalized, false, true),
    fmt(gl::RGB565, F::B5G6R5Unorm, gl::RGB, [5, 6, 5, 0, 0, 0], C::UnsignedNormalized, true, true),
    fmt(gl::RGBA4, F::Rgba4Unorm, gl::RGBA, [4, 4, 4, 4, 0, 0], C::UnsignedNormalized, true, true),
    fmt(gl::RGB5_A1, F::Rgb5A1Unorm, gl::RGBA, [5, 5, 5, 1, 0, 0], C::UnsignedNormalized, true, true),
    fmt(gl::RGBA8, F::Rgba8Unorm, gl::RGBA, [8, 8, 8, 8, 0, 0], C::UnsignedNormalized, true, true),
    fmt(gl::RGBA8_SNORM, F::Rgba8Snorm, gl::RGBA, [8, 8, 8, 8, 0, 0], C::SignedNormalized, false, true),
    fmt(gl::RGB10_A2, F::Rgb10A2Unorm, gl::RGBA, [10, 10, 10, 2, 0, 0], C::UnsignedNormalized, true, true),
    fmt(gl::RGB10_A2UI, F::Rgb10A2Uint, gl::RGBA, [10, 10, 10, 2, 0, 0], C::UnsignedInt, true, false),
    fmt(gl::SRGB8, F::Rgbx8Srgb, gl::RGB, [8, 8, 8, 0, 0, 0], C::UnsignedNormalized, false, true),
    fmt(gl::SRGB8_ALPHA8, F::Rgba8Srgb, gl::RGBA, [8, 8, 8, 8, 0, 0], C::UnsignedNormalized, true, true),
    ffmt(gl::R16F, F::R16Float, gl::RED, [16, 0, 0, 0, 0, 0], true),
    ffmt(gl::RG16F, F::Rg16Float, gl::RG, [16, 16, 0, 0, 0, 0], true),
    fmt(gl::RGB16F, F::Rgbx16Float, gl::RGB, [16, 16, 16, 0, 0, 0], C::Float, false, true),
    ffmt(gl::RGBA16F, F::Rgba16Float, gl::RGBA, [16, 16, 16, 16, 0, 0], true),
    ffmt(gl::R32F, F::R32Float, gl::RED, [32, 0, 0, 0, 0, 0], false),
    ffmt(gl::RG32F, F::Rg32Float, gl::RG, [32, 32, 0, 0, 0, 0], false),
    fmt(gl::RGB32F, F::Rgbx32Float, gl::RGB, [32, 32, 32, 0, 0, 0], C::Float, false, false),
    ffmt(gl::RGBA32F, F::Rgba32Float, gl::RGBA, [32, 32, 32, 32, 0, 0], false),
    ffmt(gl::R11F_G11F_B10F, F::R11G11B10Float, gl::RGB, [11, 11, 10, 0, 0, 0], true),
    fmt(gl::RGB9_E5, F::Rgb9E5Float, gl::RGB, [9, 9, 9, 0, 0, 0], C::Float, false, true),
    fmt(gl::R8I, F::R8Sint, gl::RED, [8, 0, 0, 0, 0, 0], C::Int, true, false),
    fmt(gl::R8UI, F::R8Uint, gl::RED, [8, 0, 0, 0, 0, 0], C::UnsignedInt, true, false),
    fmt(gl::R16I, F::R16Sint, gl::RED, [16, 0, 0, 0, 0, 0], C::Int, true, false),
    fmt(gl::R16UI, F::R16Uint, gl::RED, [16, 0, 0, 0, 0, 0], C::UnsignedInt, true, false),
    fmt(gl::R32I, F::R32Sint, gl::RED, [32, 0, 0, 0, 0, 0], C::Int, true, false),
    fmt(gl::R32UI, F::R32Uint, gl::RED, [32, 0, 0, 0, 0, 0], C::UnsignedInt, true, false),
    fmt(gl::RG8I, F::Rg8Sint, gl::RG, [8, 8, 0, 0, 0, 0], C::Int, true, false),
    fmt(gl::RG8UI, F::Rg8Uint, gl::RG, [8, 8, 0, 0, 0, 0], C::UnsignedInt, true, false),
    fmt(gl::RG16I, F::Rg16Sint, gl::RG, [16, 16, 0, 0, 0, 0], C::Int, true, false),
    fmt(gl::RG16UI, F::Rg16Uint, gl::RG, [16, 16, 0, 0, 0, 0], C::UnsignedInt, true, false),
    fmt(gl::RG32I, F::Rg32Sint, gl::RG, [32, 32, 0, 0, 0, 0], C::Int, true, false),
    fmt(gl::RG32UI, F::Rg32Uint, gl::RG, [32, 32, 0, 0, 0, 0], C::UnsignedInt, true, false),
    fmt(gl::RGB8I, F::Rgbx8Sint, gl::RGB, [8, 8, 8, 0, 0, 0], C::Int, false, false),
    fmt(gl::RGB8UI, F::Rgbx8Uint, gl::RGB, [8, 8, 8, 0, 0, 0], C::UnsignedInt, false, false),
    fmt(gl::RGB16I, F::Rgbx16Sint, gl::RGB, [16, 16, 16, 0, 0, 0], C::Int, false, false),
    fmt(gl::RGB16UI, F::Rgbx16Uint, gl::RGB, [16, 16, 16, 0, 0, 0], C::UnsignedInt, false, false),
    fmt(gl::RGB32I, F::Rgbx32Sint, gl::RGB, [32, 32, 32, 0, 0, 0], C::Int, false, false),
    fmt(gl::RGB32UI, F::Rgbx32Uint, gl::RGB, [32, 32, 32, 0, 0, 0], C::UnsignedInt, false, false),
    fmt(gl::RGBA8I, F::Rgba8Sint, gl::RGBA, [8, 8, 8, 8, 0, 0], C::Int, true, false),
    fmt(gl::RGBA8UI, F::Rgba8Uint, gl::RGBA, [8, 8, 8, 8, 0, 0], C::UnsignedInt, true, false),
    fmt(gl::RGBA16I, F::Rgba16Sint, gl::RGBA, [16, 16, 16, 16, 0, 0], C::Int, true, false),
    fmt(gl::RGBA16UI, F::Rgba16Uint, gl::RGBA, [16, 16, 16, 16, 0, 0], C::UnsignedInt, true, false),
    fmt(gl::RGBA32I, F::Rgba32Sint, gl::RGBA, [32, 32, 32, 32, 0, 0], C::Int, true, false),
    fmt(gl::RGBA32UI, F::Rgba32Uint, gl::RGBA, [32, 32, 32, 32, 0, 0], C::UnsignedInt, true, false),
    fmt(
        gl::DEPTH_COMPONENT16,
        F::D16Unorm,
        gl::DEPTH_COMPONENT,
        [0, 0, 0, 0, 16, 0],
        C::UnsignedNormalized,
        false,
        false,
    ),
    fmt(
        gl::DEPTH_COMPONENT24,
        F::D24Unorm,
        gl::DEPTH_COMPONENT,
        [0, 0, 0, 0, 24, 0],
        C::UnsignedNormalized,
        false,
        false,
    ),
    fmt(gl::DEPTH_COMPONENT32F, F::D32Float, gl::DEPTH_COMPONENT, [0, 0, 0, 0, 32, 0], C::Float, false, false),
    fmt(
        gl::DEPTH24_STENCIL8,
        F::D24UnormS8Uint,
        gl::DEPTH_STENCIL,
        [0, 0, 0, 0, 24, 8],
        C::UnsignedNormalized,
        false,
        false,
    ),
    fmt(gl::DEPTH32F_STENCIL8, F::D32FloatS8Uint, gl::DEPTH_STENCIL, [0, 0, 0, 0, 32, 8], C::Float, false, false),
    fmt(gl::STENCIL_INDEX8, F::S8Uint, gl::STENCIL_INDEX8, [0, 0, 0, 0, 0, 8], C::UnsignedInt, false, false),
];

/// OpenGL ES 2.0's luminance and alpha formats as internal formats (their
/// "sized" names are the unsized ones; they are filterable, not
/// renderable).
const LEGACY: &[Internal] = &[
    fmt(gl::LUMINANCE, F::L8Unorm, gl::LUMINANCE, [0, 0, 0, 0, 0, 0], C::UnsignedNormalized, false, true),
    fmt(gl::ALPHA, F::A8Unorm, gl::ALPHA, [0, 0, 0, 8, 0, 0], C::UnsignedNormalized, false, true),
    fmt(gl::LUMINANCE_ALPHA, F::L8A8Unorm, gl::LUMINANCE_ALPHA, [0, 0, 0, 8, 0, 0], C::UnsignedNormalized, false, true),
];

/// Luminance/alpha formats with half-float and float data
/// (`OES_texture_half_float`, `OES_texture_float`).
const LEGACY_FLOAT: &[(u32, u32, Format)] = &[
    (gl::LUMINANCE, gl::HALF_FLOAT, F::L16Float),
    (gl::ALPHA, gl::HALF_FLOAT, F::A16Float),
    (gl::LUMINANCE_ALPHA, gl::HALF_FLOAT, F::L16A16Float),
    (gl::LUMINANCE, gl::FLOAT, F::L32Float),
    (gl::ALPHA, gl::FLOAT, F::A32Float),
    (gl::LUMINANCE_ALPHA, gl::FLOAT, F::L32A32Float),
];

/// The sized internal format named `gl`.
pub fn sized(internalformat: u32) -> Option<&'static Internal> {
    SIZED.iter().find(|f| f.gl == internalformat)
}

/// The internal format whose texels a storage format holds: a sized one,
/// or one of OpenGL ES 2.0's luminance and alpha formats.
pub fn of_format(f: Format) -> Option<&'static Internal> {
    SIZED.iter().chain(LEGACY).find(|i| i.format == f)
}

/// Why a texture format combination is refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FormatError {
    /// An unknown token: `GL_INVALID_ENUM`.
    Enum,
    /// Known tokens that do not combine: `GL_INVALID_OPERATION`.
    Operation,
    /// A bad value (`GL_INVALID_VALUE`, e.g. an internal format that is
    /// not a format at all in `TexImage`).
    Value,
}

/// The internal format `TexImage*` creates for `(internalformat, format,
/// type)`, checked against tables 3.2 and 3.3 (and OpenGL ES 2.0's float
/// texture extensions for unsized formats).
pub fn tex_format(internalformat: u32, format: u32, ty: u32) -> Result<Internal, FormatError> {
    if !is_type(ty) {
        return Err(FormatError::Enum);
    }
    if !is_format(format) {
        return Err(FormatError::Enum);
    }
    // Unsized internal formats (table 3.3) plus extensions.
    let from_unsized = match (internalformat, format, ty) {
        (gl::RGBA, gl::RGBA, gl::UNSIGNED_BYTE) => Some(gl::RGBA8),
        (gl::RGBA, gl::RGBA, gl::UNSIGNED_SHORT_4_4_4_4) => Some(gl::RGBA4),
        (gl::RGBA, gl::RGBA, gl::UNSIGNED_SHORT_5_5_5_1) => Some(gl::RGB5_A1),
        (gl::RGB, gl::RGB, gl::UNSIGNED_BYTE) => Some(gl::RGB8),
        (gl::RGB, gl::RGB, gl::UNSIGNED_SHORT_5_6_5) => Some(gl::RGB565),
        (gl::RGBA, gl::RGBA, gl::HALF_FLOAT | HALF_FLOAT_OES) => Some(gl::RGBA16F),
        (gl::RGBA, gl::RGBA, gl::FLOAT) => Some(gl::RGBA32F),
        (gl::RGB, gl::RGB, gl::HALF_FLOAT | HALF_FLOAT_OES) => Some(gl::RGB16F),
        (gl::RGB, gl::RGB, gl::FLOAT) => Some(gl::RGB32F),
        (gl::DEPTH_COMPONENT, gl::DEPTH_COMPONENT, gl::UNSIGNED_SHORT) => Some(gl::DEPTH_COMPONENT16),
        (gl::DEPTH_COMPONENT, gl::DEPTH_COMPONENT, gl::UNSIGNED_INT) => Some(gl::DEPTH_COMPONENT24),
        (gl::DEPTH_STENCIL, gl::DEPTH_STENCIL, gl::UNSIGNED_INT_24_8) => Some(gl::DEPTH24_STENCIL8),
        _ => None,
    };
    if let Some(s) = from_unsized {
        return sized(s).copied().ok_or(FormatError::Operation);
    }
    let legacy = |i: u32| LEGACY.iter().find(|x| x.gl == i).copied();
    match (internalformat, format, ty) {
        (gl::LUMINANCE | gl::ALPHA | gl::LUMINANCE_ALPHA, f, gl::UNSIGNED_BYTE) if f == internalformat => {
            return legacy(internalformat).ok_or(FormatError::Operation);
        }
        (gl::LUMINANCE | gl::ALPHA | gl::LUMINANCE_ALPHA, f, t) if f == internalformat => {
            let t = if t == HALF_FLOAT_OES { gl::HALF_FLOAT } else { t };
            return match LEGACY_FLOAT.iter().find(|x| x.0 == f && x.1 == t) {
                Some(&(_, _, format)) => {
                    let mut i = legacy(internalformat).ok_or(FormatError::Operation)?;
                    i.format = format;
                    i.component = ComponentType::Float;
                    Ok(i)
                }
                None => Err(FormatError::Operation),
            };
        }
        _ => {}
    }
    let Some(info) = sized(internalformat) else {
        return Err(if is_format(internalformat) { FormatError::Operation } else { FormatError::Value });
    };
    if internalformat == gl::STENCIL_INDEX8 {
        return Err(FormatError::Operation);
    }
    use gl::*;
    let ok = match (format, ty) {
        (RGBA, UNSIGNED_BYTE) => matches!(internalformat, RGBA8 | RGB5_A1 | RGBA4 | SRGB8_ALPHA8),
        (RGBA, BYTE) => internalformat == RGBA8_SNORM,
        (RGBA, UNSIGNED_SHORT_4_4_4_4) => internalformat == RGBA4,
        (RGBA, UNSIGNED_SHORT_5_5_5_1) => internalformat == RGB5_A1,
        (RGBA, UNSIGNED_INT_2_10_10_10_REV) => matches!(internalformat, RGB10_A2 | RGB5_A1),
        (RGBA, HALF_FLOAT) => internalformat == RGBA16F,
        (RGBA, FLOAT) => matches!(internalformat, RGBA32F | RGBA16F),
        (RGBA_INTEGER, UNSIGNED_BYTE) => internalformat == RGBA8UI,
        (RGBA_INTEGER, BYTE) => internalformat == RGBA8I,
        (RGBA_INTEGER, UNSIGNED_SHORT) => internalformat == RGBA16UI,
        (RGBA_INTEGER, SHORT) => internalformat == RGBA16I,
        (RGBA_INTEGER, UNSIGNED_INT) => internalformat == RGBA32UI,
        (RGBA_INTEGER, INT) => internalformat == RGBA32I,
        (RGBA_INTEGER, UNSIGNED_INT_2_10_10_10_REV) => internalformat == RGB10_A2UI,
        (RGB, UNSIGNED_BYTE) => matches!(internalformat, RGB8 | RGB565 | SRGB8),
        (RGB, BYTE) => internalformat == RGB8_SNORM,
        (RGB, UNSIGNED_SHORT_5_6_5) => internalformat == RGB565,
        (RGB, UNSIGNED_INT_10F_11F_11F_REV) => internalformat == R11F_G11F_B10F,
        (RGB, UNSIGNED_INT_5_9_9_9_REV) => internalformat == RGB9_E5,
        (RGB, HALF_FLOAT) => matches!(internalformat, RGB16F | R11F_G11F_B10F | RGB9_E5),
        (RGB, FLOAT) => matches!(internalformat, RGB32F | RGB16F | R11F_G11F_B10F | RGB9_E5),
        (RGB_INTEGER, UNSIGNED_BYTE) => internalformat == RGB8UI,
        (RGB_INTEGER, BYTE) => internalformat == RGB8I,
        (RGB_INTEGER, UNSIGNED_SHORT) => internalformat == RGB16UI,
        (RGB_INTEGER, SHORT) => internalformat == RGB16I,
        (RGB_INTEGER, UNSIGNED_INT) => internalformat == RGB32UI,
        (RGB_INTEGER, INT) => internalformat == RGB32I,
        (RG, UNSIGNED_BYTE) => internalformat == RG8,
        (RG, BYTE) => internalformat == RG8_SNORM,
        (RG, HALF_FLOAT) => internalformat == RG16F,
        (RG, FLOAT) => matches!(internalformat, RG32F | RG16F),
        (RG_INTEGER, UNSIGNED_BYTE) => internalformat == RG8UI,
        (RG_INTEGER, BYTE) => internalformat == RG8I,
        (RG_INTEGER, UNSIGNED_SHORT) => internalformat == RG16UI,
        (RG_INTEGER, SHORT) => internalformat == RG16I,
        (RG_INTEGER, UNSIGNED_INT) => internalformat == RG32UI,
        (RG_INTEGER, INT) => internalformat == RG32I,
        (RED, UNSIGNED_BYTE) => internalformat == R8,
        (RED, BYTE) => internalformat == R8_SNORM,
        (RED, HALF_FLOAT) => internalformat == R16F,
        (RED, FLOAT) => matches!(internalformat, R32F | R16F),
        (RED_INTEGER, UNSIGNED_BYTE) => internalformat == R8UI,
        (RED_INTEGER, BYTE) => internalformat == R8I,
        (RED_INTEGER, UNSIGNED_SHORT) => internalformat == R16UI,
        (RED_INTEGER, SHORT) => internalformat == R16I,
        (RED_INTEGER, UNSIGNED_INT) => internalformat == R32UI,
        (RED_INTEGER, INT) => internalformat == R32I,
        (DEPTH_COMPONENT, UNSIGNED_SHORT) => internalformat == DEPTH_COMPONENT16,
        (DEPTH_COMPONENT, UNSIGNED_INT) => matches!(internalformat, DEPTH_COMPONENT24 | DEPTH_COMPONENT16),
        (DEPTH_COMPONENT, FLOAT) => internalformat == DEPTH_COMPONENT32F,
        (DEPTH_STENCIL, UNSIGNED_INT_24_8) => internalformat == DEPTH24_STENCIL8,
        (DEPTH_STENCIL, FLOAT_32_UNSIGNED_INT_24_8_REV) => internalformat == DEPTH32F_STENCIL8,
        _ => false,
    };
    if ok { Ok(*info) } else { Err(FormatError::Operation) }
}

/// `GL_HALF_FLOAT_OES` (OpenGL ES 2.0's half float type, a different value
/// from OpenGL ES 3.0's `GL_HALF_FLOAT`).
pub const HALF_FLOAT_OES: u32 = 0x8D61;

/// Whether `t` is a pixel data type.
pub fn is_type(t: u32) -> bool {
    use gl::*;
    matches!(
        t,
        UNSIGNED_BYTE
            | BYTE
            | UNSIGNED_SHORT
            | SHORT
            | UNSIGNED_INT
            | INT
            | HALF_FLOAT
            | HALF_FLOAT_OES
            | FLOAT
            | UNSIGNED_SHORT_5_6_5
            | UNSIGNED_SHORT_4_4_4_4
            | UNSIGNED_SHORT_5_5_5_1
            | UNSIGNED_INT_2_10_10_10_REV
            | UNSIGNED_INT_10F_11F_11F_REV
            | UNSIGNED_INT_5_9_9_9_REV
            | UNSIGNED_INT_24_8
            | FLOAT_32_UNSIGNED_INT_24_8_REV
    )
}

/// Whether `f` is a pixel data format.
pub fn is_format(f: u32) -> bool {
    use gl::*;
    matches!(
        f,
        RED | RED_INTEGER
            | RG
            | RG_INTEGER
            | RGB
            | RGB_INTEGER
            | RGBA
            | RGBA_INTEGER
            | DEPTH_COMPONENT
            | DEPTH_STENCIL
            | LUMINANCE_ALPHA
            | LUMINANCE
            | ALPHA
    )
}
