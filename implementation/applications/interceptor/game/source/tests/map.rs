// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! A picture of Veda Bay seen from above (`INTERCEPTOR_MAP=path.png`), and
//! the layout's sanity: the loop never comes near itself.

use alloc::vec;

use glam::Vec3Swizzles;

use crate::layout::VEDA_BAY;
use crate::road::{Road, Section};
use crate::terrain::{HALF_X, HALF_Z, Terrain};

#[test]
fn the_loop_keeps_its_distance_from_itself() {
    let road = Road::new(VEDA_BAY);
    std::println!("Veda Bay: {:.0} m, {} samples", road.length(), road.len());
    assert!(road.length() > 5000.0 && road.length() < 9000.0);
    // Samples far apart along the road are far apart on the ground.
    let n = road.len();
    let step = 4;
    for i in (0..n).step_by(step) {
        for j in (0..n).step_by(step) {
            let along = road.delta(i as f32 * road.spacing(), j as f32 * road.spacing()).abs();
            if along > 300.0 {
                let d = road.samples()[i].pos.xz().distance(road.samples()[j].pos.xz());
                assert!(d > 120.0, "samples {i} and {j} are {d:.0} m apart on the ground");
            }
        }
    }
    // No corner tighter than a radius of 45 m, and the grades gentle.
    for s in road.samples() {
        assert!(s.curvature.abs() < 1.0 / 45.0, "a corner of radius {:.0} m", 1.0 / s.curvature.abs());
        assert!(s.forward.y.abs() < 0.09, "a grade of {:.1}%", s.forward.y * 100.0);
    }
}

#[test]
fn map() {
    let Ok(path) = std::env::var("INTERCEPTOR_MAP") else { return };
    let road = Road::new(VEDA_BAY);
    let terrain = Terrain::new(&road, 7);
    let (w, h) = (1200u32, 1000u32);
    // The whole world.
    let scale = w as f32 / (2.0 * HALF_X);
    let to_px = |x: f32, z: f32| (((x + HALF_X) * scale) as i32, ((z + HALF_Z) * scale) as i32);
    let mut px = vec![0u32; (w * h) as usize];
    // The ground: blue under water (darker deeper), green to brown to white
    // with height, shaded from the north-west.
    for y in 0..h {
        for x in 0..w {
            let (wx, wz) = (x as f32 / scale - HALF_X, y as f32 / scale - HALF_Z);
            let g = terrain.height(wx, wz);
            let n = terrain.normal(wx, wz);
            let light = (0.55 + 0.45 * (n.x * -0.5 + n.y * 0.7 + n.z * -0.5)).clamp(0.2, 1.0);
            let (r, gg, b) = if g < 0.0 {
                let d = (-g / 25.0).min(1.0);
                (20.0 + 30.0 * (1.0 - d), 60.0 + 60.0 * (1.0 - d), 110.0 + 70.0 * (1.0 - d))
            } else {
                let t = (g / 250.0).min(1.0);
                (70.0 + 130.0 * t, 120.0 + 60.0 * t - 40.0 * t * t, 60.0 + 140.0 * t * t)
            };
            let c = |v: f32| ((v * light).clamp(0.0, 255.0)) as u32;
            px[(y * w + x) as usize] = 0xFF00_0000 | c(r) << 16 | c(gg) << 8 | c(b);
        }
    }
    for s in road.samples() {
        let color = match s.section {
            Section::City => 0xFFE0E0E0,
            Section::Hills => 0xFF60C060,
            Section::Tunnel => 0xFF805020,
            Section::Coast => 0xFFE0C060,
            Section::Bridge => 0xFF60A0FF,
            Section::Port => 0xFFE06040,
        };
        let (cx, cy) = to_px(s.pos.x, s.pos.z);
        for dy in -1..=1 {
            for dx in -1..=1 {
                let (x, y) = (cx + dx, cy + dy);
                if x >= 0 && y >= 0 && (x as u32) < w && (y as u32) < h {
                    px[(y as u32 * w + x as u32) as usize] = color;
                }
            }
        }
    }
    // The start: a white square.
    let (sx, sy) = to_px(road.samples()[0].pos.x, road.samples()[0].pos.z);
    for dy in -4..=4 {
        for dx in -4..=4 {
            px[((sy + dy) as u32 * w + (sx + dx) as u32) as usize] = 0xFFFFFFFF;
        }
    }
    let image = image::Image { width: w, height: h, pixels: px };
    std::fs::write(path, image::encode_png(&image).unwrap()).unwrap();
}
