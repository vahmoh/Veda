// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The sound: a quiet 440 Hz tone, and a click (10 ms of 1 kHz, louder)
//! from every whole second of media time, when the picture's marker is at
//! its left edge. Both channels alike, at the output's rate; the phase
//! follows the media time, so the sound after a seek is what playing would
//! have reached.

use alloc::vec::Vec;

/// Peaks of the tone and of the click.
const TONE: i64 = 3_000;
const CLICK: i64 = 12_000;

/// Appends frames `first ..` (`frames` of them) of the sound at `rate` to
/// `out`, interleaved stereo.
pub fn fill(out: &mut Vec<i16>, first: u64, frames: u64, rate: u32) {
    let rate = rate.max(1) as u64;
    let click = rate / 100;
    for f in first..first.saturating_add(frames) {
        let since = f % rate;
        let v = if since < click { level(CLICK, phase(since, 1_000, rate)) } else { level(TONE, phase(f, 440, rate)) };
        out.push(v);
        out.push(v);
    }
}

/// The phase (2³² to a turn) of a tone of `hz` at frame `f` of sound at
/// `rate`: whole turns on whole seconds.
fn phase(f: u64, hz: u64, rate: u64) -> u32 {
    (((f.wrapping_mul(hz) % rate) << 32) / rate) as u32
}

/// `peak` times the sine of `phase`.
fn level(peak: i64, phase: u32) -> i16 {
    ((sine(phase) * peak) >> 30) as i16
}

/// The sine of `phase` (2³² to a turn) in 2.30 fixed point, from its
/// Taylor series on a quarter turn (within 4e-6).
fn sine(phase: u32) -> i64 {
    // (π/2)ⁿ/n!, alternating, in 2.30.
    const C1: i64 = 1_686_629_713;
    const C3: i64 = -693_598_668;
    const C5: i64 = 85_569_306;
    const C7: i64 = -5_026_995;
    const C9: i64 = 172_272;
    let quadrant = phase >> 30;
    // The way into the quarter turn, 0 ..= 1 in 2.30.
    let mut x = (phase & 0x3FFF_FFFF) as i64;
    if quadrant & 1 == 1 {
        x = (1 << 30) - x;
    }
    let x2 = (x * x) >> 30;
    let p = C7 + ((C9 * x2) >> 30);
    let p = C5 + ((p * x2) >> 30);
    let p = C3 + ((p * x2) >> 30);
    let p = C1 + ((p * x2) >> 30);
    let s = (p * x) >> 30;
    if quadrant >= 2 { -s } else { s }
}
