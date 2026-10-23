// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The running game: the cars on the road and who drives them, the
//! physics stepping at a fixed rate, the camera, and what a frame shows.
//!
//! Physics runs in steps of [`STEP`] whatever the frame rate, so that the
//! cars drive the same on a slow machine as on a fast one; frames are
//! drawn between the last two steps, interpolated, so that motion stays
//! smooth at any frame rate.
//!
//! Each car has a number of its own, its [`CarId`], given as it joins the
//! game and never given to another: what refers to a car (a police car's
//! target, a suspect's pursuer, a chase's suspect, a crash) holds its
//! number, and [`Game::index`] finds the car, or finds it gone.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

use rand::SeedableRng;
use rand_pcg::Pcg32;

use crate::camera::Follow;
use crate::carmodel::Kind;
use crate::driver::{Place, Role, Scene};
use crate::env::Environment;
use crate::input::{Driver, Keys};
use crate::physics::{self, Impact};
use crate::prelude::*;
use crate::render::{Camera, CarView, Light, Livery};
use crate::traffic;
use crate::vehicle::{Controls, Pose, Vehicle};
use crate::world::World;

/// The physics' steps a second, and its step (s).
pub const RATE: u32 = 120;
pub const STEP: f32 = 1.0 / RATE as f32;
/// The most time a frame catches up (s): a longer stall slows the game
/// rather than making it jump.
const MAX_CATCH_UP: f32 = 0.1;

/// Which car this is, as long as the game lasts: given as it joins the
/// game ([`Game::add_car`]), never given to another car of the game.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CarId(pub u32);

/// The games made so far (see [`Game::serial`]).
static GAMES: AtomicU32 = AtomicU32::new(0);

/// A car in the game.
#[derive(Clone, Debug)]
pub struct Car {
    /// Which car this is (none, `CarId(0)`, until it joins a game).
    pub id: CarId,
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
    /// back), at `speed`, standing for now (until it is given a driver).
    pub fn on_road(world: &World, kind: Kind, s: f32, lateral: f32, backwards: bool, speed: f32) -> Car {
        let vehicle = Vehicle::new(kind, Vec3::ZERO, 0.0);
        let livery = match kind {
            Kind::Police => Livery::Police,
            Kind::Taxi => Livery::Taxi,
            _ => Livery::Plain,
        };
        let mut car = Car {
            id: CarId(0),
            kind,
            prev: Pose::of(&vehicle),
            vehicle,
            paint: Vec3::new(0.5, 0.5, 0.5),
            livery,
            hint: 0,
            controls: Controls::default(),
            siren: false,
            headlights: true,
            damage: 0.0,
            role: Role::Parked,
        };
        car.put(world, s, lateral, backwards, speed);
        car
    }

