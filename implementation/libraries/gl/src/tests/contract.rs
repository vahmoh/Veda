// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What every renderer owes the front end ([`Backend`]'s contract), tested
//! on the software renderer and on the virgl renderer through a fake host
//! whose memory starts out holding stale data, as a GPU driver's recycled
//! memory does; and through Veda's renderer, where the tests run on it
//! (`GL_TEST_BACKEND=gallium`).

use std::boxed::Box;
use std::vec;
use std::vec::Vec;

use super::*;
use crate::backend::{Backend, Region, ResourceDesc, Target};
use crate::format::Format;
use crate::virgl::{VirglBackend, fake};

/// Both renderers: the software one, and virgl's on a fake host (with the
/// host); and virgl's through Veda's renderer where the tests run on it,
/// which makes new resources read as zero itself.
fn backends() -> Vec<(&'static str, Box<dyn Backend>, Option<fake::Shared>)> {
    let soft: Box<dyn Backend> = Box::new(SoftBackend::new(Box::new(StdWorkers(1))));
    let (t, host) = fake::Fake::new();
    let virgl: Box<dyn Backend> = Box::new(VirglBackend::new(Box::new(t)).unwrap());
    #[allow(unused_mut)]
    let mut all = vec![("soft", soft, None), ("virgl", virgl, Some(host))];
    #[cfg(target_os = "linux")]
    if std::env::var("GL_TEST_BACKEND").is_ok_and(|v| v == "gallium") {
        let t = crate::virgl::gallium::GalliumTransport::new().expect("Veda's renderer is not available");
        all.push(("gallium", Box::new(VirglBackend::new(Box::new(t)).unwrap()), None));
    }
    all
}

/// Every byte of every level and layer of a resource, read back.
fn read_all(b: &mut dyn Backend, id: u32, d: &ResourceDesc) -> Vec<u8> {
    let tb = if d.target == Target::Buffer { 1 } else { d.format.bytes() };
    let mut all = Vec::new();
    for l in 0..d.levels {
        let (w, h, layers) = d.level_size(l);
        let (row, image) = (w as usize * tb, w as usize * tb * h as usize);
        let mut out = vec![0x5Au8; image * layers as usize];
        b.read(id, l, Region::new(0, 0, 0, w, h, layers), &mut out, row, image);
        all.extend(out);
    }
    all
}

#[test]
fn new_resources_read_as_zero() {
    let desc = |target, format, width, height, depth, levels| ResourceDesc {
        target,
        format,
        width,
        height,
        depth,
        levels,
        samples: 0,
    };
    let kinds = [
        desc(Target::Buffer, Format::R8Uint, 3000, 1, 1, 1),
        desc(Target::Texture2D, Format::Rgba8Unorm, 13, 7, 1, 4),
        desc(Target::TextureCube, Format::Rgba8Unorm, 8, 8, 6, 4),
        desc(Target::Texture2DArray, Format::R8Unorm, 5, 3, 4, 2),
        desc(Target::Texture3D, Format::Rgba16Float, 4, 4, 4, 3),
        // A format no host renders to, and a depth-stencil one.
        desc(Target::Texture2D, Format::Rgb9E5Float, 6, 6, 1, 2),
        desc(Target::Texture2D, Format::D24UnormS8Uint, 4, 4, 1, 1),
        desc(Target::Renderbuffer, Format::Rgba8Unorm, 9, 9, 1, 1),
        desc(Target::Renderbuffer, Format::D24UnormS8Uint, 9, 9, 1, 1),
    ];
    for (name, mut b, _) in backends() {
        for d in &kinds {
            let id = b.create_resource(d).unwrap();
            let bytes = read_all(&mut *b, id, d);
            assert!(bytes.iter().all(|&x| x == 0), "{name}: {d:?} reads {:?}", &bytes[..bytes.len().min(16)]);
            b.destroy_resource(id);
        }
    }
}

