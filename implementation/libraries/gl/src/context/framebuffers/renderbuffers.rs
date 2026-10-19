// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Renderbuffer objects: images that are only rendered to, with storage of
//! their own (multisampled, if asked for) or memory from outside, and their
//! queries.

use alloc::vec::Vec;

use crate::backend::{ResourceDesc, Target};
use crate::context::{Attachment, Context, Renderbuffer};
use crate::format::{self, Internal};
use crate::gl;

impl Context {
    /// `glGenRenderbuffers`.
    pub fn gen_renderbuffers(&mut self, names: &mut [u32]) {
        for n in names {
            *n = self.renderbuffers.generate();
        }
    }

    /// One renderbuffer name.
    pub fn gen_renderbuffer(&mut self) -> u32 {
        self.renderbuffers.generate()
    }

    /// `glIsRenderbuffer`.
    pub fn is_renderbuffer(&self, name: u32) -> bool {
        self.renderbuffers.key(name).is_some()
    }

    /// `glBindRenderbuffer`.
    pub fn bind_renderbuffer(&mut self, target: u32, name: u32) {
        if target != gl::RENDERBUFFER {
            return self.err(gl::INVALID_ENUM);
        }
        self.local.renderbuffer =
            if name == 0 { None } else { Some(self.renderbuffers.get_or_create(name, Renderbuffer::default)) };
    }

    /// `glDeleteRenderbuffers`.
    pub fn delete_renderbuffers(&mut self, names: &[u32]) {
        for &name in names {
            if let Some(key) = self.renderbuffers.key(name) {
                if self.local.renderbuffer == Some(key) {
                    self.local.renderbuffer = None;
                }
                self.detach_from_bound_framebuffers(|a| a == Attachment::Renderbuffer(key));
            }
            if let Some((_, Some(dead))) = self.renderbuffers.delete(name) {
                self.destroy_renderbuffer(dead);
            }
        }
    }

    pub(super) fn destroy_renderbuffer(&mut self, rb: Renderbuffer) {
        if let Some(r) = rb.resource {
            self.backend.destroy_resource(r);
        }
    }

