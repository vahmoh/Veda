//! The land and the sea around the road: a height field on a grid.
//!
//! Veda Bay lies inside the loop: the city on its north shore, the hills
//! on its east, the headland and the coast on its south, the port on its
//! west, and its mouth, which the bridge crosses, in the south-west. The
//! ocean washes the cliffs below the coast road and the port's quays.
//! Mountains rise to the east and north-east.
//!
//! Each point's height comes from where it is relative to the road (its
//! distance from the centre line, the side it is on, and what the nearest
//! stretch of road is) blended with regional shapes and noise: near the
//! road the ground meets it in cuttings and embankments; further away it
//! takes its own shape; past the shore lines it falls into the water. The
//! sea's surface is at height 0.

use alloc::vec;
use alloc::vec::Vec;

use fastnoise_lite::{FastNoiseLite, FractalType, NoiseType};

use crate::prelude::*;

use crate::road::{Location, Road, Section};

/// The grid's cell size (m).
pub const CELL: f32 = 8.0;
/// The world's extent: x from `-HALF_X` to `HALF_X`, z likewise.
pub const HALF_X: f32 = 2400.0;
pub const HALF_Z: f32 = 2000.0;
/// How far below the road's surface the ground under it lies (the road's
/// own mesh is the surface there).
const UNDER_ROAD: f32 = 0.7;

/// What the ground is like on one side of a stretch of road.
#[derive(Clone, Copy, Debug, Default)]
struct Side {
    /// The water begins this far from the road (very far: none).
    coast: f32,
    /// How rugged the land is (noise amplitude, m).
    rough: f32,
    /// How much the land rises (or falls) away from the road.
    rise: f32,
    /// Over how many metres the land falls to the water: a beach, or a
    /// cliff or a quay.
    shelf: f32,
    /// How much the shore line wanders (m).
    wobble: f32,
}

impl Side {
    const fn new(coast: f32, rough: f32, rise: f32, shelf: f32, wobble: f32) -> Side {
        Side { coast, rough, rise, shelf, wobble }
    }

    fn lerp(&self, o: &Side, t: f32) -> Side {
        let l = |a: f32, b: f32| a + (b - a) * t;
        Side {
            coast: l(self.coast, o.coast),
            rough: l(self.rough, o.rough),
            rise: l(self.rise, o.rise),
            shelf: l(self.shelf, o.shelf),
            wobble: l(self.wobble, o.wobble),
        }
    }
}

/// What the ground is like either side of a stretch of road (eased along
/// the road, so that districts change gradually).
#[derive(Clone, Copy, Debug, Default)]
struct Shore {
    /// Inside the loop (towards the bay) and outside.
    inside: Side,
    outside: Side,
    /// 1 inside a tunnel (the hill stays over it), 0 outside, a short ramp
    /// at its mouths.
    tunnel: f32,
    /// 1 on a bridge (water under it), 0 off it and on its abutments.
    bridge: f32,
}

fn shore_of(section: Section) -> Shore {
    // No water on that side.
    const FAR: f32 = 5000.0;
    let (inside, outside) = match section {
        // A waterfront of quays, and the city beyond the avenue.
        Section::City => (Side::new(230.0, 0.6, -3.0, 6.0, 8.0), Side::new(FAR, 1.0, 5.0, 40.0, 0.0)),
        // Slopes down to the bay's beaches; up into the mountains.
        Section::Hills => (Side::new(260.0, 10.0, -12.0, 70.0, 40.0), Side::new(FAR, 28.0, 70.0, 40.0, 0.0)),
        // The headland the tunnel goes through.
        Section::Tunnel => (Side::new(300.0, 14.0, 45.0, 30.0, 30.0), Side::new(180.0, 14.0, 40.0, 14.0, 25.0)),
        // The headland's slopes to the bay; cliffs to the ocean.
        Section::Coast => (Side::new(230.0, 10.0, 28.0, 50.0, 35.0), Side::new(55.0, 4.0, 2.0, 12.0, 18.0)),
        // Water under the deck.
        Section::Bridge => (Side::new(0.0, 0.0, 0.0, 4.0, 0.0), Side::new(0.0, 0.0, 0.0, 4.0, 0.0)),
        // Quays either side of the port.
        Section::Port => (Side::new(140.0, 0.4, -1.0, 4.0, 4.0), Side::new(150.0, 0.4, -1.0, 4.0, 4.0)),
    };
    Shore { inside, outside, tunnel: 0.0, bridge: 0.0 }
}

