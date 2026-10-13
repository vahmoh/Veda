// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The pictures, with OpenGL ES 3.0: the world's meshes on the GPU, the
//! sky, and a frame drawn from a camera.
//!
//! At start-up the renderer uploads the world (the road's chunks, the
//! ground's chunks with two levels of detail, the sea), paints the
//! textures, compiles the programs and paints the sky into a cube map,
//! which the background and every reflection sample. Each frame it fills
//! the frame's uniform block once, then draws what the camera can see,
//! front to back, and the sky where nothing else is.

pub mod gpu;
mod passing;
pub mod post;
pub mod shaders;

pub use passing::Passing;
use passing::PassingGpu;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::prelude::*;
use vgl::{Context, gl};

use crate::carmodel::{self, Kind};
use crate::city::Block;
use crate::env::Environment;
use crate::groundmesh;
use crate::lamps::{self, Lamp};
use crate::roadmesh;
use crate::scenery;
use crate::terrain::{HALF_X, HALF_Z};
use crate::texgen;
use crate::world::World;
use gpu::{Geometry, Program, float_bytes, mat};
use shaders::{MAX_LIGHTS, source};

/// What the renderer can afford.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quality {
    /// A software renderer: small textures, short view, no
    /// post-processing.
    Low,
    /// A GPU.
    High,
}

impl Quality {
    /// The quality a renderer called `name` (`GL_RENDERER`) affords: the
    /// software renderers (Veda's, and Mesa's softpipe and llvmpipe) low,
    /// GPUs high.
    pub fn for_renderer(name: &str) -> Quality {
        let soft = ["software", "softpipe", "llvmpipe"];
        if soft.iter().any(|s| name.to_ascii_lowercase().contains(s)) { Quality::Low } else { Quality::High }
    }
}

/// What renders, in words, from the renderer's name (`GL_RENDERER`): the
/// GPU as Mesa names it, without the trademarks; under QEMU, whose virgl
/// renders on the host's GPU, the host's, saying so; or the software
/// renderer.
pub fn device_name(renderer: &str) -> String {
    let (name, host) = match renderer.strip_prefix("virgl (").and_then(|r| r.strip_suffix(')')) {
        Some(inner) => (inner, true),
        None => (renderer, false),
    };
    let name = name.strip_prefix("Mesa ").unwrap_or(name);
    // NVIDIA's own driver adds the bus and the instruction set.
    let name = name.split("/PCIe").next().unwrap_or(name);
    let mut words = String::new();
    for word in name.replace("(R)", "").replace("(TM)", "").split_whitespace() {
        if !words.is_empty() {
            words.push(' ');
        }
        words.push_str(word);
    }
    if host {
        words.push_str(" through QEMU");
    }
    words
}

/// A camera.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub eye: Vec3,
    pub target: Vec3,
    pub up: Vec3,
    /// Vertical field of view (radians).
    pub fov: f32,
}

impl Camera {
    pub fn view(&self) -> Mat4 {
        glam::camera::rh::view::look_at_mat4(self.eye, self.target, self.up)
    }
}

/// How a car is painted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Livery {
    Plain,
    /// Black and white, a stripe along the doors.
    Police,
    /// A checker band.
    Taxi,
}

/// A car to draw.
#[derive(Clone, Copy, Debug)]
pub struct CarView {
    pub kind: Kind,
    /// Car space (+z forward, +x to its left) to the world.
    pub transform: Mat4,
    pub paint: Vec3,
    pub livery: Livery,
    /// The wheels' turn about their axles, and the front wheels' steering
    /// (radians, to the left positive).
    pub spin: f32,
    pub steer: f32,
    /// How brightly its headlights and its brake lights shine (0..1).
    pub headlights: f32,
    pub brake: f32,
    /// How brightly the light bar's red and blue halves shine now (0..1).
    pub bar: [f32; 2],
}

/// A light that moves: a light bar's flash (a point), a headlight (a
/// spot).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Light {
    pub pos: Vec3,
    /// Colour and strength (linear).
    pub color: Vec3,
    /// Nothing beyond this (m).
    pub radius: f32,
    /// A spot's direction and the cosine of its half-angle (a point's
    /// cone is `None`).
    pub spot: Option<(Vec3, f32)>,
}

/// What a frame shows.
pub struct Frame<'a> {
    pub camera: Camera,
    pub cars: &'a [CarView],
    /// The lights that move; the strongest near the camera are used.
    pub lights: &'a [Light],
    /// Seconds, for what moves by itself (the sea, the light bars, the
    /// rain) and what ages (skid marks): the game's clock.
    pub time: f32,
    /// Smoke, sparks, skid marks and rain.
    pub passing: Passing<'a>,
}

/// The nearest and farthest the camera sees (m).
pub const NEAR: f32 = 0.35;
pub const FAR: f32 = 7000.0;

/// A sphere is outside the frustum `planes` (each `(normal, d)`, inside
/// where `normal . p + d >= 0`).
fn outside(planes: &[Vec4; 6], center: Vec3, radius: f32) -> bool {
    planes.iter().any(|p| p.xyz().dot(center) + p.w < -radius * p.xyz().length())
}

/// The frustum's planes from a view-projection matrix (OpenGL's clip
/// space).
fn frustum(m: &Mat4) -> [Vec4; 6] {
    let r = [m.row(0), m.row(1), m.row(2), m.row(3)];
    [r[3] + r[0], r[3] - r[0], r[3] + r[1], r[3] - r[1], r[3] + r[2], r[3] - r[2]]
}

/// A chunk of road on the GPU.
struct RoadGpu {
    tarmac: Geometry,
    sides: Geometry,
    center: Vec3,
    radius: f32,
}

/// A chunk of ground on the GPU: one vertex buffer, two index buffers.
struct GroundGpu {
    fine: (u32, i32),
    coarse: (u32, i32),
    center: Vec3,
    radius: f32,
}

/// Sizes that depend on the quality.
struct Sizes {
    sky: i32,
    asphalt: usize,
    surfaces: usize,
    ground: usize,
    ripples: usize,
    /// Beyond this the ground is drawn coarse (m).
    fine_ground: f32,
}

impl Sizes {
    fn of(q: Quality) -> Sizes {
        match q {
            Quality::Low => {
                Sizes { sky: 64, asphalt: 256, surfaces: 128, ground: 128, ripples: 128, fine_ground: 200.0 }
            }
            Quality::High => {
                Sizes { sky: 256, asphalt: 512, surfaces: 256, ground: 512, ripples: 256, fine_ground: 600.0 }
            }
        }
    }
}

