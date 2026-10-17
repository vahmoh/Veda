// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Pixel formats: what resources store, what OpenGL ES calls them, and
//! conversion to and from the pixel data applications pass.
//!
//! * [`Format`] is a storage layout, the same in every back end: texels are
//!   packed as GL's packed types describe them (`RGB565` is a 16-bit value
//!   with red in the top five bits), other formats are their components in
//!   order, little-endian. RGB formats of 8, 16 and 32-bit components are
//!   stored with a fourth, unused component, as GPUs keep them. Texels
//!   convert to and from [`Texel`], a neutral form.
//! * [`Internal`] describes each sized internal format of OpenGL ES 3.0
//!   (tables 3.13 and 3.14), and the unsized and luminance/alpha formats of
//!   OpenGL ES 2.0 with their effective sized formats; [`tex_format`]
//!   checks a `(internalformat, format, type)` combination against tables
//!   3.2 and 3.3 (in `internal`).
//! * [`Client`] decodes and encodes pixels in any of those external
//!   layouts through [`Texel`], with GL's conversion rules (equations 2.1
//!   to 2.4) (in `client`).

mod client;
mod internal;

use glsl::ops::{f16_to_f32, f32_to_f16};

pub use client::Client;
pub use internal::{
    ComponentType, FormatError, HALF_FLOAT_OES, Internal, SIZED, is_format, is_type, of_format, sized, tex_format,
};

/// A storage layout.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Format {
    R8Unorm,
    R8Snorm,
    R8Uint,
    R8Sint,
    Rg8Unorm,
    Rg8Snorm,
    Rg8Uint,
    Rg8Sint,
    /// RGB8, with an unused fourth byte.
    Rgbx8Unorm,
    Rgbx8Snorm,
    Rgbx8Srgb,
    Rgbx8Uint,
    Rgbx8Sint,
    Rgba8Unorm,
    Rgba8Snorm,
    Rgba8Srgb,
    Rgba8Uint,
    Rgba8Sint,
    R16Float,
    R16Uint,
    R16Sint,
    Rg16Float,
    Rg16Uint,
    Rg16Sint,
    Rgbx16Float,
    Rgbx16Uint,
    Rgbx16Sint,
    Rgba16Float,
    Rgba16Uint,
    Rgba16Sint,
    R32Float,
    R32Uint,
    R32Sint,
    Rg32Float,
    Rg32Uint,
    Rg32Sint,
    Rgbx32Float,
    Rgbx32Uint,
    Rgbx32Sint,
    Rgba32Float,
    Rgba32Uint,
    Rgba32Sint,
    /// `UNSIGNED_SHORT_5_6_5`: red in bits 11-15.
    B5G6R5Unorm,
    /// `UNSIGNED_SHORT_4_4_4_4`: red in bits 12-15, alpha in 0-3.
    Rgba4Unorm,
    /// `UNSIGNED_SHORT_5_5_5_1`: red in bits 11-15, alpha in bit 0.
    Rgb5A1Unorm,
    /// `UNSIGNED_INT_2_10_10_10_REV`: red in bits 0-9, alpha in 30-31.
    Rgb10A2Unorm,
    Rgb10A2Uint,
    /// `UNSIGNED_INT_10F_11F_11F_REV`.
    R11G11B10Float,
    /// `UNSIGNED_INT_5_9_9_9_REV`.
    Rgb9E5Float,
    /// 16-bit depth.
    D16Unorm,
    /// 24-bit depth in bits 8-31, bits 0-7 unused.
    D24Unorm,
    /// `UNSIGNED_INT_24_8`: depth in bits 8-31, stencil in 0-7.
    D24UnormS8Uint,
    D32Float,
    /// `FLOAT_32_UNSIGNED_INT_24_8_REV`: a float depth, then a word with
    /// the stencil in its low byte.
    D32FloatS8Uint,
    S8Uint,
    /// OpenGL ES 2.0's luminance and alpha formats.
    L8Unorm,
    A8Unorm,
    L8A8Unorm,
    L16Float,
    A16Float,
    L16A16Float,
    L32Float,
    A32Float,
    L32A32Float,
}

/// What a format's components are.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Class {
    Unorm,
    Snorm,
    Float,
    Uint,
    Sint,
    Depth,
    DepthStencil,
    Stencil,
}

impl Format {
    /// Bytes per texel.
    pub fn bytes(self) -> usize {
        use Format::*;
        match self {
            R8Unorm | R8Snorm | R8Uint | R8Sint | S8Uint | L8Unorm | A8Unorm => 1,
            Rg8Unorm | Rg8Snorm | Rg8Uint | Rg8Sint | R16Float | R16Uint | R16Sint | B5G6R5Unorm | Rgba4Unorm
            | Rgb5A1Unorm | D16Unorm | L8A8Unorm | L16Float | A16Float => 2,
            Rgbx8Unorm | Rgbx8Snorm | Rgbx8Srgb | Rgbx8Uint | Rgbx8Sint | Rgba8Unorm | Rgba8Snorm | Rgba8Srgb
            | Rgba8Uint | Rgba8Sint | Rg16Float | Rg16Uint | Rg16Sint | R32Float | R32Uint | R32Sint | Rgb10A2Unorm
            | Rgb10A2Uint | R11G11B10Float | Rgb9E5Float | D24Unorm | D24UnormS8Uint | D32Float | L16A16Float
            | L32Float | A32Float => 4,
            Rgbx16Float | Rgbx16Uint | Rgbx16Sint | Rgba16Float | Rgba16Uint | Rgba16Sint | Rg32Float | Rg32Uint
            | Rg32Sint | D32FloatS8Uint | L32A32Float => 8,
            Rgbx32Float | Rgbx32Uint | Rgbx32Sint | Rgba32Float | Rgba32Uint | Rgba32Sint => 16,
        }
    }

