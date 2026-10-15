// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The pictures, with OpenGL ES 3.0.
//!
//! At start-up the renderer uploads Veda Bay, paints its textures,
//! compiles the programs, paints the street lights' pools into a map of
//! the world and the sky into a cube map, which the background and every
//! reflection sample. [`Renderer::render`] draws a frame: it fills the
//! frame's uniform block, draws the sun's shadow map, then what the camera
//! can see, front to back (the cars, the road, what stands by it, the
//! ground, the sea), the sky where nothing else is, the lights' glows and
//! what passes in the air, and post-processes the picture.
//!
//! | Module | Contents |
//! |--------|----------|
//! | `world` | Veda Bay on the GPU: the road, the ground, the sea, the buildings, the street lights, the trees, the bridge, the signs |
//! | `instances` | instances on the GPU; the cars and their lights' glows, refilled every frame |
//! | `sky` | the sky, painted for the time of day, and the background |
//! | `shadow` | the sun's shadow map |
//! | `lightmap` | the street lights' pools, painted once into a map of the world |
//! | `passing` | skid marks, smoke, spray, sparks and rain |
//! | [`post`] | bloom, tone mapping and the grade |
//! | [`shaders`], [`gpu`] | the shaders, and helpers over OpenGL ES |

pub mod gpu;
mod instances;
mod lightmap;
mod passing;
pub mod post;
pub mod shaders;
mod shadow;
mod sky;
mod world;

pub use passing::Passing;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use vgl::{Context, gl};

use crate::carmodel::Kind;
use crate::env::Environment;
use crate::prelude::*;
use crate::terrain::{HALF_X, HALF_Z};
use crate::world::World;
use gpu::{Program, mat};
use instances::{Cars, Glows};
use passing::PassingGpu;
use shaders::{MAX_LIGHTS, source};
use shadow::Shadow;
use sky::Sky;
use world::WorldGpu;

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

/// How often the pictures' time wraps round (s): a whole number of turns
/// of everything it moves (the sea's ripples, the bridge's beacons, the
/// rain falling through its box), so that the wrap is never seen; skid
/// marks age across it.
pub const TIME_WRAP: f64 = 17_000.0;

/// The pictures' time at the game's clock `clock` (s): wrapped every
/// [`TIME_WRAP`] seconds, so that a float keeps its precision however long
/// the game runs.
pub fn shader_time(clock: f64) -> f32 {
    (clock % TIME_WRAP) as f32
}

/// What a frame shows.
pub struct Frame<'a> {
    pub camera: Camera,
    pub cars: &'a [CarView],
    /// The lights that move; the strongest near the camera are used.
    pub lights: &'a [Light],
    /// Seconds, for what moves by itself (the sea, the beacons, the rain)
    /// and what ages (skid marks): the game's clock as [`shader_time`]
    /// wraps it.
    pub time: f32,
    /// Smoke, sparks, skid marks and rain.
    pub passing: Passing<'a>,
}

/// The nearest and farthest the camera sees (m).
pub const NEAR: f32 = 0.35;
pub const FAR: f32 = 7000.0;

/// The planes of a view's frustum, each `(normal, d)` with a normal of
/// unit length, inside where `normal . p + d >= 0`.
struct Frustum([Vec4; 6]);

impl Frustum {
    /// The frustum of a view-projection matrix (OpenGL's clip space).
    fn new(m: &Mat4) -> Frustum {
        let r = [m.row(0), m.row(1), m.row(2), m.row(3)];
        let planes = [r[3] + r[0], r[3] - r[0], r[3] + r[1], r[3] - r[1], r[3] + r[2], r[3] - r[2]];
        Frustum(planes.map(|p| p / p.xyz().length().max(1e-12)))
    }

