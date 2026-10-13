// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! "Aurora Timelapse": northern lights over a mountain lake, 1280x720 at
//! 24 fps, 16 s, with an ambient pad and soft bells.
//!
//! Ribbons of light (green at their sharp lower border, through teal to
//! violet at the top of their rays) wave slowly across a starry sky, each a
//! translucent path filled with a gradient; stars twinkle and a meteor
//! falls; the camera pans slowly over dark mountains, whose snow catches
//! the light, and the lake below mirrors it all through gentle ripples.

use std::f32::consts::TAU;

use fastnoise_lite::{FastNoiseLite, FractalType, NoiseType};
use rand::{RngExt, SeedableRng};
use rand_pcg::Pcg32;
use vaudio::synth::{self, Adsr, Analog, FilterSpec, Fm, Mode, OscSpec, Part, Song, Wave, theory};
use vraster::{Path, Rasterizer, Transform};

use crate::paint::{Blend, Canvas, Rgb, add, fbm1, gradient, hex, mix, noise1, scale, smoothstep, tonemap};

pub const WIDTH: usize = 1280;
pub const HEIGHT: usize = 720;
pub const FPS: u32 = 24;
pub const SECONDS: u32 = 16;
pub const FRAMES: u32 = FPS * SECONDS;
const RATE: u32 = crate::RATE;

/// The lake's far shore, in rows from the top.
const SHORE: usize = 500;
/// How far the camera pans over the clip, in pixels.
const PAN: f32 = 300.0;
/// The near hills move this much faster than the distance.
const NEAR: f32 = 1.3;

/// The camera's offset at `t` seconds: a steady pan that eases at both ends.
fn pan(t: f32) -> f32 {
    let s = t / SECONDS as f32;
    PAN * (s - 0.6 * (TAU * s).sin() / TAU)
}

/// A star bright enough to twinkle: its place in the panorama, size and
/// colour, and how fast (Hz) and from where its brightness wavers.
struct Star {
    x: f32,
    y: f32,
    sigma: f32,
    color: Rgb,
    rate: f32,
    phase: f32,
}

/// A curtain of light.
struct Ribbon {
    /// The lower border's height at rest.
    base: f32,
    /// (amplitude, wavelength, angular speed, phase) of its waves.
    waves: [(f32, f32, f32, f32); 4],
    /// How far its rays reach up.
    height: f32,
    strength: f32,
    /// Shifts the colours towards violet.
    tint: f32,
    /// Where in the panorama it fades in and out.
    span: (f32, f32),
    /// Where its slow brightening and dimming starts.
    pulse: f32,
    seed: u32,
}

impl Ribbon {
    fn edge(&self, x: f32, t: f32) -> f32 {
        self.base + self.waves.iter().map(|&(a, l, w, p)| a * (TAU * x / l + w * t + p).sin()).sum::<f32>()
    }
}

/// Fractal Perlin noise (FastNoise Lite) over noise units: `octaves`
/// octaves, each `lacunarity` times the frequency and half the amplitude of
/// the one before.
fn fractal(kind: FractalType, octaves: i32, lacunarity: f32) -> FastNoiseLite {
    let mut noise = FastNoiseLite::with_seed(0xA0_7A);
    noise.set_noise_type(Some(NoiseType::Perlin));
    noise.set_frequency(Some(1.0));
    noise.set_fractal_type(Some(kind));
    noise.set_fractal_octaves(Some(octaves));
    noise.set_fractal_lacunarity(Some(lacunarity));
    noise.set_fractal_gain(Some(0.5));
    noise
}

/// Ridged noise from 0 to 1: 1 on the crests, falling away sharply.
fn ridges(noise: &FastNoiseLite, x: f32, y: f32) -> f32 {
    let r = 0.5 * (noise.get_noise_2d(x, y) + 1.0);
    r * r
}

/// The scene: the sky and the mountains are painted once, as panoramas
/// wider than the frame, and the camera's window moves over them.
pub struct Aurora {
    sky: Canvas,
    far: Vec<f32>,
    snow: Canvas,
    near: Vec<f32>,
    stars: Vec<Star>,
    ribbons: Vec<Ribbon>,
    colours: Vec<Rgb>,
}

