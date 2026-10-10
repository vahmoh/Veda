//! Small helpers over OpenGL ES: programs with their uniforms, meshes in
//! buffers, textures and framebuffers.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::prelude::*;
use vgl::{Context, gl};

use crate::mesh::{Mesh, STRIDE};

/// Vertex attribute locations, the same in every program.
pub mod attr {
    pub const POS: u32 = 0;
    pub const NORMAL: u32 = 1;
    pub const UV: u32 = 2;
    pub const PARAMS: u32 = 3;
    /// Per-instance attributes start here.
    pub const INST0: u32 = 4;
    pub const INST1: u32 = 5;
    pub const INST2: u32 = 6;
    pub const INST3: u32 = 7;
    pub const INST4: u32 = 8;
}

/// The attribute names programs declare, by location.
const ATTRIBS: [(u32, &str); 9] = [
    (attr::POS, "a_pos"),
    (attr::NORMAL, "a_normal"),
    (attr::UV, "a_uv"),
    (attr::PARAMS, "a_params"),
    (attr::INST0, "a_inst0"),
    (attr::INST1, "a_inst1"),
    (attr::INST2, "a_inst2"),
    (attr::INST3, "a_inst3"),
    (attr::INST4, "a_inst4"),
];

/// The uniform block every lit program shares, at binding 0.
pub const FRAME_BLOCK: &str = "Frame";
pub const FRAME_BINDING: u32 = 0;

/// A linked program and the locations of its uniforms.
pub struct Program {
    pub id: u32,
    names: Vec<&'static str>,
    locations: Vec<i32>,
}

