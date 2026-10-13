// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! A pursuit: the player's police car after a suspect.
//!
//! The suspect is stopped one of two ways: **taken down**, its car wrecked
//! by enough hard knocks from the police (a suspect's car breaks after
//! about six good rams; a wall it is pushed into straight after counts
//! double; its own mistakes count as for any car), or **busted**, boxed
//! in and held to a standstill with the police alongside for a few
//! seconds. It **escapes** if it gets far enough ahead and stays there,
//! out of sight. A timer counts up; a mission may set a limit.

use crate::carmodel::Kind;
use crate::driver::{Role, Suspect};
use crate::game::{Car, Event, Game, STEP};
use crate::prelude::*;
use crate::vehicle::Spec;

/// Further ahead than this, the suspect is getting away (m).
pub const ESCAPE_GAP: f32 = 380.0;
/// For this long, and it has escaped (s).
pub const ESCAPE_TIME: f32 = 8.0;
/// Held this slow (m/s), this near (m), this long (s): busted.
pub const BUST_SPEED: f32 = 2.5;
pub const BUST_RANGE: f32 = 11.0;
pub const BUST_TIME: f32 = 3.0;
/// How much harder a suspect's car takes knocks from the police than
/// other cars do.
pub const FRAGILITY: f32 = 3.2;
/// A wall hit this soon after a ram (s) is the ram's doing.
const PUSHED: f32 = 2.0;

/// How a pursuit stands.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum State {
    /// On; `escaping` counts how long the suspect has been out of reach,
    /// `busting` how long it has been held.
    Chasing { escaping: f32, busting: f32 },
    /// Over: stopped by its damage.
    TakenDown,
    /// Over: held to a standstill.
    Busted,
    /// Over: it got away.
    Escaped,
}

/// The suspect's car and driver.
#[derive(Clone, Copy, Debug)]
pub struct Wanted {
    pub kind: Kind,
    pub spec: Spec,
    pub paint: Vec3,
    /// How well it drives (0.8..1.0) and how readily it rams back (0..1).
    pub skill: f32,
    pub aggression: f32,
}

/// A pursuit under way.
#[derive(Clone, Copy, Debug)]
pub struct Pursuit {
    pub suspect: usize,
    pub state: State,
    /// Seconds since it began.
    pub time: f32,
    /// Hits the police landed, and the hardest.
    pub hits: u32,
    /// The distance between them along the road (m, positive while the
    /// suspect is ahead).
    pub gap: f32,
    /// Seconds since the police last hit it.
    since_hit: f32,
}

impl Pursuit {
    /// Puts the suspect on the road `ahead` metres in front of the player
    /// at the player's speed (or `speed` if faster), and starts the chase.
    pub fn start(game: &mut Game, wanted: &Wanted, ahead: f32, speed: f32) -> Pursuit {
        let p = game.player;
        let (s, _) = game.cars[p].place(&game.world);
        let start = game.world.road.wrap(s + ahead);
        let lane = crate::road::Road::lane_center(1, 1.0);
        let speed = speed.max(game.cars[p].vehicle.speed());
        let mut car = Car::on_road(&game.world, wanted.kind, wanted.spec, start, lane, false, speed);
        car.paint = wanted.paint;
        let seed = (start * 13.0) as u32 ^ 0x5EED;
        car.role = Role::Suspect(Suspect::new(lane, wanted.skill, wanted.aggression, p, seed));
        game.cars.push(car);
        let suspect = game.cars.len() - 1;
        Pursuit {
            suspect,
            state: State::Chasing { escaping: 0.0, busting: 0.0 },
            time: 0.0,
            hits: 0,
            gap: ahead,
            since_hit: f32::MAX,
        }
    }

    /// Whether it is still on.
    pub fn on(&self) -> bool {
        matches!(self.state, State::Chasing { .. })
    }

    /// How much of the suspect's car is left (1 whole, 0 wrecked).
    pub fn integrity(&self, game: &Game) -> f32 {
        1.0 - game.cars[self.suspect].damage
    }

