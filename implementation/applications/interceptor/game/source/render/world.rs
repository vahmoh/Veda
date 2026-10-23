// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Veda Bay on the GPU, uploaded once: the road's chunks, the ground's
//! chunks with two levels of detail, the sea, the buildings, the street
//! lights, the trees, the harbour bridge, the billboards and the neon
//! signs, the street lights' glows; their textures, painted at start-up,
//! and the programs that draw them.
//!
//! Each frame the road's chunks and the trees are culled against the
//! camera's view (the trees' instances refilled with those in sight); the
//! rest is drawn whole, a draw for every kind of thing.

use alloc::string::String;
use alloc::vec::Vec;

use ::gl::{Context, gl};

use super::gpu::{self, Geometry, Program};
use super::instances::{self, Instances};
use super::shaders;
use super::{Frustum, Quality, Sizes, Stats, lit_program};
use crate::city::Block;
use crate::groundmesh;
use crate::lamps;
use crate::prelude::*;
use crate::roadmesh;
use crate::scenery;
use crate::texgen;
use crate::world::World;

/// A chunk of road on the GPU.
struct RoadGpu {
    tarmac: Geometry,
    sides: Geometry,
    center: Vec3,
    radius: f32,
}

/// A chunk of ground on the GPU: one vertex buffer, two index buffers
/// (fine and coarse), a vertex array and an index count for each.
struct GroundGpu {
    fine: (u32, i32),
    coarse: (u32, i32),
    center: Vec3,
    radius: f32,
}

/// A mesh drawn once for each of its instances.
struct Instanced {
    mesh: Geometry,
    instances: Instances,
}

impl Instanced {
    /// `mesh` with `data`'s instances (`floats` floats each).
    fn new(gl: &mut Context, mesh: &crate::mesh::Mesh, floats: usize, data: &[f32]) -> Instanced {
        let mesh = Geometry::new(gl, mesh);
        let instances = Instances::fixed(gl, mesh.vao, floats, data);
        Instanced { mesh, instances }
    }

    fn draw(&self, gl: &mut Context, stats: &mut Stats) {
        if self.instances.count() > 0 {
            self.instances.draw(gl, &self.mesh);
            stats.count_instanced(self.mesh.count, self.instances.count());
        }
    }
}

/// The trees: a mesh of each kind with an instance buffer of those in
/// sight, refilled every frame; and where they all are.
struct Trees {
    kinds: [(Geometry, Instances, Vec<f32>); 2],
    all: Vec<scenery::Tree>,
    /// Further than this they are not drawn (m).
    far: f32,
}

/// The world on the GPU.
pub struct WorldGpu {
    terrain: Program,
    tarmac: Program,
    surface: Program,
    water: Program,
    building: Program,
    prop: Program,
    sign: Program,
    asphalt: u32,
    surfaces: u32,
    ground_tex: u32,
    ripples: u32,
    walls: u32,
    signs: u32,
    road: Vec<RoadGpu>,
    /// The road's chunks in view this frame.
    visible: Vec<usize>,
    ground: Vec<GroundGpu>,
    /// Beyond this the ground is drawn coarse (m).
    fine_ground: f32,
    sea: Geometry,
    /// The unit box every block is drawn with, and the blocks.
    blocks: Instanced,
    /// Each kind of street light, with its instances.
    lamps: Vec<(Instanced, lamps::Kind)>,
    trees: Trees,
    /// The harbour bridge's towers and cables (one instance).
    bridge: Instanced,
    /// The billboards' frames and the neon signs, and the billboards'
    /// faces with their pictures.
    boards: Instanced,
    neons: Instanced,
    faces: Instanced,
    /// The lamps' glows (an instanced quad): those lit after dark, and
    /// those day and night (a tunnel's lamps, the bridge's beacons).
    lamp_glows: Instances,
    always_glows: Instances,
    /// The lane markings' dashes' period (m).
    dash: f32,
}

