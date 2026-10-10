//! The cars' shapes, made from a few measurements.
//!
//! A body is lofted through cross-sections from the tail to the nose.
//! Each cross-section runs from the middle of the floor out under the
//! sill, up the wheel arch's liner where a wheel is, up the door to the
//! shoulder, then up the glasshouse (or across the bonnet or the boot
//! lid, where there is none) to the middle of the roof; the other half is
//! its mirror image. The centre line's profile from the side, the
//! shoulder's height and the plan's widths shape the sections; where the
//! windscreen, the side windows, the pillars and the rear window are
//! decides each face's material. Lights, grille, mirrors, plates and, for
//! the police, the light bar and the push bar are added as small parts.
//!
//! Car space: +z forward, +y up, +x to the car's left (so that the left,
//! up and forward vectors make a right-handed frame); the origin is on the
//! ground between the axles. Materials are in the vertices' first
//! parameter ([`part`]).

use alloc::vec;
use alloc::vec::Vec;

use crate::prelude::*;

use crate::mesh::{self, Mesh};

/// What a face of a car is made of.
pub mod part {
    pub const PAINT: u8 = 0;
    pub const GLASS: u8 = 1;
    /// Black plastic and rubber: trim, bumpers' lower parts, window seals.
    pub const TRIM: u8 = 2;
    pub const CHROME: u8 = 3;
    pub const HEADLIGHT: u8 = 4;
    pub const TAILLIGHT: u8 = 5;
    /// The police light bar's halves.
    pub const BAR_RED: u8 = 6;
    pub const BAR_BLUE: u8 = 7;
    pub const TIRE: u8 = 8;
    pub const RIM: u8 = 9;
    pub const PLATE: u8 = 10;
    /// The underside, arch liners and grille: dark.
    pub const DARK: u8 = 11;
    /// Indicators (amber).
    pub const INDICATOR: u8 = 12;
}

/// The kinds of car on Veda Bay's roads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// The police's interceptor: a big saloon with a light bar and a push
    /// bar.
    Police,
    /// The same saloon, as everyone drives it.
    Saloon,
    Hatchback,
    Estate,
    Suv,
    Van,
    /// A low two-door with a fastback.
    Coupe,
    /// Long bonnet, short boot, wide.
    Muscle,
    /// Mid-engined, low and wide as a wedge.
    Supercar,
    Taxi,
}

impl Kind {
    pub const ALL: [Kind; 10] = [
        Kind::Police,
        Kind::Saloon,
        Kind::Hatchback,
        Kind::Estate,
        Kind::Suv,
        Kind::Van,
        Kind::Coupe,
        Kind::Muscle,
        Kind::Supercar,
        Kind::Taxi,
    ];