    /// The sample counts the renderer supports for a format (descending).
    pub(crate) fn sample_counts(&self, i: &Internal) -> &'static [u32] {
        let renderable = i.renderable || i.float_renderable || i.format.has_depth() || i.format.has_stencil();
        if !renderable || i.format.is_integer() || self.backend.caps().max_samples < 4 { &[] } else { &[4] }
    }

    /// `glRenderbufferStorage`.
    pub fn renderbuffer_storage(&mut self, target: u32, internalformat: u32, width: i32, height: i32) {
        self.renderbuffer_storage_multisample(target, 0, internalformat, width, height);
    }

    /// `glRenderbufferStorageMultisample`.
    pub fn renderbuffer_storage_multisample(
        &mut self,
        target: u32,
        samples: i32,
        internalformat: u32,
        width: i32,
        height: i32,
    ) {
        if target != gl::RENDERBUFFER {
            return self.err(gl::INVALID_ENUM);
        }
        let caps_float = self.backend.caps().color_buffer_float;
        let Some(i) = format::sized(internalformat).copied().filter(|i| {
            i.renderable || (i.float_renderable && caps_float) || i.format.has_depth() || i.format.has_stencil()
        }) else {
            return self.err(gl::INVALID_ENUM);
        };
        let max = self.backend.caps().max_renderbuffer_size as i32;
        if samples < 0 || width < 0 || height < 0 || width > max || height > max {
            return self.err(gl::INVALID_VALUE);
        }
        if samples > 0 && i.format.is_integer() {
            return self.err(gl::INVALID_OPERATION);
        }
        let counts = self.sample_counts(&i);
        let samples = if samples == 0 {
            0
        } else {
            match counts.iter().rev().find(|&&c| c >= samples as u32) {
                Some(&c) => c,
                None => return self.err(gl::INVALID_OPERATION),
            }
        };
        let Some(key) = self.local.renderbuffer else { return self.err(gl::INVALID_OPERATION) };
        let (w, h) = (width as u32, height as u32);
        let resource = if w > 0 && h > 0 {
            let desc = ResourceDesc {
                target: Target::Renderbuffer,
                format: i.format,
                width: w,
                height: h,
                depth: 1,
                levels: 1,
                samples,
            };
            match self.backend.create_resource(&desc) {
                Ok(r) => Some(r),
                Err(_) => return self.err(gl::OUT_OF_MEMORY),
            }
        } else {
            None
        };
        let rb = self.renderbuffers.get_mut(key);
        let old = core::mem::replace(rb, Renderbuffer { internal: Some(i), width: w, height: h, samples, resource });
        self.destroy_renderbuffer(old);
    }

    /// Makes the bound renderbuffer's storage memory from outside: a
    /// picture of `width` x `height` pixels, which the GPU then renders into
    /// in place (what `glEGLImageTargetRenderbufferStorageOES` does with an
    /// image of a display's buffer, or of a window's). `internalformat` is
    /// `GL_RGBA8`, or `GL_RGB8`, whose pixels' fourth byte is never read.
    /// Fails with `GL_INVALID_OPERATION` if the renderer cannot take such
    /// memory, `GL_OUT_OF_MEMORY` if not now.
    pub fn renderbuffer_storage_external(
        &mut self,
        target: u32,
        internalformat: u32,
        width: i32,
        height: i32,
        memory: &crate::backend::External,
    ) {
        if target != gl::RENDERBUFFER {
            return self.err(gl::INVALID_ENUM);
        }
        let Some(i) = format::sized(internalformat).copied().filter(|i| matches!(i.gl, gl::RGBA8 | gl::RGB8)) else {
            return self.err(gl::INVALID_ENUM);
        };
        let max = self.backend.caps().max_renderbuffer_size as i32;
        let rows = (memory.stride() as usize).checked_mul(height.max(0) as usize);
        if width <= 0 || height <= 0 || width > max || height > max || (memory.stride() as usize) < width as usize * 4 {
            return self.err(gl::INVALID_VALUE);
        }
        if rows.is_none_or(|bytes| bytes > memory.size()) {
            return self.err(gl::INVALID_VALUE);
        }
        let Some(key) = self.local.renderbuffer else { return self.err(gl::INVALID_OPERATION) };
        let (w, h) = (width as u32, height as u32);
        let desc = ResourceDesc {
            target: Target::Renderbuffer,
            format: i.format,
            width: w,
            height: h,
            depth: 1,
            levels: 1,
            samples: 0,
        };
        let resource = match self.backend.import_resource(&desc, memory) {
            Ok(r) => r,
            Err(crate::backend::ImportError::Unsupported) => return self.err(gl::INVALID_OPERATION),
            Err(crate::backend::ImportError::NoMemory) => return self.err(gl::OUT_OF_MEMORY),
        };
        let rb = self.renderbuffers.get_mut(key);
        let old = core::mem::replace(
            rb,
            Renderbuffer { internal: Some(i), width: w, height: h, samples: 0, resource: Some(resource) },
        );
        self.destroy_renderbuffer(old);
    }

    /// `glGetRenderbufferParameteriv` (one value).
    pub fn get_renderbuffer_parameteri(&mut self, target: u32, pname: u32) -> i32 {
        if target != gl::RENDERBUFFER {
            self.err(gl::INVALID_ENUM);
            return 0;
        }
        let Some(key) = self.local.renderbuffer else {
            self.err(gl::INVALID_OPERATION);
            return 0;
        };
        let rb = *self.renderbuffers.get(key);
        let bits = rb.internal.map_or([0; 6], |i| i.bits);
        match pname {
            gl::RENDERBUFFER_WIDTH => rb.width as i32,
            gl::RENDERBUFFER_HEIGHT => rb.height as i32,
            gl::RENDERBUFFER_INTERNAL_FORMAT => rb.internal.map_or(gl::RGBA4, |i| i.gl) as i32,
            gl::RENDERBUFFER_SAMPLES => rb.samples as i32,
            gl::RENDERBUFFER_RED_SIZE => i32::from(bits[0]),
            gl::RENDERBUFFER_GREEN_SIZE => i32::from(bits[1]),
            gl::RENDERBUFFER_BLUE_SIZE => i32::from(bits[2]),
            gl::RENDERBUFFER_ALPHA_SIZE => i32::from(bits[3]),
            gl::RENDERBUFFER_DEPTH_SIZE => i32::from(bits[4]),
            gl::RENDERBUFFER_STENCIL_SIZE => i32::from(bits[5]),
            _ => {
                self.err(gl::INVALID_ENUM);
                0
            }
        }
    }

    /// `glGetInternalformativ`.
    pub fn get_internalformativ(&mut self, target: u32, internalformat: u32, pname: u32, out: &mut [i32]) {
        if target != gl::RENDERBUFFER {
            return self.err(gl::INVALID_ENUM);
        }
        let caps_float = self.backend.caps().color_buffer_float;
        let Some(i) = format::sized(internalformat).copied().filter(|i| {
            i.renderable || (i.float_renderable && caps_float) || i.format.has_depth() || i.format.has_stencil()
        }) else {
            return self.err(gl::INVALID_ENUM);
        };
        let counts: Vec<i32> = self.sample_counts(&i).iter().map(|&c| c as i32).collect();
        match pname {
            gl::NUM_SAMPLE_COUNTS => crate::context::vertex::fill(out, &[counts.len() as i32]),
            gl::SAMPLES => crate::context::vertex::fill(out, &counts),
            _ => self.err(gl::INVALID_ENUM),
        }
    }
}
