// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The browser's compositing at a page's size, as WebKit's TextureMapper
//! does it on Veda: Skia's tiles (BGRA, 512 x 512) uploaded into BGRA
//! textures whole and in parts, an image's BGRA bytes in an RGBA texture
//! that the shader turns round, drawn with TextureMapper's shader (GLSL ES
//! 1.00, a colour matrix applied to every texel) into an RGBA8
//! renderbuffer larger than the GPU renderer's staging memory, then read
//! back whole as BGRA. Run on a GPU (`GL_TEST_BACKEND`) it checks what the
//! small images of the other tests cannot: tiled and compressed layouts,
//! uploads and reads in bands.

use super::*;

const TILE: i32 = 512;

const VS: &str = "attribute vec2 a_vertex;
uniform mat4 u_transform;
varying vec2 v_texCoord;
void main() { v_texCoord = a_vertex; gl_Position = u_transform * vec4(a_vertex, 0.0, 1.0); }";

const FS: &str = "precision mediump float;
uniform sampler2D s_sampler;
uniform mat4 u_textureColorSpaceMatrix;
uniform float u_opacity;
varying vec2 v_texCoord;
void main() { gl_FragColor = u_textureColorSpaceMatrix * texture2D(s_sampler, v_texCoord) * u_opacity; }";

/// A tile's BGRA bytes: each pixel says where it is and in which tile.
fn tile_pixels(tile: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity((TILE * TILE * 4) as usize);
    for y in 0..TILE {
        for x in 0..TILE {
            out.extend_from_slice(&[
                x as u8,
                y as u8,
                tile.wrapping_mul(37) ^ (x >> 8) as u8 ^ ((y >> 8) << 1) as u8,
                255,
            ]);
        }
    }
    out
}

/// The transform that puts the unit square on pixels `x, y, w, h` of a
/// `fw` x `fh` framebuffer, top row first (as TextureMapper draws).
fn place(x: i32, y: i32, w: i32, h: i32, fw: i32, fh: i32) -> [f32; 16] {
    let (sx, sy) = (2.0 * w as f32 / fw as f32, 2.0 * h as f32 / fh as f32);
    let (tx, ty) = (2.0 * x as f32 / fw as f32 - 1.0, 2.0 * y as f32 / fh as f32 - 1.0);
    [sx, 0.0, 0.0, 0.0, 0.0, sy, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, tx, ty, 0.0, 1.0]
}

const IDENTITY: [f32; 16] = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];
/// TextureMapper's matrix for BGRA bytes in an RGBA texture: red and blue
/// swapped.
const BGRA_TO_RGBA: [f32; 16] = [0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0];

