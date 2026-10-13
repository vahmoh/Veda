// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The computer's drivers drive: traffic flows without piling up, a
//! suspect runs fast without getting stuck, the police catch up with it.

use crate::carmodel::Kind;
use crate::driver::{Police, Role, Suspect};
use crate::env::{Environment, TimeOfDay, Weather};
use crate::game::{Car, Event, Game, STEP};
use crate::vehicle::Spec;
use crate::world::World;

fn game() -> Game {
    Game::new(World::new(), Environment::new(TimeOfDay::Day, Weather::Clear))
}

/// Runs `secs` seconds, calling `each` after every step's events.
fn run(g: &mut Game, secs: f32, mut each: impl FnMut(&Game)) {
    let steps = (secs / STEP) as usize;
    for _ in 0..steps {
        g.update(STEP);
        for c in &g.cars {
            assert!(c.vehicle.pos.is_finite() && c.vehicle.vel.is_finite(), "a car's state went bad");
        }
        each(g);
    }
}

#[test]
fn traffic_flows() {
    let mut g = game();
    // The player cruises with the traffic.
    g.autopilot = true;
    g.start_traffic();
    let n = g.cars.len();
    std::println!("{} traffic cars", n - 1);
    assert!(n > 15, "only {} cars", n);
    let mut crashes = 0;
    let mut speed_sum = 0.0;
    let mut samples = 0;
    let start = g.cars[0].place(&g.world).0;
    run(&mut g, 90.0, |g| {
        for e in &g.events {
            let Event::Crash { impact, .. } = e;
            if impact.speed > 5.0 {
                crashes += 1;
            }
        }
        for c in &g.cars[1..] {
            speed_sum += c.vehicle.speed().abs();
            samples += 1;
        }
    });
    let mean = speed_sum / samples as f32;
    let travelled = g.world.road.delta(start, g.cars[0].place(&g.world).0).rem_euclid(g.world.road.length());
    std::println!("mean traffic speed {mean:.1} m/s, {crashes} crashes in 90 s, the player went {travelled:.0} m");
    assert!(mean > 7.0, "traffic crawls at {mean} m/s");
    assert!(crashes < 25, "{crashes} crashes");
    // Nobody left the road.
    for c in &g.cars {
        let at = g.world.road.locate(c.vehicle.pos, Some(c.hint));
        let (_, wall) = g.world.road.widths(at.s);
        assert!(at.lateral.abs() <= wall + 0.5, "a car off the road at lateral {}", at.lateral);
    }
}

/// Puts a suspect `ahead` metres in front of the player, at `speed`.
fn suspect(g: &mut Game, ahead: f32, speed: f32) -> usize {
    let s = g.cars[g.player].place(&g.world).0 + ahead;
    let mut car = Car::on_road(&g.world, Kind::Coupe, Spec::sports(), s, 2.1, false, speed);
    car.role = Role::Suspect(Suspect::new(2.1, 0.95, 0.5, g.player, 7));
    g.cars.push(car);
    g.cars.len() - 1
}

#[test]
fn a_suspect_runs_fast_and_free() {
    let mut g = game();
    let i = suspect(&mut g, 40.0, 25.0);
    let start = g.cars[i].place(&g.world).0;
    let mut walls = 0;
    let mut last = start;
    let mut travelled = 0.0;
    run(&mut g, 120.0, |g| {
        for e in &g.events {
            let Event::Crash { a, b, impact } = e;
            if *a == i && b.is_none() && impact.speed > 4.0 {
                walls += 1;
            }
        }
        let s = g.cars[i].place(&g.world).0;
        travelled += g.world.road.delta(last, s);
        last = s;
    });
    let mean = travelled / 120.0;
    std::println!(
        "the suspect went {travelled:.0} m in 120 s ({:.0} km/h on average), hit walls {walls} times",
        mean * 3.6
    );
    assert!(mean > 30.0, "only {mean} m/s on average");
    assert!(walls < 8, "{walls} hard hits on walls");
}

#[test]
fn the_police_catch_up() {
    let mut g = game();
    // As a pursuit starts: the suspect a little ahead, both at speed.
    let i = suspect(&mut g, 50.0, 25.0);
    g.cars[g.player].vehicle.set_speed(25.0);
    // The player's car, driven by the computer as police.
    let p = g.player;
    g.cars[p].role = Role::Police(Police::new(2.1, i, 1.1));
    g.cars[p].siren = true;
    let mut closest = f32::MAX;
    let mut touched = false;
    run(&mut g, 120.0, |g| {
        let d = g.cars[p].vehicle.pos.distance(g.cars[i].vehicle.pos);
        closest = closest.min(d);
        touched |= g.events.iter().any(|e| {
            let Event::Crash { a, b, .. } = e;
            (*a == p && *b == Some(i)) || (*a == i && *b == Some(p))
        });
    });
    let end = g.world.road.delta(g.cars[p].place(&g.world).0, g.cars[i].place(&g.world).0);
    std::println!(
        "closest {closest:.1} m, {end:.0} m behind at the end, touched {touched}, suspect damage {:.2}",
        g.cars[i].damage
    );
    // On its tail, and never losing it.
    assert!(closest < 25.0, "never closer than {closest} m");
    assert!(end < 160.0, "{end} m behind at the end");
}

#[test]
fn a_chase_through_traffic_has_few_bad_crashes() {
    // The computer drives the police car after a suspect through the
    // traffic: the hard crashes (closing faster than 15 m/s) involving
    // either of them, which wreck cars, stay rare.
    let mut g = game();
    g.start_traffic();
    let i = suspect(&mut g, 60.0, 22.0);
    g.cars[g.player].vehicle.set_speed(22.0);
    g.set_autopilot(true, Some(i));
    let p = g.player;
    let mut hard = 0;
    run(&mut g, 120.0, |g| {
        for e in &g.events {
            let Event::Crash { a, b, impact } = e;
            let involved = [*a, b.unwrap_or(usize::MAX)].iter().any(|&c| c == p || c == i);
            if involved && impact.speed > 15.0 {
                hard += 1;
            }
        }
    });
    std::println!(
        "{hard} hard crashes in 120 s, police damage {:.2}, suspect damage {:.2}",
        g.cars[p].damage,
        g.cars[i].damage
    );
    assert!(hard <= 2, "{hard} hard crashes");
}
