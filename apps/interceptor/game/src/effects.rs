//! What passes: tyre smoke where wheels slide, spray thrown up from a wet
//! road, sparks where metal meets metal or a wall, and the skid marks
//! sliding tyres leave on the road (fading over a minute). Made here from
//! the cars and their crashes, so that it is tested on the host; drawn by
//! the renderer.
//!
//! Skid marks are kept in a ring: the oldest give way to the newest, and
//! the renderer copies only the new ones to the GPU.

use alloc::vec::Vec;

use rand::{RngExt, SeedableRng};
use rand_distr::UnitBall;
use rand_pcg::Pcg32;

use crate::game::{Event, Game};
use crate::prelude::*;

/// The most particles at once, and skid marks' segments kept.
pub const MAX_PARTICLES: usize = 2048;
pub const MAX_SKIDS: usize = 4096;
/// How long a skid mark lasts (s).
pub const SKID_LIFE: f32 = 75.0;
/// Further from the camera than this (m), nothing is made.
const NEAR: f32 = 160.0;
/// A wheel sliding faster than this (m/s) marks the road; faster than
/// `SMOKES`, it smokes.
const MARKS: f32 = 2.5;
const SMOKES: f32 = 5.0;
/// How far apart a mark's points are (m).
const MARK_STEP: f32 = 0.7;

/// What a particle is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Tyre smoke: grows, drifts up, thins.
    Smoke,
    /// Spray off a wet road: quicker, lighter.
    Spray,
    /// A spark: falls, bounces, burns out.
    Spark,
}

/// A particle.
#[derive(Clone, Copy, Debug)]
pub struct Particle {
    pub kind: Kind,
    pub pos: Vec3,
    pub vel: Vec3,
    /// Seconds since it was made, and how long it lasts.
    pub age: f32,
    pub life: f32,
    /// Its size (m) as made.
    pub size: f32,
    /// Where it stops falling (sparks).
    floor: f32,
}

impl Particle {
    /// How much of its life is gone (0..1).
    pub fn spent(&self) -> f32 {
        (self.age / self.life).min(1.0)
    }

    /// Where it is drawn: a puff of smoke or spray kept above the road
    /// by most of its size (a puff cut by the road shows a hard edge).
    pub fn center(&self) -> Vec3 {
        match self.kind {
            Kind::Smoke | Kind::Spray => {
                Vec3::new(self.pos.x, self.pos.y.max(self.floor + self.size_now() * 0.7), self.pos.z)
            }
            Kind::Spark => self.pos,
        }
    }

    /// Its size now (m).
    pub fn size_now(&self) -> f32 {
        match self.kind {
            Kind::Smoke => self.size + self.age * 1.7,
            Kind::Spray => self.size + self.age * 2.4,
            Kind::Spark => self.size,
        }
    }
}

/// A segment of a skid mark: from `a` to `b` on the road, made at `born`
/// (the game's clock), as dark as `dark` (0..1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Skid {
    pub a: Vec3,
    pub b: Vec3,
    pub born: f32,
    pub dark: f32,
}

/// A car's wheels' last marks, by its id.
struct Trail {
    car: u32,
    last: [Option<Vec3>; 4],
    seen: bool,
}

/// The passing effects.
pub struct Effects {
    pub particles: Vec<Particle>,
    skids: Vec<Skid>,
    /// Segments made so far (the next goes at `made % MAX_SKIDS`).
    made: u64,
    trails: Vec<Trail>,
    rng: Pcg32,
}

impl Default for Effects {
    fn default() -> Effects {
        Effects::new()
    }
}

impl Effects {
    pub fn new() -> Effects {
        Effects {
            particles: Vec::with_capacity(MAX_PARTICLES),
            skids: Vec::with_capacity(MAX_SKIDS),
            made: 0,
            trails: Vec::new(),
            rng: Pcg32::seed_from_u64(0xEFFEC7),
        }
    }

    /// The skid marks' ring, and how many segments were ever made.
    pub fn skids(&self) -> (&[Skid], u64) {
        (&self.skids, self.made)
    }

    fn add_skid(&mut self, s: Skid) {
        let at = (self.made % MAX_SKIDS as u64) as usize;
        if at < self.skids.len() {
            self.skids[at] = s;
        } else {
            self.skids.push(s);
        }
        self.made += 1;
    }

