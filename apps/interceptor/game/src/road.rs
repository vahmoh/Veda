//! The road: one closed loop through Veda Bay, its centre line sampled
//! every [`SPACING`] metres.
//!
//! The loop is a centripetal Catmull-Rom spline through the layout's
//! control points (`layout`), smoothed and resampled at equal steps, with
//! its heights eased into gentle grades and its corners banked. Every
//! sample knows its frame (forward, right and up), its curvature, and the
//! kind of road it is ([`Section`]), which decides the cross-section
//! ([`Profile`]): where the lanes are, how far the tarmac goes, and where
//! the walls are that cars cannot pass.
//!
//! Positions on the road are given as `(s, lateral)`: the distance along
//! the centre line from the start (wrapping at [`Road::length`]) and the
//! signed distance across it, positive to the right of the direction of
//! travel. Traffic keeps to the right: lanes with a positive lateral
//! offset go forward (increasing `s`), the others the other way.

use alloc::vec;
use alloc::vec::Vec;

use crate::prelude::*;

/// Distance between samples (m), before the loop's length is divided
/// evenly.
pub const SPACING: f32 = 2.0;
/// Width of a lane (m).
pub const LANE_WIDTH: f32 = 3.6;
/// Lanes in each direction.
pub const LANES: usize = 2;
/// Half the width of the centre markings (or the median barrier).
pub const MEDIAN: f32 = 0.3;
/// Where the outer lane ends (the edge line).
pub const EDGE: f32 = MEDIAN + LANE_WIDTH * LANES as f32;
/// How far around a change of section its cross-section blends.
const BLEND: f32 = 30.0;

/// What kind of road a stretch is: it decides the cross-section and what
/// stands beside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    /// Downtown avenues: kerbs, pavements, towers.
    City,
    /// Up through the hills: guardrails, trees, houses.
    Hills,
    /// Through the headland.
    Tunnel,
    /// Along the cliffs above the sea.
    Coast,
    /// The harbour bridge: a divided deck high over the water.
    Bridge,
    /// The container port: a divided road between the stacks.
    Port,
}

impl Section {
    /// The cross-section of this kind of road.
    pub const fn profile(self) -> Profile {
        match self {
            Section::City => Profile { paved: 9.0, wall: 14.0, median: false, pavement: true },
            Section::Hills => Profile { paved: 9.0, wall: 10.4, median: false, pavement: false },
            Section::Tunnel => Profile { paved: 8.6, wall: 9.6, median: false, pavement: false },
            Section::Coast => Profile { paved: 9.0, wall: 10.4, median: false, pavement: false },
            Section::Bridge => Profile { paved: 8.6, wall: 9.2, median: true, pavement: false },
            Section::Port => Profile { paved: 9.4, wall: 11.0, median: true, pavement: false },
        }
    }

    /// The speed limit traffic keeps to here (m/s).
    pub const fn speed_limit(self) -> f32 {
        match self {
            Section::City => 15.3,
            Section::Hills | Section::Coast => 22.2,
            Section::Tunnel | Section::Port => 22.2,
            Section::Bridge => 27.8,
        }
    }
}

/// A cross-section, symmetric about the centre line.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Profile {
    /// The tarmac (lanes and shoulder) ends this far from the centre line.
    pub paved: f32,
    /// Cars cannot go further than this from the centre line: a wall, a
    /// barrier, a railing or the buildings.
    pub wall: f32,
    /// A barrier down the middle: cars keep to their side.
    pub median: bool,
    /// A raised pavement between the tarmac and the wall.
    pub pavement: bool,
}

/// Height of a kerb (m).
pub const KERB: f32 = 0.15;

