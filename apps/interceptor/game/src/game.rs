//! The running game: the cars on the road and who drives them, the
//! physics stepping at a fixed rate, the camera, and what a frame shows.
//!
//! Physics runs in steps of [`STEP`] whatever the frame rate, so that the
//! cars drive the same on a slow machine as on a fast one; frames are
//! drawn between the last two steps, interpolated, so that motion stays
//! smooth at any frame rate.

use alloc::vec::Vec;

use crate::prelude::*;

use crate::camera::Follow;
use crate::carmodel::Kind;
use crate::env::Environment;
use crate::input::{Driver, Keys};
use crate::physics::{self, Impact};
use crate::render::{Camera, CarView, Light, Livery};
use crate::vehicle::{Controls, Spec, Vehicle};
use crate::world::World;

/// The physics' step (s).
pub const STEP: f32 = 1.0 / 120.0;
/// The most time a frame catches up (s): a longer stall slows the game
/// rather than making it jump.
const MAX_CATCH_UP: f32 = 0.1;

/// Where a car was at the last step, to draw it between steps.
#[derive(Clone, Copy, Debug)]
struct Pose {
    pos: Vec3,
    yaw: f32,
    pitch: f32,
    roll: f32,
    up: Vec3,
    spin: f32,
    steer: f32,
}

impl Pose {
    fn of(v: &Vehicle) -> Pose {
        Pose { pos: v.pos, yaw: v.yaw, pitch: v.pitch, roll: v.roll, up: v.up, spin: v.spin, steer: v.steer }
    }

    fn lerp(&self, o: &Pose, t: f32) -> Pose {
        let mut dyaw = (o.yaw - self.yaw) % core::f32::consts::TAU;
        if dyaw > core::f32::consts::PI {
            dyaw -= core::f32::consts::TAU;
        } else if dyaw < -core::f32::consts::PI {
            dyaw += core::f32::consts::TAU;
        }
        Pose {
            pos: self.pos.lerp(o.pos, t),
            yaw: self.yaw + dyaw * t,
            pitch: self.pitch + (o.pitch - self.pitch) * t,
            roll: self.roll + (o.roll - self.roll) * t,
            up: self.up.lerp(o.up, t).normalize_or(Vec3::Y),
            spin: self.spin + (o.spin - self.spin) * t,
            steer: self.steer + (o.steer - self.steer) * t,
        }
    }

    fn transform(&self) -> Mat4 {
        let fwd = Vec3::new(self.yaw.sin(), 0.0, self.yaw.cos());
        let f = (fwd - self.up * fwd.dot(self.up)).normalize_or(fwd);
        let left = self.up.cross(f).normalize_or(Vec3::X);
        let basis = Mat4::from_cols(left.extend(0.0), self.up.extend(0.0), f.extend(0.0), self.pos.extend(1.0));
        basis * Mat4::from_quat(Quat::from_rotation_z(self.roll) * Quat::from_rotation_x(self.pitch))
    }
}

/// A car in the game.
#[derive(Clone, Debug)]
pub struct Car {
    pub kind: Kind,
    pub vehicle: Vehicle,
    pub paint: Vec3,
    pub livery: Livery,
    /// The road sample it is near.
    pub hint: usize,
    pub controls: Controls,
    pub siren: bool,
    pub headlights: bool,
    /// How battered it is (0 new, 1 wrecked).
    pub damage: f32,
    prev: Pose,
}

impl Car {
    /// A car of `kind` on the road at `(s, lateral)`, facing forward (or
    /// back), at `speed`.
    pub fn on_road(world: &World, kind: Kind, spec: Spec, s: f32, lateral: f32, backwards: bool, speed: f32) -> Car {
        let (p, f, r, _) = world.road.frame(s);
        let f = if backwards { -f } else { f };
        let yaw = f.x.atan2(f.z);
        let pos = p + r * lateral;
        let mut vehicle = Vehicle::new(spec, pos, yaw);
        vehicle.set_speed(speed);
        let hint = world.road.locate(pos, None).index;
        let livery = match kind {
            Kind::Police => Livery::Police,
            Kind::Taxi => Livery::Taxi,
            _ => Livery::Plain,
        };
        Car {
            kind,
            prev: Pose::of(&vehicle),
            vehicle,
            paint: Vec3::new(0.5, 0.5, 0.5),
            livery,
            hint,
            controls: Controls::default(),
            siren: false,
            headlights: true,
            damage: 0.0,
        }
    }

