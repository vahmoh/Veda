// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The computer's drivers.
//!
//! Each drives a car with the same controls as the player, through a
//! [`Pilot`]: it steers for a point ahead on the line it wants to follow
//! (pure pursuit), and holds the speed it wants, braking in time for the
//! corners ahead. What the line and the speed are depends on who drives:
//!
//! * **Traffic** keeps to its lane at about the limit, follows the car
//!   ahead at a safe distance (the Intelligent Driver Model), overtakes
//!   slow cars when the next lane is clear, and pulls over to let a siren
//!   by.
//! * **A suspect** drives at the edge of its car's grip, picks whichever
//!   lane is clearest (the other carriageway too, where nothing divides
//!   it), swerves to block a police car about to pass, and slows as it is
//!   damaged.
//! * **The police** chase their target: they aim where it will be, and
//!   once alongside its rear quarter they turn into it (a PIT).
//!
//! Drivers see the road through [`Scene`]: every car's place on it (its
//! body's middle), in the game's order of its cars, which a driver looks
//! through for those near its line.

use alloc::vec::Vec;

use crate::game::CarId;
use crate::prelude::*;
use crate::road::{Road, Section};
use crate::vehicle::{Controls, G, Vehicle};

/// Where a car is on the road, as drivers see it.
#[derive(Clone, Copy, Debug)]
pub struct Place {
    /// Which car it is.
    pub id: CarId,
    pub s: f32,
    pub lateral: f32,
    /// Speed along the road (negative against it).
    pub along: f32,
    /// The way it means to go: 1 along the road, -1 against it.
    pub dir: f32,
    pub length: f32,
    /// Its siren is on.
    pub siren: bool,
}

/// Every car's place, for the drivers: car `i`'s is `places[i]`.
pub struct Scene {
    pub places: Vec<Place>,
    /// The loop's length (m).
    pub length: f32,
}

impl Scene {
    /// Where car `id`'s place is in `places`, if it is on the road.
    pub fn find(&self, id: CarId) -> Option<usize> {
        self.places.iter().position(|p| p.id == id)
    }

    /// The signed distance along the road from `a` to `b`, the short way.
    pub fn gap(&self, a: f32, b: f32) -> f32 {
        let d = Euclid::rem_euclid(&(b - a), &self.length);
        if d > self.length * 0.5 { d - self.length } else { d }
    }

    /// The nearest car ahead of car `me` (in its direction of travel)
    /// within `range` metres whose lateral offset is within `half` of
    /// `lateral`: its index and the gap (bumper to bumper).
    pub fn ahead(&self, me: usize, lateral: f32, half: f32, range: f32) -> Option<(usize, f32)> {
        self.ahead_but(me, lateral, half, range, None)
    }

    /// As [`Scene::ahead`], not counting car `but` (the car being chased).
    pub fn ahead_but(
        &self,
        me: usize,
        lateral: f32,
        half: f32,
        range: f32,
        but: Option<usize>,
    ) -> Option<(usize, f32)> {
        let m = &self.places[me];
        let mut best: Option<(usize, f32)> = None;
        for (j, o) in self.places.iter().enumerate() {
            if j == me || Some(j) == but || (o.lateral - lateral).abs() > half {
                continue;
            }
            let d = self.gap(m.s, o.s) * m.dir - (m.length + o.length) * 0.5;
            if d > -1.0 && d < range && best.is_none_or(|b| d < b.1) {
                best = Some((j, d.max(0.0)));
            }
        }
        best
    }
}

/// The line among `candidates` (lateral offsets, each with a price) with
/// the most room ahead of car `me`, judged by the first car in it but
/// `but`: one going the same way can be followed, one coming the other way
/// must be far off; moving across costs, and a line must be clearly better
/// than `current` to be worth leaving it.
fn roomiest(me: usize, scene: &Scene, candidates: &[(f32, f32)], current: f32, but: Option<usize>) -> f32 {
    let p = &scene.places[me];
    let mut best = (current, f32::MIN);
    for &(lat, price) in candidates {
        let room = match scene.ahead_but(me, lat, 2.0, 220.0, but) {
            Some((j, gap)) => {
                let theirs = scene.places[j].along * p.dir;
                if theirs > 1.0 {
                    // Going its way: as good as the speed it can keep
                    // behind it, and the room to it.
                    110.0 + (theirs / (p.along * p.dir).max(10.0)).min(1.0) * 80.0 + gap.min(150.0) * 0.2
                } else {
                    // Coming at it (or standing): as good as the time
                    // before they meet; within four seconds, no line at all.
                    let t = gap / ((p.along * p.dir).max(0.0) - theirs).max(1.0);
                    if t < 4.0 { t * 15.0 - 80.0 } else { t.min(10.0) * 22.0 }
                }
            }
            None => 230.0,
        };
        let change = (lat - current).abs() * 2.0 + if (lat - current).abs() > 0.5 { 25.0 } else { 0.0 };
        let score = room - price - change;
        if score > best.1 {
            best = (lat, score);
        }
    }
    best.0
}