/// One sample of the centre line.
#[derive(Clone, Copy, Debug)]
pub struct Sample {
    /// The centre line, on the road's surface.
    pub pos: Vec3,
    /// Unit vectors: along the road, across it (to the right, tilted with
    /// the banking), and up out of its surface.
    pub forward: Vec3,
    pub right: Vec3,
    pub up: Vec3,
    /// How fast the road turns (radians per metre), positive to the right.
    pub curvature: f32,
    pub section: Section,
    /// The cross-section's widths here, blended across changes of section.
    pub paved: f32,
    pub wall: f32,
    /// How far into a stretch with a median barrier (0 outside, 1 in it):
    /// the barrier's ends are where this crosses one half.
    pub median: f32,
    pub pavement: f32,
}

/// A control point of the layout.
#[derive(Clone, Copy, Debug)]
pub struct ControlPoint {
    pub x: f32,
    pub z: f32,
    /// The height the road should pass at (m above the sea).
    pub y: f32,
    /// The kind of road from here to the next point.
    pub section: Section,
}

/// Where a point is relative to the road.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Location {
    /// The sample at or before the point.
    pub index: usize,
    /// Distance along the road (0..length).
    pub s: f32,
    /// Distance across it (positive to the right).
    pub lateral: f32,
    /// The height of the road's surface there (banked), at that lateral
    /// offset; a pavement's kerb is above it.
    pub height: f32,
}

/// The road.
pub struct Road {
    samples: Vec<Sample>,
    spacing: f32,
    length: f32,
    /// Sample indices by grid cell, for finding the road from anywhere.
    grid: Grid,
}

/// Size of the grid's cells (m).
const CELL: f32 = 48.0;

struct Grid {
    x0: f32,
    z0: f32,
    w: usize,
    h: usize,
    cells: Vec<Vec<u32>>,
}

impl Grid {
    fn cell(&self, x: f32, z: f32) -> Option<usize> {
        let cx = ((x - self.x0) / CELL).floor();
        let cz = ((z - self.z0) / CELL).floor();
        if cx < 0.0 || cz < 0.0 || cx >= self.w as f32 || cz >= self.h as f32 {
            return None;
        }
        Some(cz as usize * self.w + cx as usize)
    }
}

/// A point on a closed centripetal Catmull-Rom spline through `p` (with
/// the parameter of each point's segment from 0 to 1).
fn catmull_rom(p0: Vec3, p1: Vec3, p2: Vec3, p3: Vec3, t: f32) -> Vec3 {
    // Knot spacing by the square root of the distance (centripetal): no
    // cusps or loops however unevenly the points are spaced.
    let knot = |a: Vec3, b: Vec3| a.xz().distance(b.xz()).sqrt().max(1e-3);
    let t0 = 0.0;
    let t1 = t0 + knot(p0, p1);
    let t2 = t1 + knot(p1, p2);
    let t3 = t2 + knot(p2, p3);
    let u = t1 + (t2 - t1) * t;
    let a1 = p0 * ((t1 - u) / (t1 - t0)) + p1 * ((u - t0) / (t1 - t0));
    let a2 = p1 * ((t2 - u) / (t2 - t1)) + p2 * ((u - t1) / (t2 - t1));
    let a3 = p2 * ((t3 - u) / (t3 - t2)) + p3 * ((u - t2) / (t3 - t2));
    let b1 = a1 * ((t2 - u) / (t2 - t0)) + a2 * ((u - t0) / (t2 - t0));
    let b2 = a2 * ((t3 - u) / (t3 - t1)) + a3 * ((u - t1) / (t3 - t1));
    b1 * ((t2 - u) / (t2 - t1)) + b2 * ((u - t1) / (t2 - t1))
}