impl Default for Aurora {
    fn default() -> Self {
        Self::new()
    }
}

/// Entries of the colour ramp of the light, over heights 0 to [`RAMP_TOP`].
const RAMP: usize = 256;
const RAMP_TOP: f32 = 1.8;

impl Aurora {
    pub fn new() -> Aurora {
        let sw = WIDTH + PAN.ceil() as usize + 2;
        let mut sky = Canvas::new(sw, SHORE);
        let (fbm2, fbm3, fbm4) =
            (fractal(FractalType::FBm, 2, 2.0), fractal(FractalType::FBm, 3, 2.0), fractal(FractalType::FBm, 4, 2.0));
        let (ridged4, ridged5) = (fractal(FractalType::Ridged, 4, 2.0), fractal(FractalType::Ridged, 5, 2.1));
        let stops = [(0.0, hex(0x02040C)), (0.45, hex(0x050C1C)), (0.8, hex(0x0A1A30)), (1.0, hex(0x123247))];
        sky.shade(|x, y| {
            let v = y / SHORE as f32;
            let mut c = gradient(&stops, v);
            // Airglow low over the mountains.
            c = add(c, scale(hex(0x0F3A33), smoothstep(0.55, 1.0, v) * 0.35));
            // A faint Milky Way across the upper sky, with dark lanes.
            let d = (y - (40.0 + x * 0.16)) / 70.0;
            let band = (-d * d).exp();
            if band > 0.01 {
                let cloud = 0.5 + 0.5 * fbm4.get_noise_2d(x / 90.0, y / 90.0);
                let lane = smoothstep(0.1, 0.5, fbm3.get_noise_2d(x / 40.0 + 7.0, y / 25.0).abs());
                c = add(c, scale(hex(0x9FB0D8), band * cloud * lane * 0.07));
            }
            c
        });
        let mut rng = Pcg32::seed_from_u64(0x57A2);
        // Faint stars, painted into the panorama.
        for _ in 0..2600 {
            let (x, y) = (rng.random::<f32>() * sw as f32, rng.random::<f32>() * SHORE as f32);
            let b = 0.05 + 0.25 * rng.random::<f32>().powi(3);
            sky.splat(x, y, rng.random_range(0.45..0.7), scale(star_colour(&mut rng), b));
        }
        // Bright ones twinkle, frame by frame.
        let stars = (0..220)
            .map(|_| Star {
                x: rng.random::<f32>() * sw as f32,
                y: rng.random::<f32>().powf(1.3) * SHORE as f32 * 0.95,
                sigma: rng.random_range(0.55..1.1),
                color: scale(star_colour(&mut rng), rng.random_range(0.35..1.0)),
                rate: rng.random_range(0.4..2.2),
                phase: rng.random::<f32>() * TAU,
            })
            .collect();

        // The mountains: a far range with a few tall peaks, and near hills.
        let far: Vec<f32> = (0..sw)
            .map(|x| {
                let x = x as f32;
                let ridge = ridges(&ridged5, x / 520.0 + 3.1, 0.37);
                let rise = 40.0 + 230.0 * ridge.powf(1.6) + 14.0 * fbm3.get_noise_2d(x / 45.0, 1.7);
                SHORE as f32 - rise
            })
            .collect();
        let mut snow = Canvas::new(sw, SHORE);
        snow.shade(|x, y| {
            let ridge = far[(x as usize).min(sw - 1)];
            let below = y - ridge;
            if below < 0.0 {
                return [0.0; 3];
            }
            let streaks = ridges(&ridged4, x / 26.0, y / 60.0);
            let k =
                smoothstep(0.62, 0.9, streaks) * (-below / 70.0).exp() * smoothstep(150.0, 230.0, SHORE as f32 - ridge);
            [k; 3]
        });
        let nw = WIDTH + (PAN * NEAR).ceil() as usize + 2;
        let mut near: Vec<f32> = (0..nw)
            .map(|x| {
                let x = x as f32;
                let hill = 0.5 + 0.5 * fbm4.get_noise_2d(x / 260.0 + 11.0, 4.2);
                SHORE as f32 - 6.0 - 46.0 * hill * hill
            })
            .collect();
        // Spruces along the near shore, thinning out in clearings.
        let ground = near.clone();
        let mut x = 0.0;
        while x < nw as f32 {
            let density = smoothstep(-0.25, 0.2, fbm2.get_noise_2d(x / 140.0, 9.3));
            if rng.random::<f32>() < density {
                let height = rng.random_range(9.0..27.0) * (0.55 + 0.45 * density);
                let half = height * rng.random_range(0.17..0.26);
                let base = ground[(x as usize).min(nw - 1)] + 2.0;
                let (from, to) = ((x - half).floor().max(0.0) as usize, (x + half).ceil() as usize);
                for (c, top) in near.iter_mut().enumerate().take(to).skip(from) {
                    let d = ((c as f32 + 0.5 - x) / half).abs();
                    if d < 1.0 {
                        // Pointed, with ragged sides.
                        let ragged = 1.0 - 0.12 * ((c as f32 * 2.7 + height).sin() * 0.5 + 0.5);
                        *top = top.min(base - height * (1.0 - d).powf(0.85) * ragged);
                    }
                }
            }
            x += rng.random_range(3.0..8.0);
        }

        let ribbons = vec![
            Ribbon {
                base: 318.0,
                waves: [
                    (30.0, 980.0, 0.32, 0.0),
                    (16.0, 410.0, -0.55, 1.3),
                    (9.0, 190.0, 0.9, 2.0),
                    (2.5, 55.0, -1.7, 0.5),
                ],
                height: 190.0,
                strength: 1.0,
                tint: 0.0,
                span: (-200.0, 2000.0),
                pulse: 0.6,
                seed: 11,
            },
            Ribbon {
                base: 215.0,
                waves: [
                    (26.0, 1200.0, -0.24, 2.2),
                    (12.0, 470.0, 0.48, 0.4),
                    (7.0, 210.0, -0.8, 1.0),
                    (2.0, 65.0, 1.5, 2.6),
                ],
                height: 150.0,
                strength: 0.6,
                tint: 0.3,
                span: (250.0, 1450.0),
                pulse: 2.5,
                seed: 23,
            },
            Ribbon {
                base: 385.0,
                waves: [
                    (20.0, 760.0, 0.42, 4.0),
                    (9.0, 300.0, -0.75, 0.7),
                    (5.0, 150.0, 1.1, 3.1),
                    (2.0, 50.0, -1.4, 1.2),
                ],
                height: 120.0,
                strength: 0.45,
                tint: 0.1,
                span: (-100.0, 820.0),
                pulse: 4.0,
                seed: 37,
            },
        ];
        let ramp = [
            (0.0, hex(0xC2FFE6)),
            (0.06, hex(0x5CFFA8)),
            (0.3, hex(0x31F08A)),
            (0.55, hex(0x1FC7A0)),
            (0.8, hex(0x3F7BEA)),
            (1.05, hex(0x8C52F2)),
            (1.35, hex(0xC048D8)),
            (RAMP_TOP, hex(0xD04AB8)),
        ];
        let colours = (0..RAMP).map(|i| gradient(&ramp, i as f32 / (RAMP - 1) as f32 * RAMP_TOP)).collect();
        Aurora { sky, far, snow, near, stars, ribbons, colours }
    }