    /// Where it is along the road (s) and across it.
    pub fn place(&self, world: &World) -> (f32, f32) {
        let at = world.road.locate(self.vehicle.pos, Some(self.hint));
        (at.s, at.lateral)
    }
}

/// The light bar's pattern at `t` seconds: each half flashes four times
/// in a burst, the halves taking turns, 1.6 bursts a second. `[red,
/// blue]`, each 0 or 1.
pub fn light_bar(t: f32) -> [f32; 2] {
    let half = |side: f32| {
        let phase = (t * 1.6 + side * 0.5).fract();
        if phase < 0.5 && (phase * 8.0).fract() >= 0.4 { 1.0 } else { 0.0 }
    };
    [half(0.0), half(1.0)]
}

/// Something that happened during a step that the sounds and the effects
/// want to know about.
#[derive(Clone, Copy, Debug)]
pub enum Event {
    /// Car `a` crashed into car `b` (or a wall, `b` none).
    Crash { a: usize, b: Option<usize>, impact: Impact },
}

/// The game.
pub struct Game {
    pub world: World,
    pub env: Environment,
    pub cars: Vec<Car>,
    pub player: usize,
    pub follow: Follow,
    /// The player's keys.
    pub keys: Keys,
    /// The game's clock (s), and the time not yet stepped.
    pub clock: f32,
    pending: f32,
    driver: Driver,
    /// What happened since the last frame.
    pub events: Vec<Event>,
    /// The last frame's camera.
    pub camera: Camera,
    pub paused: bool,
}

impl Game {
    /// A game in `world` lit by `env`, the player in a police car at the
    /// start of the road.
    pub fn new(world: World, env: Environment) -> Game {
        let mut player = Car::on_road(&world, Kind::Police, Spec::interceptor(), 20.0, 2.1, false, 0.0);
        player.paint = Vec3::new(0.02, 0.02, 0.02);
        let follow = Follow::new(&player.vehicle);
        let camera = Camera { eye: Vec3::ZERO, target: Vec3::Z, up: Vec3::Y, fov: 1.0 };
        let mut g = Game {
            world,
            env,
            cars: Vec::new(),
            player: 0,
            follow,
            keys: Keys::default(),
            clock: 0.0,
            pending: 0.0,
            driver: Driver::default(),
            events: Vec::new(),
            camera,
            paused: false,
        };
        g.cars.push(player);
        g
    }

    /// Lets go of the controls (the window lost the keyboard).
    pub fn release(&mut self) {
        self.keys = Keys::default();
        self.driver.release();
    }

    /// Advances the game by `dt` seconds of real time.
    pub fn update(&mut self, dt: f32) {
        self.events.clear();
        if self.paused {
            return;
        }
        self.pending = (self.pending + dt).min(MAX_CATCH_UP);
        while self.pending >= STEP {
            self.pending -= STEP;
            self.step();
        }
    }

    /// One physics step.
    fn step(&mut self) {
        self.clock += STEP;
        let p = self.player;
        let speed = self.cars[p].vehicle.speed();
        self.cars[p].controls = self.driver.update(self.keys, speed, STEP);
        let world = &self.world;
        for (i, car) in self.cars.iter_mut().enumerate() {
            car.prev = Pose::of(&car.vehicle);
            let hint = car.hint;
            let ground = |q: Vec3| world.ground(q, hint);
            let c = car.controls;
            car.vehicle.step(&c, STEP, &ground);
            if let Some(impact) = physics::collide_walls(world, &mut car.vehicle, car.hint)
                && impact.speed > 1.0
            {
                self.events.push(Event::Crash { a: i, b: None, impact });
            }
            car.hint = world.road.locate(car.vehicle.pos, Some(car.hint)).index;
        }
        // Car against car: those near each other along the road.
        let n = self.cars.len();
        for i in 0..n {
            for j in i + 1..n {
                let (a, b) = self.cars.split_at_mut(j);
                if let Some(impact) = physics::collide_cars(&mut a[i].vehicle, &mut b[0].vehicle)
                    && impact.speed > 0.5
                {
                    self.events.push(Event::Crash { a: i, b: Some(j), impact });
                }
            }
        }
        for e in &self.events {
            let Event::Crash { a, b, impact } = *e;
            let hit = (impact.speed / 25.0).min(1.0);
            for c in [Some(a), b].into_iter().flatten() {
                self.cars[c].damage = (self.cars[c].damage + hit * hit * 0.25).min(1.0);
                if c == self.player {
                    self.follow.jolt(hit);
                }
            }
        }
    }

