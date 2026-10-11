//! The cameras: those that follow a car (from behind, near or far, from
//! the bonnet and from the bumper), and the menus' [`Cinematic`] camera,
//! filming a chase as television would.
//!
//! The chase cameras trail the car on springs: further back and wider as
//! the speed rises, looking where the car is going while it slides,
//! shaken by crashes, and kept inside the road's walls and under a
//! tunnel's roof. The springs are solved, not stepped: a camera is where
//! it would be however long the frames are, so frames of uneven length
//! (a slow one now and then) never move it about behind the car.

use core::ops::{Add, Div, Mul, Sub};

use crate::prelude::*;

use crate::render::Camera;
use crate::vehicle::Vehicle;
use crate::world::World;

/// Which camera.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Chase,
    Near,
    Bonnet,
    Bumper,
}

impl View {
    /// The next view (the camera key cycles through them).
    pub fn next(self) -> View {
        match self {
            View::Chase => View::Near,
            View::Near => View::Bonnet,
            View::Bonnet => View::Bumper,
            View::Bumper => View::Chase,
        }
    }
}

/// A camera following a car.
#[derive(Clone, Debug)]
pub struct Follow {
    pub view: View,
    pos: Vec3,
    vel: Vec3,
    /// The direction it looks along, smoothed (on the ground).
    look: Vec2,
    fov: f32,
    shake: f32,
    shake_time: f32,
    /// Looking back over the shoulder.
    pub behind: bool,
    /// Where the chase camera wanted to be at the last frame, and for which
    /// view (looking back or not): the spring follows that place as it
    /// moved since.
    last: Option<(Vec3, View, bool)>,
    /// The direction it turned to look along and the field of view it
    /// widened to at the last frame, followed likewise.
    wanted: Option<(Vec2, f32)>,
}

impl Follow {
    /// A camera already in place behind `car`.
    pub fn new(car: &Vehicle) -> Follow {
        let f = car.forward();
        let mut c = Follow {
            view: View::Chase,
            pos: Vec3::ZERO,
            vel: Vec3::ZERO,
            look: f,
            fov: 1.05,
            shake: 0.0,
            shake_time: 0.0,
            behind: false,
            last: None,
            wanted: None,
        };
        c.pos = c.desired(car).0;
        c
    }

    /// Shakes it (a crash of `strength`, about 0..1).
    pub fn jolt(&mut self, strength: f32) {
        self.shake = (self.shake + strength).min(1.2);
    }

    /// Where the chase camera wants to be, and the point it looks at.
    fn desired(&self, car: &Vehicle) -> (Vec3, Vec3) {
        let speed = car.vel.length();
        let (back, up, ahead) = match self.view {
            View::Chase => (7.0 + speed * 0.02, 2.2 + speed * 0.004, 10.0),
            _ => (4.4 + speed * 0.015, 1.6 + speed * 0.003, 8.0),
        };
        let dir = if self.behind { -self.look } else { self.look };
        let base = car.pos + Vec3::Y * 0.6;
        let eye = base - Vec3::new(dir.x, 0.0, dir.y) * back + Vec3::Y * up;
        let target = base + Vec3::new(dir.x, 0.0, dir.y) * ahead + Vec3::Y * 0.5;
        (eye, target)
    }

