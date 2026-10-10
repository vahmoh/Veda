//! The running game: the cars on the road and who drives them, the
//! physics stepping at a fixed rate, the camera, and what a frame shows.
//!
//! Physics runs in steps of [`STEP`] whatever the frame rate, so that the
//! cars drive the same on a slow machine as on a fast one; frames are
//! drawn between the last two steps, interpolated, so that motion stays
//! smooth at any frame rate.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

use crate::prelude::*;

use vmath::Rng;

use crate::camera::Follow;
use crate::carmodel::Kind;
use crate::driver::{Place, Role, Scene};
use crate::env::Environment;
use crate::input::{Driver, Keys};
use crate::physics::{self, Impact};
use crate::render::{Camera, CarView, Light, Livery};
use crate::traffic;
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

/// The next car's [`Car::id`].
static NEXT_ID: AtomicU32 = AtomicU32::new(1);

/// A car in the game.
#[derive(Clone, Debug)]
pub struct Car {
    /// Which car this is, for as long as it is on the road (its sounds
    /// follow it by this; its place in the game's list changes).
    pub id: u32,
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
    /// Who drives it.
    pub role: Role,
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
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
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
            role: Role::Parked,
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

/// The player's nitrous: a few seconds of half again the engine's power,
/// filling up again slowly.
#[derive(Clone, Copy, Debug)]
pub struct Boost {
    /// How much is left (0..1).
    pub level: f32,
    /// Burning now.
    pub on: bool,
}

impl Boost {
    /// Seconds a full charge lasts, and to fill an empty one.
    const BURN: f32 = 5.0;
    const FILL: f32 = 25.0;

    pub fn new() -> Boost {
        Boost { level: 1.0, on: false }
    }

    /// Burns while `held` (and while there is some left), fills otherwise.
    pub fn update(&mut self, held: bool, dt: f32) {
        self.on = held && self.level > 0.0;
        if self.on {
            self.level = (self.level - dt / Self::BURN).max(0.0);
        } else {
            self.level = (self.level + dt / Self::FILL).min(1.0);
        }
    }

    /// The engine's power with it.
    pub fn power(&self) -> f32 {
        if self.on { 1.5 } else { 1.0 }
    }
}

/// How hard a suspect `lead` metres ahead of its pursuer drives: flat
/// out while the police are close, easing off as its lead grows, so that a
/// chase stays a chase (and still a hard one).
pub fn pace(lead: f32) -> f32 {
    1.0 - ((lead - 40.0) / 210.0).clamp(0.0, 1.0) * 0.35
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
    /// Veda Bay (made once, shared by every game played in it).
    pub world: Arc<World>,
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
    /// Traffic round the player.
    pub traffic: bool,
    /// The computer drives the player's car (for demonstrations and the
    /// tests).
    pub autopilot: bool,
    autopilot_role: Role,
    rng: Rng,
    /// The player's nitrous.
    pub boost: Boost,
    /// Every car's place, as the drivers saw it at the last step.
    pub scene: Scene,
    traffic_due: f32,
}

impl Game {
    /// A game in `world` lit by `env`, the player in a police car at the
    /// start of the road.
    pub fn new(world: impl Into<Arc<World>>, env: Environment) -> Game {
        let world = world.into();
        let mut player = Car::on_road(&world, Kind::Police, Spec::interceptor(), 20.0, 2.1, false, 0.0);
        player.paint = Vec3::new(0.02, 0.02, 0.02);
        player.role = Role::Player;
        let follow = Follow::new(&player.vehicle);
        let camera = Camera { eye: Vec3::ZERO, target: Vec3::Z, up: Vec3::Y, fov: 1.0 };
        let length = world.road.length();
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
            traffic: false,
            autopilot: false,
            autopilot_role: Role::Traffic(crate::driver::Traffic::new(0, 1.0, 1.15)),
            rng: Rng::new(crate::world::SEED ^ 0x7AFF),
            boost: Boost::new(),
            scene: Scene { places: Vec::new(), length },
            traffic_due: 0.0,
        };
        g.cars.push(player);
        g
    }

    /// Fills the road round the player with traffic (and keeps it busy
    /// from then on).
    pub fn start_traffic(&mut self) {
        self.traffic = true;
        let player_s = self.cars[self.player].place(&self.world).0;
        let want = traffic::wanted(&self.world, player_s);
        let mut tries = 0;
        while self.traffic_count() < want && tries < want * 20 {
            tries += 1;
            let slot = traffic::any_slot(&mut self.rng, &self.world.road, player_s);
            self.add_traffic(&slot);
        }
    }

    fn traffic_count(&self) -> usize {
        self.cars.iter().filter(|c| matches!(c.role, Role::Traffic(_))).count()
    }

    /// Every car's s and lateral offset.
    fn spots(&self) -> Vec<(f32, f32)> {
        self.cars.iter().map(|c| c.place(&self.world)).collect()
    }

