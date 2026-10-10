//! What passes, drawn: skid marks laid on the road, smoke, spray and
//! sparks in the air, and the rain round the camera.
//!
//! The skid marks' buffer mirrors the game's ring of them: only the
//! segments made since the last frame are copied. The particles are copied
//! every frame (smoke and spray, blended; then the sparks, added). The rain
//! is drawn from a fixed set of drops falling through a box that follows
//! the camera, all of it worked out on the GPU.

use alloc::string::String;
use alloc::vec::Vec;

use vgl::{Context, gl};

use super::gpu::{self, Program, float_bytes};
use super::shaders::{self, source};
use super::{Camera, Quality, Stats};
use crate::effects::{Kind, MAX_SKIDS, Particle, Skid};
use crate::prelude::*;

/// What passes, for a frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct Passing<'a> {
    pub particles: &'a [Particle],
    /// The ring of skid marks, and how many were ever made.
    pub skids: &'a [Skid],
    pub skids_made: u64,
    /// How hard it rains where the camera is (0..1: none under a roof).
    pub rain: f32,
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
    smoke: (u32, u32),
    sparks: (u32, u32),
    rain_vao: u32,
    drops: i32,
}

/// A vertex array of instanced quads (corners at -1 and 1) reading
/// `floats` floats an instance from its own buffer; and that buffer.
fn quads(gl: &mut Context, floats: usize, data: &[f32]) -> (u32, u32) {
    let vao = gl.gen_vertex_array();
    gl.bind_vertex_array(vao);
    let corners = gl.gen_buffer();
    gl.bind_buffer(gl::ARRAY_BUFFER, corners);
    let quad = [-1.0, -1.0, 1.0, -1.0, 1.0, 1.0, -1.0, -1.0, 1.0, 1.0, -1.0, 1.0];
    gl.buffer_data(gl::ARRAY_BUFFER, &float_bytes(&quad), gl::STATIC_DRAW);
    gl.vertex_attrib_pointer(gpu::attr::POS, 2, gl::FLOAT, false, 0, 0);
    gl.enable_vertex_attrib_array(gpu::attr::POS);
    let instances = gl.gen_buffer();
    gl.bind_buffer(gl::ARRAY_BUFFER, instances);
    gl.buffer_data(gl::ARRAY_BUFFER, &float_bytes(data), gl::STATIC_DRAW);
    gpu::set_instance_layout(gl, floats, 0);
    gl.bind_vertex_array(0);
    (vao, instances)
}