/// Points every `step` along the closed polyline `pts` (with their
/// sections): returns them and the step that divides the loop evenly.
fn resample(pts: &[(Vec3, Section)], step: f32) -> (Vec<(Vec3, Section)>, f32) {
    let n = pts.len();
    let seg = |i: usize| pts[i].0.xz().distance(pts[(i + 1) % n].0.xz());
    let total: f32 = (0..n).map(seg).sum();
    let count = (total / step).round().max(8.0) as usize;
    let step = total / count as f32;
    let mut out = Vec::with_capacity(count);
    let (mut i, mut along) = (0usize, 0.0f32);
    for k in 0..count {
        let want = k as f32 * step;
        while i < n - 1 && along + seg(i) < want {
            along += seg(i);
            i += 1;
        }
        let t = ((want - along) / seg(i).max(1e-6)).clamp(0.0, 1.0);
        let (a, b) = (pts[i].0, pts[(i + 1) % n].0);
        out.push((a + (b - a) * t, pts[i].1));
    }
    (out, step)
}

/// Averages each value with its neighbours `radius` samples either side
/// (the loop wraps), `passes` times.
fn smooth_loop(v: &mut [f32], radius: usize, passes: usize) {
    let n = v.len();
    if n == 0 || radius == 0 {
        return;
    }
    let mut tmp = vec![0.0; n];
    for _ in 0..passes {
        // A running sum over the window.
        let mut sum: f32 = (0..=2 * radius).map(|k| v[(k + n * 4 - radius) % n]).sum();
        let w = (2 * radius + 1) as f32;
        for i in 0..n {
            tmp[i] = sum / w;
            sum += v[(i + radius + 1) % n] - v[(i + n * 4 - radius) % n];
        }
        v.copy_from_slice(&tmp);
    }
}

impl Road {
    /// The road through `points` (a closed loop, at least four points).
    pub fn new(points: &[ControlPoint]) -> Road {
        let n = points.len();
        assert!(n >= 4, "a road needs four control points");
        let p = |i: usize| {
            let c = points[i % n];
            Vec3::new(c.x, c.y, c.z)
        };
        // The spline, densely.
        let mut dense = Vec::new();
        for (i, point) in points.iter().enumerate() {
            let (p0, p1, p2, p3) = (p(i + n - 1), p(i), p(i + 1), p(i + 2));
            let steps = (p1.xz().distance(p2.xz()) / 2.0).ceil().max(4.0) as usize;
            for k in 0..steps {
                dense.push((catmull_rom(p0, p1, p2, p3, k as f32 / steps as f32), point.section));
            }
        }
        // Evenly spaced, then smoothed across: the curvature changes
        // gradually, as on a road built with transition curves.
        let (mut pts, _) = resample(&dense, SPACING);
        let (mut xs, mut zs): (Vec<f32>, Vec<f32>) = pts.iter().map(|(v, _)| (v.x, v.z)).unzip();
        smooth_loop(&mut xs, 6, 3);
        smooth_loop(&mut zs, 6, 3);
        for (k, (v, _)) in pts.iter_mut().enumerate() {
            v.x = xs[k];
            v.z = zs[k];
        }
        let (pts, spacing) = resample(&pts, SPACING);
        let count = pts.len();
        // Heights: eased into long, gentle grades.
        let mut ys: Vec<f32> = pts.iter().map(|(v, _)| v.y).collect();
        smooth_loop(&mut ys, (40.0 / spacing) as usize, 4);
        let pos: Vec<Vec3> = pts.iter().zip(&ys).map(|((v, _), &y)| Vec3::new(v.x, y, v.z)).collect();
        let sections: Vec<Section> = pts.iter().map(|(_, s)| *s).collect();

        // Frames and curvature.
        let at = |i: isize| pos[i.rem_euclid(count as isize) as usize];
        let mut forward = Vec::with_capacity(count);
        let mut curvature = Vec::with_capacity(count);
        for i in 0..count as isize {
            let f = (at(i + 1) - at(i - 1)).normalize_or(Vec3::Z);
            forward.push(f);
            // The change of the horizontal heading over two steps.
            let a = (at(i) - at(i - 1)).xz().normalize_or(Vec2::Y);
            let b = (at(i + 1) - at(i)).xz().normalize_or(Vec2::Y);
            // Positive to the right: the right of a heading (x, z) on the
            // ground is (-z, x).
            let turn = a.perp_dot(b).atan2(a.dot(b));
            curvature.push(turn / spacing);
        }
        smooth_loop(&mut curvature, 3, 2);
        // Banking into the corners (at most 5 degrees), eased.
        let mut bank: Vec<f32> = curvature.iter().map(|k| (k * 9.0).clamp(-0.09, 0.09)).collect();
        smooth_loop(&mut bank, (25.0 / spacing) as usize, 3);

        // Cross-sections, blended across changes of section.
        let radius = (BLEND / spacing) as usize;
        let mut paved: Vec<f32> = sections.iter().map(|s| s.profile().paved).collect();
        let mut wall: Vec<f32> = sections.iter().map(|s| s.profile().wall).collect();
        let mut median: Vec<f32> = sections.iter().map(|s| f32::from(u8::from(s.profile().median))).collect();
        let mut pavement: Vec<f32> = sections.iter().map(|s| f32::from(u8::from(s.profile().pavement))).collect();
        for v in [&mut paved, &mut wall, &mut median, &mut pavement] {
            smooth_loop(v, radius, 1);
        }

        let samples: Vec<Sample> = (0..count)
            .map(|i| {
                let f = forward[i];
                let flat_right = Vec3::new(-f.z, 0.0, f.x).normalize_or(Vec3::X);
                // Tilt the right vector about the forward one: down on the
                // inside of the corner.
                let (sb, cb) = bank[i].sin_cos();
                let right = (flat_right * cb + Vec3::Y * -sb).normalize_or(flat_right);
                let up = right.cross(f).normalize_or(Vec3::Y);
                Sample {
                    pos: pos[i],
                    forward: f,
                    right,
                    up,
                    curvature: curvature[i],
                    section: sections[i],
                    paved: paved[i],
                    wall: wall[i],
                    median: median[i],
                    pavement: pavement[i],
                }
            })
            .collect();
        let grid = Self::build_grid(&samples);
        Road { samples, spacing, length: spacing * count as f32, grid }
    }

