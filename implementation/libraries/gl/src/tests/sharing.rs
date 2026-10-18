// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Share groups (contexts sharing objects) and surfaceless contexts.

use super::*;

const VS: &str = "#version 300 es
in vec2 pos; out vec2 uv;
void main() { gl_Position = vec4(pos, 0.0, 1.0); uv = pos * 0.5 + 0.5; }";
const TEXTURED: &str = "#version 300 es
precision highp float; uniform sampler2D t; in vec2 uv; out vec4 color;
void main() { color = texture(t, uv); }";

/// The other context of a share group: one more, in the 4 x 4 window.
fn other(c: &mut Context) -> crate::context::Local {
    c.new_local(Config { width: 4, height: 4, ..Config::default() })
}

/// A 1 x 1 texture of one color.
fn texture(c: &mut Context, rgba: [u8; 4]) -> u32 {
    let t = c.gen_texture();
    c.bind_texture(gl::TEXTURE_2D, t);
    c.tex_image_2d(gl::TEXTURE_2D, 0, gl::RGBA8, 1, 1, 0, gl::RGBA, gl::UNSIGNED_BYTE, &rgba[..]);
    c.tex_parameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::NEAREST as i32);
    t
}

/// Draws a full-window quad with `program`, its texture on unit 0.
fn draw_quad(c: &mut Context, program: u32) {
    c.use_program(program);
    buffer_f32(c, gl::ARRAY_BUFFER, &[-1.0, -1.0, 1.0, -1.0, -1.0, 1.0, -1.0, 1.0, 1.0, -1.0, 1.0, 1.0]);
    let loc = c.get_attrib_location(program, "pos") as u32;
    c.vertex_attrib_pointer(loc, 2, gl::FLOAT, false, 0, 0);
    c.enable_vertex_attrib_array(loc);
    c.draw_arrays(gl::TRIANGLES, 0, 6);
}

#[test]
fn contexts_share_objects_but_not_containers() {
    let mut c = context(4, 4);
    let mut b = other(&mut c);
    let t = texture(&mut c, [10, 20, 30, 255]);
    let p = program(&mut c, VS, TEXTURED);
    let fb = c.gen_framebuffer();
    c.bind_framebuffer(gl::FRAMEBUFFER, fb);
    // The other context: the texture and program are its too, the
    // framebuffer and the bindings are not.
    c.swap_local(&mut b);
    assert!(c.is_texture(t) && c.is_program(p));
    assert!(!c.is_framebuffer(fb));
    assert_eq!(c.get_integer(gl::TEXTURE_BINDING_2D), 0);
    assert_eq!(c.get_integer(gl::FRAMEBUFFER_BINDING), 0);
    c.bind_texture(gl::TEXTURE_2D, t);
    draw_quad(&mut c, p);
    no_error(&mut c);
    assert_eq!(px(&read_rgba(&mut c, 4, 4), 4, 1, 1), [10, 20, 30, 255]);
    // Back in the first: its own state as it left it.
    c.swap_local(&mut b);
    assert_eq!(c.get_integer(gl::FRAMEBUFFER_BINDING), fb as i32);
    assert_eq!(c.get_integer(gl::TEXTURE_BINDING_2D), t as i32);
    assert_eq!(c.get_integer(gl::CURRENT_PROGRAM), 0);
}

#[test]
fn deleting_unbinds_in_the_other_contexts() {
    let mut c = context(4, 4);
    let mut b = other(&mut c);
    let t = texture(&mut c, [1, 2, 3, 4]);
    let buffer = buffer_f32(&mut c, gl::ARRAY_BUFFER, &[0.0]);
    c.swap_local(&mut b);
    c.delete_textures(&[t]);
    c.delete_buffers(&[buffer]);
    assert!(!c.is_texture(t));
    c.swap_local(&mut b);
    assert_eq!(c.get_integer(gl::TEXTURE_BINDING_2D), 0);
    assert_eq!(c.get_integer(gl::ARRAY_BUFFER_BINDING), 0);
    // What is bound now is the default texture, which works.
    c.tex_image_2d(gl::TEXTURE_2D, 0, gl::RGBA8, 1, 1, 0, gl::RGBA, gl::UNSIGNED_BYTE, &[9u8, 9, 9, 9][..]);
    no_error(&mut c);
}

#[test]
fn a_program_deleted_elsewhere_stays_in_use() {
    let mut c = context(4, 4);
    let mut b = other(&mut c);
    let t = texture(&mut c, [200, 100, 50, 255]);
    let p = program(&mut c, VS, TEXTURED);
    c.use_program(p);
    c.swap_local(&mut b);
    c.delete_program(p);
    assert!(!c.is_program(p));
    c.swap_local(&mut b);
    c.bind_texture(gl::TEXTURE_2D, t);
    buffer_f32(&mut c, gl::ARRAY_BUFFER, &[-1.0, -1.0, 3.0, -1.0, -1.0, 3.0]);
    c.vertex_attrib_pointer(0, 2, gl::FLOAT, false, 0, 0);
    c.enable_vertex_attrib_array(0);
    c.draw_arrays(gl::TRIANGLES, 0, 3);
    no_error(&mut c);
    assert_eq!(px(&read_rgba(&mut c, 4, 4), 4, 2, 2), [200, 100, 50, 255]);
}