    /// The measurements of this kind's body.
    pub fn shape(self) -> Shape {
        match self {
            Kind::Police | Kind::Saloon | Kind::Taxi => Shape {
                length: 5.05,
                width: 1.92,
                wheelbase: 2.95,
                axle_shift: 0.02,
                wheel_radius: 0.36,
                tire_width: 0.25,
                floor: 0.17,
                belt: 0.98,
                top: &[
                    (-2.525, 0.62),
                    (-2.47, 0.92),
                    (-2.30, 1.01),
                    (-1.62, 1.04),
                    (-0.98, 1.44),
                    (-0.05, 1.47),
                    (0.78, 0.97),
                    (1.30, 0.90),
                    (2.28, 0.80),
                    (2.48, 0.70),
                    (2.525, 0.50),
                ],
                cowl: 0.78,
                a: -0.05,
                c: -0.98,
                deck: -1.62,
                roof_width: 0.70,
                window_inset: 0.09,
                nose: 0.55,
                tail: 0.45,
                ends: (0.30, 0.62),
            },
            Kind::Hatchback => Shape {
                length: 4.15,
                width: 1.80,
                wheelbase: 2.62,
                axle_shift: 0.05,
                wheel_radius: 0.32,
                tire_width: 0.21,
                floor: 0.16,
                belt: 0.96,
                top: &[
                    (-2.075, 0.70),
                    (-2.03, 1.05),
                    (-1.85, 1.40),
                    (-1.20, 1.50),
                    (0.0, 1.50),
                    (0.62, 1.00),
                    (1.30, 0.86),
                    (1.95, 0.76),
                    (2.075, 0.52),
                ],
                cowl: 0.62,
                a: 0.0,
                c: -1.20,
                deck: -2.0,
                roof_width: 0.66,
                window_inset: 0.09,
                nose: 0.5,
                tail: 0.3,
                ends: (0.30, 0.66),
            },
            Kind::Estate => Shape {
                length: 4.95,
                width: 1.88,
                wheelbase: 2.90,
                axle_shift: 0.0,
                wheel_radius: 0.35,
                tire_width: 0.24,
                floor: 0.17,
                belt: 0.98,
                top: &[
                    (-2.475, 0.65),
                    (-2.44, 1.00),
                    (-2.32, 1.42),
                    (-1.90, 1.47),
                    (-0.05, 1.48),
                    (0.75, 0.98),
                    (1.30, 0.90),
                    (2.25, 0.80),
                    (2.475, 0.52),
                ],
                cowl: 0.75,
                a: -0.05,
                c: -1.90,
                deck: -2.40,
                roof_width: 0.70,
                window_inset: 0.09,
                nose: 0.5,
                tail: 0.32,
                ends: (0.30, 0.64),
            },
            Kind::Suv => Shape {
                length: 4.90,
                width: 1.98,
                wheelbase: 2.90,
                axle_shift: 0.0,
                wheel_radius: 0.40,
                tire_width: 0.27,
                floor: 0.24,
                belt: 1.14,
                top: &[
                    (-2.45, 0.80),
                    (-2.42, 1.20),
                    (-2.30, 1.74),
                    (-1.80, 1.78),
                    (0.10, 1.80),
                    (0.85, 1.18),
                    (1.40, 1.10),
                    (2.25, 1.02),
                    (2.45, 0.70),
                ],
                cowl: 0.85,
                a: 0.10,
                c: -1.80,
                deck: -2.33,
                roof_width: 0.76,
                window_inset: 0.08,
                nose: 0.45,
                tail: 0.3,
                ends: (0.40, 0.86),
            },
            Kind::Van => Shape {
                length: 5.30,
                width: 2.00,
                wheelbase: 3.30,
                axle_shift: 0.15,
                wheel_radius: 0.37,
                tire_width: 0.24,
                floor: 0.20,
                belt: 1.12,
                top: &[
                    (-2.65, 0.75),
                    (-2.62, 1.40),
                    (-2.58, 2.06),
                    (-2.40, 2.10),
                    (1.20, 2.10),
                    (1.80, 1.20),
                    (2.40, 1.06),
                    (2.65, 0.72),
                ],
                cowl: 1.80,
                a: 1.20,
                c: 0.40,
                deck: 0.30,
                roof_width: 0.86,
                window_inset: 0.05,
                nose: 0.45,
                tail: 0.15,
                ends: (0.35, 0.80),
            },
            Kind::Coupe => Shape {
                length: 4.55,
                width: 1.92,
                wheelbase: 2.70,
                axle_shift: -0.05,
                wheel_radius: 0.35,
                tire_width: 0.28,
                floor: 0.13,
                belt: 0.86,
                top: &[
                    (-2.275, 0.62),
                    (-2.22, 0.92),
                    (-2.00, 0.98),
                    (-1.10, 1.22),
                    (-0.45, 1.29),
                    (0.30, 1.27),
                    (0.95, 0.88),
                    (1.60, 0.78),
                    (2.20, 0.66),
                    (2.275, 0.46),
                ],
                cowl: 0.95,
                a: 0.30,
                c: -0.45,
                deck: -1.70,
                roof_width: 0.64,
                window_inset: 0.12,
                nose: 0.6,
                tail: 0.4,
                ends: (0.26, 0.58),
            },
            Kind::Muscle => Shape {
                length: 5.00,
                width: 1.96,
                wheelbase: 2.95,
                axle_shift: -0.10,
                wheel_radius: 0.37,
                tire_width: 0.29,
                floor: 0.15,
                belt: 0.94,
                top: &[
                    (-2.50, 0.66),
                    (-2.46, 0.96),
                    (-2.20, 1.00),
                    (-1.45, 1.03),
                    (-0.90, 1.33),
                    (-0.15, 1.36),
                    (0.50, 0.95),
                    (1.40, 0.92),
                    (2.30, 0.86),
                    (2.50, 0.62),
                ],
                cowl: 0.50,
                a: -0.15,
                c: -0.90,
                deck: -1.45,
                roof_width: 0.66,
                window_inset: 0.11,
                nose: 0.35,
                tail: 0.3,
                ends: (0.28, 0.70),
            },
            Kind::Supercar => Shape {
                length: 4.60,
                width: 2.02,
                wheelbase: 2.70,
                axle_shift: -0.10,
                wheel_radius: 0.35,
                tire_width: 0.32,
                floor: 0.11,
                belt: 0.82,
                top: &[
                    (-2.30, 0.60),
                    (-2.25, 0.92),
                    (-1.90, 0.96),
                    (-0.75, 1.10),
                    (-0.20, 1.16),
                    (0.40, 1.12),
                    (1.20, 0.72),
                    (2.10, 0.56),
                    (2.30, 0.40),
                ],
                cowl: 1.20,
                a: 0.40,
                c: -0.20,
                deck: -0.75,
                roof_width: 0.60,
                window_inset: 0.14,
                nose: 0.75,
                tail: 0.35,
                ends: (0.22, 0.50),
            },
        }
    }
}

