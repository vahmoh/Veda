//! The buildings: rows of them along the downtown avenues, towers in the
//! core, lower blocks further out, sheds and warehouses along the port.
//!
//! Each building is a box (or a tower of stacked boxes, each narrower than
//! the one below), turned to face the road; its facade (windows, floors,
//! shop fronts, lights at night) is painted by its program from its style
//! and seed. Buildings stand clear of the road's walls and of the water.

use alloc::vec::Vec;

use crate::prelude::*;
use vmath::Rng;

use crate::road::{Road, Section};
use crate::terrain::Terrain;

/// What a building looks like.
pub mod style {
    /// A glass curtain wall.
    pub const GLASS: u8 = 0;
    /// Concrete with ribbon windows.
    pub const OFFICE: u8 = 1;
    /// Brick, with punched windows.
    pub const BRICK: u8 = 2;
    /// Pale render with balconies.
    pub const FLATS: u8 = 3;
    /// Corrugated metal sheds.
    pub const SHED: u8 = 4;
    /// Stone, with tall piers.
    pub const STONE: u8 = 5;
    pub const COUNT: u8 = 6;
}

/// A box of a building.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Block {
    /// The middle of its footprint, at its foot.
    pub base: Vec3,
    /// How it is turned about the vertical (radians): its width runs along
    /// `(cos, -sin)` in x and z.
    pub yaw: f32,
    /// Width (along the road), height, depth.
    pub size: Vec3,
    pub style: u8,
    /// Its walls' colour (linear).
    pub tint: Vec3,
    /// Picks its windows' lights and details (0..1).
    pub seed: f32,
}

impl Block {
    /// The footprint's corners (x, z).
    pub fn corners(&self) -> [Vec2; 4] {
        let (s, c) = self.yaw.sin_cos();
        let along = Vec2::new(c, -s);
        let across = Vec2::new(s, c);
        let (hw, hd) = (self.size.x * 0.5, self.size.z * 0.5);
        let o = self.base.xz();
        [
            o - along * hw - across * hd,
            o + along * hw - across * hd,
            o + along * hw + across * hd,
            o - along * hw + across * hd,
        ]
    }
}

/// Where downtown's core is: the towers rise towards it.
const CORE: Vec2 = Vec2::new(-60.0, -640.0);

/// Whether a footprint stands clear of the road (every corner more than
/// `margin` outside the walls) and on dry land.
fn clear(road: &Road, terrain: &Terrain, corners: &[Vec2; 4], margin: f32) -> bool {
    // The corners and the edges' midpoints.
    let mut points = [Vec2::ZERO; 8];
    for k in 0..4 {
        points[k] = corners[k];
        points[k + 4] = (corners[k] + corners[(k + 1) % 4]) * 0.5;
    }
    points.iter().all(|p| {
        let at = road.locate(Vec3::new(p.x, 0.0, p.y), None);
        let wall = road.sample(at.index as isize).wall;
        at.lateral.abs() > wall + margin && terrain.height(p.x, p.y) > 1.0
    })
}

/// A building of `size` on the road's `side` at `s`, `lateral` metres out
/// to its front face, turned to face the road; moved out until it is
/// clear of it. `None` if it never is (or stands in the water).
fn place(road: &Road, terrain: &Terrain, s: f32, side: f32, lateral: f32, size: Vec3) -> Option<(Vec3, f32)> {
    let (_, f, _, _) = road.frame(s);
    let yaw = (-f.z).atan2(f.x);
    let mut out = lateral + size.z * 0.5;
    for _ in 0..6 {
        let p = road.point(s, side * out);
        let block = Block { base: Vec3::new(p.x, 0.0, p.z), yaw, size, style: 0, tint: Vec3::ONE, seed: 0.0 };
        let corners = block.corners();
        if clear(road, terrain, &corners, 0.6) {
            // The foot: the lowest ground under it, a little below.
            let ground = corners.iter().map(|c| terrain.height(c.x, c.y)).fold(f32::MAX, f32::min);
            return Some((Vec3::new(p.x, ground.min(p.y) - 0.6, p.z), yaw));
        }
        out += 3.0;
    }
    None
}

