//! Cars in Veda Bay: the ground under them, the walls they cannot pass,
//! and their crashes into the walls and into each other.
//!
//! Crashes are resolved as impulses between rigid bodies at the point of
//! contact, with friction, so that a car struck at a rear corner spins, a
//! car scraping a wall slows and turns along it, and a heavy van shoves a
//! small car aside. Each crash is reported ([`Impact`]) for the damage,
//! the sounds and the sparks.

use crate::prelude::*;

use crate::road::{KERB, Road};
use crate::vehicle::{Ground, Vehicle};
use crate::world::World;

/// Half the median barrier's width at a car's height.
pub const MEDIAN_HALF: f32 = 0.32;

/// A crash.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Impact {
    /// Where (on the ground).
    pub point: Vec3,
    /// Away from what was hit.
    pub normal: Vec2,
    /// How fast the two were closing (m/s).
    pub speed: f32,
    /// The impulse exchanged (N s).
    pub impulse: f32,
    /// How fast they slid along each other (m/s): scraping.
    pub slide: f32,
}

/// Closing slower than this (m/s), the bumpers take it: no harm done
/// (cars leaning on each other, pushing, touching).
const HARMLESS: f32 = 2.0;

impl Impact {
    /// How hard it was, 0 (a touch) to 1 (a head-on at 90 km/h).
    pub fn hit(&self) -> f32 {
        ((self.speed - HARMLESS) / 23.0).clamp(0.0, 1.0)
    }

    /// The share of a car it wrecks (0..0.25).
    pub fn harm(&self) -> f32 {
        let h = self.hit();
        h * h * 0.25
    }
}

impl World {
    /// The ground at `p`, on the road near sample `hint`: the tarmac
    /// (banked), the kerb and the pavement downtown, the verges' gravel
    /// elsewhere.
    pub fn ground(&self, p: Vec3, hint: usize) -> Ground {
        let at = self.road.locate(p, Some(hint));
        let sample = self.road.sample(at.index as isize);
        let (paved, _) = self.road.widths(at.s);
        let a = at.lateral.abs();
        let up = sample.up;
        if a <= paved {
            return Ground { height: at.height, normal: up, grip: 1.0 };
        }
        if sample.pavement >= 0.5 {
            return Ground { height: at.height + KERB, normal: up, grip: 0.95 };
        }
        // Gravel falling away from the tarmac.
        let drop = ((a - paved) * 0.18).min(0.6);
        Ground { height: at.height - drop, normal: up, grip: 0.7 }
    }
}

/// The ground-plane's normal pointing back towards the road's middle from
/// lateral offset `lateral` at `s`.
fn inwards(road: &Road, s: f32, lateral: f32) -> Vec2 {
    let (_, _, r, _) = road.frame(s);
    let flat = Vec2::new(r.x, r.z).normalize_or(Vec2::X);
    flat * -lateral.signum()
}

/// The 2D cross product `a x b` as the torque about +y of a force `b` at
/// offset `a` (x and z in `.x` and `.y`).
fn cross(a: Vec2, b: Vec2) -> f32 {
    a.y * b.x - a.x * b.y
}

/// The velocity of the point at offset `r` from a body's centre.
fn point_velocity(v: &Vehicle, r: Vec2) -> Vec2 {
    v.vel + Vec2::new(v.yaw_rate * r.y, -v.yaw_rate * r.x)
}

/// Applies impulse `j` at offset `r` to `v`.
fn apply(v: &mut Vehicle, r: Vec2, j: Vec2) {
    v.vel += j / v.spec.mass;
    v.yaw_rate += cross(r, j) / v.spec.inertia;
}

