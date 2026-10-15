// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What is drawn instanced: [`Instances`], a buffer of instances on the
//! GPU that a vertex array reads; and from data refilled every frame, the
//! cars (each kind's bodies and wheels) and their lights' glows.
//!
//! A buffer refilled every frame is rewritten in place, and grown only
//! when it is too small (to twice what it must then hold): making a buffer
//! again costs a renderer far more than copying into one.

use alloc::vec::Vec;

use vgl::{Context, gl};

use super::gpu::{self, Geometry};
use super::{CarView, Livery, Stats};
use crate::carmodel::{self, Kind};
use crate::prelude::*;

/// A buffer of instances, `floats` floats each, that a vertex array reads
/// (its per-instance attributes from [`gpu::attr::INST0`]).
pub struct Instances {
    /// The vertex array that reads them.
    pub vao: u32,
    buffer: u32,
    floats: usize,
    /// How much the buffer holds (bytes), and how many instances it has.
    capacity: usize,
    count: i32,
    usage: u32,
    /// The instances' bytes, each fill's working.
    bytes: Vec<u8>,
}

impl Instances {
    /// Instances for vertex array `vao`, refilled every frame (none until
    /// then).
    pub fn new(gl: &mut Context, vao: u32, floats: usize) -> Instances {
        Instances::with(gl, vao, floats, gl::DYNAMIC_DRAW)
    }

    /// Instances for vertex array `vao`, filled once with `data`.
    pub fn fixed(gl: &mut Context, vao: u32, floats: usize, data: &[f32]) -> Instances {
        let mut i = Instances::with(gl, vao, floats, gl::STATIC_DRAW);
        i.fill(gl, data);
        i.bytes = Vec::new();
        i
    }

    fn with(gl: &mut Context, vao: u32, floats: usize, usage: u32) -> Instances {
        gl.bind_vertex_array(vao);
        let buffer = gl.gen_buffer();
        gl.bind_buffer(gl::ARRAY_BUFFER, buffer);
        gpu::set_instance_layout(gl, floats, 0);
        gl.bind_vertex_array(0);
        gl.bind_buffer(gl::ARRAY_BUFFER, 0);
        Instances { vao, buffer, floats, capacity: 0, count: 0, usage, bytes: Vec::new() }
    }

    /// How many instances it has.
    pub fn count(&self) -> i32 {
        self.count
    }

    /// Fills it with `data` (whole instances).
    pub fn fill(&mut self, gl: &mut Context, data: &[f32]) {
        self.count = (data.len() / self.floats) as i32;
        if data.is_empty() {
            return;
        }
        gpu::put_floats(&mut self.bytes, data);
        gl.bind_buffer(gl::ARRAY_BUFFER, self.buffer);
        if self.bytes.len() > self.capacity {
            // The first fill exactly; growing, with room for more.
            self.capacity = if self.capacity == 0 { self.bytes.len() } else { self.bytes.len() * 2 };
            gl.buffer_data_size(gl::ARRAY_BUFFER, self.capacity, self.usage);
        }
        gl.buffer_sub_data(gl::ARRAY_BUFFER, 0, &self.bytes);
        gl.bind_buffer(gl::ARRAY_BUFFER, 0);
    }

    /// Draws `geometry` (whose vertex array reads these) once for each.
    pub fn draw(&self, gl: &mut Context, geometry: &Geometry) {
        geometry.draw_instanced(gl, self.count);
    }

    /// Draws the quad of a vertex array from [`quad`] once for each.
    pub fn draw_quads(&self, gl: &mut Context) {
        if self.count > 0 {
            gl.bind_vertex_array(self.vao);
            gl.draw_arrays_instanced(gl::TRIANGLES, 0, 6, self.count);
        }
    }

    /// Lets go of its buffer (and its vertex array).
    pub fn delete(self, gl: &mut Context) {
        gl.delete_vertex_arrays(&[self.vao]);
        gl.delete_buffers(&[self.buffer]);
    }
}

