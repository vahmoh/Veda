// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Symbols that compiled Rust code expects from a C runtime, whatever the
//! architecture: the floating-point remainders LLVM emits calls to (for
//! `%` on `f32` and `f64`). The rest are the architecture's
//! (`architecture`).

/// The exact remainder, as C's `fmodf` (see [`fmod`]).
#[unsafe(no_mangle)]
pub extern "C" fn fmodf(x: f32, y: f32) -> f32 {
    libm::fmodf(x, y)
}

/// The exact remainder, as C's `fmod` (libm's, which computes it on the
/// numbers' integer bits: a float remainder in it would call this function
/// again).
#[unsafe(no_mangle)]
pub extern "C" fn fmod(x: f64, y: f64) -> f64 {
    libm::fmod(x, y)
}
