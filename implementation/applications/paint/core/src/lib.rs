// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `paint-core` — what Paint does to pictures, apart from its window, so that it can be
//! tested on the host.
//!
//! * [`document`]: a picture being edited: its history, its selection and the edit under way.
//! * [`history`]: undo and redo, in tiles.
//! * [`brush`]: strokes of the pencil, the brush, the airbrush and the eraser.
//! * [`fill`]: filling an area of one colour.
//! * [`shapes`]: lines, rectangles, ellipses, polygons, stars, hearts and arrows.
//! * [`text`]: a box of text on the picture.
//! * [`transform`] and [`effects`]: turning, mirroring, cropping and resizing; colour effects,
//!   blurring and sharpening.
//! * [`color`]: colours by hue, number and name, the palette, pixel arithmetic.
//! * [`view`] and [`render`]: where the picture is in its window, and drawing it there.
//! * [`codec`]: pictures from and to files.
//! * [`settings`]: the tools, their settings and the colours, and what Paint remembers of
//!   them.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod brush;
pub mod codec;
pub mod color;
pub mod document;
pub mod effects;
pub mod fill;
pub mod history;
pub mod render;
pub mod settings;
pub mod shapes;
pub mod text;
pub mod transform;
pub mod view;

pub use document::Document;
