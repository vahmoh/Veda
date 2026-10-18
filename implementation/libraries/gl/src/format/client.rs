// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Pixel data as applications lay it out, a `(format, type)` pair, and its
//! conversion to and from [`Texel`]s.

use super::{Format as F, HALF_FLOAT_OES, Texel, clamp01};
use crate::gl;
use glsl::ops::{f16_to_f32, f32_to_f16};

/// A pixel layout in application memory: a `(format, type)` pair.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Client {
    pub format: u32,
    pub ty: u32,
}

impl Client {
    pub fn new(format: u32, ty: u32) -> Client {
        let ty = if ty == HALF_FLOAT_OES { gl::HALF_FLOAT } else { ty };
        Client { format, ty }
    }

    /// Components per pixel (packed types count their whole value as one
    /// group of all components).
    pub fn components(&self) -> usize {
        use gl::*;
        match self.format {
            RED | RED_INTEGER | DEPTH_COMPONENT | LUMINANCE | ALPHA => 1,
            RG | RG_INTEGER | LUMINANCE_ALPHA | DEPTH_STENCIL => 2,
            RGB | RGB_INTEGER => 3,
            _ => 4,
        }
    }

    /// Whether the type packs all components in one value.
    pub fn packed(&self) -> bool {
        use gl::*;
        matches!(
            self.ty,
            UNSIGNED_SHORT_5_6_5
                | UNSIGNED_SHORT_4_4_4_4
                | UNSIGNED_SHORT_5_5_5_1
                | UNSIGNED_INT_2_10_10_10_REV
                | UNSIGNED_INT_10F_11F_11F_REV
                | UNSIGNED_INT_5_9_9_9_REV
                | UNSIGNED_INT_24_8
                | FLOAT_32_UNSIGNED_INT_24_8_REV
        )
    }

    /// Bytes of one component (or of the packed value).
    pub fn type_bytes(&self) -> usize {
        use gl::*;
        match self.ty {
            UNSIGNED_BYTE | BYTE => 1,
            UNSIGNED_SHORT
            | SHORT
            | HALF_FLOAT
            | UNSIGNED_SHORT_5_6_5
            | UNSIGNED_SHORT_4_4_4_4
            | UNSIGNED_SHORT_5_5_5_1 => 2,
            FLOAT_32_UNSIGNED_INT_24_8_REV => 8,
            _ => 4,
        }
    }

    /// Bytes per pixel (one packed value, or a value per component).
    pub fn bytes_per_pixel(&self) -> usize {
        if self.packed() { self.type_bytes() } else { self.type_bytes() * self.components() }
    }

    fn integer(&self) -> bool {
        use gl::*;
        matches!(self.format, RED_INTEGER | RG_INTEGER | RGB_INTEGER | RGBA_INTEGER)
    }

    /// Decodes one pixel (GL's unpacking: normalised types to [0, 1] or
    /// [-1, 1], missing components 0, alpha 1).
    pub fn decode(&self, b: &[u8]) -> Texel {
        use gl::*;
        let n = self.components();
        let word = |i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
        let half = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
        match (self.format, self.ty) {
            (DEPTH_COMPONENT, UNSIGNED_SHORT) => return Texel::Depth(f32::from(half(0)) / 65535.0, 0),
            (DEPTH_COMPONENT, UNSIGNED_INT) => return Texel::Depth((f64::from(word(0)) / 4_294_967_295.0) as f32, 0),
            (DEPTH_COMPONENT, FLOAT) => return Texel::Depth(f32::from_bits(word(0)), 0),
            (DEPTH_STENCIL, UNSIGNED_INT_24_8) => {
                let v = word(0);
                return Texel::Depth((v >> 8) as f32 / 16_777_215.0, v as u8);
            }
            (DEPTH_STENCIL, FLOAT_32_UNSIGNED_INT_24_8_REV) => {
                return Texel::Depth(f32::from_bits(word(0)), b[4]);
            }
            (BGRA_EXT, UNSIGNED_BYTE) => {
                let n = |i: usize| f32::from(b[i]) / 255.0;
                return Texel::Float([n(2), n(1), n(0), n(3)]);
            }
            _ => {}
        }
        if self.packed() {
            let f = match self.ty {
                UNSIGNED_SHORT_5_6_5 => F::B5G6R5Unorm,
                UNSIGNED_SHORT_4_4_4_4 => F::Rgba4Unorm,
                UNSIGNED_SHORT_5_5_5_1 => F::Rgb5A1Unorm,
                UNSIGNED_INT_2_10_10_10_REV if self.integer() => F::Rgb10A2Uint,
                UNSIGNED_INT_2_10_10_10_REV => F::Rgb10A2Unorm,
                UNSIGNED_INT_10F_11F_11F_REV => F::R11G11B10Float,
                _ => F::Rgb9E5Float,
            };
            return f.decode(b);
        }
        let int = self.integer();
        let mut fv = [0.0f32, 0.0, 0.0, 1.0];
        let mut iv = [0i32, 0, 0, 1];
        let mut uv = [0u32, 0, 0, 1];
        let ts = self.type_bytes();
        for i in 0..n {
            let o = i * ts;
            match self.ty {
                UNSIGNED_BYTE => {
                    uv[i] = u32::from(b[o]);
                    fv[i] = f32::from(b[o]) / 255.0;
                }
                BYTE => {
                    iv[i] = i32::from(b[o] as i8);
                    fv[i] = (f32::from(b[o] as i8) / 127.0).max(-1.0);
                }
                UNSIGNED_SHORT => {
                    uv[i] = u32::from(half(o));
                    fv[i] = f32::from(half(o)) / 65535.0;
                }
                SHORT => {
                    iv[i] = i32::from(half(o) as i16);
                    fv[i] = (f32::from(half(o) as i16) / 32767.0).max(-1.0);
                }
                UNSIGNED_INT => {
                    uv[i] = word(o);
                    fv[i] = (f64::from(word(o)) / 4_294_967_295.0) as f32;
                }
                INT => {
                    iv[i] = word(o) as i32;
                    fv[i] = ((word(o) as i32) as f64 / 2_147_483_647.0).max(-1.0) as f32;
                }
                HALF_FLOAT => fv[i] = f16_to_f32(half(o)),
                FLOAT => fv[i] = f32::from_bits(word(o)),
                _ => {}
            }
        }
        if int {
            // The signedness comes from the type.
            return match self.ty {
                BYTE | SHORT | INT => Texel::Int(iv),
                _ => Texel::Uint(uv),
            };
        }
        // Spread luminance and alpha.
        match self.format {
            LUMINANCE => Texel::Float([fv[0], fv[0], fv[0], 1.0]),
            ALPHA => Texel::Float([0.0, 0.0, 0.0, fv[0]]),
            LUMINANCE_ALPHA => Texel::Float([fv[0], fv[0], fv[0], fv[1]]),
            _ => Texel::Float(fv),
        }
    }

