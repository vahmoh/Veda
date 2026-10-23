// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Interceptor's game: everything but the window, the keyboard and the
//! speakers, so that it can be tested on the host.
//!
//! | Module | Contents |
//! |--------|----------|
//! | [`layout`] | Veda Bay's control points |
//! | [`road`] | the loop's centre line: frames, cross-sections, where a point is on it |
//! | [`terrain`] | the land and the sea around the road |
//! | [`city`] | the buildings |
//! | [`lamps`] | the street lights |
//! | [`scenery`] | the trees, and the harbour bridge's towers and cables |
//! | [`world`] | all of Veda Bay, made at start-up |
//! | [`render`] | the pictures, with OpenGL ES 3.0 |
//! | [`env`](mod@env) | the time of day and the weather |
//! | [`mesh`] | meshes and the shapes they are made of |
//! | [`roadmesh`], [`groundmesh`] | the road's, the ground's and the sea's meshes |
//! | [`texgen`] | textures painted at start-up |
//! | [`font`] | a stroke font for the signs |
//! | [`carmodel`] | the cars' shapes |
//! | [`vehicle`], [`physics`] | how cars drive and crash |
//! | [`camera`] | the cameras that follow a car |
//! | [`input`] | the keyboard into a driver's controls |
//! | [`driver`], [`traffic`] | the computer's drivers, and the traffic round the player |
//! | [`game`] | the running game |
//! | [`effects`] | tyre smoke, spray, sparks and skid marks |
//! | [`pursuit`] | a chase: takedowns, busts, escapes |
//! | [`career`] | the missions, their scores and stars, and what is saved |
//! | [`session`] | a mission, a quick chase, a patrol or the menus' showcase, start to end |
//! | [`scale`] | the resolution the software renderer renders at |
//! | [`audio`] | the sounds: engines, sirens, tyres, crashes, the cues, the music |
//! | [`sound`] | what the game sounds like, frame by frame |

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod audio;
pub mod camera;
pub mod career;
pub mod carmodel;
pub mod city;
pub mod driver;
pub mod effects;
pub mod env;
pub mod font;
pub mod game;
pub mod groundmesh;
pub mod input;
pub mod lamps;
pub mod layout;
pub mod mesh;
pub mod physics;
pub mod pursuit;
pub mod render;
pub mod road;
pub mod roadmesh;
pub mod scale;
pub mod scenery;
pub mod session;
pub mod sound;
pub mod terrain;
pub mod texgen;
pub mod traffic;
pub mod vehicle;
pub mod world;

#[cfg(test)]
mod tests;

/// What most modules use: glam's vector types, their swizzles (`v.xz()`)
/// and its `FloatExt` (`lerp`, `smoothstep`); [`Exact`](prelude::Exact),
/// the float methods `core` lacks whose results are exact; and num-traits'
/// `Euclid` (`Euclid::rem_euclid(&x, &y)` is called as a function: its
/// signature is not std's).
///
/// The game must play the same in the host's tests as in Veda, and a
/// chase magnifies the last bit of any difference into another chase. So
/// the functions whose results are not exact (`sin`, `atan2`, `exp`,
/// `powf`...) are called from libm by name on every target
/// (`libm::sinf(x)`), never as methods: where `std` is linked (the tests)
/// its own methods, which are its C library's, would take precedence over
/// any trait's. None is in scope as a method here, so such a call does not
/// build for Veda. Where `std` is linked the methods of `Exact` are std's,
/// which give the same results.
mod prelude {
    pub use glam::{FloatExt, Mat4, Quat, Vec2, Vec3, Vec3Swizzles, Vec4, Vec4Swizzles};
    pub use num_traits::Euclid;

    /// The float methods `core` lacks whose results are exact (rounded
    /// correctly, or exactly representable), so the same whoever computes
    /// them; and `powi`, multiplications in the same order everywhere.
    #[allow(dead_code, reason = "where std is linked its own methods, the same, are called instead")]
    pub trait Exact {
        fn sqrt(self) -> Self;
        fn floor(self) -> Self;
        fn ceil(self) -> Self;
        fn round(self) -> Self;
        fn powi(self, n: i32) -> Self;
    }

    impl Exact for f32 {
        #[inline]
        fn sqrt(self) -> f32 {
            libm::sqrtf(self)
        }

        #[inline]
        fn floor(self) -> f32 {
            libm::floorf(self)
        }

        #[inline]
        fn ceil(self) -> f32 {
            libm::ceilf(self)
        }

        #[inline]
        fn round(self) -> f32 {
            libm::roundf(self)
        }

        #[inline]
        fn powi(self, n: i32) -> f32 {
            num_traits::float::FloatCore::powi(self, n)
        }
    }
}