/// A body's measurements (metres; z along the car, y up).
#[derive(Clone, Copy, Debug)]
pub struct Shape {
    pub length: f32,
    pub width: f32,
    pub wheelbase: f32,
    /// Where the axles' middle is, forward of the body's.
    pub axle_shift: f32,
    pub wheel_radius: f32,
    pub tire_width: f32,
    /// The underside's height.
    pub floor: f32,
    /// The shoulder (the side windows' lower edge).
    pub belt: f32,
    /// The top of the body along its middle, `(z, y)` from the tail to
    /// the nose.
    pub top: &'static [(f32, f32)],
    /// The windscreen from the cowl back to the roof's front (`a`); the
    /// rear window from the roof's back (`c`) to the deck.
    pub cowl: f32,
    pub a: f32,
    pub c: f32,
    pub deck: f32,
    /// Half the roof's width.
    pub roof_width: f32,
    /// How far the side windows' foot is set in from the shoulder.
    pub window_inset: f32,
    /// How far the plan rounds in at the nose and the tail.
    pub nose: f32,
    pub tail: f32,
    /// The bumpers' faces at the very ends: from this height to that.
    pub ends: (f32, f32),
}

impl Shape {
    /// The axles' z: front, rear.
    pub fn axles(&self) -> (f32, f32) {
        (self.axle_shift + self.wheelbase * 0.5, self.axle_shift - self.wheelbase * 0.5)
    }

    /// Half the distance between a pair of wheels' middles.
    pub fn half_track(&self) -> f32 {
        self.width * 0.5 - self.tire_width * 0.5 - 0.04
    }

    /// The top of the body's middle at `z`.
    pub fn top_at(&self, z: f32) -> f32 {
        let t = self.top;
        if z <= t[0].0 {
            return t[0].1;
        }
        for w in t.windows(2) {
            let ((z0, y0), (z1, y1)) = (w[0], w[1]);
            if z <= z1 {
                let u = (z - z0) / (z1 - z0).max(1e-4);
                // Smooth between the points.
                let u = u * u * (3.0 - 2.0 * u) * 0.5 + u * 0.5;
                return y0 + (y1 - y0) * u;
            }
        }
        t[t.len() - 1].1
    }

    /// Half the body's width at `z`: rounding in towards the ends.
    fn half_width(&self, z: f32) -> f32 {
        let half = self.length * 0.5;
        let w = self.width * 0.5;
        let into = if z > 0.0 { (z - (half - self.nose)) / self.nose } else { (-z - (half - self.tail)) / self.tail };
        if into <= 0.0 {
            return w;
        }
        let into = into.min(1.0);
        // A quarter ellipse, down to 80% at the very end.
        w * (0.80 + 0.20 * (1.0 - into * into).max(0.0).sqrt())
    }

    /// The wheel arch's top at `z` (`floor` where there is no wheel).
    fn arch(&self, z: f32) -> f32 {
        let r = self.wheel_radius + 0.07;
        let (f, b) = self.axles();
        let mut y = self.floor;
        for axle in [f, b] {
            let d = (z - axle) / r;
            if d.abs() < 1.0 {
                y = y.max(self.wheel_radius + r * (1.0 - d * d).sqrt() * 0.92);
            }
        }
        y
    }
}

/// A cross-section's points (one side, from the floor's middle to the
/// roof's), and the material of the strip that leaves each point
/// upwards, as the loft needs them.
struct Section {
    points: Vec<Vec2>,
}

