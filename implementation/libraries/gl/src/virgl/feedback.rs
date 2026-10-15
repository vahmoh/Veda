// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Transform feedback where the host records only zeros.
//!
//! Some hosts' OpenGL drivers capture nothing but zeros from a vertex
//! shader that writes `gl_ClipDistance` (Intel's on Windows, desktop and
//! OpenGL ES alike), and virglrenderer's vertex shaders all do there, for
//! user clip planes. The first draw that captures checks the host with a
//! point whose values are known. Where they do not come back, the vertices
//! a draw captures are shaded in guest memory instead, by the software
//! renderer's vertex stage, from the buffers' contents as this renderer
//! keeps them (read back where the GPU wrote them); the host still draws
//! what the application sees.

use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use vglsl::interp::Env;

use super::VirglBackend;
use super::ops::{NO_BLEND, NO_TESTS, PLAIN_RASTER};
use crate::backend::*;
use crate::format::Format;
use crate::pixels::try_zeroed;
use crate::soft::program::SoftProgram;
use crate::soft::texture::{DrawTextures, Sampled, TexLevel};
use crate::soft::vertex;

const CHECK_VS: &str = "#version 300 es\nout vec4 v;\nvoid main() { v = vec4(1.0, 2.0, 3.0, 4.0); gl_Position = vec4(0.0, 0.0, 0.0, 1.0); }\n";
const CHECK_FS: &str = "#version 300 es\nprecision mediump float;\nout vec4 o;\nvoid main() { o = vec4(1.0); }\n";

/// A texture's levels read back for the vertex shader: how they are
/// sampled, and the texels, which `sampled` points into (moving a `Vec`
/// leaves its buffer where it is).
struct ReadBack {
    sampled: Sampled,
    texels: Vec<Vec<u8>>,
}

impl VirglBackend {
    /// Whether the host records transform feedback, found out at the first
    /// draw that captures.
    pub(super) fn feedback_works(&mut self) -> bool {
        if let Some(ok) = self.feedback_on_host {
            return ok;
        }
        // The check itself captures on the host.
        self.feedback_on_host = Some(true);
        let ok = self.unqueried(|b| b.check_feedback()).unwrap_or(false);
        self.feedback_on_host = Some(ok);
        ok
    }

    /// Captures one point's (1, 2, 3, 4) on the host and reads it back.
    fn check_feedback(&mut self) -> Option<bool> {
        let options = vglsl::Options::default();
        let vs = vglsl::compile(vglsl::Stage::Vertex, &[CHECK_VS], &options).shader?;
        let fs = vglsl::compile(vglsl::Stage::Fragment, &[CHECK_FS], &options).shader?;
        let bindings =
            vglsl::link::Bindings { attributes: Vec::new(), feedback: vec!["v".into()], feedback_separate: false };
        let p = vglsl::program::link(&vs, &fs, &bindings, &options.limits).program?;
        let program = self.program(Arc::new(p));
        let one =
            |target, format, width| ResourceDesc { target, format, width, height: 1, depth: 1, levels: 1, samples: 0 };
        let buffer = self.create(&one(Target::Buffer, Format::R8Unorm, 16)).ok();
        let target = self.create(&one(Target::Renderbuffer, Format::Rgba8Unorm, 1)).ok();
        let mut ok = None;
        if let (Some(buffer), Some(target)) = (buffer, target) {
            let mut fb = Framebuffer { width: 1, height: 1, ..Framebuffer::default() };
            fb.colors[0] = Some(Surface { resource: target, level: 0, layer: 0 });
            fb.draw_buffers[0] = Some(0);
            let ranges = [BufferRange { buffer, offset: 0, size: 16 }];
            let state = DrawState {
                framebuffer: &fb,
                viewport: Viewport { x: 0.0, y: 0.0, w: 1.0, h: 1.0, near: 0.0, far: 1.0 },
                scissor: None,
                raster: Raster { discard: true, ..PLAIN_RASTER },
                depth_stencil: NO_TESTS,
                blend: NO_BLEND,
                program,
                uniforms: &[],
                blocks: &[],
                textures: &[],
                attribs: &[],
                index: None,
                primitive_restart: false,
                feedback: Some(&ranges),
            };
            self.draw_call(&state, &DrawInfo { mode: Mode::Points, first: 0, count: 1, indexed: false, instances: 1 });
            let mut got = [0u8; 16];
            self.read_buffer(buffer, 0, &mut got);
            let want = [1.0f32, 2.0, 3.0, 4.0].map(f32::to_bits);
            ok = Some(got.as_chunks::<4>().0.iter().map(|b| u32::from_le_bytes(*b)).eq(want));
        }
        for r in [buffer, target].into_iter().flatten() {
            self.destroy(r);
        }
        self.drop_program(program);
        ok
    }

