// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What passes, drawn: skid marks laid on the road, smoke, spray and
//! sparks in the air, and the rain round the camera.
//!
//! The skid marks' buffer mirrors the game's ring of them: only the
//! segments made since the last frame are copied. The particles are copied
//! every frame (smoke and spray, blended; then the sparks, added), into
//! instance buffers rewritten in place. The rain is drawn from a fixed set
//! of drops falling through a box that follows the camera, all of it
//! worked out on the GPU.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;

use rand::{RngExt, SeedableRng};
use rand_pcg::Pcg32;
use vgl::{Context, gl};

use super::gpu::{self, Program};
use super::instances::{self, Instances};
use super::shaders;
use super::{Camera, Quality, Stats, lit_program};
use crate::effects::{Kind, MAX_SKIDS, Particle, Skid};
use crate::prelude::*;

/// What passes, for a frame.
#[derive(Clone, Copy, Debug)]
pub struct Passing<'a> {
    pub particles: &'a VecDeque<Particle>,
    /// The ring of skid marks, and how many were ever made.
    pub skids: &'a [Skid],
    pub skids_made: u64,
    /// How hard it rains where the camera is (0..1: none under a roof).
    pub rain: f32,
}

/// No particles.
static NO_PARTICLES: VecDeque<Particle> = VecDeque::new();

impl Default for Passing<'_> {
    /// Nothing passing.
    fn default() -> Self {
        Passing { particles: &NO_PARTICLES, skids: &[], skids_made: 0, rain: 0.0 }
    }
}

/// Floats in a skid mark's vertex: position, then when it was made, how
/// dark, and where across the mark.
const SKID_FLOATS: usize = 6;
/// A mark's width (m).
const SKID_WIDTH: f32 = 0.24;
/// Floats per particle instance.
const PARTICLE_FLOATS: usize = 12;

/// The GPU's side of what passes.
pub struct PassingGpu {
    skid: Program,
    particle: Program,
    rain: Program,
    skid_vao: u32,
    skid_vbo: u32,
    /// Segments copied so far.
    skids_copied: u64,
    /// The new segments' vertices, as floats and as bytes (each frame's
    /// working).
    skid_floats: Vec<f32>,
    skid_bytes: Vec<u8>,
    /// The particles, blended (smoke, spray) and added (sparks); and this
    /// frame's of each.
    smoke: Instances,
    sparks: Instances,
    soft: Vec<f32>,
    hot: Vec<f32>,
    drops: Instances,
}

impl PassingGpu {
    pub fn new(gl: &mut Context, defines: &str, quality: Quality) -> Result<PassingGpu, String> {
        let lit = |gl: &mut Context, name: &str, vs: &str, fs: &str, uniforms: &[&'static str]| {
            lit_program(gl, defines, name, vs, fs, uniforms, None)
        };
        let skid = lit(gl, "skid marks", shaders::SKID_VS, shaders::SKID_FS, &[])?;
        let particle = lit(gl, "particles", shaders::PARTICLE_VS, shaders::PARTICLE_FS, &["u_right", "u_up"])?;
        let rain = lit(gl, "rain", shaders::RAIN_VS, shaders::RAIN_FS, &["u_box", "u_fall", "u_rain"])?;

        let skid_vao = gl.gen_vertex_array();
        gl.bind_vertex_array(skid_vao);
        let skid_vbo = gl.gen_buffer();
        gl.bind_buffer(gl::ARRAY_BUFFER, skid_vbo);
        gl.buffer_data_size(gl::ARRAY_BUFFER, MAX_SKIDS * 6 * SKID_FLOATS * 4, gl::DYNAMIC_DRAW);
        let stride = (SKID_FLOATS * 4) as i32;
        gl.vertex_attrib_pointer(gpu::attr::POS, 3, gl::FLOAT, false, stride, 0);
        gl.vertex_attrib_pointer(gpu::attr::NORMAL, 3, gl::FLOAT, false, stride, 12);
        gl.enable_vertex_attrib_array(gpu::attr::POS);
        gl.enable_vertex_attrib_array(gpu::attr::NORMAL);
        gl.bind_vertex_array(0);

        let quads = |gl: &mut Context| {
            let (vao, _) = instances::quad(gl);
            Instances::new(gl, vao, PARTICLE_FLOATS)
        };
        let (smoke, sparks) = (quads(gl), quads(gl));

        // The drops: where each is in the box, and a number deciding
        // whether it falls in a given rain (all of them in a downpour).
        let drops = if quality == Quality::High { 7000 } else { 2000 };
        let mut rng = Pcg32::seed_from_u64(0x4A1D);
        let data: Vec<f32> = (0..drops * 4).map(|_| rng.random::<f32>()).collect();
        let (vao, _) = instances::quad(gl);
        let drops = Instances::fixed(gl, vao, 4, &data);
        Ok(PassingGpu {
            skid,
            particle,
            rain,
            skid_vao,
            skid_vbo,
            skids_copied: 0,
            skid_floats: Vec::new(),
            skid_bytes: Vec::new(),
            smoke,
            sparks,
            soft: Vec::new(),
            hot: Vec::new(),
            drops,
        })
    }

