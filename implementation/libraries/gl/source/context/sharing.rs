// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Share groups (OpenGL ES 3.0, appendix D): contexts that share their
//! buffers, textures, renderbuffers, samplers, shaders and programs, and
//! sync objects, are one [`Context`] with a [`Local`] for each, of which
//! the one in use is the context's own part.
//!
//! A context's bindings refer to shared objects without keeping them
//! (containers keep what they refer to): deleting an object unbinds it from
//! the context that deletes it, and from another when that one is next in
//! use. The program a context uses stays in use, as its executable lives
//! while it is.

use super::{Context, IndexedBinding, Local};

impl Context {
    /// Ends the bindings of the context now in use to objects another
    /// context deleted meanwhile.
    pub(crate) fn forget_deleted(&mut self) {
        let Context { textures, samplers, buffers, renderbuffers, programs, local, .. } = self;
        let defaults = local.default_textures;
        for unit in &mut local.texture_units {
            for (t, key) in unit.iter_mut().enumerate() {
                if !textures.contains(*key) {
                    *key = defaults[t];
                }
            }
        }
        for unit in &mut local.sampler_units {
            if unit.is_some_and(|k| !samplers.contains(k)) {
                *unit = None;
            }
        }
        let b = &mut local.bound;
        for slot in
            [&mut b.array, &mut b.copy_read, &mut b.copy_write, &mut b.pixel_pack, &mut b.pixel_unpack, &mut b.uniform]
        {
            if slot.is_some_and(|k| !buffers.contains(k)) {
                *slot = None;
            }
        }
        for i in &mut b.uniform_indexed {
            if i.buffer.is_some_and(|k| !buffers.contains(k)) {
                *i = IndexedBinding::default();
            }
        }
        if local.renderbuffer.is_some_and(|k| !renderbuffers.contains(k)) {
            local.renderbuffer = None;
        }
        if local.program != 0 && !programs.objects.contains_key(&local.program) {
            local.program = 0;
        }
    }

    /// Lets go of what the context now in use has of its own, which then
    /// goes: its transform feedback and queries end, its objects are
    /// deleted (and what they kept of the shared ones with them), and so is
    /// its default framebuffer.
    pub(crate) fn release_local(&mut self) {
        if self.tf().active {
            self.end_transform_feedback();
        }
        for k in self.local.active_queries.into_iter().flatten() {
            let target = self.local.queries.get(k).target;
            self.end_query(target);
        }
        self.use_program(0);
        let names = self.local.framebuffers.names();
        self.delete_framebuffers(&names);
        let names = self.local.vertex_arrays.names();
        self.delete_vertex_arrays(&names);
        let names = self.local.feedbacks.names();
        self.delete_transform_feedbacks(&names);
        let names = self.local.queries.names();
        self.delete_queries(&names);
        let vao = core::mem::take(&mut self.local.default_vao);
        self.release_vao_buffers(&vao);
        let feedback = core::mem::take(&mut self.local.default_feedback);
        self.release_buffer(feedback.generic);
        for b in feedback.bindings {
            self.release_buffer(b.buffer);
        }
        for key in self.local.default_textures {
            self.release_texture(key);
        }
        let fb = &mut self.local.default_fb;
        for r in [fb.color.take(), fb.depth_stencil.take()].into_iter().flatten() {
            self.backend.destroy_resource(r);
        }
    }
}

/// A [`Local`] is only ever in use by the thread that has its context in
/// use, which the context's user (EGL) sees to.
const _: () = {
    const fn sendable<T: Send>() {}
    sendable::<Local>();
};