/// Whether a car but `but` is coming at `me` along `lateral` (or `me` is
/// closing on one) to meet it within `secs` seconds: time to choose
/// another line.
fn meeting(me: usize, scene: &Scene, lateral: f32, secs: f32, but: Option<usize>) -> bool {
    let p = &scene.places[me];
    scene.ahead_but(me, lateral, 2.0, 140.0, but).is_some_and(|(j, gap)| {
        let closing = (p.along - scene.places[j].along) * p.dir;
        closing > 3.0 && gap < closing * secs
    })
}

/// The fastest car `me` may go along `lateral` and still stop behind the
/// first car in it but `but` (braking at `decel`, m/s²), `margin` metres
/// short of it.
fn safe_speed(me: usize, scene: &Scene, lateral: f32, decel: f32, margin: f32, but: Option<usize>) -> f32 {
    let p = &scene.places[me];
    match scene.ahead_but(me, lateral, 1.9, 160.0, but) {
        Some((j, gap)) => {
            let theirs = (scene.places[j].along * p.dir).max(0.0);
            theirs + (2.0 * decel * (gap - margin).max(0.0)).sqrt()
        }
        None => f32::MAX,
    }
}

/// Steering and pedals for following a line at a speed.
#[derive(Clone, Copy, Debug, Default)]
pub struct Pilot {
    /// The lateral offset being steered for (eased towards the wanted).
    pub lateral: f32,
    /// How long it has been stuck (s), and reversing out of it.
    stuck: f32,
    reversing: f32,
}

impl Pilot {
    pub fn new(lateral: f32) -> Pilot {
        Pilot { lateral, stuck: 0.0, reversing: 0.0 }
    }

    /// The controls that take `car` (at `place`) along the road in its
    /// direction (`place.dir`) towards `want_lateral` at `want_speed`
    /// (m/s), its tyres trusted up to `grip` (m/s²), over `dt` seconds.
    /// `lane_rate` is how fast it moves across (m/s).
    pub fn drive(
        &mut self,
        road: &Road,
        car: &Vehicle,
        place: &Place,
        want_lateral: f32,
        want_speed: f32,
        grip: f32,
        lane_rate: f32,
        dt: f32,
    ) -> Controls {
        let step = lane_rate * dt;
        self.lateral += (want_lateral - self.lateral).clamp(-step, step);
        let speed = car.speed();
        let dir = place.dir;

        // Reversing out of a corner it got stuck in.
        if self.reversing > 0.0 {
            self.reversing -= dt;
            let to = road.point(place.s + dir * 10.0, self.lateral) - car.pos;
            let ang = libm::atan2f(to.xz().dot(car.left()), to.xz().dot(car.forward()));
            return Controls {
                throttle: 0.7,
                steer: (-ang * 2.0).clamp(-1.0, 1.0),
                reverse: true,
                ..Controls::default()
            };
        }

        // Steer for a point ahead on the line.
        let look = 7.0 + speed.abs() * 0.42;
        let target = road.point(place.s + dir * look, self.lateral);
        let to = Vec2::new(target.x - car.pos.x, target.z - car.pos.z);
        let alpha = libm::atan2f(to.dot(car.left()), to.dot(car.forward()));
        let wheel = libm::atanf(2.0 * car.chassis.wheelbase * libm::sinf(alpha) / to.length().max(1.0));
        // Never sharper than the tyres can follow at this speed (a mildly
        // understeering car's angle for all they grip, whatever the driver
        // plans on): asking more only slides it wide or spins it.
        let v2 = (speed * speed).max(1.0);
        let tyres = car.spec.grip * G;
        let most = 1.05 * tyres * (car.chassis.wheelbase / v2 + 0.02 / G);
        let wheel = wheel.clamp(-most, most);
        let lock = car.spec.steer_max / (1.0 + speed.abs() * 0.055);
        let steer = (wheel / lock.max(0.05)).clamp(-1.0, 1.0);

        // The speed the corners ahead allow, given the braking to get
        // down to it: as far ahead as it takes to stop from this speed.
        let decel = grip * 0.75;
        let brake_dist = speed * speed / (2.0 * decel) + 30.0;
        let mut allowed = want_speed;
        let probe = (brake_dist.min(400.0) / 6.0).ceil() as usize;
        for k in 1..=probe {
            let d = k as f32 * 6.0;
            let curv = road.curvature(place.s + dir * d).abs().max(1e-4);
            let v_corner = (grip / curv).sqrt();
            let v = (v_corner * v_corner + 2.0 * decel * (d - 8.0).max(0.0)).sqrt();
            allowed = allowed.min(v);
        }
        let err = allowed - speed;
        let mut c = Controls { steer, ..Controls::default() };
        if err > 0.0 {
            c.throttle = (err * 0.35).clamp(0.0, 1.0);
        } else {
            c.brake = (-err * 0.18).clamp(0.0, 1.0);
        }

        // Stuck against something: wanting to go and not going.
        if want_speed > 3.0 && speed.abs() < 1.0 {
            self.stuck += dt;
            if self.stuck > 1.5 {
                self.stuck = 0.0;
                self.reversing = 1.4;
            }
        } else {
            self.stuck = 0.0;
        }
        c
    }
}

