// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `music-core` — what Music decides, apart from what it draws and whom it
//! talks to, so that it can be tested on the host.
//!
//! * [`order`]: the order tracks play in (shuffled or not), what comes
//!   before and after a track, and what plays when one ends.
//! * [`cover`]: what a track's generated cover is (its style, hue and
//!   seed), told by a key that compares, and the few covers kept drawn.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod cover;
pub mod order;