    /// Follows `car` for `dt` seconds; `hint` is the road sample it is
    /// near. Returns the camera to draw with.
    pub fn update(&mut self, car: &Vehicle, world: &World, hint: usize, dt: f32) -> Camera {
        let speed = car.vel.length();
        // Look along a blend of the heading and the motion: through a
        // slide, more along the motion.
        let heading = car.forward();
        let motion = if speed > 4.0 { car.vel / speed } else { heading };
        let want = (heading * 0.55 + motion * 0.45).normalize_or(heading);
        // Wider with speed.
        let fov = 1.0 + (speed / 70.0).min(1.0) * 0.28;
        let (want_before, fov_before) = self.wanted.unwrap_or((want, fov));
        self.look = ease(self.look, want_before, want, 5.0, dt).normalize_or(want);
        self.fov = ease(self.fov, fov_before, fov, 2.0, dt);
        self.wanted = Some((want, fov));
        self.shake_time += dt;
        self.shake *= (-dt * 3.0).exp();

        let mut cam = match self.view {
            View::Chase | View::Near => {
                let (eye, target) = self.desired(car);
                // A critically damped spring after it, the place it wants
                // having moved steadily since the last frame (but for a
                // change of view, which it eases to from where it is).
                let w = if self.view == View::Chase { 9.0 } else { 11.0 };
                let from = match self.last {
                    Some((at, view, behind)) if view == self.view && behind == self.behind => at,
                    _ => eye,
                };
                (self.pos, self.vel) = spring(self.pos, self.vel, from, eye, w, dt);
                self.last = Some((eye, self.view, self.behind));
                // Never far behind (after a teleport): further than the
                // spring's own lag at this speed.
                if self.pos.distance(eye) > 25.0 + 2.0 * speed / w {
                    self.pos = eye;
                    self.vel = Vec3::ZERO;
                }
                Camera { eye: self.pos, target, up: Vec3::Y, fov: self.fov }
            }
            View::Bonnet | View::Bumper => {
                let m = car.transform();
                let (eye, ahead) = if self.view == View::Bonnet {
                    (Vec3::new(0.0, 1.35, 0.6), Vec3::new(0.0, 1.2, 20.0))
                } else {
                    (Vec3::new(0.0, 0.62, car.spec.half_length + 0.1), Vec3::new(0.0, 0.6, 30.0))
                };
                let (eye, ahead) = if self.behind {
                    (Vec3::new(eye.x, eye.y, -eye.z.abs() + 0.2), Vec3::new(0.0, ahead.y, -ahead.z))
                } else {
                    (eye, ahead)
                };
                self.pos = m.transform_point3(eye);
                self.vel = Vec3::ZERO;
                self.last = None;
                Camera { eye: self.pos, target: m.transform_point3(ahead), up: Vec3::Y, fov: self.fov * 1.05 }
            }
        };
        cam.eye = keep_in(world, cam.eye, hint);
        // Shake: a few incommensurate waves.
        if self.shake > 0.01 {
            let t = self.shake_time;
            let s = self.shake * 0.25;
            cam.eye += Vec3::new((t * 37.0).sin() * s, (t * 43.0).sin() * s * 0.7, (t * 29.0).cos() * s);
        }
        cam
    }
}

/// A critically damped spring of rate `w` (1/s), at `pos` going at `vel`,
/// after a place moving steadily from `from` to `to` over `dt` seconds:
/// where it is and how fast it goes at the end. Solved exactly, so it is
/// the same over one long frame as over many short ones (stepped, it
/// would come out ahead by more the longer the frame, and overshoot).
fn spring(pos: Vec3, vel: Vec3, from: Vec3, to: Vec3, w: f32, dt: f32) -> (Vec3, Vec3) {
    if dt <= 0.0 {
        return (pos, vel);
    }
    // A step of under a millisecond is too short to tell the place's speed
    // by: it counts as having moved at once.
    let from = if dt < 1e-3 { to } else { from };
    // Relative to the place, which goes at `v`, the spring settles `lag`
    // behind it; what is left of where it was dies away as (c1 + c2 t)
    // e^(-w t).
    let v = (to - from) / dt;
    let lag = v * (2.0 / w);
    let c1 = pos - from + lag;
    let c2 = vel - v + c1 * w;
    let decay = (-w * dt).exp();
    let left = (c1 + c2 * dt) * decay;
    let rate = (c2 - (c1 + c2 * dt) * w) * decay;
    (to - lag + left, v + rate)
}

/// Eases `x` at `rate` (1/s) after a value moving steadily from `from` to
/// `to` over `dt` seconds, solved exactly as [`spring`] is.
fn ease<T>(x: T, from: T, to: T, rate: f32, dt: f32) -> T
where
    T: Copy + Add<Output = T> + Sub<Output = T> + Mul<f32, Output = T> + Div<f32, Output = T>,
{
    if dt <= 0.0 {
        return x;
    }
    let from = if dt < 1e-3 { to } else { from };
    // It settles `lag` behind a value going steadily; the rest dies away.
    let lag = (to - from) / (rate * dt);
    to - lag + (x - from + lag) * (-rate * dt).exp()
}

