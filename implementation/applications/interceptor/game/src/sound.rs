// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What the game sounds like: from the cars on the road and what happened
//! to them, the [`Scene`] the mixer plays each frame (the engines, sirens
//! and tyres nearest the ears, the wind, a scrape, a tunnel's echo) and
//! the [`Cue`]s of what happened (crashes, the nitrous, a startled
//! driver's horn).

use alloc::vec::Vec;

use crate::audio::{Cue, Emitter, EngineState, Layout, Listener, Scene, SirenState, TireState, Wail};
use crate::carmodel::Kind;
use crate::driver::Role;
use crate::game::{Car, CarId, Event, Game};
use crate::prelude::*;
use crate::road::Section;

/// The engine a kind of car has.
pub fn layout(kind: Kind) -> Layout {
    match kind {
        Kind::Police => Layout::V8Charged,
        Kind::Muscle => Layout::V8,
        Kind::Supercar => Layout::V12,
        Kind::Coupe => Layout::Flat6,
        Kind::Suv => Layout::V6,
        Kind::Van => Layout::Diesel,
        Kind::Saloon | Kind::Hatchback | Kind::Estate | Kind::Taxi => Layout::I4,
    }
}

/// Engines further than this are not heard (m); sirens carry further.
const ENGINE_RANGE: f32 = 260.0;
const SIREN_RANGE: f32 = 900.0;
const TIRE_RANGE: f32 = 110.0;
/// Closer to a suspect than this, a siren yelps (m).
const YELP_RANGE: f32 = 45.0;
/// Two things that knocked are not heard knocking again until they have
/// been apart this long (s): a scrape is one sound, not a hundred.
const KNOCK_GAP: f32 = 0.3;

/// Keeps what the sounds need from frame to frame.
pub struct Sound {
    /// The game it last heard (its serial): its cars' numbers are its own.
    game: Option<u32>,
    /// Recent knocks: the two cars (a wall none), the time left before
    /// they knock again, and how hard it was.
    knocks: Vec<(CarId, Option<CarId>, f32, f32)>,
    boosting: bool,
    scrape: f32,
    tunnel: f32,
    /// Horns to sound: the time to wait (s), the car, how long.
    horns: Vec<(f32, CarId, f32)>,
    /// Every car by its distance from the ears (each frame's working).
    near: Vec<(f32, usize)>,
}

impl Sound {
    pub fn new() -> Sound {
        Sound {
            game: None,
            knocks: Vec::new(),
            boosting: false,
            scrape: 0.0,
            tunnel: 0.0,
            horns: Vec::new(),
            near: Vec::new(),
        }
    }