/// Keeps `v` inside the road's walls and on its side of a median barrier;
/// `hint` is the sample it was near. Returns the hardest crash, if any.
pub fn collide_walls(world: &World, v: &mut Vehicle, hint: usize) -> Option<Impact> {
    let road = &world.road;
    let center = road.locate(v.pos, Some(hint));
    let side = center.lateral.signum();
    let mut hardest: Option<Impact> = None;
    for corner in v.corners() {
        let p = Vec3::new(corner.x, v.pos.y, corner.y);
        let at = road.locate(p, Some(center.index));
        let (_, wall) = road.widths(at.s);
        let mut contact = None;
        if at.lateral.abs() > wall {
            contact = Some((at.lateral.abs() - wall, inwards(road, at.s, at.lateral)));
        } else if road.sample(at.index as isize).median >= 0.5 && at.lateral * side < MEDIAN_HALF {
            // The barrier down the middle: back to the car's own side.
            contact = Some((MEDIAN_HALF - at.lateral * side, inwards(road, at.s, -side)));
        }
        let Some((depth, n)) = contact else { continue };
        // Out of the wall.
        v.pos.x += n.x * depth;
        v.pos.z += n.y * depth;
        let r = corner + n * depth - v.pos.xz();
        let vp = point_velocity(v, r);
        let vn = vp.dot(n);
        if vn >= 0.0 {
            continue;
        }
        // A wall is immovable: the impulse stops the corner's motion into
        // it (with a little bounce), and friction along it.
        let rn = cross(r, n);
        let k = 1.0 / v.spec.mass + rn * rn / v.spec.inertia;
        let j = -(1.0 + 0.25) * vn / k;
        apply(v, r, n * j);
        let t = Vec2::new(-n.y, n.x);
        let vt = point_velocity(v, r).dot(t);
        let rt = cross(r, t);
        let kt = 1.0 / v.spec.mass + rt * rt / v.spec.inertia;
        let jt = (-vt / kt).clamp(-0.45 * j, 0.45 * j);
        apply(v, r, t * jt);
        let impact = Impact { point: p, normal: n, speed: -vn, impulse: j, slide: vt.abs() };
        if hardest.is_none_or(|h| h.speed < impact.speed) {
            hardest = Some(impact);
        }
    }
    hardest
}

/// The box's projection's half length on `axis`.
fn radius(v: &Vehicle, axis: Vec2) -> f32 {
    v.spec.half_length * v.forward().dot(axis).abs() + v.spec.half_width * v.left().dot(axis).abs()
}