/// Whether a barrier divides the road at `s` or within `ahead` metres in
/// direction `dir`: a driver on the other side by then would be trapped
/// on the wrong carriageway.
pub fn divided_soon(road: &Road, s: f32, dir: f32, ahead: f32) -> bool {
    let ahead = ahead.max(0.0);
    let mut d = 0.0;
    while d <= ahead {
        if road.divided(s + dir * d) {
            return true;
        }
        d += 10.0;
    }
    false
}

/// The lateral offsets a driver heading `dir` at `place` may aim for:
/// its own carriageway where a barrier divides the road here or within
/// four seconds; but if a barrier already stands between it and its own
/// side, the carriageway it is trapped on.
fn allowed(road: &Road, place: &Place, paved: f32) -> (f32, f32) {
    // A car's half width and a little: all its wheels on the tarmac.
    let edge = paved - 1.25;
    let soon = divided_soon(road, place.s, place.dir, 60.0 + place.along.abs() * 4.0);
    if !soon {
        return (-edge, edge);
    }
    let own = place.lateral * place.dir > 0.0;
    if road.divided(place.s) && !own {
        // Trapped: drive the other carriageway until it opens.
        if place.dir > 0.0 { (-edge, -1.2) } else { (1.2, edge) }
    } else if place.dir > 0.0 {
        (1.2, edge)
    } else {
        (-edge, -1.2)
    }
}

/// How the computer drives a car.
#[derive(Clone, Copy, Debug)]
pub enum Role {
    /// Driven by the player.
    Player,
    /// Ordinary traffic.
    Traffic(Traffic),
    /// On the run.
    Suspect(Suspect),
    /// Chasing a suspect.
    Police(Police),
    /// Standing still on its brakes (a roadblock, a wreck, a suspect
    /// caught).
    Parked,
}

/// A traffic driver.
#[derive(Clone, Copy, Debug)]
pub struct Traffic {
    pub pilot: Pilot,
    /// 0 the inner lane, 1 the outer.
    pub lane: usize,
    /// 1 on the forward side (along the road), -1 the other way.
    pub side: f32,
    /// The speed it likes, as a share of the limit.
    pub keen: f32,
    /// Pulled over for a siren (s left to wait once it has passed).
    pub yielding: f32,
    /// Shaken by a crash: stopped for this long (s).
    pub shaken: f32,
    /// Seconds before it may change lanes again.
    pub settle: f32,
}

impl Traffic {
    pub fn new(lane: usize, side: f32, keen: f32) -> Traffic {
        Traffic {
            pilot: Pilot::new(Road::lane_center(lane, side)),
            lane,
            side,
            keen,
            yielding: 0.0,
            shaken: 0.0,
            settle: 2.0,
        }
    }

