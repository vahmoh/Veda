//! The cameras that follow a car: from behind (near or far), from the
//! bonnet and from the bumper, and an orbit for the menus and replays.
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
            View::Chase => (6.2 + speed * 0.02, 2.1 + speed * 0.004, 10.0),
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
        // Kept inside the walls and above the ground.
        let at = world.road.locate(cam.eye, Some(hint));
        let (_, wall) = world.road.widths(at.s);
        if at.lateral.abs() > wall - 0.4 {
            let (_, _, r, _) = world.road.frame(at.s);
            let over = at.lateral.abs() - (wall - 0.4);
            cam.eye -= Vec3::new(r.x, 0.0, r.z).normalize_or(Vec3::X) * (over * at.lateral.signum());
        }
        let floor = at.height + 0.4;
        if cam.eye.y < floor {
            cam.eye.y = floor;
        }
        // In a tunnel, under its roof.
        let sample = world.road.sample(at.index as isize);
        if sample.section == crate::road::Section::Tunnel {
            cam.eye.y = cam.eye.y.min(at.height + 5.8);
        }
        // Shake: a few incommensurate waves.
        if self.shake > 0.01 {
            let t = self.shake_time;
            let s = self.shake * 0.25;
            cam.eye += Vec3::new((t * 37.0).sin() * s, (t * 43.0).sin() * s * 0.7, (t * 29.0).cos() * s);
        }
        cam
    }
}
