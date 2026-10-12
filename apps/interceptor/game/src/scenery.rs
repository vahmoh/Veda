//! The bay's furniture beyond the road and the city: trees on the hills
//! and along the coast, the harbour bridge's towers, cables and lights,
//! billboards along the open road, and neon names on downtown's roofs.
//!
//! Trees grow where the land is dry and not too steep, thick near the road
//! and thinning away from it: pines in the hills, broad trees towards the
//! sea. The bridge is a suspension bridge: two towers either side of the
//! deck, rising from the water, the main cables hung between them and
//! down to the deck at its ends, hangers every few metres, red lights on
//! the towers' tops and a string of lights along each cable. Billboards
//! (lit from within at night) face the drivers coming; the neon signs face
//! the street from the tallest towers by it.

use alloc::vec::Vec;

use rand::{RngExt, SeedableRng};
use rand_pcg::Pcg32;

use crate::city::Block;
use crate::mesh::{Mesh, Params, cuboid, lathe, sphere};
use crate::prelude::*;
use crate::road::{Road, Section};
use crate::terrain::Terrain;

/// The parts of the props' meshes (the prop program's materials).
pub mod part {
    pub const BARK: u8 = 3;
    pub const LEAVES: u8 = 4;
    pub const STEEL: u8 = 5;
    pub const BEACON: u8 = 6;
    pub const NEON: u8 = 7;
}

/// What a tree is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TreeKind {
    Pine,
    Broad,
}

/// A tree.
#[derive(Clone, Copy, Debug)]
pub struct Tree {
    pub kind: TreeKind,
    pub base: Vec3,
    pub yaw: f32,
    pub scale: f32,
    /// Its leaves' colour.
    pub tint: Vec3,
}

/// The most trees (fewer where the land is busy).
const MOST_TREES: usize = 4000;

/// Plants the trees along the road outside the city.
pub fn trees(road: &Road, terrain: &Terrain, blocks: &[Block], boards: &[Billboard], seed: u64) -> Vec<Tree> {
    let mut rng = Pcg32::seed_from_u64(seed ^ 0x7EE5);
    let mut out: Vec<Tree> = Vec::new();
    let mut s = 0.0;
    while s < road.length() && out.len() < MOST_TREES {
        let at = road.at(s);
        s += 5.0;
        let wooded = match at.section {
            Section::Hills => 1.0,
            Section::Coast => 0.55,
            Section::Port => 0.15,
            Section::City | Section::Tunnel | Section::Bridge => 0.0,
        };
        if wooded == 0.0 {
            continue;
        }
        for side in [-1.0f32, 1.0] {
            // Most near the road, fewer further away.
            for _ in 0..6 {
                if rng.random::<f32>() >= 0.5 * wooded {
                    continue;
                }
                let away = at.wall + 4.0 + rng.random::<f32>().powi(2) * 120.0;
                let p = road.point(s + rng.random_range(-2.5..2.5), side * away);
                let (x, z) = (p.x, p.z);
                if terrain.is_water(x, z) || terrain.normal(x, z).y < 0.72 {
                    continue;
                }
                // Clear of every stretch of the road (it loops back), and of
                // the buildings.
                let near = road.locate_from(Vec3::new(x, 0.0, z), terrain.nearest_sample(x, z));
                let (_, wall) = road.widths(near.s);
                if near.lateral.abs() < wall + 3.0 {
                    continue;
                }
                let on_block =
                    blocks.iter().any(|b| b.base.xz().distance(Vec2::new(x, z)) < b.size.x.max(b.size.z) * 0.8);
                // Not in front of a billboard, nor against it.
                let by_board = boards.iter().any(|b| b.base.xz().distance(Vec2::new(x, z)) < 18.0);
                if on_block
                    || by_board
                    || out.iter().rev().take(40).any(|t| t.base.xz().distance(Vec2::new(x, z)) < 3.5)
                {
                    continue;
                }
                let pine = match at.section {
                    Section::Hills => rng.random_bool(0.75),
                    _ => rng.random_bool(0.3),
                };
                let green = rng.random_range(0.0..1.0);
                let tint = if pine {
                    Vec3::new(0.05, 0.13 + green * 0.05, 0.07)
                } else {
                    Vec3::new(0.09 + green * 0.06, 0.18 + green * 0.08, 0.05)
                };
                out.push(Tree {
                    kind: if pine { TreeKind::Pine } else { TreeKind::Broad },
                    base: Vec3::new(x, terrain.height(x, z) - 0.2, z),
                    yaw: rng.random_range(0.0..=core::f32::consts::TAU),
                    scale: rng.random_range(0.75..1.35),
                    tint,
                });
            }
        }
    }
    out
}