    /// The scene after the game's last update and frame (`dt` seconds
    /// after the one before), at `volume`, and the cues of what happened.
    pub fn update(&mut self, game: &Game, volume: f32, dt: f32) -> (Scene, Vec<Cue>) {
        if self.game != Some(game.serial()) {
            // Another game: what was heard of the last one's cars is not
            // this one's.
            *self = Sound { game: Some(game.serial()), near: core::mem::take(&mut self.near), ..Sound::new() };
        }
        let cam = &game.camera;
        let ahead = (cam.target - cam.eye).normalize_or(Vec3::Z);
        let right = ahead.cross(cam.up).normalize_or(Vec3::X);
        let player = game.player();
        let ears = Listener { pos: cam.eye, vel: velocity(player), right };
        let mut cues = Vec::new();
        let p = game.player;
        // Each car's voices by its number in its game.
        let key = |c: &Car| (u64::from(game.serial()) << 32) | u64::from(c.id.0);

        // Every car by its distance from the ears, the player's first.
        let near = &mut self.near;
        near.clear();
        near.extend(
            game.cars.iter().enumerate().map(|(i, c)| (if i == p { 0.0 } else { c.vehicle.pos.distance(cam.eye) }, i)),
        );
        near.sort_by(|a, b| a.0.total_cmp(&b.0));
        let suspect_near = |at: Vec3| {
            game.cars.iter().any(|c| matches!(c.role, Role::Suspect(_)) && c.vehicle.pos.distance(at) < YELP_RANGE)
        };

        let engines = near
            .iter()
            .filter(|&&(d, i)| d < ENGINE_RANGE && game.cars[i].damage < 1.0)
            .map(|&(_, i)| engine(&game.cars[i], key(&game.cars[i]), i == p, i == p && game.boost.on))
            .collect();
        let sirens = near
            .iter()
            .filter(|&&(d, i)| d < SIREN_RANGE && game.cars[i].siren)
            .map(|&(_, i)| {
                let c = &game.cars[i];
                let close = suspect_near(c.vehicle.pos);
                SirenState {
                    key: key(c),
                    emitter: Emitter { pos: c.vehicle.pos + Vec3::Y * 1.7, vel: velocity(c) },
                    wail: if close { Wail::Yelp } else { Wail::Wail },
                    own: i == p,
                }
            })
            .collect();
        let tires = near
            .iter()
            .filter(|&&(d, _)| d < TIRE_RANGE)
            .map(|&(_, i)| {
                let c = &game.cars[i];
                let v = &c.vehicle;
                TireState {
                    key: key(c),
                    emitter: Emitter { pos: v.pos + Vec3::Y * 0.3, vel: velocity(c) },
                    slip: if v.airborne { 0.0 } else { v.slip.iter().fold(0.0f32, |m, &s| m.max(s)) },
                    speed: if v.airborne { 0.0 } else { v.vel.length() },
                }
            })
            .collect();

        // Knocks: a crash sound for each new one.
        for k in &mut self.knocks {
            k.2 -= dt;
        }
        let mut scraping = 0.0f32;
        let me = player.id;
        for e in &game.events {
            let Event::Crash { a, b, impact } = *e;
            if a == me || b == Some(me) {
                scraping = scraping.max(impact.slide);
            }
            let strength = ((impact.speed - 1.5) / 18.0).clamp(0.0, 1.0);
            let known = self.knocks.iter_mut().find(|k| k.0 == a && k.1 == b);
            let new = match known {
                Some(k) => {
                    // Still touching: only a much harder knock is heard.
                    let harder = k.2 > 0.0 && strength > k.3 * 1.8 + 0.1;
                    let apart = k.2 <= 0.0;
                    k.2 = KNOCK_GAP;
                    if harder || apart {
                        k.3 = strength;
                    }
                    harder || apart
                }
                None => {
                    self.knocks.push((a, b, KNOCK_GAP, strength));
                    true
                }
            };
            if new && strength > 0.02 {
                let vel = game.car(a).map_or(Vec3::ZERO, velocity);
                cues.push(Cue::Crash { emitter: Emitter { pos: impact.point + Vec3::Y * 0.5, vel }, strength });
            }
            // A driver shaken by a knock leans on the horn.
            if new && impact.speed > 4.0 {
                for id in [Some(a), b].into_iter().flatten() {
                    if game.car(id).is_some_and(|c| matches!(c.role, Role::Traffic(_)))
                        && !self.horns.iter().any(|h| h.1 == id)
                    {
                        let r = hash(id.0 ^ game.steps() as u32);
                        self.horns.push((0.4 + r * 0.5, id, 0.4 + r * 0.8));
                    }
                }
            }
        }
        self.knocks.retain(|k| k.2 > -1.0);
        for h in &mut self.horns {
            h.0 -= dt;
            if h.0 <= 0.0
                && let Some(c) = game.car(h.1)
            {
                cues.push(Cue::Horn { emitter: Emitter { pos: c.vehicle.pos + Vec3::Y, vel: velocity(c) }, secs: h.2 });
            }
        }
        self.horns.retain(|h| h.0 > 0.0);

        // The nitrous lighting.
        if game.boost.on && !self.boosting {
            cues.push(Cue::Boost);
        }
        self.boosting = game.boost.on;

        // Scraping fades quickly once apart; tunnels echo near their
        // mouths a little, inside fully.
        let scrape_to = (scraping / 12.0).min(1.0);
        self.scrape = if scrape_to > self.scrape { scrape_to } else { (self.scrape - dt * 5.0).max(scrape_to) };
        let road = &game.world.road;
        let at = road.locate(cam.eye, Some(player.hint));
        let inside = [-24.0f32, -12.0, 0.0, 12.0, 24.0]
            .iter()
            .filter(|&&d| road.at(at.s + d).section == Section::Tunnel)
            .count();
        let tunnel_to = inside as f32 / 5.0;
        self.tunnel += (tunnel_to - self.tunnel) * (1.0 - libm::expf(-dt * 3.0));

        let scene = Scene {
            listener: ears,
            engines,
            sirens,
            tires,
            scrape: self.scrape,
            wind: ears.vel.length(),
            tunnel: self.tunnel,
            rain: game.env.rain,
            volume,
            paused: game.paused,
        };
        (scene, cues)
    }
}

impl Default for Sound {
    fn default() -> Sound {
        Sound::new()
    }
}

/// A car's velocity in three dimensions.
fn velocity(c: &Car) -> Vec3 {
    Vec3::new(c.vehicle.vel.x, c.vehicle.vy, c.vehicle.vel.y)
}

/// A car's engine, as the mixer hears it (`key` its voice's), the
/// player's `own`, with the nitrous burning if `boost`.
fn engine(c: &Car, key: u64, own: bool, boost: bool) -> EngineState {
    let v = &c.vehicle;
    let s = &v.spec;
    EngineState {
        key,
        emitter: Emitter { pos: v.pos + Vec3::Y * 0.5, vel: velocity(c) },
        layout: layout(c.kind),
        rpm: v.rpm,
        redline: s.rpm_red,
        // The drive is cut while changing gear.
        load: if v.shifting() { 0.0 } else { v.throttle },
        limiter: v.rpm >= s.rpm_red - 1.0,
        boost,
        own,
    }
}

/// A number in 0..1 from `x`, well mixed.
fn hash(x: u32) -> f32 {
    let mut h = x.wrapping_mul(0x9E37_79B9);
    h ^= h >> 15;
    h = h.wrapping_mul(0x85EB_CA6B);
    h ^= h >> 13;
    (h >> 8) as f32 / 16_777_216.0
}