/// How many points a cross-section has on one side.
const POINTS: usize = 16;

/// Where on a section the glasshouse begins (the point at the shoulder)
/// and where its sides meet the roof.
const SHOULDER: usize = 8;
const ROOF_EDGE: usize = 12;

fn section(s: &Shape, z: f32) -> Section {
    let w = s.half_width(z);
    let half = s.length * 0.5;
    // Towards the very ends the section shrinks to the bumper's face.
    let end = ((z.abs() - (half - 0.12)) / 0.12).clamp(0.0, 1.0);
    let top = s.top_at(z);
    let floor = s.floor + end * (s.ends.0 - s.floor);
    let arch = s.arch(z).max(floor);
    let belt = s.belt.min(top - 0.01);
    let has_glass = top > s.belt + 0.03;
    let w_in = w - 0.30;
    let r = 0.06;
    let mut p = Vec::with_capacity(POINTS);
    p.push(Vec2::new(0.0, floor));
    p.push(Vec2::new(w_in, floor));
    p.push(Vec2::new(w_in, arch));
    p.push(Vec2::new(w - r, arch));
    p.push(Vec2::new(w - 0.01, arch + r));
    // The door: a little fuller at the middle.
    let mid = (arch + belt) * 0.5;
    p.push(Vec2::new(w + 0.012, mid));
    p.push(Vec2::new(w, belt - r));
    p.push(Vec2::new(w - r * 0.5, belt - r * 0.2));
    // The shoulder.
    p.push(Vec2::new(w - r * 1.6, belt));
    if has_glass {
        // The glasshouse, leaning in, rounding over to the roof.
        let rw = s.roof_width.min(w - s.window_inset - 0.05);
        let foot = w - s.window_inset;
        let gh = top - belt;
        p.push(Vec2::new(foot, belt + 0.02));
        p.push(Vec2::new(foot + (rw - foot) * 0.45, belt + gh * 0.45));
        p.push(Vec2::new(rw + 0.02, top - 0.07));
        p.push(Vec2::new(rw - 0.06, top - 0.012));
        p.push(Vec2::new(rw * 0.66, top + 0.01));
        p.push(Vec2::new(rw * 0.33, top + 0.02));
        p.push(Vec2::new(0.0, top + 0.024));
    } else {
        // The bonnet or the boot lid: crowned a little.
        let t = top.max(belt);
        p.push(Vec2::new(w - r * 2.6, t + 0.004));
        p.push(Vec2::new(w * 0.86, t + 0.008));
        p.push(Vec2::new(w * 0.72, t + 0.012));
        p.push(Vec2::new(w * 0.55, t + 0.016));
        p.push(Vec2::new(w * 0.36, t + 0.02));
        p.push(Vec2::new(w * 0.18, t + 0.022));
        p.push(Vec2::new(0.0, t + 0.024));
    }
    debug_assert_eq!(p.len(), POINTS);
    // At the very ends everything shrinks between the bumper's top and
    // bottom.
    if end > 0.0 {
        for q in &mut p {
            q.y = q.y.min(s.ends.1 + (top - s.ends.1) * (1.0 - end)).max(floor);
        }
    }
    Section { points: p }
}

/// The stations along the body, closer together where the shape changes.
fn stations(s: &Shape) -> Vec<f32> {
    let half = s.length * 0.5;
    let mut z = vec![-half];
    let mut keys: Vec<f32> = s.top.iter().map(|p| p.0).collect();
    let (f, b) = s.axles();
    let r = s.wheel_radius + 0.07;
    for axle in [f, b] {
        for k in -6..=6 {
            keys.push(axle + r * k as f32 / 6.0);
        }
    }
    keys.extend_from_slice(&[s.cowl, s.a, s.c, s.deck, -half + 0.06, half - 0.06, -half + 0.12, half - 0.12]);
    keys.push(-half + s.tail * 0.5);
    keys.push(half - s.nose * 0.5);
    keys.retain(|k| k.abs() < half);
    keys.push(half);
    keys.sort_by(f32::total_cmp);
    // Fill gaps longer than 12 cm.
    for &k in &keys {
        let last = *z.last().unwrap();
        let gap = k - last;
        if gap < 0.01 {
            continue;
        }
        let n = (gap / 0.12).ceil() as usize;
        for i in 1..=n {
            z.push(last + gap * i as f32 / n as f32);
        }
    }
    z
}

