//! The ground's and the sea's meshes.
//!
//! The ground is cut into square chunks, each drawn at full detail near
//! the camera or at a quarter of it further away; a skirt hanging from
//! each chunk's edges hides the cracks where the two meet. The sea is one
//! mesh: a grid over the world where the water is, its vertices carrying
//! the depth (for the shallows' colour and foam), and a ring of large
//! quads out to the horizon.

use alloc::vec::Vec;

use crate::prelude::*;

use crate::mesh::Mesh;
use crate::terrain::{CELL, HALF_X, HALF_Z, Terrain};

/// Cells per chunk side.
pub const CHUNK_CELLS: usize = 32;
/// The coarse level's step (vertices).
pub const COARSE: usize = 4;
/// How far the skirts hang (m).
const SKIRT: f32 = 12.0;

/// A chunk of ground: vertices, and the indices of its two levels of
/// detail.
pub struct GroundChunk {
    pub mesh: Mesh,
    /// Indices of the fine and the coarse level (skirts included).
    pub fine: Vec<u32>,
    pub coarse: Vec<u32>,
    pub center: Vec3,
    pub radius: f32,
}

/// The ground in chunks.
pub fn ground_chunks(t: &Terrain) -> Vec<GroundChunk> {
    let mut out = Vec::new();
    let (cw, ch) = ((t.w - 1).div_ceil(CHUNK_CELLS), (t.h - 1).div_ceil(CHUNK_CELLS));
    for cj in 0..ch {
        for ci in 0..cw {
            out.push(chunk(t, ci * CHUNK_CELLS, cj * CHUNK_CELLS));
        }
    }
    out
}

fn chunk(t: &Terrain, i0: usize, j0: usize) -> GroundChunk {
    let i1 = (i0 + CHUNK_CELLS).min(t.w - 1);
    let j1 = (j0 + CHUNK_CELLS).min(t.h - 1);
    let (nw, nh) = (i1 - i0 + 1, j1 - j0 + 1);
    let mut mesh = Mesh::new();
    let world = |i: usize, j: usize| Vec2::new(-HALF_X + i as f32 * CELL, -HALF_Z + j as f32 * CELL);
    let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    for j in j0..=j1 {
        for i in i0..=i1 {
            let p = world(i, j);
            let pos = Vec3::new(p.x, t.vertex(i, j), p.y);
            lo = lo.min(pos);
            hi = hi.max(pos);
            mesh.push(pos, t.normal(p.x, p.y), Vec2::ZERO, [0, 255, 0, 0]);
        }
    }
    let at = |i: usize, j: usize| (j * nw + i) as u32;
    // The skirt: a copy of each edge vertex, lowered.
    let mut edge = Vec::new();
    for i in 0..nw {
        edge.push((i, 0));
    }
    for j in 1..nh {
        edge.push((nw - 1, j));
    }
    for i in (0..nw - 1).rev() {
        edge.push((i, nh - 1));
    }
    for j in (1..nh - 1).rev() {
        edge.push((0, j));
    }
    let skirt_base = mesh.base();
    for &(i, j) in &edge {
        let v = mesh.vertices[at(i, j) as usize];
        mesh.push(v.pos - Vec3::Y * SKIRT, v.normal, Vec2::ZERO, v.params);
    }
    let lowered = |k: usize| skirt_base + k as u32;
    let level = |step: usize| -> Vec<u32> {
        let mut idx = Vec::new();
        let mut j = 0;
        while j + 1 < nh {
            let jn = (j + step).min(nh - 1);
            let mut i = 0;
            while i + 1 < nw {
                let inx = (i + step).min(nw - 1);
                let (a, b, c, d) = (at(i, j), at(inx, j), at(inx, jn), at(i, jn));
                // Facing up: (i, j) -> (i, j+1) -> (i+1, j+1) is
                // counter-clockwise seen from above (x right, z down).
                idx.extend_from_slice(&[a, d, c, a, c, b]);
                i = inx;
            }
            j = jn;
        }
        // Skirts along the edge, at this level's step.
        let mut k = 0;
        while k < edge.len() {
            let kn = (k + step).min(edge.len());
            let (e0, e1) = (edge[k], edge[kn % edge.len()]);
            // Only between neighbours along the same side (the corners
            // join themselves).
            let (top0, top1) = (at(e0.0, e0.1), at(e1.0, e1.1));
            let (bot0, bot1) = (lowered(k), lowered(kn % edge.len()));
            // The edge runs clockwise seen from above (x right, z down);
            // the skirt faces out.
            idx.extend_from_slice(&[top0, bot1, bot0, top0, top1, bot1]);
            k = kn;
        }
        idx
    };
    let fine = level(1);
    let coarse = level(COARSE);
    let center = (lo + hi) * 0.5;
    GroundChunk { mesh, fine, coarse, center, radius: (hi - lo).length() * 0.5 + SKIRT }
}