    fn add(&mut self, p: Particle) {
        if self.particles.len() >= MAX_PARTICLES {
            // The oldest makes way.
            let oldest = (0..self.particles.len())
                .max_by(|&a, &b| self.particles[a].spent().total_cmp(&self.particles[b].spent()));
            if let Some(i) = oldest {
                self.particles.swap_remove(i);
            }
        }
        self.particles.push(p);
    }

    /// A random vector in the unit ball.
    fn jitter(&mut self) -> Vec3 {
        Vec3::from(self.rng.sample::<[f32; 3], _>(UnitBall))
    }

    /// Follows `game` through the `dt` seconds its last update took, seen
    /// from `eye`: what its sliding wheels and crashes made, and what was
    /// made before drifting, falling and fading.
    pub fn update(&mut self, game: &Game, eye: Vec3, dt: f32) {
        if dt <= 0.0 {
            return;
        }
        let wet = game.env.wetness;
        for t in &mut self.trails {
            t.seen = false;
        }
        for car in &game.cars {
            let v = &car.vehicle;
            if v.pos.distance(eye) > NEAR {
                continue;
            }
            let ti = match self.trails.iter().position(|t| t.car == car.id) {
                Some(i) => i,
                None => {
                    self.trails.push(Trail { car: car.id, last: [None; 4], seen: false });
                    self.trails.len() - 1
                }
            };
            self.trails[ti].seen = true;
            let speed = v.vel.length();
            let vel = Vec3::new(v.vel.x, v.vy, v.vel.y);
            for (w, at) in v.wheel_points().into_iter().enumerate() {
                let ground = game.world.ground(Vec3::new(at.x, v.pos.y, at.y), car.hint);
                let p = Vec3::new(at.x, ground.height + 0.02, at.y);
                let slip = if v.airborne { 0.0 } else { v.slip[w] };
                // The marks: a segment every so often while it slides.
                if slip > MARKS {
                    let dark = ((slip - MARKS) / 6.0).clamp(0.15, 0.85) * (1.0 - 0.5 * wet);
                    match self.trails[ti].last[w] {
                        Some(last) if last.distance(p) > 4.0 => self.trails[ti].last[w] = Some(p),
                        Some(last) if last.distance(p) >= MARK_STEP => {
                            self.add_skid(Skid { a: last, b: p, born: game.clock, dark });
                            self.trails[ti].last[w] = Some(p);
                        }
                        Some(_) => {}
                        None => self.trails[ti].last[w] = Some(p),
                    }
                } else {
                    self.trails[ti].last[w] = None;
                }
                // Smoke from a wheel sliding hard (less off a wet road).
                if slip > SMOKES {
                    let rate = ((slip - SMOKES) * 5.0).min(36.0) * (1.0 - 0.7 * wet);
                    for _ in 0..self.count(rate * dt) {
                        let j = self.jitter();
                        let life = 1.6 + self.rng.random::<f32>() * 0.9;
                        self.add(Particle {
                            kind: Kind::Smoke,
                            pos: p + Vec3::Y * 0.25 + j * 0.2,
                            vel: vel * 0.25 + Vec3::new(j.x * 0.8, 0.6 + j.y.abs() * 0.6, j.z * 0.8),
                            age: 0.0,
                            life,
                            size: 0.45,
                            floor: p.y,
                        });
                    }
                }
                // Spray behind the rear wheels on a wet road at speed.
                if wet > 0.0 && w >= 2 && speed > 12.0 && !v.airborne {
                    let rate = ((speed - 12.0) * 1.2).min(30.0) * wet;
                    for _ in 0..self.count(rate * dt) {
                        let j = self.jitter();
                        let life = 0.5 + self.rng.random::<f32>() * 0.4;
                        self.add(Particle {
                            kind: Kind::Spray,
                            pos: p + Vec3::Y * 0.2 + j * 0.15,
                            vel: vel * 0.55 + Vec3::new(j.x * 1.2, 0.8 + j.y.abs(), j.z * 1.2),
                            age: 0.0,
                            life,
                            size: 0.35,
                            floor: p.y,
                        });
                    }
                }
            }
        }
        self.trails.retain(|t| t.seen);
        // Sparks where cars hit each other or a wall hard, or scrape.
        for e in &game.events {
            let Event::Crash { a, impact, .. } = *e;
            let fierce = impact.speed + impact.slide * 0.5;
            if fierce < 5.0 || impact.point.distance(eye) > NEAR {
                continue;
            }
            let car = &game.cars[a].vehicle;
            let vel = Vec3::new(car.vel.x, 0.0, car.vel.y);
            let away = Vec3::new(impact.normal.x, 0.0, impact.normal.y);
            let n = (fierce * 1.2).clamp(4.0, 36.0) as usize;
            for _ in 0..n {
                let j = self.jitter();
                let kick = 3.0 + self.rng.random::<f32>() * 8.0;
                let up = 0.3 + self.rng.random::<f32>() * 0.4;
                let life = 0.25 + self.rng.random::<f32>() * 0.45;
                self.add(Particle {
                    kind: Kind::Spark,
                    pos: impact.point + Vec3::Y * up,
                    vel: vel * 0.6 + (away * 0.6 + j).normalize_or(away) * kick + Vec3::Y * (1.5 + kick * 0.3),
                    age: 0.0,
                    life,
                    size: 0.07,
                    floor: impact.point.y + 0.03,
                });
            }
        }
        // On they go.
        for p in &mut self.particles {
            p.age += dt;
            match p.kind {
                Kind::Smoke | Kind::Spray => {
                    // Slowed by the air, the smoke rising, the spray falling.
                    p.vel *= (-dt * if p.kind == Kind::Smoke { 1.6 } else { 2.5 }).exp();
                    p.vel.y += dt * if p.kind == Kind::Smoke { 0.35 } else { -1.5 };
                }
                Kind::Spark => {
                    p.vel.y -= 9.81 * dt;
                    if p.pos.y < p.floor && p.vel.y < 0.0 {
                        p.vel.y *= -0.35;
                        p.vel.x *= 0.6;
                        p.vel.z *= 0.6;
                    }
                }
            }
            p.pos += p.vel * dt;
        }
        self.particles.retain(|p| p.age < p.life);
    }