    /// Puts a new traffic car at `slot` if there is room; true if it did.
    fn add_traffic(&mut self, slot: &traffic::Slot) -> bool {
        if !traffic::free(&self.world.road, &self.spots(), slot, 24.0) {
            return false;
        }
        let car = self.traffic_car(slot);
        self.cars.push(car);
        true
    }

    /// A traffic car at `slot`, at the speed its lane drives at.
    fn traffic_car(&mut self, slot: &traffic::Slot) -> Car {
        let road = &self.world.road;
        let section = road.at(slot.s).section;
        let (kind, spec, paint) = traffic::pick(&mut self.rng, section);
        let lateral = crate::road::Road::lane_center(slot.lane, slot.side);
        let role = traffic::role(&mut self.rng, slot);
        let keen = match role {
            Role::Traffic(t) => t.keen,
            _ => 1.0,
        };
        let speed = section.speed_limit() * keen * 0.9;
        let mut car = Car::on_road(&self.world, kind, spec, slot.s, lateral, slot.side < 0.0, speed);
        car.paint = paint;
        car.role = role;
        car.headlights = true;
        car
    }

    /// Moves traffic that has left the window round the player back to
    /// its far end, and adds cars where the road is busier.
    fn keep_traffic(&mut self) {
        let player_s = self.cars[self.player].place(&self.world).0;
        for i in 0..self.cars.len() {
            if !matches!(self.cars[i].role, Role::Traffic(_)) {
                continue;
            }
            let s = self.cars[i].place(&self.world).0;
            if traffic::gone(&self.world.road, player_s, s) {
                // Somewhere new, out of sight; tried a few times.
                for _ in 0..8 {
                    let slot = traffic::far_slot(&mut self.rng, &self.world.road, player_s);
                    if traffic::free(&self.world.road, &self.spots(), &slot, 24.0) {
                        self.cars[i] = self.traffic_car(&slot);
                        break;
                    }
                }
            }
        }
        let want = traffic::wanted(&self.world, player_s);
        if self.traffic_count() < want {
            let slot = traffic::far_slot(&mut self.rng, &self.world.road, player_s);
            self.add_traffic(&slot);
        }
    }