    pub fn class(self) -> Class {
        use Format::*;
        match self {
            R8Snorm | Rg8Snorm | Rgbx8Snorm | Rgba8Snorm => Class::Snorm,
            R8Uint | Rg8Uint | Rgbx8Uint | Rgba8Uint | R16Uint | Rg16Uint | Rgbx16Uint | Rgba16Uint | R32Uint
            | Rg32Uint | Rgbx32Uint | Rgba32Uint | Rgb10A2Uint => Class::Uint,
            R8Sint | Rg8Sint | Rgbx8Sint | Rgba8Sint | R16Sint | Rg16Sint | Rgbx16Sint | Rgba16Sint | R32Sint
            | Rg32Sint | Rgbx32Sint | Rgba32Sint => Class::Sint,
            R16Float | Rg16Float | Rgbx16Float | Rgba16Float | R32Float | Rg32Float | Rgbx32Float | Rgba32Float
            | R11G11B10Float | Rgb9E5Float | L16Float | A16Float | L16A16Float | L32Float | A32Float | L32A32Float => {
                Class::Float
            }
            D16Unorm | D24Unorm | D32Float => Class::Depth,
            D24UnormS8Uint | D32FloatS8Uint => Class::DepthStencil,
            S8Uint => Class::Stencil,
            _ => Class::Unorm,
        }
    }

    pub fn is_srgb(self) -> bool {
        matches!(self, Format::Rgbx8Srgb | Format::Rgba8Srgb)
    }

    pub fn has_depth(self) -> bool {
        matches!(self.class(), Class::Depth | Class::DepthStencil)
    }

    pub fn has_stencil(self) -> bool {
        matches!(self.class(), Class::Stencil | Class::DepthStencil)
    }

    /// Whether texels are integers (sampled without filtering, written
    /// without conversion).
    pub fn is_integer(self) -> bool {
        matches!(self.class(), Class::Uint | Class::Sint)
    }