fn params(part: u8) -> Params {
    [part, 0, 0, 0]
}

/// A tree's mesh (about 10 m tall), its leaves to be coloured by the
/// instance.
pub fn tree_mesh(kind: TreeKind) -> Mesh {
    let mut m = Mesh::new();
    let leaves = params(part::LEAVES);
    match kind {
        TreeKind::Pine => {
            m.append(&lathe(&[(0.28, 0.0), (0.2, 2.2)], 6, false, params(part::BARK)));
            for (r, y0, y1) in [(2.6, 1.6, 6.0), (2.0, 4.0, 8.2), (1.3, 6.4, 10.4)] {
                m.append(&lathe(&[(r, y0), (0.0, y1)], 8, true, leaves));
            }
        }
        TreeKind::Broad => {
            m.append(&lathe(&[(0.34, 0.0), (0.24, 4.2)], 6, false, params(part::BARK)));
            let crown = |r: f32, at: Vec3, m: &mut Mesh| {
                m.append_transformed(&sphere(r, 5, 8, leaves), &Mat4::from_translation(at));
            };
            crown(3.0, Vec3::new(0.0, 6.0, 0.0), &mut m);
            crown(2.1, Vec3::new(1.6, 5.0, 0.6), &mut m);
            crown(1.9, Vec3::new(-1.3, 5.3, -0.9), &mut m);
        }
    }
    m
}

/// A billboard: the ground under its middle, which way its face looks,
/// and which picture it shows.
#[derive(Clone, Copy, Debug)]
pub struct Billboard {
    pub base: Vec3,
    pub yaw: f32,
    pub design: u8,
}

/// A billboard's face (m), and how high its bottom stands.
pub const BOARD: (f32, f32) = (12.0, 6.0);
const BOARD_LIFT: f32 = 4.0;
/// The designs (as texgen paints them).
pub const DESIGNS: u8 = 4;

/// Billboards along the open road, every few hundred metres on alternate
/// sides, turned to the drivers coming.
pub fn billboards(road: &Road, terrain: &Terrain) -> Vec<Billboard> {
    let mut out = Vec::new();
    let mut s = 180.0;
    let mut k = 0u8;
    while s < road.length() {
        let at = road.at(s);
        let here = s;
        s += 320.0;
        if matches!(at.section, Section::City | Section::Tunnel | Section::Bridge) {
            continue;
        }
        let side = if k.is_multiple_of(2) { 1.0 } else { -1.0 };
        let p = road.point(here, side * (at.wall + 7.0));
        if terrain.is_water(p.x, p.z) || terrain.normal(p.x, p.z).y < 0.8 {
            continue;
        }
        let near = road.locate_from(p, terrain.nearest_sample(p.x, p.z));
        let (_, wall) = road.widths(near.s);
        if near.lateral.abs() < wall + 4.0 {
            continue;
        }
        // Facing back down the road, turned a little towards it.
        let (_, f, r, _) = road.frame(here);
        let face = (-Vec3::new(f.x, 0.0, f.z) - Vec3::new(r.x, 0.0, r.z) * (side * 0.45)).normalize_or(Vec3::Z);
        out.push(Billboard {
            base: Vec3::new(p.x, terrain.height(p.x, p.z), p.z),
            yaw: face.x.atan2(face.z),
            design: k % DESIGNS,
        });
        k += 1;
    }
    out
}