impl WorldGpu {
    /// Uploads `world`, its textures as large as `sizes` has them, with the
    /// programs compiled with `defines`.
    pub fn new(
        gl: &mut Context,
        world: &World,
        sizes: &Sizes,
        quality: Quality,
        defines: &'static str,
    ) -> Result<WorldGpu, String> {
        let lit = |gl: &mut Context, name: &str, vs: &str, fs: &str, uniforms: &[&'static str], own| {
            lit_program(gl, defines, name, vs, fs, uniforms, own)
        };
        let terrain = lit(gl, "terrain", shaders::STATIC_VS, shaders::TERRAIN_FS, &[], Some("u_ground"))?;
        let tarmac = lit(gl, "tarmac", shaders::STATIC_VS, shaders::TARMAC_FS, &["u_dash"], Some("u_asphalt"))?;
        let surface = lit(gl, "surface", shaders::STATIC_VS, shaders::SURFACE_FS, &[], Some("u_surfaces"))?;
        let water = lit(gl, "water", shaders::STATIC_VS, shaders::WATER_FS, &[], Some("u_ripples"))?;
        let building = lit(gl, "building", shaders::BUILDING_VS, shaders::BUILDING_FS, &[], Some("u_walls"))?;
        let prop = lit(gl, "prop", shaders::PROP_VS, shaders::PROP_FS, &["u_always"], None)?;
        let sign = lit(gl, "signs", shaders::SIGN_VS, shaders::SIGN_FS, &[], Some("u_signs"))?;

        // Textures.
        let aniso = gl.caps().max_anisotropy.min(8.0);
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
        let signs = array(gl, &texgen::billboards(if quality == Quality::High { 512 } else { 256 }));

        // The world's meshes.
        let road: Vec<RoadGpu> = roadmesh::chunks(&world.road)
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
        let lamp_kinds = [lamps::Kind::Post, lamps::Kind::Double, lamps::Kind::Ceiling].map(|k| {
            let data: Vec<f32> = world
                .lamps
                .iter()
                .filter(|l| l.kind == k)
                .flat_map(|l| [l.base.x, l.base.y, l.base.z, l.yaw, l.color.x, l.color.y, l.color.z, 1.0])
                .collect();
            (Instanced::new(gl, &lamps::mesh(k), 8, &data), k)
        });
        let tree_kinds = [scenery::TreeKind::Pine, scenery::TreeKind::Broad].map(|k| {
            let mesh = Geometry::new(gl, &scenery::tree_mesh(k));
            let instances = Instances::new(gl, mesh.vao, 8);
            (mesh, instances, Vec::new())
        });
        let far = if quality == Quality::High { 900.0 } else { 320.0 };
        let trees = Trees { kinds: tree_kinds, all: world.trees.clone(), far };
        let (bridge_mesh, beacons, bridge_lights) = scenery::bridge(&world.road);
        // Where the bridge is (its mesh is in the world's coordinates) and
        // its paint.
        let bridge = Instanced::new(gl, &bridge_mesh, 8, &[0.0, 0.0, 0.0, 0.0, 0.62, 0.17, 0.07, 1.0]);
        // The billboards and the neon signs: a static instance each.
        let board_data: Vec<f32> = world
            .billboards
            .iter()
            .flat_map(|b| [b.base.x, b.base.y, b.base.z, b.yaw, f32::from(b.design), 0.0, 0.0, 1.0])
            .collect();
        let boards = Instanced::new(gl, &scenery::board_frame_mesh(), 8, &board_data);
        let faces = Instanced::new(gl, &scenery::board_face_mesh(), 8, &board_data);
        let neon_data: Vec<f32> = world
            .neons
            .iter()
            .flat_map(|n| [n.base.x, n.base.y, n.base.z, n.yaw, n.color.x, n.color.y, n.color.z, 1.0])
            .collect();
        let neons = Instanced::new(gl, &scenery::neon_mesh(), 8, &neon_data);
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
            let quads = |gl: &mut Context, data: &[f32]| {
                let (vao, _) = instances::quad(gl);
                Instances::fixed(gl, vao, 8, data)
            };
            (quads(gl, &dark), quads(gl, &always))
        };
        Ok(WorldGpu {
            terrain,
            tarmac,
            surface,
            water,
            building,
            prop,
            sign,
            asphalt,
            surfaces,
            ground_tex,
            ripples,
            walls,
            signs,
            visible: Vec::with_capacity(road.len()),
            road,
            ground,
            fine_ground: sizes.fine_ground,
            sea,
            blocks,
            lamps: lamp_kinds.into(),
            trees,
            bridge,
            boards,
            neons,
            faces,
            lamp_glows,
            always_glows,
            dash: world.dash_period(),
        })
    }

    /// Finds what is in view this frame through `planes` from `eye`: the
    /// road's chunks, and the trees near enough to be seen (their
    /// instances refilled).
    pub fn cull(&mut self, gl: &mut Context, planes: &Frustum, eye: Vec3) {
        self.visible.clear();
        self.visible
            .extend((0..self.road.len()).filter(|&i| !planes.outside(self.road[i].center, self.road[i].radius)));
        let far = self.trees.far;
        for (_, _, data) in &mut self.trees.kinds {
            data.clear();
        }
        for t in &self.trees.all {
            let top = t.base + Vec3::Y * (5.0 * t.scale);
            if top.distance_squared(eye) > far * far || planes.outside(top, 7.0 * t.scale) {
                continue;
            }
            let k = usize::from(t.kind == scenery::TreeKind::Broad);
            let data = &mut self.trees.kinds[k].2;
            data.extend_from_slice(&[t.base.x, t.base.y, t.base.z, t.yaw, t.tint.x, t.tint.y, t.tint.z, t.scale]);
        }
        for (_, instances, data) in &mut self.trees.kinds {
            instances.fill(gl, data);
        }
    }

    /// The road's tarmac in view.
    pub fn draw_tarmac(&self, gl: &mut Context, stats: &mut Stats) {
        gl.use_program(self.tarmac.id);
        gl.uniform1f(self.tarmac.at("u_dash"), self.dash);
        gl.active_texture(gl::TEXTURE0 + gpu::unit::OWN);
        gl.bind_texture(gl::TEXTURE_2D, self.asphalt);
        for &i in &self.visible {
            self.road[i].tarmac.draw(gl);
            stats.count(self.road[i].tarmac.count);
        }
    }

    /// The rest, after the tarmac (and what is laid on it): the road's
    /// sides in view, the buildings, the street lights, the trees, the
    /// bridge, the billboards and the neon signs, the ground in view (fine
    /// near `eye`) and the sea.
    pub fn draw_rest(&self, gl: &mut Context, planes: &Frustum, eye: Vec3, stats: &mut Stats) {
        gl.active_texture(gl::TEXTURE0 + gpu::unit::OWN);
        gl.use_program(self.surface.id);
        gl.bind_texture(gl::TEXTURE_2D_ARRAY, self.surfaces);
        for &i in &self.visible {
            self.road[i].sides.draw(gl);
            stats.count(self.road[i].sides.count);
        }

        // The buildings, all in one draw.
        gl.use_program(self.building.id);
        gl.bind_texture(gl::TEXTURE_2D_ARRAY, self.walls);
        self.blocks.draw(gl, stats);

        // The street lights, the trees, the bridge, the billboards' frames
        // and the neon signs.
        gl.use_program(self.prop.id);
        for (lamps, kind) in &self.lamps {
            gl.uniform1f(self.prop.at("u_always"), if kind.always_lit() { 1.0 } else { 0.0 });
            lamps.draw(gl, stats);
        }
        gl.uniform1f(self.prop.at("u_always"), 0.0);
        self.draw_props(gl, stats);
        // The billboards' pictures.
        gl.use_program(self.sign.id);
        gl.bind_texture(gl::TEXTURE_2D_ARRAY, self.signs);
        self.faces.draw(gl, stats);

        // The ground, fine near the camera.
        gl.use_program(self.terrain.id);
        gl.bind_texture(gl::TEXTURE_2D_ARRAY, self.ground_tex);
        for c in &self.ground {
            if planes.outside(c.center, c.radius) {
                continue;
            }
            let near = (c.center.distance(eye) - c.radius).max(0.0);
            let (vao, count) = if near < self.fine_ground { c.fine } else { c.coarse };
            gl.bind_vertex_array(vao);
            gl.draw_elements(gl::TRIANGLES, count, gl::UNSIGNED_INT, 0);
            stats.count(count);
        }

        // The sea.
        gl.use_program(self.water.id);
        gl.bind_texture(gl::TEXTURE_2D, self.ripples);
        self.sea.draw(gl);
        stats.count(self.sea.count);
    }

    /// The trees in view, the bridge, the billboards' frames and the neon
    /// signs, with the program bound (lit, or the shadow map's).
    fn draw_props(&self, gl: &mut Context, stats: &mut Stats) {
        for (mesh, instances, _) in &self.trees.kinds {
            if instances.count() > 0 {
                instances.draw(gl, mesh);
                stats.count_instanced(mesh.count, instances.count());
            }
        }
        self.bridge.draw(gl, stats);
        self.boards.draw(gl, stats);
        self.neons.draw(gl, stats);
    }

    /// What casts the sun's shadows, seen through `planes` from the sun,
    /// with the shadow map's `programs` (static meshes', the buildings',
    /// the props'): the road's sides, the buildings, the street lights,
    /// the trees and the bridge.
    pub fn draw_casters(&self, gl: &mut Context, planes: &Frustum, programs: [u32; 3], stats: &mut Stats) {
        let [statik, building, prop] = programs;
        gl.use_program(statik);
        for c in &self.road {
            if !planes.outside(c.center, c.radius) {
                c.sides.draw(gl);
                stats.count(c.sides.count);
            }
        }
        gl.use_program(building);
        self.blocks.draw(gl, stats);
        gl.use_program(prop);
        for (lamps, _) in &self.lamps {
            lamps.draw(gl, stats);
        }
        self.draw_props(gl, stats);
    }

    /// Whether some lamps glow by day too.
    pub fn glows_always(&self) -> bool {
        self.always_glows.count() > 0
    }

    /// The lamps' glows with the glow program bound: those lit after dark
    /// if `dark`, and those always lit.
    pub fn draw_glows(&self, gl: &mut Context, dark: bool, stats: &mut Stats) {
        for (glows, on) in [(&self.lamp_glows, dark), (&self.always_glows, true)] {
            if on && glows.count() > 0 {
                glows.draw_quads(gl);
                stats.count_instanced(6, glows.count());
            }
        }
    }
}

/// The unit box the blocks are drawn with (no floor), with an instance of
/// each block: where, how turned, how big, what style, what colour, what
/// seed.
fn upload_blocks(gl: &mut Context, blocks: &[Block]) -> Instanced {
    let mut cube = crate::mesh::cuboid(Vec3::new(-0.5, 0.0, -0.5), Vec3::new(0.5, 1.0, 0.5), [0; 4]);
    // Drop the floor.
    let keep: Vec<u32> =
        cube.indices.chunks(3).filter(|t| cube.vertices[t[0] as usize].normal.y > -0.5).flatten().copied().collect();
    cube.indices = keep;
    let mut data = Vec::with_capacity(blocks.len() * 12);
    for b in blocks {
        data.extend_from_slice(&[b.base.x, b.base.y, b.base.z, b.yaw]);
        data.extend_from_slice(&[b.size.x, b.size.y, b.size.z, f32::from(b.style)]);
        data.extend_from_slice(&[b.tint.x, b.tint.y, b.tint.z, b.seed]);
    }
    Instanced::new(gl, &cube, 12, &data)
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
