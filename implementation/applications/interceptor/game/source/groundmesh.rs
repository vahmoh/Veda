// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The ground's and the sea's meshes.
//!
//! The ground is cut into square chunks, each drawn at full detail near
//! the camera or at a quarter of it further away; a skirt hanging from
//! each chunk's edges hides the cracks where the two meet. At a tunnel's
//! mouths the ground is cut along the middle of the headwall, the step
//! between the cutting and the hillside left for the headwall to hide
//! ([`Cut`]). The sea is one mesh: a grid over the world where the water
//! is, its vertices carrying the depth (for the shallows' colour and
//! foam), and a ring of large quads out to the horizon.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use crate::prelude::*;

use crate::mesh::Mesh;
use crate::terrain::{CELL, Cut, HALF_X, HALF_Z, Terrain};

/// Cells per chunk side (512 m: few draws, each culled whole).
pub const CHUNK_CELLS: usize = 64;
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
    // The cuts at the tunnels' mouths that may cross this chunk's cells
    // (at either level).
    let (near_lo, near_hi) = (world(i0, j0) - CELL * COARSE as f32, world(i1, j1) + CELL * COARSE as f32);
    let cuts: Vec<&Cut> = t
        .cuts()
        .iter()
        .filter(|c| {
            let (a, b) = (c.point(-c.reach()), c.point(c.reach()));
            a.min(b).cmple(near_hi).all() && a.max(b).cmpge(near_lo).all()
        })
        .collect();
    let cut_base = mesh.base();
    let level = |mesh: &mut Mesh, step: usize| -> Vec<u32> {
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
        let made = mesh.base();
        for c in &cuts {
            idx = cut(mesh, &idx, c);
        }
        if !cuts.is_empty() {
            // The fine level's normals round the cuts serve both levels.
            smooth(mesh, &idx, made, &|p| step == 1 && cuts.iter().any(|c| c.near(p.xz(), 2.0 * CELL)));
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
    let fine = level(&mut mesh, 1);
    let coarse = level(&mut mesh, COARSE);
    for v in &mesh.vertices[cut_base as usize..] {
        lo = lo.min(v.pos);
        hi = hi.max(v.pos);
    }
    let center = (lo + hi) * 0.5;
    GroundChunk { mesh, fine, coarse, center, radius: (hi - lo).length() * 0.5 + SKIRT }
}

/// A corner of a triangle being cut: a vertex of the mesh, or an end of
/// the cut (`x` across it), where the ground is the same either side.
#[derive(Clone, Copy)]
enum Corner {
    Vertex(u32),
    End(f32),
}

/// The triangles `idx` of `mesh`, those the cut `c` crosses cut along it.
fn cut(mesh: &mut Mesh, idx: &[u32], c: &Cut) -> Vec<u32> {
    let mut out = Vec::with_capacity(idx.len());
    for t in idx.chunks(3) {
        cut_triangle(mesh, &mut out, c, [Corner::Vertex(t[0]), Corner::Vertex(t[1]), Corner::Vertex(t[2])]);
    }
    out
}

/// Adds the triangle `tri` of `mesh` to `out`, cut along `c` where `c`
/// crosses it: in a part before the cut and a part behind it, whose
/// points on the cut are at the ground's height on their side, the step
/// between them hidden by the headwall. A triangle with an end of the cut
/// inside is split round that end first, so that past its ends the ground
/// stays whole. A point of the cut on an edge is computed from the edge's
/// vertices in the order of their indices, so that the two triangles of
/// the edge put it in the same place.
fn cut_triangle(mesh: &mut Mesh, out: &mut Vec<u32>, c: &Cut, tri: [Corner; 3]) {
    let reach = c.reach();
    let behind = |mesh: &Mesh, k: Corner| match k {
        Corner::Vertex(i) => Some(c.along(mesh.vertices[i as usize].pos.xz()) >= 0.0),
        Corner::End(_) => None,
    };
    let crossing = |mesh: &Mesh, i: u32, j: u32| {
        let (pi, pj) = (mesh.vertices[i.min(j) as usize].pos.xz(), mesh.vertices[i.max(j) as usize].pos.xz());
        let (ai, aj) = (c.along(pi), c.along(pj));
        pi + (pj - pi) * (ai / (ai - aj))
    };
    let index = |k: Corner| match k {
        Corner::Vertex(i) => i,
        Corner::End(_) => u32::MAX,
    };
    let ends = tri.iter().filter(|k| matches!(k, Corner::End(_))).count();
    match ends {
        0 => {
            let side = tri.map(|k| behind(mesh, k).unwrap_or(false));
            if side[0] == side[1] && side[1] == side[2] {
                return emit_corners(mesh, out, c, tri);
            }
            // The corner alone on its side, and the two after it.
            let k = (0..3).find(|&k| side[k] != side[(k + 1) % 3] && side[k] != side[(k + 2) % 3]).unwrap_or(0);
            let (p, q, r) = (tri[k], tri[(k + 1) % 3], tri[(k + 2) % 3]);
            let e_pq = crossing(mesh, index(p), index(q));
            let e_rp = crossing(mesh, index(r), index(p));
            let (xa, xb) = (c.across(e_pq), c.across(e_rp));
            let (lo, hi) = (xa.min(xb), xa.max(xb));
            if hi <= -reach || lo >= reach {
                return emit_corners(mesh, out, c, tri);
            }
            if let Some(end) = [-reach, reach].into_iter().find(|&e| lo < e && e < hi) {
                for (u, v) in [(tri[0], tri[1]), (tri[1], tri[2]), (tri[2], tri[0])] {
                    cut_triangle(mesh, out, c, [Corner::End(end), u, v]);
                }
                return;
            }
            // The cut runs across it, from edge to edge.
            let apex = vertex(mesh, c, p, side[k]);
            fan(mesh, out, c, apex, (e_pq, xa), (e_rp, xb), side[k]);
            let (q, r) = (vertex(mesh, c, q, !side[k]), vertex(mesh, c, r, !side[k]));
            let e = on_cut(mesh, c, e_rp, xb, !side[k]);
            emit(mesh, out, [q, r, e]);
            fan(mesh, out, c, q, (e_rp, xb), (e_pq, xa), !side[k]);
        }
        1 => {
            let k = (0..3).find(|&k| matches!(tri[k], Corner::End(_))).unwrap_or(0);
            let (h, u, v) = (tri[k], tri[(k + 1) % 3], tri[(k + 2) % 3]);
            let (Corner::End(xh), Some(su), Some(sv)) = (h, behind(mesh, u), behind(mesh, v)) else {
                return;
            };
            if su == sv {
                return emit_corners(mesh, out, c, tri);
            }
            let e = crossing(mesh, index(u), index(v));
            let xe = c.across(e);
            // Beyond the end, the ground is whole.
            if (xe - xh) * xh >= 0.0 {
                return emit_corners(mesh, out, c, tri);
            }
            // Past the other end too: split round that one.
            if (xe + xh) * xh < 0.0 {
                for (a, b) in [(h, u), (u, v), (v, h)] {
                    cut_triangle(mesh, out, c, [Corner::End(-xh), a, b]);
                }
                return;
            }
            let (pu, pv) = (vertex(mesh, c, u, su), vertex(mesh, c, v, sv));
            fan(mesh, out, c, pu, (c.point(xh), xh), (e, xe), su);
            fan(mesh, out, c, pv, (e, xe), (c.point(xh), xh), sv);
        }
        _ => {
            // Two ends: the cut runs along an edge, the third corner on one
            // side of it.
            let k = (0..3).find(|&k| matches!(tri[k], Corner::Vertex(_))).unwrap_or(0);
            let (w, h1, h2) = (tri[k], tri[(k + 1) % 3], tri[(k + 2) % 3]);
            let (Some(sw), Corner::End(x1), Corner::End(x2)) = (behind(mesh, w), h1, h2) else {
                return;
            };
            let apex = vertex(mesh, c, w, sw);
            fan(mesh, out, c, apex, (c.point(x1), x1), (c.point(x2), x2), sw);
        }
    }
}

/// Shades the ground round the cuts as the surfaces their triangles make,
/// either side of a cut: gives each vertex from `made` on (those the cuts
/// made), and each before it at a place that is `near` a cut, the mean of
/// its triangles' normals in `idx`, weighted by their areas. (Elsewhere
/// the height field's normals are as good, and do not straddle a cut.)
/// The cuts' points are made once for each triangle, so they are matched
/// by where they are.
fn smooth(mesh: &mut Mesh, idx: &[u32], made: u32, near: &dyn Fn(Vec3) -> bool) {
    let key = |p: Vec3| [p.x.to_bits(), p.y.to_bits(), p.z.to_bits()];
    let wanted = |mesh: &Mesh, i: u32| i >= made || near(mesh.vertices[i as usize].pos);
    let mut sums: BTreeMap<[u32; 3], Vec3> = BTreeMap::new();
    for t in idx.chunks(3) {
        let [a, b, c] = [t[0], t[1], t[2]].map(|i| mesh.vertices[i as usize].pos);
        let n = (b - a).cross(c - a);
        for &i in t {
            if wanted(mesh, i) {
                *sums.entry(key(mesh.vertices[i as usize].pos)).or_insert(Vec3::ZERO) += n;
            }
        }
    }
    for i in 0..mesh.base() {
        if wanted(mesh, i) {
            let v = &mut mesh.vertices[i as usize];
            if let Some(n) = sums.get(&key(v.pos)) {
                v.normal = n.normalize_or(Vec3::Y);
            }
        }
    }
}

/// The index of corner `k`'s vertex, added to `mesh` if it is an end of
/// the cut `c` (on the side `behind` of the cut or before it, the same).
fn vertex(mesh: &mut Mesh, c: &Cut, k: Corner, behind: bool) -> u32 {
    match k {
        Corner::Vertex(i) => i,
        Corner::End(x) => on_cut(mesh, c, c.point(x), x, behind),
    }
}

/// Adds to `mesh` the point `p` on the cut `c`, `x` across it, at the
/// ground's height on the side `behind` it or before it (its normal is
/// [`smooth`]'s to give).
fn on_cut(mesh: &mut Mesh, c: &Cut, p: Vec2, x: f32, behind: bool) -> u32 {
    let (before, behind_it) = c.heights(x);
    let h = if behind { behind_it } else { before };
    mesh.push(Vec3::new(p.x, h, p.y), Vec3::Y, Vec2::ZERO, [0, 255, 0, 0])
}

/// Triangles from `apex` to the cut `c` between the points `from` and
/// `to` on it (with how far across it each is), through the bends of the
/// ground's height between them, on the side `behind` it or before it.
fn fan(mesh: &mut Mesh, out: &mut Vec<u32>, c: &Cut, apex: u32, from: (Vec2, f32), to: (Vec2, f32), behind: bool) {
    let (lo, hi) = (from.1.min(to.1), from.1.max(to.1));
    let mut points = Vec::with_capacity(4);
    points.push(from);
    let mut bends: Vec<f32> = c.bends().into_iter().filter(|&x| lo < x && x < hi).collect();
    if to.1 < from.1 {
        bends.reverse();
    }
    points.extend(bends.into_iter().map(|x| (c.point(x), x)));
    points.push(to);
    let mut prev = on_cut(mesh, c, points[0].0, points[0].1, behind);
    for &(p, x) in &points[1..] {
        let next = on_cut(mesh, c, p, x, behind);
        emit(mesh, out, [apex, prev, next]);
        prev = next;
    }
}

/// Adds the triangle of `tri`'s corners to `out`, uncut (an end of the
/// cut `c` among them is where the ground is the same either side).
fn emit_corners(mesh: &mut Mesh, out: &mut Vec<u32>, c: &Cut, tri: [Corner; 3]) {
    let v = tri.map(|k| vertex(mesh, c, k, true));
    emit(mesh, out, v);
}

/// Adds triangle `t` of `mesh` to `out`, wound to face up.
fn emit(mesh: &Mesh, out: &mut Vec<u32>, t: [u32; 3]) {
    let [a, b, c] = t.map(|i| mesh.vertices[i as usize].pos);
    // Facing up: counter-clockwise seen from above (x right, z down).
    if (b.z - a.z) * (c.x - a.x) - (b.x - a.x) * (c.z - a.z) >= 0.0 {
        out.extend_from_slice(&t);
    } else {
        out.extend_from_slice(&[t[0], t[2], t[1]]);
    }
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
    use crate::road::{HEADWALL_THICKNESS, Road};

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
        assert!(chunks.len() > 50, "{} chunks", chunks.len());
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

    /// The ground's triangles (not the skirts) at the fine or the coarse
    /// level whose corners are all within `r` of `p` on the ground.
    fn ground_near(chunks: &[GroundChunk], coarse: bool, p: Vec2, r: f32) -> Vec<[Vec3; 3]> {
        let mut out = Vec::new();
        for c in chunks {
            for t in (if coarse { &c.coarse } else { &c.fine }).chunks(3) {
                let v = [t[0], t[1], t[2]].map(|i| c.mesh.vertices[i as usize].pos);
                if (v[1] - v[0]).cross(v[2] - v[0]).y != 0.0 && v.iter().all(|q| q.xz().distance(p) < r) {
                    out.push(v);
                }
            }
        }
        out
    }

    /// The height of the ground of `tris` at `p`.
    fn height_at(tris: &[[Vec3; 3]], p: Vec2) -> Option<f32> {
        tris.iter().find_map(|&[a, b, c]| {
            let (a2, b2, c2) = (a.xz(), b.xz(), c.xz());
            let area = (b2 - a2).perp_dot(c2 - a2);
            let u = (c2 - b2).perp_dot(p - b2) / area;
            let v = (a2 - c2).perp_dot(p - c2) / area;
            let w = 1.0 - u - v;
            (u >= -1e-4 && v >= -1e-4 && w >= -1e-4).then_some(a.y * u + b.y * v + c.y * w)
        })
    }

    #[test]
    fn the_road_into_a_tunnel_is_open() {
        let road = Road::new(VEDA_BAY);
        let t = Terrain::new(&road, 7);
        let chunks = ground_chunks(&t);
        let portals = road.portals();
        assert_eq!(portals.len(), 2, "the tunnel's two mouths");
        for portal in &portals {
            let ring = road.samples()[portal.index];
            let s = portal.index as f32 * road.spacing();
            let tris = ground_near(&chunks, false, ring.pos.xz(), 80.0);
            // Through the mouth, across the tunnel's width: the ground is
            // under the road before the headwall, over the roof behind it.
            for d in (-40..=40).map(|d| d as f32) {
                if (-1.0..=HEADWALL_THICKNESS + 1.0).contains(&d) {
                    continue;
                }
                for x in (-9..=9).map(|x| x as f32) {
                    let p = road.point(s + d * portal.inward, x);
                    let g = height_at(&tris, p.xz()).expect("a hole in the ground");
                    if d < 0.0 {
                        assert!(g < p.y - 0.2, "the ground {:.1} m over the road, {d} m from a mouth", g - p.y);
                    } else {
                        assert!(g > p.y + 8.0, "the ground {:.1} m over the road, {d} m into a mouth", g - p.y);
                    }
                }
            }
        }
    }

    #[test]
    fn the_ground_is_whole_but_where_a_headwall_hides_its_cut() {
        let road = Road::new(VEDA_BAY);
        let t = Terrain::new(&road, 7);
        let chunks = ground_chunks(&t);
        // Edges by their ends, as exactly as the ends are.
        let key = |p: Vec3| [p.x.to_bits(), p.y.to_bits(), p.z.to_bits()];
        let point = |k: [u32; 3]| Vec3::from_array(k.map(f32::from_bits));
        for c in t.cuts() {
            let middle = c.point(0.0);
            for coarse in [false, true] {
                let mut edges = BTreeMap::new();
                for tri in ground_near(&chunks, coarse, middle, 140.0) {
                    for (a, b) in [(tri[0], tri[1]), (tri[1], tri[2]), (tri[2], tri[0])] {
                        *edges.entry((key(a).min(key(b)), key(a).max(key(b)))).or_insert(0) += 1;
                    }
                }
                let mut cut = 0.0;
                for ((a, b), n) in edges {
                    let (a, b) = (point(a), point(b));
                    assert!(n <= 2, "an edge of {n} triangles");
                    // Away from the edge of the patch, an edge of one
                    // triangle is on the cut, within its reach.
                    if n == 1 && a.xz().distance(middle) < 70.0 && b.xz().distance(middle) < 70.0 {
                        for p in [a, b] {
                            assert!(c.along(p.xz()).abs() < 1e-3, "a crack {:.2} m off the cut", c.along(p.xz()));
                            assert!(c.across(p.xz()).abs() < c.reach() + 1e-3, "the cut beyond its reach");
                        }
                        cut += a.xz().distance(b.xz());
                    }
                }
                // Both sides of the cut, from end to end.
                assert!((cut - 4.0 * c.reach()).abs() < 0.01, "{cut} m of cut edges");
            }
        }
    }
}
