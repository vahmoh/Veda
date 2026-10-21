// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The virgl renderer's own logic, on a fake host (`virgl::fake`) that runs
//! anywhere: what it asks the host for, what it keeps, and when it waits.

use super::*;
use crate::virgl::fake;

fn fake_context(w: u32, h: u32) -> (Context, fake::Shared) {
    fake::context(Config { width: w, height: h, ..Config::default() })
}

#[test]
fn client_waits_end_when_their_time_does() {
    let (mut c, host) = fake_context(1, 1);
    host.borrow_mut().auto_signal = false;
    let sync = c.fence_sync(gl::SYNC_GPU_COMMANDS_COMPLETE, 0);
    // The GPU has not finished: a wait of a millisecond times out, and the
    // context is not taken for lost.
    assert_eq!(c.client_wait_sync(sync, 0, 1_000_000), gl::TIMEOUT_EXPIRED);
    assert_eq!(c.get_synciv(sync, gl::SYNC_STATUS), gl::UNSIGNALED as i32);
    let fence = c.fence();
    assert!(fence != 0 && !c.wait_fence(fence, 1_000));
    // Once it has, waits are satisfied.
    let mut h = host.borrow_mut();
    h.signaled = h.fences;
    drop(h);
    assert_eq!(c.client_wait_sync(sync, 0, 1_000_000), gl::CONDITION_SATISFIED);
    assert_eq!(c.client_wait_sync(sync, 0, 1_000_000), gl::ALREADY_SIGNALED);
    assert!(c.wait_fence(fence, 0));
    no_error(&mut c);
}

/// A program drawing a triangle from attribute `pos` (location 0), its
/// buffer bound and enabled.
fn triangle(c: &mut Context) -> u32 {
    let p = program(
        c,
        "#version 300 es\nin vec4 pos; void main() { gl_Position = pos; }",
        "#version 300 es\nprecision mediump float; out vec4 o; void main() { o = vec4(1.0); }",
    );
    c.bind_attrib_location(p, 0, "pos");
    c.link_program(p);
    c.use_program(p);
    buffer_f32(c, gl::ARRAY_BUFFER, &[-1.0, -1.0, 0.0, 1.0, 3.0, -1.0, 0.0, 1.0, -1.0, 3.0, 0.0, 1.0]);
    c.vertex_attrib_pointer(0, 4, gl::FLOAT, false, 0, 0);
    c.enable_vertex_attrib_array(0);
    p
}

#[test]
fn re_specified_buffers_reuse_their_resources() {
    let (mut c, host) = fake_context(4, 4);
    let b = c.gen_buffer();
    c.bind_buffer(gl::ARRAY_BUFFER, b);
    let frame = |c: &mut Context, n: usize| {
        let data: Vec<u8> = (0..n).map(|i| i as u8).collect();
        c.buffer_data(gl::ARRAY_BUFFER, &data, gl::STREAM_DRAW);
        c.flush();
    };
    // Two buffers (one the GPU may still read, one to write) serve sizes
    // that vary a little.
    frame(&mut c, 1100);
    frame(&mut c, 1100);
    let (creates, destroys) = (host.borrow().creates, host.borrow().destroys);
    // Frame after frame, no resource is made or destroyed.
    for k in 0..20 {
        frame(&mut c, 1100 + 7 * k);
    }
    assert_eq!((host.borrow().creates, host.borrow().destroys), (creates, destroys));
    // A buffer made of a reused one reads as zero where it was not written.
    c.buffer_data_size(gl::ARRAY_BUFFER, 1100, gl::STREAM_DRAW);
    let got = c.map_buffer_range(gl::ARRAY_BUFFER, 0, 1100, gl::MAP_READ_BIT).unwrap().to_vec();
    assert!(got.iter().all(|&x| x == 0));
    c.unmap_buffer(gl::ARRAY_BUFFER);
    no_error(&mut c);
}