/// A billboard's posts and its box (the face is drawn apart).
pub fn board_frame_mesh() -> Mesh {
    let mut m = Mesh::new();
    let metal = params(0);
    let (w, h) = BOARD;
    for x in [-w * 0.3, w * 0.3] {
        m.append(&cuboid(Vec3::new(x - 0.25, -1.0, -0.6), Vec3::new(x + 0.25, BOARD_LIFT + 0.2, -0.1), metal));
    }
    m.append(&cuboid(
        Vec3::new(-w * 0.5 - 0.2, BOARD_LIFT - 0.2, -0.5),
        Vec3::new(w * 0.5 + 0.2, BOARD_LIFT + h + 0.2, 0.0),
        metal,
    ));
    m
}

/// A billboard's face: a picture across `BOARD`, looking along +z.
pub fn board_face_mesh() -> Mesh {
    let mut m = Mesh::new();
    let (w, h) = BOARD;
    let p = params(0);
    let n = Vec3::Z;
    let a = m.push(Vec3::new(-w * 0.5, BOARD_LIFT, 0.02), n, Vec2::new(0.0, 1.0), p);
    let b = m.push(Vec3::new(w * 0.5, BOARD_LIFT, 0.02), n, Vec2::new(1.0, 1.0), p);
    let c = m.push(Vec3::new(w * 0.5, BOARD_LIFT + h, 0.02), n, Vec2::new(1.0, 0.0), p);
    let d = m.push(Vec3::new(-w * 0.5, BOARD_LIFT + h, 0.02), n, Vec2::new(0.0, 0.0), p);
    m.quad(a, b, c, d);
    m
}

/// A neon sign on a roof: where it stands, which way it faces, its
/// colour.
#[derive(Clone, Copy, Debug)]
pub struct Neon {
    pub base: Vec3,
    pub yaw: f32,
    pub color: Vec3,
}

/// What the neon signs say, and how tall their letters are (m).
pub const NEON_TEXT: &str = "VEDA";
pub const NEON_SIZE: f32 = 7.0;

/// The neon signs: on the tallest towers by the street, facing it, apart
/// from each other.
pub fn neons(road: &Road, blocks: &[Block]) -> Vec<Neon> {
    let colors = [Vec3::new(0.15, 0.75, 1.0), Vec3::new(1.0, 0.15, 0.7), Vec3::new(0.3, 0.45, 1.0)];
    let wide = crate::font::width(NEON_TEXT) * NEON_SIZE / crate::font::CAP + 2.0;
    let mut tall: Vec<&Block> = blocks
        .iter()
        .filter(|b| b.size.y > 45.0 && b.size.x > wide && road.locate(b.base, None).lateral.abs() < 70.0)
        .collect();
    tall.sort_by(|a, b| b.size.y.total_cmp(&a.size.y));
    let mut out: Vec<Neon> = Vec::new();
    for b in tall {
        if out.len() >= 7 || out.iter().any(|n| n.base.xz().distance(b.base.xz()) < 180.0) {
            continue;
        }
        let at = road.locate(b.base, None);
        let to_road = (road.point(at.s, 0.0) - b.base).xz().normalize_or(Vec2::Y);
        let edge = b.size.z * 0.5 - 1.5;
        let base = Vec3::new(b.base.x + to_road.x * edge, b.base.y + b.size.y, b.base.z + to_road.y * edge);
        out.push(Neon { base, yaw: to_road.x.atan2(to_road.y), color: colors[out.len() % colors.len()] });
    }
    out
}