    /// The controls for car `me` this step.
    pub fn drive(&mut self, me: usize, road: &Road, scene: &Scene, car: &Vehicle, dt: f32) -> Controls {
        let p = &scene.places[me];
        let section = road.at(p.s).section;
        let side = self.side;
        self.settle = (self.settle - dt).max(0.0);
        if self.shaken > 0.0 {
            self.shaken -= dt;
            return Controls { brake: 1.0, ..Controls::default() };
        }
        let limit = section.speed_limit() * self.keen;
        // A siren behind, coming: over to the shoulder, slow.
        let siren_behind = scene.places.iter().enumerate().any(|(j, o)| {
            j != me && o.siren && o.dir == p.dir && {
                let d = scene.gap(p.s, o.s) * p.dir;
                d < -2.0 && d > -110.0
            }
        });
        if siren_behind {
            self.yielding = 1.5;
        } else {
            self.yielding = (self.yielding - dt).max(0.0);
        }
        let (want_lateral, mut want_speed) = if self.yielding > 0.0 {
            let (paved, _) = road.widths(p.s);
            (side * (paved - 1.3), limit * 0.35)
        } else {
            (Road::lane_center(self.lane, side), limit)
        };
        // Following the car ahead (the Intelligent Driver Model).
        if let Some((j, gap)) = scene.ahead(me, self.pilot.lateral, 1.9, 140.0) {
            let o = &scene.places[j];
            let v = p.along * p.dir;
            let lead = o.along * p.dir;
            let (t, s0, a, b) = (1.3f32, 3.5f32, 2.0f32, 3.0f32);
            let s_star = s0 + v.max(0.0) * t + v.max(0.0) * (v - lead) / (2.0 * (a * b).sqrt());
            let free = 1.0 - (v / want_speed.max(1.0)).powi(4);
            let acc = a * (free - (s_star.max(0.0) / gap.max(0.5)).powi(2));
            // As a speed to aim at a moment from now.
            want_speed = want_speed.min((v + acc * 1.2).max(0.0));
            // Overtake on the other lane of this side when it is clear.
            if self.yielding <= 0.0 && self.settle <= 0.0 && lead < limit * 0.75 && gap < 45.0 {
                let other = 1 - self.lane;
                let lat = Road::lane_center(other, side);
                let clear = scene.places.iter().enumerate().all(|(k, q)| {
                    k == me || (q.lateral - lat).abs() > 2.2 || {
                        let d = scene.gap(p.s, q.s) * p.dir;
                        !(-14.0..30.0).contains(&d)
                    }
                });
                if clear {
                    self.lane = other;
                    self.settle = 6.0;
                }
            }
        }
        // Slow down into the tunnel and on the bridge's approach, as people
        // do; keep a little under the limit in the city.
        if section == Section::City {
            want_speed = want_speed.min(limit * 1.05);
        }
        self.pilot.drive(road, car, p, want_lateral, want_speed, 6.5, 1.4, dt)
    }
}

/// A driver on the run.
#[derive(Clone, Copy, Debug)]
pub struct Suspect {
    pub pilot: Pilot,
    /// How hard it drives (0.8 cautious .. 1.0 at the limit).
    pub skill: f32,
    /// How readily it swerves at the police (0..1).
    pub aggression: f32,
    /// The car it runs from.
    pub pursuer: CarId,
    /// The lateral it has chosen, and when it next chooses.
    choice: f32,
    rethink: f32,
    /// A swerve under way (s left), and towards which offset.
    swerve: f32,
    swerve_to: f32,
    /// Its own dice.
    rng: u32,
}

impl Suspect {
    pub fn new(lateral: f32, skill: f32, aggression: f32, pursuer: CarId, seed: u32) -> Suspect {
        Suspect {
            pilot: Pilot::new(lateral),
            skill,
            aggression,
            pursuer,
            choice: lateral,
            rethink: 0.0,
            swerve: 0.0,
            swerve_to: 0.0,
            rng: seed | 1,
        }
    }

    /// A number in 0..1.
    fn roll(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        (self.rng >> 8) as f32 / (1 << 24) as f32
    }