    fn build_grid(samples: &[Sample]) -> Grid {
        let (mut lo, mut hi) = (Vec2::splat(f32::MAX), Vec2::splat(f32::MIN));
        for s in samples {
            lo = lo.min(s.pos.xz());
            hi = hi.max(s.pos.xz());
        }
        // Room for points well off the road.
        let margin = 400.0;
        let (x0, z0) = (lo.x - margin, lo.y - margin);
        let w = ((hi.x - lo.x + 2.0 * margin) / CELL).ceil() as usize + 1;
        let h = ((hi.y - lo.y + 2.0 * margin) / CELL).ceil() as usize + 1;
        let mut grid = Grid { x0, z0, w, h, cells: vec![Vec::new(); w * h] };
        // Each sample in its cell and the ring around it, so that a query
        // finds every sample within a cell's width.
        for (i, s) in samples.iter().enumerate() {
            for dz in -1..=1 {
                for dx in -1..=1 {
                    let (x, z) = (s.pos.x + dx as f32 * CELL, s.pos.z + dz as f32 * CELL);
                    if let Some(c) = grid.cell(x, z)
                        && grid.cells[c].last() != Some(&(i as u32))
                    {
                        grid.cells[c].push(i as u32);
                    }
                }
            }
        }
        for c in &mut grid.cells {
            c.sort_unstable();
            c.dedup();
        }
        grid
    }

    /// The loop's length (m).
    pub fn length(&self) -> f32 {
        self.length
    }

    /// The distance between samples (m).
    pub fn spacing(&self) -> f32 {
        self.spacing
    }

    /// The number of samples.
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// Sample `i`, wrapping around the loop.
    pub fn sample(&self, i: isize) -> &Sample {
        &self.samples[i.rem_euclid(self.samples.len() as isize) as usize]
    }