    /// Encodes one pixel (`ReadPixels`): floats are clamped to [0, 1] for
    /// unsigned normalised types and rounded.
    pub fn encode(&self, t: &Texel, out: &mut [u8]) {
        use gl::*;
        let c = t.as_float();
        match (self.format, self.ty) {
            (_, UNSIGNED_INT_2_10_10_10_REV) => {
                let f = if self.integer() { F::Rgb10A2Uint } else { F::Rgb10A2Unorm };
                f.encode(t, out);
                return;
            }
            (_, UNSIGNED_SHORT_5_6_5) => return F::B5G6R5Unorm.encode(t, out),
            (_, UNSIGNED_SHORT_4_4_4_4) => return F::Rgba4Unorm.encode(t, out),
            (_, UNSIGNED_SHORT_5_5_5_1) => return F::Rgb5A1Unorm.encode(t, out),
            (_, UNSIGNED_INT_10F_11F_11F_REV) => return F::R11G11B10Float.encode(t, out),
            (_, UNSIGNED_INT_5_9_9_9_REV) => return F::Rgb9E5Float.encode(t, out),
            (BGRA_EXT, UNSIGNED_BYTE) => {
                let u = |x: f32| (clamp01(x) * 255.0 + 0.5) as u8;
                out[..4].copy_from_slice(&[u(c[2]), u(c[1]), u(c[0]), u(c[3])]);
                return;
            }
            _ => {}
        }
        let n = self.components();
        let ts = self.type_bytes();
        let src: [f32; 4] = match self.format {
            LUMINANCE => [c[0], 0.0, 0.0, 0.0],
            ALPHA => [c[3], 0.0, 0.0, 0.0],
            LUMINANCE_ALPHA => [c[0], c[3], 0.0, 0.0],
            _ => c,
        };
        let iv = t.as_int();
        let uv = t.as_uint();
        for i in 0..n {
            let o = i * ts;
            match self.ty {
                UNSIGNED_BYTE if self.integer() => out[o] = uv[i].min(255) as u8,
                BYTE if self.integer() => out[o] = iv[i].clamp(-128, 127) as i8 as u8,
                UNSIGNED_BYTE => out[o] = (clamp01(src[i]) * 255.0 + 0.5) as u8,
                BYTE => out[o] = libm::roundf(src[i].clamp(-1.0, 1.0) * 127.0) as i8 as u8,
                UNSIGNED_SHORT => out[o..o + 2].copy_from_slice(&(uv[i].min(65535) as u16).to_le_bytes()),
                SHORT => out[o..o + 2].copy_from_slice(&(iv[i].clamp(-32768, 32767) as i16).to_le_bytes()),
                UNSIGNED_INT => out[o..o + 4].copy_from_slice(&uv[i].to_le_bytes()),
                INT => out[o..o + 4].copy_from_slice(&iv[i].to_le_bytes()),
                HALF_FLOAT => out[o..o + 2].copy_from_slice(&f32_to_f16(src[i]).to_le_bytes()),
                FLOAT => out[o..o + 4].copy_from_slice(&src[i].to_le_bytes()),
                _ => {}
            }
        }
    }
}
