//! Textures made on the processor at start-up: tileable noise, and the
//! world's surfaces painted with it (asphalt, concrete, paving, metal,
//! grass, rock, sand, the water's ripples).
//!
//! Colours are sRGB-encoded (the textures are sRGB, so the GPU samples
//! them in linear light); the alpha channel holds what the surface's
//! program reads from it, the roughness for most. Every texture tiles.

use alloc::vec;
use alloc::vec::Vec;

use crate::prelude::*;

/// An RGBA8 picture.
#[derive(Clone, Debug)]
pub struct Image {
    pub w: usize,
    pub h: usize,
    /// Rows from the first, four bytes a texel.
    pub data: Vec<u8>,
}

impl Image {
    /// A picture whose texel `(x, y)` is `f(x, y)` (r, g, b, a in 0..1).
    pub fn from_fn(w: usize, h: usize, f: impl Fn(usize, usize) -> [f32; 4]) -> Image {
        let mut data = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let c = f(x, y);
                let o = (y * w + x) * 4;
                for k in 0..4 {
                    data[o + k] = (c[k].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                }
            }
        }
        Image { w, h, data }
    }
}

/// An integer hash of a lattice point (and a seed) to 0..1.
fn hash(x: i32, y: i32, seed: u32) -> f32 {
    let mut h =
        (x as u32).wrapping_mul(0x8DA6_B343) ^ (y as u32).wrapping_mul(0xD816_3841) ^ seed.wrapping_mul(0xCB1A_B31F);
    h ^= h >> 13;
    h = h.wrapping_mul(0x5BD1_E995);
    h ^= h >> 15;
    (h & 0x00FF_FFFF) as f32 / 16_777_215.0
}

/// Value noise with period `period` lattice cells, at `(x, y)` in cells:
/// smooth, in 0..1, and tiling.
pub fn value(x: f32, y: f32, period: i32, seed: u32) -> f32 {
    let (fx, fy) = (x.floor(), y.floor());
    let (tx, ty) = (x - fx, y - fy);
    let (ix, iy) = (fx as i32, fy as i32);
    let w = |i: i32| i.rem_euclid(period.max(1));
    let (sx, sy) = (tx * tx * (3.0 - 2.0 * tx), ty * ty * (3.0 - 2.0 * ty));
    let a = hash(w(ix), w(iy), seed);
    let b = hash(w(ix + 1), w(iy), seed);
    let c = hash(w(ix), w(iy + 1), seed);
    let d = hash(w(ix + 1), w(iy + 1), seed);
    let top = a + (b - a) * sx;
    let bottom = c + (d - c) * sx;
    top + (bottom - top) * sy
}

/// Fractal value noise over a tile `size` texels wide, starting with
/// `cells` lattice cells across it: `octaves` layers, each twice as fine
/// and `gain` times as strong. In 0..1, tiling.
pub fn fbm(x: usize, y: usize, size: usize, cells: i32, octaves: u32, gain: f32, seed: u32) -> f32 {
    let (mut sum, mut amp, mut norm) = (0.0, 1.0, 0.0);
    let mut c = cells;
    for o in 0..octaves {
        let u = x as f32 / size as f32 * c as f32;
        let v = y as f32 / size as f32 * c as f32;
        sum += value(u, v, c, seed.wrapping_add(o * 977)) * amp;
        norm += amp;
        amp *= gain;
        c *= 2;
    }
    sum / norm
}

/// Distance to the nearest of a tiling scatter of points (`cells` across
/// the tile), and the hash of that point: Worley noise for pebbles,
/// cracks and cells.
pub fn worley(x: usize, y: usize, size: usize, cells: i32, seed: u32) -> (f32, f32) {
    let u = x as f32 / size as f32 * cells as f32;
    let v = y as f32 / size as f32 * cells as f32;
    let (cx, cy) = (u.floor() as i32, v.floor() as i32);
    let mut best = (f32::MAX, 0.0);
    for dy in -1..=1 {
        for dx in -1..=1 {
            let (gx, gy) = (cx + dx, cy + dy);
            let (wx, wy) = (gx.rem_euclid(cells), gy.rem_euclid(cells));
            let px = gx as f32 + hash(wx, wy, seed);
            let py = gy as f32 + hash(wx, wy, seed ^ 0x55AA);
            let d = ((px - u) * (px - u) + (py - v) * (py - v)).sqrt();
            if d < best.0 {
                best = (d, hash(wx, wy, seed ^ 0x1234));
            }
        }
    }
    best
}