    /// Whether a sphere is wholly outside it.
    fn outside(&self, center: Vec3, radius: f32) -> bool {
        self.0.iter().any(|p| p.xyz().dot(center) + p.w < -radius)
    }
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

/// A lit program, compiled with `defines`: `vs` and `fs` after the
/// prelude, with `uniforms`; every lit program samples the sky, the street
/// lights' map and the shadow map at the shared units, and its own texture
/// `own` (if any) at the next.
fn lit_program(
    gl: &mut Context,
    defines: &str,
    name: &str,
    vs: &str,
    fs: &str,
    uniforms: &[&'static str],
    own: Option<&'static str>,
) -> Result<Program, String> {
    let mut samplers = gpu::LIT_SAMPLERS.to_vec();
    if let Some(t) = own {
        samplers.push((t, gpu::unit::OWN));
    }
    Program::new(gl, name, &source(defines, true, vs), &source(defines, true, fs), uniforms, &samplers)
}

/// What the last frame drew.
#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    pub draws: u32,
    pub triangles: u32,
}

impl Stats {
    /// A draw of the triangles of `indices` indices.
    fn count(&mut self, indices: i32) {
        self.draws += 1;
        self.triangles += indices as u32 / 3;
    }

    /// A draw of `instances` copies of the triangles of `indices` indices.
    fn count_instanced(&mut self, indices: i32, instances: i32) {
        self.draws += 1;
        self.triangles += (indices / 3 * instances) as u32;
    }
}

/// The floats of the frame's uniform block (std140: matrices, then
/// vectors of four).
const FRAME_FLOATS: usize = 16 * 2 + 4 * 10 + 4 * 3 * MAX_LIGHTS;

/// The renderer and everything it keeps on the GPU.
pub struct Renderer {
    quality: Quality,
    env: Environment,
    /// What the programs are compiled with.
    defines: &'static str,
    frame_ubo: u32,
    /// The frame's uniforms, and the lights that matter most in it (each
    /// frame's working).
    uniforms: Vec<f32>,
    uniform_bytes: Vec<u8>,
    chosen: Vec<Light>,
    /// The street lights' pools, seen from above, and a stand-in for the
    /// sun's shadows where there are none.
    light_map: u32,
    no_shadow: u32,
    world: WorldGpu,
    sky: Sky,
    /// The cars, their program, and their lights' glows (with the program
    /// of every glow).
    cars: Cars,
    car: Program,
    glows: Glows,
    glow: Program,
    /// The sun's shadows, with a GPU, by day (made when first needed).
    shadow: Option<Shadow>,
    /// Post-processing, with a GPU.
    post: Option<post::Post>,
    /// Skid marks, particles and rain.
    passing: PassingGpu,
    /// Statistics of the last frame.
    pub stats: Stats,
}

impl Renderer {
    /// Uploads `world` and prepares to draw it lit by `env`.
    pub fn new(gl: &mut Context, world: &World, env: Environment, quality: Quality) -> Result<Renderer, String> {
        let sizes = Sizes::of(quality);
        // A GPU draws in high dynamic range and tone maps afterwards; a
        // software renderer (or a GPU that cannot draw in floats) tone
        // maps in each program.
        let hdr = quality == Quality::High && gl.caps().color_buffer_float;
        let defines = match (quality, hdr) {
            (Quality::Low, _) => "#define TONEMAP\n#define LOW\n",
            (Quality::High, false) => "#define TONEMAP\n",
            (Quality::High, true) => "",
        };
        let car = lit_program(gl, defines, "car", shaders::CAR_VS, shaders::CAR_FS, &["u_shape"], None)?;
        let glow = lit_program(gl, defines, "glow", shaders::GLOW_VS, shaders::GLOW_FS, &["u_right", "u_up"], None)?;
        let frame_ubo = gl.gen_buffer();
        gl.bind_buffer(gl::UNIFORM_BUFFER, frame_ubo);
        gl.buffer_data_size(gl::UNIFORM_BUFFER, FRAME_FLOATS * 4, gl::DYNAMIC_DRAW);
        gl.bind_buffer(gl::UNIFORM_BUFFER, 0);
        let world_gpu = WorldGpu::new(gl, world, &sizes, quality, defines)?;
        let cars = Cars::new(gl);
        let glows = Glows::new(gl);
        let fullscreen = gpu::fullscreen_triangle(gl);
        let light_map = lightmap::bake(gl, &world.lamps, quality, defines)?;
        let no_shadow = shadow::none(gl);
        let post = if hdr { Some(post::Post::new(gl, fullscreen)?) } else { None };
        let shadow = if shadow::casts(&env, quality) { Some(Shadow::new(gl, defines)?) } else { None };
        let passing = PassingGpu::new(gl, defines, quality)?;
        let sky = Sky::new(gl, defines, &env, sizes.sky, fullscreen)?;
        let e = gl.get_error();
        if e != gl::NO_ERROR {
            return Err(format!("GL error {e:#x} while setting up"));
        }
        Ok(Renderer {
            quality,
            env,
            defines,
            frame_ubo,
            uniforms: Vec::with_capacity(FRAME_FLOATS),
            uniform_bytes: Vec::with_capacity(FRAME_FLOATS * 4),
            chosen: Vec::new(),
            light_map,
            no_shadow,
            world: world_gpu,
            sky,
            cars,
            car,
            glows,
            glow,
            shadow,
            post,
            passing,
            stats: Stats::default(),
        })
    }

