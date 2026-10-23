// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The chase camera keeps its place behind the car however long the
//! frames are: at any frame rate, through frames of uneven length, and
//! through stalls longer than the game catches up on.

use alloc::sync::Arc;
use alloc::vec::Vec;

use rand::{RngExt, SeedableRng};
use rand_pcg::Pcg32;

use crate::career::SUSPECTS;
use crate::env::{Environment, TimeOfDay, Weather};
use crate::game::{Game, STEP};
use crate::prelude::*;
use crate::pursuit::Pursuit;
use crate::road::Section;
use crate::world::World;

/// One frame: the time drawn and how far behind the car (as drawn) the
/// camera is, along the car's heading.
#[derive(Clone, Copy, Debug)]
struct Sample {
    t: f32,
    behind: f32,
}

/// A chase at speed on the coast road, the computer driving the player's
/// car after a supercar (following it, not ramming it: a knock shakes the
/// camera): `warm` seconds of smooth frames, then frames of the lengths
/// `dts` gives until `secs` more seconds of the game have passed.
fn film(world: &Arc<World>, warm: f32, secs: f32, dts: impl Fn(usize) -> f32) -> Vec<Sample> {
    let mut g = Game::new(world.clone(), Environment::new(TimeOfDay::Day, Weather::Clear));
    g.place_player(crate::career::section_start(&g.world.road, Section::Coast), 30.0);
    let chase = Pursuit::start(&mut g, &SUSPECTS[3].1, 40.0, 35.0);
    g.set_autopilot(true, Some(chase.suspect));
    g.autopilot_follows(30.0);
    // The time drawn: the game's clock, a step back, and what it has not
    // stepped yet (followed here as the game follows it).
    let mut pending = 0.0f32;
    let clock = |g: &Game| g.clock() as f32;
    let mut frame = |g: &mut Game, dt: f32| {
        g.update(dt);
        pending = (pending + dt).min(0.1);
        while pending >= STEP {
            pending -= STEP;
        }
        let (views, _) = g.frame(dt);
        let m = views[g.player].transform;
        let ahead = m.z_axis.truncate();
        let ahead = Vec3::new(ahead.x, 0.0, ahead.z).normalize();
        Sample { t: clock(g) - STEP + pending, behind: -(g.camera.eye - m.w_axis.truncate()).dot(ahead) }
    };
    let smooth = 1.0 / 240.0;
    for _ in 0..(warm / smooth) as usize {
        frame(&mut g, smooth);
    }
    let end = clock(&g) + secs;
    let mut out = Vec::new();
    let mut i = 0;
    while clock(&g) < end {
        out.push(frame(&mut g, dts(i)));
        i += 1;
    }
    out
}

/// How far behind the camera is at time `t` in `film` (between its frames).
fn behind_at(film: &[Sample], t: f32) -> f32 {
    let k = film.partition_point(|s| s.t < t).clamp(1, film.len() - 1);
    let (a, b) = (film[k - 1], film[k]);
    a.behind + (b.behind - a.behind) * ((t - a.t) / (b.t - a.t)).clamp(0.0, 1.0)
}

#[test]
fn the_chase_camera_keeps_its_place_whatever_the_frames() {
    let world = Arc::new(World::new());
    let (warm, secs) = (12.0, 12.0);
    // Frames at 240 a second: the camera as it should be.
    let reference = film(&world, warm, secs + 0.5, |_| 1.0 / 240.0);
    let mut rng = Pcg32::seed_from_u64(7);
    let uneven: Vec<f32> = (0..2000).map(|_| rng.random_range(0.008..0.040)).collect();
    let cases: [(&str, &dyn Fn(usize) -> f32); 7] = [
        ("75 fps", &|_| 1.0 / 75.0),
        ("60 fps", &|_| 1.0 / 60.0),
        ("20 fps", &|_| 1.0 / 20.0),
        ("60 fps, every other frame late", &|i| if i % 2 == 0 { 1.0 / 60.0 } else { 2.0 / 60.0 }),
        ("60 fps, a 100 ms frame a second", &|i| if i % 60 == 59 { 0.1 } else { 1.0 / 60.0 }),
        ("60 fps, a 250 ms stall every 2 s", &|i| if i % 120 == 119 { 0.25 } else { 1.0 / 60.0 }),
        ("8 to 40 ms at random", &|i| uneven[i % uneven.len()]),
    ];
    let mean = reference.iter().map(|s| s.behind).sum::<f32>() / reference.len() as f32;
    std::println!("at 240 fps the camera is {mean:.2} m behind the car");
    for (name, dts) in cases {
        let frames = film(&world, warm, secs, dts);
        // Off from where it should be, and how much that changes from one
        // frame to the next (the camera jumping about behind the car): a
        // few millimetres, a few centimetres as the car scrapes a wall
        // (stepping the spring once a frame put it 0.5 m off at 60 fps,
        // 4 m off after a 100 ms frame, 30 m after a stall).
        let off: Vec<f32> = frames.iter().map(|s| s.behind - behind_at(&reference, s.t)).collect();
        let worst = off.iter().fold(0.0f32, |m, o| m.max(o.abs()));
        let jump = off.windows(2).fold(0.0f32, |m, w| m.max((w[1] - w[0]).abs()));
        std::println!("{name}: {} frames, at worst {worst:.3} m off, jumping {jump:.3} m", frames.len());
        assert!(worst < 0.1 && jump < 0.1, "{name}: {worst} m off, jumping {jump} m");
    }
}