/// A vertex array of a quad (two triangles, corners at -1 and 1), for
/// [`Instances`] to draw once for each, and the buffer of its corners.
pub fn quad(gl: &mut Context) -> (u32, u32) {
    let vao = gl.gen_vertex_array();
    gl.bind_vertex_array(vao);
    let corners = gl.gen_buffer();
    gl.bind_buffer(gl::ARRAY_BUFFER, corners);
    let quad = [-1.0, -1.0, 1.0, -1.0, 1.0, 1.0, -1.0, -1.0, 1.0, 1.0, -1.0, 1.0];
    gl.buffer_data(gl::ARRAY_BUFFER, &gpu::float_bytes(&quad), gl::STATIC_DRAW);
    gl.vertex_attrib_pointer(gpu::attr::POS, 2, gl::FLOAT, false, 0, 0);
    gl.enable_vertex_attrib_array(gpu::attr::POS);
    gl.bind_vertex_array(0);
    gl.bind_buffer(gl::ARRAY_BUFFER, 0);
    (vao, corners)
}

/// Floats per car instance: three rows of the transform, paint and
/// livery, lamps.
const CAR_FLOATS: usize = 20;

/// Beyond the fog and the shadows' reach, a car is not worth drawing (m).
const CARS_SEEN: f32 = 900.0;

/// A kind of car on the GPU: its body and its wheel, each with this
/// frame's instances.
struct CarKind {
    kind: Kind,
    body: Geometry,
    wheel: Geometry,
    /// Where its wheels are (car space).
    wheels: [Vec3; 4],
    bodies: Instances,
    wheels_drawn: Instances,
    /// This frame's instances, as floats.
    body_data: Vec<f32>,
    wheel_data: Vec<f32>,
}

/// The cars, every kind's meshes, and this frame's instances of them.
pub struct Cars {
    kinds: Vec<CarKind>,
}

impl Cars {
    /// Every kind's meshes, on the GPU.
    pub fn new(gl: &mut Context) -> Cars {
        let kinds = Kind::ALL
            .iter()
            .map(|&kind| {
                let m = carmodel::build(kind);
                let body = Geometry::new(gl, &m.body);
                let wheel = Geometry::new(gl, &m.wheel);
                let bodies = Instances::new(gl, body.vao, CAR_FLOATS);
                let wheels_drawn = Instances::new(gl, wheel.vao, CAR_FLOATS);
                CarKind {
                    kind,
                    body,
                    wheel,
                    wheels: m.wheels,
                    bodies,
                    wheels_drawn,
                    body_data: Vec::new(),
                    wheel_data: Vec::new(),
                }
            })
            .collect();
        Cars { kinds }
    }

    /// Fills each kind's instances with this frame's cars near `eye`:
    /// bodies, and wheels (the right-hand ones turned round so that their
    /// faces look out; the front ones steered).
    pub fn fill(&mut self, gl: &mut Context, cars: &[CarView], eye: Vec3) {
        for k in &mut self.kinds {
            k.body_data.clear();
            k.wheel_data.clear();
        }
        let push = |out: &mut Vec<f32>, m: &Mat4, paint: [f32; 4], lamps: [f32; 4]| {
            for r in 0..3 {
                let row = m.row(r);
                out.extend_from_slice(&[row.x, row.y, row.z, row.w]);
            }
            out.extend_from_slice(&paint);
            out.extend_from_slice(&lamps);
        };
        for c in cars {
            if c.transform.w_axis.xyz().distance(eye) > CARS_SEEN {
                continue;
            }
            let Some(k) = self.kinds.iter_mut().find(|e| e.kind == c.kind) else { continue };
            let livery = match c.livery {
                Livery::Plain => 0.0,
                Livery::Police => 1.0,
                Livery::Taxi => 2.0,
            };
            let paint = [c.paint.x, c.paint.y, c.paint.z, livery];
            let lamps = [c.headlights, c.brake, c.bar[0], c.bar[1]];
            push(&mut k.body_data, &c.transform, paint, lamps);
            for (w, &at) in k.wheels.iter().enumerate() {
                let steer = if w < 2 { c.steer } else { 0.0 };
                let mut m = c.transform * Mat4::from_translation(at) * Mat4::from_rotation_y(steer);
                if at.x < 0.0 {
                    m *= Mat4::from_rotation_y(core::f32::consts::PI) * Mat4::from_rotation_x(-c.spin);
                } else {
                    m *= Mat4::from_rotation_x(c.spin);
                }
                push(&mut k.wheel_data, &m, [0.0; 4], [0.0; 4]);
            }
        }
        for k in &mut self.kinds {
            k.bodies.fill(gl, &k.body_data);
            k.wheels_drawn.fill(gl, &k.wheel_data);
        }
    }