/// What a resource held does not show in the next one made, though the
/// host may give it the same memory (drivers recycle what is freed, within
/// the process that serves every client): a new resource reads as zero.
#[test]
fn what_resources_held_does_not_show_in_new_ones() {
    let desc = |target, format, width, height, levels| ResourceDesc {
        target,
        format,
        width,
        height,
        depth: 1,
        levels,
        samples: 0,
    };
    let kinds = [
        desc(Target::Texture2D, Format::Rgba8Unorm, 64, 64, 2),
        // One no host draws into, and a depth-stencil renderbuffer.
        desc(Target::Texture2D, Format::Rgb9E5Float, 16, 16, 1),
        desc(Target::Renderbuffer, Format::D24UnormS8Uint, 32, 32, 1),
        desc(Target::Buffer, Format::R8Uint, 4096, 1, 1),
    ];
    for (name, mut b, _) in backends() {
        for d in &kinds {
            for _ in 0..3 {
                let id = b.create_resource(d).unwrap();
                let tb = if d.target == Target::Buffer { 1 } else { d.format.bytes() };
                for l in 0..d.levels {
                    let (w, h, layers) = d.level_size(l);
                    let (row, image) = (w as usize * tb, w as usize * tb * h as usize);
                    let pattern = vec![0xA5u8; image * layers as usize];
                    b.write(id, l, Region::new(0, 0, 0, w, h, layers), &pattern, row, image);
                }
                assert!(read_all(&mut *b, id, d).iter().any(|&x| x != 0), "{name}: {d:?} was not written");
                b.destroy_resource(id);
                // The host has let it go before the next is made.
                b.finish();
                let id = b.create_resource(d).unwrap();
                let bytes = read_all(&mut *b, id, d);
                assert!(bytes.iter().all(|&x| x == 0), "{name}: {d:?} reads {:?}", &bytes[..bytes.len().min(16)]);
                b.destroy_resource(id);
                b.finish();
            }
        }
    }
}

/// A buffer specified anew in the storage it has (`respecify_buffer`)
/// reads as a new one would, zeros, never what it held, which a copy
/// queued before it still reads; and holds what is written next, where the
/// caller writes it whole.
#[test]
fn buffers_specified_anew_read_as_new_ones() {
    let desc = |width| ResourceDesc {
        target: Target::Buffer,
        format: Format::R8Uint,
        width,
        height: 1,
        depth: 1,
        levels: 1,
        samples: 0,
    };
    let (held, written) = (vec![0xA5u8; 4096], vec![0x3Cu8; 4096]);
    for (name, mut b, _) in backends() {
        let buffer = b.create_resource(&desc(4096)).unwrap();
        let copy = b.create_resource(&desc(4096)).unwrap();
        b.write(buffer, 0, Region::bytes(0, 4096), &held, 4096, 4096);
        b.copy_buffer(buffer, 0, copy, 0, 4096);
        // The software renderer has the front end make a new resource.
        let kept = b.respecify_buffer(buffer, 3000, false);
        assert_eq!(kept, name != "soft", "{name}: the buffer kept its storage");
        if kept {
            let bytes = read_all(&mut *b, buffer, &desc(3000));
            assert!(bytes.iter().all(|&x| x == 0), "{name}: specified anew, reads {:?}", &bytes[..16]);
            assert!(read_all(&mut *b, copy, &desc(4096)) == held, "{name}: the copy did not read what it held");
            assert!(b.respecify_buffer(buffer, 4096, true), "{name}: the buffer did not keep its storage");
            b.write(buffer, 0, Region::bytes(0, 4096), &written, 4096, 4096);
            assert!(read_all(&mut *b, buffer, &desc(4096)) == written, "{name}: it does not hold what was written");
        }
        b.destroy_resource(buffer);
        b.destroy_resource(copy);
    }
}

