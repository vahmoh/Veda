// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The road's meshes: the tarmac, and what lines it (kerbs and pavements,
//! barriers, verges and guardrails, a tunnel's walls and roof), swept
//! along the centre line in chunks that can be culled one by one.
//!
//! The tarmac's texture coordinates are road coordinates, `(lateral, s)`,
//! from which its program paints the markings. Everything else carries a
//! material in its parameters (see [`material`]) and texture coordinates
//! in metres.

use alloc::vec::Vec;

use crate::prelude::*;

use crate::mesh::{Mesh, Params};
use crate::road::{EDGE, KERB, MEDIAN, Road, Section};

/// Samples per chunk.
pub const CHUNK: usize = 48;

/// Materials of the things beside the road (the first parameter byte):
/// layers of the surface texture array.
pub mod material {
    pub const CONCRETE: u8 = 0;
    pub const PAVING: u8 = 1;
    pub const KERB: u8 = 2;
    pub const METAL: u8 = 3;
    pub const GRAVEL: u8 = 4;
    pub const TILES: u8 = 5;
    pub const PAINTED: u8 = 6;
    pub const DARK: u8 = 7;
    /// How many there are.
    pub const COUNT: usize = 8;
}

/// A chunk of road: its tarmac and its sides, with bounds for culling.
pub struct Chunk {
    /// The tarmac (road coordinates as texture coordinates).
    pub tarmac: Mesh,
    /// Kerbs, pavements, barriers, verges, rails, tunnel walls.
    pub sides: Mesh,
    /// Bounding sphere.
    pub center: Vec3,
    pub radius: f32,
}

/// How exposed to the sky the road is at sample `i` (255 open, low in a
/// tunnel), as the shade parameter.
fn shade(road: &Road, depth: &[f32], i: usize) -> u8 {
    let s = road.samples()[i];
    if s.section == Section::Tunnel {
        // Daylight reaches 30 m in.
        let t = (depth[i] / 30.0).clamp(0.0, 1.0);
        (255.0 - t * 225.0) as u8
    } else {
        255
    }
}

/// The road's chunks.
pub fn chunks(road: &Road) -> Vec<Chunk> {
    let depth = road.section_depths();
    let n = road.len();
    let mut out = Vec::new();
    let mut start = 0;
    while start < n {
        let end = (start + CHUNK).min(n);
        out.push(chunk(road, &depth, start, end));
        start = end;
    }
    out
}

/// One cross-section point of a strip: lateral offset and height above
/// the road's surface.
#[derive(Clone, Copy)]
struct Point {
    lateral: f32,
    height: f32,
}

const fn pt(lateral: f32, height: f32) -> Point {
    Point { lateral, height }
}

/// Sweeps `profile` along samples `start..=end` (the last one shared with
/// the next chunk) into `mesh`. A profile goes round its solid with the
/// outside on its left (seen in the lateral-up plane): each segment faces
/// that way, as its own band (hard edges between them). `v` runs along
/// the road in metres, `u` along the profile.
fn sweep(mesh: &mut Mesh, road: &Road, start: usize, end: usize, profile: &[Point], params: &dyn Fn(usize) -> Params) {
    if profile.len() < 2 {
        return;
    }
    for seg in profile.windows(2) {
        let (a, b) = (seg[0], seg[1]);
        let dir = Vec2::new(b.lateral - a.lateral, b.height - a.height);
        let (u0, u1) = (0.0, dir.length());
        if u1 < 1e-5 {
            continue;
        }
        // The outside, on the left.
        let out = Vec2::new(-dir.y, dir.x) / u1;
        let base = mesh.base();
        for i in start..=end {
            let s = road.sample(i as isize);
            let v = i as f32 * road.spacing();
            let p = |q: Point| s.pos + s.right * q.lateral + s.up * q.height;
            let normal = (s.right * out.x + s.up * out.y).normalize_or(s.up);
            let par = params(i % road.len());
            mesh.push(p(a), normal, Vec2::new(u0, v), par);
            mesh.push(p(b), normal, Vec2::new(u1, v), par);
        }
        for k in 0..(end - start) as u32 {
            let (i0, i1) = (base + k * 2, base + k * 2 + 2);
            // Across then forward turns counter-clockwise seen from the
            // left of the profile's direction.
            mesh.quad(i0, i0 + 1, i1 + 1, i1);
        }
    }
}

