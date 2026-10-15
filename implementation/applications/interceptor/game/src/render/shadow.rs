// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The sun's shadows, with a GPU, by day and at sunset: a shadow map of
//! the square round where the camera looks, seen along the sunlight, its
//! middle moved by whole texels so that the shadows' edges stand still as
//! the camera moves. Where there is none, a map of one texel stands in,
//! so that every lit program reads one alike.

use alloc::string::String;

use vgl::{Context, gl};

use super::gpu::{self, Program};
use super::instances::Cars;
use super::shaders::{self, source};
use super::world::WorldGpu;
use super::{Camera, Frustum, Quality, Stats};
use crate::env::{Environment, TimeOfDay};
use crate::prelude::*;

/// Half the side of the square the shadow map covers (m).
const HALF: f32 = 95.0;

/// The shadow map's texels a side.
const SIZE: i32 = 2048;

/// The sun's shadow map and the programs that draw its casters into it.
pub struct Shadow {
    fbo: u32,
    /// The map.
    pub tex: u32,
    statik: Program,
    building: Program,
    prop: Program,
    car: Program,
}

/// Whether the sun casts shadows in `env` at `quality` (with a GPU, by day
/// and at sunset).
pub fn casts(env: &Environment, quality: Quality) -> bool {
    quality == Quality::High && env.shadows > 0.0 && env.time != TimeOfDay::Night
}

impl Shadow {
    /// The shadow map and the programs that draw into it.
    pub fn new(gl: &mut Context, defines: &str) -> Result<Shadow, String> {
        // The prelude's samplers at their units, as in every lit program
        // (samplers of different kinds may not share one).
        let depth = |gl: &mut Context, name: &str, vs: &str| {
            let (v, f) = (source(defines, true, vs), source(defines, true, shaders::DEPTH_FS));
            Program::new(gl, name, &v, &f, &[], &gpu::LIT_SAMPLERS)
        };
        let statik = depth(gl, "shadow static", shaders::STATIC_VS)?;
        let building = depth(gl, "shadow buildings", shaders::BUILDING_VS)?;
        let prop = depth(gl, "shadow props", shaders::PROP_VS)?;
        let car = depth(gl, "shadow cars", shaders::CAR_VS)?;
        let tex = gl.gen_texture();
        gl.bind_texture(gl::TEXTURE_2D, tex);
        gl.tex_storage_2d(gl::TEXTURE_2D, 1, gl::DEPTH_COMPONENT24, SIZE, SIZE);
        for (pname, v) in [
            (gl::TEXTURE_MIN_FILTER, gl::LINEAR),
            (gl::TEXTURE_MAG_FILTER, gl::LINEAR),
            (gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE),
            (gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE),
            (gl::TEXTURE_COMPARE_MODE, gl::COMPARE_REF_TO_TEXTURE),
            (gl::TEXTURE_COMPARE_FUNC, gl::LEQUAL),
        ] {
            gl.tex_parameteri(gl::TEXTURE_2D, pname, v as i32);
        }
        let fbo = gl.gen_framebuffer();
        gl.bind_framebuffer(gl::FRAMEBUFFER, fbo);
        gl.framebuffer_texture_2d(gl::FRAMEBUFFER, gl::DEPTH_ATTACHMENT, gl::TEXTURE_2D, tex, 0);
        gl.draw_buffers(&[gl::NONE]);
        gl.read_buffer(gl::NONE);
        let complete = gl.check_framebuffer_status(gl::FRAMEBUFFER) == gl::FRAMEBUFFER_COMPLETE;
        gl.bind_framebuffer(gl::FRAMEBUFFER, 0);
        if !complete {
            return Err("the shadow map's framebuffer is incomplete".into());
        }
        Ok(Shadow { fbo, tex, statik, building, prop, car })
    }

    /// A shadow texel's size as the lit programs read it.
    pub fn texel() -> f32 {
        1.0 / SIZE as f32
    }

    /// The sun's view and projection, the sun towards `sun`: a square box
    /// round where `camera` looks, seen along the sunlight, its middle
    /// moved by whole texels.
    pub fn view_proj(camera: &Camera, sun: Vec3) -> Mat4 {
        let fwd = (camera.target - camera.eye).normalize_or(Vec3::Z);
        let flat = Vec3::new(fwd.x, 0.0, fwd.z).normalize_or(Vec3::Z);
        let center = camera.eye + flat * (HALF * 0.6);
        let up = if sun.y.abs() > 0.95 { Vec3::Z } else { Vec3::Y };
        let view = glam::camera::rh::view::look_at_mat4(Vec3::ZERO, -sun, up);
        let c = view.transform_point3(center);
        let texel = 2.0 * HALF / SIZE as f32;
        let (cx, cy) = ((c.x / texel).round() * texel, (c.y / texel).round() * texel);
        // Towards the sun far enough for the towers, beyond for the ground.
        let proj = glam::camera::rh::proj::opengl::orthographic(
            cx - HALF,
            cx + HALF,
            cy - HALF,
            cy + HALF,
            -c.z - 700.0,
            -c.z + 150.0,
        );
        proj * view
    }

    /// Draws what casts shadows into the map, seen from the sun through
    /// `sun`: the road's sides, the buildings, the street lights, the
    /// trees and the bridge, the cars.
    pub fn draw(&self, gl: &mut Context, sun: &Mat4, world: &WorldGpu, cars: &Cars, stats: &mut Stats) {
        let planes = Frustum::new(sun);
        gl.bind_framebuffer(gl::FRAMEBUFFER, self.fbo);
        gl.viewport(0, 0, SIZE, SIZE);
        gl.enable(gl::DEPTH_TEST);
        gl.depth_func(gl::LESS);
        gl.depth_mask(true);
        gl.clear(gl::DEPTH_BUFFER_BIT);
        // Open meshes (pavements, rails) cast from both sides.
        gl.disable(gl::CULL_FACE);
        gl.enable(gl::POLYGON_OFFSET_FILL);
        gl.polygon_offset(1.6, 3.0);
        world.draw_casters(gl, &planes, [self.statik.id, self.building.id, self.prop.id], stats);
        gl.use_program(self.car.id);
        cars.draw(gl, -1, stats);
        gl.disable(gl::POLYGON_OFFSET_FILL);
        gl.enable(gl::CULL_FACE);
    }
}

/// A depth texture of one texel at the far plane, compared as shadow maps
/// are: where there is no shadow map, nothing is in shadow.
pub fn none(gl: &mut Context) -> u32 {
    let t = gl.gen_texture();
    gl.bind_texture(gl::TEXTURE_2D, t);
    gl.tex_storage_2d(gl::TEXTURE_2D, 1, gl::DEPTH_COMPONENT24, 1, 1);
    let one = 0xFFFF_FFFFu32.to_le_bytes();
    gl.tex_sub_image_2d(gl::TEXTURE_2D, 0, 0, 0, 1, 1, gl::DEPTH_COMPONENT, gl::UNSIGNED_INT, &one[..]);
    gl.tex_parameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::NEAREST as i32);
    gl.tex_parameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::NEAREST as i32);
    gl.tex_parameteri(gl::TEXTURE_2D, gl::TEXTURE_COMPARE_MODE, gl::COMPARE_REF_TO_TEXTURE as i32);
    gl.tex_parameteri(gl::TEXTURE_2D, gl::TEXTURE_COMPARE_FUNC, gl::LEQUAL as i32);
    t
}