/// How far the sea's ring reaches (m).
pub const HORIZON: f32 = 9000.0;

/// The sea: where the ground is below (or near) the water, plus a ring to
/// the horizon. Texture coordinate `u` is the depth.
pub fn sea(t: &Terrain) -> Mesh {
    let mut m = Mesh::new();
    let step = 4; // terrain cells per sea cell (32 m)
    let (w, h) = ((t.w - 1) / step, (t.h - 1) / step);
    let size = CELL * step as f32;
    let depth = |i: usize, j: usize| -t.vertex(i * step, j * step);
    let up = Vec3::Y;
    for j in 0..h {
        for i in 0..w {
            let corners = [(i, j), (i + 1, j), (i + 1, j + 1), (i, j + 1)];
            if corners.iter().all(|&(a, b)| depth(a, b) < -1.0) {
                // Dry land all round.
                continue;
            }
            let base = m.base();
            for (a, b) in corners {
                let p = Vec3::new(-HALF_X + a as f32 * size, 0.0, -HALF_Z + b as f32 * size);
                m.push(p, up, Vec2::new(depth(a, b), 0.0), [0; 4]);
            }
            // Facing up (x right, z down): 0 -> 3 -> 2 counter-clockwise.
            m.quad(base, base + 3, base + 2, base + 1);
        }
    }
    // The ring: the world's rectangle out to the horizon, deep.
    let (x0, x1, z0, z1) = (-HALF_X, HALF_X, -HALF_Z, HALF_Z);
    let deep = Vec2::new(30.0, 0.0);
    let rect = [
        (x0, z0, -HORIZON, -HORIZON),
        (x1, z0, HORIZON, -HORIZON),
        (x1, z1, HORIZON, HORIZON),
        (x0, z1, -HORIZON, HORIZON),
    ];
    for k in 0..4 {
        let (a, b) = (rect[k], rect[(k + 1) % 4]);
        let base = m.base();
        m.push(Vec3::new(a.0, 0.0, a.1), up, deep, [0; 4]);
        m.push(Vec3::new(b.0, 0.0, b.1), up, deep, [0; 4]);
        m.push(Vec3::new(b.2, 0.0, b.3), up, deep, [0; 4]);
        m.push(Vec3::new(a.2, 0.0, a.3), up, deep, [0; 4]);
        // Inner edge, then outer: counter-clockwise seen from above.
        m.quad(base, base + 1, base + 2, base + 3);
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::VEDA_BAY;
    use crate::road::Road;

    fn faces_up(m: &Mesh, idx: &[u32]) -> (usize, usize) {
        let (mut up, mut down) = (0, 0);
        for t in idx.chunks(3) {
            let (a, b, c) =
                (m.vertices[t[0] as usize].pos, m.vertices[t[1] as usize].pos, m.vertices[t[2] as usize].pos);
            let n = (b - a).cross(c - a);
            if n.y > 1e-3 {
                up += 1;
            } else if n.y < -1e-3 {
                down += 1;
            }
        }
        (up, down)
    }

    #[test]
    fn ground_and_sea_face_up() {
        let road = Road::new(VEDA_BAY);
        let t = Terrain::new(&road, 7);
        let chunks = ground_chunks(&t);
        assert!(chunks.len() > 100);
        for c in &chunks {
            for idx in [&c.fine, &c.coarse] {
                let (up, down) = faces_up(&c.mesh, idx);
                assert!(up > 0 && down == 0, "{up} up, {down} down");
            }
            assert!(c.coarse.len() < c.fine.len() / 8);
            // The skirts face away from the chunk's middle.
            for t in c.fine.chunks(3) {
                let (a, b, cc) = (
                    c.mesh.vertices[t[0] as usize].pos,
                    c.mesh.vertices[t[1] as usize].pos,
                    c.mesh.vertices[t[2] as usize].pos,
                );
                let n = (b - a).cross(cc - a);
                if n.y.abs() < 1e-3 && n.length() > 1e-3 {
                    let out = Vec2::new(a.x - c.center.x, a.z - c.center.z);
                    assert!(Vec2::new(n.x, n.z).dot(out) > 0.0, "a skirt facing in");
                }
            }
        }
        let sea = sea(&t);
        let all: Vec<u32> = sea.indices.clone();
        let (up, down) = faces_up(&sea, &all);
        assert!(up > 100 && down == 0, "{up} up, {down} down");
    }
}