    /// The controls for car `me` this step; `damage` slows it; `pace` (the
    /// pursuit's rubber band, about 0.85..1.05) scales its speed.
    pub fn drive(
        &mut self,
        me: usize,
        road: &Road,
        scene: &Scene,
        car: &Vehicle,
        damage: f32,
        pace: f32,
        dt: f32,
    ) -> Controls {
        let p = scene.places[me];
        let (paved, _) = road.widths(p.s);
        // A barrier down the middle here or soon: keep to this side.
        let divided = divided_soon(road, p.s, p.dir, 60.0 + p.along.abs() * 4.0);
        self.rethink -= dt;
        if self.rethink <= 0.0 || meeting(me, scene, self.choice, 1.6, None) || meeting(me, scene, p.lateral, 1.6, None)
        {
            self.rethink = 0.25;
            self.choice = self.best_lateral(me, scene, paved, divided);
        }
        let mut want = self.choice;
        // The pursuer about to pass or ram: swerve across its path.
        if let Some(them) = scene.find(self.pursuer).map(|j| scene.places[j]) {
            let behind = scene.gap(them.s, p.s) * p.dir;
            if self.swerve <= 0.0 && behind > 2.0 && behind < 14.0 && (them.lateral - p.lateral).abs() > 1.0 {
                // Now and then, as the driver dares (about once a second at
                // full aggression).
                if self.roll() < self.aggression * dt * 1.2 {
                    self.swerve = 0.9;
                    self.swerve_to = them.lateral;
                }
            }
        }
        if self.swerve > 0.0 {
            self.swerve -= dt;
            want = self.swerve_to;
        }
        let (lo, hi) = allowed(road, &p, paved);
        let want = want.clamp(lo, hi);
        // Flat out but for the pursuit's pace: the corners and the damage
        // hold it back.
        // Boxed in, it brakes rather than ploughing on (the bolder, the
        // later).
        let (decel, margin) = (9.0 + 3.0 * self.aggression, 6.0 - 3.0 * self.aggression);
        let room =
            safe_speed(me, scene, want, decel, margin, None).min(safe_speed(me, scene, p.lateral, decel, margin, None));
        let speed = (80.0 * pace * (1.0 - 0.45 * damage)).min(room);
        // What the tyres give in a corner, less what the load's shift
        // takes, as the driver dares (and as the pace lets it).
        let grip = car.spec.grip * G * 0.8 * self.skill * pace;
        self.pilot.drive(road, car, &p, want, speed, grip, 3.8, dt)
    }

    /// The lateral offset with the most room ahead: each lane of its side
    /// (and of the other where nothing divides them, with a price for the
    /// oncoming traffic), and the shoulders.
    fn best_lateral(&self, me: usize, scene: &Scene, paved: f32, divided: bool) -> f32 {
        let side = scene.places[me].dir;
        let mut candidates = Vec::with_capacity(6);
        for lane in 0..2 {
            candidates.push((Road::lane_center(lane, side), 0.0));
            if !divided {
                candidates.push((Road::lane_center(lane, -side), 18.0 + lane as f32 * 8.0));
            }
        }
        candidates.push((side * (paved - 1.3), 10.0));
        roomiest(me, scene, &candidates, self.choice, None)
    }
}

/// A police driver.
#[derive(Clone, Copy, Debug)]
pub struct Police {
    pub pilot: Pilot,
    /// The car it chases.
    pub target: CarId,
    pub skill: f32,
    /// Following this far behind rather than ramming (m): a chase filmed
    /// for the menus, kept going.
    pub follow: Option<f32>,
    /// The line it has chosen through the traffic, and when it next
    /// chooses.
    choice: f32,
    rethink: f32,
}

impl Police {
    pub fn new(lateral: f32, target: CarId, skill: f32) -> Police {
        Police { pilot: Pilot::new(lateral), target, skill, follow: None, choice: lateral, rethink: 0.0 }
    }