    /// Follows the pursuit through a step of the game, just taken (see
    /// [`Game::update_with`]): step by step, it goes the same however the
    /// game's frames fall.
    pub fn step(&mut self, game: &mut Game) {
        let dt = STEP;
        let State::Chasing { mut escaping, mut busting } = self.state else { return };
        self.time += dt;
        self.since_hit += dt;
        let (p, i) = (game.player, self.suspect);
        // The knocks it took: harder from the police (any police car), and
        // from a wall it was pushed into; a ram by the player counted.
        for k in game.step_events() {
            let Event::Crash { a, b, impact } = game.events[k];
            let other = match (a == i, b == Some(i)) {
                (true, _) => b,
                (_, true) => Some(a),
                _ => continue,
            };
            let police = other.is_some_and(|o| o == p || matches!(game.cars[o].role, Role::Police(_)));
            let pushed = other.is_none() && self.since_hit < PUSHED;
            let k = if police {
                1.0
            } else if pushed {
                2.0
            } else {
                0.0
            };
            game.cars[i].damage = (game.cars[i].damage + impact.harm() * (FRAGILITY - 1.0) * k).min(1.0);
            if police && impact.speed > 3.0 {
                self.since_hit = 0.0;
                if other == Some(p) {
                    self.hits += 1;
                }
            }
        }
        let ps = game.cars[p].place(&game.world).0;
        let ss = game.cars[i].place(&game.world).0;
        self.gap = game.world.road.delta(ps, ss);
        if game.cars[i].damage >= 1.0 {
            self.state = State::TakenDown;
            game.cars[i].role = Role::Parked;
            game.cars[i].headlights = false;
            return;
        }
        // Held still with the police right there.
        let near = game.cars[p].vehicle.pos.distance(game.cars[i].vehicle.pos) < BUST_RANGE;
        if near && game.cars[i].vehicle.speed().abs() < BUST_SPEED {
            busting += dt;
        } else {
            busting = (busting - dt * 2.0).max(0.0);
        }
        if busting >= BUST_TIME {
            self.state = State::Busted;
            game.cars[i].role = Role::Parked;
            return;
        }
        if self.gap > ESCAPE_GAP {
            escaping += dt;
        } else {
            escaping = (escaping - dt).max(0.0);
        }
        if escaping >= ESCAPE_TIME {
            self.state = State::Escaped;
            return;
        }
        self.state = State::Chasing { escaping, busting };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::{Environment, TimeOfDay, Weather};
    use crate::world::World;

    fn wanted() -> Wanted {
        Wanted {
            kind: Kind::Coupe,
            spec: Spec::sports(),
            paint: Vec3::new(0.5, 0.02, 0.02),
            skill: 0.95,
            aggression: 0.0,
        }
    }

    #[test]
    fn rams_take_a_suspect_down() {
        let mut g = Game::new(World::new(), Environment::new(TimeOfDay::Day, Weather::Clear));
        let mut chase = Pursuit::start(&mut g, &wanted(), 30.0, 20.0);
        // The player's car rams it, again and again: put it right behind,
        // faster.
        let mut rams = 0;
        for _ in 0..40 {
            if !chase.on() {
                break;
            }
            let (s, l) = g.cars[chase.suspect].place(&g.world);
            let v = g.cars[chase.suspect].vehicle.speed();
            let p = g.player;
            let mut car = Car::on_road(&g.world, Kind::Police, Spec::interceptor(), s - 6.0, l, false, v + 14.0);
            car.role = Role::Player;
            g.cars[p] = car;
            for _ in 0..(0.6 / STEP) as usize {
                g.update_with(STEP, |g| chase.step(g));
            }
            rams += 1;
        }
        std::println!("{rams} rams, {} hits, state {:?}", chase.hits, chase.state);
        assert_eq!(chase.state, State::TakenDown);
        assert!((3..=12).contains(&rams), "{rams} rams");
    }

    #[test]
    fn a_suspect_left_alone_escapes() {
        let mut g = Game::new(World::new(), Environment::new(TimeOfDay::Day, Weather::Clear));
        let mut chase = Pursuit::start(&mut g, &wanted(), 60.0, 30.0);
        // The player stays put.
        for _ in 0..(90.0 / STEP) as usize {
            g.update_with(STEP, |g| chase.step(g));
            if !chase.on() {
                break;
            }
        }
        assert_eq!(chase.state, State::Escaped);
    }
}