    /// The color as a shader sees it, from stored bytes: normalised and
    /// float formats as floats, integer formats as integers. Missing
    /// components read as 0, alpha as 1 (luminance as L, L, L, 1; alpha
    /// as 0, 0, 0, A).
    pub fn decode(self, b: &[u8]) -> Texel {
        use Format::*;
        let u8n = |i: usize| f32::from(b[i]) / 255.0;
        let s8n = |i: usize| (f32::from(b[i] as i8) / 127.0).max(-1.0);
        let h = |i: usize| f16_to_f32(u16::from_le_bytes([b[2 * i], b[2 * i + 1]]));
        let f = |i: usize| f32::from_le_bytes([b[4 * i], b[4 * i + 1], b[4 * i + 2], b[4 * i + 3]]);
        let u16x = |i: usize| u32::from(u16::from_le_bytes([b[2 * i], b[2 * i + 1]]));
        let i16x = |i: usize| i32::from(i16::from_le_bytes([b[2 * i], b[2 * i + 1]]));
        let u32x = |i: usize| u32::from_le_bytes([b[4 * i], b[4 * i + 1], b[4 * i + 2], b[4 * i + 3]]);
        let word = || u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        let half = || u16::from_le_bytes([b[0], b[1]]);
        match self {
            R8Unorm => Texel::Float([u8n(0), 0.0, 0.0, 1.0]),
            R8Snorm => Texel::Float([s8n(0), 0.0, 0.0, 1.0]),
            R8Uint => Texel::Uint([u32::from(b[0]), 0, 0, 1]),
            R8Sint => Texel::Int([i32::from(b[0] as i8), 0, 0, 1]),
            Rg8Unorm => Texel::Float([u8n(0), u8n(1), 0.0, 1.0]),
            Rg8Snorm => Texel::Float([s8n(0), s8n(1), 0.0, 1.0]),
            Rg8Uint => Texel::Uint([u32::from(b[0]), u32::from(b[1]), 0, 1]),
            Rg8Sint => Texel::Int([i32::from(b[0] as i8), i32::from(b[1] as i8), 0, 1]),
            Rgbx8Unorm => Texel::Float([u8n(0), u8n(1), u8n(2), 1.0]),
            Rgbx8Snorm => Texel::Float([s8n(0), s8n(1), s8n(2), 1.0]),
            Rgbx8Srgb => Texel::Float([srgb_to_linear(b[0]), srgb_to_linear(b[1]), srgb_to_linear(b[2]), 1.0]),
            Rgbx8Uint => Texel::Uint([u32::from(b[0]), u32::from(b[1]), u32::from(b[2]), 1]),
            Rgbx8Sint => Texel::Int([i32::from(b[0] as i8), i32::from(b[1] as i8), i32::from(b[2] as i8), 1]),
            Rgba8Unorm => Texel::Float([u8n(0), u8n(1), u8n(2), u8n(3)]),
            Rgba8Snorm => Texel::Float([s8n(0), s8n(1), s8n(2), s8n(3)]),
            Rgba8Srgb => Texel::Float([srgb_to_linear(b[0]), srgb_to_linear(b[1]), srgb_to_linear(b[2]), u8n(3)]),
            Rgba8Uint => Texel::Uint([u32::from(b[0]), u32::from(b[1]), u32::from(b[2]), u32::from(b[3])]),
            Rgba8Sint => {
                Texel::Int([i32::from(b[0] as i8), i32::from(b[1] as i8), i32::from(b[2] as i8), i32::from(b[3] as i8)])
            }
            R16Float => Texel::Float([h(0), 0.0, 0.0, 1.0]),
            R16Uint => Texel::Uint([u16x(0), 0, 0, 1]),
            R16Sint => Texel::Int([i16x(0), 0, 0, 1]),
            Rg16Float => Texel::Float([h(0), h(1), 0.0, 1.0]),
            Rg16Uint => Texel::Uint([u16x(0), u16x(1), 0, 1]),
            Rg16Sint => Texel::Int([i16x(0), i16x(1), 0, 1]),
            Rgbx16Float => Texel::Float([h(0), h(1), h(2), 1.0]),
            Rgbx16Uint => Texel::Uint([u16x(0), u16x(1), u16x(2), 1]),
            Rgbx16Sint => Texel::Int([i16x(0), i16x(1), i16x(2), 1]),
            Rgba16Float => Texel::Float([h(0), h(1), h(2), h(3)]),
            Rgba16Uint => Texel::Uint([u16x(0), u16x(1), u16x(2), u16x(3)]),
            Rgba16Sint => Texel::Int([i16x(0), i16x(1), i16x(2), i16x(3)]),
            R32Float => Texel::Float([f(0), 0.0, 0.0, 1.0]),
            R32Uint => Texel::Uint([u32x(0), 0, 0, 1]),
            R32Sint => Texel::Int([u32x(0) as i32, 0, 0, 1]),
            Rg32Float => Texel::Float([f(0), f(1), 0.0, 1.0]),
            Rg32Uint => Texel::Uint([u32x(0), u32x(1), 0, 1]),
            Rg32Sint => Texel::Int([u32x(0) as i32, u32x(1) as i32, 0, 1]),
            Rgbx32Float => Texel::Float([f(0), f(1), f(2), 1.0]),
            Rgbx32Uint => Texel::Uint([u32x(0), u32x(1), u32x(2), 1]),
            Rgbx32Sint => Texel::Int([u32x(0) as i32, u32x(1) as i32, u32x(2) as i32, 1]),
            Rgba32Float => Texel::Float([f(0), f(1), f(2), f(3)]),
            Rgba32Uint => Texel::Uint([u32x(0), u32x(1), u32x(2), u32x(3)]),
            Rgba32Sint => Texel::Int([u32x(0) as i32, u32x(1) as i32, u32x(2) as i32, u32x(3) as i32]),
            B5G6R5Unorm => {
                let v = half();
                Texel::Float([
                    f32::from((v >> 11) & 31) / 31.0,
                    f32::from((v >> 5) & 63) / 63.0,
                    f32::from(v & 31) / 31.0,
                    1.0,
                ])
            }
            Rgba4Unorm => {
                let v = half();
                Texel::Float([
                    f32::from((v >> 12) & 15) / 15.0,
                    f32::from((v >> 8) & 15) / 15.0,
                    f32::from((v >> 4) & 15) / 15.0,
                    f32::from(v & 15) / 15.0,
                ])
            }
            Rgb5A1Unorm => {
                let v = half();
                Texel::Float([
                    f32::from((v >> 11) & 31) / 31.0,
                    f32::from((v >> 6) & 31) / 31.0,
                    f32::from((v >> 1) & 31) / 31.0,
                    f32::from(v & 1),
                ])
            }
            Rgb10A2Unorm => {
                let v = word();
                Texel::Float([
                    (v & 1023) as f32 / 1023.0,
                    ((v >> 10) & 1023) as f32 / 1023.0,
                    ((v >> 20) & 1023) as f32 / 1023.0,
                    (v >> 30) as f32 / 3.0,
                ])
            }
            Rgb10A2Uint => {
                let v = word();
                Texel::Uint([v & 1023, (v >> 10) & 1023, (v >> 20) & 1023, v >> 30])
            }
            R11G11B10Float => {
                let v = word();
                Texel::Float([uf11_to_f32(v & 0x7FF), uf11_to_f32((v >> 11) & 0x7FF), uf10_to_f32(v >> 22), 1.0])
            }
            Rgb9E5Float => {
                let v = word();
                let e = (v >> 27) as i32 - 15 - 9;
                let scale = libm::exp2f(e as f32);
                Texel::Float([
                    (v & 511) as f32 * scale,
                    ((v >> 9) & 511) as f32 * scale,
                    ((v >> 18) & 511) as f32 * scale,
                    1.0,
                ])
            }
            D16Unorm => Texel::Depth(f32::from(half()) / 65535.0, 0),
            D24Unorm => Texel::Depth((word() >> 8) as f32 / 16_777_215.0, 0),
            D24UnormS8Uint => {
                let v = word();
                Texel::Depth((v >> 8) as f32 / 16_777_215.0, v as u8)
            }
            D32Float => Texel::Depth(f(0), 0),
            D32FloatS8Uint => Texel::Depth(f(0), b[4]),
            S8Uint => Texel::Depth(0.0, b[0]),
            L8Unorm => Texel::Float([u8n(0), u8n(0), u8n(0), 1.0]),
            A8Unorm => Texel::Float([0.0, 0.0, 0.0, u8n(0)]),
            L8A8Unorm => Texel::Float([u8n(0), u8n(0), u8n(0), u8n(1)]),
            L16Float => Texel::Float([h(0), h(0), h(0), 1.0]),
            A16Float => Texel::Float([0.0, 0.0, 0.0, h(0)]),
            L16A16Float => Texel::Float([h(0), h(0), h(0), h(1)]),
            L32Float => Texel::Float([f(0), f(0), f(0), 1.0]),
            A32Float => Texel::Float([0.0, 0.0, 0.0, f(0)]),
            L32A32Float => Texel::Float([f(0), f(0), f(0), f(1)]),
        }
    }