/// The buildings of Veda Bay.
pub fn buildings(road: &Road, terrain: &Terrain, seed: u64) -> Vec<Block> {
    let mut rng = Rng::new(seed ^ 0xC17E);
    let mut out = Vec::new();
    let city = |s: f32| road.at(s).section == Section::City && road.at(s).pavement >= 0.99;
    for side in [-1.0f32, 1.0] {
        // The rows: the front one on the pavement's edge, then two more
        // behind it, further out and taller.
        for (row, lateral) in [(0, 15.0f32), (1, 52.0), (2, 96.0)] {
            let mut s = rng.range_f32(0.0, 20.0);
            while s < road.length() {
                if !city(s) {
                    s += 8.0;
                    continue;
                }
                let p = road.point(s, 0.0);
                let core = (1.0 - p.xz().distance(CORE) / 900.0).clamp(0.0, 1.0);
                let width = match row {
                    0 => rng.range_f32(14.0, 34.0),
                    _ => rng.range_f32(22.0, 44.0),
                };
                let depth = match row {
                    0 => rng.range_f32(16.0, 30.0),
                    _ => rng.range_f32(20.0, 38.0),
                };
                // Taller in the core and further back; a few towers.
                let tall = core * core * if rng.chance(0.25 + 0.35 * core) { rng.range_f32(1.0, 2.2) } else { 0.5 };
                let height = (12.0 + rng.range_f32(0.0, 18.0) + tall * 70.0 * (1.0 + row as f32 * 0.5)).min(190.0);
                let style = if height > 70.0 {
                    if rng.chance(0.6) { style::GLASS } else { style::STONE }
                } else {
                    [style::OFFICE, style::BRICK, style::FLATS, style::BRICK, style::OFFICE][rng.below_usize(5)]
                };
                let size = Vec3::new(width, height, depth);
                if let Some((base, yaw)) = place(road, terrain, s + width * 0.5, side, lateral, size) {
                    let tint = tint_of(style, &mut rng);
                    let seed = rng.next_f32();
                    out.push(Block { base, yaw, size, style, tint, seed });
                    // Towers step back as they rise.
                    if height > 60.0 && rng.chance(0.7) {
                        let mut below = *out.last().unwrap();
                        let mut top = below.base.y + below.size.y;
                        for _ in 0..rng.range_u32(1, 3) {
                            let shrink = rng.range_f32(0.65, 0.85);
                            let rise = below.size.y * rng.range_f32(0.25, 0.5);
                            let next = Block {
                                base: Vec3::new(below.base.x, top, below.base.z),
                                size: Vec3::new(below.size.x * shrink, rise, below.size.z * shrink),
                                ..below
                            };
                            out.push(next);
                            top += rise;
                            below = next;
                        }
                    }
                }
                // Alleys between them, and now and then a square.
                let gap = if rng.chance(0.12) { rng.range_f32(18.0, 32.0) } else { rng.range_f32(2.5, 7.0) };
                s += width + gap;
            }
        }
    }
    // The port's sheds and warehouses, both sides.
    for side in [-1.0f32, 1.0] {
        let mut s = 0.0;
        while s < road.length() {
            if road.at(s).section != Section::Port {
                s += 10.0;
                continue;
            }
            let size = Vec3::new(rng.range_f32(30.0, 70.0), rng.range_f32(8.0, 16.0), rng.range_f32(18.0, 32.0));
            let lateral = 24.0 + rng.range_f32(0.0, 30.0);
            if rng.chance(0.55)
                && let Some((base, yaw)) = place(road, terrain, s + size.x * 0.5, side, lateral, size)
            {
                let tint = tint_of(style::SHED, &mut rng);
                out.push(Block { base, yaw, size, style: style::SHED, tint, seed: rng.next_f32() });
            }
            s += size.x + rng.range_f32(10.0, 40.0);
        }
    }
    out
}

/// A wall colour for `style`.
fn tint_of(style: u8, rng: &mut Rng) -> Vec3 {
    let pick = |rng: &mut Rng, colors: &[Vec3]| colors[rng.below_usize(colors.len())] * rng.range_f32(0.85, 1.1);
    match style {
        style::GLASS => {
            pick(rng, &[Vec3::new(0.30, 0.42, 0.48), Vec3::new(0.22, 0.30, 0.38), Vec3::new(0.35, 0.38, 0.36)])
        }
        style::OFFICE => {
            pick(rng, &[Vec3::new(0.40, 0.39, 0.36), Vec3::new(0.30, 0.31, 0.32), Vec3::new(0.48, 0.44, 0.38)])
        }
        style::BRICK => {
            pick(rng, &[Vec3::new(0.36, 0.14, 0.08), Vec3::new(0.30, 0.18, 0.12), Vec3::new(0.42, 0.26, 0.16)])
        }
        style::FLATS => {
            pick(rng, &[Vec3::new(0.62, 0.58, 0.50), Vec3::new(0.55, 0.50, 0.44), Vec3::new(0.46, 0.50, 0.52)])
        }
        style::SHED => pick(
            rng,
            &[
                Vec3::new(0.30, 0.34, 0.38),
                Vec3::new(0.45, 0.18, 0.12),
                Vec3::new(0.20, 0.32, 0.24),
                Vec3::new(0.50, 0.48, 0.44),
            ],
        ),
        _ => pick(rng, &[Vec3::new(0.50, 0.46, 0.40), Vec3::new(0.42, 0.40, 0.38)]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::VEDA_BAY;

    #[test]
    fn buildings_stand_clear_of_the_road_and_the_water() {
        let road = Road::new(VEDA_BAY);
        let terrain = Terrain::new(&road, 7);
        let blocks = buildings(&road, &terrain, 7);
        std::println!("{} blocks", blocks.len());
        assert!(blocks.len() > 150, "only {} blocks", blocks.len());
        assert!(blocks.iter().any(|b| b.base.y + b.size.y > 100.0), "no towers");
        for b in &blocks {
            for c in b.corners() {
                let at = road.locate(Vec3::new(c.x, 0.0, c.y), None);
                assert!(at.lateral.abs() > road.sample(at.index as isize).wall, "a building on the road at {c:?}");
            }
            assert!(b.style < style::COUNT);
        }
    }
}