    /// Lights the world by `env` from now on: the sky is painted again, and
    /// the shadow map made if the sun casts shadows for the first time.
    pub fn set_environment(&mut self, gl: &mut Context, env: Environment) -> Result<(), String> {
        self.env = env;
        if shadow::casts(&env, self.quality) && self.shadow.is_none() {
            self.shadow = Some(Shadow::new(gl, self.defines)?);
        }
        self.sky.paint(gl, self.defines, &env)
    }

    /// Fills the frame's uniform block: seen through `view_proj` from
    /// `eye`, the sun's shadows through `sun` (none without), at `time`,
    /// lit by those of `lights` that matter most here.
    fn upload_frame(
        &mut self,
        gl: &mut Context,
        view_proj: &Mat4,
        sun: Option<&Mat4>,
        eye: Vec3,
        time: f32,
        lights: &[Light],
    ) {
        let e = &self.env;
        let f = &mut self.uniforms;
        f.clear();
        f.extend_from_slice(&mat(view_proj));
        f.extend_from_slice(&mat(sun.unwrap_or(&Mat4::IDENTITY)));
        let v4 = |f: &mut Vec<f32>, v: Vec3, w: f32| f.extend_from_slice(&[v.x, v.y, v.z, w]);
        v4(f, eye, time);
        // No shadow map, no shadows.
        v4(f, e.sun_dir, if sun.is_some() { e.shadows } else { 0.0 });
        v4(f, e.sun_color, 0.0);
        v4(f, e.sky_ambient, 0.0);
        v4(f, e.ground_ambient, 0.0);
        v4(f, e.fog_color, e.fog_density);
        f.extend_from_slice(&[e.fog_falloff, e.fog_glow, e.fog_max, 0.0]);
        f.extend_from_slice(&[e.wetness, e.rain, e.street_lights, e.exposure]);
        f.extend_from_slice(&[-HALF_X, -HALF_Z, 0.5 / HALF_X, 0.5 / HALF_Z]);
        // The lights that matter most here: bright and near.
        let chosen = &mut self.chosen;
        chosen.clear();
        chosen.extend(lights.iter().filter(|l| l.pos.distance(eye) < l.radius + 150.0));
        let weight = |l: &Light| (l.color.x + l.color.y + l.color.z) / (1.0 + l.pos.distance_squared(eye));
        chosen.sort_by(|a, b| weight(b).total_cmp(&weight(a)));
        chosen.truncate(MAX_LIGHTS);
        let texel = if sun.is_some() { Shadow::texel() } else { 0.0 };
        f.extend_from_slice(&[chosen.len() as f32, e.reflections, texel, 0.0]);
        for l in chosen.iter() {
            f.extend_from_slice(&[l.pos.x, l.pos.y, l.pos.z, l.radius]);
            f.extend_from_slice(&[l.color.x, l.color.y, l.color.z, 0.0]);
            match l.spot {
                Some((d, cone)) => f.extend_from_slice(&[d.x, d.y, d.z, cone]),
                None => f.extend_from_slice(&[0.0, -1.0, 0.0, -2.0]),
            }
        }
        f.resize(FRAME_FLOATS, 0.0);
        gpu::put_floats(&mut self.uniform_bytes, f);
        gl.bind_buffer(gl::UNIFORM_BUFFER, self.frame_ubo);
        gl.buffer_sub_data(gl::UNIFORM_BUFFER, 0, &self.uniform_bytes);
        gl.bind_buffer(gl::UNIFORM_BUFFER, 0);
        gl.bind_buffer_base(gl::UNIFORM_BUFFER, gpu::FRAME_BINDING, self.frame_ubo);
    }