/// Separates `a` and `b` if their boxes overlap and exchanges the impulse
/// of their crash. Returns it, if they crashed.
pub fn collide_cars(a: &mut Vehicle, b: &mut Vehicle) -> Option<Impact> {
    let d = b.pos.xz() - a.pos.xz();
    let reach = a.spec.half_length + a.spec.half_width + b.spec.half_length + b.spec.half_width;
    if d.length_squared() > reach * reach || (a.pos.y - b.pos.y).abs() > 2.0 {
        return None;
    }
    // Separating axes: each box's sides.
    let axes = [a.forward(), a.left(), b.forward(), b.left()];
    let mut best = (f32::MAX, Vec2::ZERO, 0usize);
    for (i, &axis) in axes.iter().enumerate() {
        let overlap = radius(a, axis) + radius(b, axis) - d.dot(axis).abs();
        if overlap <= 0.0 {
            return None;
        }
        if overlap < best.0 {
            let n = if d.dot(axis) < 0.0 { -axis } else { axis };
            best = (overlap, n, i);
        }
    }
    let (depth, n, axis) = best;
    // The point of contact: the corner of one box deepest into the other.
    let point = if axis < 2 {
        // An axis of a's: b's corner furthest back along n.
        b.corners().into_iter().min_by(|p, q| p.dot(n).total_cmp(&q.dot(n))).unwrap_or(b.pos.xz())
    } else {
        a.corners().into_iter().max_by(|p, q| p.dot(n).total_cmp(&q.dot(n))).unwrap_or(a.pos.xz())
    };
    // Apart, by their masses.
    let (ia, ib) = (1.0 / a.spec.mass, 1.0 / b.spec.mass);
    let push = depth / (ia + ib);
    a.pos.x -= n.x * push * ia;
    a.pos.z -= n.y * push * ia;
    b.pos.x += n.x * push * ib;
    b.pos.z += n.y * push * ib;
    let (ra, rb) = (point - a.pos.xz(), point - b.pos.xz());
    let rel = point_velocity(b, rb) - point_velocity(a, ra);
    let vn = rel.dot(n);
    if vn >= 0.0 {
        return None;
    }
    let (rna, rnb) = (cross(ra, n), cross(rb, n));
    let k = ia + ib + rna * rna / a.spec.inertia + rnb * rnb / b.spec.inertia;
    let j = -(1.0 + 0.2) * vn / k;
    apply(a, ra, -n * j);
    apply(b, rb, n * j);
    // Friction between the bodies.
    let t = Vec2::new(-n.y, n.x);
    let vt = (point_velocity(b, rb) - point_velocity(a, ra)).dot(t);
    let (rta, rtb) = (cross(ra, t), cross(rb, t));
    let kt = ia + ib + rta * rta / a.spec.inertia + rtb * rtb / b.spec.inertia;
    let jt = (-vt / kt).clamp(-0.35 * j, 0.35 * j);
    apply(a, ra, -t * jt);
    apply(b, rb, t * jt);
    let y = (a.pos.y + b.pos.y) * 0.5 + 0.5;
    Some(Impact { point: Vec3::new(point.x, y, point.y), normal: n, speed: -vn, impulse: j, slide: vt.abs() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vehicle::{Controls, Spec};

    #[test]
    fn a_rear_corner_hit_spins_the_car() {
        // b drives along +z; a, alongside on its left with its nose by b's
        // rear wheel, steers into b's rear quarter: b turns (a PIT).
        let mut b = Vehicle::new(Spec::sports(), Vec3::new(0.0, 0.0, 10.0), 0.0);
        b.set_speed(25.0);
        let mut a = Vehicle::new(Spec::interceptor(), Vec3::new(2.8, 0.0, 6.5), -0.3);
        a.set_speed(26.0);
        let flat = |_: Vec3| Ground { height: 0.0, normal: Vec3::Y, grip: 1.0 };
        let mut hit = None;
        for _ in 0..120 {
            a.step(&Controls::default(), 1.0 / 120.0, &flat);
            b.step(&Controls::default(), 1.0 / 120.0, &flat);
            if let Some(i) = collide_cars(&mut a, &mut b) {
                hit.get_or_insert(i);
            }
        }
        let hit = hit.expect("they never touched");
        assert!(hit.speed > 1.0, "closing at {}", hit.speed);
        assert!(
            b.yaw_rate.abs() > 0.3 || (b.yaw - 0.0).abs() > 0.3,
            "b did not turn: yaw {} rate {}",
            b.yaw,
            b.yaw_rate
        );
    }

    #[test]
    fn walls_hold_cars_and_bounce_them() {
        let world = World::new();
        let road = &world.road;
        // A car driving straight at the wall from the middle of the road.
        let s = 400.0;
        let (p, f, r, _) = road.frame(s);
        let yaw_out = r.x.atan2(r.z);
        let mut v = Vehicle::new(Spec::interceptor(), p + r * 5.0, yaw_out);
        v.set_speed(15.0);
        let mut hint = road.locate(v.pos, None).index;
        let mut crashed = false;
        for _ in 0..240 {
            let g = |q: Vec3| world.ground(q, hint);
            v.step(&Controls::default(), 1.0 / 120.0, &g);
            crashed |= collide_walls(&world, &mut v, hint).is_some();
            hint = road.locate(v.pos, Some(hint)).index;
        }
        assert!(crashed, "it never reached the wall");
        let at = road.locate(v.pos, Some(hint));
        let (_, wall) = road.widths(at.s);
        assert!(at.lateral.abs() < wall, "through the wall: {}", at.lateral);
        let _ = f;
    }
}