fn lerp_shore(a: &Shore, b: &Shore, t: f32) -> Shore {
    Shore {
        inside: a.inside.lerp(&b.inside, t),
        outside: a.outside.lerp(&b.outside, t),
        tunnel: a.tunnel + (b.tunnel - a.tunnel) * t,
        bridge: a.bridge + (b.bridge - a.bridge) * t,
    }
}

/// The height field.
pub struct Terrain {
    /// Vertices per row and per column.
    pub w: usize,
    pub h: usize,
    heights: Vec<f32>,
    /// For each vertex, the road sample nearest it.
    nearest: Vec<u32>,
}

impl Terrain {
    /// The world's corner (the first vertex).
    pub const ORIGIN: Vec2 = Vec2::new(-HALF_X, -HALF_Z);

    /// Shapes the land around `road`.
    pub fn new(road: &Road, seed: u64) -> Terrain {
        let w = (2.0 * HALF_X / CELL) as usize + 1;
        let h = (2.0 * HALF_Z / CELL) as usize + 1;
        let nearest = nearest_samples(road, w, h);
        let inside = inside_loop(road, w, h);
        let shores = shores_along(road);
        let relief = Relief::new(seed);
        // Where each vertex is relative to the road, and what the ground
        // is like there as the nearest stretch of road has it. Those fields
        // change abruptly where another stretch becomes the nearest, so
        // they are blurred before the heights are made of them.
        let mut at = Vec::with_capacity(w * h);
        let mut fields: [Vec<f32>; FIELDS] = core::array::from_fn(|_| Vec::with_capacity(w * h));
        for j in 0..h {
            for i in 0..w {
                let k = j * w + i;
                let p = Vec3::new(-HALF_X + i as f32 * CELL, 0.0, -HALF_Z + j as f32 * CELL);
                let loc = road.locate_from(p, nearest[k] as usize);
                let shore = &shores[loc.index];
                let side = if inside[k] { &shore.inside } else { &shore.outside };
                let values = [loc.height, side.coast, side.rough, side.rise, side.shelf, side.wobble];
                for (f, v) in fields.iter_mut().zip(values) {
                    f.push(v);
                }
                at.push(loc);
            }
        }
        for f in &mut fields {
            blur(f, w, h, 10);
            blur(f, w, h, 10);
        }
        let mut heights = vec![0.0f32; w * h];
        for (k, height) in heights.iter_mut().enumerate() {
            let p = Vec2::new(-HALF_X + (k % w) as f32 * CELL, -HALF_Z + (k / w) as f32 * CELL);
            let values: [f32; FIELDS] = core::array::from_fn(|f| fields[f][k]);
            *height = natural(road, &shores[at[k].index], &relief, p, &at[k], &values, inside[k]);
        }
        Terrain { w, h, heights, nearest }
    }

    /// The vertex `(i, j)`'s height.
    pub fn vertex(&self, i: usize, j: usize) -> f32 {
        self.heights[j.min(self.h - 1) * self.w + i.min(self.w - 1)]
    }

    /// The road sample nearest vertex `(i, j)`.
    pub fn nearest(&self, i: usize, j: usize) -> usize {
        self.nearest[j.min(self.h - 1) * self.w + i.min(self.w - 1)] as usize
    }

    /// The road sample nearest `(x, z)` (as near as its cell's corner's).
    pub fn nearest_sample(&self, x: f32, z: f32) -> usize {
        let i = ((x + HALF_X) / CELL).round().max(0.0) as usize;
        let j = ((z + HALF_Z) / CELL).round().max(0.0) as usize;
        self.nearest(i, j)
    }

    /// The height at `(x, z)`, interpolated as the mesh's triangles are.
    pub fn height(&self, x: f32, z: f32) -> f32 {
        let fx = ((x + HALF_X) / CELL).clamp(0.0, (self.w - 1) as f32 - 1e-3);
        let fz = ((z + HALF_Z) / CELL).clamp(0.0, (self.h - 1) as f32 - 1e-3);
        let (i, j) = (fx as usize, fz as usize);
        let (u, v) = (fx - i as f32, fz - j as f32);
        let (h00, h10, h01, h11) =
            (self.vertex(i, j), self.vertex(i + 1, j), self.vertex(i, j + 1), self.vertex(i + 1, j + 1));
        // The cell's two triangles share the diagonal from (0,0) to (1,1).
        if u >= v { h00 + (h10 - h00) * u + (h11 - h10) * v } else { h00 + (h11 - h01) * u + (h01 - h00) * v }
    }