/// Keeps `eye` inside the road's walls, above the ground and under a
/// tunnel's roof (the road sample near it is `hint`).
fn keep_in(world: &World, mut eye: Vec3, hint: usize) -> Vec3 {
    let at = world.road.locate(eye, Some(hint));
    let (_, wall) = world.road.widths(at.s);
    if at.lateral.abs() > wall - 0.4 {
        let (_, _, r, _) = world.road.frame(at.s);
        let over = at.lateral.abs() - (wall - 0.4);
        eye -= Vec3::new(r.x, 0.0, r.z).normalize_or(Vec3::X) * (over * at.lateral.signum());
    }
    eye.y = eye.y.max(at.height + 0.4);
    if world.road.sample(at.index as isize).section == crate::road::Section::Tunnel {
        eye.y = eye.y.min(at.height + 5.8);
    }
    eye
}

/// A shot of the cinematic camera.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shot {
    /// From a helicopter, high and behind, circling slowly.
    Helicopter,
    /// From the roadside ahead, the cars rushing past.
    Roadside,
    /// Low behind the hero, wide.
    Low,
    /// Alongside the car chased.
    Alongside,
    /// Ahead of the car chased, looking back at both.
    Ahead,
}

/// The menus' camera: a chase filmed as television would, a shot every
/// few seconds, cut to the next.
#[derive(Clone, Debug)]
pub struct Cinematic {
    shot: Shot,
    /// Seconds into the shot, and how long it lasts.
    t: f32,
    length: f32,
    /// Shots so far (which one comes next).
    count: u32,
    /// A roadside camera's place, and the road sample near it.
    spot: Vec3,
    spot_hint: usize,
}

impl Default for Cinematic {
    fn default() -> Cinematic {
        Cinematic::new()
    }
}

impl Cinematic {
    pub fn new() -> Cinematic {
        Cinematic { shot: Shot::Helicopter, t: 0.0, length: 7.0, count: 0, spot: Vec3::ZERO, spot_hint: 0 }
    }