/// What a strip of the body (from section point `k` up to `k + 1`) is
/// made of at `z`.
fn material(s: &Shape, k: usize, z: f32, top: f32) -> u8 {
    let has_glass = top > s.belt + 0.03;
    if k < 3 {
        // Floor, liner, the arch's roof.
        return part::DARK;
    }
    if k == 3 {
        // The sill's lower edge.
        return part::TRIM;
    }
    if !has_glass || k < SHOULDER {
        return part::PAINT;
    }
    if k == SHOULDER {
        // The seal under the side window.
        return part::TRIM;
    }
    if k == ROOF_EDGE - 1 {
        // The frame over the side windows.
        return part::TRIM;
    }
    let side = k < ROOF_EDGE - 1;
    if side {
        // Side windows between the pillars, a B pillar in the middle.
        let b = (s.a + s.c) * 0.5;
        let pillar = 0.07;
        if z < s.a - pillar && z > s.c + pillar * 0.6 && (z - b).abs() > 0.05 {
            return part::GLASS;
        }
        if z <= s.cowl && z >= s.deck && !(z < s.a && z > s.c) {
            // The A and C pillars.
            return part::TRIM;
        }
        return part::TRIM;
    }
    // The top: windscreen, roof, rear window.
    if z > s.a + 0.03 && z < s.cowl - 0.02 {
        return part::GLASS;
    }
    if z < s.c - 0.03 && z > s.deck + 0.03 {
        return part::GLASS;
    }
    part::PAINT
}

/// A car's meshes: the body (every material, in the vertices' first
/// parameter), and one wheel (at the origin, axle along x) the renderer
/// draws four times.
pub struct CarMesh {
    pub body: Mesh,
    pub wheel: Mesh,
    /// Where each wheel's middle is (front left, front right, rear left,
    /// rear right).
    pub wheels: [Vec3; 4],
}

/// The meshes of a car of `kind`.
pub fn build(kind: Kind) -> CarMesh {
    let s = kind.shape();
    let zs = stations(&s);
    let sections: Vec<Section> = zs.iter().map(|&z| section(&s, z)).collect();
    let mut body = Mesh::new();

    // The grid of positions (both halves): for each station, the points
    // from the left side's floor middle up to the roof middle, then down
    // the right side.
    let ring = POINTS * 2 - 1;
    let pos = |i: usize, j: usize| -> Vec3 {
        let p = &sections[i].points;
        let (k, side) = if j < POINTS { (j, 1.0) } else { (ring - 1 - j, -1.0) };
        let q = p[k];
        Vec3::new(q.x * side, q.y, zs[i])
    };
    // Smooth normals from the grid's neighbours.
    let n_st = zs.len();
    let normal = |i: usize, j: usize| -> Vec3 {
        let along = pos((i + 1).min(n_st - 1), j) - pos(i.saturating_sub(1), j);
        let around = pos(i, (j + 1).min(ring - 1)) - pos(i, j.saturating_sub(1));
        // Around runs up the left side (+x), over the roof and down the
        // right: around x along points out (up x forward = left).
        around.cross(along).normalize_or(Vec3::Y)
    };
    for i in 0..n_st - 1 {
        for j in 0..ring - 1 {
            let k = if j < POINTS - 1 { j } else { ring - 2 - j };
            let zm = (zs[i] + zs[i + 1]) * 0.5;
            let m = material(&s, k, zm, s.top_at(zm));
            let quad = [(i, j), (i + 1, j), (i + 1, j + 1), (i, j + 1)];
            let base = body.base();
            for (a, b) in quad {
                let p = pos(a, b);
                // Hard edges between materials: each quad its own corners,
                // with the grid's smooth normals.
                let n = if m == part::GLASS { flat_normal(&pos, &quad) } else { normal(a, b) };
                body.push(p, n, Vec2::new(p.z, p.y), [m, 0, 0, 0]);
            }
            // Outward: along z then around (the left side upwards).
            body.quad(base, base + 3, base + 2, base + 1);
        }
    }
    // The ends: fans closing the first and last sections.
    for (i, forward) in [(0usize, false), (n_st - 1, true)] {
        let center = {
            let mut c = Vec3::ZERO;
            for j in 0..ring {
                c += pos(i, j);
            }
            c / ring as f32
        };
        let n = if forward { Vec3::Z } else { Vec3::NEG_Z };
        let mid = body.push(center, n, Vec2::new(center.z, center.y), [part::PAINT, 0, 0, 0]);
        let first = body.base();
        for j in 0..ring {
            let p = pos(i, j);
            body.push(p, (n * 2.0 + normal(i, j)).normalize(), Vec2::new(p.x, p.y), [part::PAINT, 0, 0, 0]);
        }
        // The ring turns counter-clockwise seen from the front (the car's
        // left is on the right then), clockwise seen from behind.
        for j in 0..ring as u32 - 1 {
            if forward {
                body.triangle(mid, first + j, first + j + 1);
            } else {
                body.triangle(mid, first + j + 1, first + j);
            }
        }
    }
    add_details(&mut body, kind, &s);
    let (f, b) = s.axles();
    let t = s.half_track();
    let r = s.wheel_radius;
    let wheels = [Vec3::new(t, r, f), Vec3::new(-t, r, f), Vec3::new(t, r, b), Vec3::new(-t, r, b)];
    CarMesh { body, wheel: wheel(&s), wheels }
}