#[test]
fn composites_and_reads_back_a_page_as_webkit_does() {
    let (fw, fh) = (1920, 1088);
    let mut c = context(1, 1);
    let rb = c.gen_renderbuffer();
    c.bind_renderbuffer(gl::RENDERBUFFER, rb);
    c.renderbuffer_storage(gl::RENDERBUFFER, gl::RGBA8, fw, fh);
    let fb = c.gen_framebuffer();
    c.bind_framebuffer(gl::FRAMEBUFFER, fb);
    c.framebuffer_renderbuffer(gl::FRAMEBUFFER, gl::COLOR_ATTACHMENT0, gl::RENDERBUFFER, rb);
    assert_eq!(c.check_framebuffer_status(gl::FRAMEBUFFER), gl::FRAMEBUFFER_COMPLETE);
    c.viewport(0, 0, fw, fh);
    c.clear_color(0.0, 0.0, 0.0, 0.0);
    c.clear(gl::COLOR_BUFFER_BIT);

    let p = program(&mut c, VS, FS);
    c.use_program(p);
    buffer_f32(&mut c, gl::ARRAY_BUFFER, &[0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0, 1.0]);
    let at = c.get_attrib_location(p, "a_vertex") as u32;
    c.vertex_attrib_pointer(at, 2, gl::FLOAT, false, 0, 0);
    c.enable_vertex_attrib_array(at);
    let (transform, matrix, opacity) = (
        c.get_uniform_location(p, "u_transform"),
        c.get_uniform_location(p, "u_textureColorSpaceMatrix"),
        c.get_uniform_location(p, "u_opacity"),
    );
    let sampler = c.get_uniform_location(p, "s_sampler");
    c.uniform1i(sampler, 0);
    c.uniform1f(opacity, 1.0);

    // What each pixel of the frame should be, as BGRA bytes.
    let mut expected = vec![0u8; (fw * fh * 4) as usize];
    let put = |expected: &mut Vec<u8>, x0: i32, y0: i32, w: i32, h: i32, src: &[u8], stride: i32| {
        for y in 0..h.min(fh - y0) {
            for x in 0..w.min(fw - x0) {
                let s = ((y * stride + x) * 4) as usize;
                let d = (((y0 + y) * fw + x0 + x) * 4) as usize;
                expected[d..d + 4].copy_from_slice(&src[s..s + 4]);
            }
        }
    };

    // The tiles: BGRA textures, as WebKit makes them for Skia's tiles
    // (EXT_texture_format_BGRA8888), each drawn where it belongs.
    let mut textures = Vec::new();
    let mut tile = 0u8;
    for ty in (0..fh).step_by(TILE as usize) {
        for tx in (0..fw).step_by(TILE as usize) {
            let pixels = tile_pixels(tile);
            let t = c.gen_texture();
            c.bind_texture(gl::TEXTURE_2D, t);
            for (k, v) in [(gl::TEXTURE_MIN_FILTER, gl::NEAREST), (gl::TEXTURE_MAG_FILTER, gl::NEAREST)] {
                c.tex_parameteri(gl::TEXTURE_2D, k, v as i32);
            }
            c.tex_image_2d(
                gl::TEXTURE_2D,
                0,
                gl::BGRA_EXT,
                TILE,
                TILE,
                0,
                gl::BGRA_EXT,
                gl::UNSIGNED_BYTE,
                &pixels[..],
            );
            put(&mut expected, tx, ty, TILE, TILE, &pixels, TILE);
            // A part painted again, from rows of the whole tile's width
            // (GL_UNPACK_ROW_LENGTH, as WebKit sends parts of a tile).
            let (px0, py0, pw, ph) = (100 + i32::from(tile) * 7, 50 + i32::from(tile) * 3, 201, 133);
            let fresh: Vec<u8> = tile_pixels(tile ^ 0x5a).iter().map(|b| b ^ 0x0f).collect();
            let part = &fresh[((py0 * TILE + px0) * 4) as usize..];
            c.pixel_storei(gl::UNPACK_ROW_LENGTH, TILE);
            c.tex_sub_image_2d(gl::TEXTURE_2D, 0, px0, py0, pw, ph, gl::BGRA_EXT, gl::UNSIGNED_BYTE, part);
            c.pixel_storei(gl::UNPACK_ROW_LENGTH, 0);
            put(&mut expected, tx + px0, ty + py0, pw, ph, part, TILE);
            no_error(&mut c);
            c.uniform_matrix4fv(transform, false, &place(tx, ty, TILE, TILE, fw, fh));
            c.uniform_matrix4fv(matrix, false, &IDENTITY);
            c.draw_arrays(gl::TRIANGLES, 0, 6);
            textures.push(t);
            tile += 1;
        }
    }

    // An image: BGRA bytes in an RGBA texture, turned round by the shader.
    let (ix, iy, iw, ih) = (333, 222, 300, 200);
    let image = tile_pixels(200);
    let t = c.gen_texture();
    c.bind_texture(gl::TEXTURE_2D, t);
    for (k, v) in [(gl::TEXTURE_MIN_FILTER, gl::NEAREST), (gl::TEXTURE_MAG_FILTER, gl::NEAREST)] {
        c.tex_parameteri(gl::TEXTURE_2D, k, v as i32);
    }
    c.pixel_storei(gl::UNPACK_ROW_LENGTH, TILE);
    c.tex_image_2d(gl::TEXTURE_2D, 0, gl::RGBA8, iw, ih, 0, gl::RGBA, gl::UNSIGNED_BYTE, &image[..]);
    c.pixel_storei(gl::UNPACK_ROW_LENGTH, 0);
    c.uniform_matrix4fv(transform, false, &place(ix, iy, iw, ih, fw, fh));
    c.uniform_matrix4fv(matrix, false, &BGRA_TO_RGBA);
    c.draw_arrays(gl::TRIANGLES, 0, 6);
    // Its RGBA texels are the image's bytes, which the matrix swaps back:
    // the frame holds the image's BGRA bytes there.
    put(&mut expected, ix, iy, iw, ih, &image, TILE);
    no_error(&mut c);

    // The frame, read back whole as BGRA (AcceleratedSurface's readback).
    let mut frame = vec![0u8; (fw * fh * 4) as usize];
    c.read_pixels(0, 0, fw, fh, gl::BGRA_EXT, gl::UNSIGNED_BYTE, &mut frame[..]);
    no_error(&mut c);
    let wrong = (0..(fw * fh) as usize).filter(|&i| frame[i * 4..i * 4 + 4] != expected[i * 4..i * 4 + 4]).count();
    let first = (0..(fw * fh) as usize).find(|&i| frame[i * 4..i * 4 + 4] != expected[i * 4..i * 4 + 4]);
    assert_eq!(
        wrong,
        0,
        "{wrong} pixels wrong; the first at {:?}: {:?}, not {:?}",
        first.map(|i| (i as i32 % fw, i as i32 / fw)),
        first.map(|i| &frame[i * 4..i * 4 + 4]),
        first.map(|i| &expected[i * 4..i * 4 + 4]),
    );
}