    /// Films `hero` (`hint`: the road sample it is near) chasing `chased`,
    /// if anything, `dt` seconds on.
    pub fn update(&mut self, world: &World, hero: &Vehicle, hint: usize, chased: Option<&Vehicle>, dt: f32) -> Camera {
        self.t += dt;
        let road = &world.road;
        let at = road.locate(hero.pos, Some(hint));
        let tunnel = road.at(at.s).section == crate::road::Section::Tunnel;
        // A helicopter cannot follow into a tunnel; a roadside camera's
        // shot ends once the cars have gone by.
        let passed = self.shot == Shot::Roadside && {
            let f = Vec3::new(hero.forward().x, 0.0, hero.forward().y);
            (hero.pos - self.spot).dot(f) > 30.0
        };
        // Slow cars (a bust, a crash) make dull close-ups: then from above.
        let slow = hero.vel.length() < 8.0;
        let dull = slow && self.shot != Shot::Helicopter && !tunnel;
        if self.t > self.length || passed || dull || (tunnel && self.shot == Shot::Helicopter) {
            self.next(world, hero, hint, chased.is_some(), tunnel);
        }
        let f2 = hero.forward();
        let fwd = Vec3::new(f2.x, 0.0, f2.y);
        let side = Vec3::new(f2.y, 0.0, -f2.x);
        let lift = Vec3::Y;
        let other = chased.unwrap_or(hero);
        // Where the car chased is going (its heading may be anywhere in a
        // spin), on the ground.
        let going = {
            let v = Vec3::new(other.vel.x, 0.0, other.vel.y);
            let f = other.forward();
            if v.length() > 3.0 { v.normalize() } else { Vec3::new(f.x, 0.0, f.y) }
        };
        let (eye, target, fov) = match self.shot {
            Shot::Helicopter => {
                // Over the street (not the blocks beside it), swinging
                // slowly from behind to one side.
                let a = self.t * 0.06 + self.count as f32;
                let s = at.s - 28.0 * a.cos();
                let (_, wall) = road.widths(s);
                let lateral = (at.lateral + 12.0 * a.sin()).clamp(-wall, wall);
                let mid = (hero.pos + other.pos) * 0.5;
                (road.point(s, lateral) + lift * 32.0, mid + lift, 0.75)
            }
            Shot::Roadside => {
                // Zoomed to keep the cars about the same size as they come.
                let target = hero.pos.lerp(other.pos, 0.3) + lift * 0.8;
                let fov = 2.0 * (7.0 / self.spot.distance(target).max(1.0)).atan();
                (self.spot, target, fov.clamp(0.22, 1.0))
            }
            Shot::Low => (hero.pos - fwd * 6.5 + side * 1.3 + lift * 0.9, hero.pos + fwd * 25.0 + lift * 1.2, 1.1),
            Shot::Alongside => {
                // From the side with the road's width, not the wall's.
                let slide = (self.t / self.length - 0.5) * 6.0;
                let o = going;
                let to_right = Vec3::new(o.z, 0.0, -o.x);
                let (_, _, r, _) = road.frame(at.s);
                let them = road.locate(other.pos, Some(at.index));
                let inward = if them.lateral > 0.0 { -r } else { r };
                let side = if to_right.dot(inward) > 0.0 { to_right } else { -to_right };
                (other.pos + side * 7.5 + o * slide + lift * 1.4, other.pos + lift * 0.7, 0.9)
            }
            Shot::Ahead => (other.pos + going * 17.0 + lift * 2.4, (hero.pos + other.pos) * 0.5 + lift * 0.8, 0.8),
        };
        let eye = if self.shot == Shot::Helicopter { eye } else { keep_in(world, eye, at.index) };
        Camera { eye, target, up: Vec3::Y, fov }
    }

    /// Cuts to the next shot.
    fn next(&mut self, world: &World, hero: &Vehicle, hint: usize, chasing: bool, tunnel: bool) {
        const ORDER: [Shot; 6] = [Shot::Helicopter, Shot::Low, Shot::Roadside, Shot::Ahead, Shot::Alongside, Shot::Low];
        self.count += 1;
        let mut shot = ORDER[self.count as usize % ORDER.len()];
        let speed = hero.vel.length();
        if !chasing && matches!(shot, Shot::Ahead | Shot::Alongside) {
            shot = Shot::Low;
        }
        // The cars must come by soon for a roadside camera; slow ones are
        // best seen from above.
        if shot == Shot::Roadside && speed < 15.0 {
            shot = Shot::Low;
        }
        if speed < 8.0 {
            shot = Shot::Helicopter;
        }
        if tunnel && shot == Shot::Helicopter {
            shot = Shot::Low;
        }
        self.shot = shot;
        self.t = 0.0;
        self.length = match shot {
            Shot::Helicopter => 8.0,
            Shot::Roadside => 7.0,
            _ => 6.0,
        };
        if shot == Shot::Roadside {
            // Ahead, just off the road on alternate sides, and between the
            // street lamps (whose glare would fill the picture).
            let at = world.road.locate(hero.pos, Some(hint));
            let ahead = 25.0 + speed * 2.2;
            let side = if self.count.is_multiple_of(2) { 1.0 } else { -1.0 };
            let spot = |d: f32| {
                let s = world.road.wrap(at.s + ahead + d);
                let (paved, wall) = world.road.widths(s);
                world.road.point(s, side * (paved + 1.0).min(wall - 0.6)) + Vec3::Y * 1.3
            };
            let clear = |p: Vec3| world.lamps.iter().all(|l| (l.base - p).xz().length() > 6.0);
            let best = [0.0, 12.0, -12.0, 17.0, -17.0].into_iter().map(spot).find(|&p| clear(p));
            self.spot = best.unwrap_or_else(|| spot(0.0));
            self.spot_hint = world.road.locate(self.spot, None).index;
        }
    }
}