fn flat_normal(pos: &dyn Fn(usize, usize) -> Vec3, quad: &[(usize, usize); 4]) -> Vec3 {
    let p: Vec<Vec3> = quad.iter().map(|&(a, b)| pos(a, b)).collect();
    (p[1] - p[0]).cross(p[3] - p[0]).normalize_or(Vec3::Y) * -1.0
}

/// A box part from `lo` to `hi` (car space) of material `m`.
fn part_box(body: &mut Mesh, lo: Vec3, hi: Vec3, m: u8) {
    body.append(&mesh::cuboid(lo, hi, [m, 0, 0, 0]));
}

/// Lights, grille, plates, mirrors; the police's light bar and push bar;
/// the taxi's sign.
fn add_details(body: &mut Mesh, kind: Kind, s: &Shape) {
    let half = s.length * 0.5;
    let w = s.width * 0.5;
    let (lo, hi) = s.ends;
    let face_y = (lo + hi) * 0.5;
    // Headlights: at the front corners, just above the bumper's middle.
    let hl_y = (hi - 0.06).min(s.top_at(half - 0.1) - 0.05);
    for side in [-1.0f32, 1.0] {
        let x0 = side * (w * 0.80 - 0.02);
        let x1 = side * (w * 0.80 - 0.38);
        let (a, b) = (x0.min(x1), x0.max(x1));
        part_box(
            body,
            Vec3::new(a, hl_y - 0.09, half - 0.16),
            Vec3::new(b, hl_y + 0.03, half - 0.005),
            part::HEADLIGHT,
        );
        // Indicators beside them.
        let (c, d) = if side > 0.0 { (b, b + 0.08) } else { (a - 0.08, a) };
        part_box(body, Vec3::new(c, hl_y - 0.07, half - 0.17), Vec3::new(d, hl_y + 0.0, half - 0.02), part::INDICATOR);
        // Taillights: wide, at the rear corners.
        let t_y = (hi - 0.02).min(s.top_at(-half + 0.1) - 0.04);
        let x2 = side * (w * 0.82 - 0.01);
        let x3 = side * (w * 0.82 - 0.42);
        let (e, f) = (x2.min(x3), x2.max(x3));
        part_box(body, Vec3::new(e, t_y - 0.13, -half + 0.005), Vec3::new(f, t_y, -half + 0.12), part::TAILLIGHT);
        // Mirrors on the doors' front corners.
        let mz = s.a + (s.cowl - s.a) * 0.15;
        let mx = side * (w + 0.11);
        let (g, h) = (mx.min(side * (w - 0.02)), mx.max(side * (w - 0.02)));
        part_box(body, Vec3::new(g, s.belt + 0.04, mz - 0.07), Vec3::new(h, s.belt + 0.17, mz + 0.06), part::TRIM);
    }
    // The grille and the lower intake.
    let gw = w * 0.42;
    part_box(body, Vec3::new(-gw, face_y - 0.09, half - 0.06), Vec3::new(gw, hl_y - 0.02, half + 0.006), part::DARK);
    part_box(body, Vec3::new(-w * 0.6, lo + 0.02, half - 0.08), Vec3::new(w * 0.6, lo + 0.1, half + 0.004), part::DARK);
    // Plates.
    part_box(body, Vec3::new(-0.26, lo + 0.1, half - 0.03), Vec3::new(0.26, lo + 0.22, half + 0.012), part::PLATE);
    part_box(
        body,
        Vec3::new(-0.26, face_y - 0.04, -half - 0.012),
        Vec3::new(0.26, face_y + 0.08, -half + 0.03),
        part::PLATE,
    );
    // The lower bumpers' black strips.
    part_box(
        body,
        Vec3::new(-w * 0.82, lo - 0.02, -half - 0.01),
        Vec3::new(w * 0.82, lo + 0.06, -half + 0.08),
        part::TRIM,
    );
    match kind {
        Kind::Police => {
            // The light bar across the roof, behind the windscreen: a
            // base, then red to the left, blue to the right.
            let roof = s.top_at(s.a - 0.25);
            let z0 = s.a - 0.42;
            let z1 = s.a - 0.14;
            part_box(
                body,
                Vec3::new(-0.62, roof - 0.01, z0 - 0.04),
                Vec3::new(0.62, roof + 0.05, z1 + 0.04),
                part::TRIM,
            );
            part_box(body, Vec3::new(0.03, roof + 0.05, z0), Vec3::new(0.60, roof + 0.15, z1), part::BAR_RED);
            part_box(body, Vec3::new(-0.60, roof + 0.05, z0), Vec3::new(-0.03, roof + 0.15, z1), part::BAR_BLUE);
            // Its feet.
            for x in [-0.5f32, 0.5] {
                part_box(
                    body,
                    Vec3::new(x - 0.05, roof - 0.02, z0 + 0.02),
                    Vec3::new(x + 0.05, roof + 0.01, z1 - 0.02),
                    part::TRIM,
                );
            }
            // The push bar on the nose.
            let front = half + 0.10;
            part_box(body, Vec3::new(-0.62, lo - 0.02, front - 0.05), Vec3::new(0.62, lo + 0.06, front), part::TRIM);
            part_box(
                body,
                Vec3::new(-0.62, hl_y - 0.02, front - 0.05),
                Vec3::new(0.62, hl_y + 0.04, front),
                part::TRIM,
            );
            for x in [-0.5f32, 0.5] {
                part_box(
                    body,
                    Vec3::new(x - 0.04, lo - 0.02, front - 0.08),
                    Vec3::new(x + 0.04, hl_y + 0.1, front),
                    part::TRIM,
                );
            }
            // Strobes in the grille, and a spotlight by the mirror.
            part_box(
                body,
                Vec3::new(-0.30, hl_y - 0.07, half + 0.006),
                Vec3::new(-0.16, hl_y - 0.02, half + 0.02),
                part::BAR_BLUE,
            );
            part_box(
                body,
                Vec3::new(0.16, hl_y - 0.07, half + 0.006),
                Vec3::new(0.30, hl_y - 0.02, half + 0.02),
                part::BAR_RED,
            );
            part_box(
                body,
                Vec3::new(w - 0.05, s.belt + 0.12, s.a + 0.05),
                Vec3::new(w + 0.06, s.belt + 0.22, s.a + 0.2),
                part::CHROME,
            );
            // An antenna on the boot.
            part_box(
                body,
                Vec3::new(-0.01, s.top_at(-half + 0.5), -half + 0.48),
                Vec3::new(0.01, s.top_at(-half + 0.5) + 0.7, -half + 0.5),
                part::TRIM,
            );
        }
        Kind::Taxi => {
            let roof = s.top_at(s.a - 0.5);
            let z = s.a - 0.55;
            part_box(body, Vec3::new(-0.25, roof, z - 0.12), Vec3::new(0.25, roof + 0.2, z + 0.12), part::HEADLIGHT);
        }
        Kind::Suv | Kind::Estate | Kind::Van => {
            // Roof rails.
            for side in [-1.0f32, 1.0] {
                let x = side * (s.roof_width - 0.08);
                let (a, b) = (x.min(x + side * 0.05), x.max(x + side * 0.05));
                let roof = s.top_at((s.a + s.c) * 0.5);
                part_box(body, Vec3::new(a, roof, s.c + 0.1), Vec3::new(b, roof + 0.06, s.a - 0.1), part::TRIM);
            }
        }
        Kind::Supercar | Kind::Coupe => {
            // A wing on the tail.
            let y = s.top_at(-half + 0.3) + 0.12;
            part_box(
                body,
                Vec3::new(-w * 0.85, y, -half + 0.08),
                Vec3::new(w * 0.85, y + 0.035, -half + 0.4),
                part::TRIM,
            );
            for x in [-0.55f32, 0.55] {
                part_box(
                    body,
                    Vec3::new(x - 0.02, y - 0.13, -half + 0.2),
                    Vec3::new(x + 0.02, y, -half + 0.32),
                    part::TRIM,
                );
            }
        }
        _ => {}
    }
}

