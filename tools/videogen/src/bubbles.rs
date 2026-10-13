// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! "Rising Bubbles": translucent bubbles rising through sunlit water, 360x640
//! (portrait) at 30 fps, 6 s, without sound.
//!
//! Everything moves in whole cycles of the clip's length, so it loops
//! seamlessly: each bubble rises through the frame once or twice and
//! wobbles a whole number of times, the light rays sway and the motes in
//! the water bob.

use std::f32::consts::TAU;

use rand::{RngExt, SeedableRng};
use rand_pcg::Pcg32;

use crate::paint::{Canvas, Rgb, add, gradient, hex, mix, scale, smoothstep, tonemap};

pub const WIDTH: usize = 360;
pub const HEIGHT: usize = 640;
pub const FPS: u32 = 30;
pub const SECONDS: u32 = 6;
pub const FRAMES: u32 = FPS * SECONDS;

/// A bubble: where it rises, its radius, and how it wobbles.
struct Bubble {
    x: f32,
    r: f32,
    /// Where in its rise the loop starts (0-1).
    start: f32,
    /// Rises per loop.
    laps: u32,
    /// How far it wobbles sideways, how many times a loop, from where.
    wobble: f32,
    wobbles: u32,
    phase: f32,
    /// 0 far away (small, faint, soft) to 1 close by.
    near: f32,
}

/// A shaft of sunlight from the surface: where it starts, how wide it is,
/// how far it sways and how bright it is.
struct Ray {
    x: f32,
    width: f32,
    sway: f32,
    phase: f32,
    strength: f32,
}

/// A speck in the water, bobbing around its place.
struct Mote {
    x: f32,
    y: f32,
    size: f32,
    drift: (f32, f32),
    phase: f32,
    bright: f32,
}

/// The water, the rays, the motes and the bubbles.
pub struct Bubbles {
    water: Canvas,
    bubbles: Vec<Bubble>,
    rays: Vec<Ray>,
    motes: Vec<Mote>,
}

impl Default for Bubbles {
    fn default() -> Self {
        Self::new()
    }
}

impl Bubbles {
    pub fn new() -> Bubbles {
        let (w, h) = (WIDTH as f32, HEIGHT as f32);
        let mut water = Canvas::new(WIDTH, HEIGHT);
        let stops = [(0.0, hex(0x6FD8E8)), (0.22, hex(0x2EA3CC)), (0.55, hex(0x135C96)), (1.0, hex(0x061A40))];
        water.shade(|x, y| {
            let c = gradient(&stops, y / h);
            // A brighter patch under the sun, darker sides.
            let dx = (x / w - 0.42) * 1.6;
            let glow = (-(dx * dx) * 3.0).exp() * (1.0 - y / h).powi(2) * 0.18;
            let side = 1.0 - 0.25 * smoothstep(0.25, 0.75, dx.abs());
            add(scale(c, side), scale(hex(0xBFF4FF), glow))
        });
        let mut rng = Pcg32::seed_from_u64(0xB0BB1E5);
        let mut bubbles: Vec<Bubble> = (0..34)
            .map(|i| {
                let near = rng.random::<f32>();
                let r = 3.0 + 9.0 * near + rng.random::<f32>() * (6.0 + 20.0 * near * near);
                Bubble {
                    x: rng.random_range(0.04..0.96) * w,
                    r,
                    start: (i as f32 * 0.618_034).fract(),
                    laps: if r > 14.0 { 2 } else { 1 },
                    wobble: rng.random_range(0.15..0.5) * r.sqrt() * 3.0,
                    wobbles: 2 + rng.random_range(0..4),
                    phase: rng.random::<f32>() * TAU,
                    near,
                }
            })
            .collect();
        // Far bubbles first.
        bubbles.sort_by(|a, b| a.near.total_cmp(&b.near));
        let rays = (0..6)
            .map(|i| Ray {
                x: (i as f32 + 0.3 + rng.random::<f32>() * 0.5) / 6.0 * w * 1.3 - 0.15 * w,
                width: rng.random_range(14.0..40.0),
                sway: rng.random_range(6.0..16.0),
                phase: rng.random::<f32>() * TAU,
                strength: rng.random_range(0.05..0.11),
            })
            .collect();
        let motes = (0..70)
            .map(|_| Mote {
                x: rng.random::<f32>() * w,
                y: rng.random::<f32>() * h,
                size: rng.random_range(0.5..1.3),
                drift: (rng.random_range(3.0..10.0), rng.random_range(4.0..14.0)),
                phase: rng.random::<f32>() * TAU,
                bright: rng.random_range(0.08..0.3),
            })
            .collect();
        Bubbles { water, bubbles, rays, motes }
    }