    /// The light's colour at height `s` (0 at the lower border).
    fn colour(&self, s: f32) -> Rgb {
        let i = (s.clamp(0.0, RAMP_TOP) / RAMP_TOP * (RAMP - 1) as f32) as usize;
        self.colours[i]
    }

    /// The picture of `frame`, as `0xFFRRGGBB` pixels.
    pub fn picture(&self, frame: u32) -> Vec<u32> {
        let t = frame as f32 / FPS as f32;
        let p = pan(t);
        let mut c = Canvas::new(WIDTH, HEIGHT);
        let mut raster = Rasterizer::new();

        // The sky, through the camera's window.
        let (ip, fp) = (p.floor() as usize, p - p.floor());
        for y in 0..SHORE {
            let src = &self.sky.px[y * self.sky.w..(y + 1) * self.sky.w];
            for x in 0..WIDTH {
                c.px[y * WIDTH + x] = mix(src[x + ip], src[x + ip + 1], fp);
            }
        }
        for s in &self.stars {
            let x = s.x - p;
            if x < -4.0 || x > WIDTH as f32 + 4.0 {
                continue;
            }
            let twinkle = 0.62 + 0.38 * (TAU * s.rate * t + s.phase).sin();
            c.splat(x, s.y, s.sigma, scale(s.color, twinkle));
        }
        self.meteor(&mut c, &mut raster, t);
        for r in &self.ribbons {
            self.ribbon(&mut c, &mut raster, r, t, p);
        }
        self.mountains(&mut c, &mut raster, p);
        lake(&mut c, t);

        for (i, px) in c.px.iter_mut().enumerate() {
            let (x, y) = ((i % WIDTH) as f32 / WIDTH as f32 - 0.5, (i / WIDTH) as f32 / HEIGHT as f32 - 0.5);
            let k = 1.0 - 0.32 * smoothstep(0.35, 0.95, (x * x + y * y * 0.6).sqrt() * 1.25);
            *px = tonemap(scale(*px, k));
        }
        c.to_pixels()
    }