    /// Stores a texel: floats are clamped and rounded to normalised
    /// formats (GL's equations 2.3 and 2.4), converted to half or
    /// packed floats; integers are clamped to the component's range.
    /// Luminance formats take red as luminance.
    pub fn encode(self, t: &Texel, out: &mut [u8]) {
        use Format::*;
        let c = t.as_float();
        let ci = t.as_int();
        let cu = t.as_uint();
        let unorm = |x: f32, max: f32| -> u32 { (clamp01(x) * max + 0.5) as u32 };
        let snorm = |x: f32, max: f32| -> i32 {
            let v = x.clamp(-1.0, 1.0) * max;
            libm::roundf(if v.is_nan() { 0.0 } else { v }) as i32
        };
        let put_u16 = |out: &mut [u8], i: usize, v: u16| out[2 * i..2 * i + 2].copy_from_slice(&v.to_le_bytes());
        let put_u32 = |out: &mut [u8], i: usize, v: u32| out[4 * i..4 * i + 4].copy_from_slice(&v.to_le_bytes());
        let put_f32 = |out: &mut [u8], i: usize, v: f32| out[4 * i..4 * i + 4].copy_from_slice(&v.to_le_bytes());
        let h = |x: f32| f32_to_f16(x);
        let su = |x: u32, max: u32| x.min(max);
        let si = |x: i32, min: i32, max: i32| x.clamp(min, max);
        match self {
            R8Unorm | L8Unorm => out[0] = unorm(c[0], 255.0) as u8,
            A8Unorm => out[0] = unorm(c[3], 255.0) as u8,
            L8A8Unorm => {
                out[0] = unorm(c[0], 255.0) as u8;
                out[1] = unorm(c[3], 255.0) as u8;
            }
            R8Snorm | Rg8Snorm | Rgbx8Snorm | Rgba8Snorm => {
                let n = self.bytes().min(4);
                let k = match self {
                    R8Snorm => 1,
                    Rg8Snorm => 2,
                    Rgbx8Snorm => 3,
                    _ => 4,
                };
                for i in 0..n {
                    out[i] = if i < k { snorm(c[i], 127.0) as i8 as u8 } else { 0 };
                }
            }
            Rg8Unorm | Rgbx8Unorm | Rgba8Unorm => {
                let k = match self {
                    Rg8Unorm => 2,
                    Rgbx8Unorm => 3,
                    _ => 4,
                };
                for i in 0..self.bytes() {
                    out[i] = if i < k { unorm(c[i], 255.0) as u8 } else { 255 };
                }
            }
            Rgbx8Srgb | Rgba8Srgb => {
                for i in 0..3 {
                    out[i] = linear_to_srgb(c[i]);
                }
                out[3] = if self == Rgba8Srgb { unorm(c[3], 255.0) as u8 } else { 255 };
            }
            R8Uint | Rg8Uint | Rgbx8Uint | Rgba8Uint => {
                for i in 0..self.bytes() {
                    out[i] = su(cu[i], 255) as u8;
                }
            }
            R8Sint | Rg8Sint | Rgbx8Sint | Rgba8Sint => {
                for i in 0..self.bytes() {
                    out[i] = si(ci[i], -128, 127) as i8 as u8;
                }
            }
            R16Float | Rg16Float | Rgbx16Float | Rgba16Float | L16Float | L16A16Float | A16Float => {
                let src: [f32; 4] = match self {
                    L16A16Float => [c[0], c[3], 0.0, 0.0],
                    A16Float => [c[3], 0.0, 0.0, 0.0],
                    _ => c,
                };
                for (i, &v) in src.iter().enumerate().take(self.bytes() / 2) {
                    put_u16(out, i, h(v));
                }
            }
            R16Uint | Rg16Uint | Rgbx16Uint | Rgba16Uint => {
                for (i, &v) in cu.iter().enumerate().take(self.bytes() / 2) {
                    put_u16(out, i, su(v, 65535) as u16);
                }
            }
            R16Sint | Rg16Sint | Rgbx16Sint | Rgba16Sint => {
                for (i, &v) in ci.iter().enumerate().take(self.bytes() / 2) {
                    put_u16(out, i, si(v, -32768, 32767) as i16 as u16);
                }
            }
            R32Float | Rg32Float | Rgbx32Float | Rgba32Float | L32Float | L32A32Float | A32Float => {
                let src: [f32; 4] = match self {
                    L32A32Float => [c[0], c[3], 0.0, 0.0],
                    A32Float => [c[3], 0.0, 0.0, 0.0],
                    _ => c,
                };
                for (i, &v) in src.iter().enumerate().take(self.bytes() / 4) {
                    put_f32(out, i, v);
                }
            }
            R32Uint | Rg32Uint | Rgbx32Uint | Rgba32Uint => {
                for (i, &v) in cu.iter().enumerate().take(self.bytes() / 4) {
                    put_u32(out, i, v);
                }
            }
            R32Sint | Rg32Sint | Rgbx32Sint | Rgba32Sint => {
                for (i, &v) in ci.iter().enumerate().take(self.bytes() / 4) {
                    put_u32(out, i, v as u32);
                }
            }
            B5G6R5Unorm => {
                let v = (unorm(c[0], 31.0) << 11) | (unorm(c[1], 63.0) << 5) | unorm(c[2], 31.0);
                put_u16(out, 0, v as u16);
            }
            Rgba4Unorm => {
                let v =
                    (unorm(c[0], 15.0) << 12) | (unorm(c[1], 15.0) << 8) | (unorm(c[2], 15.0) << 4) | unorm(c[3], 15.0);
                put_u16(out, 0, v as u16);
            }
            Rgb5A1Unorm => {
                let v =
                    (unorm(c[0], 31.0) << 11) | (unorm(c[1], 31.0) << 6) | (unorm(c[2], 31.0) << 1) | unorm(c[3], 1.0);
                put_u16(out, 0, v as u16);
            }
            Rgb10A2Unorm => {
                let v = unorm(c[0], 1023.0)
                    | (unorm(c[1], 1023.0) << 10)
                    | (unorm(c[2], 1023.0) << 20)
                    | (unorm(c[3], 3.0) << 30);
                put_u32(out, 0, v);
            }
            Rgb10A2Uint => {
                let v = su(cu[0], 1023) | (su(cu[1], 1023) << 10) | (su(cu[2], 1023) << 20) | (su(cu[3], 3) << 30);
                put_u32(out, 0, v);
            }
            R11G11B10Float => {
                let v = f32_to_uf11(c[0]) | (f32_to_uf11(c[1]) << 11) | (f32_to_uf10(c[2]) << 22);
                put_u32(out, 0, v);
            }
            Rgb9E5Float => put_u32(out, 0, f32_to_rgb9e5(c[0], c[1], c[2])),
            D16Unorm => put_u16(out, 0, unorm(t.depth(), 65535.0) as u16),
            D24Unorm => put_u32(out, 0, unorm(t.depth(), 16_777_215.0) << 8),
            D24UnormS8Uint => put_u32(out, 0, (unorm(t.depth(), 16_777_215.0) << 8) | u32::from(t.stencil())),
            D32Float => put_f32(out, 0, t.depth()),
            D32FloatS8Uint => {
                put_f32(out, 0, t.depth());
                put_u32(out, 1, u32::from(t.stencil()));
            }
            S8Uint => out[0] = t.stencil(),
        }
    }
}