impl Program {
    /// Compiles and links a program from a vertex and a fragment shader
    /// (complete sources), binding the shared attribute locations and the
    /// frame block, and looks up `uniforms`; `samplers` are set to texture
    /// units 0, 1, 2... in order.
    pub fn new(
        gl: &mut Context,
        name: &str,
        vs: &str,
        fs: &str,
        uniforms: &[&'static str],
        samplers: &[&'static str],
    ) -> Result<Program, String> {
        let compile = |gl: &mut Context, kind: u32, src: &str| -> Result<u32, String> {
            let s = gl.create_shader(kind);
            gl.shader_source(s, &[src]);
            gl.compile_shader(s);
            if gl.get_shaderiv(s, gl::COMPILE_STATUS) == 0 {
                let what = if kind == gl::VERTEX_SHADER { "vertex" } else { "fragment" };
                let log = gl.get_shader_info_log(s);
                gl.delete_shader(s);
                return Err(format!("{name}: {what} shader: {log}"));
            }
            Ok(s)
        };
        let v = compile(gl, gl::VERTEX_SHADER, vs)?;
        let f = match compile(gl, gl::FRAGMENT_SHADER, fs) {
            Ok(f) => f,
            Err(e) => {
                gl.delete_shader(v);
                return Err(e);
            }
        };
        let id = gl.create_program();
        gl.attach_shader(id, v);
        gl.attach_shader(id, f);
        for (loc, attr) in ATTRIBS {
            gl.bind_attrib_location(id, loc, attr);
        }
        gl.link_program(id);
        gl.delete_shader(v);
        gl.delete_shader(f);
        if gl.get_programiv(id, gl::LINK_STATUS) == 0 {
            let log = gl.get_program_info_log(id);
            gl.delete_program(id);
            return Err(format!("{name}: link: {log}"));
        }
        let block = gl.get_uniform_block_index(id, FRAME_BLOCK);
        if block != gl::INVALID_INDEX {
            gl.uniform_block_binding(id, block, FRAME_BINDING);
        }
        let mut names = Vec::new();
        let mut locations = Vec::new();
        for &u in uniforms.iter().chain(samplers) {
            names.push(u);
            locations.push(gl.get_uniform_location(id, u));
        }
        gl.use_program(id);
        for (unit, &s) in samplers.iter().enumerate() {
            let at = gl.get_uniform_location(id, s);
            gl.uniform1i(at, unit as i32);
        }
        gl.use_program(0);
        Ok(Program { id, names, locations })
    }

    /// The location of uniform `name` (-1 if the program has none: setting
    /// it does nothing).
    pub fn at(&self, name: &str) -> i32 {
        self.names.iter().position(|n| *n == name).map_or(-1, |i| self.locations[i])
    }
}

/// A mesh in buffers, with its vertex array.
pub struct Geometry {
    pub vao: u32,
    pub vbo: u32,
    pub ibo: u32,
    pub count: i32,
}

impl Geometry {
    /// Uploads `mesh` with the shared vertex layout.
    pub fn new(gl: &mut Context, mesh: &Mesh) -> Geometry {
        let vao = gl.gen_vertex_array();
        gl.bind_vertex_array(vao);
        let vbo = gl.gen_buffer();
        gl.bind_buffer(gl::ARRAY_BUFFER, vbo);
        gl.buffer_data(gl::ARRAY_BUFFER, &mesh.vertex_bytes(), gl::STATIC_DRAW);
        set_vertex_layout(gl);
        let ibo = gl.gen_buffer();
        gl.bind_buffer(gl::ELEMENT_ARRAY_BUFFER, ibo);
        gl.buffer_data(gl::ELEMENT_ARRAY_BUFFER, &mesh.index_bytes(), gl::STATIC_DRAW);
        gl.bind_vertex_array(0);
        Geometry { vao, vbo, ibo, count: mesh.indices.len() as i32 }
    }

    /// Draws every triangle.
    pub fn draw(&self, gl: &mut Context) {
        if self.count > 0 {
            gl.bind_vertex_array(self.vao);
            gl.draw_elements(gl::TRIANGLES, self.count, gl::UNSIGNED_INT, 0);
        }
    }

    /// Draws `instances` copies.
    pub fn draw_instanced(&self, gl: &mut Context, instances: i32) {
        if self.count > 0 && instances > 0 {
            gl.bind_vertex_array(self.vao);
            gl.draw_elements_instanced(gl::TRIANGLES, self.count, gl::UNSIGNED_INT, 0, instances);
        }
    }

    pub fn delete(&self, gl: &mut Context) {
        gl.delete_vertex_arrays(&[self.vao]);
        gl.delete_buffers(&[self.vbo, self.ibo]);
    }
}

/// Points the bound vertex array's shared attributes at the bound array
/// buffer, laid out as [`Mesh::vertex_bytes`] has them.
pub fn set_vertex_layout(gl: &mut Context) {
    let s = STRIDE as i32;
    gl.vertex_attrib_pointer(attr::POS, 3, gl::FLOAT, false, s, 0);
    gl.vertex_attrib_pointer(attr::NORMAL, 3, gl::FLOAT, false, s, 12);
    gl.vertex_attrib_pointer(attr::UV, 2, gl::FLOAT, false, s, 24);
    gl.vertex_attrib_pointer(attr::PARAMS, 4, gl::UNSIGNED_BYTE, true, s, 32);
    for a in [attr::POS, attr::NORMAL, attr::UV, attr::PARAMS] {
        gl.enable_vertex_attrib_array(a);
    }
}

/// Per-instance attributes from the bound array buffer: `floats` floats
/// per instance, in vec4 attributes from [`attr::INST0`].
pub fn set_instance_layout(gl: &mut Context, floats: usize, offset: usize) {
    let stride = (floats * 4) as i32;
    let mut at = 0;
    let mut loc = attr::INST0;
    while at < floats {
        let n = (floats - at).min(4) as i32;
        gl.vertex_attrib_pointer(loc, n, gl::FLOAT, false, stride, offset + at * 4);
        gl.enable_vertex_attrib_array(loc);
        gl.vertex_attrib_divisor(loc, 1);
        at += 4;
        loc += 1;
    }
}

/// Floats as little-endian bytes.
pub fn float_bytes(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for f in v {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

/// A matrix's columns as 16 floats.
pub fn mat(m: &Mat4) -> [f32; 16] {
    m.to_cols_array()
}

/// A 2D texture of RGBA8 texels (`pixels` rows from the first), with a
/// full chain of mipmaps, repeating, filtered trilinearly (anisotropically
/// where the renderer can).
pub fn texture_rgba(gl: &mut Context, w: i32, h: i32, pixels: &[u8], srgb: bool, anisotropy: f32) -> u32 {
    let t = gl.gen_texture();
    gl.bind_texture(gl::TEXTURE_2D, t);
    let levels = mip_levels(w, h);
    gl.tex_storage_2d(gl::TEXTURE_2D, levels, if srgb { gl::SRGB8_ALPHA8 } else { gl::RGBA8 }, w, h);
    gl.tex_sub_image_2d(gl::TEXTURE_2D, 0, 0, 0, w, h, gl::RGBA, gl::UNSIGNED_BYTE, pixels);
    gl.generate_mipmap(gl::TEXTURE_2D);
    gl.tex_parameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::LINEAR_MIPMAP_LINEAR as i32);
    gl.tex_parameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::LINEAR as i32);
    gl.tex_parameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_S, gl::REPEAT as i32);
    gl.tex_parameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_T, gl::REPEAT as i32);
    if anisotropy > 1.0 {
        gl.tex_parameterf(gl::TEXTURE_2D, gl::TEXTURE_MAX_ANISOTROPY_EXT, anisotropy);
    }
    t
}