    /// One curtain: its lower border, its rays and its glow.
    fn ribbon(&self, c: &mut Canvas, raster: &mut Rasterizer, r: &Ribbon, t: f32, p: f32) {
        let n = WIDTH + 1;
        // Per column: the border, the reach of the rays, the brightness.
        let mut edge = Vec::with_capacity(n);
        let mut reach = Vec::with_capacity(n);
        let mut bright = Vec::with_capacity(n);
        let pulse = 0.8 + 0.2 * (0.45 * t + r.pulse).sin();
        for x in 0..n {
            let xw = x as f32 + p;
            edge.push(r.edge(xw, t));
            reach.push(r.height * (0.65 + 0.35 * noise1(xw / 150.0 + t * 0.25, r.seed + 1)));
            let fade = smoothstep(r.span.0, r.span.0 + 300.0, xw) * smoothstep(r.span.1, r.span.1 - 300.0, xw);
            // Bright patches and faint stretches drifting along the curtain.
            let along = smoothstep(-0.35, 0.45, fbm1(xw / 260.0 - t * 0.12, 3, r.seed + 2));
            bright.push(r.strength * fade * pulse * (0.06 + 0.94 * along));
        }
        // Rays: fine vertical streaks drifting along the curtain; folds
        // (where the border is steep) glow brighter.
        let rays: Vec<f32> = (0..n)
            .map(|x| {
                let xw = x as f32 + p;
                let drift = t * 1.2 + 1.5 * (t * 0.3 + xw * 0.002).sin();
                let fold = ((edge[(x + 1).min(n - 1)] - edge[x.saturating_sub(1)]) * 0.5).abs().min(3.0);
                let streak = smoothstep(0.3, 0.9, 0.5 + 0.5 * fbm1(xw * 0.075 + drift, 3, r.seed + 3));
                (0.3 + 0.7 * streak) * (1.0 + 0.3 * fold)
            })
            .collect();
        // Bright rays reach higher.
        let tops: Vec<f32> = reach.iter().zip(&rays).map(|(h, ray)| h * (0.45 + 0.7 * ray)).collect();
        let mut path = Path::new();
        path.move_to(0.0, edge[0] + 40.0);
        for (x, &e) in edge.iter().enumerate().step_by(4) {
            path.line_to(x as f32, e + 40.0);
        }
        path.line_to(WIDTH as f32, edge[n - 1] + 40.0);
        let top = |x: usize| edge[x] - 1.4 * tops[x];
        path.line_to(WIDTH as f32, top(n - 1));
        for x in (0..n).rev().step_by(2) {
            path.line_to(x as f32, top(x));
        }
        path.close();
        let pink = hex(0xFF6FB8);
        c.fill_path(raster, &path, &Transform::IDENTITY, Blend::Add, |x, y| {
            let dv = edge[x] - (y as f32 + 0.5);
            let (b, ray) = (bright[x], rays[x]);
            let glow = 0.035 * (-dv.abs() / 12.0).exp();
            let line = 0.45 * (0.35 + 0.65 * ray);
            if dv < 0.0 {
                // Below the border: a sharp fall, a faint glow, a pink fringe.
                let k = line * (dv / 2.2).exp() + glow;
                (mix(self.colour(0.0), pink, smoothstep(-1.0, -5.0, dv) * 0.6), k * b)
            } else {
                let s = dv / tops[x];
                let onset = 1.0 - (-dv / 2.5).exp();
                let core = (-dv / 24.0).exp() * (0.5 + 0.5 * ray);
                let tail = (-1.5 * s).exp() * smoothstep(1.4, 0.9, s) * ray;
                let k = line * (-dv / 4.0).exp() + glow + onset * (0.85 * core + 0.6 * tail);
                (self.colour(s + r.tint), k * b)
            }
        });
    }