/// `x` clamped to [0, 1], NaN to 0 (as conversions to normalized
/// integers and depths clamp).
#[inline(always)]
pub(crate) fn clamp01(x: f32) -> f32 {
    if x > 0.0 {
        if x < 1.0 { x } else { 1.0 }
    } else {
        // Negative values and NaN.
        0.0
    }
}

/// A texel in a neutral form.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Texel {
    Float([f32; 4]),
    Int([i32; 4]),
    Uint([u32; 4]),
    /// Depth (as a float in [0, 1] for fixed-point formats) and stencil.
    Depth(f32, u8),
}

impl Texel {
    pub fn as_float(&self) -> [f32; 4] {
        match *self {
            Texel::Float(f) => f,
            Texel::Int(i) => i.map(|x| x as f32),
            Texel::Uint(u) => u.map(|x| x as f32),
            Texel::Depth(d, _) => [d, 0.0, 0.0, 1.0],
        }
    }

    pub fn as_int(&self) -> [i32; 4] {
        match *self {
            Texel::Int(i) => i,
            Texel::Uint(u) => u.map(|x| x as i32),
            Texel::Float(f) => f.map(|x| x as i32),
            Texel::Depth(_, s) => [i32::from(s), 0, 0, 1],
        }
    }

    pub fn as_uint(&self) -> [u32; 4] {
        match *self {
            Texel::Uint(u) => u,
            Texel::Int(i) => i.map(|x| x as u32),
            Texel::Float(f) => f.map(|x| x as u32),
            Texel::Depth(_, s) => [u32::from(s), 0, 0, 1],
        }
    }

    pub fn depth(&self) -> f32 {
        match *self {
            Texel::Depth(d, _) => d,
            Texel::Float(f) => f[0],
            _ => 0.0,
        }
    }

    pub fn stencil(&self) -> u8 {
        match *self {
            Texel::Depth(_, s) => s,
            Texel::Uint(u) => u[0] as u8,
            Texel::Int(i) => i[0] as u8,
            Texel::Float(_) => 0,
        }
    }
}

/// Where the texels of an image are in memory: `width` x `height` x
/// `depth` (slices or layers), rows `row` bytes apart and images `image`
/// bytes apart.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Geometry {
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub row: usize,
    pub image: usize,
}

impl Geometry {
    /// Tightly packed texels of `bytes` each.
    pub fn packed(width: u32, height: u32, depth: u32, bytes: usize) -> Geometry {
        let row = width as usize * bytes;
        Geometry { width, height, depth, row, image: row * height as usize }
    }

    fn at(&self, x: u32, y: u32, z: u32, bytes: usize) -> usize {
        z as usize * self.image + y as usize * self.row + x as usize * bytes
    }
}

/// Fills a mipmap level from the one above it: each texel is the average
/// of the 2x2 texels it covers (2x2x2 in a volume, whose slices halve
/// too; layers and faces do not), or of one along a dimension that did not
/// shrink. Colors are averaged in linear space (sRGB ones decoded first).
pub fn downsample(f: Format, src: &[u8], s: Geometry, dst: &mut [u8], d: Geometry, volume: bool) {
    let tb = f.bytes();
    for z in 0..d.depth {
        let zs = if volume && s.depth > 1 { [2 * z, (2 * z + 1).min(s.depth - 1)] } else { [z, z] };
        for y in 0..d.height {
            let ys = if s.height > 1 { [2 * y, (2 * y + 1).min(s.height - 1)] } else { [0, 0] };
            for x in 0..d.width {
                let xs = if s.width > 1 { [2 * x, (2 * x + 1).min(s.width - 1)] } else { [0, 0] };
                let mut sum = [0.0f32; 4];
                for &k in &zs {
                    for &j in &ys {
                        for &i in &xs {
                            let o = s.at(i, j, k, tb);
                            let v = f.decode(&src[o..o + tb]).as_float();
                            for c in 0..4 {
                                sum[c] += v[c];
                            }
                        }
                    }
                }
                let o = d.at(x, y, z, tb);
                f.encode(&Texel::Float(sum.map(|v| v / 8.0)), &mut dst[o..o + tb]);
            }
        }
    }
}