    /// What to draw now: every car between its last two steps, and the
    /// lights that move with them (headlights, light bars); the camera
    /// follows the player (`dt` moves it).
    pub fn frame(&mut self, dt: f32) -> (Vec<CarView>, Vec<Light>) {
        let t = (self.pending / STEP).clamp(0.0, 1.0);
        let mut views = Vec::with_capacity(self.cars.len());
        let mut lights = Vec::new();
        let night = self.env.street_lights;
        for (i, car) in self.cars.iter().enumerate() {
            let pose = car.prev.lerp(&Pose::of(&car.vehicle), t);
            let m = pose.transform();
            // Each car's bar keeps its own time, so that a convoy does not
            // flash as one.
            let bar = if car.siren { light_bar(self.clock + i as f32 * 0.37) } else { [0.0, 0.0] };
            let half = car.vehicle.spec.half_length;
            if car.headlights && night > 0.0 {
                let pos = m.transform_point3(Vec3::new(0.0, 0.72, half + 0.2));
                let dir = m.transform_vector3(Vec3::new(0.0, -0.09, 1.0)).normalize();
                lights.push(Light {
                    pos,
                    color: Vec3::new(1.0, 0.94, 0.84) * 70.0,
                    radius: 50.0,
                    spot: Some((dir, 0.86)),
                });
            }
            if car.siren {
                let roof = m.transform_point3(Vec3::new(0.0, 1.75, 0.0));
                let side = m.transform_vector3(Vec3::X) * 0.4;
                // Daylight drowns most of it.
                let k = 0.15 + 0.85 * night.min(1.0);
                if bar[0] > 0.0 {
                    lights.push(Light {
                        pos: roof + side,
                        color: Vec3::new(1.0, 0.04, 0.02) * (30.0 * k),
                        radius: 22.0,
                        spot: None,
                    });
                }
                if bar[1] > 0.0 {
                    lights.push(Light {
                        pos: roof - side,
                        color: Vec3::new(0.05, 0.2, 1.0) * (36.0 * k),
                        radius: 22.0,
                        spot: None,
                    });
                }
            }
            views.push(CarView {
                kind: car.kind,
                transform: pose.transform(),
                paint: car.paint,
                livery: car.livery,
                spin: pose.spin,
                steer: pose.steer,
                headlights: if car.headlights { 1.0 } else { 0.0 },
                brake: if car.vehicle.braking > 0.05 { 1.0 } else { 0.0 },
                bar,
            });
        }
        // The camera follows the player as drawn.
        let player = &self.cars[self.player];
        let mut seen = player.vehicle.clone();
        let pose = player.prev.lerp(&Pose::of(&player.vehicle), t);
        seen.pos = pose.pos;
        seen.yaw = pose.yaw;
        self.camera = self.follow.update(&seen, &self.world, player.hint, dt);
        (views, lights)
    }

    /// The player's car.
    pub fn player(&self) -> &Car {
        &self.cars[self.player]
    }

    /// Switches the player's siren and light bar on or off.
    pub fn toggle_siren(&mut self) {
        let p = self.player;
        self.cars[p].siren = !self.cars[p].siren;
    }

    /// Puts the player's car back on the road where it is, in its lane,
    /// facing forward and standing still (after a spin or getting stuck).
    pub fn reset_player(&mut self) {
        let p = self.player;
        let (s, lateral) = self.cars[p].place(&self.world);
        let lane = if lateral < 0.0 { -2.1 } else { 2.1 };
        let old = &self.cars[p];
        let mut car = Car::on_road(&self.world, old.kind, old.vehicle.spec, s, lane, lateral < 0.0, 0.0);
        car.paint = old.paint;
        car.siren = old.siren;
        car.damage = old.damage;
        self.cars[p] = car;
        self.follow = Follow::new(&self.cars[p].vehicle);
    }
}
