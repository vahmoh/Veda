// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Meshes built on the processor: the vertex layout every static object
//! shares, and the shapes the world and the cars are made of.
//!
//! A vertex is a position, a normal, two texture coordinates and four
//! bytes of material parameters ([`Vertex`]); a mesh is vertices with 32-bit
//! triangle indices. The builders here make boxes, cylinders, lathes and
//! strips; `road` sweeps profiles along the road.

use alloc::vec::Vec;

use crate::prelude::*;

/// Bytes per vertex: position (3 floats), normal (3), texture
/// coordinates (2), material parameters (4 bytes).
pub const STRIDE: usize = 36;

/// What a vertex's material parameters mean is up to the program drawing
/// it; by convention `[material, shade, variant, glow]`: the kind of
/// surface, how exposed to the sky it is (255 open, 0 enclosed), a
/// variant of its pattern, and how much it shines by itself.
pub type Params = [u8; 4];

/// A vertex.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vertex {
    pub pos: Vec3,
    pub normal: Vec3,
    pub uv: Vec2,
    pub params: Params,
}

/// Triangles.
#[derive(Clone, Debug, Default)]
pub struct Mesh {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
}

impl Mesh {
    pub fn new() -> Mesh {
        Mesh::default()
    }

    /// The index the next vertex gets.
    pub fn base(&self) -> u32 {
        self.vertices.len() as u32
    }

    pub fn push(&mut self, pos: Vec3, normal: Vec3, uv: Vec2, params: Params) -> u32 {
        self.vertices.push(Vertex { pos, normal, uv, params });
        self.vertices.len() as u32 - 1
    }

    pub fn triangle(&mut self, a: u32, b: u32, c: u32) {
        self.indices.extend_from_slice(&[a, b, c]);
    }

    /// Two triangles: `a b c` and `a c d` (counter-clockwise seen from the
    /// front).
    pub fn quad(&mut self, a: u32, b: u32, c: u32, d: u32) {
        self.indices.extend_from_slice(&[a, b, c, a, c, d]);
    }

    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    /// Appends `other`.
    pub fn append(&mut self, other: &Mesh) {
        let base = self.base();
        self.vertices.extend_from_slice(&other.vertices);
        self.indices.extend(other.indices.iter().map(|i| i + base));
    }

    /// Appends `other` moved by `m` (normals turned with it).
    pub fn append_transformed(&mut self, other: &Mesh, m: &Mat4) {
        let base = self.base();
        let normal = m.try_inverse().unwrap_or(Mat4::IDENTITY).transpose();
        for v in &other.vertices {
            self.vertices.push(Vertex {
                pos: m.transform_point3(v.pos),
                normal: normal.transform_vector3(v.normal).normalize_or(v.normal),
                ..*v
            });
        }
        self.indices.extend(other.indices.iter().map(|i| i + base));
    }

    /// Sets every vertex's parameters.
    pub fn with_params(mut self, params: Params) -> Mesh {
        for v in &mut self.vertices {
            v.params = params;
        }
        self
    }

    /// The vertices as bytes in the layout of [`STRIDE`].
    pub fn vertex_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.vertices.len() * STRIDE);
        for v in &self.vertices {
            for f in [v.pos.x, v.pos.y, v.pos.z, v.normal.x, v.normal.y, v.normal.z, v.uv.x, v.uv.y] {
                out.extend_from_slice(&f.to_le_bytes());
            }
            out.extend_from_slice(&v.params);
        }
        out
    }

    /// The indices as bytes.
    pub fn index_bytes(&self) -> Vec<u8> {
        self.indices.iter().flat_map(|i| i.to_le_bytes()).collect()
    }

    /// The bounding box `(min, max)` (zero for an empty mesh).
    pub fn bounds(&self) -> (Vec3, Vec3) {
        if self.vertices.is_empty() {
            return (Vec3::ZERO, Vec3::ZERO);
        }
        let mut lo = Vec3::splat(f32::MAX);
        let mut hi = Vec3::splat(f32::MIN);
        for v in &self.vertices {
            lo = lo.min(v.pos);
            hi = hi.max(v.pos);
        }
        (lo, hi)
    }
}

/// An axis-aligned box from `lo` to `hi`, each face with its own vertices
/// (flat normals) and texture coordinates in metres.
pub fn cuboid(lo: Vec3, hi: Vec3, params: Params) -> Mesh {
    let mut m = Mesh::new();
    let faces: [(Vec3, Vec3, Vec3); 6] = [
        (Vec3::X, Vec3::NEG_Z, Vec3::Y),
        (Vec3::NEG_X, Vec3::Z, Vec3::Y),
        (Vec3::Y, Vec3::X, Vec3::NEG_Z),
        (Vec3::NEG_Y, Vec3::X, Vec3::Z),
        (Vec3::Z, Vec3::X, Vec3::Y),
        (Vec3::NEG_Z, Vec3::NEG_X, Vec3::Y),
    ];
    let c = (lo + hi) * 0.5;
    let half = (hi - lo) * 0.5;
    for (n, u, v) in faces {
        let center = c + n * half.dot(n.abs());
        let (hu, hv) = (half.dot(u.abs()), half.dot(v.abs()));
        let base = m.base();
        for (a, b) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
            let p = center + u * (a * hu) + v * (b * hv);
            m.push(p, n, Vec2::new(p.dot(u), p.dot(v)), params);
        }
        m.quad(base, base + 1, base + 2, base + 3);
    }
    m
}