/// sRGB-encoded bytes as linear floats: the bits of the exact values of the
/// sRGB curve (section 3.8.16), rounded to the nearest float.
#[rustfmt::skip]
static SRGB_TO_LINEAR: [u32; 256] = [
    0x00000000, 0x399F22B4, 0x3A1F22B4, 0x3A6EB40E, 0x3A9F22B4, 0x3AC6EB61, 0x3AEEB40E, 0x3B0B3E5D,
    0x3B1F22B4, 0x3B33070A, 0x3B46EB61, 0x3B5B518E, 0x3B70F18F, 0x3B83E1C6, 0x3B8FE616, 0x3B9C87FD,
    0x3BA9C9B6, 0x3BB7AD6F, 0x3BC6354A, 0x3BD56360, 0x3BE539C1, 0x3BF5BA71, 0x3C0373B6, 0x3C0C6153,
    0x3C15A705, 0x3C1F45BE, 0x3C293E6B, 0x3C3391F7, 0x3C3E4149, 0x3C494D44, 0x3C54B6C9, 0x3C607EB4,
    0x3C6CA5DF, 0x3C792D22, 0x3C830AA9, 0x3C89AF9F, 0x3C9085DC, 0x3C978DC6, 0x3C9EC7C2, 0x3CA63433,
    0x3CADD37D, 0x3CB5A602, 0x3CBDAC21, 0x3CC5E63A, 0x3CCE54AC, 0x3CD6F7D5, 0x3CDFD010, 0x3CE8DDBA,
    0x3CF2212D, 0x3CFB9AC3, 0x3D02A56A, 0x3D0798DD, 0x3D0CA7E6, 0x3D11D2AF, 0x3D171964, 0x3D1C7C30,
    0x3D21FB3C, 0x3D2796B2, 0x3D2D4EBB, 0x3D332381, 0x3D39152B, 0x3D3F23E4, 0x3D454FD2, 0x3D4B991D,
    0x3D51FFEC, 0x3D588468, 0x3D5F26B6, 0x3D65E6FD, 0x3D6CC563, 0x3D73C20E, 0x3D7ADD24, 0x3D810B65,
    0x3D84B793, 0x3D88732E, 0x3D8C3E48, 0x3D9018F4, 0x3D940344, 0x3D97FD49, 0x3D9C0715, 0x3DA020BA,
    0x3DA44A4A, 0x3DA883D6, 0x3DACCD6F, 0x3DB12727, 0x3DB5910F, 0x3DBA0B38, 0x3DBE95B3, 0x3DC33090,
    0x3DC7DBE0, 0x3DCC97B4, 0x3DD1641D, 0x3DD6412B, 0x3DDB2EEE, 0x3DE02D76, 0x3DE53CD4, 0x3DEA5D18,
    0x3DEF8E51, 0x3DF4D090, 0x3DFA23E5, 0x3DFF885E, 0x3E027F06, 0x3E05427F, 0x3E080EA2, 0x3E0AE377,
    0x3E0DC104, 0x3E10A753, 0x3E13966A, 0x3E168E51, 0x3E198F0F, 0x3E1C98AC, 0x3E1FAB30, 0x3E22C6A1,
    0x3E25EB07, 0x3E29186A, 0x3E2C4ED0, 0x3E2F8E42, 0x3E32D6C5, 0x3E362862, 0x3E39831F, 0x3E3CE703,
    0x3E405417, 0x3E43CA60, 0x3E4749E6, 0x3E4AD2AF, 0x3E4E64C3, 0x3E520029, 0x3E55A4E7, 0x3E595305,
    0x3E5D0A89, 0x3E60CB7A, 0x3E6495DF, 0x3E6869BE, 0x3E6C471F, 0x3E702E07, 0x3E741E7E, 0x3E78188B,
    0x3E7C1C33, 0x3E8014BF, 0x3E822039, 0x3E84308B, 0x3E8645B8, 0x3E885FC3, 0x3E8A7EB0, 0x3E8CA281,
    0x3E8ECB3B, 0x3E90F8DF, 0x3E932B72, 0x3E9562F6, 0x3E979F6F, 0x3E99E0E0, 0x3E9C274C, 0x3E9E72B6,
    0x3EA0C321, 0x3EA31890, 0x3EA57307, 0x3EA7D288, 0x3EAA3716, 0x3EACA0B6, 0x3EAF0F68, 0x3EB18332,
    0x3EB3FC15, 0x3EB67A14, 0x3EB8FD34, 0x3EBB8576, 0x3EBE12DE, 0x3EC0A56E, 0x3EC33D2A, 0x3EC5DA14,
    0x3EC87C30, 0x3ECB2380, 0x3ECDD008, 0x3ED081CA, 0x3ED338C9, 0x3ED5F508, 0x3ED8B68A, 0x3EDB7D52,
    0x3EDE4963, 0x3EE11ABF, 0x3EE3F169, 0x3EE6CD65, 0x3EE9AEB5, 0x3EEC955B, 0x3EEF815C, 0x3EF272B8,
    0x3EF56974, 0x3EF86593, 0x3EFB6716, 0x3EFE6E00, 0x3F00BD2B, 0x3F02460C, 0x3F03D1A5, 0x3F055FF7,
    0x3F06F104, 0x3F0884CD, 0x3F0A1B54, 0x3F0BB499, 0x3F0D509F, 0x3F0EEF65, 0x3F1090EF, 0x3F12353D,
    0x3F13DC50, 0x3F15862A, 0x3F1732CC, 0x3F18E237, 0x3F1A946E, 0x3F1C4970, 0x3F1E0140, 0x3F1FBBDE,
    0x3F21794D, 0x3F23398C, 0x3F24FC9F, 0x3F26C285, 0x3F288B41, 0x3F2A56D2, 0x3F2C253C, 0x3F2DF67F,
    0x3F2FCA9C, 0x3F31A194, 0x3F337B6A, 0x3F35581D, 0x3F3737B0, 0x3F391A24, 0x3F3AFF7A, 0x3F3CE7B2,
    0x3F3ED2CF, 0x3F40C0D2, 0x3F42B1BC, 0x3F44A58E, 0x3F469C49, 0x3F4895EF, 0x3F4A9280, 0x3F4C91FF,
    0x3F4E946C, 0x3F5099C9, 0x3F52A216, 0x3F54AD56, 0x3F56BB88, 0x3F58CCAF, 0x3F5AE0CC, 0x3F5CF7DF,
    0x3F5F11EA, 0x3F612EEF, 0x3F634EEE, 0x3F6571E9, 0x3F6797E0, 0x3F69C0D5, 0x3F6BECCA, 0x3F6E1BBF,
    0x3F704DB5, 0x3F7282AE, 0x3F74BAAB, 0x3F76F5AE, 0x3F7933B6, 0x3F7B74C6, 0x3F7DB8DE, 0x3F800000,
];

