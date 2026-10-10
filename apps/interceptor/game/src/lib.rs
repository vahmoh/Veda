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
//! | [`world`] | all of Veda Bay, made at start-up |
//! | [`render`] | the pictures, with OpenGL ES 3.0 |
//! | [`env`] | the time of day and the weather |
//! | [`mesh`] | meshes and the shapes they are made of |
//! | [`roadmesh`], [`groundmesh`] | the road's, the ground's and the sea's meshes |
//! | [`texgen`] | textures painted at start-up |
//! | [`carmodel`] | the cars' shapes |
//! | [`vehicle`], [`physics`] | how cars drive and crash |
//! | [`camera`] | the cameras that follow a car |
//! | [`input`] | the keyboard into a driver's controls |
//! | [`driver`], [`traffic`] | the computer's drivers, and the traffic round the player |
//! | [`game`] | the running game |
//! | [`pursuit`] | a chase: takedowns, busts, escapes |
//! | [`scale`] | the resolution the scene is rendered at |
//! | [`audio`] | the sounds: engines, sirens, tyres, crashes, the cues, the music |
//! | [`sound`] | what the game sounds like, frame by frame |

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod audio;
pub mod camera;
pub mod carmodel;
pub mod city;
pub mod driver;
pub mod env;
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
pub mod sound;
pub mod terrain;
pub mod texgen;
pub mod traffic;
pub mod vehicle;
pub mod world;

#[cfg(test)]
mod tests;

/// What most modules use: the vector types and `FloatExt`, whose float
/// methods (`x.sin()`) `core` lacks. Where `std` is linked (the tests, and
/// builds alongside the tests' dependencies) its own methods take
/// precedence, so a glob keeps the import from being reported unused.
mod prelude {
    pub use vmath::{FloatExt, Mat4, Quat, Vec2, Vec3, Vec4};
}
