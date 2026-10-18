// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The default framebuffer: the buffers a window provides, which the
//! context makes at the window's size and shows in the window's pixels.

use crate::backend::{Framebuffer, Present, ResourceDesc, ResourceId, Surface, Target};
use crate::context::{Context, DefaultFramebuffer};
use crate::format::{self, Format};
use crate::gl;

impl Context {
    /// (Re)creates the default framebuffer's buffers at `width` x `height`
    /// (none for a surfaceless context).
    pub(crate) fn create_default_framebuffer(&mut self, width: u32, height: u32) {
        for r in [self.local.default_fb.color.take(), self.local.default_fb.depth_stencil.take()].into_iter().flatten()
        {
            self.backend.destroy_resource(r);
        }
        if self.local.config.surfaceless {
            self.local.default_fb =
                DefaultFramebuffer { draw_buffer: gl::BACK, read_buffer: gl::BACK, ..Default::default() };
            self.local.default_samples = 0;
            return;
        }
        let color = format::sized(self.local.config.color).or_else(|| format::sized(gl::RGBA8)).copied();
        let depth = match (self.local.config.depth_bits, self.local.config.stencil_bits) {
            (0, 0) => None,
            (0, _) => format::sized(gl::STENCIL_INDEX8),
            (16, 0) => format::sized(gl::DEPTH_COMPONENT16),
            (_, 0) => format::sized(gl::DEPTH_COMPONENT24),
            _ => format::sized(gl::DEPTH24_STENCIL8),
        }
        .copied();
        let samples = if self.local.config.samples > 0 && self.backend.caps().max_samples >= 4 { 4 } else { 0 };
        let (w, h) = (width.max(1), height.max(1));
        let make = |c: &mut Context, f: Format| {
            let desc = ResourceDesc {
                target: Target::Renderbuffer,
                format: f,
                width: w,
                height: h,
                depth: 1,
                levels: 1,
                samples,
            };
            c.backend.create_resource(&desc).ok()
        };
        let fb = &mut self.local.default_fb;
        fb.width = width;
        fb.height = height;
        fb.color_internal = color;
        fb.depth_internal = depth;
        if fb.draw_buffer == 0 {
            fb.draw_buffer = gl::BACK;
            fb.read_buffer = gl::BACK;
        }
        self.local.default_fb.color = color.and_then(|c| make(self, c.format));
        self.local.default_fb.depth_stencil = depth.and_then(|d| make(self, d.format));
        // What the renderer could not make the framebuffer has not, and the
        // GL's queries (DEPTH_BITS, ...) say so.
        if self.local.default_fb.color.is_none() {
            self.local.default_fb.color_internal = None;
        }
        if self.local.default_fb.depth_stencil.is_none() {
            self.local.default_fb.depth_internal = None;
        }
        self.local.default_samples = samples;
    }

    /// What the context in use draws into as its default framebuffer: a
    /// surface `width` x `height` (EGL's pbuffers), or none (a surfaceless
    /// context's). Buffers of another size, or none before, are made anew
    /// (their contents undefined).
    pub fn set_surface(&mut self, size: Option<(u32, u32)>) {
        let fb = &self.local.default_fb;
        match size {
            None if !self.local.config.surfaceless => {
                self.local.config.surfaceless = true;
                self.create_default_framebuffer(0, 0);
            }
            Some((w, h)) if self.local.config.surfaceless || (w, h) != (fb.width, fb.height) => {
                self.local.config.surfaceless = false;
                self.create_default_framebuffer(w, h);
            }
            _ => {}
        }
    }

    /// Resizes the default framebuffer (the window changed size). Its
    /// contents become undefined.
    pub fn resize(&mut self, width: u32, height: u32) {
        if (width, height) != (self.local.default_fb.width, self.local.default_fb.height) {
            self.create_default_framebuffer(width, height);
        }
    }

    /// The default framebuffer's color buffer and size, for presenting it.
    pub fn default_color_buffer(&self) -> Option<(ResourceId, Format, u32, u32)> {
        let fb = &self.local.default_fb;
        Some((fb.color?, fb.color_internal?.format, fb.width, fb.height))
    }

    /// The default framebuffer as a renderer sees it.
    pub(super) fn default_framebuffer(&self, draw: bool) -> Framebuffer {
        let fb = &self.local.default_fb;
        let mut out = Framebuffer {
            width: fb.width.max(1),
            height: fb.height.max(1),
            samples: self.local.default_samples,
            ..Default::default()
        };
        out.colors[0] = fb.color.map(|r| Surface { resource: r, level: 0, layer: 0 });
        let ds = fb.depth_stencil.map(|r| Surface { resource: r, level: 0, layer: 0 });
        let di = fb.depth_internal;
        out.depth = ds.filter(|_| di.is_some_and(|i| i.format.has_depth()));
        out.stencil = ds.filter(|_| di.is_some_and(|i| i.format.has_stencil()));
        let buffer = if draw { fb.draw_buffer } else { fb.read_buffer };
        out.draw_buffers[0] = (buffer == gl::BACK).then_some(0);
        out
    }

    /// Finishes rendering and shows the default framebuffer in a window's
    /// pixels, scaled to their size.
    pub fn present_to(&mut self, dst: &mut Present<'_>) {
        let Some((color, _, w, h)) = self.default_color_buffer() else { return };
        self.backend.present(color, w, h, dst);
    }
}