/// A 2D array texture of `layers` RGBA8 images of `w` x `h` (back to back
/// in `pixels`), mipmapped and repeating like [`texture_rgba`].
pub fn texture_array(gl: &mut Context, w: i32, h: i32, layers: i32, pixels: &[u8], srgb: bool, anisotropy: f32) -> u32 {
    let t = gl.gen_texture();
    gl.bind_texture(gl::TEXTURE_2D_ARRAY, t);
    let levels = mip_levels(w, h);
    gl.tex_storage_3d(gl::TEXTURE_2D_ARRAY, levels, if srgb { gl::SRGB8_ALPHA8 } else { gl::RGBA8 }, w, h, layers);
    gl.tex_sub_image_3d(gl::TEXTURE_2D_ARRAY, 0, 0, 0, 0, w, h, layers, gl::RGBA, gl::UNSIGNED_BYTE, pixels);
    gl.generate_mipmap(gl::TEXTURE_2D_ARRAY);
    gl.tex_parameteri(gl::TEXTURE_2D_ARRAY, gl::TEXTURE_MIN_FILTER, gl::LINEAR_MIPMAP_LINEAR as i32);
    gl.tex_parameteri(gl::TEXTURE_2D_ARRAY, gl::TEXTURE_MAG_FILTER, gl::LINEAR as i32);
    gl.tex_parameteri(gl::TEXTURE_2D_ARRAY, gl::TEXTURE_WRAP_S, gl::REPEAT as i32);
    gl.tex_parameteri(gl::TEXTURE_2D_ARRAY, gl::TEXTURE_WRAP_T, gl::REPEAT as i32);
    if anisotropy > 1.0 {
        gl.tex_parameterf(gl::TEXTURE_2D_ARRAY, gl::TEXTURE_MAX_ANISOTROPY_EXT, anisotropy);
    }
    t
}

/// Mipmap levels down to 1x1.
pub fn mip_levels(w: i32, h: i32) -> i32 {
    let mut n = 1;
    let mut s = w.max(h);
    while s > 1 {
        s /= 2;
        n += 1;
    }
    n
}

/// A full-screen triangle's vertex array (positions in clip space, at
/// the shared position attribute).
pub fn fullscreen_triangle(gl: &mut Context) -> u32 {
    let vao = gl.gen_vertex_array();
    gl.bind_vertex_array(vao);
    let vbo = gl.gen_buffer();
    gl.bind_buffer(gl::ARRAY_BUFFER, vbo);
    gl.buffer_data(gl::ARRAY_BUFFER, &float_bytes(&[-1.0, -1.0, 3.0, -1.0, -1.0, 3.0]), gl::STATIC_DRAW);
    gl.vertex_attrib_pointer(attr::POS, 2, gl::FLOAT, false, 0, 0);
    gl.enable_vertex_attrib_array(attr::POS);
    gl.bind_vertex_array(0);
    vao
}