    /// Puts it on the road at `(s, lateral)`, facing forward (or back), at
    /// `speed`: the same car, with its driver, its looks and its damage,
    /// somewhere else.
    pub fn put(&mut self, world: &World, s: f32, lateral: f32, backwards: bool, speed: f32) {
        let (p, f, r, _) = world.road.frame(s);
        let f = if backwards { -f } else { f };
        let pos = p + r * lateral;
        let mut vehicle = Vehicle::new(self.kind, pos, libm::atan2f(f.x, f.z));
        vehicle.set_speed(speed);
        self.hint = world.road.locate(pos, None).index;
        self.prev = Pose::of(&vehicle);
        self.vehicle = vehicle;
        self.controls = Controls::default();
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
pub fn light_bar(t: f64) -> [f32; 2] {
    let half = |side: f64| {
        let phase = (t * 1.6 + side * 0.5) % 1.0;
        if phase < 0.5 && (phase * 8.0) % 1.0 >= 0.4 { 1.0 } else { 0.0 }
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

impl Default for Boost {
    fn default() -> Boost {
        Boost::new()
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
    Crash { a: CarId, b: Option<CarId>, impact: Impact },
}

/// The game.
pub struct Game {
    /// Veda Bay (made once, shared by every game played in it).
    pub world: Arc<World>,
    pub env: Environment,
    pub cars: Vec<Car>,
    /// Where the player's car is in `cars` (it is never taken off).
    pub player: usize,
    /// The chase camera behind the player's car, unless another camera
    /// films the game (the menus' showcase).
    pub follow: Option<Follow>,
    /// The player's keys.
    pub keys: Keys,
    /// Steps taken since it began (its clock), and the time not yet
    /// stepped (s).
    steps: u64,
    pending: f32,
    /// How far the time drawn has moved since the last frame (s).
    moved: f32,
    /// How far the cameras moved at the last frame (s).
    pub frame_time: f32,
    driver: Driver,
    /// What happened since the last frame.
    pub events: Vec<Event>,
    /// Where the last step's events start in `events`.
    step_first: usize,
    /// The last frame's camera.
    pub camera: Camera,
    pub paused: bool,
    /// Traffic round the player.
    pub traffic: bool,
    /// The computer drives the player's car (for demonstrations and the
    /// tests).
    pub autopilot: bool,
    autopilot_role: Role,
    rng: Pcg32,
    /// The player's nitrous.
    pub boost: Boost,
    /// Every car's place, as the drivers saw it at the last step.
    pub scene: Scene,
    traffic_due: f32,
    /// The number the next car gets, and this game's own.
    next_id: u32,
    serial: u32,
    /// Each step's working: the engines' power, and the cars in order
    /// along the road.
    power: Vec<f32>,
    order: Vec<usize>,
}

impl Game {
    /// A game in `world` lit by `env`, the player in a police car at the
    /// start of the road.
    pub fn new(world: impl Into<Arc<World>>, env: Environment) -> Game {
        let world = world.into();
        let mut player = Car::on_road(&world, Kind::Police, 20.0, 2.1, false, 0.0);
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
            follow: Some(follow),
            keys: Keys::default(),
            steps: 0,
            pending: 0.0,
            moved: 0.0,
            frame_time: 0.0,
            driver: Driver::default(),
            events: Vec::new(),
            step_first: 0,
            camera,
            paused: false,
            traffic: false,
            autopilot: false,
            autopilot_role: Role::Traffic(crate::driver::Traffic::new(0, 1.0, 1.15)),
            rng: Pcg32::seed_from_u64(crate::world::SEED ^ 0x7AFF),
            boost: Boost::new(),
            scene: Scene { places: Vec::new(), length },
            traffic_due: 0.0,
            next_id: 1,
            serial: GAMES.fetch_add(1, Ordering::Relaxed),
            power: Vec::new(),
            order: Vec::new(),
        };
        g.add_car(player);
        g
    }

    /// Which game this is (each game made has its own number): for what
    /// outlives a game and follows its cars by their numbers, which every
    /// game gives from one (the speakers' voices).
    pub fn serial(&self) -> u32 {
        self.serial
    }

    /// The game's clock: seconds since it began.
    pub fn clock(&self) -> f64 {
        self.steps as f64 / f64::from(RATE)
    }

    /// Steps taken since it began.
    pub fn steps(&self) -> u64 {
        self.steps
    }

    /// Puts `car` on the road in the game, giving it its number.
    pub fn add_car(&mut self, mut car: Car) -> CarId {
        car.id = self.new_id();
        let id = car.id;
        self.cars.push(car);
        id
    }

    /// A number no car of the game has had.
    fn new_id(&mut self) -> CarId {
        let id = CarId(self.next_id);
        self.next_id += 1;
        id
    }

    /// Where car `id` is in `cars`, if it is still on the road.
    pub fn index(&self, id: CarId) -> Option<usize> {
        self.cars.iter().position(|c| c.id == id)
    }

    /// Car `id`, if it is still on the road.
    pub fn car(&self, id: CarId) -> Option<&Car> {
        self.cars.iter().find(|c| c.id == id)
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
        self.add_car(car);
        true
    }

    /// A traffic car at `slot`, at the speed its lane drives at.
    fn traffic_car(&mut self, slot: &traffic::Slot) -> Car {
        let road = &self.world.road;
        let section = road.at(slot.s).section;
        let (kind, paint) = traffic::pick(&mut self.rng, section);
        let lateral = crate::road::Road::lane_center(slot.lane, slot.side);
        let role = traffic::role(&mut self.rng, slot);
        let keen = match role {
            Role::Traffic(t) => t.keen,
            _ => 1.0,
        };
        let speed = section.speed_limit() * keen * 0.9;
        let mut car = Car::on_road(&self.world, kind, slot.s, lateral, slot.side < 0.0, speed);
        car.paint = paint;
        car.role = role;
        car.headlights = true;
        car
    }

    /// Replaces traffic that has left the window round the player with a
    /// new car at its far end, and adds cars where the road is busier.
    fn keep_traffic(&mut self) {
        let player_s = self.cars[self.player].place(&self.world).0;
        for i in 0..self.cars.len() {
            if !matches!(self.cars[i].role, Role::Traffic(_)) {
                continue;
            }
            let s = self.cars[i].place(&self.world).0;
            if traffic::gone(&self.world.road, player_s, s) {
                // Somewhere new, out of sight; tried a few times. Another
                // car: what referred to the one gone finds it gone.
                for _ in 0..8 {
                    let slot = traffic::far_slot(&mut self.rng, &self.world.road, player_s);
                    if traffic::free(&self.world.road, &self.spots(), &slot, 24.0) {
                        let mut car = self.traffic_car(&slot);
                        car.id = self.new_id();
                        self.cars[i] = car;
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
            let v = &c.vehicle;
            let middle = v.middle();
            let at = road.locate(Vec3::new(middle.x, v.pos.y, middle.y), Some(c.hint));
            let (_, f, _, _) = road.frame(at.s);
            let flat = Vec2::new(f.x, f.z).normalize_or(Vec2::Y);
            let along = v.vel.dot(flat);
            let dir = match c.role {
                Role::Traffic(t) => t.side,
                // Chases run along the road.
                Role::Suspect(_) | Role::Police(_) => 1.0,
                _ => {
                    if v.forward().dot(flat) >= 0.0 {
                        1.0
                    } else {
                        -1.0
                    }
                }
            };
            self.scene.places.push(Place {
                id: c.id,
                s: at.s,
                lateral: at.lateral,
                along,
                dir,
                length: v.chassis.half_length * 2.0,
                siren: c.siren,
            });
        }
    }

    /// Lets go of the controls (the window lost the keyboard).
    pub fn release(&mut self) {
        self.keys = Keys::default();
        self.driver.release();
    }

    /// Advances the game by `dt` seconds of real time ([`Game::update_with`],
    /// nothing done after each step).
    pub fn update(&mut self, dt: f32) -> f32 {
        self.update_with(dt, |_| {})
    }

    /// Advances the game by `dt` seconds of real time, calling `each` after
    /// every step (whose events [`Game::step_events`] gives), so that what
    /// follows the game follows it step by step, whatever the frames.
    /// Returns how far the game moved on (s): `dt`, but no more than it
    /// catches up on after a stall, and nothing while paused.
    pub fn update_with(&mut self, dt: f32, mut each: impl FnMut(&mut Game)) -> f32 {
        let (steps, moved) = self.take_time(dt);
        for _ in 0..steps {
            self.step();
            each(self);
        }
        moved
    }

    /// Takes `dt` seconds of real time in: the last frame's events are
    /// forgotten, and the time is added to what is to be stepped (no more
    /// than the game catches up on after a stall, nothing while paused).
    /// Returns the steps due now, which the caller takes with
    /// [`Game::step`] (following each as it needs to), and how far the
    /// game moves on (s).
    pub fn take_time(&mut self, dt: f32) -> (usize, f32) {
        self.events.clear();
        self.step_first = 0;
        if self.paused {
            return (0, 0.0);
        }
        let pending = (self.pending + dt).min(MAX_CATCH_UP);
        let moved = pending - self.pending;
        self.moved += moved;
        self.pending = pending;
        let mut steps = 0;
        while self.pending >= STEP {
            self.pending -= STEP;
            steps += 1;
        }
        (steps, moved)
    }

    /// Where the last step's events are in `events`.
    pub fn step_events(&self) -> core::ops::Range<usize> {
        self.step_first..self.events.len()
    }

    /// One physics step (one of those [`Game::take_time`] made due).
    pub fn step(&mut self) {
        self.steps += 1;
        // This step's events follow the frame's earlier steps'.
        let first = self.events.len();
        self.step_first = first;
        let p = self.player;
        let speed = self.cars[p].vehicle.speed();
        self.cars[p].controls = self.driver.update(self.keys, speed, STEP);
        self.update_scene();
        self.drive();
        // The player's nitrous.
        self.boost.update(self.keys.boost && !self.autopilot, STEP);
        self.power[p] *= self.boost.power();
        let world = &self.world;
        for (car, power) in self.cars.iter_mut().zip(&self.power) {
            car.vehicle.power = power * (1.0 - 0.3 * car.damage);
            car.prev = Pose::of(&car.vehicle);
            let hint = car.hint;
            let ground = |q: Vec3| world.ground(q, hint);
            let c = car.controls;
            car.vehicle.step(&c, STEP, &ground);
            if let Some(impact) = physics::collide_walls(world, &mut car.vehicle, car.hint)
                && impact.speed > 1.0
            {
                self.events.push(Event::Crash { a: car.id, b: None, impact });
            }
            car.hint = world.road.locate(car.vehicle.pos, Some(car.hint)).index;
        }
        self.collide();
        for k in first..self.events.len() {
            let Event::Crash { a, b, impact } = self.events[k];
            for id in [Some(a), b].into_iter().flatten() {
                let Some(c) = self.index(id) else { continue };
                self.cars[c].damage = (self.cars[c].damage + impact.harm()).min(1.0);
                if c == self.player
                    && let Some(follow) = &mut self.follow
                {
                    follow.jolt(impact.hit());
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

    /// The computer's drivers take the controls for this step; their
    /// engines' power as the chase has it goes into `power`.
    fn drive(&mut self) {
        let p = self.player;
        let road = &self.world.road;
        let scene = &self.scene;
        self.power.clear();
        self.power.resize(self.cars.len(), 1.0);
        for i in 0..self.cars.len() {
            let autopilot = i == p && self.autopilot;
            let mut role = if autopilot { self.autopilot_role } else { self.cars[i].role };
            let car = &self.cars[i];
            let c = match &mut role {
                Role::Player => continue,
                Role::Parked => Controls { brake: 1.0, ..Controls::default() },
                Role::Traffic(t) => t.drive(i, road, scene, &car.vehicle, STEP),
                Role::Suspect(sus) => {
                    // Its lead over its pursuer sets its pace (alone, it
                    // eases right off).
                    let lead =
                        scene.find(sus.pursuer).map_or(f32::MAX, |j| scene.gap(scene.places[j].s, scene.places[i].s));
                    let pace = pace(lead);
                    self.power[i] = pace;
                    sus.drive(i, road, scene, &car.vehicle, car.damage, pace, STEP)
                }
                Role::Police(pol) => {
                    // Pursuit-tuned, and given its head when fallen behind.
                    let behind =
                        scene.find(pol.target).map_or(0.0, |t| scene.gap(scene.places[i].s, scene.places[t].s));
                    self.power[i] = 1.1 + ((behind - 40.0) / 160.0).clamp(0.0, 1.0) * 0.4;
                    pol.drive(i, road, scene, &car.vehicle, STEP)
                }
            };
            self.cars[i].controls = c;
            if autopilot {
                self.autopilot_role = role;
            } else {
                self.cars[i].role = role;
            }
        }
    }

    /// Car against car: those near each other along the road (sorted by
    /// their place, each against those within reach of it).
    fn collide(&mut self) {
        let n = self.cars.len();
        self.order.clear();
        self.order.extend(0..n);
        let places = &self.scene.places;
        self.order.sort_by(|&a, &b| places[a].s.total_cmp(&places[b].s));
        let length = self.scene.length;
        for (k, &i) in self.order.iter().enumerate() {
            for step in 1..n {
                let j = self.order[(k + step) % n];
                let gap = Euclid::rem_euclid(&(places[j].s - places[i].s), &length);
                if gap > 12.0 {
                    break;
                }
                let (lo, hi) = (i.min(j), i.max(j));
                let (a, b) = self.cars.split_at_mut(hi);
                if let Some(impact) = physics::collide_cars(&mut a[lo].vehicle, &mut b[0].vehicle)
                    && impact.speed > 0.5
                {
                    self.events.push(Event::Crash { a: a[lo].id, b: Some(b[0].id), impact });
                }
            }
        }
    }

    /// What to draw now: every car between its last two steps, and the
    /// lights that move with them (headlights, light bars); the chase
    /// camera, if it films, follows the player as drawn. `dt` is the time
    /// since the last frame (0 while frozen), which the camera moves by
    /// while the game waits (a countdown); while it runs, the camera moves
    /// on as far as the cars did, so that a stall the game does not catch
    /// up on moves it no further than them.
    pub fn frame(&mut self, dt: f32) -> (Vec<CarView>, Vec<Light>) {
        let moved = core::mem::take(&mut self.moved);
        self.frame_time = if self.paused { dt } else { moved };
        let t = (self.pending / STEP).clamp(0.0, 1.0);
        let clock = self.clock();
        let mut views = Vec::with_capacity(self.cars.len());
        let mut lights = Vec::new();
        let night = self.env.street_lights;
        for car in &self.cars {
            let pose = car.prev.lerp(&Pose::of(&car.vehicle), t);
            let m = pose.transform(car.vehicle.chassis.middle);
            // Each car's bar keeps its own time (by its number, whatever
            // comes and goes), so that a convoy does not flash as one.
            let bar = if car.siren { light_bar(clock + f64::from(car.id.0) * 0.37) } else { [0.0, 0.0] };
            let half = car.vehicle.chassis.half_length;
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
                transform: m,
                paint: car.paint,
                livery: car.livery,
                spin: pose.spin,
                steer: pose.steer,
                headlights: if car.headlights { 1.0 } else { 0.0 },
                brake: if car.vehicle.braking > 0.05 { 1.0 } else { 0.0 },
                bar,
            });
        }
        // The chase camera follows the player as drawn.
        if self.follow.is_some() {
            let seen = self.drawn(self.player);
            let hint = self.cars[self.player].hint;
            if let Some(follow) = &mut self.follow {
                self.camera = follow.update(&seen, &self.world, hint, self.frame_time);
            }
        }
        (views, lights)
    }

    /// Car `i`'s vehicle as drawn now (between its last two steps): what
    /// cameras film.
    pub fn drawn(&self, i: usize) -> Vehicle {
        let car = &self.cars[i];
        let mut v = car.vehicle.clone();
        car.prev.lerp(&Pose::of(&car.vehicle), (self.pending / STEP).clamp(0.0, 1.0)).apply(&mut v);
        v
    }

    /// Takes car `id` off the road (never the player's). What refers to it
    /// (a police car's target, this frame's crashes) finds it gone.
    pub fn remove_car(&mut self, id: CarId) {
        let Some(i) = self.index(id) else { return };
        if i == self.player {
            return;
        }
        self.cars.remove(i);
        if self.player > i {
            self.player -= 1;
        }
    }

    /// Lets the computer drive the player's car (or gives it back): as
    /// police after `target` if there is one, as traffic otherwise.
    pub fn set_autopilot(&mut self, on: bool, target: Option<CarId>) {
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

    /// How hard it rains where the camera is (not at all in a tunnel).
    pub fn rain_here(&self) -> f32 {
        if self.env.rain <= 0.0 {
            return 0.0;
        }
        let at = self.world.road.locate(self.camera.eye, Some(self.player().hint));
        self.env.rain * (1.0 - self.world.road.covered(at.s))
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
        self.respawn_player(s, 2.1, false, speed);
    }

    /// Puts the player's car back on the road where it is, in its lane,
    /// facing forward and standing still (after a spin or getting stuck).
    pub fn reset_player(&mut self) {
        let (s, lateral) = self.cars[self.player].place(&self.world);
        let backwards = lateral < 0.0;
        self.respawn_player(s, if backwards { -2.1 } else { 2.1 }, backwards, 0.0);
    }

    /// Puts the player's car at `(s, lateral)`, facing forward (or back),
    /// at `speed`: the same car, its driver (the player, or the computer),
    /// its looks and its damage kept; its pedals and steering from rest;
    /// the chase camera behind it.
    fn respawn_player(&mut self, s: f32, lateral: f32, backwards: bool, speed: f32) {
        let p = self.player;
        self.cars[p].put(&self.world, s, lateral, backwards, speed);
        self.driver.release();
        if let Some(follow) = &mut self.follow {
            let view = follow.view;
            *follow = Follow::new(&self.cars[p].vehicle);
            follow.view = view;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::{Police, Suspect};
    use crate::env::{TimeOfDay, Weather};

    fn game() -> Game {
        Game::new(World::new(), Environment::new(TimeOfDay::Day, Weather::Clear))
    }

    fn run(g: &mut Game, secs: f32) {
        for _ in 0..(secs / STEP) as usize {
            g.update(STEP);
        }
    }

    #[test]
    fn the_keys_drive_the_car_after_a_reset() {
        let mut g = game();
        g.place_player(400.0, 0.0);
        // Put back on the road, then driven: it goes forward, the player
        // at the wheel.
        g.reset_player();
        assert!(matches!(g.player().role, Role::Player));
        g.keys.up = true;
        run(&mut g, 2.0);
        let v = &g.player().vehicle;
        assert!(v.speed() > 8.0 && v.gear >= 1, "going {} m/s in gear {}", v.speed(), v.gear);
        // Let go and put back again: the same car stands where it was put.
        g.keys = Keys::default();
        let id = g.player().id;
        g.reset_player();
        assert_eq!(g.player().id, id);
        let at = g.player().vehicle.pos;
        run(&mut g, 2.0);
        assert!(g.player().vehicle.pos.distance(at) < 0.05, "it rolled {} m", g.player().vehicle.pos.distance(at));
    }

    #[test]
    fn the_clock_counts_steps_and_the_pictures_time_wraps_unseen() {
        let mut g = game();
        run(&mut g, 2.5);
        assert_eq!(g.clock(), g.steps() as f64 / 120.0);
        // Hours on, the light bars flash as they did, a whole number of
        // bursts later.
        let t = 70.0 * 3600.0;
        for k in 0..64 {
            // (Clear of the flashes' edges.)
            let dt = (f64::from(k) + 0.5) / 64.0;
            assert_eq!(light_bar(t + dt), light_bar(dt), "{dt}");
        }
        assert_eq!(crate::render::shader_time(crate::render::TIME_WRAP * 3.0 + 0.25), 0.25);
    }

    #[test]
    fn a_parked_car_stays_put() {
        let mut g = game();
        // Stopping from speed on its brakes, then held on them.
        let mut car = Car::on_road(&g.world, Kind::Saloon, 300.0, 5.7, false, 12.0);
        car.role = Role::Parked;
        let id = g.add_car(car);
        run(&mut g, 8.0);
        let i = g.index(id).unwrap();
        let at = g.cars[i].vehicle.pos;
        run(&mut g, 4.0);
        let v = &g.cars[i].vehicle;
        assert!(v.pos.distance(at) < 0.05 && v.gear >= 1, "a parked car moved {} m", v.pos.distance(at));
    }

    #[test]
    fn taking_a_car_off_leaves_the_others_and_what_refers_to_them_right() {
        let mut g = game();
        g.start_traffic();
        let me = g.player().id;
        // A suspect, a police car after it, and another car between them
        // in the list.
        let s = g.player().place(&g.world).0;
        let mut suspect = Car::on_road(&g.world, Kind::Coupe, s + 60.0, 2.1, false, 20.0);
        suspect.role = Role::Suspect(Suspect::new(2.1, 0.9, 0.3, me, 3));
        let suspect = g.add_car(suspect);
        let other = g.add_car(Car::on_road(&g.world, Kind::Van, s + 300.0, 5.7, false, 0.0));
        let mut unit = Car::on_road(&g.world, Kind::Police, s - 20.0, 5.7, false, 20.0);
        unit.role = Role::Police(Police::new(5.7, suspect, 1.0));
        let unit = g.add_car(unit);
        g.set_autopilot(true, Some(suspect));
        run(&mut g, 1.0);
        // A crash this frame naming the car taken off.
        let impact = Impact { point: Vec3::ZERO, normal: Vec2::X, speed: 6.0, impulse: 100.0, slide: 0.0 };
        g.events.push(Event::Crash { a: other, b: Some(unit), impact });
        let before = g.cars.len();
        g.remove_car(other);
        assert_eq!(g.cars.len(), before - 1);
        assert!(g.index(other).is_none() && g.car(other).is_none());
        // The others are where their numbers say.
        for id in [me, suspect, unit] {
            assert_eq!(g.car(id).map(|c| c.id), Some(id));
        }
        assert_eq!(g.player().id, me);
        // What follows the game hears of the crash, and finds a car gone.
        let mut sound = crate::sound::Sound::new();
        let mut effects = crate::effects::Effects::new();
        let _ = sound.update(&g, 1.0, 1.0 / 60.0);
        effects.update(&g, g.camera.eye, 1.0 / 60.0);
        // Taking the suspect off: those after it drive on, finding it gone.
        g.remove_car(suspect);
        for _ in 0..240 {
            g.update(STEP);
            let _ = sound.update(&g, 1.0, STEP);
        }
        assert!(g.car(unit).is_some() && g.cars.iter().all(|c| c.vehicle.pos.is_finite()));
        // New cars never take a number that was given.
        let new = g.add_car(Car::on_road(&g.world, Kind::Saloon, s + 500.0, 2.1, false, 0.0));
        assert!(![me, suspect, other, unit].contains(&new));
    }
}