    /// Draws `frame` into the bound framebuffer (`width` x `height`).
    pub fn render(&mut self, gl: &mut Context, frame: &Frame, width: i32, height: i32) {
        self.stats = Stats::default();
        let camera = &frame.camera;
        let aspect = width as f32 / height.max(1) as f32;
        let proj = glam::camera::rh::proj::opengl::perspective(camera.fov, aspect, NEAR, FAR);
        let view_proj = proj * camera.view();
        let planes = Frustum::new(&view_proj);
        // This frame's cars, by kind, and what of the world is in view.
        self.cars.fill(gl, frame.cars, camera.eye);
        self.world.cull(gl, &planes, camera.eye);
        // The sun's shadow map first.
        let sun = (self.shadow.is_some() && shadow::casts(&self.env, self.quality))
            .then(|| Shadow::view_proj(camera, self.env.sun_dir));
        if let Some(sun) = &sun {
            self.upload_frame(gl, sun, None, camera.eye, frame.time, &[]);
            if let Some(sh) = &self.shadow {
                sh.draw(gl, sun, &self.world, &self.cars, &mut self.stats);
            }
        }
        self.upload_frame(gl, &view_proj, sun.as_ref(), camera.eye, frame.time, frame.lights);

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
        gl.active_texture(gl::TEXTURE0 + gpu::unit::ENV);
        gl.bind_texture(gl::TEXTURE_CUBE_MAP, self.sky.map);

        // The cars first: they hide the most.
        gl.use_program(self.car.id);
        self.cars.draw(gl, self.car.at("u_shape"), &mut self.stats);

        // The road: the tarmac, the skid marks on it, then the rest.
        self.world.draw_tarmac(gl, &mut self.stats);
        self.passing.draw_skids(gl, &frame.passing, &mut self.stats);
        self.world.draw_rest(gl, &planes, camera.eye, &mut self.stats);

        // The sky, where nothing else is.
        self.sky.draw(gl, &view_proj, self.env.sky.stars, &mut self.stats);

        // The glows, added on top (tested against what hides them): the
        // street lights' at night, the cars', the tunnel's and the bridge's
        // beacons' always.
        let dark = 0.2 + 0.8 * self.env.street_lights.min(1.0);
        let cars_glow = self.glows.fill(gl, frame.cars, camera.eye, dark);
        let lamps_glow = self.env.street_lights > 0.0;
        if lamps_glow || cars_glow || self.world.glows_always() {
            let view = camera.view();
            let (right, up) = (view.row(0).xyz(), view.row(1).xyz());
            gl.use_program(self.glow.id);
            gl.uniform3f(self.glow.at("u_right"), right.x, right.y, right.z);
            gl.uniform3f(self.glow.at("u_up"), up.x, up.y, up.z);
            gl.disable(gl::CULL_FACE);
            gl.enable(gl::BLEND);
            gl.blend_func(gl::ONE, gl::ONE);
            self.world.draw_glows(gl, lamps_glow, &mut self.stats);
            self.glows.draw(gl, &mut self.stats);
            gl.disable(gl::BLEND);
            gl.enable(gl::CULL_FACE);
        }
        gl.depth_mask(true);
        gl.depth_func(gl::LESS);
        gl.bind_vertex_array(0);

        // Smoke, sparks and rain in the air.
        self.passing.draw_air(gl, &frame.passing, camera, &mut self.stats);

        if let Some(p) = self.post.as_mut() {
            p.finish(gl, &post::Grade::of(&self.env));
        }
    }
}
