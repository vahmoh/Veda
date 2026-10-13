// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The time of day and the weather: the sun or the moon, the sky, the
//! ambient light, the fog, how wet the roads are and whether the street
//! lights are on.
//!
//! Light is linear and relative: a white surface facing the sun reflects
//! the sun's colour; the exposure scales everything before tone mapping.

use crate::prelude::*;
use crate::render::post::Grade;

/// When a chase happens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeOfDay {
    Day,
    Sunset,
    Night,
}

/// What the sky is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Weather {
    Clear,
    Rain,
}

/// How the sky's cube map is painted.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SkyLook {
    pub zenith: Vec3,
    pub horizon: Vec3,
    /// Around the sun.
    pub glow: Vec3,
    /// The sun's or the moon's disc.
    pub disc: Vec3,
    pub clouds: Vec3,
    /// 0 (clear) to 1 (overcast).
    pub cloud_cover: f32,
    pub stars: f32,
    /// The city's glow low in the north.
    pub city: Vec3,
}

/// Everything about the light of a moment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Environment {
    pub time: TimeOfDay,
    pub weather: Weather,
    /// Towards the sun (or the moon at night).
    pub sun_dir: Vec3,
    pub sun_color: Vec3,
    /// How dark the sun's shadows are (0 none, 1 full).
    pub shadows: f32,
    pub sky_ambient: Vec3,
    pub ground_ambient: Vec3,
    pub fog_color: Vec3,
    /// Fog per metre at sea level, how fast it thins with height, how much
    /// it glows towards the sun, and the most it covers.
    pub fog_density: f32,
    pub fog_falloff: f32,
    pub fog_glow: f32,
    pub fog_max: f32,
    /// 0 dry to 1 soaked.
    pub wetness: f32,
    /// How hard it rains (0..1).
    pub rain: f32,
    /// The street lights' brightness (0 off).
    pub street_lights: f32,
    /// How strongly surfaces reflect the sky.
    pub reflections: f32,
    pub exposure: f32,
    pub sky: SkyLook,
}

impl Environment {
    /// How the frame is graded after it is drawn.
    pub fn grade(&self) -> Grade {
        let mut g = match self.time {
            TimeOfDay::Day => Grade {
                exposure: self.exposure,
                bloom: 0.1,
                saturation: 1.1,
                contrast: 1.05,
                lift: [0.002, 0.003, 0.006],
                gain: [1.02, 1.0, 0.97],
                vignette: 0.22,
            },
            TimeOfDay::Sunset => Grade {
                exposure: self.exposure,
                bloom: 0.18,
                saturation: 1.15,
                contrast: 1.06,
                lift: [0.004, 0.002, 0.006],
                gain: [1.08, 0.99, 0.88],
                vignette: 0.28,
            },
            TimeOfDay::Night => Grade {
                exposure: self.exposure,
                bloom: 0.3,
                saturation: 1.12,
                contrast: 1.08,
                lift: [0.003, 0.005, 0.012],
                gain: [1.0, 0.98, 1.04],
                vignette: 0.32,
            },
        };
        if self.weather == Weather::Rain {
            g.saturation *= 0.85;
            g.bloom *= 1.3;
        }
        g
    }