impl PassingGpu {
    pub fn new(gl: &mut Context, defines: &str, quality: Quality) -> Result<PassingGpu, String> {
        let lit = |gl: &mut Context, name: &str, vs: &str, fs: &str, uniforms: &[&'static str]| {
            Program::new(gl, name, &source(defines, true, vs), &source(defines, true, fs), uniforms, &gpu::LIT_SAMPLERS)
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

        let smoke = quads(gl, PARTICLE_FLOATS, &[]);
        let sparks = quads(gl, PARTICLE_FLOATS, &[]);

        // The drops: where each is in the box, and a number deciding
        // whether it falls in a given rain (all of them in a downpour).
        let drops = if quality == Quality::High { 7000 } else { 2000 };
        let mut rng = vmath::Rng::new(0x4A1D);
        let data: Vec<f32> = (0..drops * 4).map(|_| rng.next_f32()).collect();
        let (rain_vao, _) = quads(gl, 4, &data);
        Ok(PassingGpu { skid, particle, rain, skid_vao, skid_vbo, skids_copied: 0, smoke, sparks, rain_vao, drops })
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
            let mut v = Vec::with_capacity(run * 6 * SKID_FLOATS);
            for s in &skids[slot..slot + run] {
                let dir = s.b - s.a;
                let side = Vec3::new(dir.z, 0.0, -dir.x).normalize_or(Vec3::X) * (SKID_WIDTH * 0.5);
                let corner = |p: Vec3, across: f32, v: &mut Vec<f32>| {
                    v.extend_from_slice(&[p.x, p.y, p.z, s.born, s.dark, across]);
                };
                // Counter-clockwise seen from above.
                corner(s.a - side, -1.0, &mut v);
                corner(s.b + side, 1.0, &mut v);
                corner(s.a + side, 1.0, &mut v);
                corner(s.a - side, -1.0, &mut v);
                corner(s.b - side, -1.0, &mut v);
                corner(s.b + side, 1.0, &mut v);
            }
            gl.buffer_sub_data(gl::ARRAY_BUFFER, slot * 6 * SKID_FLOATS * 4, &float_bytes(&v));
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
        stats.draws += 1;
        stats.triangles += (n * 2) as u32;
    }

    /// Smoke, spray, sparks and rain, over everything solid (tested against
    /// it, not hiding anything).
    pub fn draw_air(&mut self, gl: &mut Context, passing: &Passing, camera: &Camera, stats: &mut Stats) {
        let view = camera.view();
        let (right, up) = (view.row(0).xyz(), view.row(1).xyz());
        // The particles, in two lists: the blended, the added.
        let (mut soft, mut hot) = (Vec::new(), Vec::new());
        for p in passing.particles {
            let spent = p.spent();
            let fade_in = (p.age / 0.12).min(1.0);
            let (list, kind, color, alpha) = match p.kind {
                Kind::Smoke => (&mut soft, 0.0, Vec3::new(0.36, 0.36, 0.38), 0.18 * (1.0 - spent).powi(2) * fade_in),
                Kind::Spray => (&mut soft, 1.0, Vec3::new(0.6, 0.63, 0.68), 0.16 * (1.0 - spent) * fade_in),
                Kind::Spark => (&mut hot, 2.0, Vec3::new(12.0, 4.8, 1.0), 1.0 - spent * spent),
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
        gl.disable(gl::CULL_FACE);
        gl.enable(gl::BLEND);
        gl.depth_mask(false);
        if !soft.is_empty() || !hot.is_empty() {
            gl.use_program(self.particle.id);
            gl.uniform3f(self.particle.at("u_right"), right.x, right.y, right.z);
            gl.uniform3f(self.particle.at("u_up"), up.x, up.y, up.z);
            for (data, (vao, buf), additive) in [(&soft, self.smoke, false), (&hot, self.sparks, true)] {
                if data.is_empty() {
                    continue;
                }
                if additive {
                    gl.blend_func(gl::ONE, gl::ONE);
                } else {
                    gl.blend_func(gl::SRC_ALPHA, gl::ONE_MINUS_SRC_ALPHA);
                }
                gl.bind_buffer(gl::ARRAY_BUFFER, buf);
                gl.buffer_data(gl::ARRAY_BUFFER, &float_bytes(data), gl::STREAM_DRAW);
                gl.bind_vertex_array(vao);
                gl.draw_arrays_instanced(gl::TRIANGLES, 0, 6, (data.len() / PARTICLE_FLOATS) as i32);
                stats.draws += 1;
            }
        }
        if passing.rain > 0.0 {
            gl.use_program(self.rain.id);
            gl.uniform3f(self.rain.at("u_box"), 34.0, 22.0, 34.0);
            gl.uniform3f(self.rain.at("u_fall"), 1.4, -11.0, 0.9);
            gl.uniform1f(self.rain.at("u_rain"), passing.rain.min(1.0));
            gl.blend_func(gl::SRC_ALPHA, gl::ONE);
            gl.bind_vertex_array(self.rain_vao);
            gl.draw_arrays_instanced(gl::TRIANGLES, 0, 6, self.drops);
            stats.draws += 1;
        }
        gl.depth_mask(true);
        gl.disable(gl::BLEND);
        gl.enable(gl::CULL_FACE);
        gl.bind_vertex_array(0);
    }
}