const SOLID_VS: &str = "attribute vec2 a_vertex;
uniform mat4 u_transform;
void main() { gl_Position = u_transform * vec4(a_vertex, 0.0, 1.0); }";

const SOLID_FS: &str = "precision mediump float;
uniform vec4 u_color;
void main() { gl_FragColor = u_color; }";

#[test]
fn clears_and_fills_a_page_as_webkit_does() {
    // A page's background (vedaos.org's navy, which shows any swap of red
    // and blue), cleared over all of a large renderbuffer, as GPUs clear
    // fast; then filled in part with TextureMapper's solid colour, and
    // cleared again in part (a scissor), and read back whole as BGRA.
    let (fw, fh) = (1920, 1088);
    let mut c = context(1, 1);
    let rb = c.gen_renderbuffer();
    c.bind_renderbuffer(gl::RENDERBUFFER, rb);
    c.renderbuffer_storage(gl::RENDERBUFFER, gl::RGBA8, fw, fh);
    let fb = c.gen_framebuffer();
    c.bind_framebuffer(gl::FRAMEBUFFER, fb);
    c.framebuffer_renderbuffer(gl::FRAMEBUFFER, gl::COLOR_ATTACHMENT0, gl::RENDERBUFFER, rb);
    c.viewport(0, 0, fw, fh);
    let navy = [0x0du8, 0x0f, 0x2b, 0xff];
    let f = |v: u8| f32::from(v) / 255.0;
    c.clear_color(f(navy[0]), f(navy[1]), f(navy[2]), 1.0);
    c.clear(gl::COLOR_BUFFER_BIT);

    let p = program(&mut c, SOLID_VS, SOLID_FS);
    c.use_program(p);
    buffer_f32(&mut c, gl::ARRAY_BUFFER, &[0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0, 1.0]);
    let at = c.get_attrib_location(p, "a_vertex") as u32;
    c.vertex_attrib_pointer(at, 2, gl::FLOAT, false, 0, 0);
    c.enable_vertex_attrib_array(at);
    let (transform, color) = (c.get_uniform_location(p, "u_transform"), c.get_uniform_location(p, "u_color"));
    let orange = [0xf0u8, 0x80, 0x10, 0xff];
    let (sx, sy, sw, sh) = (700, 300, 640, 360);
    c.uniform_matrix4fv(transform, false, &place(sx, sy, sw, sh, fw, fh));
    c.uniform4f(color, f(orange[0]), f(orange[1]), f(orange[2]), 1.0);
    c.draw_arrays(gl::TRIANGLES, 0, 6);
    let teal = [0x10u8, 0x90, 0x80, 0xff];
    let (cx, cy, cw, ch) = (100, 900, 300, 120);
    c.enable(gl::SCISSOR_TEST);
    c.scissor(cx, cy, cw, ch);
    c.clear_color(f(teal[0]), f(teal[1]), f(teal[2]), 1.0);
    c.clear(gl::COLOR_BUFFER_BIT);
    c.disable(gl::SCISSOR_TEST);
    no_error(&mut c);

    let mut frame = vec![0u8; (fw * fh * 4) as usize];
    c.read_pixels(0, 0, fw, fh, gl::BGRA_EXT, gl::UNSIGNED_BYTE, &mut frame[..]);
    no_error(&mut c);
    let inside =
        |x: i32, y: i32, (rx, ry, rw, rh): (i32, i32, i32, i32)| x >= rx && x < rx + rw && y >= ry && y < ry + rh;
    let mut wrong = 0;
    let mut first = None;
    for y in 0..fh {
        for x in 0..fw {
            let rgba = if inside(x, y, (cx, cy, cw, ch)) {
                teal
            } else if inside(x, y, (sx, sy, sw, sh)) {
                orange
            } else {
                navy
            };
            let want = [rgba[2], rgba[1], rgba[0], rgba[3]];
            let i = ((y * fw + x) * 4) as usize;
            let got = [frame[i], frame[i + 1], frame[i + 2], frame[i + 3]];
            if got.iter().zip(want).any(|(g, w)| g.abs_diff(w) > 1) {
                wrong += 1;
                first.get_or_insert((x, y, got, want));
            }
        }
    }
    assert_eq!(wrong, 0, "{wrong} pixels wrong; the first (x, y, read, BGRA wanted): {first:?}");
}