#[test]
fn a_context_that_goes_lets_go_of_what_it_kept() {
    let mut c = context(4, 4);
    let mut b = other(&mut c);
    let t = texture(&mut c, [5, 6, 7, 8]);
    c.swap_local(&mut b);
    // The other context's framebuffer keeps the texture after its name
    // is deleted, and so do its vertex array and query objects theirs.
    let fb = c.gen_framebuffer();
    c.bind_framebuffer(gl::FRAMEBUFFER, fb);
    c.framebuffer_texture_2d(gl::FRAMEBUFFER, gl::COLOR_ATTACHMENT0, gl::TEXTURE_2D, t, 0);
    let vao = c.gen_vertex_array();
    c.bind_vertex_array(vao);
    let buffer = buffer_f32(&mut c, gl::ARRAY_BUFFER, &[0.0, 1.0]);
    c.vertex_attrib_pointer(0, 1, gl::FLOAT, false, 0, 0);
    let q = c.gen_query();
    c.begin_query(gl::ANY_SAMPLES_PASSED, q);
    no_error(&mut c);
    c.swap_local(&mut b);
    c.delete_textures(&[t]);
    let textures = c.textures.iter().count();
    c.destroy_local(b);
    // Its framebuffer's texture and its four default ones are gone.
    assert_eq!(c.textures.iter().count(), textures - 5);
    // The buffer it made is the group's, and lives on, kept by its name
    // alone: deleting that ends it.
    assert!(c.is_buffer(buffer));
    c.delete_buffers(&[buffer]);
    assert_eq!(c.buffers.iter().count(), 0);
    no_error(&mut c);
}

#[test]
fn surfaceless_contexts_draw_into_framebuffer_objects_only() {
    let workers: Box<dyn Workers> = Box::new(StdWorkers(2));
    let config = Config { width: 0, height: 0, surfaceless: true, ..Config::default() };
    let mut c = Context::new(Box::new(SoftBackend::new(workers)), config);
    assert_eq!(c.check_framebuffer_status(gl::FRAMEBUFFER), gl::FRAMEBUFFER_UNDEFINED);
    assert_eq!(c.get_integer(gl::RED_BITS), 0);
    c.clear(gl::COLOR_BUFFER_BIT);
    assert_eq!(c.get_error(), gl::INVALID_FRAMEBUFFER_OPERATION);
    let mut out = [0u8; 4];
    c.read_pixels(0, 0, 1, 1, gl::RGBA, gl::UNSIGNED_BYTE, &mut out[..]);
    assert_eq!(c.get_error(), gl::INVALID_FRAMEBUFFER_OPERATION);
    // A framebuffer object is drawn into as anywhere.
    let t = c.gen_texture();
    c.bind_texture(gl::TEXTURE_2D, t);
    c.tex_storage_2d(gl::TEXTURE_2D, 1, gl::RGBA8, 2, 2);
    let fb = c.gen_framebuffer();
    c.bind_framebuffer(gl::FRAMEBUFFER, fb);
    c.framebuffer_texture_2d(gl::FRAMEBUFFER, gl::COLOR_ATTACHMENT0, gl::TEXTURE_2D, t, 0);
    assert_eq!(c.check_framebuffer_status(gl::FRAMEBUFFER), gl::FRAMEBUFFER_COMPLETE);
    c.viewport(0, 0, 2, 2);
    c.clear_color(0.0, 1.0, 0.0, 1.0);
    c.clear(gl::COLOR_BUFFER_BIT);
    c.read_pixels(1, 1, 1, 1, gl::RGBA, gl::UNSIGNED_BYTE, &mut out[..]);
    no_error(&mut c);
    assert_eq!(out, [0, 255, 0, 255]);
}

#[test]
fn a_context_takes_a_surface_and_leaves_it() {
    let workers: Box<dyn Workers> = Box::new(StdWorkers(2));
    let config = Config { width: 0, height: 0, surfaceless: true, ..Config::default() };
    let mut c = Context::new(Box::new(SoftBackend::new(workers)), config);
    c.set_surface(Some((3, 2)));
    assert_eq!(c.check_framebuffer_status(gl::FRAMEBUFFER), gl::FRAMEBUFFER_COMPLETE);
    c.viewport(0, 0, 3, 2);
    c.clear_color(0.0, 0.0, 1.0, 1.0);
    c.clear(gl::COLOR_BUFFER_BIT);
    assert_eq!(px(&read_rgba(&mut c, 3, 2), 3, 2, 1), [0, 0, 255, 255]);
    c.set_surface(None);
    assert_eq!(c.check_framebuffer_status(gl::FRAMEBUFFER), gl::FRAMEBUFFER_UNDEFINED);
    no_error(&mut c);
}