#[test]
fn buffers_specified_anew_for_each_draw_keep_their_storage() {
    let (mut c, host) = fake_context(4, 4);
    triangle(&mut c);
    // As librw streams vertices: for each draw, the buffer specified anew,
    // empty and large, and the draw's vertices written at its start; with
    // no flush between, no buffer's fence passes for the pool to reuse it.
    const SIZE: usize = 280_000;
    let vertices: Vec<u8> = [-1.0f32, -1.0, 0.0, 1.0, 3.0, -1.0, 0.0, 1.0, -1.0, 3.0, 0.0, 1.0]
        .iter()
        .flat_map(|f| f.to_le_bytes())
        .collect();
    let draw = |c: &mut Context| {
        c.buffer_data_size(gl::ARRAY_BUFFER, SIZE, gl::STREAM_DRAW);
        c.buffer_sub_data(gl::ARRAY_BUFFER, 0, &vertices);
        c.draw_arrays(gl::TRIANGLES, 0, 3);
    };
    // Its storage made, and what the first draws make, once.
    draw(&mut c);
    draw(&mut c);
    let (creates, destroys) = (host.borrow().creates, host.borrow().destroys);
    for _ in 0..100 {
        draw(&mut c);
    }
    c.flush();
    // The buffer kept its storage: no resource was made or destroyed (each
    // a call to the renderer), and it holds the vertices, zeros after them.
    assert_eq!((host.borrow().creates, host.borrow().destroys), (creates, destroys));
    let got = c.map_buffer_range(gl::ARRAY_BUFFER, 0, SIZE, gl::MAP_READ_BIT).unwrap().to_vec();
    assert_eq!(got[..vertices.len()], vertices[..]);
    assert!(got[vertices.len()..].iter().all(|&x| x == 0));
    c.unmap_buffer(gl::ARRAY_BUFFER);
    // Specified with data, it keeps it too, the data whole.
    let data: Vec<u8> = (0..SIZE).map(|i| (i % 251) as u8).collect();
    c.buffer_data(gl::ARRAY_BUFFER, &data, gl::STREAM_DRAW);
    assert_eq!((host.borrow().creates, host.borrow().destroys), (creates, destroys));
    let got = c.map_buffer_range(gl::ARRAY_BUFFER, 0, SIZE, gl::MAP_READ_BIT).unwrap().to_vec();
    assert!(got == data);
    c.unmap_buffer(gl::ARRAY_BUFFER);
    // Far smaller, it takes storage of its size (a buffer the pool kept).
    c.buffer_data(gl::ARRAY_BUFFER, &vertices, gl::STREAM_DRAW);
    let got = c.map_buffer_range(gl::ARRAY_BUFFER, 0, vertices.len(), gl::MAP_READ_BIT).unwrap().to_vec();
    assert_eq!(got, vertices);
    c.unmap_buffer(gl::ARRAY_BUFFER);
    no_error(&mut c);
}

#[test]
fn destroying_resources_waits_for_the_next_submission() {
    let (mut c, host) = fake_context(4, 4);
    triangle(&mut c);
    let textures: Vec<u32> = (0..4)
        .map(|_| {
            let t = c.gen_texture();
            c.bind_texture(gl::TEXTURE_2D, t);
            c.tex_image_2d(gl::TEXTURE_2D, 0, gl::RGBA8, 4, 4, 0, gl::RGBA, gl::UNSIGNED_BYTE, None);
            t
        })
        .collect();
    c.draw_arrays(gl::TRIANGLES, 0, 3);
    c.flush();
    let h = |host: &fake::Shared| {
        let h = host.borrow();
        (h.destroys, h.submits, h.sent(crate::virgl::protocol::CMD_SET_VERTEX_BUFFERS))
    };
    let (destroys, submits, vertex_buffers) = h(&host);
    // Deleted, the textures go with the next submission, together; what is
    // bound stays bound, so the next draw sends nothing of it again.
    c.delete_textures(&textures);
    assert_eq!(h(&host), (destroys, submits, vertex_buffers));
    c.draw_arrays(gl::TRIANGLES, 0, 3);
    c.flush();
    assert_eq!(h(&host), (destroys + 4, submits + 1, vertex_buffers));
    no_error(&mut c);
}

