//! The cameras: those that follow a car (from behind, near or far, from
//! the bonnet and from the bumper), and the menus' [`Cinematic`] camera,
//! filming a chase as television would.
//!
//! The chase cameras trail the car on springs: further back and wider as
//! the speed rises, looking where the car is going while it slides,
//! shaken by crashes, and kept inside the road's walls and under a
//! tunnel's roof.

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
        let k = 1.0 - (-dt * 5.0).exp();
        self.look = (self.look + (want - self.look) * k).normalize_or(want);
        // Wider with speed.
        let fov = 1.0 + (speed / 70.0).min(1.0) * 0.28;
        self.fov += (fov - self.fov) * (1.0 - (-dt * 2.0).exp());
        self.shake_time += dt;
        self.shake *= (-dt * 3.0).exp();

        let mut cam = match self.view {
            View::Chase | View::Near => {
                let (eye, target) = self.desired(car);
                // A critically damped spring towards it.
                let w = if self.view == View::Chase { 9.0 } else { 11.0 };
                let a = (eye - self.pos) * (w * w) - self.vel * (2.0 * w);
                self.vel += a * dt;
                self.pos += self.vel * dt;
                // Never far behind (after a teleport or a long frame).
                if self.pos.distance(eye) > 25.0 {
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
        if self.t > self.length || passed || (tunnel && self.shot == Shot::Helicopter) {
            self.next(world, hero, hint, chased.is_some(), tunnel);
        }
        let f2 = hero.forward();
        let fwd = Vec3::new(f2.x, 0.0, f2.y);
        let side = Vec3::new(f2.y, 0.0, -f2.x);
        let lift = Vec3::Y;
        let other = chased.unwrap_or(hero);
        let (eye, target, fov) = match self.shot {
            Shot::Helicopter => {
                let a = self.t * 0.06 + self.count as f32;
                let around = fwd * (-26.0 * a.cos()) + side * (18.0 * a.sin());
                let mid = (hero.pos + other.pos) * 0.5;
                (hero.pos + around + lift * 20.0, mid + lift, 0.75)
            }
            Shot::Roadside => (self.spot, hero.pos.lerp(other.pos, 0.3) + lift * 0.8, 0.62),
            Shot::Low => (hero.pos - fwd * 5.2 + side * 1.1 + lift * 0.65, hero.pos + fwd * 25.0 + lift * 1.2, 1.15),
            Shot::Alongside => {
                let slide = (self.t / self.length - 0.5) * 6.0;
                let o = Vec3::new(other.forward().x, 0.0, other.forward().y);
                let s = Vec3::new(o.z, 0.0, -o.x);
                (other.pos + s * 5.5 + o * slide + lift * 1.1, other.pos + lift * 0.7, 0.95)
            }
            Shot::Ahead => {
                let o = Vec3::new(other.forward().x, 0.0, other.forward().y);
                (other.pos + o * 13.0 + lift * 2.0, (hero.pos + other.pos) * 0.5 + lift * 0.8, 0.8)
            }
        };
        let eye = if self.shot == Shot::Helicopter { eye } else { keep_in(world, eye, at.index) };
        Camera { eye, target, up: Vec3::Y, fov }
    }

    /// Cuts to the next shot.
    fn next(&mut self, world: &World, hero: &Vehicle, hint: usize, chasing: bool, tunnel: bool) {
        const ORDER: [Shot; 6] = [Shot::Helicopter, Shot::Low, Shot::Roadside, Shot::Ahead, Shot::Alongside, Shot::Low];
        self.count += 1;
        let mut shot = ORDER[self.count as usize % ORDER.len()];
        if !chasing && matches!(shot, Shot::Ahead | Shot::Alongside) {
            shot = Shot::Low;
        }
        if tunnel && shot == Shot::Helicopter {
            shot = Shot::Low;
        }
        self.shot = shot;
        self.t = 0.0;
        self.length = match shot {
            Shot::Helicopter => 8.0,
            Shot::Roadside => 9.0,
            _ => 6.0,
        };
        if shot == Shot::Roadside {
            // Ahead, at the road's edge, on alternate sides.
            let at = world.road.locate(hero.pos, Some(hint));
            let ahead = 40.0 + hero.vel.length() * 2.5;
            let s = world.road.wrap(at.s + ahead);
            let (_, wall) = world.road.widths(s);
            let side = if self.count.is_multiple_of(2) { 1.0 } else { -1.0 };
            self.spot = world.road.point(s, side * (wall - 1.0)) + Vec3::Y * 1.3;
            self.spot_hint = world.road.locate(self.spot, None).index;
        }
    }
}