    /// A meteor streaking across the upper sky.
    fn meteor(&self, c: &mut Canvas, raster: &mut Rasterizer, t: f32) {
        let (start, length) = (9.1, 0.45);
        let q = (t - start) / length;
        if !(0.0..=1.0).contains(&q) {
            return;
        }
        let (x0, y0, x1, y1) = (980.0, 60.0, 640.0, 230.0);
        let (hx, hy) = (x0 + (x1 - x0) * q, y0 + (y1 - y0) * q);
        let (dx, dy) = ((x1 - x0) / 380.0, (y1 - y0) / 380.0);
        let tail = 150.0 * q.min(0.6) / 0.6;
        let (tx, ty) = (hx - dx * tail, hy - dy * tail);
        let (nx, ny) = (-dy * 1.3, dx * 1.3);
        let mut path = Path::new();
        path.move_to(hx + nx, hy + ny);
        path.line_to(hx - nx, hy - ny);
        path.line_to(tx, ty);
        path.close();
        let fade = smoothstep(0.0, 0.15, q) * smoothstep(1.0, 0.7, q);
        c.fill_path(raster, &path, &Transform::IDENTITY, Blend::Add, |x, y| {
            let along = ((x as f32 - tx) * dx + (y as f32 - ty) * dy) / tail.max(1.0);
            (hex(0xE8F4FF), along.clamp(0.0, 1.0).powi(2) * fade * 0.9)
        });
        c.splat(hx, hy, 1.6, scale(hex(0xE8F4FF), 0.5 * fade));
    }

    /// The far range (with snow catching the aurora's light) and the near hills.
    fn mountains(&self, c: &mut Canvas, raster: &mut Rasterizer, p: f32) {
        let at = |v: &[f32], x: f32| {
            let i = x.floor() as usize;
            let f = x - x.floor();
            v[i.min(v.len() - 2)] * (1.0 - f) + v[(i + 1).min(v.len() - 1)] * f
        };
        let shore = SHORE as f32;
        for (heights, offset, near) in [(&self.far, p, false), (&self.near, p * NEAR, true)] {
            let mut path = Path::new();
            path.move_to(0.0, shore);
            for x in (0..=WIDTH).step_by(if near { 1 } else { 2 }) {
                path.line_to(x as f32, at(heights, x as f32 + offset));
            }
            path.line_to(WIDTH as f32, shore);
            path.close();
            let (ip, fp) = (p.floor() as usize, p - p.floor());
            c.fill_path(raster, &path, &Transform::IDENTITY, Blend::Over, |x, y| {
                let ridge = at(heights, x as f32 + 0.5 + offset);
                let below = (y as f32 + 0.5 - ridge).max(0.0);
                if near {
                    return (mix(hex(0x020408), hex(0x04070D), smoothstep(0.0, 40.0, below)), 1.0);
                }
                let k = smoothstep(0.0, shore - ridge, below);
                let mut col = mix(hex(0x060A13), hex(0x0B1424), k);
                // The aurora's light on the snow, and along the ridge.
                let row = &self.snow.px[y * self.snow.w..];
                let snow = row[x + ip][0] * (1.0 - fp) + row[x + ip + 1][0] * fp;
                col = add(col, scale(hex(0x6FA8A0), snow * 0.16));
                col = add(col, scale(hex(0x2A6E5E), 0.12 * (-below / 3.0).exp()));
                (col, 1.0)
            });
        }
    }
}