/// The renderer and everything it keeps on the GPU.
pub struct Renderer {
    quality: Quality,
    sizes: Sizes,
    env: Environment,
    sky: Program,
    terrain: Program,
    tarmac: Program,
    surface: Program,
    water: Program,
    building: Program,
    car: Program,
    prop: Program,
    glow: Program,
    frame_ubo: u32,
    /// The street lights' pools, seen from above, and a stand-in for the
    /// sun's shadows where there are none.
    light_map: u32,
    no_shadow: u32,
    /// Each kind of street light, with its instances.
    lamp_meshes: Vec<(Geometry, i32, lamps::Kind)>,
    /// The trees (pines, broad trees): their meshes, an instance buffer
    /// refilled with those in view, and how many are in it; and where they
    /// all are.
    tree_meshes: [(Geometry, u32, i32); 2],
    trees: Vec<scenery::Tree>,
    /// The harbour bridge's towers and cables (one instance).
    bridge: Geometry,
    /// The billboards' frames and the neon signs (all of them, instanced),
    /// and the billboards' faces with their pictures.
    boards: (Geometry, i32),
    neons: (Geometry, i32),
    faces: (Geometry, i32),
    sign: Program,
    signs: u32,
    /// The lamps' glows (an instanced quad): those lit after dark, and
    /// those day and night (a tunnel's lamps, the bridge's beacons).
    lamp_glows: (u32, i32),
    always_glows: (u32, i32),
    /// The cars' lights' glows, refilled every frame (the vertex array and
    /// its instance buffer).
    car_glows: (u32, u32),
    env_map: u32,
    asphalt: u32,
    surfaces: u32,
    ground_tex: u32,
    ripples: u32,
    walls: u32,
    road: Vec<RoadGpu>,
    ground: Vec<GroundGpu>,
    sea: Geometry,
    /// The unit box every block is drawn with, and the blocks.
    blocks: (Geometry, i32),
    /// Each kind of car's body and wheel, drawn instanced.
    cars: Vec<CarGpu>,
    fullscreen: u32,
    dash: f32,
    /// Post-processing, with a GPU.
    post: Option<post::Post>,
    /// The sun's shadows, with a GPU, by day (made when first needed).
    shadow: Option<Shadow>,
    /// What the programs are compiled with.
    defines: &'static str,
    /// Skid marks, particles and rain.
    passing: PassingGpu,
    /// Statistics of the last frame.
    pub stats: Stats,
}

/// What the last frame drew.
#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    pub draws: u32,
    pub triangles: u32,
}

/// A kind of car on the GPU: its body and its wheel, each with a buffer
/// of this frame's instances.
struct CarGpu {
    kind: Kind,
    body: Geometry,
    wheel: Geometry,
    /// Where its wheels are (car space).
    wheels: [Vec3; 4],
    body_instances: u32,
    wheel_instances: u32,
    /// How many of each this frame.
    bodies: i32,
    wheels_drawn: i32,
}

/// Floats per car instance: three rows of the transform, paint and
/// livery, lamps.
const CAR_FLOATS: usize = 20;

/// The sun's shadow map and the programs that draw its casters.
struct Shadow {
    fbo: u32,
    tex: u32,
    size: i32,
    statik: Program,
    building: Program,
    prop: Program,
    car: Program,
}

impl Shadow {
    /// The shadow map and the programs that draw into it.
    fn new(gl: &mut Context, defines: &str) -> Result<Shadow, String> {
        // The prelude's samplers at their units, as in every lit program
        // (samplers of different kinds may not share one).
        let depth = |gl: &mut Context, name: &str, vs: &str, uniforms: &[&'static str]| {
            let (v, f) = (source(defines, true, vs), source(defines, true, shaders::DEPTH_FS));
            Program::new(gl, name, &v, &f, uniforms, &gpu::LIT_SAMPLERS)
        };
        let statik = depth(gl, "shadow static", shaders::STATIC_VS, &[])?;
        let building = depth(gl, "shadow buildings", shaders::BUILDING_VS, &[])?;
        let prop = depth(gl, "shadow props", shaders::PROP_VS, &[])?;
        let car = depth(gl, "shadow cars", shaders::CAR_VS, &[])?;
        let size = 2048;
        let tex = gl.gen_texture();
        gl.bind_texture(gl::TEXTURE_2D, tex);
        gl.tex_storage_2d(gl::TEXTURE_2D, 1, gl::DEPTH_COMPONENT24, size, size);
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
        Ok(Shadow { fbo, tex, size, statik, building, prop, car })
    }
}

/// Whether the sun casts shadows in `env` at `quality` (with a GPU, by day
/// and at sunset).
fn casts_shadows(env: &Environment, quality: Quality) -> bool {
    quality == Quality::High && env.shadows > 0.0 && env.time != crate::env::TimeOfDay::Night
}

/// Half the side of the square the shadow map covers (m).
const SHADOW_HALF: f32 = 95.0;

/// The floats of the frame's uniform block (std140: matrices, then
/// vectors of four).
const FRAME_FLOATS: usize = 16 * 2 + 4 * 10 + 4 * 3 * MAX_LIGHTS;