    /// The controls for car `me` this step (its target gone, it drives
    /// on in the inner lane at the limit).
    pub fn drive(&mut self, me: usize, road: &Road, scene: &Scene, car: &Vehicle, dt: f32) -> Controls {
        let p = scene.places[me];
        let (paved, _) = road.widths(p.s);
        let grip = car.spec.grip * G * 0.85 * self.skill;
        let Some(target) = scene.find(self.target) else {
            let want = Road::lane_center(0, p.dir);
            let speed = road.at(p.s).section.speed_limit().min(safe_speed(me, scene, want, 10.0, 6.0, None));
            return self.pilot.drive(road, car, &p, want, speed, grip, 4.5, dt);
        };
        let t = scene.places[target];
        let gap = scene.gap(p.s, t.s) * p.dir;
        let close = gap.abs() < 30.0;
        // Far behind, its own fastest line: towards the inside of the bend
        // ahead (on its own side where the road is divided).
        let mut want = {
            let curv = road.curvature(p.s + 25.0 + p.along.max(0.0) * 0.8);
            let into = ((curv.abs() - 0.003) / 0.009).clamp(0.0, 1.0);
            let inside = curv.signum() * (paved - 2.0) * into;
            let base = Road::lane_center(0, 1.0);
            base + (inside - base) * into
        };
        let mut speed = 76.0 * self.skill;
        if gap.abs() < 60.0 {
            // Nearer, on its tail.
            want = t.lateral;
        }
        if let Some(d) = self.follow {
            // On its tail at a distance, its speed matched.
            if gap < d + 40.0 {
                want = t.lateral;
                speed = (t.along * p.dir + (gap - d) * 0.6).max(0.0);
            }
        } else if close {
            if gap > 4.0 {
                // Line up with its rear quarter, a little to the side, then
                // close in: the nose against its quarter turns it round.
                let offset = if p.lateral > t.lateral { 1.4 } else { -1.4 };
                want = t.lateral + offset * (gap / 12.0).clamp(0.3, 1.0);
                speed = (t.along * p.dir + 6.0).max(10.0);
            } else if gap > -3.0 {
                // Alongside its rear: turn into it.
                want = t.lateral;
                speed = t.along * p.dir + 3.0;
            } else {
                // Ahead of it: hold back.
                speed = (t.along * p.dir - 4.0).max(5.0);
            }
        }
        // Through the traffic: its line while that is clear of all but the
        // suspect, else the one with the most room (the siren clears the
        // way, mostly); never faster than it can stop behind a car in it.
        let target = Some(target);
        self.rethink -= dt;
        let danger = |lat: f32, secs: f32| meeting(me, scene, lat, secs, target);
        if self.rethink <= 0.0 || danger(self.choice, 2.0) || danger(p.lateral, 2.0) {
            self.rethink = 0.25;
            let divided = divided_soon(road, p.s, p.dir, 60.0 + p.along.abs() * 4.0);
            let mut candidates = Vec::with_capacity(6);
            candidates.push((want, 0.0));
            for lane in 0..2 {
                candidates.push((Road::lane_center(lane, p.dir), 6.0));
                if !divided {
                    candidates.push((Road::lane_center(lane, -p.dir), 30.0 + lane as f32 * 10.0));
                }
            }
            self.choice = roomiest(me, scene, &candidates, self.choice, target);
        }
        let ramming = close && self.follow.is_none();
        if !ramming || danger(want, 2.5) || danger(p.lateral, 2.5) {
            want = self.choice;
        }
        // Moving across, it is on both lines.
        for lat in [want, p.lateral] {
            speed = speed.min(safe_speed(me, scene, lat, 10.0, 6.0, target));
        }
        let (lo, hi) = allowed(road, &p, paved);
        let want = want.clamp(lo, hi);
        self.pilot.drive(road, car, &p, want, speed, grip, 4.5, dt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::carmodel::Kind;
    use crate::game::CarId;
    use crate::vehicle::Ground;
    use crate::world::World;

    #[test]
    fn a_driver_stuck_backs_out() {
        let world = World::new();
        let road = &world.road;
        let s = 400.0;
        let (p, f, r, _) = road.frame(s);
        let mut car = Vehicle::new(Kind::Saloon, p + r * 2.1, libm::atan2f(f.x, f.z));
        let place = Place { id: CarId(1), s, lateral: 2.1, along: 0.0, dir: 1.0, length: 4.6, siren: false };
        let mut pilot = Pilot::new(2.1);
        let dt = 1.0 / 120.0;
        // Wanting to go and going nowhere (held against something), it
        // backs out: in reverse, on the throttle.
        let mut c = Controls::default();
        for _ in 0..240 {
            c = pilot.drive(road, &car, &place, 2.1, 10.0, 6.5, 1.4, dt);
            if c.reverse {
                break;
            }
        }
        assert!(c.reverse && c.throttle > 0.5 && c.brake == 0.0, "{c:?}");
        let flat = |_: Vec3| Ground { height: 0.0, normal: Vec3::Y, grip: 1.0 };
        for _ in 0..120 {
            let c = pilot.drive(road, &car, &place, 2.1, 10.0, 6.5, 1.4, dt);
            car.step(&c, dt, &flat);
        }
        assert!(car.speed() < -0.5 && car.gear == -1, "going {} m/s in gear {}", car.speed(), car.gear);
    }
}