/// A neon sign's mesh: the letters' tubes (coloured by the instance) on a
/// dark scaffold, centred, looking along +z.
pub fn neon_mesh() -> Mesh {
    let mut m = Mesh::new();
    let k = NEON_SIZE / crate::font::CAP;
    let w = crate::font::width(NEON_TEXT) * k;
    let tube = params(part::NEON);
    for (a, b) in crate::font::layout(NEON_TEXT, NEON_SIZE) {
        let (a, b) = (Vec3::new(a.x - w * 0.5, a.y + 1.6, 0.4), Vec3::new(b.x - w * 0.5, b.y + 1.6, 0.4));
        let b = if a.distance(b) < 0.05 { b + Vec3::X * 0.05 } else { b };
        m.append(&beam(a, b, 0.55, 0.55, Vec3::Z, tube));
    }
    let metal = params(0);
    m.append(&cuboid(Vec3::new(-w * 0.5 - 0.5, 0.0, -0.4), Vec3::new(w * 0.5 + 0.5, 0.6, 0.0), metal));
    for x in [-w * 0.35, 0.0, w * 0.35] {
        m.append(&cuboid(Vec3::new(x - 0.15, 0.0, -0.3), Vec3::new(x + 0.15, NEON_SIZE + 1.6, -0.05), metal));
    }
    m
}

/// A beam from `a` to `b`, `w` by `h` across (its height along `up`).
fn beam(a: Vec3, b: Vec3, w: f32, h: f32, up: Vec3, p: Params) -> Mesh {
    let along = b - a;
    let len = along.length().max(1e-3);
    let z = along / len;
    let x = up.cross(z).normalize_or(Vec3::X);
    let y = z.cross(x);
    let basis = Mat4::from_cols(x.extend(0.0), y.extend(0.0), z.extend(0.0), a.extend(1.0));
    let mut m = Mesh::new();
    m.append_transformed(&cuboid(Vec3::new(-w * 0.5, -h * 0.5, 0.0), Vec3::new(w * 0.5, h * 0.5, len), p), &basis);
    m
}