    /// The surface normal at `(x, z)`.
    pub fn normal(&self, x: f32, z: f32) -> Vec3 {
        let e = CELL * 0.5;
        let dx = self.height(x + e, z) - self.height(x - e, z);
        let dz = self.height(x, z + e) - self.height(x, z - e);
        Vec3::new(-dx, 2.0 * e, -dz).normalize_or(Vec3::Y)
    }

    /// Whether `(x, z)` is under the sea.
    pub fn is_water(&self, x: f32, z: f32) -> bool {
        self.height(x, z) < 0.0
    }
}

/// The road's shores, sample by sample, eased along the road.
fn shores_along(road: &Road) -> Vec<Shore> {
    let n = road.len();
    let raw: Vec<Shore> = road.samples().iter().map(|s| shore_of(s.section)).collect();
    // A box filter over 120 m, twice: changes of district are gradual.
    let radius = (120.0 / road.spacing()) as usize;
    let mut cur = raw;
    for _ in 0..2 {
        let mut next = Vec::with_capacity(n);
        for i in 0..n {
            let mut acc = Shore::default();
            let mut count = 0.0;
            for d in 0..=2 * radius {
                let s = &cur[(i + n * 4 + d - radius) % n];
                count += 1.0;
                acc = lerp_shore(&acc, s, 1.0 / count);
            }
            next.push(acc);
        }
        cur = next;
    }
    // Tunnels and bridges have exact ends: the hill covers a tunnel from
    // 20 m inside its mouths, and a bridge stands on land for its first
    // and last 30 m.
    let depth = road.section_depths();
    for (i, sh) in cur.iter_mut().enumerate() {
        let s = road.samples()[i].section;
        sh.tunnel = if s == Section::Tunnel { depth[i].smoothstep(14.0, 26.0) } else { 0.0 };
        sh.bridge = if s == Section::Bridge { depth[i].smoothstep(24.0, 40.0) } else { 0.0 };
    }
    cur
}

/// How many fields [`Terrain::new`] blurs: the road's height, then the
/// [`Side`]'s coast, roughness, rise, shelf and wobble.
const FIELDS: usize = 6;

/// The noise the land is shaped with: fractal Perlin noise, each octave at
/// twice the frequency and half the amplitude of the one before.
struct Relief {
    /// Rolling hills, about 290 m across.
    hills: FastNoiseLite,
    /// Ridges about 830 m apart.
    ridges: FastNoiseLite,
    /// The wobble of the shore lines, about 250 m long.
    shore: FastNoiseLite,
}

impl Relief {
    fn new(seed: u64) -> Relief {
        // A field's octaves take consecutive seeds from its own.
        let seed = seed as i32;
        Relief {
            hills: fractal(seed, FractalType::FBm, 0.0035, 5, 2.0),
            ridges: fractal(seed.wrapping_add(100), FractalType::Ridged, 0.0012, 4, 2.1),
            shore: fractal(seed.wrapping_add(200), FractalType::FBm, 0.004, 4, 2.0),
        }
    }

    /// The hills at `p`, about -1 to 1.
    fn hills(&self, p: Vec2) -> f32 {
        self.hills.get_noise_2d(p.x, p.y)
    }

    /// The ridges at `p`, 0 to 1: 1 on their crests, falling away sharply.
    fn ridges(&self, p: Vec2) -> f32 {
        let r = 0.5 * (self.ridges.get_noise_2d(p.x, p.y) + 1.0);
        r * r
    }

    /// How far the shore lines move in and out at `p`, about -1 to 1.
    fn shore(&self, p: Vec2) -> f32 {
        self.shore.get_noise_2d(p.x, p.y)
    }
}

/// Fractal Perlin noise of `octaves` octaves at `frequency` (per metre).
fn fractal(seed: i32, kind: FractalType, frequency: f32, octaves: i32, lacunarity: f32) -> FastNoiseLite {
    let mut noise = FastNoiseLite::with_seed(seed);
    noise.set_noise_type(Some(NoiseType::Perlin));
    noise.set_frequency(Some(frequency));
    noise.set_fractal_type(Some(kind));
    noise.set_fractal_octaves(Some(octaves));
    noise.set_fractal_lacunarity(Some(lacunarity));
    noise.set_fractal_gain(Some(0.5));
    noise
}

