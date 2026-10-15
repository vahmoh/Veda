// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The street lights' pools of light, painted once into a map of the
//! world seen from above, which every lit program reads: their light
//! added up in colour, the highest lamp's height in alpha.

use alloc::string::String;
use alloc::vec::Vec;

use vgl::{Context, gl};

use super::Quality;
use super::gpu::Program;
use super::instances::{self, Instances};
use super::shaders::{self, source};
use crate::lamps::{self, Lamp};
use crate::terrain::{HALF_X, HALF_Z};

/// Paints `lamps`' pools into a new map (larger with a GPU).
pub fn bake(gl: &mut Context, lamps: &[Lamp], quality: Quality, defines: &str) -> Result<u32, String> {
    let size = if quality == Quality::High { 2048 } else { 1024 };
    let tex = gl.gen_texture();
    gl.bind_texture(gl::TEXTURE_2D, tex);
    let float = gl.caps().color_buffer_float;
    gl.tex_storage_2d(gl::TEXTURE_2D, 1, if float { gl::RGBA16F } else { gl::RGBA8 }, size, size);
    for (p, v) in [
        (gl::TEXTURE_MIN_FILTER, gl::LINEAR),
        (gl::TEXTURE_MAG_FILTER, gl::LINEAR),
        (gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE),
        (gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE),
    ] {
        gl.tex_parameteri(gl::TEXTURE_2D, p, v as i32);
    }
    let p = Program::new(
        gl,
        "light map",
        &source(defines, false, shaders::LIGHT_MAP_VS),
        &source(defines, false, shaders::LIGHT_MAP_FS),
        &["u_rect"],
        &[],
    )?;
    let mut data = Vec::new();
    for l in lamps {
        let heads = if l.kind == lamps::Kind::Double { &l.heads[..] } else { &l.heads[..1] };
        for h in heads {
            // An 8-bit map holds a quarter of the light (the programs
            // cannot tell: the pools are dim enough).
            let c = if float { l.color } else { l.color * 0.25 };
            // A tunnel's lamps are lit day and night: they hang 1000 m
            // higher in the map, which lampLight() reads so.
            let hang = if l.kind.always_lit() { h.y + 1000.0 } else { h.y };
            data.extend_from_slice(&[h.x, h.z, l.radius, hang, c.x, c.y, c.z, 0.0]);
        }
    }
    let (vao, corners) = instances::quad(gl);
    let pools = Instances::fixed(gl, vao, 8, &data);
    let fbo = gl.gen_framebuffer();
    gl.bind_framebuffer(gl::FRAMEBUFFER, fbo);
    gl.framebuffer_texture_2d(gl::FRAMEBUFFER, gl::COLOR_ATTACHMENT0, gl::TEXTURE_2D, tex, 0);
    gl.viewport(0, 0, size, size);
    gl.clear_color(0.0, 0.0, 0.0, 0.0);
    gl.clear(gl::COLOR_BUFFER_BIT);
    gl.disable(gl::DEPTH_TEST);
    gl.disable(gl::CULL_FACE);
    gl.enable(gl::BLEND);
    gl.blend_func(gl::ONE, gl::ONE);
    gl.blend_equation_separate(gl::FUNC_ADD, gl::MAX);
    gl.use_program(p.id);
    gl.uniform4f(p.at("u_rect"), -HALF_X, -HALF_Z, 0.5 / HALF_X, 0.5 / HALF_Z);
    pools.draw_quads(gl);
    gl.blend_equation(gl::FUNC_ADD);
    gl.disable(gl::BLEND);
    gl.bind_vertex_array(0);
    gl.bind_framebuffer(gl::FRAMEBUFFER, 0);
    gl.delete_framebuffers(&[fbo]);
    pools.delete(gl);
    gl.delete_buffers(&[corners]);
    gl.delete_program(p.id);
    Ok(tex)
}