/// The harbour bridge's towers, cables and hangers (in the world's
/// coordinates), and the points its lights shine from: the towers' red
/// beacons, then the cables' lights.
pub fn bridge(road: &Road) -> (Mesh, Vec<Vec3>, Vec<Vec3>) {
    let mut m = Mesh::new();
    let (mut beacons, mut lights) = (Vec::new(), Vec::new());
    let samples = road.samples();
    let n = samples.len();
    let Some(first) =
        (0..n).find(|&i| samples[i].section == Section::Bridge && samples[(i + n - 1) % n].section != Section::Bridge)
    else {
        return (m, beacons, lights);
    };
    let len = (0..n).take_while(|k| samples[(first + k) % n].section == Section::Bridge).count();
    let s0 = first as f32 * road.spacing();
    let span = len as f32 * road.spacing();
    let steel = params(part::STEEL);
    let (_, wall) = road.widths(s0 + span * 0.5);
    let out = wall + 1.4;
    // The towers, at a quarter and three quarters of the way.
    let tower_s = [s0 + span * 0.24, s0 + span * 0.76];
    let deck = |s: f32| road.point(s, 0.0).y;
    let high = deck(tower_s[0]).max(deck(tower_s[1])) + 62.0;
    for &ts in &tower_s {
        let (_, f, r, _) = road.frame(ts);
        let flat = Vec3::new(f.x, 0.0, f.z).normalize_or(Vec3::Z);
        for side in [-1.0f32, 1.0] {
            let foot = road.point(ts, side * out);
            let base = Vec3::new(foot.x, -6.0, foot.z);
            let top = Vec3::new(foot.x, high, foot.z);
            m.append(&beam(base, top, 3.0, 3.6, flat, steel));
            beacons.push(top + Vec3::Y * 1.2);
        }
        // Cross beams: under the deck, above it, and at the top.
        let (a, b) = (road.point(ts, -out), road.point(ts, out));
        for y in [deck(ts) - 3.0, deck(ts) + 26.0, high - 4.0] {
            let (a, b) = (Vec3::new(a.x, y, a.z), Vec3::new(b.x, y, b.z));
            m.append(&beam(a, b, 2.4, 3.2, Vec3::Y, steel));
        }
        let _ = r;
    }
    // The main cables: from the deck at the ends up to the towers' tops,
    // and hung between the towers, low in the middle.
    let cable_y = |s: f32| -> f32 {
        let (a, b) = (tower_s[0], tower_s[1]);
        if s < a {
            let t = (s - s0) / (a - s0);
            deck(s) + 2.0 + (high - 2.0 - deck(s0)) * t * t
        } else if s > b {
            let t = (s0 + span - s) / (s0 + span - b);
            deck(s) + 2.0 + (high - 2.0 - deck(s0 + span)) * t * t
        } else {
            let u = (s - a) / (b - a) * 2.0 - 1.0;
            deck(s) + 4.0 + (high - 4.0 - deck(s)) * u * u
        }
    };
    let step = 6.0;
    for side in [-1.0f32, 1.0] {
        let at = |s: f32| {
            let p = road.point(s, side * out);
            Vec3::new(p.x, cable_y(s), p.z)
        };
        let mut s = s0;
        while s < s0 + span - 1e-3 {
            let next = (s + step).min(s0 + span);
            m.append(&beam(at(s), at(next), 0.7, 0.7, Vec3::Y, steel));
            // Every other step, a hanger down to the deck and a light.
            let p = at(s);
            if (((s - s0) / step) as u32).is_multiple_of(2) {
                let d = road.point(s, side * (out - 0.6));
                if p.y - d.y > 2.0 {
                    m.append(&beam(Vec3::new(d.x, d.y, d.z), Vec3::new(d.x, p.y, d.z), 0.16, 0.16, Vec3::Z, steel));
                }
                lights.push(p + Vec3::Y * 0.6);
            }
            s = next;
        }
    }
    (m, beacons, lights)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::World;

    #[test]
    fn trees_grow_off_the_road_and_the_bridge_stands_over_the_water() {
        let w = World::new();
        let trees = &w.trees;
        let pines = trees.iter().filter(|t| t.kind == TreeKind::Pine).count();
        std::println!("{} trees ({} pines)", trees.len(), pines);
        assert!(trees.len() > 800 && pines > 100 && pines < trees.len());
        for t in trees {
            let at = w.road.locate(t.base, None);
            let (_, wall) = w.road.widths(at.s);
            assert!(at.lateral.abs() > wall + 2.5, "a tree on the road");
            assert!(!w.terrain.is_water(t.base.x, t.base.z), "a tree in the sea");
        }
        let (mesh, beacons, lights) = bridge(&w.road);
        std::println!(
            "the bridge: {} triangles, {} beacons, {} lights",
            mesh.indices.len() / 3,
            beacons.len(),
            lights.len()
        );
        assert_eq!(beacons.len(), 4);
        assert!(lights.len() > 20 && mesh.indices.len() > 1000);
        for b in &beacons {
            assert!(b.y > 60.0 && w.terrain.is_water(b.x, b.z), "a tower at {b:?}");
        }
        std::println!("{} billboards, {} neon signs", w.billboards.len(), w.neons.len());
        assert!(w.billboards.len() >= 6 && w.neons.len() >= 3);
        for b in &w.billboards {
            let at = w.road.locate(b.base, None);
            let (_, wall) = w.road.widths(at.s);
            assert!(at.lateral.abs() > wall + 3.0, "a billboard on the road");
        }
        for n in &w.neons {
            assert!(n.base.y > 40.0, "a neon sign low at {:?}", n.base);
        }
        assert!(!neon_mesh().indices.is_empty() && board_face_mesh().indices.len() == 6);
    }
}
