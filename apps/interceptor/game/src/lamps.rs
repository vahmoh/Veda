//! The street lights: where they stand along the road and what they are
//! like. Downtown, warm white lamps on both kerbs; on the open roads,
//! sodium lamps on the outer verge; on divided roads, double lamps on the
//! median; in the tunnel, two rows of lights under its roof.

use alloc::vec::Vec;

use crate::prelude::*;
use crate::road::{Road, Section};

/// What a light is like.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A tall post with an arm over the road (and a white lamp).
    Post,
    /// A post on the median with an arm each way.
    Double,
    /// A fitting under a tunnel's roof.
    Ceiling,
}

impl Kind {
    /// Whether it is lit day and night (a tunnel's), not only after dark.
    pub fn always_lit(self) -> bool {
        self == Kind::Ceiling
    }
}

/// A street light.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Lamp {
    pub kind: Kind,
    /// The post's foot (the fitting's place for a ceiling light).
    pub base: Vec3,
    /// Which way the arm reaches (radians about the vertical, as a car's
    /// heading: towards `(sin, cos)`).
    pub yaw: f32,
    /// Where the light comes from: each lamp head.
    pub heads: [Vec3; 2],
    /// Its colour and strength (linear), and the radius of its pool (m).
    pub color: Vec3,
    pub radius: f32,
}

/// A post's height and its arm's reach.
pub const HEIGHT: f32 = 9.0;
pub const ARM: f32 = 2.2;

const WARM: Vec3 = Vec3::new(1.0, 0.86, 0.68);
const SODIUM: Vec3 = Vec3::new(1.0, 0.62, 0.26);

/// The lights along `road`.
pub fn place(road: &Road) -> Vec<Lamp> {
    let mut out = Vec::new();
    let depth = road.section_depths();
    let spacing = 34.0;
    let n = (road.length() / spacing) as usize;
    for k in 0..n {
        let s = k as f32 * spacing;
        let i = (s / road.spacing()) as usize % road.len();
        let sample = road.samples()[i];
        let (_, _, r, _) = road.frame(s);
        let flat_r = Vec3::new(r.x, 0.0, r.z).normalize_or(Vec3::X);
        match sample.section {
            Section::Tunnel => {
                // Lit only well inside; the mouths are lit by day.
                if depth[i] < 12.0 {
                    continue;
                }
                // Three fittings to every post's spacing, in two rows.
                for j in 0..3 {
                    let s = s + j as f32 * spacing / 3.0;
                    let (p, f, r, u) = road.frame(s);
                    let yaw = f.x.atan2(f.z);
                    for lateral in [-3.6f32, 3.6] {
                        let head = p + r * lateral + u * 6.9;
                        out.push(Lamp {
                            kind: Kind::Ceiling,
                            base: head,
                            yaw,
                            heads: [head, head],
                            color: SODIUM * 1.8,
                            radius: 12.0,
                        });
                    }
                }
            }
            _ if sample.median >= 0.5 => {
                // On the median, an arm over each carriageway.
                let (p, _, _, _) = road.frame(s);
                let base = p;
                let yaw = flat_r.x.atan2(flat_r.z);
                let reach = flat_r * 4.6;
                let top = Vec3::Y * (HEIGHT + 1.0);
                let color = if sample.section == Section::Bridge { WARM * 1.6 } else { SODIUM * 2.0 };
                out.push(Lamp {
                    kind: Kind::Double,
                    base,
                    yaw,
                    heads: [base + top + reach, base + top - reach],
                    color,
                    radius: 20.0,
                });
            }
            _ => {
                let city = sample.pavement >= 0.5;
                // Downtown on both kerbs; elsewhere on the outside of the
                // bends.
                let sides: &[f32] = if city {
                    &[1.0, -1.0]
                } else if sample.curvature > 0.0 {
                    &[-1.0]
                } else {
                    &[1.0]
                };
                for &side in sides {
                    let lateral = if city { sample.paved + 0.9 } else { sample.wall + 0.5 } * side;
                    let base = road.point(s, lateral);
                    let inward = flat_r * -side;
                    let yaw = inward.x.atan2(inward.z);
                    let head = base + Vec3::Y * HEIGHT + inward * ARM;
                    let color = if city { WARM * 2.2 } else { SODIUM * 2.2 };
                    out.push(Lamp { kind: Kind::Post, base, yaw, heads: [head, head], color, radius: 21.0 });
                }
            }
        }
    }
    out
}