impl Renderer {
    /// Uploads `world` and prepares to draw it lit by `env`.
    pub fn new(gl: &mut Context, world: &World, env: Environment, quality: Quality) -> Result<Renderer, String> {
        let sizes = Sizes::of(quality);
        // A GPU draws in high dynamic range and tone maps afterwards; a
        // software renderer (or a GPU that cannot draw in floats) tone
        // maps in each program.
        let hdr = quality == Quality::High && gl.backend().caps().color_buffer_float;
        let defines = match (quality, hdr) {
            (Quality::Low, _) => "#define TONEMAP\n#define LOW\n",
            (Quality::High, false) => "#define TONEMAP\n",
            (Quality::High, true) => "",
        };
        // Every lit program samples the sky, the street lights' map and the
        // shadow map at the shared units, and its own texture (if any) at
        // the next.
        let lit =
            |gl: &mut Context, name: &str, vs: &str, fs: &str, uniforms: &[&'static str], own: Option<&'static str>| {
                let mut samplers = gpu::LIT_SAMPLERS.to_vec();
                if let Some(t) = own {
                    samplers.push((t, gpu::unit::OWN));
                }
                Program::new(gl, name, &source(defines, true, vs), &source(defines, true, fs), uniforms, &samplers)
            };
        let sky = lit(gl, "sky", shaders::SKY_VS, shaders::SKY_FS, &["u_invViewProj", "u_stars"], None)?;
        let terrain = lit(gl, "terrain", shaders::STATIC_VS, shaders::TERRAIN_FS, &[], Some("u_ground"))?;
        let tarmac = lit(gl, "tarmac", shaders::STATIC_VS, shaders::TARMAC_FS, &["u_dash"], Some("u_asphalt"))?;
        let surface = lit(gl, "surface", shaders::STATIC_VS, shaders::SURFACE_FS, &[], Some("u_surfaces"))?;
        let water = lit(gl, "water", shaders::STATIC_VS, shaders::WATER_FS, &[], Some("u_ripples"))?;
        let building = lit(gl, "building", shaders::BUILDING_VS, shaders::BUILDING_FS, &[], Some("u_walls"))?;
        let prop = lit(gl, "prop", shaders::PROP_VS, shaders::PROP_FS, &["u_always"], None)?;
        let glow = lit(gl, "glow", shaders::GLOW_VS, shaders::GLOW_FS, &["u_right", "u_up"], None)?;
        let car = lit(gl, "car", shaders::CAR_VS, shaders::CAR_FS, &["u_shape"], None)?;

        let frame_ubo = gl.gen_buffer();
        gl.bind_buffer(gl::UNIFORM_BUFFER, frame_ubo);
        gl.buffer_data_size(gl::UNIFORM_BUFFER, FRAME_FLOATS * 4, gl::DYNAMIC_DRAW);
        gl.bind_buffer(gl::UNIFORM_BUFFER, 0);

        // Textures.
        let aniso = gl.backend().caps().max_anisotropy.min(8.0);
        let asphalt = {
            let img = texgen::asphalt(sizes.asphalt);
            gpu::texture_rgba(gl, img.w as i32, img.h as i32, &img.data, true, aniso)
        };
        let array = |gl: &mut Context, imgs: &[texgen::Image]| {
            let (w, h) = (imgs[0].w, imgs[0].h);
            let mut data = Vec::with_capacity(w * h * 4 * imgs.len());
            for i in imgs {
                data.extend_from_slice(&i.data);
            }
            gpu::texture_array(gl, w as i32, h as i32, imgs.len() as i32, &data, true, aniso)
        };
        let surfaces = array(gl, &texgen::surfaces(sizes.surfaces));
        let ground_tex = array(gl, &texgen::ground(sizes.ground));
        let ripples = {
            let img = texgen::ripples(sizes.ripples);
            gpu::texture_rgba(gl, img.w as i32, img.h as i32, &img.data, false, aniso)
        };
        let walls = array(gl, &texgen::walls(sizes.surfaces));

        // The world's meshes.
        let road = roadmesh::chunks(&world.road)
            .into_iter()
            .map(|c| RoadGpu {
                tarmac: Geometry::new(gl, &divided_flags(&c.tarmac, world)),
                sides: Geometry::new(gl, &c.sides),
                center: c.center,
                radius: c.radius,
            })
            .collect();
        let ground = groundmesh::ground_chunks(&world.terrain)
            .into_iter()
            .map(|c| {
                let vbo = gl.gen_buffer();
                gl.bind_buffer(gl::ARRAY_BUFFER, vbo);
                gl.buffer_data(gl::ARRAY_BUFFER, &c.mesh.vertex_bytes(), gl::STATIC_DRAW);
                let level = |gl: &mut Context, idx: &[u32]| {
                    let vao = gl.gen_vertex_array();
                    gl.bind_vertex_array(vao);
                    gl.bind_buffer(gl::ARRAY_BUFFER, vbo);
                    gpu::set_vertex_layout(gl);
                    let ibo = gl.gen_buffer();
                    gl.bind_buffer(gl::ELEMENT_ARRAY_BUFFER, ibo);
                    let bytes: Vec<u8> = idx.iter().flat_map(|i| i.to_le_bytes()).collect();
                    gl.buffer_data(gl::ELEMENT_ARRAY_BUFFER, &bytes, gl::STATIC_DRAW);
                    gl.bind_vertex_array(0);
                    (vao, idx.len() as i32)
                };
                GroundGpu { fine: level(gl, &c.fine), coarse: level(gl, &c.coarse), center: c.center, radius: c.radius }
            })
            .collect();
        let sea = Geometry::new(gl, &groundmesh::sea(&world.terrain));
        let blocks = upload_blocks(gl, &world.blocks);
        let cars = Kind::ALL
            .iter()
            .map(|&k| {
                let m = carmodel::build(k);
                let body = Geometry::new(gl, &m.body);
                let wheel = Geometry::new(gl, &m.wheel);
                let mut instances = |g: &Geometry| {
                    gl.bind_vertex_array(g.vao);
                    let b = gl.gen_buffer();
                    gl.bind_buffer(gl::ARRAY_BUFFER, b);
                    gl.buffer_data_size(gl::ARRAY_BUFFER, CAR_FLOATS * 4, gl::STREAM_DRAW);
                    gpu::set_instance_layout(gl, CAR_FLOATS, 0);
                    gl.bind_vertex_array(0);
                    b
                };
                let body_instances = instances(&body);
                let wheel_instances = instances(&wheel);
                CarGpu {
                    kind: k,
                    body,
                    wheel,
                    wheels: m.wheels,
                    body_instances,
                    wheel_instances,
                    bodies: 0,
                    wheels_drawn: 0,
                }
            })
            .collect();
        let fullscreen = gpu::fullscreen_triangle(gl);
        let light_map = bake_light_map(gl, &world.lamps, quality, defines)?;
        let no_shadow = no_shadow_map(gl);
        let lamp_meshes = [lamps::Kind::Post, lamps::Kind::Double, lamps::Kind::Ceiling]
            .iter()
            .map(|&k| {
                let of_kind: Vec<&Lamp> = world.lamps.iter().filter(|l| l.kind == k).collect();
                let g = Geometry::new(gl, &lamps::mesh(k));
                let mut data = Vec::with_capacity(of_kind.len() * 8);
                for l in &of_kind {
                    data.extend_from_slice(&[
                        l.base.x, l.base.y, l.base.z, l.yaw, l.color.x, l.color.y, l.color.z, 1.0,
                    ]);
                }
                gl.bind_vertex_array(g.vao);
                let ib = gl.gen_buffer();
                gl.bind_buffer(gl::ARRAY_BUFFER, ib);
                gl.buffer_data(gl::ARRAY_BUFFER, &float_bytes(&data), gl::STATIC_DRAW);
                gpu::set_instance_layout(gl, 8, 0);
                gl.bind_vertex_array(0);
                (g, of_kind.len() as i32, k)
            })
            .collect();
        let tree_meshes = [scenery::TreeKind::Pine, scenery::TreeKind::Broad].map(|k| {
            let g = Geometry::new(gl, &scenery::tree_mesh(k));
            gl.bind_vertex_array(g.vao);
            let ib = gl.gen_buffer();
            gl.bind_buffer(gl::ARRAY_BUFFER, ib);
            gl.buffer_data_size(gl::ARRAY_BUFFER, 8 * 4, gl::STREAM_DRAW);
            gpu::set_instance_layout(gl, 8, 0);
            gl.bind_vertex_array(0);
            (g, ib, 0)
        });
        let (bridge_mesh, beacons, bridge_lights) = scenery::bridge(&world.road);
        let bridge = {
            let g = Geometry::new(gl, &bridge_mesh);
            gl.bind_vertex_array(g.vao);
            let ib = gl.gen_buffer();
            gl.bind_buffer(gl::ARRAY_BUFFER, ib);
            // Where it is (the mesh is in the world's coordinates) and its
            // paint.
            gl.buffer_data(
                gl::ARRAY_BUFFER,
                &float_bytes(&[0.0, 0.0, 0.0, 0.0, 0.62, 0.17, 0.07, 1.0]),
                gl::STATIC_DRAW,
            );
            gpu::set_instance_layout(gl, 8, 0);
            gl.bind_vertex_array(0);
            g
        };
        // The billboards and the neon signs: a static instance each.
        let instanced = |gl: &mut Context, mesh: &crate::mesh::Mesh, data: &[f32]| {
            let g = Geometry::new(gl, mesh);
            gl.bind_vertex_array(g.vao);
            let ib = gl.gen_buffer();
            gl.bind_buffer(gl::ARRAY_BUFFER, ib);
            gl.buffer_data(gl::ARRAY_BUFFER, &float_bytes(data), gl::STATIC_DRAW);
            gpu::set_instance_layout(gl, 8, 0);
            gl.bind_vertex_array(0);
            (g, (data.len() / 8) as i32)
        };
        let board_data: Vec<f32> = world
            .billboards
            .iter()
            .flat_map(|b| [b.base.x, b.base.y, b.base.z, b.yaw, f32::from(b.design), 0.0, 0.0, 1.0])
            .collect();
        let boards = instanced(gl, &scenery::board_frame_mesh(), &board_data);
        let faces = instanced(gl, &scenery::board_face_mesh(), &board_data);
        let neon_data: Vec<f32> = world
            .neons
            .iter()
            .flat_map(|n| [n.base.x, n.base.y, n.base.z, n.yaw, n.color.x, n.color.y, n.color.z, 1.0])
            .collect();
        let neons = instanced(gl, &scenery::neon_mesh(), &neon_data);
        let sign = lit(gl, "signs", shaders::SIGN_VS, shaders::SIGN_FS, &[], Some("u_signs"))?;
        let signs = {
            let imgs = texgen::billboards(if quality == Quality::High { 512 } else { 256 });
            array(gl, &imgs)
        };
        let (lamp_glows, always_glows) = {
            let (mut dark, mut always) = (Vec::new(), Vec::new());
            // The bridge's: red beacons on its towers (always), white along
            // its cables.
            for b in &beacons {
                always.extend_from_slice(&[b.x, b.y, b.z, 1.6, 1.6, 0.04, 0.02, 0.0]);
            }
            for l in &bridge_lights {
                dark.extend_from_slice(&[l.x, l.y, l.z, 0.9, 0.55, 0.5, 0.42, 0.0]);
            }
            for l in &world.lamps {
                let heads = if l.kind == lamps::Kind::Double { &l.heads[..] } else { &l.heads[..1] };
                for h in heads {
                    let size = if l.kind == lamps::Kind::Ceiling { 0.6 } else { 1.2 };
                    let c = l.color * 0.35;
                    let list = if l.kind.always_lit() { &mut always } else { &mut dark };
                    list.extend_from_slice(&[h.x, h.y - 0.2, h.z, size, c.x, c.y, c.z, 0.0]);
                }
            }
            let quads = |gl: &mut Context, data: &[f32]| (instanced_quads(gl, data).0, (data.len() / 8) as i32);
            (quads(gl, &dark), quads(gl, &always))
        };
        let car_glows = instanced_quads(gl, &[]);

        let post = if hdr { Some(post::Post::new(gl, fullscreen)?) } else { None };
        let shadow = if casts_shadows(&env, quality) { Some(Shadow::new(gl, defines)?) } else { None };
        let passing = PassingGpu::new(gl, defines, quality)?;
        let mut r = Renderer {
            quality,
            sizes,
            env,
            sky,
            terrain,
            tarmac,
            surface,
            water,
            building,
            car,
            prop,
            glow,
            frame_ubo,
            light_map,
            no_shadow,
            lamp_meshes,
            tree_meshes,
            trees: world.trees.clone(),
            bridge,
            boards,
            neons,
            faces,
            sign,
            signs,
            lamp_glows,
            always_glows,
            car_glows,
            // Painted by `bake_sky`.
            env_map: 0,
            asphalt,
            surfaces,
            ground_tex,
            ripples,
            walls,
            road,
            ground,
            sea,
            blocks,
            cars,
            fullscreen,
            dash: world.dash_period(),
            post,
            shadow,
            defines,
            passing,
            stats: Stats::default(),
        };
        r.bake_sky(gl, defines)?;
        let e = gl.get_error();
        if e != gl::NO_ERROR {
            return Err(format!("GL error {e:#x} while setting up"));
        }
        Ok(r)
    }

    /// The quality it draws at.
    pub fn quality(&self) -> Quality {
        self.quality
    }

    /// Lights the world by `env` from now on: the sky is painted again, and
    /// the shadow map made if the sun casts shadows for the first time.
    pub fn set_environment(&mut self, gl: &mut Context, env: Environment) -> Result<(), String> {
        self.env = env;
        if casts_shadows(&env, self.quality) && self.shadow.is_none() {
            self.shadow = Some(Shadow::new(gl, self.defines)?);
        }
        self.bake_sky(gl, self.defines)
    }

    /// The light the frame is lit by.
    pub fn environment(&self) -> &Environment {
        &self.env
    }

    /// Paints the sky of the environment into the cube map.
    fn bake_sky(&mut self, gl: &mut Context, defines: &str) -> Result<(), String> {
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
        let size = self.sizes.sky;
        // Storage once allocated is fixed: a new sky, a new texture.
        gl.delete_textures(&[self.env_map]);
        self.env_map = gl.gen_texture();
        gl.bind_texture(gl::TEXTURE_CUBE_MAP, self.env_map);
        // In half floats where the renderer draws into them, so that the
        // sun's reflection keeps its brightness.
        let format = if gl.backend().caps().color_buffer_float { gl::RGBA16F } else { gl::RGBA8 };
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
        let e = &self.env;
        let v3 = |gl: &mut Context, name: &str, v: Vec3| gl.uniform3f(p.at(name), v.x, v.y, v.z);
        v3(gl, "u_sun", e.sun_dir);
        v3(gl, "u_zenith", e.sky.zenith);
        v3(gl, "u_horizon", e.sky.horizon);
        v3(gl, "u_glow", e.sky.glow);
        v3(gl, "u_disc", e.sky.disc);
        v3(gl, "u_city", e.sky.city);
        let c = e.sky.clouds;
        gl.uniform4f(p.at("u_clouds"), c.x, c.y, c.z, e.sky.cloud_cover);
        gl.uniform1f(p.at("u_stars"), e.sky.stars);
        gl.bind_vertex_array(self.fullscreen);
        gl.viewport(0, 0, size, size);
        gl.disable(gl::DEPTH_TEST);
        gl.disable(gl::CULL_FACE);
        for (i, (f, r, u)) in faces.iter().enumerate() {
            gl.framebuffer_texture_2d(
                gl::FRAMEBUFFER,
                gl::COLOR_ATTACHMENT0,
                gl::TEXTURE_CUBE_MAP_POSITIVE_X + i as u32,
                self.env_map,
                0,
            );
            gl.uniform3f(p.at("u_forward"), f[0], f[1], f[2]);
            gl.uniform3f(p.at("u_right"), r[0], r[1], r[2]);
            gl.uniform3f(p.at("u_up"), u[0], u[1], u[2]);
            gl.draw_arrays(gl::TRIANGLES, 0, 3);
        }
        gl.bind_framebuffer(gl::FRAMEBUFFER, 0);
        gl.delete_framebuffers(&[fbo]);
        gl.delete_program(p.id);
        gl.bind_texture(gl::TEXTURE_CUBE_MAP, self.env_map);
        gl.generate_mipmap(gl::TEXTURE_CUBE_MAP);
        Ok(())
    }

    /// Fills the frame's uniform block.
    fn upload_frame(
        &self,
        gl: &mut Context,
        view_proj: &Mat4,
        sun: Option<&Mat4>,
        eye: Vec3,
        time: f32,
        lights: &[Light],
    ) {
        let e = &self.env;
        let mut f = Vec::with_capacity(FRAME_FLOATS);
        f.extend_from_slice(&mat(view_proj));
        f.extend_from_slice(&mat(sun.unwrap_or(&Mat4::IDENTITY)));
        let v4 = |f: &mut Vec<f32>, v: Vec3, w: f32| f.extend_from_slice(&[v.x, v.y, v.z, w]);
        v4(&mut f, eye, time);
        // No shadow map, no shadows.
        v4(&mut f, e.sun_dir, if sun.is_some() { e.shadows } else { 0.0 });
        v4(&mut f, e.sun_color, 0.0);
        v4(&mut f, e.sky_ambient, 0.0);
        v4(&mut f, e.ground_ambient, 0.0);
        v4(&mut f, e.fog_color, e.fog_density);
        f.extend_from_slice(&[e.fog_falloff, e.fog_glow, e.fog_max, 0.0]);
        f.extend_from_slice(&[e.wetness, e.rain, e.street_lights, e.exposure]);
        f.extend_from_slice(&[-HALF_X, -HALF_Z, 0.5 / HALF_X, 0.5 / HALF_Z]);
        // The lights that matter most here: bright and near.
        let mut chosen: Vec<&Light> = lights.iter().filter(|l| l.pos.distance(eye) < l.radius + 150.0).collect();
        let weight = |l: &Light| (l.color.x + l.color.y + l.color.z) / (1.0 + l.pos.distance_squared(eye));
        chosen.sort_by(|a, b| weight(b).total_cmp(&weight(a)));
        chosen.truncate(MAX_LIGHTS);
        let texel = self.shadow.as_ref().filter(|_| sun.is_some()).map_or(0.0, |sh| 1.0 / sh.size as f32);
        f.extend_from_slice(&[chosen.len() as f32, e.reflections, texel, 0.0]);
        for l in &chosen {
            f.extend_from_slice(&[l.pos.x, l.pos.y, l.pos.z, l.radius]);
            f.extend_from_slice(&[l.color.x, l.color.y, l.color.z, 0.0]);
            match l.spot {
                Some((d, cone)) => f.extend_from_slice(&[d.x, d.y, d.z, cone]),
                None => f.extend_from_slice(&[0.0, -1.0, 0.0, -2.0]),
            }
        }
        f.resize(FRAME_FLOATS, 0.0);
        gl.bind_buffer(gl::UNIFORM_BUFFER, self.frame_ubo);
        gl.buffer_sub_data(gl::UNIFORM_BUFFER, 0, &float_bytes(&f));
        gl.bind_buffer(gl::UNIFORM_BUFFER, 0);
        gl.bind_buffer_base(gl::UNIFORM_BUFFER, gpu::FRAME_BINDING, self.frame_ubo);
    }

    /// Draws `frame` into the bound framebuffer (`width` x `height`).
    pub fn render(&mut self, gl: &mut Context, frame: &Frame, width: i32, height: i32) {
        let camera = &frame.camera;
        let time = frame.time;
        let aspect = width as f32 / height.max(1) as f32;
        let proj = glam::camera::rh::proj::opengl::perspective(camera.fov, aspect, NEAR, FAR);
        let view_proj = proj * camera.view();
        let planes = frustum(&view_proj);
        // This frame's cars, by kind, and the trees in view.
        self.upload_cars(gl, frame.cars, camera.eye);
        self.upload_trees(gl, &planes, camera.eye);
        // The sun's shadow map first.
        let sun = (self.shadow.is_some() && casts_shadows(&self.env, self.quality)).then(|| self.sun_view_proj(camera));
        if let Some(sun) = sun {
            self.upload_frame(gl, &sun, None, camera.eye, time, &[]);
            self.draw_shadow_map(gl, &sun);
        }
        self.upload_frame(gl, &view_proj, sun.as_ref(), camera.eye, time, frame.lights);
        self.stats = Stats::default();

        match self.post.as_mut() {
            Some(p) => p.begin(gl, width, height),
            None => {
                gl.bind_framebuffer(gl::FRAMEBUFFER, 0);
                gl.viewport(0, 0, width, height);
            }
        }
        gl.enable(gl::DEPTH_TEST);
        gl.depth_func(gl::LESS);
        gl.depth_mask(true);
        gl.enable(gl::CULL_FACE);
        gl.cull_face(gl::BACK);
        gl.disable(gl::BLEND);
        gl.clear_color(0.0, 0.0, 0.0, 1.0);
        gl.clear(gl::DEPTH_BUFFER_BIT | gl::COLOR_BUFFER_BIT);
        gl.active_texture(gl::TEXTURE0 + gpu::unit::LIGHT_MAP);
        gl.bind_texture(gl::TEXTURE_2D, self.light_map);
        gl.active_texture(gl::TEXTURE0 + gpu::unit::SHADOW);
        let shadow_map = self.shadow.as_ref().filter(|_| sun.is_some()).map_or(self.no_shadow, |sh| sh.tex);
        gl.bind_texture(gl::TEXTURE_2D, shadow_map);
        gl.active_texture(gl::TEXTURE0);
        gl.bind_texture(gl::TEXTURE_CUBE_MAP, self.env_map);

        // The cars first: they hide the most.
        let car_program = self.car.id;
        self.draw_cars(gl, &car_program, true);

        // The road: the tarmac, then its sides.
        let visible: Vec<usize> =
            (0..self.road.len()).filter(|&i| !outside(&planes, self.road[i].center, self.road[i].radius)).collect();
        gl.use_program(self.tarmac.id);
        gl.uniform1f(self.tarmac.at("u_dash"), self.dash);
        gl.active_texture(gl::TEXTURE1);
        gl.bind_texture(gl::TEXTURE_2D, self.asphalt);
        for &i in &visible {
            self.road[i].tarmac.draw(gl);
            self.count(self.road[i].tarmac.count);
        }
        self.passing.draw_skids(gl, &frame.passing, &mut self.stats);
        gl.use_program(self.surface.id);
        gl.bind_texture(gl::TEXTURE_2D_ARRAY, self.surfaces);
        for &i in &visible {
            self.road[i].sides.draw(gl);
            self.count(self.road[i].sides.count);
        }

        // The buildings, all in one draw.
        gl.use_program(self.building.id);
        gl.bind_texture(gl::TEXTURE_2D_ARRAY, self.walls);
        self.blocks.0.draw_instanced(gl, self.blocks.1);
        self.stats.draws += 1;
        self.stats.triangles += (self.blocks.0.count / 3 * self.blocks.1) as u32;

        // The street lights, the trees, the bridge.
        gl.use_program(self.prop.id);
        for (g, n, kind) in &self.lamp_meshes {
            gl.uniform1f(self.prop.at("u_always"), if kind.always_lit() { 1.0 } else { 0.0 });
            g.draw_instanced(gl, *n);
            self.stats.draws += 1;
        }
        gl.uniform1f(self.prop.at("u_always"), 0.0);
        let (draws, triangles) = self.draw_props(gl);
        self.stats.draws += draws;
        self.stats.triangles += triangles;
        // The billboards' pictures.
        if self.faces.1 > 0 {
            gl.use_program(self.sign.id);
            gl.active_texture(gl::TEXTURE0 + gpu::unit::OWN);
            gl.bind_texture(gl::TEXTURE_2D_ARRAY, self.signs);
            self.faces.0.draw_instanced(gl, self.faces.1);
            self.stats.draws += 1;
        }

        // The ground, fine near the camera.
        gl.use_program(self.terrain.id);
        gl.bind_texture(gl::TEXTURE_2D_ARRAY, self.ground_tex);
        for k in 0..self.ground.len() {
            let c = &self.ground[k];
            if outside(&planes, c.center, c.radius) {
                continue;
            }
            let near = (c.center.distance(camera.eye) - c.radius).max(0.0);
            let (vao, count) = if near < self.sizes.fine_ground { c.fine } else { c.coarse };
            gl.bind_vertex_array(vao);
            gl.draw_elements(gl::TRIANGLES, count, gl::UNSIGNED_INT, 0);
            self.count(count);
        }

        // The sea.
        gl.use_program(self.water.id);
        gl.bind_texture(gl::TEXTURE_2D, self.ripples);
        self.sea.draw(gl);
        self.count(self.sea.count);

        // The sky, where nothing else is.
        gl.active_texture(gl::TEXTURE0);
        gl.depth_func(gl::LEQUAL);
        gl.depth_mask(false);
        gl.use_program(self.sky.id);
        gl.uniform_matrix4fv(
            self.sky.at("u_invViewProj"),
            false,
            &mat(&view_proj.try_inverse().unwrap_or(Mat4::IDENTITY)),
        );
        gl.uniform1f(self.sky.at("u_stars"), self.env.sky.stars);
        gl.bind_vertex_array(self.fullscreen);
        gl.draw_arrays(gl::TRIANGLES, 0, 3);

        // The glows, added on top (tested against what hides them): the
        // street lights' at night, the cars', the tunnel's and the bridge's
        // beacons' always.
        let car_glows = car_glows(frame.cars, camera.eye, 0.2 + 0.8 * self.env.street_lights.min(1.0));
        if self.env.street_lights > 0.0 || !car_glows.is_empty() || self.always_glows.1 > 0 {
            let view = camera.view();
            let (right, up) = (view.row(0).xyz(), view.row(1).xyz());
            gl.use_program(self.glow.id);
            gl.uniform3f(self.glow.at("u_right"), right.x, right.y, right.z);
            gl.uniform3f(self.glow.at("u_up"), up.x, up.y, up.z);
            gl.disable(gl::CULL_FACE);
            gl.enable(gl::BLEND);
            gl.blend_func(gl::ONE, gl::ONE);
            if self.env.street_lights > 0.0 {
                gl.bind_vertex_array(self.lamp_glows.0);
                gl.draw_arrays_instanced(gl::TRIANGLES, 0, 6, self.lamp_glows.1);
                self.stats.draws += 1;
            }
            if self.always_glows.1 > 0 {
                gl.bind_vertex_array(self.always_glows.0);
                gl.draw_arrays_instanced(gl::TRIANGLES, 0, 6, self.always_glows.1);
                self.stats.draws += 1;
            }
            if !car_glows.is_empty() {
                gl.bind_buffer(gl::ARRAY_BUFFER, self.car_glows.1);
                gl.buffer_data(gl::ARRAY_BUFFER, &float_bytes(&car_glows), gl::STREAM_DRAW);
                gl.bind_vertex_array(self.car_glows.0);
                gl.draw_arrays_instanced(gl::TRIANGLES, 0, 6, (car_glows.len() / 8) as i32);
                self.stats.draws += 1;
            }
            gl.disable(gl::BLEND);
            gl.enable(gl::CULL_FACE);
        }
        gl.depth_mask(true);
        gl.depth_func(gl::LESS);
        gl.bind_vertex_array(0);

        // Smoke, sparks and rain in the air.
        self.passing.draw_air(gl, &frame.passing, camera, &mut self.stats);

        if let Some(p) = self.post.as_mut() {
            p.finish(gl, &self.env.grade());
        }
    }

    /// The sun's view and projection: a square box round where the camera
    /// looks, seen along the sunlight, its middle moved by whole texels so
    /// that the shadows' edges stand still as the camera moves.
    fn sun_view_proj(&self, camera: &Camera) -> Mat4 {
        let size = self.shadow.as_ref().map_or(2048, |s| s.size) as f32;
        let fwd = (camera.target - camera.eye).normalize_or(Vec3::Z);
        let flat = Vec3::new(fwd.x, 0.0, fwd.z).normalize_or(Vec3::Z);
        let center = camera.eye + flat * (SHADOW_HALF * 0.6);
        let sun = self.env.sun_dir;
        let up = if sun.y.abs() > 0.95 { Vec3::Z } else { Vec3::Y };
        let view = glam::camera::rh::view::look_at_mat4(Vec3::ZERO, -sun, up);
        let c = view.transform_point3(center);
        let texel = 2.0 * SHADOW_HALF / size;
        let (cx, cy) = ((c.x / texel).round() * texel, (c.y / texel).round() * texel);
        // Towards the sun far enough for the towers, beyond for the ground.
        let proj = glam::camera::rh::proj::opengl::orthographic(
            cx - SHADOW_HALF,
            cx + SHADOW_HALF,
            cy - SHADOW_HALF,
            cy + SHADOW_HALF,
            -c.z - 700.0,
            -c.z + 150.0,
        );
        proj * view
    }

    /// Draws what casts shadows into the shadow map, seen from the sun.
    fn draw_shadow_map(&mut self, gl: &mut Context, sun: &Mat4) {
        let Some(sh) = self.shadow.as_ref() else { return };
        let planes = frustum(sun);
        gl.bind_framebuffer(gl::FRAMEBUFFER, sh.fbo);
        gl.viewport(0, 0, sh.size, sh.size);
        gl.enable(gl::DEPTH_TEST);
        gl.depth_func(gl::LESS);
        gl.depth_mask(true);
        gl.clear(gl::DEPTH_BUFFER_BIT);
        // Open meshes (pavements, rails) cast from both sides.
        gl.disable(gl::CULL_FACE);
        gl.enable(gl::POLYGON_OFFSET_FILL);
        gl.polygon_offset(1.6, 3.0);
        gl.use_program(sh.statik.id);
        let mut draws = 0;
        for c in &self.road {
            if !outside(&planes, c.center, c.radius) {
                c.sides.draw(gl);
                draws += 1;
            }
        }
        gl.use_program(sh.building.id);
        self.blocks.0.draw_instanced(gl, self.blocks.1);
        gl.use_program(sh.prop.id);
        for (g, n, _) in &self.lamp_meshes {
            g.draw_instanced(gl, *n);
        }
        let car_program = sh.car.id;
        draws += self.draw_props(gl).0;
        draws += self.draw_cars(gl, &car_program, false);
        gl.disable(gl::POLYGON_OFFSET_FILL);
        gl.enable(gl::CULL_FACE);
        self.stats.draws += draws + 2 + self.lamp_meshes.len() as u32;
    }

    /// Fills each kind of car's instance buffers with this frame's cars
    /// near the camera: bodies, and wheels (the right-hand ones turned
    /// round so that their faces look out; the front ones steered).
    fn upload_cars(&mut self, gl: &mut Context, cars: &[CarView], eye: Vec3) {
        let mut bodies: Vec<Vec<f32>> = self.cars.iter().map(|_| Vec::new()).collect();
        let mut wheels: Vec<Vec<f32>> = self.cars.iter().map(|_| Vec::new()).collect();
        let push = |out: &mut Vec<f32>, m: &Mat4, paint: [f32; 4], lamps: [f32; 4]| {
            for r in 0..3 {
                let row = m.row(r);
                out.extend_from_slice(&[row.x, row.y, row.z, row.w]);
            }
            out.extend_from_slice(&paint);
            out.extend_from_slice(&lamps);
        };
        for c in cars {
            // Beyond the fog and the shadows' reach, not worth drawing.
            if c.transform.w_axis.xyz().distance(eye) > 900.0 {
                continue;
            }
            let Some(k) = self.cars.iter().position(|e| e.kind == c.kind) else { continue };
            let livery = match c.livery {
                Livery::Plain => 0.0,
                Livery::Police => 1.0,
                Livery::Taxi => 2.0,
            };
            let paint = [c.paint.x, c.paint.y, c.paint.z, livery];
            let lamps = [c.headlights, c.brake, c.bar[0], c.bar[1]];
            push(&mut bodies[k], &c.transform, paint, lamps);
            for (w, &at) in self.cars[k].wheels.iter().enumerate() {
                let steer = if w < 2 { c.steer } else { 0.0 };
                let mut m = c.transform * Mat4::from_translation(at) * Mat4::from_rotation_y(steer);
                if at.x < 0.0 {
                    m *= Mat4::from_rotation_y(core::f32::consts::PI) * Mat4::from_rotation_x(-c.spin);
                } else {
                    m *= Mat4::from_rotation_x(c.spin);
                }
                push(&mut wheels[k], &m, [0.0; 4], [0.0; 4]);
            }
        }
        for (k, g) in self.cars.iter_mut().enumerate() {
            g.bodies = (bodies[k].len() / CAR_FLOATS) as i32;
            g.wheels_drawn = (wheels[k].len() / CAR_FLOATS) as i32;
            if g.bodies > 0 {
                gl.bind_buffer(gl::ARRAY_BUFFER, g.body_instances);
                gl.buffer_data(gl::ARRAY_BUFFER, &float_bytes(&bodies[k]), gl::STREAM_DRAW);
                gl.bind_buffer(gl::ARRAY_BUFFER, g.wheel_instances);
                gl.buffer_data(gl::ARRAY_BUFFER, &float_bytes(&wheels[k]), gl::STREAM_DRAW);
            }
        }
    }

    /// Draws this frame's cars with `program` (the cars' own, or the
    /// shadow map's): each kind's bodies, then its wheels. Returns the
    /// draws.
    fn draw_cars(&mut self, gl: &mut Context, program: &u32, lit: bool) -> u32 {
        gl.use_program(*program);
        if lit {
            gl.active_texture(gl::TEXTURE0);
            gl.bind_texture(gl::TEXTURE_CUBE_MAP, self.env_map);
        }
        let at_shape = if lit { self.car.at("u_shape") } else { -1 };
        let mut draws = 0;
        for g in &self.cars {
            if g.bodies == 0 {
                continue;
            }
            let shape = g.kind.shape();
            gl.uniform4f(at_shape, shape.cowl, shape.a, shape.c, shape.belt);
            g.body.draw_instanced(gl, g.bodies);
            g.wheel.draw_instanced(gl, g.wheels_drawn);
            draws += 2;
            self.stats.triangles += (g.body.count / 3 * g.bodies + g.wheel.count / 3 * g.wheels_drawn) as u32;
        }
        self.stats.draws += draws;
        draws
    }

    /// Fills the trees' instance buffers with those in view and near
    /// enough to be seen.
    fn upload_trees(&mut self, gl: &mut Context, planes: &[Vec4; 6], eye: Vec3) {
        let far = if self.quality == Quality::High { 900.0 } else { 320.0 };
        let mut data = [Vec::new(), Vec::new()];
        for t in &self.trees {
            let top = t.base + Vec3::Y * (5.0 * t.scale);
            if top.distance_squared(eye) > far * far || outside(planes, top, 7.0 * t.scale) {
                continue;
            }
            let k = usize::from(t.kind == scenery::TreeKind::Broad);
            data[k].extend_from_slice(&[t.base.x, t.base.y, t.base.z, t.yaw, t.tint.x, t.tint.y, t.tint.z, t.scale]);
        }
        for (k, d) in data.iter().enumerate() {
            let (_, buf, n) = &mut self.tree_meshes[k];
            *n = (d.len() / 8) as i32;
            if *n > 0 {
                gl.bind_buffer(gl::ARRAY_BUFFER, *buf);
                gl.buffer_data(gl::ARRAY_BUFFER, &float_bytes(d), gl::STREAM_DRAW);
            }
        }
        gl.bind_buffer(gl::ARRAY_BUFFER, 0);
    }

    /// Draws the trees and the bridge with the program bound (lit, or into
    /// the shadow map); returns the draws and the triangles.
    fn draw_props(&self, gl: &mut Context) -> (u32, u32) {
        let (mut draws, mut triangles) = (1, (self.bridge.count / 3) as u32);
        for (g, _, n) in &self.tree_meshes {
            if *n > 0 {
                g.draw_instanced(gl, *n);
                triangles += (g.count / 3 * *n) as u32;
                draws += 1;
            }
        }
        self.bridge.draw_instanced(gl, 1);
        for (g, n) in [&self.boards, &self.neons] {
            if *n > 0 {
                g.draw_instanced(gl, *n);
                triangles += (g.count / 3 * *n) as u32;
                draws += 1;
            }
        }
        (draws, triangles)
    }

    fn count(&mut self, indices: i32) {
        self.stats.draws += 1;
        self.stats.triangles += indices as u32 / 3;
    }
}

/// The glows of the cars' lights: headlights seen from ahead, tail lights
/// from behind (brighter when braking), the light bar's flashes from
/// anywhere. Eight floats each: place and size, colour.
fn car_glows(cars: &[CarView], eye: Vec3, dark: f32) -> Vec<f32> {
    let mut out = Vec::new();
    for c in cars {
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
    out
}

/// A vertex array drawing a quad (two triangles, corners at -1 and 1)
/// once for each instance in `data` (eight floats each: two vec4s), and
/// the instance buffer.
fn instanced_quads(gl: &mut Context, data: &[f32]) -> (u32, u32) {
    let vao = gl.gen_vertex_array();
    gl.bind_vertex_array(vao);
    let vbo = gl.gen_buffer();
    gl.bind_buffer(gl::ARRAY_BUFFER, vbo);
    let corners = [-1.0, -1.0, 1.0, -1.0, 1.0, 1.0, -1.0, -1.0, 1.0, 1.0, -1.0, 1.0];
    gl.buffer_data(gl::ARRAY_BUFFER, &float_bytes(&corners), gl::STATIC_DRAW);
    gl.vertex_attrib_pointer(gpu::attr::POS, 2, gl::FLOAT, false, 0, 0);
    gl.enable_vertex_attrib_array(gpu::attr::POS);
    let ib = gl.gen_buffer();
    gl.bind_buffer(gl::ARRAY_BUFFER, ib);
    gl.buffer_data(gl::ARRAY_BUFFER, &float_bytes(data), gl::STATIC_DRAW);
    gpu::set_instance_layout(gl, 8, 0);
    gl.bind_vertex_array(0);
    (vao, ib)
}

/// Paints the street lights' pools into a map of the world, seen from
/// above: their light added up in colour, the highest lamp's height in
/// alpha.
fn bake_light_map(gl: &mut Context, lamps: &[Lamp], quality: Quality, defines: &str) -> Result<u32, String> {
    let size = if quality == Quality::High { 2048 } else { 1024 };
    let tex = gl.gen_texture();
    gl.bind_texture(gl::TEXTURE_2D, tex);
    let float = gl.backend().caps().color_buffer_float;
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
    let (vao, buffer) = instanced_quads(gl, &data);
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
    gl.bind_vertex_array(vao);
    gl.draw_arrays_instanced(gl::TRIANGLES, 0, 6, (data.len() / 8) as i32);
    gl.blend_equation(gl::FUNC_ADD);
    gl.disable(gl::BLEND);
    gl.bind_vertex_array(0);
    gl.bind_framebuffer(gl::FRAMEBUFFER, 0);
    gl.delete_framebuffers(&[fbo]);
    gl.delete_vertex_arrays(&[vao]);
    gl.delete_buffers(&[buffer]);
    gl.delete_program(p.id);
    Ok(tex)
}

/// A depth texture of one texel at the far plane, compared as shadow maps
/// are: where there is no shadow map, nothing is in shadow.
fn no_shadow_map(gl: &mut Context) -> u32 {
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

/// The unit box the blocks are drawn with (no floor), with an instance
/// buffer of the blocks: where, how turned, how big, what style, what
/// colour, what seed.
fn upload_blocks(gl: &mut Context, blocks: &[Block]) -> (Geometry, i32) {
    let mut cube = crate::mesh::cuboid(Vec3::new(-0.5, 0.0, -0.5), Vec3::new(0.5, 1.0, 0.5), [0; 4]);
    // Drop the floor.
    let keep: Vec<u32> =
        cube.indices.chunks(3).filter(|t| cube.vertices[t[0] as usize].normal.y > -0.5).flatten().copied().collect();
    cube.indices = keep;
    let geometry = Geometry::new(gl, &cube);
    let mut data = Vec::with_capacity(blocks.len() * 12);
    for b in blocks {
        data.extend_from_slice(&[b.base.x, b.base.y, b.base.z, b.yaw]);
        data.extend_from_slice(&[b.size.x, b.size.y, b.size.z, f32::from(b.style)]);
        data.extend_from_slice(&[b.tint.x, b.tint.y, b.tint.z, b.seed]);
    }
    gl.bind_vertex_array(geometry.vao);
    let ib = gl.gen_buffer();
    gl.bind_buffer(gl::ARRAY_BUFFER, ib);
    gl.buffer_data(gl::ARRAY_BUFFER, &float_bytes(&data), gl::STATIC_DRAW);
    gpu::set_instance_layout(gl, 12, 0);
    gl.bind_vertex_array(0);
    (geometry, blocks.len() as i32)
}

/// The tarmac's vertices with the third parameter set where the road is
/// divided (its program paints edge lines along the barrier there instead
/// of the double centre line).
fn divided_flags(m: &crate::mesh::Mesh, world: &World) -> crate::mesh::Mesh {
    let mut m = m.clone();
    for v in &mut m.vertices {
        let s = v.uv.y;
        if world.road.divided(s) {
            v.params[2] = 255;
        }
    }
    m
}