#[test]
fn state_objects_are_kept_to_a_bounded_number() {
    use crate::virgl::protocol::OBJ_RASTERIZER;
    let (mut c, host) = fake_context(4, 4);
    triangle(&mut c);
    // A line width or polygon offset animated over many values makes as
    // many rasterizer states.
    for k in 0..500 {
        c.line_width(1.0 + k as f32 / 100.0);
        c.draw_arrays(gl::LINES, 0, 2);
        c.enable(gl::POLYGON_OFFSET_FILL);
        c.polygon_offset(k as f32, 1.0);
        c.draw_arrays(gl::TRIANGLES, 0, 3);
    }
    c.flush();
    let live = host.borrow().count(OBJ_RASTERIZER);
    assert!(live > 0 && live <= 64 + 2, "{live} rasterizer states");
    assert_eq!(host.borrow().draws, 1000);
    no_error(&mut c);
}

#[test]
fn feedback_the_host_cannot_record_is_shaded_in_guest_memory() {
    // The fake host draws nothing, so the first capture's check fails, as
    // on hosts that record only zeros: vertices are shaded in guest memory.
    let (mut c, _host) = fake_context(1, 1);
    let v = c.create_shader(gl::VERTEX_SHADER);
    c.shader_source(
        v,
        &["#version 300 es
        in float x; out float doubled; flat out int id;
        void main() { gl_Position = vec4(0); doubled = 2.0 * x; id = gl_VertexID + gl_InstanceID * 100; }"],
    );
    c.compile_shader(v);
    let f = c.create_shader(gl::FRAGMENT_SHADER);
    c.shader_source(f, &["#version 300 es\nprecision mediump float; out vec4 o; void main() { o = vec4(1); }"]);
    c.compile_shader(f);
    let p = c.create_program();
    c.attach_shader(p, v);
    c.attach_shader(p, f);
    c.transform_feedback_varyings(p, &["doubled", "id"], gl::INTERLEAVED_ATTRIBS);
    c.link_program(p);
    c.use_program(p);
    let x = c.get_attrib_location(p, "x") as u32;
    buffer_f32(&mut c, gl::ARRAY_BUFFER, &[1.0, 2.0, 3.0, 4.0]);
    c.vertex_attrib_pointer(x, 1, gl::FLOAT, false, 0, 0);
    c.enable_vertex_attrib_array(x);
    let out = c.gen_buffer();
    c.bind_buffer_base(gl::TRANSFORM_FEEDBACK_BUFFER, 0, out);
    c.buffer_data_size(gl::TRANSFORM_FEEDBACK_BUFFER, 8 * 6, gl::STREAM_READ);
    c.begin_transform_feedback(gl::POINTS);
    c.draw_arrays_instanced(gl::POINTS, 1, 3, 2);
    c.end_transform_feedback();
    no_error(&mut c);
    let bytes = c.map_buffer_range(gl::TRANSFORM_FEEDBACK_BUFFER, 0, 8 * 6, gl::MAP_READ_BIT).unwrap().to_vec();
    let w = |i: usize| u32::from_le_bytes([bytes[4 * i], bytes[4 * i + 1], bytes[4 * i + 2], bytes[4 * i + 3]]);
    // Each vertex: doubled, id; three vertices of each of two instances.
    let got: Vec<(f32, u32)> = (0..6).map(|v| (f32::from_bits(w(2 * v)), w(2 * v + 1))).collect();
    assert_eq!(got, [(4.0, 1), (6.0, 2), (8.0, 3), (4.0, 101), (6.0, 102), (8.0, 103)]);
}

#[test]
fn multisampling_is_offered_in_the_formats_the_host_multisamples() {
    use crate::virgl::VirglBackend;
    use crate::virgl::protocol::format;
    // The fake host as it is, or changed: a format's bit of
    // `supported_multisample_formats` (at 768) cleared, the capability set
    // ending before the mask, or fewer samples than `gl` gives.
    let context = |change: &dyn Fn(&mut Vec<u8>)| {
        let (mut t, _) = fake::Fake::new();
        change(&mut t.caps);
        let b = VirglBackend::new(std::boxed::Box::new(t)).expect("the fake host suits the renderer");
        Context::new(std::boxed::Box::new(b), Config::default())
    };
    let without = |f: u32| move |caps: &mut Vec<u8>| caps[768 + f as usize / 8] &= !(1 << (f % 8));
    // How many sample counts renderbuffers of a format may have.
    let counts = |c: &mut Context, internalformat: u32| {
        let mut n = [0];
        c.get_internalformativ(gl::RENDERBUFFER, internalformat, gl::NUM_SAMPLE_COUNTS, &mut n);
        n[0]
    };
    let mut c = context(&|_| {});
    assert_eq!(c.get_integer(gl::MAX_SAMPLES), 4);
    for f in [gl::RGBA8, gl::RGB565, gl::DEPTH24_STENCIL8, gl::DEPTH32F_STENCIL8, gl::STENCIL_INDEX8, gl::RGBA32F] {
        assert_eq!(counts(&mut c, f), 1, "{f:#x}");
    }
    // OpenGL ES 3.0 asks GL_MAX_SAMPLES of 24-bit depth with stencil (which
    // stencil alone is kept as) and of 32-bit depth with stencil: a host
    // that cannot multisample one of them multisamples nothing here.
    for f in [format::S8_UINT_Z24_UNORM, format::Z32_FLOAT_S8X24_UINT] {
        let mut c = context(&without(f));
        assert_eq!(c.get_integer(gl::MAX_SAMPLES), 0, "{f}");
        assert_eq!(counts(&mut c, gl::RGBA8), 0, "{f}");
        let rb = c.gen_renderbuffer();
        c.bind_renderbuffer(gl::RENDERBUFFER, rb);
        c.renderbuffer_storage_multisample(gl::RENDERBUFFER, 4, gl::RGBA8, 4, 4);
        assert_eq!(c.get_error(), gl::INVALID_OPERATION, "{f}");
    }
    // Nor does one that does not say, or has fewer samples.
    let mut c = context(&|caps| caps.truncate(764));
    assert_eq!(c.get_integer(gl::MAX_SAMPLES), 0);
    let mut c = context(&|caps| caps[284..288].copy_from_slice(&2u32.to_le_bytes()));
    assert_eq!(c.get_integer(gl::MAX_SAMPLES), 0);
    // A format it does not ask of goes without alone.
    let mut c = context(&without(format::R32G32B32A32_FLOAT));
    assert_eq!(c.get_integer(gl::MAX_SAMPLES), 4);
    assert_eq!((counts(&mut c, gl::RGBA32F), counts(&mut c, gl::RGBA8)), (0, 1));
    no_error(&mut c);
}

#[test]
fn hosts_short_of_what_applications_are_offered_are_refused() {
    use crate::virgl::{Unsupported, VirglBackend};
    // Offsets in `struct virgl_caps_v2`, and values below the front end's.
    for (offset, value) in [(280, 2u32), (296, 8), (356, 8), (384, 512), (384, 0)] {
        let (mut t, _) = fake::Fake::new();
        t.caps[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        assert_eq!(VirglBackend::new(std::boxed::Box::new(t)).err(), Some(Unsupported::Limits), "{offset}: {value}");
    }
    let (t, _) = fake::Fake::new();
    assert!(VirglBackend::new(std::boxed::Box::new(t)).is_ok());
}