/// A program linked by `glsl` (vertex attribute `pos` at location 0).
pub fn linked(vs: &str, fs: &str) -> std::sync::Arc<glsl::program::Program> {
    let options = glsl::Options::default();
    let vs = glsl::compile(glsl::Stage::Vertex, &[vs], &options).shader.expect("the vertex shader compiles");
    let fs = glsl::compile(glsl::Stage::Fragment, &[fs], &options).shader.expect("the fragment shader compiles");
    let bindings =
        glsl::link::Bindings { attributes: vec![("pos".into(), 0)], feedback: Vec::new(), feedback_separate: false };
    std::sync::Arc::new(glsl::program::link(&vs, &fs, &bindings, &options.limits).program.expect("the program links"))
}

#[test]
fn draws_outside_their_buffers_draw_nothing() {
    use crate::backend::*;
    let program = linked(
        "#version 300 es\nin vec4 pos; void main() { gl_Position = pos; }",
        "#version 300 es\nprecision mediump float; out vec4 o; void main() { o = vec4(1.0); }",
    );
    for (name, mut b, host) in backends() {
        let one = |target, format, width, height| ResourceDesc {
            target,
            format,
            width,
            height,
            depth: 1,
            levels: 1,
            samples: 0,
        };
        let target = b.create_resource(&one(Target::Renderbuffer, Format::Rgba8Unorm, 4, 4)).unwrap();
        let vertices = b.create_resource(&one(Target::Buffer, Format::R8Uint, 48, 1)).unwrap();
        let positions: Vec<u8> = [-1.0f32, -1.0, 0.0, 1.0, 3.0, -1.0, 0.0, 1.0, -1.0, 3.0, 0.0, 1.0]
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();
        b.write(vertices, 0, Region::bytes(0, 48), &positions, 48, 48);
        let indices = b.create_resource(&one(Target::Buffer, Format::R8Uint, 6, 1)).unwrap();
        b.write(indices, 0, Region::bytes(0, 6), &[0, 0, 1, 0, 2, 0], 6, 6);
        let p = b.create_program(program.clone());
        let mut fb = Framebuffer { width: 4, height: 4, ..Framebuffer::default() };
        fb.colors[0] = Some(Surface { resource: target, level: 0, layer: 0 });
        fb.draw_buffers[0] = Some(0);
        let attrib = |offset: usize| Attrib {
            buffer: Some(vertices),
            offset,
            stride: 16,
            size: 4,
            ty: AttribType::Float,
            normalized: false,
            integer: false,
            divisor: 0,
            current: [0; 4],
        };
        let raster = Raster {
            cull: None,
            front_ccw: true,
            polygon_offset: None,
            discard: false,
            line_width: 1.0,
            alpha_to_coverage: false,
            sample_coverage: None,
        };
        let stencil = Stencil {
            func: Func::Always,
            reference: 0,
            value_mask: !0,
            write_mask: !0,
            fail: StencilOp::Keep,
            depth_fail: StencilOp::Keep,
            pass: StencilOp::Keep,
        };
        let blend = Blend {
            enabled: false,
            eq_rgb: BlendEq::Add,
            eq_alpha: BlendEq::Add,
            src_rgb: BlendFactor::One,
            dst_rgb: BlendFactor::Zero,
            src_alpha: BlendFactor::One,
            dst_alpha: BlendFactor::Zero,
            color: [0.0; 4],
            write_mask: [true; 4],
            dither: false,
        };
        let mut draw = |offset: usize, index: bool, first: usize| {
            let attribs = [attrib(offset)];
            let state = DrawState {
                framebuffer: &fb,
                viewport: Viewport { x: 0.0, y: 0.0, w: 4.0, h: 4.0, near: 0.0, far: 1.0 },
                scissor: None,
                raster,
                depth_stencil: DepthStencil {
                    depth_test: false,
                    depth_func: Func::Always,
                    depth_write: false,
                    stencil_test: false,
                    front: stencil,
                    back: stencil,
                },
                blend,
                program: p,
                uniforms: &[],
                blocks: &[],
                textures: &[],
                attribs: &attribs,
                index: index.then_some((indices, IndexType::U16)),
                primitive_restart: false,
                feedback: None,
            };
            // Whether it drew: the host was asked to, or the pixels show it.
            let draws = host.as_ref().map(|h| h.borrow().draws);
            b.draw(&state, &DrawInfo { mode: Mode::Triangles, first, count: 3, indexed: index, instances: 1 });
            let mut px = [0u8; 4];
            b.read(target, 0, Region::new(1, 1, 0, 1, 1, 1), &mut px, 4, 4);
            match (draws, &host) {
                (Some(n), Some(h)) => h.borrow().draws > n,
                _ => px == [255; 4],
            }
        };
        // Indices beyond their buffer, attribute offsets that wrap or are
        // past the buffer: nothing is drawn.
        assert!(!draw(0, true, 1 << 40), "{name}: indices past the buffer");
        assert!(!draw(usize::MAX - 7, true, 0), "{name}: an offset that wraps");
        assert!(!draw(usize::MAX - 7, false, 0), "{name}: an offset that wraps");
        assert!(!draw(48, false, 0), "{name}: an offset at the end");
        // In range, the triangle is drawn.
        assert!(draw(0, true, 0), "{name}: in range");
    }
}