/// The natural height at `p`, which is at `at` relative to the road, with
/// the blurred fields `f` and the nearest stretch's `shore`.
fn natural(
    road: &Road,
    shore: &Shore,
    relief: &Relief,
    p: Vec2,
    at: &Location,
    f: &[f32; FIELDS],
    inside: bool,
) -> f32 {
    let [base, coast, rough, rise, shelf, wobble] = *f;
    // Under a bridge's span, water right up to it.
    let coast = coast * (1.0 - shore.bridge);
    let d = at.lateral.abs();
    let wall = road.sample(at.index as isize).wall;

    // Land: from the road's height, rising (or falling) away from it, with
    // features that grow with the distance.
    let away = d.smoothstep(wall, wall + 160.0);
    let n1 = relief.hills(p);
    let n2 = relief.ridges(p);
    let mut land = base + rise * away + rough * away * (n1 + 0.6 * n2);
    // Mountains to the east and the north, far from the road.
    if !inside {
        let far = d.smoothstep(150.0, 700.0);
        let east = p.x.smoothstep(900.0, 1800.0);
        let north = 1.0 - p.y.smoothstep(-1700.0, -900.0);
        land += (east * 260.0 + north * 140.0) * (0.55 + 0.45 * n2) * far;
    }

    // The shore: the land falls to the sea over its shelf, then the sea
    // floor deepens away from it, to a depth of 12 m.
    let edge = coast - d + relief.shore(p) * wobble;
    let mut h = if edge > 0.0 {
        let fall = 1.0 - edge.smoothstep(0.0, shelf.max(1.0));
        land + (1.0 - land) * fall
    } else {
        -1.5 - (-edge).min(150.0) * 0.07
    };

    // The road's corridor: the ground under the road, and cuttings and
    // embankments either side, except over a tunnel and under a bridge.
    let road_h = at.height;
    let corridor = 1.0 - d.smoothstep(wall + 2.0, wall + 28.0);
    let open = (1.0 - shore.tunnel) * (1.0 - shore.bridge);
    if corridor > 0.0 && open > 0.0 {
        let under = road_h - UNDER_ROAD;
        h += (under - h) * corridor * open;
    }
    // Over a tunnel the hill stays well above its roof.
    let over = shore.tunnel * (1.0 - d.smoothstep(30.0, 140.0));
    if over > 0.0 {
        let cover = road_h + 18.0 + 20.0 * d.smoothstep(0.0, 100.0);
        let under = road_h - UNDER_ROAD;
        h = h.max(under + (cover - under) * over);
    }
    h
}

/// Blurs the `w` x `h` field `f` with a box `2 * r + 1` cells wide, along
/// rows and then columns (the edges repeat).
fn blur(f: &mut [f32], w: usize, h: usize, r: usize) {
    let mut line = Vec::new();
    let mut pass = |f: &mut [f32], len: usize, count: usize, at: &dyn Fn(usize, usize) -> usize| {
        for c in 0..count {
            line.clear();
            line.extend((0..len).map(|k| f[at(c, k)]));
            let get = |k: isize| line[k.clamp(0, len as isize - 1) as usize];
            let mut sum: f32 = (-(r as isize)..=r as isize).map(get).sum();
            let width = (2 * r + 1) as f32;
            for k in 0..len {
                f[at(c, k)] = sum / width;
                sum += get(k as isize + r as isize + 1) - get(k as isize - r as isize);
            }
        }
    };
    pass(f, w, h, &|row, k| row * w + k);
    pass(f, h, w, &|col, k| k * w + col);
}