    /// Every sample, in order.
    pub fn samples(&self) -> &[Sample] {
        &self.samples
    }

    /// `s` wrapped into `0..length`.
    pub fn wrap(&self, s: f32) -> f32 {
        let s = s % self.length;
        if s < 0.0 { s + self.length } else { s }
    }

    /// The signed distance along the road from `from` to `to`, the short
    /// way round (in `-length / 2 .. length / 2`).
    pub fn delta(&self, from: f32, to: f32) -> f32 {
        let d = self.wrap(to - from);
        if d > self.length * 0.5 { d - self.length } else { d }
    }

    /// The sample index at or before `s`, and how far towards the next.
    fn index_of(&self, s: f32) -> (usize, f32) {
        let u = self.wrap(s) / self.spacing;
        let i = u.floor();
        ((i as usize).min(self.samples.len() - 1), u - i)
    }

    /// The frame at `s`, interpolated: the centre line's point, forward,
    /// right and up.
    pub fn frame(&self, s: f32) -> (Vec3, Vec3, Vec3, Vec3) {
        let (i, t) = self.index_of(s);
        let (a, b) = (self.sample(i as isize), self.sample(i as isize + 1));
        let f = a.forward.lerp(b.forward, t).normalize_or(a.forward);
        let r = a.right.lerp(b.right, t).normalize_or(a.right);
        let u = a.up.lerp(b.up, t).normalize_or(a.up);
        (a.pos.lerp(b.pos, t), f, r, u)
    }

    /// The point on the road's surface at `(s, lateral)`.
    pub fn point(&self, s: f32, lateral: f32) -> Vec3 {
        let (p, _, r, _) = self.frame(s);
        p + r * lateral
    }

    /// The sample nearest `s`.
    pub fn at(&self, s: f32) -> &Sample {
        let (i, t) = self.index_of(s);
        self.sample(i as isize + isize::from(t >= 0.5))
    }

    /// The curvature at `s` (radians per metre, positive to the right).
    pub fn curvature(&self, s: f32) -> f32 {
        let (i, t) = self.index_of(s);
        let (a, b) = (self.sample(i as isize), self.sample(i as isize + 1));
        a.curvature + (b.curvature - a.curvature) * t
    }

    /// Where `p` is relative to the road. `hint`, the sample a moving
    /// object was near the last time, makes this a local search.
    pub fn locate(&self, p: Vec3, hint: Option<usize>) -> Location {
        let n = self.samples.len();
        let near = |i: usize| self.samples[i].pos.xz().distance_squared(p.xz());
        let mut best: Option<(usize, f32)> = None;
        if let Some(h) = hint.filter(|&h| h < n) {
            // Walk downhill from the hint.
            let (mut i, mut d) = (h, near(h));
            loop {
                let (prev, next) = ((i + n - 1) % n, (i + 1) % n);
                let (dp, dn) = (near(prev), near(next));
                if dn < d {
                    (i, d) = (next, dn);
                } else if dp < d {
                    (i, d) = (prev, dp);
                } else {
                    break;
                }
            }
            // Trusted when it ends near the road (far off it, another
            // stretch of the loop may be nearer).
            let wall = self.samples[i].wall + 20.0;
            if d < wall * wall {
                best = Some((i, d));
            }
        }
        if best.is_none() {
            best = match self.grid.cell(p.x, p.z) {
                Some(c) => self.grid.cells[c]
                    .iter()
                    .map(|&i| (i as usize, near(i as usize)))
                    .min_by(|a, b| a.1.total_cmp(&b.1)),
                None => None,
            };
        }
        let i = match best {
            Some((i, _)) => i,
            // Far outside the world: every sample.
            None => (0..n).min_by(|&a, &b| near(a).total_cmp(&near(b))).unwrap_or(0),
        };
        self.project(i, p)
    }