/// One wheel: a tyre with rounded shoulders and a rim with five spokes,
/// its axle along x, its middle at the origin.
fn wheel(s: &Shape) -> Mesh {
    let r = s.wheel_radius;
    let w = s.tire_width;
    let rim_r = r * 0.66;
    // The tyre's profile (radius, along the axle), turned about the axle.
    let tire_profile = [
        (rim_r, -w * 0.5),
        (r - 0.04, -w * 0.5),
        (r - 0.01, -w * 0.44),
        (r, -w * 0.36),
        (r, w * 0.36),
        (r - 0.01, w * 0.44),
        (r - 0.04, w * 0.5),
        (rim_r, w * 0.5),
    ];
    let to_x = Mat4::from_rotation_z(-core::f32::consts::FRAC_PI_2);
    let mut m = Mesh::new();
    // `lathe` turns about y; profile y is along the axle.
    let tire = mesh::lathe(&tire_profile, 24, false, [part::TIRE, 0, 0, 0]);
    m.append_transformed(&tire, &to_x);
    // The rim: a dish, the barrel inside the tyre, a hub, spokes.
    let dish = [(rim_r, w * 0.42), (rim_r * 0.95, w * 0.40), (rim_r * 0.3, w * 0.30), (0.0, w * 0.32)];
    let rim = mesh::lathe(&dish, 20, false, [part::RIM, 0, 0, 0]);
    m.append_transformed(&rim, &to_x);
    let barrel = mesh::lathe(&[(rim_r, w * 0.45), (rim_r, -w * 0.45)], 20, false, [part::DARK, 0, 0, 0]);
    m.append_transformed(&barrel, &to_x);
    for k in 0..5 {
        let a = k as f32 / 5.0 * core::f32::consts::TAU;
        let spoke = mesh::cuboid(
            Vec3::new(w * 0.34, -0.025, rim_r * 0.25),
            Vec3::new(w * 0.46, 0.025, rim_r * 0.98),
            [part::RIM, 0, 0, 0],
        );
        m.append_transformed(&spoke, &Mat4::from_rotation_x(a));
    }
    // The other side: the inner face of the tyre's sidewall is closed by
    // the barrel; mirror the dish for wheels on the right is done by the
    // renderer turning them round.
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bodies_are_closed_and_face_out() {
        for kind in Kind::ALL {
            let c = build(kind);
            let s = kind.shape();
            let (lo, hi) = c.body.bounds();
            // About its measurements.
            assert!((hi.z - lo.z - s.length).abs() < 0.3, "{kind:?}: length {}", hi.z - lo.z);
            assert!((hi.x - lo.x) < s.width + 0.3 && (hi.x - lo.x) > s.width - 0.2, "{kind:?}: width {}", hi.x - lo.x);
            // The loft's faces point away from the car's middle line.
            let mut out = 0;
            let mut inward = 0;
            for t in c.body.indices.chunks(3) {
                let (a, b, cc) =
                    (c.body.vertices[t[0] as usize], c.body.vertices[t[1] as usize], c.body.vertices[t[2] as usize]);
                let n = (b.pos - a.pos).cross(cc.pos - a.pos);
                if n.length() < 1e-7 {
                    continue;
                }
                let mid = (a.pos + b.pos + cc.pos) / 3.0;
                let from_axis = Vec3::new(mid.x, mid.y - (lo.y + hi.y) * 0.5, 0.0);
                if n.dot(from_axis) >= 0.0 { out += 1 } else { inward += 1 }
            }
            assert!(out > inward * 3, "{kind:?}: {out} faces out, {inward} in");
            // Some glass, some paint.
            assert!(c.body.vertices.iter().any(|v| v.params[0] == part::GLASS), "{kind:?}: no glass");
            assert!(c.body.vertices.iter().any(|v| v.params[0] == part::PAINT));
            assert!(!c.wheel.is_empty());
        }
    }
}