fn star_colour(rng: &mut Pcg32) -> Rgb {
    match rng.random::<f32>() {
        v if v < 0.3 => [0.78, 0.86, 1.0],
        v if v < 0.42 => [1.0, 0.9, 0.78],
        _ => [1.0, 1.0, 1.0],
    }
}

/// The lake: the scene above mirrored through ripples, darker towards the
/// viewer.
fn lake(c: &mut Canvas, t: f32) {
    let (above, below) = c.px.split_at_mut(SHORE * WIDTH);
    let sample = |x: f32, y: f32| {
        let x = (x - 0.5).clamp(0.0, (WIDTH - 1) as f32);
        let y = (y - 0.5).clamp(0.0, (SHORE - 1) as f32);
        let (x0, y0) = (x as usize, y as usize);
        let (x1, y1) = ((x0 + 1).min(WIDTH - 1), (y0 + 1).min(SHORE - 1));
        let (fx, fy) = (x - x0 as f32, y - y0 as f32);
        let top = mix(above[y0 * WIDTH + x0], above[y0 * WIDTH + x1], fx);
        let bottom = mix(above[y1 * WIDTH + x0], above[y1 * WIDTH + x1], fx);
        mix(top, bottom, fy)
    };
    let water = hex(0x02050A);
    for (i, px) in below.iter_mut().enumerate() {
        let (x, y) = ((i % WIDTH) as f32 + 0.5, (SHORE + i / WIDTH) as f32 + 0.5);
        let q = (y - SHORE as f32) / (HEIGHT - SHORE) as f32;
        let swell = 0.8 * (x * 0.011 + t * 0.5).sin();
        let ripple = (0.25 + 1.4 * q)
            * (0.6 * (y * 0.47 + t * 1.9 + swell).sin() + 0.4 * (y * 1.31 - t * 2.7 + x * 0.0071).sin());
        let wave = 0.3 * q * (y * 0.83 + t * 1.3).sin();
        let mirrored = 2.0 * SHORE as f32 - y;
        let reflect = 0.7 - 0.28 * q;
        // Two samples a little apart soften the reflection.
        let seen = mix(sample(x + ripple - 0.7, mirrored + wave), sample(x + ripple + 0.7, mirrored + wave + 0.8), 0.5);
        *px = mix(water, seen, reflect);
    }
}

/// A soft, slowly evolving pad.
fn pad() -> Analog {
    let mut a = Analog::new(Wave::Triangle, Adsr::new(2.4, 2.0, 0.9, 3.0));
    a.oscs = vec![OscSpec::new(Wave::Triangle, 0.9, 0.0), OscSpec::new(Wave::Sine, 0.4, 12.0)];
    a.unison = 3;
    a.detune = 14.0;
    a.width = 0.35;
    a.filter = Some(FilterSpec {
        mode: Mode::LowPass,
        cutoff: 1300.0,
        q: 0.6,
        env_octaves: 0.8,
        env: Adsr::new(3.0, 3.0, 0.5, 2.0),
        keytrack: 0.2,
        vel_octaves: 0.5,
    });
    a.vibrato = (0.25, 0.04, 1.0);
    a.gain = 0.28;
    a.vel_sens = 0.2;
    a
}

