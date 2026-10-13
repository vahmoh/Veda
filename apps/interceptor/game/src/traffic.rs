// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Traffic round the player: cars both ways, as many as the district is
//! busy, in the colours people buy.
//!
//! The cars live in a window of road round the player. One that falls too
//! far behind or gets too far ahead is taken off and put back near the
//! window's far end, out of sight, so that the road is always busy where
//! the player is and nothing appears in view. Cars are moved rather than
//! removed, so that the other drivers' references to them (a police car's
//! target) stay valid.

use rand::RngExt;
use rand::seq::IndexedRandom;
use rand_pcg::Pcg32;

use crate::carmodel::Kind;
use crate::driver::{Role, Traffic};
use crate::prelude::*;
use crate::road::{Road, Section};
use crate::vehicle::Spec;
use crate::world::World;

/// The window round the player: behind, ahead (m).
pub const BEHIND: f32 = 230.0;
pub const AHEAD: f32 = 560.0;
/// No car is put back nearer the player than this (m).
const OUT_OF_SIGHT: f32 = 330.0;

/// Cars per kilometre of road (both directions together) in a district.
pub fn density(section: Section) -> f32 {
    match section {
        Section::City => 30.0,
        Section::Port => 16.0,
        Section::Bridge => 18.0,
        Section::Hills | Section::Coast => 12.0,
        Section::Tunnel => 14.0,
    }
}

/// A kind of car, its make-up and a paint, as traffic in `section`.
pub fn pick(rng: &mut Pcg32, section: Section) -> (Kind, Spec, Vec3) {
    let kinds: &[(Kind, u32)] = match section {
        Section::City => &[
            (Kind::Saloon, 5),
            (Kind::Hatchback, 5),
            (Kind::Taxi, 3),
            (Kind::Suv, 3),
            (Kind::Estate, 2),
            (Kind::Van, 2),
        ],
        Section::Port => &[(Kind::Van, 4), (Kind::Saloon, 3), (Kind::Suv, 2), (Kind::Estate, 1)],
        _ => &[
            (Kind::Saloon, 5),
            (Kind::Suv, 4),
            (Kind::Hatchback, 3),
            (Kind::Estate, 3),
            (Kind::Van, 1),
            (Kind::Coupe, 1),
        ],
    };
    let kind = kinds.choose_weighted(rng, |k| k.1).map_or(kinds[0].0, |k| k.0);
    let spec = match kind {
        Kind::Van => Spec::van(),
        Kind::Coupe => Spec::sports(),
        _ => Spec::saloon(),
    };
    // The colours of real traffic: mostly white, black, greys and silver.
    let paints = [
        (Vec3::new(0.80, 0.80, 0.80), 6),
        (Vec3::new(0.02, 0.02, 0.022), 5),
        (Vec3::new(0.35, 0.36, 0.38), 5),
        (Vec3::new(0.55, 0.56, 0.58), 4),
        (Vec3::new(0.30, 0.02, 0.02), 2),
        (Vec3::new(0.02, 0.07, 0.25), 2),
        (Vec3::new(0.05, 0.12, 0.08), 1),
        (Vec3::new(0.45, 0.33, 0.20), 1),
        (Vec3::new(0.70, 0.45, 0.04), 1),
    ];
    let mut paint = paints.choose_weighted(rng, |p| p.1).map_or(paints[0].0, |p| p.0);
    if kind == Kind::Taxi {
        paint = Vec3::new(0.85, 0.62, 0.02);
    }
    (kind, spec, paint)
}

/// A traffic car's place, lane and speed for road position `s`: forward
/// or the other way, inner or outer lane.
pub struct Slot {
    pub s: f32,
    pub lane: usize,
    pub side: f32,
}

/// Whether a car could stand at `slot` without being within `room`
/// metres of another car in the same lane (`places`: each car's s and
/// lateral).
pub fn free(road: &Road, places: &[(f32, f32)], slot: &Slot, room: f32) -> bool {
    let lat = Road::lane_center(slot.lane, slot.side);
    places.iter().all(|&(s, l)| (l - lat).abs() > 2.5 || road.delta(slot.s, s).abs() > room)
}

/// A new traffic driver for `slot`, liking a speed near the limit.
pub fn role(rng: &mut Pcg32, slot: &Slot) -> Role {
    Role::Traffic(Traffic::new(slot.lane, slot.side, rng.random_range(0.85..1.08)))
}

/// How many traffic cars the window round `s` holds.
pub fn wanted(world: &World, s: f32) -> usize {
    let road = &world.road;
    let mut cars = 0.0;
    let step = 50.0;
    let mut d = -BEHIND;
    while d < AHEAD {
        cars += density(road.at(s + d).section) * step / 1000.0;
        d += step;
    }
    cars as usize
}

/// A place in the window round the player at `player_s` to put a car,
/// out of sight: far ahead (both ways), or far behind on the player's
/// side (it will come up from behind).
pub fn far_slot(rng: &mut Pcg32, road: &Road, player_s: f32) -> Slot {
    let side = if rng.random_bool(0.5) { 1.0 } else { -1.0 };
    let lane = rng.random_range(0..2) as usize;
    let behind = side > 0.0 && rng.random_bool(0.2);
    let d = if behind { -rng.random_range(OUT_OF_SIGHT * 0.5..=BEHIND) } else { rng.random_range(OUT_OF_SIGHT..AHEAD) };
    Slot { s: road.wrap(player_s + d), lane, side }
}

/// A place anywhere in the window (for filling it at the start), but not
/// right by the player.
pub fn any_slot(rng: &mut Pcg32, road: &Road, player_s: f32) -> Slot {
    let side = if rng.random_bool(0.5) { 1.0 } else { -1.0 };
    let lane = rng.random_range(0..2) as usize;
    let mut d = rng.random_range(-BEHIND..AHEAD);
    if d.abs() < 40.0 {
        d = 40.0 * d.signum().max(0.0) + 40.0;
    }
    Slot { s: road.wrap(player_s + d), lane, side }
}

/// Whether a car at `s` has left the window round `player_s`.
pub fn gone(road: &Road, player_s: f32, s: f32) -> bool {
    let d = road.delta(player_s, s);
    !(-BEHIND - 20.0..AHEAD + 20.0).contains(&d)
}