/// The meshes of the street lights, by kind: built along +z (the arm's
/// reach), from the foot (a ceiling light from its fitting's middle).
/// Materials: 0 painted metal, 1 the lamps' glass, 2 concrete.
pub fn mesh(kind: Kind) -> crate::mesh::Mesh {
    use crate::mesh::{Mesh, cuboid, lathe};
    let mut m = Mesh::new();
    let metal = [0, 0, 0, 0];
    let glass = [1, 0, 0, 0];
    let concrete = [2, 0, 0, 0];
    let head = |m: &mut Mesh, z: f32, y: f32| {
        m.append(&cuboid(Vec3::new(-0.2, y - 0.05, z - 0.55), Vec3::new(0.2, y + 0.16, z + 0.15), metal));
        m.append(&cuboid(Vec3::new(-0.16, y - 0.09, z - 0.48), Vec3::new(0.16, y - 0.05, z + 0.08), glass));
    };
    match kind {
        Kind::Post => {
            m.append(&lathe(&[(0.26, 0.0), (0.26, 0.45), (0.2, 0.5)], 10, true, concrete));
            m.append(&lathe(&[(0.12, 0.45), (0.075, HEIGHT + 0.1), (0.0, HEIGHT + 0.16)], 10, false, metal));
            m.append(&cuboid(Vec3::new(-0.05, HEIGHT - 0.12, 0.0), Vec3::new(0.05, HEIGHT, ARM), metal));
            head(&mut m, ARM, HEIGHT - 0.1);
        }
        Kind::Double => {
            let top = HEIGHT + 1.0;
            m.append(&lathe(&[(0.18, 0.0), (0.18, 0.9), (0.14, 1.0)], 10, true, concrete));
            m.append(&lathe(&[(0.14, 0.9), (0.09, top + 0.1), (0.0, top + 0.2)], 10, false, metal));
            m.append(&cuboid(Vec3::new(-0.05, top - 0.12, -4.6), Vec3::new(0.05, top, 4.6), metal));
            head(&mut m, 4.6, top - 0.1);
            let mut other = Mesh::new();
            head(&mut other, 4.6, top - 0.1);
            m.append_transformed(&other, &Mat4::from_rotation_y(core::f32::consts::PI));
        }
        Kind::Ceiling => {
            m.append(&cuboid(Vec3::new(-0.28, 0.05, -0.7), Vec3::new(0.28, 0.25, 0.7), metal));
            m.append(&cuboid(Vec3::new(-0.24, 0.0, -0.64), Vec3::new(0.24, 0.05, 0.64), glass));
        }
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::VEDA_BAY;

    #[test]
    fn lamps_line_the_road_and_stand_off_the_tarmac() {
        let road = Road::new(VEDA_BAY);
        let lamps = place(&road);
        assert!(lamps.len() > 150, "{} lamps", lamps.len());
        assert!(lamps.iter().any(|l| l.kind == Kind::Ceiling));
        assert!(lamps.iter().any(|l| l.kind == Kind::Double));
        for l in &lamps {
            if l.kind == Kind::Post {
                let at = road.locate(l.base, None);
                let (paved, _) = road.widths(at.s);
                assert!(at.lateral.abs() > paved, "a post on the tarmac at {:?}", l.base);
                assert!(l.heads[0].y > l.base.y + 8.0);
            }
        }
    }
}