    /// Where every car is, as the drivers see it.
    fn update_scene(&mut self) {
        let road = &self.world.road;
        self.scene.places.clear();
        for c in &self.cars {
            let at = road.locate(c.vehicle.pos, Some(c.hint));
            let (_, f, _, _) = road.frame(at.s);
            let flat = Vec2::new(f.x, f.z).normalize_or(Vec2::Y);
            let along = c.vehicle.vel.dot(flat);
            let dir = match c.role {
                Role::Traffic(t) => t.side,
                // Chases run along the road.
                Role::Suspect(_) | Role::Police(_) => 1.0,
                _ => {
                    if c.vehicle.forward().dot(flat) >= 0.0 {
                        1.0
                    } else {
                        -1.0
                    }
                }
            };
            self.scene.places.push(Place {
                s: at.s,
                lateral: at.lateral,
                along,
                dir,
                length: c.vehicle.spec.half_length * 2.0,
                siren: c.siren,
            });
        }
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
        // This step's events follow the frame's earlier steps'.
        let first = self.events.len();
        let p = self.player;
        let speed = self.cars[p].vehicle.speed();
        self.cars[p].controls = self.driver.update(self.keys, speed, STEP);
        self.update_scene();
        // The computer's drivers; their engines' power as the chase has it.
        let road = &self.world.road;
        let mut power: Vec<f32> = self.cars.iter().map(|_| 1.0).collect();
        for (i, engine) in power.iter_mut().enumerate() {
            let mut role = if i == p && self.autopilot { self.autopilot_role } else { self.cars[i].role };
            let car = &self.cars[i];
            let c = match &mut role {
                Role::Player => continue,
                Role::Parked => Controls { brake: 1.0, ..Controls::default() },
                Role::Traffic(t) => t.drive(i, road, &self.scene, &car.vehicle, STEP),
                Role::Suspect(sus) => {
                    let lead = self.scene.gap(self.scene.places[sus.pursuer].s, self.scene.places[i].s);
                    let pace = pace(lead);
                    *engine = pace;
                    sus.drive(i, road, &self.scene, &car.vehicle, car.damage, pace, STEP)
                }
                Role::Police(pol) => {
                    // Pursuit-tuned, and given its head when fallen behind.
                    let behind = self.scene.gap(self.scene.places[i].s, self.scene.places[pol.target].s);
                    *engine = 1.1 + ((behind - 40.0) / 160.0).clamp(0.0, 1.0) * 0.4;
                    pol.drive(i, road, &self.scene, &car.vehicle, STEP)
                }
            };
            self.cars[i].controls = c;
            if i == p && self.autopilot {
                self.autopilot_role = role;
            } else {
                self.cars[i].role = role;
            }
        }
        // The player's nitrous.
        self.boost.update(self.keys.boost && !self.autopilot, STEP);
        power[p] *= self.boost.power();
        let world = &self.world;
        for (i, car) in self.cars.iter_mut().enumerate() {
            car.vehicle.power = power[i] * (1.0 - 0.3 * car.damage);
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
        // Car against car: those near each other along the road (sorted
        // by their place, each against those within reach of it).
        let n = self.cars.len();
        let mut order: Vec<usize> = (0..n).collect();
        let places = &self.scene.places;
        order.sort_by(|&a, &b| places[a].s.total_cmp(&places[b].s));
        let length = self.scene.length;
        for (k, &i) in order.iter().enumerate() {
            for step in 1..n {
                let j = order[(k + step) % n];
                let gap = (places[j].s - places[i].s).rem_euclid(length);
                if gap > 12.0 {
                    break;
                }
                let (lo, hi) = (i.min(j), i.max(j));
                let (a, b) = self.cars.split_at_mut(hi);
                if let Some(impact) = physics::collide_cars(&mut a[lo].vehicle, &mut b[0].vehicle)
                    && impact.speed > 0.5
                {
                    self.events.push(Event::Crash { a: lo, b: Some(hi), impact });
                }
            }
        }
        for e in &self.events[first..] {
            let Event::Crash { a, b, impact } = *e;
            for c in [Some(a), b].into_iter().flatten() {
                self.cars[c].damage = (self.cars[c].damage + impact.harm()).min(1.0);
                if c == self.player {
                    self.follow.jolt(impact.hit());
                }
                // Traffic stops, shaken, after a real knock.
                if let Role::Traffic(t) = &mut self.cars[c].role
                    && impact.speed > 4.0
                {
                    t.shaken = 2.5;
                }
            }
        }
        if self.traffic {
            self.traffic_due -= STEP;
            if self.traffic_due <= 0.0 {
                self.traffic_due = 0.5;
                self.keep_traffic();
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

    /// Takes car `i` off the road (not the player's). Cars after it move
    /// down one place: references to them (the player, a police car's
    /// target, a suspect's pursuer) are moved with them.
    pub fn remove_car(&mut self, i: usize) {
        if i == self.player || i >= self.cars.len() {
            return;
        }
        self.cars.remove(i);
        let shift = |j: &mut usize| {
            if *j > i {
                *j -= 1;
            }
        };
        shift(&mut self.player);
        for c in self.cars.iter_mut() {
            match &mut c.role {
                Role::Police(p) => shift(&mut p.target),
                Role::Suspect(s) => shift(&mut s.pursuer),
                _ => {}
            }
        }
        if let Role::Police(p) = &mut self.autopilot_role {
            shift(&mut p.target);
        }
    }

    /// Lets the computer drive the player's car (or gives it back): as
    /// police after `target` if there is one, as traffic otherwise.
    pub fn set_autopilot(&mut self, on: bool, target: Option<usize>) {
        self.autopilot = on;
        let lane = crate::road::Road::lane_center(0, 1.0);
        self.autopilot_role = match target {
            Some(t) => Role::Police(crate::driver::Police::new(lane, t, 1.0)),
            None => Role::Traffic(crate::driver::Traffic::new(0, 1.0, 1.15)),
        };
    }

    /// Has the computer, driving the player's car after a suspect, follow
    /// it `distance` metres behind rather than ram it.
    pub fn autopilot_follows(&mut self, distance: f32) {
        if let Role::Police(p) = &mut self.autopilot_role {
            p.follow = Some(distance);
        }
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

    /// Puts the player's car on the road at `s`, in the right-hand lane,
    /// at `speed`, the camera behind it.
    pub fn place_player(&mut self, s: f32, speed: f32) {
        let p = self.player;
        let old = &self.cars[p];
        let mut car = Car::on_road(&self.world, old.kind, old.vehicle.spec, s, 2.1, false, speed);
        car.id = old.id;
        car.paint = old.paint;
        car.siren = old.siren;
        car.role = old.role;
        self.cars[p] = car;
        let view = self.follow.view;
        self.follow = Follow::new(&self.cars[p].vehicle);
        self.follow.view = view;
    }

    /// Puts the player's car back on the road where it is, in its lane,
    /// facing forward and standing still (after a spin or getting stuck).
    pub fn reset_player(&mut self) {
        let p = self.player;
        let (s, lateral) = self.cars[p].place(&self.world);
        let lane = if lateral < 0.0 { -2.1 } else { 2.1 };
        let old = &self.cars[p];
        let mut car = Car::on_road(&self.world, old.kind, old.vehicle.spec, s, lane, lateral < 0.0, 0.0);
        car.id = old.id;
        car.paint = old.paint;
        car.siren = old.siren;
        car.damage = old.damage;
        self.cars[p] = car;
        let view = self.follow.view;
        self.follow = Follow::new(&self.cars[p].vehicle);
        self.follow.view = view;
    }
}
