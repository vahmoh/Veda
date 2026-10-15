// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The sky: painted for the time of day into a cube map (in half floats
//! where the renderer draws into them, so that the sun's reflection keeps
//! its brightness), which the background and every reflection sample.

use alloc::string::String;

use vgl::{Context, gl};

use super::gpu::{self, Program, mat};
use super::shaders::{self, source};
use super::{Stats, lit_program};
use crate::env::Environment;
use crate::prelude::*;

/// The sky's cube map and the program that draws the background from it.
pub struct Sky {
    program: Program,
    /// The cube map (painted by [`Sky::paint`]).
    pub map: u32,
    size: i32,
    /// A full-screen triangle's vertex array.
    fullscreen: u32,
}

impl Sky {
    /// The sky of `env`, `size` texels a face, drawn with the full-screen
    /// triangle of `fullscreen`.
    pub fn new(
        gl: &mut Context,
        defines: &'static str,
        env: &Environment,
        size: i32,
        fullscreen: u32,
    ) -> Result<Sky, String> {
        let program =
            lit_program(gl, defines, "sky", shaders::SKY_VS, shaders::SKY_FS, &["u_invViewProj", "u_stars"], None)?;
        let mut sky = Sky { program, map: 0, size, fullscreen };
        sky.paint(gl, defines, env)?;
        Ok(sky)
    }

    /// Paints the sky of `env` into the cube map.
    pub fn paint(&mut self, gl: &mut Context, defines: &str, env: &Environment) -> Result<(), String> {
        let p = Program::new(
            gl,
            "sky bake",
            &source(defines, false, shaders::FULLSCREEN_VS),
            &source(defines, false, shaders::SKY_BAKE_FS),
            &[
                "u_forward",
                "u_right",
                "u_up",
                "u_sun",
                "u_zenith",
                "u_horizon",
                "u_glow",
                "u_disc",
                "u_clouds",
                "u_stars",
                "u_city",
            ],
            &[],
        )?;
        let size = self.size;
        // Storage once allocated is fixed: a new sky, a new texture.
        gl.delete_textures(&[self.map]);
        self.map = gl.gen_texture();
        gl.bind_texture(gl::TEXTURE_CUBE_MAP, self.map);
        // In half floats where the renderer draws into them, so that the
        // sun's reflection keeps its brightness.
        let format = if gl.caps().color_buffer_float { gl::RGBA16F } else { gl::RGBA8 };
        gl.tex_storage_2d(gl::TEXTURE_CUBE_MAP, gpu::mip_levels(size, size), format, size, size);
        gl.tex_parameteri(gl::TEXTURE_CUBE_MAP, gl::TEXTURE_MIN_FILTER, gl::LINEAR_MIPMAP_LINEAR as i32);
        gl.tex_parameteri(gl::TEXTURE_CUBE_MAP, gl::TEXTURE_MAG_FILTER, gl::LINEAR as i32);
        let faces: [([f32; 3], [f32; 3], [f32; 3]); 6] = [
            ([1.0, 0.0, 0.0], [0.0, 0.0, -1.0], [0.0, -1.0, 0.0]),
            ([-1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, -1.0, 0.0]),
            ([0.0, 1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
            ([0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, -1.0]),
            ([0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, -1.0, 0.0]),
            ([0.0, 0.0, -1.0], [-1.0, 0.0, 0.0], [0.0, -1.0, 0.0]),
        ];
        let fbo = gl.gen_framebuffer();
        gl.bind_framebuffer(gl::FRAMEBUFFER, fbo);
        gl.use_program(p.id);
        let v3 = |gl: &mut Context, name: &str, v: Vec3| gl.uniform3f(p.at(name), v.x, v.y, v.z);
        v3(gl, "u_sun", env.sun_dir);
        v3(gl, "u_zenith", env.sky.zenith);
        v3(gl, "u_horizon", env.sky.horizon);
        v3(gl, "u_glow", env.sky.glow);
        v3(gl, "u_disc", env.sky.disc);
        v3(gl, "u_city", env.sky.city);
        let c = env.sky.clouds;
        gl.uniform4f(p.at("u_clouds"), c.x, c.y, c.z, env.sky.cloud_cover);
        gl.uniform1f(p.at("u_stars"), env.sky.stars);
        gl.bind_vertex_array(self.fullscreen);
        gl.viewport(0, 0, size, size);
        gl.disable(gl::DEPTH_TEST);
        gl.disable(gl::CULL_FACE);
        for (i, (f, r, u)) in faces.iter().enumerate() {
            gl.framebuffer_texture_2d(
                gl::FRAMEBUFFER,
                gl::COLOR_ATTACHMENT0,
                gl::TEXTURE_CUBE_MAP_POSITIVE_X + i as u32,
                self.map,
                0,
            );
            gl.uniform3f(p.at("u_forward"), f[0], f[1], f[2]);
            gl.uniform3f(p.at("u_right"), r[0], r[1], r[2]);
            gl.uniform3f(p.at("u_up"), u[0], u[1], u[2]);
            gl.draw_arrays(gl::TRIANGLES, 0, 3);
        }
        gl.bind_vertex_array(0);
        gl.bind_framebuffer(gl::FRAMEBUFFER, 0);
        gl.delete_framebuffers(&[fbo]);
        gl.delete_program(p.id);
        gl.bind_texture(gl::TEXTURE_CUBE_MAP, self.map);
        gl.generate_mipmap(gl::TEXTURE_CUBE_MAP);
        Ok(())
    }

    /// Draws the sky where nothing else is (behind everything drawn), seen
    /// through `view_proj`, its stars as bright as `stars`.
    pub fn draw(&self, gl: &mut Context, view_proj: &Mat4, stars: f32, stats: &mut Stats) {
        gl.active_texture(gl::TEXTURE0);
        gl.depth_func(gl::LEQUAL);
        gl.depth_mask(false);
        gl.use_program(self.program.id);
        gl.uniform_matrix4fv(
            self.program.at("u_invViewProj"),
            false,
            &mat(&view_proj.try_inverse().unwrap_or(Mat4::IDENTITY)),
        );
        gl.uniform1f(self.program.at("u_stars"), stars);
        gl.bind_vertex_array(self.fullscreen);
        gl.draw_arrays(gl::TRIANGLES, 0, 3);
        stats.count(3);
    }
}