    /// `p` projected onto the centre line near sample `i`.
    fn project(&self, i: usize, p: Vec3) -> Location {
        let n = self.samples.len() as isize;
        let mut best = (i, 0.0f32, f32::MAX);
        for a in [i as isize - 1, i as isize] {
            let (sa, sb) = (self.sample(a).pos.xz(), self.sample(a + 1).pos.xz());
            let ab = sb - sa;
            let t = ((p.xz() - sa).dot(ab) / ab.length_squared().max(1e-6)).clamp(0.0, 1.0);
            let d = (sa + ab * t).distance_squared(p.xz());
            if d < best.2 {
                best = (a.rem_euclid(n) as usize, t, d);
            }
        }
        let (index, t, _) = best;
        let s = (index as f32 + t) * self.spacing;
        let (c, _, r, _) = self.frame(s);
        let flat = Vec2::new(r.x, r.z);
        let flat_len = flat.length().max(1e-4);
        let lateral = (p.xz() - c.xz()).dot(flat) / flat_len;
        // The banked surface rises or falls along the tilted right vector.
        let height = c.y + lateral * r.y / flat_len;
        Location { index, s: self.wrap(s), lateral, height }
    }

    /// The cross-section's widths at `s`: `(paved, wall)`.
    pub fn widths(&self, s: f32) -> (f32, f32) {
        let (i, t) = self.index_of(s);
        let (a, b) = (self.sample(i as isize), self.sample(i as isize + 1));
        (a.paved + (b.paved - a.paved) * t, a.wall + (b.wall - a.wall) * t)
    }

    /// How much a roof covers the road at `s`: 1 in a tunnel, 0 in the
    /// open, between at its mouths.
    pub fn covered(&self, s: f32) -> f32 {
        [-6.0f32, 0.0, 6.0].iter().filter(|&&d| self.at(s + d).section == Section::Tunnel).count() as f32 / 3.0
    }

    /// Whether a median barrier divides the road at `s`.
    pub fn divided(&self, s: f32) -> bool {
        self.at(s).median >= 0.5
    }

    /// The lateral offset of the middle of lane `lane` (0 the inner, 1 the
    /// outer) on the side `side` (1 forward, -1 the other way).
    pub fn lane_center(lane: usize, side: f32) -> f32 {
        side * (MEDIAN + LANE_WIDTH * (lane as f32 + 0.5))
    }

    /// For each sample, how far it is along the road from the nearest
    /// sample of another section: how deep into its stretch it lies.
    pub fn section_depths(&self) -> Vec<f32> {
        let n = self.samples.len();
        let mut depth = vec![f32::MAX; n];
        let Some(change) = (0..n).find(|&i| self.samples[i].section != self.samples[(i + n - 1) % n].section) else {
            // One section all round.
            return depth;
        };
        // Forward from a change, then backward from one.
        let mut run = 0.0;
        for k in 0..n {
            let i = (change + k) % n;
            if self.samples[i].section != self.samples[(i + n - 1) % n].section {
                run = 0.0;
            }
            run += self.spacing;
            depth[i] = run;
        }
        let end = (change + n - 1) % n;
        run = 0.0;
        for k in 0..n {
            let i = (end + n - k) % n;
            if self.samples[i].section != self.samples[(i + 1) % n].section {
                run = 0.0;
            }
            run += self.spacing;
            depth[i] = depth[i].min(run);
        }
        depth
    }

