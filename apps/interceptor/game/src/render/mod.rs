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
pub mod shaders;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::prelude::*;
use vgl::{Context, gl};

use crate::carmodel::{self, Kind};
use crate::city::Block;
use crate::env::Environment;
use crate::groundmesh;
use crate::roadmesh;
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
        Mat4::look_at_rh(self.eye, self.target, self.up)
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
    /// The light bar's clock (s), while it flashes.
    pub siren: Option<f32>,
}

/// What a frame shows.
pub struct Frame<'a> {
    pub camera: Camera,
    pub cars: &'a [CarView],
    /// Seconds, for what moves by itself (the sea, the light bars).
    pub time: f32,
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
    frame_ubo: u32,
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
    /// Each kind of car's body and wheel.
    cars: Vec<(Kind, Geometry, Geometry, [Vec3; 4])>,
    fullscreen: u32,
    dash: f32,
    /// Statistics of the last frame.
    pub stats: Stats,
}

/// What the last frame drew.
#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    pub draws: u32,
    pub triangles: u32,
}

/// The floats of the frame's uniform block (std140: matrices, then
/// vectors of four).
const FRAME_FLOATS: usize = 16 * 2 + 4 * 10 + 4 * 3 * MAX_LIGHTS;

impl Renderer {
    /// Uploads `world` and prepares to draw it lit by `env`.
    pub fn new(gl: &mut Context, world: &World, env: Environment, quality: Quality) -> Result<Renderer, String> {
        let sizes = Sizes::of(quality);
        let defines = match quality {
            Quality::Low => "#define TONEMAP\n#define LOW\n",
            Quality::High => "#define TONEMAP\n",
        };
        let lit =
            |gl: &mut Context, name: &str, vs: &str, fs: &str, uniforms: &[&'static str], samplers: &[&'static str]| {
                Program::new(gl, name, &source(defines, true, vs), &source(defines, true, fs), uniforms, samplers)
            };
        let sky = lit(gl, "sky", shaders::SKY_VS, shaders::SKY_FS, &["u_invViewProj"], &["u_env"])?;
        let terrain = lit(gl, "terrain", shaders::STATIC_VS, shaders::TERRAIN_FS, &[], &["u_env", "u_ground"])?;
        let tarmac = lit(gl, "tarmac", shaders::STATIC_VS, shaders::TARMAC_FS, &["u_dash"], &["u_env", "u_asphalt"])?;
        let surface = lit(gl, "surface", shaders::STATIC_VS, shaders::SURFACE_FS, &[], &["u_env", "u_surfaces"])?;
        let water = lit(gl, "water", shaders::STATIC_VS, shaders::WATER_FS, &[], &["u_env", "u_ripples"])?;
        let building = lit(gl, "building", shaders::BUILDING_VS, shaders::BUILDING_FS, &[], &["u_env", "u_walls"])?;
        let car = lit(
            gl,
            "car",
            shaders::CAR_VS,
            shaders::CAR_FS,
            &["u_model", "u_paint", "u_livery", "u_lamps", "u_shape"],
            &["u_env"],
        )?;

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
                (k, Geometry::new(gl, &m.body), Geometry::new(gl, &m.wheel), m.wheels)
            })
            .collect();
        let fullscreen = gpu::fullscreen_triangle(gl);

        let env_map = gl.gen_texture();
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
            frame_ubo,
            env_map,
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
    fn upload_frame(&self, gl: &mut Context, view_proj: &Mat4, eye: Vec3, time: f32) {
        let e = &self.env;
        let mut f = Vec::with_capacity(FRAME_FLOATS);
        f.extend_from_slice(&mat(view_proj));
        f.extend_from_slice(&mat(&Mat4::IDENTITY));
        let v4 = |f: &mut Vec<f32>, v: Vec3, w: f32| f.extend_from_slice(&[v.x, v.y, v.z, w]);
        v4(&mut f, eye, time);
        v4(&mut f, e.sun_dir, e.shadows);
        v4(&mut f, e.sun_color, 0.0);
        v4(&mut f, e.sky_ambient, 0.0);
        v4(&mut f, e.ground_ambient, 0.0);
        v4(&mut f, e.fog_color, e.fog_density);
        f.extend_from_slice(&[e.fog_falloff, e.fog_glow, e.fog_max, 0.0]);
        f.extend_from_slice(&[e.wetness, e.rain, e.street_lights, e.exposure]);
        f.extend_from_slice(&[0.0, 0.0, 0.0, 0.0]);
        f.extend_from_slice(&[0.0, e.reflections, 0.0, 0.0]);
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
        let proj = Mat4::perspective_rh(camera.fov, aspect, NEAR, FAR);
        let view_proj = proj * camera.view();
        let planes = frustum(&view_proj);
        self.upload_frame(gl, &view_proj, camera.eye, time);
        self.stats = Stats::default();

        gl.viewport(0, 0, width, height);
        gl.enable(gl::DEPTH_TEST);
        gl.depth_func(gl::LESS);
        gl.depth_mask(true);
        gl.enable(gl::CULL_FACE);
        gl.cull_face(gl::BACK);
        gl.disable(gl::BLEND);
        gl.clear_color(0.0, 0.0, 0.0, 1.0);
        gl.clear(gl::DEPTH_BUFFER_BIT | gl::COLOR_BUFFER_BIT);
        gl.active_texture(gl::TEXTURE0);
        gl.bind_texture(gl::TEXTURE_CUBE_MAP, self.env_map);

        // The cars first: they hide the most.
        self.draw_cars(gl, frame.cars, &planes);

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
        gl.uniform_matrix4fv(self.sky.at("u_invViewProj"), false, &mat(&view_proj.inverse()));
        gl.bind_vertex_array(self.fullscreen);
        gl.draw_arrays(gl::TRIANGLES, 0, 3);
        gl.depth_mask(true);
        gl.depth_func(gl::LESS);
        gl.bind_vertex_array(0);
    }

    /// Draws `cars`: each body, then its wheels.
    fn draw_cars(&mut self, gl: &mut Context, cars: &[CarView], planes: &[Vec4; 6]) {
        if cars.is_empty() {
            return;
        }
        gl.active_texture(gl::TEXTURE0);
        gl.bind_texture(gl::TEXTURE_CUBE_MAP, self.env_map);
        gl.use_program(self.car.id);
        let (at_model, at_paint, at_livery, at_lamps, at_shape) = (
            self.car.at("u_model"),
            self.car.at("u_paint"),
            self.car.at("u_livery"),
            self.car.at("u_lamps"),
            self.car.at("u_shape"),
        );
        for c in cars {
            let center = c.transform.transform_point3(Vec3::new(0.0, 0.8, 0.0));
            if outside(planes, center, 3.2) {
                continue;
            }
            let Some(k) = self.cars.iter().position(|e| e.0 == c.kind) else { continue };
            let shape = c.kind.shape();
            gl.uniform_matrix4fv(at_model, false, &mat(&c.transform));
            gl.uniform3f(at_paint, c.paint.x, c.paint.y, c.paint.z);
            gl.uniform1i(
                at_livery,
                match c.livery {
                    Livery::Plain => 0,
                    Livery::Police => 1,
                    Livery::Taxi => 2,
                },
            );
            let (phase, on) = c.siren.map_or((0.0, 0.0), |t| (t, 1.0));
            gl.uniform4f(at_lamps, c.headlights, c.brake, phase, on);
            gl.uniform4f(at_shape, shape.cowl, shape.a, shape.c, shape.belt);
            self.cars[k].1.draw(gl);
            let body = self.cars[k].1.count;
            self.count(body);
            // The wheels: the right-hand ones turned round, so that their
            // faces look out; the front ones steered.
            let wheels = self.cars[k].3;
            for (w, &at) in wheels.iter().enumerate() {
                let right = at.x < 0.0;
                let steer = if w < 2 { c.steer } else { 0.0 };
                let mut m = c.transform * Mat4::from_translation(at) * Mat4::from_rotation_y(steer);
                if right {
                    m *= Mat4::from_rotation_y(core::f32::consts::PI) * Mat4::from_rotation_x(-c.spin);
                } else {
                    m *= Mat4::from_rotation_x(c.spin);
                }
                gl.uniform_matrix4fv(at_model, false, &mat(&m));
                self.cars[k].2.draw(gl);
                let n = self.cars[k].2.count;
                self.count(n);
            }
        }
    }

    fn count(&mut self, indices: i32) {
        self.stats.draws += 1;
        self.stats.triangles += indices as u32 / 3;
    }
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