#[test]
fn indices_are_read_as_far_as_their_buffer_goes() {
    use crate::backend::IndexType;
    use crate::soft::vertex::read_indices;
    let data = [1u8, 0, 2, 0, 3];
    assert_eq!(read_indices(&data, 0, 9, IndexType::U16).unwrap(), [1, 2]);
    assert_eq!(read_indices(&data, 4, 9, IndexType::U16).unwrap(), Vec::<u32>::new());
    // An offset past the end reads nothing.
    assert_eq!(read_indices(&data, 100, 3, IndexType::U8).unwrap(), Vec::<u32>::new());
    assert_eq!(read_indices(&data, usize::MAX, 3, IndexType::U32).unwrap(), Vec::<u32>::new());
}

#[test]
fn transfers_too_short_for_their_region_do_nothing() {
    let desc =
        |target, format, width, height| ResourceDesc { target, format, width, height, depth: 1, levels: 1, samples: 0 };
    for (name, mut b, _) in backends() {
        let buffer = b.create_resource(&desc(Target::Buffer, Format::R8Uint, 16, 1)).unwrap();
        let texture = b.create_resource(&desc(Target::Texture2D, Format::Rgba8Unorm, 4, 4)).unwrap();
        // Data shorter than the region, and pitches shorter than a row.
        b.write(buffer, 0, Region::bytes(0, 16), &[7; 8], 16, 16);
        b.write(texture, 0, Region::new(0, 0, 0, 4, 4, 1), &[7; 63], 16, 64);
        b.write(texture, 0, Region::new(0, 0, 0, 4, 4, 1), &[7; 64], 8, 64);
        let mut out = [9u8; 64];
        b.read(buffer, 0, Region::bytes(0, 16), &mut out[..16], 16, 16);
        assert_eq!(out[..16], [0; 16], "{name}: the buffer was written");
        b.read(texture, 0, Region::new(0, 0, 0, 4, 4, 1), &mut out, 16, 64);
        assert_eq!(out, [0; 64], "{name}: the texture was written");
        // Reads into too little memory read nothing.
        let mut short = [9u8; 63];
        b.read(texture, 0, Region::new(0, 0, 0, 4, 4, 1), &mut short, 16, 64);
        assert_eq!(short, [9; 63], "{name}: a short read");
        b.read(buffer, 0, Region::bytes(0, 16), &mut short[..8], 16, 16);
        assert_eq!(short, [9; 63], "{name}: a short read");
    }
}