/// A soft, glassy bell.
fn bell() -> Fm {
    Fm {
        ratio: 3.0,
        index: 1.6,
        index_decay: 1.2,
        index_sustain: 0.1,
        tine_ratio: 7.0,
        tine: 0.15,
        amp: Adsr::new(0.004, 2.6, 0.0, 1.3),
        vel_index: 0.5,
        detune: 3.0,
        tremolo: (0.0, 0.0),
        gain: 0.2,
    }
}

/// The soundtrack: interleaved stereo at 48 kHz, exactly [`SECONDS`] long,
/// fading in and out.
pub fn sound() -> Vec<i16> {
    // A beat a second.
    let mut song = Song::new(60.0, RATE as f32, SECONDS as f64);
    song.tail = 0.0;
    song.reverb = (0.92, 0.45, 0.6, 0.035);
    song.reverb_return = 1.0;
    song.delay_beats = 0.75;
    song.delay_feedback = 0.4;
    song.fade_in = 3.0;
    song.fade_out = 4.0;
    song.master.target_rms_db = -21.0;
    song.master.comp_threshold = -24.0;
    song.master.comp_ratio = 1.5;
    song.master.high_shelf = (6000.0, -4.0);

    let mut drone = Analog::new(Wave::Sine, Adsr::new(4.0, 2.0, 1.0, 4.0));
    drone.oscs = vec![OscSpec::new(Wave::Sine, 0.8, 0.0), OscSpec::new(Wave::Sine, 0.25, 7.0)];
    drone.gain = 0.4;
    let mut drone = Part::new("drone", Box::new(drone));
    drone.note(0.0, 16.0, 38.0, 0.8);
    drone.reverb = 0.25;
    drone.gain = 0.7;

    let mut pads = Part::new("pad", Box::new(pad()));
    for (i, chord) in ["Dmaj9", "Bm9", "Gmaj9", "Asus4"].iter().enumerate() {
        for p in theory::voiced(chord, 62.0) {
            pads.note(i as f64 * 4.0, 4.4, p, 0.7);
        }
    }
    pads.chorus = Some((0.2, 3.5, 0.2));
    pads.reverb = 0.5;
    pads.gain = 0.9;

    let mut bells = Part::new("bells", Box::new(bell()));
    let melody = [
        (1.5, "F#5", 0.55),
        (3.0, "A5", 0.5),
        (5.5, "B5", 0.5),
        (7.0, "F#5", 0.45),
        (9.5, "D6", 0.5),
        (11.0, "B5", 0.45),
        (12.5, "A5", 0.5),
        (14.0, "E5", 0.4),
    ];
    for (beat, name, vel) in melody {
        let pitch = theory::note(name).expect("a note name");
        bells.note(beat, 2.0, pitch, vel);
    }
    bells.pan = -0.15;
    bells.reverb = 0.6;
    bells.delay = 0.3;
    bells.gain = 1.1;

    song.parts = vec![drone, pads, bells];
    let (mix, _) = synth::render(&song);
    // Rounded without dither: the fades end in true silence.
    mix.iter().map(|v| (v * 32767.0).round().clamp(-32768.0, 32767.0) as i16).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pan_eases_from_start_to_end() {
        assert_eq!(pan(0.0), 0.0);
        assert!((pan(SECONDS as f32) - PAN).abs() < 1e-3);
        let mut last = 0.0;
        for f in 1..=FRAMES {
            let p = pan(f as f32 / FPS as f32);
            assert!(p > last && p - last < 2.5, "frame {f}");
            last = p;
        }
    }

    #[test]
    fn the_soundtrack_is_16_seconds_and_fades() {
        let pcm = sound();
        assert_eq!(pcm.len(), 2 * 16 * 48_000);
        let peak = pcm.iter().map(|s| s.unsigned_abs()).max().unwrap();
        assert!(peak > 3000, "audible: {peak}");
        assert!(pcm[..200].iter().all(|&s| s.unsigned_abs() < 50), "fades in");
        assert!(pcm[pcm.len() - 200..].iter().all(|&s| s.unsigned_abs() < 50), "fades out");
    }
}