/// sRGB encoding of a linear value.
pub fn srgb(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

fn rgb(c: Vec3, a: f32) -> [f32; 4] {
    [srgb(c.x), srgb(c.y), srgb(c.z), a]
}

/// Asphalt (spanning 4 m): dark aggregate in binder, specks of stone,
/// patches and fine cracks; roughness in alpha.
pub fn asphalt(size: usize) -> Image {
    Image::from_fn(size, size, |x, y| {
        let grain = fbm(x, y, size, 64, 3, 0.6, 1);
        let mottle = fbm(x, y, size, 4, 4, 0.55, 2);
        let (stone, which) = worley(x, y, size, 160, 3);
        let speck = if stone < 0.28 { (which - 0.5) * 0.9 } else { 0.0 };
        let cracks = {
            let (d1, _) = worley(x, y, size, 6, 4);
            let n = fbm(x, y, size, 16, 3, 0.5, 5);
            let edge = (d1 - 0.62 - n * 0.3).abs();
            if edge < 0.012 && n > 0.45 { 1.0 - edge / 0.012 } else { 0.0 }
        };
        let base = 0.055 + 0.03 * mottle + 0.035 * grain + 0.04 * speck;
        let c = Vec3::new(base, base * 0.99, base * 1.02) * (1.0 - 0.6 * cracks);
        let rough = 0.82 + 0.12 * grain - 0.1 * mottle;
        rgb(c, rough)
    })
}

/// The things beside the road (each spanning 2 m), in the order of
/// `roadmesh::material`: concrete, paving, kerb stone, galvanised metal,
/// gravel, tunnel tiles, painted steel, sooty concrete. Roughness in
/// alpha.
pub fn surfaces(size: usize) -> Vec<Image> {
    let concrete = Image::from_fn(size, size, |x, y| {
        let n = fbm(x, y, size, 8, 5, 0.55, 11);
        let pores = fbm(x, y, size, 96, 2, 0.5, 12);
        let v = 0.25 + 0.09 * n - 0.04 * (pores - 0.5).abs();
        rgb(Vec3::new(v, v * 0.98, v * 0.95), 0.85)
    });
    let paving = Image::from_fn(size, size, |x, y| {
        // Slabs half a metre square.
        let tiles = 4;
        let (u, v) = (x * tiles % size, y * tiles % size);
        let gap = u < size / 40 || v < size / 40;
        let slab = hash((x * tiles / size) as i32, (y * tiles / size) as i32, 13);
        let n = fbm(x, y, size, 16, 4, 0.5, 14);
        let base = 0.19 + 0.05 * slab + 0.05 * n;
        let c = if gap { Vec3::splat(0.07) } else { Vec3::new(base, base * 0.97, base * 0.93) };
        rgb(c, if gap { 0.95 } else { 0.8 })
    });
    let kerb = Image::from_fn(size, size, |x, y| {
        let n = fbm(x, y, size, 32, 3, 0.6, 15);
        let (d, w) = worley(x, y, size, 64, 16);
        let fleck = if d < 0.25 { (w - 0.5) * 0.12 } else { 0.0 };
        let v = 0.28 + 0.06 * n + fleck;
        rgb(Vec3::new(v, v, v * 0.98), 0.7)
    });
    let metal = Image::from_fn(size, size, |x, y| {
        let streak = fbm(x / 8, y, size / 8, 4, 3, 0.5, 17);
        let n = fbm(x, y, size, 32, 3, 0.5, 18);
        let v = 0.52 + 0.1 * streak + 0.05 * n;
        rgb(Vec3::new(v, v * 1.01, v * 1.03), 0.35 + 0.15 * n)
    });
    let gravel = Image::from_fn(size, size, |x, y| {
        let (d, w) = worley(x, y, size, 48, 19);
        let n = fbm(x, y, size, 8, 4, 0.5, 20);
        let pebble = (1.0 - d * 1.6).max(0.0);
        let v = 0.10 + 0.08 * pebble * (0.5 + w) + 0.04 * n;
        rgb(Vec3::new(v * 1.05, v, v * 0.88), 0.9)
    });
    let tiles = Image::from_fn(size, size, |x, y| {
        // Tiles a quarter of a metre square.
        let k = 8;
        let (u, v) = (x * k % size, y * k % size);
        let grout = u < size / 24 || v < size / 24;
        let t = hash((x * k / size) as i32, (y * k / size) as i32, 21);
        let n = fbm(x, y, size, 4, 3, 0.5, 22);
        let base = 0.62 + 0.05 * t - 0.12 * n;
        let c = if grout { Vec3::splat(0.25) } else { Vec3::new(base, base * 0.98, base * 0.92) };
        rgb(c, if grout { 0.9 } else { 0.25 })
    });
    let painted = Image::from_fn(size, size, |x, y| {
        let n = fbm(x, y, size, 16, 4, 0.5, 23);
        let v = 0.55 + 0.05 * n;
        rgb(Vec3::splat(v), 0.45 + 0.1 * n)
    });
    let dark = Image::from_fn(size, size, |x, y| {
        let n = fbm(x, y, size, 8, 5, 0.55, 24);
        let v = 0.07 + 0.06 * n;
        rgb(Vec3::new(v, v * 0.97, v * 0.93), 0.9)
    });
    vec![concrete, paving, kerb, metal, gravel, tiles, painted, dark]
}

/// Walls (each spanning 2 m), in the order the building program reads
/// them: concrete panels, brick, corrugated metal, stone. Roughness in
/// alpha.
pub fn walls(size: usize) -> Vec<Image> {
    let panels = Image::from_fn(size, size, |x, y| {
        // Panels 1 m wide and high, with joints.
        let k = 2;
        let (u, v) = (x * k % size, y * k % size);
        let joint = u < size / 64 || v < size / 64;
        let p = hash((x * k / size) as i32, (y * k / size) as i32, 61);
        let n = fbm(x, y, size, 8, 5, 0.5, 62);
        let stain = fbm(x, y / 4, size, 4, 3, 0.5, 63);
        let base = 0.62 + 0.08 * p + 0.1 * n - 0.12 * stain;
        let c = if joint { Vec3::splat(0.35) } else { Vec3::splat(base) };
        rgb(c, 0.8)
    });
    let brick = Image::from_fn(size, size, |x, y| {
        // Bricks 0.25 m by 0.0625 m, every other course shifted half.
        let (bw, bh) = (size / 8, size / 32);
        let row = y / bh;
        let shift = if row % 2 == 0 { 0 } else { bw / 2 };
        let col = (x + shift) / bw;
        let (u, v) = ((x + shift) % bw, y % bh);
        let mortar = u < bw / 10 || v < bh / 5;
        let b = hash(col as i32 % 8, row as i32, 64);
        let n = fbm(x, y, size, 32, 3, 0.5, 65);
        let c = if mortar { Vec3::splat(0.55) } else { Vec3::splat(0.75 + 0.3 * b + 0.15 * n) };
        rgb(c, if mortar { 0.95 } else { 0.85 })
    });
    let metal = Image::from_fn(size, size, |x, y| {
        // Vertical corrugations every 7.5 cm.
        let ridge = ((x as f32 / size as f32 * 2.0 / 0.075) * core::f32::consts::TAU).sin();
        let n = fbm(x, y, size, 8, 4, 0.5, 66);
        let rust = fbm(x, y, size, 16, 3, 0.6, 67);
        let v = 0.75 + 0.12 * ridge - 0.15 * n;
        let c = Vec3::splat(v).lerp(Vec3::new(0.55, 0.32, 0.18), (rust - 0.62).max(0.0) * 2.0);
        rgb(c, 0.45 + 0.2 * n)
    });
    let stone = Image::from_fn(size, size, |x, y| {
        // Blocks 1 m by 0.5 m.
        let (bw, bh) = (size / 2, size / 4);
        let row = y / bh;
        let shift = if row % 2 == 0 { 0 } else { bw / 2 };
        let (u, v) = ((x + shift) % bw, y % bh);
        let joint = u < size / 128 || v < size / 128;
        let b = hash(((x + shift) / bw) as i32, row as i32, 68);
        let n = fbm(x, y, size, 16, 4, 0.55, 69);
        let base = 0.78 + 0.08 * b + 0.12 * n;
        let c = if joint { Vec3::splat(0.45) } else { Vec3::new(base, base * 0.96, base * 0.9) };
        rgb(c, 0.75)
    });
    vec![panels, brick, metal, stone]
}

/// The ground's layers (each spanning 8 m): grass, rock, sand, dirt.
/// Alpha holds a height for blending them.
pub fn ground(size: usize) -> Vec<Image> {
    let grass = Image::from_fn(size, size, |x, y| {
        let blades = fbm(x, y, size, 128, 2, 0.5, 31);
        let clumps = fbm(x, y, size, 8, 4, 0.55, 32);
        let dry = fbm(x, y, size, 3, 3, 0.5, 33);
        let g = Vec3::new(0.030, 0.075, 0.016) * (0.7 + 0.6 * blades) * (0.75 + 0.5 * clumps);
        let c = g.lerp(Vec3::new(0.10, 0.09, 0.035), (dry - 0.55).max(0.0) * 1.5);
        rgb(c, 0.4 + 0.4 * clumps)
    });
    let rock = Image::from_fn(size, size, |x, y| {
        let strata = fbm(x / 2, y, size / 2, 3, 5, 0.6, 34);
        let (d, w) = worley(x, y, size, 10, 35);
        let n = fbm(x, y, size, 32, 3, 0.5, 36);
        let v = 0.18 + 0.12 * strata + 0.06 * n + 0.05 * (w - 0.5) - 0.08 * (1.0 - d).powi(8);
        rgb(Vec3::new(v * 1.04, v, v * 0.94), 0.5 + 0.5 * d)
    });
    let sand = Image::from_fn(size, size, |x, y| {
        let ripples =
            ((y as f32 / size as f32 * 40.0 + fbm(x, y, size, 4, 3, 0.5, 37) * 6.0) * core::f32::consts::TAU).sin();
        let n = fbm(x, y, size, 64, 2, 0.5, 38);
        let v = 0.52 + 0.04 * ripples + 0.06 * n;
        rgb(Vec3::new(v, v * 0.88, v * 0.66), 0.5 + 0.2 * ripples)
    });
    let dirt = Image::from_fn(size, size, |x, y| {
        let n = fbm(x, y, size, 16, 5, 0.55, 39);
        let (d, _) = worley(x, y, size, 40, 40);
        let v = 0.14 + 0.08 * n + 0.04 * d;
        rgb(Vec3::new(v * 1.1, v * 0.92, v * 0.72), 0.3 + 0.5 * n)
    });
    vec![grass, rock, sand, dirt]
}

/// Ripples for the water: a tiling normal map (x, y and z of the normal
/// in red, green and blue, mapped from -1..1).
pub fn ripples(size: usize) -> Image {
    let h = |x: usize, y: usize| {
        fbm(x % size, y % size, size, 8, 5, 0.5, 51) * 0.7 + fbm(x % size, y % size, size, 32, 3, 0.5, 52) * 0.3
    };
    Image::from_fn(size, size, |x, y| {
        let dx = h(x + 1, y) - h(x + size - 1, y);
        let dy = h(x, y + 1) - h(x, y + size - 1);
        let n = Vec3::new(-dx * 6.0, -dy * 6.0, 1.0).normalize();
        [n.x * 0.5 + 0.5, n.y * 0.5 + 0.5, n.z * 0.5 + 0.5, 1.0]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noise_tiles() {
        let size = 64;
        for y in 0..size {
            // The left column continues from the right one.
            let a = fbm(0, y, size, 4, 4, 0.5, 9);
            let b = fbm(size, y, size, 4, 4, 0.5, 9);
            assert!((a - b).abs() < 1e-5);
        }
        let (d0, h0) = worley(0, 5, size, 8, 3);
        let (d1, h1) = worley(size, 5, size, 8, 3);
        assert!((d0 - d1).abs() < 1e-4 && h0 == h1);
    }

    #[test]
    fn textures_have_their_sizes() {
        assert_eq!(asphalt(64).data.len(), 64 * 64 * 4);
        assert_eq!(surfaces(32).len(), crate::roadmesh::material::COUNT);
        assert_eq!(ground(32).len(), 4);
        let r = ripples(32);
        // Normals point up, mostly.
        assert!(r.data.chunks(4).all(|t| t[2] > 128));
    }
}