/// Where linear values start to encode as each sRGB byte above 0: entry
/// `c` is the smallest float whose exact encoding rounds to `c + 1`.
#[rustfmt::skip]
static LINEAR_TO_SRGB: [u32; 255] = [
    0x391F22B4, 0x39EEB40E, 0x3A46EB61, 0x3A8B3E5E, 0x3AB3070B, 0x3ADACFB8, 0x3B014C33, 0x3B153089,
    0x3B2914DF, 0x3B3CF936, 0x3B50F2D1, 0x3B65FB9B, 0x3B7C3403, 0x3B89D060, 0x3B962333, 0x3BA314BD,
    0x3BB0A731, 0x3BBEDCB7, 0x3BCDB76D, 0x3BDD3967, 0x3BED64AF, 0x3BFE3B46, 0x3C07DF91, 0x3C10F91B,
    0x3C1A6B32, 0x3C2436C8, 0x3C2E5CC7, 0x3C38DE1A, 0x3C43BBA4, 0x3C4EF648, 0x3C5A8EE4, 0x3C668654,
    0x3C72DD71, 0x3C7F950F, 0x3C865702, 0x3C8D148F, 0x3C940396, 0x3C9B247C, 0x3CA277A6, 0x3CA9FD78,
    0x3CB1B653, 0x3CB9A298, 0x3CC1C2A9, 0x3CCA16E3, 0x3CD29FA4, 0x3CDB5D4B, 0x3CE45032, 0x3CED78B5,
    0x3CF6D72E, 0x3D0035FC, 0x3D051BB4, 0x3D0A1CEC, 0x3D0F39D0, 0x3D14728A, 0x3D19C745, 0x3D1F382C,
    0x3D24C567, 0x3D2A6F22, 0x3D303584, 0x3D3618B7, 0x3D3C18E4, 0x3D423632, 0x3D4870CA, 0x3D4EC8D2,
    0x3D553E73, 0x3D5BD1D3, 0x3D628318, 0x3D69526A, 0x3D703FEE, 0x3D774BCA, 0x3D7E7624, 0x3D82DF90,
    0x3D869372, 0x3D8A56CB, 0x3D8E29AB, 0x3D920C27, 0x3D95FE4F, 0x3D9A0035, 0x3D9E11EC, 0x3DA23384,
    0x3DA66510, 0x3DAAA6A0, 0x3DAEF847, 0x3DB35A15, 0x3DB7CC1B, 0x3DBC4E6B, 0x3DC0E114, 0x3DC58429,
    0x3DCA37B9, 0x3DCEFBD6, 0x3DD3D08F, 0x3DD8B5F5, 0x3DDDAC19, 0x3DE2B30A, 0x3DE7CAD9, 0x3DECF395,
    0x3DF22D50, 0x3DF77817, 0x3DFCD3FC, 0x3E012087, 0x3E03DFAE, 0x3E06A77B, 0x3E0977F6, 0x3E0C5126,
    0x3E0F3314, 0x3E121DC5, 0x3E151143, 0x3E180D95, 0x3E1B12C2, 0x3E1E20D1, 0x3E2137CB, 0x3E2457B6,
    0x3E278099, 0x3E2AB27D, 0x3E2DED68, 0x3E313161, 0x3E347E70, 0x3E37D49C, 0x3E3B33EC, 0x3E3E9C67,
    0x3E420E15, 0x3E4588FB, 0x3E490D22, 0x3E4C9A90, 0x3E50314C, 0x3E53D15D, 0x3E577ACA, 0x3E5B2D9A,
    0x3E5EE9D4, 0x3E62AF7E, 0x3E667E9F, 0x3E6A573E, 0x3E6E3962, 0x3E722511, 0x3E761A52, 0x3E7A192C,
    0x3E7E21A5, 0x3E8119E2, 0x3E8327C7, 0x3E853A86, 0x3E875222, 0x3E896E9D, 0x3E8B8FFC, 0x3E8DB641,
    0x3E8FE170, 0x3E92118B, 0x3E944696, 0x3E968095, 0x3E98BF89, 0x3E9B0377, 0x3E9D4C62, 0x3E9F9A4C,
    0x3EA1ED38, 0x3EA4452B, 0x3EA6A226, 0x3EA9042E, 0x3EAB6B44, 0x3EADD76D, 0x3EB048AA, 0x3EB2BF00,
    0x3EB53A71, 0x3EB7BB00, 0x3EBA40B1, 0x3EBCCB85, 0x3EBF5B81, 0x3EC1F0A7, 0x3EC48AF9, 0x3EC72A7C,
    0x3EC9CF32, 0x3ECC791E, 0x3ECF2842, 0x3ED1DCA2, 0x3ED49641, 0x3ED75521, 0x3EDA1946, 0x3EDCE2B2,
    0x3EDFB168, 0x3EE2856A, 0x3EE55EBD, 0x3EE83D63, 0x3EEB215D, 0x3EEE0AB1, 0x3EF0F95F, 0x3EF3ED6B,
    0x3EF6E6D8, 0x3EF9E5A8, 0x3EFCE9DE, 0x3EFFF37E, 0x3F018145, 0x3F030B82, 0x3F049877, 0x3F062827,
    0x3F07BA92, 0x3F094FB9, 0x3F0AE79F, 0x3F0C8244, 0x3F0E1FAA, 0x3F0FBFD2, 0x3F1162BE, 0x3F13086E,
    0x3F14B0E4, 0x3F165C22, 0x3F180A29, 0x3F19BAFA, 0x3F1B6E96, 0x3F1D24FF, 0x3F1EDE36, 0x3F209A3C,
    0x3F225913, 0x3F241ABC, 0x3F25DF38, 0x3F27A689, 0x3F2970AF, 0x3F2B3DAD, 0x3F2D0D83, 0x3F2EE032,
    0x3F30B5BD, 0x3F328E24, 0x3F346968, 0x3F36478B, 0x3F38288F, 0x3F3A0C73, 0x3F3BF33A, 0x3F3DDCE5,
    0x3F3FC975, 0x3F41B8EB, 0x3F43AB48, 0x3F45A08F, 0x3F4798BF, 0x3F4993DB, 0x3F4B91E3, 0x3F4D92D8,
    0x3F4F96BD, 0x3F519D92, 0x3F53A758, 0x3F55B411, 0x3F57C3BE, 0x3F59D65F, 0x3F5BEBF7, 0x3F5E0486,
    0x3F60200E, 0x3F623E90, 0x3F64600C, 0x3F668485, 0x3F68ABFB, 0x3F6AD670, 0x3F6D03E5, 0x3F6F345A,
    0x3F7167D2, 0x3F739E4D, 0x3F75D7CC, 0x3F781451, 0x3F7A53DD, 0x3F7C9671, 0x3F7EDC0E,
];