    /// The look of `time` in `weather`.
    pub fn new(time: TimeOfDay, weather: Weather) -> Environment {
        let mut e = match time {
            TimeOfDay::Day => Environment {
                time,
                weather,
                sun_dir: Vec3::new(-0.38, 0.83, 0.41).normalize(),
                sun_color: Vec3::new(3.1, 2.95, 2.7),
                shadows: 1.0,
                sky_ambient: Vec3::new(0.56, 0.63, 0.76),
                ground_ambient: Vec3::new(0.24, 0.22, 0.18),
                fog_color: Vec3::new(0.42, 0.55, 0.75),
                fog_density: 0.00022,
                fog_falloff: 0.006,
                fog_glow: 0.3,
                fog_max: 0.9,
                wetness: 0.0,
                rain: 0.0,
                street_lights: 0.0,
                reflections: 1.0,
                exposure: 0.5,
                sky: SkyLook {
                    zenith: Vec3::new(0.05, 0.17, 0.55),
                    horizon: Vec3::new(0.52, 0.66, 0.86),
                    glow: Vec3::new(1.0, 0.9, 0.7),
                    disc: Vec3::new(40.0, 36.0, 30.0),
                    clouds: Vec3::new(0.95, 0.96, 1.0),
                    cloud_cover: 0.35,
                    stars: 0.0,
                    city: Vec3::ZERO,
                },
            },
            TimeOfDay::Sunset => Environment {
                time,
                weather,
                sun_dir: Vec3::new(-0.80, 0.12, 0.58).normalize(),
                sun_color: Vec3::new(2.9, 1.55, 0.72),
                shadows: 1.0,
                sky_ambient: Vec3::new(0.30, 0.28, 0.42),
                ground_ambient: Vec3::new(0.14, 0.09, 0.07),
                fog_color: Vec3::new(0.70, 0.46, 0.34),
                fog_density: 0.00035,
                fog_falloff: 0.008,
                fog_glow: 1.2,
                fog_max: 0.92,
                wetness: 0.0,
                rain: 0.0,
                street_lights: 0.35,
                reflections: 1.0,
                exposure: 0.85,
                sky: SkyLook {
                    zenith: Vec3::new(0.10, 0.12, 0.30),
                    horizon: Vec3::new(1.05, 0.55, 0.30),
                    glow: Vec3::new(1.6, 0.75, 0.28),
                    disc: Vec3::new(30.0, 16.0, 6.0),
                    clouds: Vec3::new(0.85, 0.48, 0.38),
                    cloud_cover: 0.45,
                    stars: 0.0,
                    city: Vec3::new(0.05, 0.03, 0.02),
                },
            },
            TimeOfDay::Night => Environment {
                time,
                weather,
                sun_dir: Vec3::new(0.35, 0.62, -0.45).normalize(),
                sun_color: Vec3::new(0.018, 0.022, 0.034),
                shadows: 0.5,
                sky_ambient: Vec3::new(0.012, 0.015, 0.026),
                ground_ambient: Vec3::new(0.010, 0.008, 0.007),
                fog_color: Vec3::new(0.022, 0.026, 0.042),
                fog_density: 0.0005,
                fog_falloff: 0.008,
                fog_glow: 0.4,
                fog_max: 0.95,
                wetness: 0.0,
                rain: 0.0,
                street_lights: 1.0,
                reflections: 1.0,
                exposure: 2.6,
                sky: SkyLook {
                    zenith: Vec3::new(0.004, 0.007, 0.018),
                    horizon: Vec3::new(0.025, 0.032, 0.055),
                    glow: Vec3::new(0.05, 0.06, 0.08),
                    disc: Vec3::new(2.5, 2.6, 2.8),
                    clouds: Vec3::new(0.03, 0.035, 0.05),
                    cloud_cover: 0.25,
                    stars: 0.6,
                    city: Vec3::new(0.09, 0.05, 0.025),
                },
            },
        };
        if weather == Weather::Rain {
            e.wetness = 1.0;
            e.rain = 1.0;
            e.shadows *= 0.3;
            e.sun_color *= 0.35;
            e.sky.cloud_cover = 0.95;
            e.sky.clouds *= 0.55;
            e.sky.zenith = e.sky.zenith.lerp(e.sky.horizon * 0.6, 0.6);
            e.sky.stars = 0.0;
            e.sky.disc *= 0.0;
            e.fog_density *= 2.5;
            e.fog_color = e.fog_color.lerp(e.sky.horizon * 0.7, 0.5);
            e.sky_ambient *= 0.8;
            e.street_lights = e.street_lights.max(0.6);
        }
        e
    }
}