/// For each vertex of a `w` x `h` grid, the nearest road sample: seeded on
/// the road, then spread to every cell in two sweeps, each cell keeping
/// the nearest of its neighbours' samples.
fn nearest_samples(road: &Road, w: usize, h: usize) -> Vec<u32> {
    let none = u32::MAX;
    let mut best = vec![none; w * h];
    let mut dist = vec![f32::MAX; w * h];
    let pos = |k: usize| Vec2::new(-HALF_X + (k % w) as f32 * CELL, -HALF_Z + (k / w) as f32 * CELL);
    let point = |i: u32| road.samples()[i as usize].pos.xz();
    for (i, s) in road.samples().iter().enumerate() {
        let gi = ((s.pos.x + HALF_X) / CELL).round();
        let gj = ((s.pos.z + HALF_Z) / CELL).round();
        if gi < 0.0 || gj < 0.0 || gi >= w as f32 || gj >= h as f32 {
            continue;
        }
        let k = gj as usize * w + gi as usize;
        let d = pos(k).distance_squared(s.pos.xz());
        if d < dist[k] {
            dist[k] = d;
            best[k] = i as u32;
        }
    }
    let relax = |k: usize, from: usize, best: &mut Vec<u32>, dist: &mut Vec<f32>| {
        let b = best[from];
        if b != none {
            let d = pos(k).distance_squared(point(b));
            if d < dist[k] {
                dist[k] = d;
                best[k] = b;
            }
        }
    };
    for _ in 0..2 {
        for j in 0..h {
            for i in 0..w {
                let k = j * w + i;
                if i > 0 {
                    relax(k, k - 1, &mut best, &mut dist);
                }
                if j > 0 {
                    relax(k, k - w, &mut best, &mut dist);
                    if i > 0 {
                        relax(k, k - w - 1, &mut best, &mut dist);
                    }
                    if i + 1 < w {
                        relax(k, k - w + 1, &mut best, &mut dist);
                    }
                }
            }
            for i in (0..w.saturating_sub(1)).rev() {
                let k = j * w + i;
                relax(k, k + 1, &mut best, &mut dist);
            }
        }
        for j in (0..h).rev() {
            for i in (0..w).rev() {
                let k = j * w + i;
                if i + 1 < w {
                    relax(k, k + 1, &mut best, &mut dist);
                }
                if j + 1 < h {
                    relax(k, k + w, &mut best, &mut dist);
                    if i + 1 < w {
                        relax(k, k + w + 1, &mut best, &mut dist);
                    }
                    if i > 0 {
                        relax(k, k + w - 1, &mut best, &mut dist);
                    }
                }
            }
            for i in 1..w {
                let k = j * w + i;
                relax(k, k - 1, &mut best, &mut dist);
            }
        }
    }
    best.iter().map(|&b| if b == none { 0 } else { b }).collect()
}

/// For each vertex of the grid, whether it is inside the loop: the
/// crossings of the centre line with each row, counted from the left.
fn inside_loop(road: &Road, w: usize, h: usize) -> Vec<bool> {
    let mut inside = vec![false; w * h];
    let n = road.len();
    let mut xs = Vec::new();
    for j in 0..h {
        let z = -HALF_Z + j as f32 * CELL;
        xs.clear();
        for i in 0..n {
            let (a, b) = (road.samples()[i].pos, road.samples()[(i + 1) % n].pos);
            if (a.z <= z) != (b.z <= z) {
                let t = (z - a.z) / (b.z - a.z);
                xs.push(a.x + (b.x - a.x) * t);
            }
        }
        xs.sort_unstable_by(f32::total_cmp);
        for pair in xs.chunks(2) {
            if let [x0, x1] = pair {
                let i0 = ((x0 + HALF_X) / CELL).ceil().max(0.0) as usize;
                let i1 = ((x1 + HALF_X) / CELL).floor().min((w - 1) as f32);
                if i1 < 0.0 {
                    continue;
                }
                for i in i0..=(i1 as usize).min(w - 1) {
                    inside[j * w + i] = true;
                }
            }
        }
    }
    inside
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::VEDA_BAY;

    #[test]
    fn the_ground_meets_the_road_and_the_bay_is_water() {
        let road = Road::new(VEDA_BAY);
        let t = Terrain::new(&road, 7);
        // Under the road (but in the tunnel and on the bridge) the ground
        // is just below the surface.
        for (k, s) in road.samples().iter().enumerate().step_by(5) {
            if matches!(s.section, Section::Tunnel | Section::Bridge) {
                continue;
            }
            for lateral in [-6.0, 0.0, 6.0] {
                let p = road.point(k as f32 * road.spacing(), lateral);
                let g = t.height(p.x, p.z);
                assert!(
                    g < p.y - 0.2 && g > p.y - 3.0,
                    "sample {k} ({:?}): ground {g:.1} under road {:.1}",
                    s.section,
                    p.y
                );
            }
        }
        // The middle of the bay is water, the city north of the avenue
        // land, the ocean south of the coast water.
        assert!(t.is_water(0.0, 100.0));
        assert!(!t.is_water(0.0, -900.0));
        assert!(t.is_water(300.0, 1200.0));
        // Over the tunnel there is a hill.
        let mid = road.samples().iter().position(|s| s.section == Section::Tunnel).unwrap() + 60;
        let p = road.samples()[mid].pos;
        assert!(t.height(p.x, p.z) > p.y + 12.0, "only {:.1} m over the tunnel", t.height(p.x, p.z) - p.y);
    }
}