    /// Records what a draw's vertices capture by shading them in guest
    /// memory (`samplers`: the sampler indices its vertex shader uses).
    pub(super) fn feedback_in_guest(
        &mut self,
        source: &Arc<vglsl::program::Program>,
        samplers: &[u32],
        s: &DrawState<'_>,
        info: &DrawInfo,
        ranges: &[BufferRange],
    ) {
        let sp = self.guest_programs.entry(s.program).or_insert_with(|| Arc::new(SoftProgram::new(source.clone())));
        let sp = sp.clone();
        if sp.feedback.is_empty() {
            return;
        }
        // What the vertices are made of, as it is now.
        let mut buffers: Vec<u32> = s.attribs.iter().filter_map(|a| a.buffer).collect();
        buffers.extend(s.blocks.iter().flatten().map(|r| r.buffer));
        for b in buffers {
            self.current_shadow(b);
        }
        let (mut sampled, mut texels) = (Vec::new(), Vec::new());
        for i in 0..s.textures.len() {
            let read = if samplers.contains(&(i as u32)) { self.read_back_texture(&s.textures[i]) } else { None };
            let (one, its_texels) = read.map(|r| (r.sampled, r.texels)).unzip();
            sampled.push(one);
            texels.extend(its_texels);
        }
        let recorded = {
            let bytes = |id: u32| self.resources.get(&id).and_then(|r| r.shadow.as_ref()).map(|s| s.data.as_slice());
            let fetch = vertex::fetches(s.attribs, bytes);
            let blocks = vertex::block_snapshots(&sp.program, s.blocks, bytes);
            let block_refs: Vec<&[u8]> = blocks.iter().map(Vec::as_slice).collect();
            let tex = DrawTextures { bound: &sampled };
            let env = Env { uniforms: s.uniforms, blocks: &block_refs, textures: &tex };
            vertex::capture(&sp, &fetch, &env, info)
        };
        drop(texels);
        let Some(recorded) = recorded else { return };
        for (bi, range) in ranges.iter().enumerate() {
            if let Some(data) = vertex::feedback_bytes(&sp, bi, range.size, &recorded) {
                self.write_buffer(range.buffer, range.offset, &data);
            }
        }
    }

    /// A texture's levels, read back as a sampler of the vertex shader sees
    /// them.
    fn read_back_texture(&mut self, b: &TextureBinding) -> Option<ReadBack> {
        let v = b.view?;
        let d = self.resources.get(&v.resource)?.desc;
        let tb = d.format.bytes();
        let (mut texels, mut levels) = (Vec::new(), Vec::new());
        for l in v.base_level..=v.max_level.min(d.levels.saturating_sub(1)) {
            let (w, h, depth) = d.level_size(l);
            let (row, image) = (w as usize * tb, w as usize * h as usize * tb);
            let mut data = try_zeroed(image * depth as usize)?;
            self.read_texture(v.resource, l, Region::new(0, 0, 0, w, h, depth), &mut data, row, image);
            levels.push(TexLevel { ptr: data.as_ptr(), width: w, height: h, depth, row, image });
            texels.push(data);
        }
        if levels.is_empty() {
            return None;
        }
        Some(ReadBack { sampled: Sampled::new(&v, b.sampler, d.format, levels), texels })
    }
}