    /// Copies the skid marks made since the last frame.
    fn copy_skids(&mut self, gl: &mut Context, skids: &[Skid], made: u64) {
        if made < self.skids_copied {
            // A new game: its marks start again.
            self.skids_copied = 0;
        }
        let cap = MAX_SKIDS as u64;
        let from = self.skids_copied.max(made.saturating_sub(cap));
        if from == made {
            return;
        }
        gl.bind_buffer(gl::ARRAY_BUFFER, self.skid_vbo);
        // In at most two runs: up to the ring's end, and from its start.
        let mut at = from;
        while at < made {
            let slot = (at % cap) as usize;
            let run = ((made - at) as usize).min(MAX_SKIDS - slot);
            let v = &mut self.skid_floats;
            v.clear();
            for s in &skids[slot..slot + run] {
                let dir = s.b - s.a;
                let side = Vec3::new(dir.z, 0.0, -dir.x).normalize_or(Vec3::X) * (SKID_WIDTH * 0.5);
                let corner = |p: Vec3, across: f32, v: &mut Vec<f32>| {
                    v.extend_from_slice(&[p.x, p.y, p.z, s.born, s.dark, across]);
                };
                // Counter-clockwise seen from above.
                corner(s.a - side, -1.0, v);
                corner(s.b + side, 1.0, v);
                corner(s.a + side, 1.0, v);
                corner(s.a - side, -1.0, v);
                corner(s.b - side, -1.0, v);
                corner(s.b + side, 1.0, v);
            }
            gpu::put_floats(&mut self.skid_bytes, v);
            gl.buffer_sub_data(gl::ARRAY_BUFFER, slot * 6 * SKID_FLOATS * 4, &self.skid_bytes);
            at += run as u64;
        }
        gl.bind_buffer(gl::ARRAY_BUFFER, 0);
        self.skids_copied = made;
    }

    /// The skid marks, over the road just drawn (before what stands on it).
    pub fn draw_skids(&mut self, gl: &mut Context, passing: &Passing, stats: &mut Stats) {
        self.copy_skids(gl, passing.skids, passing.skids_made);
        let n = passing.skids_made.min(MAX_SKIDS as u64) as i32;
        if n == 0 {
            return;
        }
        gl.use_program(self.skid.id);
        gl.enable(gl::BLEND);
        gl.blend_func(gl::SRC_ALPHA, gl::ONE_MINUS_SRC_ALPHA);
        gl.depth_mask(false);
        gl.depth_func(gl::LEQUAL);
        gl.enable(gl::POLYGON_OFFSET_FILL);
        gl.polygon_offset(-1.0, -2.0);
        gl.bind_vertex_array(self.skid_vao);
        gl.draw_arrays(gl::TRIANGLES, 0, n * 6);
        gl.disable(gl::POLYGON_OFFSET_FILL);
        gl.depth_func(gl::LESS);
        gl.depth_mask(true);
        gl.disable(gl::BLEND);
        gl.bind_vertex_array(0);
        stats.count(n * 6);
    }

    /// Smoke, spray, sparks and rain, over everything solid (tested against
    /// it, not hiding anything).
    pub fn draw_air(&mut self, gl: &mut Context, passing: &Passing, camera: &Camera, stats: &mut Stats) {
        let view = camera.view();
        let (right, up) = (view.row(0).xyz(), view.row(1).xyz());
        // The particles, in two lists: the blended, the added.
        let (soft, hot) = (&mut self.soft, &mut self.hot);
        soft.clear();
        hot.clear();
        for p in passing.particles {
            let spent = p.spent();
            let fade_in = (p.age / 0.12).min(1.0);
            let (list, kind, color, alpha) = match p.kind {
                Kind::Smoke => (&mut *soft, 0.0, Vec3::new(0.36, 0.36, 0.38), 0.18 * (1.0 - spent).powi(2) * fade_in),
                Kind::Spray => (&mut *soft, 1.0, Vec3::new(0.6, 0.63, 0.68), 0.16 * (1.0 - spent) * fade_in),
                Kind::Spark => (&mut *hot, 2.0, Vec3::new(12.0, 4.8, 1.0), 1.0 - spent * spent),
            };
            let (c, v) = (p.center(), p.vel);
            list.extend_from_slice(&[
                c.x,
                c.y,
                c.z,
                p.size_now(),
                v.x,
                v.y,
                v.z,
                kind,
                color.x,
                color.y,
                color.z,
                alpha,
            ]);
        }
        self.smoke.fill(gl, soft);
        self.sparks.fill(gl, hot);
        gl.disable(gl::CULL_FACE);
        gl.enable(gl::BLEND);
        gl.depth_mask(false);
        if self.smoke.count() > 0 || self.sparks.count() > 0 {
            gl.use_program(self.particle.id);
            gl.uniform3f(self.particle.at("u_right"), right.x, right.y, right.z);
            gl.uniform3f(self.particle.at("u_up"), up.x, up.y, up.z);
            for (particles, additive) in [(&self.smoke, false), (&self.sparks, true)] {
                if particles.count() == 0 {
                    continue;
                }
                if additive {
                    gl.blend_func(gl::ONE, gl::ONE);
                } else {
                    gl.blend_func(gl::SRC_ALPHA, gl::ONE_MINUS_SRC_ALPHA);
                }
                particles.draw_quads(gl);
                stats.count_instanced(6, particles.count());
            }
        }
        if passing.rain > 0.0 {
            gl.use_program(self.rain.id);
            // Through a box of 34 by 22 by 34 m, falling at (1.4, -11, 0.9)
            // m/s: whole boxes in super::TIME_WRAP seconds (the wrap unseen).
            gl.uniform3f(self.rain.at("u_box"), 34.0, 22.0, 34.0);
            gl.uniform3f(self.rain.at("u_fall"), 1.4, -11.0, 0.9);
            gl.uniform1f(self.rain.at("u_rain"), passing.rain.min(1.0));
            gl.blend_func(gl::SRC_ALPHA, gl::ONE);
            self.drops.draw_quads(gl);
            stats.count_instanced(6, self.drops.count());
        }
        gl.depth_mask(true);
        gl.disable(gl::BLEND);
        gl.enable(gl::CULL_FACE);
        gl.bind_vertex_array(0);
    }
}