/// sRGB-encoded 8 bits to linear.
#[inline(always)]
pub fn srgb_to_linear(c: u8) -> f32 {
    f32::from_bits(SRGB_TO_LINEAR[c as usize])
}

/// Linear to sRGB-encoded 8 bits, rounded to nearest (exactly, by a search
/// of the rounding boundaries). NaN encodes as 0.
#[inline]
pub fn linear_to_srgb(x: f32) -> u8 {
    if x.is_nan() || x <= 0.0 {
        return 0;
    }
    LINEAR_TO_SRGB.partition_point(|&t| f32::from_bits(t) <= x) as u8
}

/// An unsigned 11-bit float (5-bit exponent, 6-bit mantissa) to f32.
pub fn uf11_to_f32(v: u32) -> f32 {
    small_float(v, 6)
}

/// An unsigned 10-bit float (5-bit exponent, 5-bit mantissa) to f32.
pub fn uf10_to_f32(v: u32) -> f32 {
    small_float(v, 5)
}

fn small_float(v: u32, mbits: u32) -> f32 {
    let e = (v >> mbits) & 31;
    let mant = v & ((1 << mbits) - 1);
    let mf = mant as f32 / (1u32 << mbits) as f32;
    match e {
        0 => mf * libm::exp2f(-14.0),
        31 => {
            if mant == 0 {
                f32::INFINITY
            } else {
                f32::NAN
            }
        }
        _ => (1.0 + mf) * libm::exp2f(e as f32 - 15.0),
    }
}

/// f32 to an unsigned 11-bit float (negative values become 0).
pub fn f32_to_uf11(x: f32) -> u32 {
    to_small_float(x, 6)
}

/// f32 to an unsigned 10-bit float.
pub fn f32_to_uf10(x: f32) -> u32 {
    to_small_float(x, 5)
}

fn to_small_float(x: f32, mbits: u32) -> u32 {
    if x.is_nan() {
        return (31 << mbits) | 1;
    }
    if x.partial_cmp(&0.0) != Some(core::cmp::Ordering::Greater) {
        return 0;
    }
    if x.is_infinite() {
        return 31 << mbits;
    }
    // Rounded once, to nearest even, from the float's own bits.
    let max_finite = (30 << mbits) | ((1 << mbits) - 1);
    let bits = x.to_bits();
    let exp = ((bits >> 23) & 0xFF) as i32 - 127;
    if exp > 15 {
        return max_finite;
    }
    let e = exp + 15;
    let mant = (bits & 0x7F_FFFF) | 0x80_0000;
    let shift = 23 - mbits as i32 + if e >= 1 { 0 } else { 1 - e };
    if shift > 24 {
        return 0;
    }
    let shift = shift as u32;
    let half = 1u32 << (shift - 1);
    let rem = mant & ((1 << shift) - 1);
    let mut q = mant >> shift;
    if rem > half || (rem == half && q & 1 == 1) {
        q += 1;
    }
    if e < 1 {
        // Subnormal; rounding up to 2^mbits reads as the smallest normal.
        return q;
    }
    let mut e = e as u32;
    if q >> (mbits + 1) != 0 {
        q >>= 1;
        e += 1;
    }
    if e >= 31 {
        return max_finite;
    }
    (e << mbits) | (q & ((1 << mbits) - 1))
}

/// Packs a color into RGB9_E5 (GL's shared-exponent rules).
pub fn f32_to_rgb9e5(r: f32, g: f32, b: f32) -> u32 {
    const MAX: f32 = 65408.0; // (2^9 - 1) / 2^9 * 2^16
    let c = |x: f32| if x.is_nan() || x <= 0.0 { 0.0 } else { x.min(MAX) };
    let (r, g, b) = (c(r), c(g), c(b));
    let max = r.max(g).max(b);
    if max == 0.0 {
        return 0;
    }
    // floor(log2(max)) from the float's exponent (exact; subnormals
    // fall under the -16 floor).
    let fl = ((max.to_bits() >> 23) & 0xFF) as i32 - 127;
    let mut e = fl.max(-16) + 1 + 15;
    let scale = |e: i32| libm::exp2f((e - 15 - 9) as f32);
    let maxs = libm::floorf(max / scale(e) + 0.5) as i32;
    if maxs == 512 {
        e += 1;
    }
    let s = scale(e);
    let q = |x: f32| (libm::floorf(x / s + 0.5) as u32).min(511);
    q(r) | (q(g) << 9) | (q(b) << 18) | ((e as u32 & 31) << 27)
}