    /// Draws this frame's cars with the program bound (the cars' own, or
    /// the shadow map's), each kind's bodies, then its wheels; `shape` is
    /// where the program takes a body's shape (-1 if it does not).
    pub fn draw(&self, gl: &mut Context, shape: i32, stats: &mut Stats) {
        for k in &self.kinds {
            if k.bodies.count() == 0 {
                continue;
            }
            let s = k.kind.shape();
            gl.uniform4f(shape, s.cowl, s.a, s.c, s.belt);
            k.bodies.draw(gl, &k.body);
            k.wheels_drawn.draw(gl, &k.wheel);
            stats.count_instanced(k.body.count, k.bodies.count());
            stats.count_instanced(k.wheel.count, k.wheels_drawn.count());
        }
    }
}

/// The glows of the cars' lights, refilled every frame: headlights seen
/// from ahead, tail lights from behind (brighter when braking), the light
/// bars' flashes from anywhere.
pub struct Glows {
    quads: Instances,
    data: Vec<f32>,
}

impl Glows {
    pub fn new(gl: &mut Context) -> Glows {
        let (vao, _) = quad(gl);
        Glows { quads: Instances::new(gl, vao, 8), data: Vec::new() }
    }

    /// Fills them with this frame's `cars`' lights seen from `eye`, as
    /// strong as the dark makes them (`dark`); true if there are any.
    pub fn fill(&mut self, gl: &mut Context, cars: &[CarView], eye: Vec3, dark: f32) -> bool {
        self.data.clear();
        for c in cars {
            car_glows(&mut self.data, c, eye, dark);
        }
        self.quads.fill(gl, &self.data);
        self.quads.count() > 0
    }

    /// Draws them with the glow program bound.
    pub fn draw(&self, gl: &mut Context, stats: &mut Stats) {
        if self.quads.count() > 0 {
            self.quads.draw_quads(gl);
            stats.count_instanced(6, self.quads.count());
        }
    }
}

/// Car `c`'s lights' glows seen from `eye`, in the dark `dark`, into `out`:
/// eight floats each, place and size, colour.
fn car_glows(out: &mut Vec<f32>, c: &CarView, eye: Vec3, dark: f32) {
    let s = c.kind.shape();
    let (half, w) = (s.length * 0.5, s.width * 0.5);
    let m = &c.transform;
    let forward = m.transform_vector3(Vec3::Z).normalize_or(Vec3::Z);
    let mut glow = |local: Vec3, facing: Option<Vec3>, size: f32, color: Vec3| {
        let p = m.transform_point3(local);
        let k = match facing {
            Some(d) => {
                let toward = (eye - p).normalize_or(Vec3::Y);
                let f = d.dot(toward).max(0.0);
                0.12 + 0.88 * f * f
            }
            None => 1.0,
        };
        let c = color * (k * dark);
        if c.x + c.y + c.z > 0.01 {
            out.extend_from_slice(&[p.x, p.y, p.z, size, c.x, c.y, c.z, 0.0]);
        }
    };
    let hl_y = (s.ends.1 - 0.06).min(s.top_at(half - 0.1) - 0.05) - 0.03;
    let t_y = (s.ends.1 - 0.02).min(s.top_at(-half + 0.1) - 0.04) - 0.06;
    for side in [-1.0f32, 1.0] {
        if c.headlights > 0.0 {
            let at = Vec3::new(side * (w * 0.80 - 0.2), hl_y, half + 0.02);
            glow(at, Some(forward), 0.32, Vec3::new(1.0, 0.95, 0.85) * (3.0 * c.headlights));
        }
        let tail = 0.25 * c.headlights + 1.5 * c.brake;
        if tail > 0.0 {
            let at = Vec3::new(side * (w * 0.82 - 0.21), t_y, -half - 0.02);
            glow(at, Some(-forward), 0.22, Vec3::new(1.0, 0.03, 0.02) * tail);
        }
    }
    if c.bar[0] + c.bar[1] > 0.0 {
        let roof = s.top_at(s.a - 0.25) + 0.12;
        let z = s.a - 0.28;
        glow(Vec3::new(0.32, roof, z), None, 0.8, Vec3::new(1.0, 0.05, 0.02) * (6.0 * c.bar[0]));
        glow(Vec3::new(-0.32, roof, z), None, 0.8, Vec3::new(0.1, 0.25, 1.0) * (7.0 * c.bar[1]));
    }
}