/// The runs of samples in `start..end` where `keep` holds, as `(first,
/// last)` pairs (the last sample shared with the next run's neighbour).
fn runs(start: usize, end: usize, keep: &dyn Fn(usize) -> bool) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = start;
    while i < end {
        if !keep(i) {
            i += 1;
            continue;
        }
        let first = i;
        while i < end && keep(i) {
            i += 1;
        }
        out.push((first, i));
    }
    out
}

fn chunk(road: &Road, depth: &[f32], start: usize, end: usize) -> Chunk {
    let n = road.len();
    let sample = |i: usize| road.samples()[i % n];
    let mut tarmac = Mesh::new();
    let mut sides = Mesh::new();

    // The tarmac, from shoulder to shoulder, with breaks at the lane edges
    // and the middle so that banking and the barrier stay true.
    {
        let base = tarmac.base();
        for i in start..=end {
            let s = sample(i);
            let along = i as f32 * road.spacing();
            let sh = shade(road, depth, i % n);
            for lateral in [-s.paved, -EDGE, -MEDIAN, MEDIAN, EDGE, s.paved] {
                let p = s.pos + s.right * lateral;
                tarmac.push(p, s.up, Vec2::new(lateral, along), [0, sh, 0, 0]);
            }
        }
        for k in 0..(end - start) as u32 {
            for c in 0..5u32 {
                let (a, b) = (base + k * 6 + c, base + (k + 1) * 6 + c);
                tarmac.quad(a, a + 1, b + 1, b);
            }
        }
    }

    let params = |m: u8| move |i: usize| -> Params { [m, shade(road, depth, i), 0, 0] };

    // The median barrier where the road is divided: a concrete safety
    // barrier's profile.
    for (a, b) in runs(start, end, &|i| sample(i).median >= 0.5) {
        let barrier = [
            pt(-0.30, 0.0),
            pt(-0.24, 0.08),
            pt(-0.10, 0.32),
            pt(-0.08, 0.82),
            pt(0.08, 0.82),
            pt(0.10, 0.32),
            pt(0.24, 0.08),
            pt(0.30, 0.0),
        ];
        sweep(&mut sides, road, a, b, &barrier, &params(material::CONCRETE));
    }

    // Each side.
    for side in [-1.0f32, 1.0] {
        // The profiles are drawn for the right side; on the left, mirrored
        // and reversed, so that their outsides stay on their left.
        let mirror = |p: &[Point]| -> Vec<Point> {
            let mut m: Vec<Point> = p.iter().map(|q| Point { lateral: q.lateral * side, height: q.height }).collect();
            if side < 0.0 {
                m.reverse();
            }
            m
        };
        // Downtown: a kerb and a pavement to the buildings.
        for (a, b) in runs(start, end, &|i| sample(i).pavement >= 0.5) {
            let (paved, wall) = (sample(a).paved, sample(a).wall);
            let kerb = [pt(paved, 0.0), pt(paved, KERB)];
            sweep(&mut sides, road, a, b, &mirror(&kerb), &params(material::KERB));
            let walk = [pt(paved, KERB), pt(paved + 0.35, KERB)];
            sweep(&mut sides, road, a, b, &mirror(&walk), &params(material::KERB));
            let walk = [pt(paved + 0.35, KERB), pt(wall + 1.0, KERB)];
            sweep(&mut sides, road, a, b, &mirror(&walk), &params(material::PAVING));
        }
        // In a tunnel: its wall, a walkway and the roof.
        for (a, b) in runs(start, end, &|i| sample(i).section == Section::Tunnel && depth[i % n] >= 20.0) {
            let (paved, wall) = (sample(a).paved, sample(a).wall);
            let walkway = [pt(paved, 0.0), pt(paved, 0.25), pt(paved, 0.25), pt(wall, 0.25)];
            sweep(&mut sides, road, a, b, &mirror(&walkway), &params(material::CONCRETE));
            let wall_profile = [pt(wall, 0.25), pt(wall, 4.2), pt(wall - 1.2, 6.2), pt(wall - 3.4, 7.2), pt(0.0, 7.6)];
            sweep(&mut sides, road, a, b, &mirror(&wall_profile[..2]), &params(material::TILES));
            sweep(&mut sides, road, a, b, &mirror(&wall_profile[1..]), &params(material::DARK));
        }
        // Elsewhere off the tarmac: a verge, and a guardrail on posts.
        for (a, b) in runs(start, end, &|i| {
            let s = sample(i);
            s.pavement < 0.5 && !(s.section == Section::Tunnel && depth[i % n] >= 20.0)
        }) {
            let (paved, wall) = (sample(a).paved, sample(a).wall);
            let bridge = sample(a).section == Section::Bridge;
            if bridge {
                // A concrete parapet.
                let parapet = [
                    pt(paved, 0.0),
                    pt(paved, 0.25),
                    pt(wall - 0.25, 1.05),
                    pt(wall, 1.1),
                    pt(wall + 0.2, 1.1),
                    pt(wall + 0.35, 0.0),
                    pt(wall + 0.35, -1.6),
                ];
                sweep(&mut sides, road, a, b, &mirror(&parapet), &params(material::CONCRETE));
                continue;
            }
            let verge = [pt(paved, 0.0), pt(wall + 2.5, -0.5)];
            sweep(&mut sides, road, a, b, &mirror(&verge), &params(material::GRAVEL));
            let rail_at = wall - 0.4;
            let rail = [
                pt(rail_at, 0.42),
                pt(rail_at - 0.12, 0.55),
                pt(rail_at, 0.62),
                pt(rail_at - 0.12, 0.70),
                pt(rail_at, 0.80),
                pt(rail_at + 0.06, 0.80),
                pt(rail_at + 0.06, 0.42),
                pt(rail_at, 0.42),
            ];
            sweep(&mut sides, road, a, b, &mirror(&rail), &params(material::METAL));
        }
    }

    // Bounds.
    let (lo, hi) = {
        let (a, b) = tarmac.bounds();
        let (c, d) = if sides.is_empty() { (a, b) } else { sides.bounds() };
        (a.min(c), b.max(d))
    };
    let center = (lo + hi) * 0.5;
    Chunk { tarmac, sides, center, radius: (hi - lo).length() * 0.5 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::VEDA_BAY;

    #[test]
    fn chunks_cover_the_road_and_face_up() {
        let road = Road::new(VEDA_BAY);
        let chunks = chunks(&road);
        assert_eq!(chunks.len(), road.len().div_ceil(CHUNK));
        let quads: usize = chunks.iter().map(|c| c.tarmac.indices.len() / 6).sum();
        assert_eq!(quads, road.len() * 5);
        for c in &chunks {
            for t in c.tarmac.indices.chunks(3) {
                let (a, b, cc) = (
                    c.tarmac.vertices[t[0] as usize],
                    c.tarmac.vertices[t[1] as usize],
                    c.tarmac.vertices[t[2] as usize],
                );
                let face = (b.pos - a.pos).cross(cc.pos - a.pos);
                assert!(face.y > 0.0, "the tarmac faces down");
            }
            assert!(!c.sides.is_empty());
            // Every side face faces the way its vertices' normals say.
            for t in c.sides.indices.chunks(3) {
                let (a, b, cc) =
                    (c.sides.vertices[t[0] as usize], c.sides.vertices[t[1] as usize], c.sides.vertices[t[2] as usize]);
                let face = (b.pos - a.pos).cross(cc.pos - a.pos);
                if face.length() > 1e-6 {
                    assert!(face.normalize().dot(a.normal) > 0.0, "a side face facing away from its normal");
                }
            }
            for v in c.tarmac.vertices.iter().chain(&c.sides.vertices) {
                assert!(v.pos.distance(c.center) <= c.radius + 0.01);
            }
        }
    }
}