    /// The largest curvature (absolute) between `s` and `s + ahead` (or
    /// behind, for a negative distance).
    pub fn max_curvature(&self, s: f32, ahead: f32) -> f32 {
        let steps = (ahead.abs() / self.spacing).ceil() as usize;
        let dir = ahead.signum();
        (0..=steps).map(|k| self.curvature(s + dir * k as f32 * self.spacing).abs()).fold(0.0, f32::max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A rounded square, 400 m a side.
    fn square() -> Road {
        let c = |x: f32, z: f32, section| ControlPoint { x, z, y: 10.0, section };
        Road::new(&[
            c(0.0, 0.0, Section::City),
            c(200.0, -20.0, Section::City),
            c(400.0, 0.0, Section::Hills),
            c(420.0, 200.0, Section::Hills),
            c(400.0, 400.0, Section::Bridge),
            c(200.0, 420.0, Section::Bridge),
            c(0.0, 400.0, Section::Port),
            c(-20.0, 200.0, Section::Port),
        ])
    }

    #[test]
    fn samples_are_evenly_spaced_round_the_loop() {
        let road = square();
        assert!(road.length() > 1300.0 && road.length() < 1700.0, "{}", road.length());
        for i in 0..road.len() as isize {
            let d = road.sample(i).pos.xz().distance(road.sample(i + 1).pos.xz());
            assert!((d - road.spacing()).abs() < 0.15, "step {i}: {d}");
        }
        assert!((road.spacing() - SPACING).abs() < 0.05);
    }

    #[test]
    fn frames_are_orthonormal_and_right_is_to_the_right() {
        let road = square();
        for s in road.samples() {
            assert!((s.forward.length() - 1.0).abs() < 1e-3);
            assert!(s.forward.dot(s.right).abs() < 1e-3);
            assert!(s.up.y > 0.95);
            // Looking along +x, the right is +z (y up, right-handed).
            let flat = Vec3::new(s.forward.x, 0.0, s.forward.z).normalize();
            let r = Vec3::new(s.right.x, 0.0, s.right.z).normalize();
            assert!((flat.cross(Vec3::Y) - r).length() < 0.05);
        }
        // The square goes round clockwise seen from above (x right, z
        // down), so it turns right, mostly.
        let total: f32 = road.samples().iter().map(|s| s.curvature * road.spacing()).sum();
        assert!((total - core::f32::consts::TAU).abs() < 0.1, "total turning {total}");
    }

    #[test]
    fn locate_finds_the_point_it_was_given() {
        let road = square();
        for k in 0..40 {
            let s = k as f32 * road.length() / 40.0 + 0.7;
            for lateral in [-12.0, -3.0, 0.0, 5.5, 9.0] {
                let p = road.point(s, lateral);
                for hint in [None, Some(road.locate(p, None).index)] {
                    let at = road.locate(p, hint);
                    assert!(road.delta(s, at.s).abs() < 0.3, "s {s} found {}", at.s);
                    assert!((at.lateral - lateral).abs() < 0.3, "lateral {lateral} found {}", at.lateral);
                    assert!((at.height - p.y).abs() < 0.05);
                }
            }
        }
        // A stale hint far away still finds it.
        let p = road.point(500.0, 2.0);
        let at = road.locate(p, Some(0));
        assert!(road.delta(500.0, at.s).abs() < 0.3);
    }

    #[test]
    fn cross_sections_blend_between_sections() {
        let road = square();
        let walls: Vec<f32> = road.samples().iter().map(|s| s.wall).collect();
        for w in walls.windows(2) {
            assert!((w[0] - w[1]).abs() < 0.5, "a step in the walls: {w:?}");
        }
        // The bridge is divided, the city is not.
        assert!(road.samples().iter().any(|s| s.section == Section::Bridge && s.median > 0.99));
        assert!(road.samples().iter().any(|s| s.section == Section::City && s.median < 0.01));
    }

    #[test]
    fn delta_and_wrap() {
        let road = square();
        let l = road.length();
        assert!((road.wrap(-1.0) - (l - 1.0)).abs() < 1e-3);
        assert!((road.delta(l - 5.0, 5.0) - 10.0).abs() < 1e-3);
        assert!((road.delta(5.0, l - 5.0) + 10.0).abs() < 1e-3);
        assert_eq!(Road::lane_center(0, 1.0), MEDIAN + LANE_WIDTH * 0.5);
        assert!(Road::lane_center(1, -1.0) < -5.0);
    }
}