    /// How many to make for `n` expected (the fraction by chance).
    fn count(&mut self, n: f32) -> usize {
        let whole = n.floor();
        whole as usize + usize::from(self.rng.random::<f32>() < n - whole)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::carmodel::Kind as CarKind;
    use crate::driver::Role;
    use crate::env::{Environment, TimeOfDay, Weather};
    use crate::game::{Car, STEP};
    use crate::vehicle::Spec;
    use crate::world::World;

    #[test]
    fn a_sliding_car_smokes_and_marks_the_road() {
        let mut g = Game::new(World::new(), Environment::new(TimeOfDay::Day, Weather::Clear));
        let p = g.player;
        g.cars[p] = Car::on_road(&g.world, CarKind::Police, Spec::interceptor(), 300.0, 2.1, false, 25.0);
        g.cars[p].role = Role::Player;
        let mut fx = Effects::new();
        // Handbrake and steering: the tail slides.
        g.keys.handbrake = true;
        g.keys.left = true;
        for _ in 0..60 {
            g.update(STEP * 2.0);
            g.frame(STEP * 2.0);
            fx.update(&g, g.camera.eye, STEP * 2.0);
        }
        let (skids, made) = fx.skids();
        let smoke = fx.particles.iter().filter(|p| p.kind == Kind::Smoke).count();
        std::println!("{made} skid segments, {smoke} smoke");
        assert!(made > 10 && skids.len() as u64 == made.min(MAX_SKIDS as u64));
        assert!(smoke > 5);
        for s in skids {
            assert!(s.a.distance(s.b) < 4.0 && s.dark > 0.0 && s.a.is_finite());
        }
        // Smoke rises and fades away.
        g.keys = Default::default();
        for _ in 0..400 {
            g.update(STEP * 2.0);
            fx.update(&g, g.camera.eye, STEP * 2.0);
        }
        assert!(fx.particles.iter().all(|p| p.kind != Kind::Smoke || p.age < p.life));
    }

    #[test]
    fn crashes_throw_sparks_that_fall() {
        let mut g = Game::new(World::new(), Environment::new(TimeOfDay::Night, Weather::Clear));
        let p = g.player;
        // Straight into the wall of a bend at speed.
        g.cars[p] = Car::on_road(&g.world, CarKind::Police, Spec::interceptor(), 300.0, 2.1, false, 30.0);
        g.cars[p].role = Role::Player;
        g.cars[p].vehicle.yaw += 0.6;
        g.keys.up = true;
        let mut fx = Effects::new();
        let mut most = 0;
        for _ in 0..240 {
            g.update(STEP);
            fx.update(&g, g.cars[p].vehicle.pos, STEP);
            most = most.max(fx.particles.iter().filter(|p| p.kind == Kind::Spark).count());
        }
        std::println!("at most {most} sparks");
        assert!(most >= 4);
        assert!(fx.particles.len() <= MAX_PARTICLES);
    }
}