    /// The picture of `frame`, as `0xFFRRGGBB` pixels.
    pub fn picture(&self, frame: u32) -> Vec<u32> {
        let (w, h) = (WIDTH as f32, HEIGHT as f32);
        // The loop's progress, 0 to 1.
        let u = frame as f32 / FRAMES as f32;
        let mut c = self.water.clone();

        // Light rays from the surface, slanting a little and fading with depth.
        let slant = 0.18;
        for (i, p) in c.px.iter_mut().enumerate() {
            let (x, y) = ((i % WIDTH) as f32 + 0.5, (i / WIDTH) as f32 + 0.5);
            let depth = (-y / (h * 0.55)).exp();
            let mut light = 0.0;
            for r in &self.rays {
                let centre = r.x + slant * y + r.sway * (TAU * u + r.phase).sin();
                let d = (x - centre).abs() / (r.width * (1.0 + y / h));
                if d < 1.0 {
                    let flicker = 0.75 + 0.25 * (TAU * 2.0 * u + r.phase * 1.7).sin();
                    light += (1.0 - d * d).powi(2) * r.strength * flicker;
                }
            }
            *p = add(*p, scale(hex(0xD8FAFF), light * depth));
        }

        for m in &self.motes {
            let a = TAU * u + m.phase;
            let x = m.x + m.drift.0 * a.sin();
            let y = m.y + m.drift.1 * (a + 1.3).cos();
            c.splat(x, y, m.size, scale(hex(0xCFF6FF), m.bright));
        }

        for b in &self.bubbles {
            let travel = h + 2.0 * b.r + 40.0;
            let rise = (b.start + b.laps as f32 * u).fract();
            let y = h + b.r + 20.0 - rise * travel;
            let x = b.x + b.wobble * (TAU * b.wobbles as f32 * u + b.phase).sin();
            // A little squash and stretch as it wobbles.
            let squash = 0.06 * (TAU * 2.0 * b.wobbles as f32 * u + b.phase).sin();
            bubble(&mut c, x, y, b.r * (1.0 + squash), b.r * (1.0 - squash), b.near);
        }

        // A soft vignette, then the tone curve.
        for (i, p) in c.px.iter_mut().enumerate() {
            let (x, y) = ((i % WIDTH) as f32 / w - 0.5, (i / WIDTH) as f32 / h - 0.5);
            let k = 1.0 - 0.3 * smoothstep(0.3, 0.85, (x * x * 2.2 + y * y).sqrt());
            *p = tonemap(scale(*p, k));
        }
        c.to_pixels()
    }
}

/// Paints one bubble: a faint body, a bright rim, a highlight up and to the
/// left and a glint of light focused at its bottom. Far bubbles are fainter
/// and softer.
fn bubble(c: &mut Canvas, cx: f32, cy: f32, rx: f32, ry: f32, near: f32) {
    let soft = 1.0 + (1.0 - near) * 1.5;
    let x0 = ((cx - rx - 3.0).floor() as i32).max(0);
    let x1 = ((cx + rx + 3.0).ceil() as i32).min(c.w as i32 - 1);
    let y0 = ((cy - ry - 3.0).floor() as i32).max(0);
    let y1 = ((cy + ry + 3.0).ceil() as i32).min(c.h as i32 - 1);
    let tint: Rgb = hex(0xE6FBFF);
    let strength = 0.45 + 0.55 * near;
    let r = rx.max(ry);
    for y in y0..=y1 {
        for x in x0..=x1 {
            let (dx, dy) = ((x as f32 + 0.5 - cx) / rx, (y as f32 + 0.5 - cy) / ry);
            let d = (dx * dx + dy * dy).sqrt();
            // Coverage from the distance to the edge in pixels.
            let edge = ((1.0 - d) * r / soft + 0.5).clamp(0.0, 1.0);
            if edge <= 0.0 {
                continue;
            }
            let rim = smoothstep(0.55, 1.0, d).powf(1.6) * 0.7;
            let body = 0.07;
            let alpha = (body + rim) * edge * strength;
            // Highlights.
            let hl = {
                let (hx, hy) = (dx + 0.38, dy + 0.42);
                let e = (hx * hx * 1.4 + hy * hy).sqrt() / 0.24;
                smoothstep(1.0, 0.35, e) * 0.9
            };
            let glint = {
                let (gx, gy) = (dx - 0.18, dy - 0.62);
                let e = (gx * gx * 0.5 + gy * gy * 2.5).sqrt() / 0.3;
                smoothstep(1.0, 0.2, e) * 0.35 * smoothstep(0.4, 0.95, d)
            };
            let p = &mut c.px[y as usize * c.w + x as usize];
            *p = mix(*p, tint, alpha.min(1.0));
            *p = add(*p, scale(tint, (hl + glint) * edge * strength));
        }
    }
}