/// A cylinder along y from `y0` to `y1` of radius `r`, `sides` around,
/// smooth sided, with caps if `caps`.
pub fn cylinder(r: f32, y0: f32, y1: f32, sides: usize, caps: bool, params: Params) -> Mesh {
    lathe(&[(r, y0), (r, y1)], sides, caps, params)
}

/// A surface of revolution about y through the profile points `(radius,
/// y)`, `sides` around, smooth around and with flat caps where the profile
/// ends off the axis if `caps`.
pub fn lathe(profile: &[(f32, f32)], sides: usize, caps: bool, params: Params) -> Mesh {
    let mut m = Mesh::new();
    let n = profile.len();
    if n < 2 {
        return m;
    }
    // The profile's normals (in the radius-height plane), per point.
    let normal_2d = |i: usize| {
        let (a, b) = (profile[i.saturating_sub(1)], profile[(i + 1).min(n - 1)]);
        let (dr, dy) = (b.0 - a.0, b.1 - a.1);
        Vec2::new(dy, -dr).normalize_or(Vec2::X)
    };
    let mut length = 0.0;
    let mut v_of = Vec::with_capacity(n);
    for i in 0..n {
        if i > 0 {
            length += Vec2::new(profile[i].0 - profile[i - 1].0, profile[i].1 - profile[i - 1].1).length();
        }
        v_of.push(length);
    }
    let ring = sides as u32 + 1;
    for (i, &(r, y)) in profile.iter().enumerate() {
        let nn = normal_2d(i);
        for k in 0..=sides {
            let a = k as f32 / sides as f32 * core::f32::consts::TAU;
            let (s, c) = a.sin_cos();
            m.push(
                Vec3::new(c * r, y, s * r),
                Vec3::new(c * nn.x, nn.y, s * nn.x),
                Vec2::new(a * r.max(0.01), v_of[i]),
                params,
            );
        }
    }
    for i in 0..n as u32 - 1 {
        for k in 0..sides as u32 {
            let (a, b) = (i * ring + k, (i + 1) * ring + k);
            m.quad(a, b, b + 1, a + 1);
        }
    }
    if caps {
        for (&(r, y), up) in [(&profile[0], false), (&profile[n - 1], true)] {
            if r <= 1e-4 {
                continue;
            }
            let normal = if up { Vec3::Y } else { Vec3::NEG_Y };
            let center = m.push(Vec3::new(0.0, y, 0.0), normal, Vec2::ZERO, params);
            let first = m.base();
            for k in 0..=sides {
                let a = k as f32 / sides as f32 * core::f32::consts::TAU;
                let (s, c) = a.sin_cos();
                m.push(Vec3::new(c * r, y, s * r), normal, Vec2::new(c * r, s * r), params);
            }
            for k in 0..sides as u32 {
                if up {
                    m.triangle(center, first + k + 1, first + k);
                } else {
                    m.triangle(center, first + k, first + k + 1);
                }
            }
        }
    }
    m
}

/// A sphere of radius `r`, `rings` from pole to pole and `sides` around.
pub fn sphere(r: f32, rings: usize, sides: usize, params: Params) -> Mesh {
    let profile: Vec<(f32, f32)> = (0..=rings)
        .map(|i| {
            let a = i as f32 / rings as f32 * core::f32::consts::PI;
            (a.sin() * r, -a.cos() * r)
        })
        .collect();
    let mut m = lathe(&profile, sides, false, params);
    for v in &mut m.vertices {
        v.normal = v.pos.normalize_or(Vec3::Y);
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cuboid_faces_point_out_and_wind_counter_clockwise() {
        let m = cuboid(Vec3::new(-1.0, 0.0, -2.0), Vec3::new(1.0, 3.0, 2.0), [0; 4]);
        assert_eq!(m.vertices.len(), 24);
        assert_eq!(m.indices.len(), 36);
        for t in m.indices.chunks(3) {
            let (a, b, c) = (m.vertices[t[0] as usize], m.vertices[t[1] as usize], m.vertices[t[2] as usize]);
            let face = (b.pos - a.pos).cross(c.pos - a.pos).normalize();
            assert!(face.dot(a.normal) > 0.99, "a triangle facing {face:?}, normal {:?}", a.normal);
        }
        assert_eq!(m.bounds(), (Vec3::new(-1.0, 0.0, -2.0), Vec3::new(1.0, 3.0, 2.0)));
    }

    #[test]
    fn lathe_and_sphere_face_out() {
        for m in [cylinder(1.0, 0.0, 2.0, 12, true, [0; 4]), sphere(2.0, 8, 12, [0; 4])] {
            let center = (m.bounds().0 + m.bounds().1) * 0.5;
            for t in m.indices.chunks(3) {
                let (a, b, c) = (m.vertices[t[0] as usize], m.vertices[t[1] as usize], m.vertices[t[2] as usize]);
                let face = (b.pos - a.pos).cross(c.pos - a.pos);
                if face.length() < 1e-6 {
                    continue;
                }
                let mid = (a.pos + b.pos + c.pos) / 3.0;
                assert!(face.dot(mid - center) > 0.0, "a triangle facing in");
            }
        }
    }

    #[test]
    fn bytes_have_the_stride() {
        let m = cuboid(Vec3::ZERO, Vec3::ONE, [1, 2, 3, 4]);
        let b = m.vertex_bytes();
        assert_eq!(b.len(), m.vertices.len() * STRIDE);
        assert_eq!(&b[32..36], &[1, 2, 3, 4]);
    }
}
