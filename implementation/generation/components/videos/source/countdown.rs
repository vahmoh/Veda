// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! "Countdown": an A/V sync test, 640x360 at 30 fps, exactly 12 s.
//!
//! The first second is 75% colour bars. Then the numbers 10 to 1 count
//! down, a second each, under a hand sweeping round once a second, and the
//! last second is a title card. The frame that starts at every whole second
//! from 1 s to 11 s is all white, and a 50 ms beep at 1 kHz starts on the
//! very same sample (48000 k); the sound is silent otherwise. A timecode
//! (hours, minutes, seconds, frames) runs along the bottom.

use std::f32::consts::{FRAC_PI_2, TAU};

use font::Font;
use gfx::{Canvas, Color, FillRule, Path, Rect, StrokeStyle};

use crate::paint::text_at;

pub const WIDTH: usize = 640;
pub const HEIGHT: usize = 360;
pub const FPS: u32 = 30;
pub const SECONDS: u32 = 12;
pub const FRAMES: u32 = FPS * SECONDS;
const RATE: u32 = crate::RATE;

/// The beep: 50 ms of 1 kHz at -12 dBFS.
const BEEP_SAMPLES: usize = RATE as usize / 20;
const BEEP_HZ: f64 = 1000.0;
const BEEP_LEVEL: f64 = 0.25;

/// The 75% bars, left to right.
const BARS: [u32; 8] = [0xBFBFBF, 0xBFBF00, 0x00BFBF, 0x00BF00, 0xBF00BF, 0xBF0000, 0x0000BF, 0x000000];

const BACKGROUND: Color = Color::hex(0x10141A);
const DIM: Color = Color::hex(0x5E6773);
const GREY: Color = Color::hex(0x8A94A3);
const LIGHT: Color = Color::hex(0xF4F6F8);

/// True for the frames that are a white flash.
pub fn is_flash(frame: u32) -> bool {
    frame.is_multiple_of(FPS) && (1..SECONDS).contains(&(frame / FPS))
}

/// The picture of `frame`, as `0xFFRRGGBB` pixels.
pub fn picture(frame: u32) -> Vec<u32> {
    let mut pixels = vec![0xFF00_0000; WIDTH * HEIGHT];
    let (w, h) = (WIDTH as i32, HEIGHT as i32);
    let mut c = Canvas::new(&mut pixels, w, h, w);
    let (second, f) = (frame / FPS, frame % FPS);
    if second == 0 {
        let bar = w / BARS.len() as i32;
        for (i, &rgb) in BARS.iter().enumerate() {
            c.fill_rect(Rect::new(i as i32 * bar, 0, bar, h), Color::hex(rgb));
        }
    } else if is_flash(frame) {
        c.clear(Color::WHITE);
    } else {
        let semibold = Font::from_bytes(crate::INTER_SEMIBOLD).expect("Inter SemiBold loads");
        let regular = Font::from_bytes(crate::INTER).expect("Inter loads");
        let mono = Font::from_bytes(crate::MONO).expect("JetBrains Mono loads");
        c.clear(BACKGROUND);
        if second <= 10 {
            leader(&mut c, &semibold, 11 - second, f);
            let title = text_at(&semibold, 17.0, "Countdown", 18.0, 30.0, 0.0);
            c.fill_path(&title, GREY, FillRule::NonZero);
            let sync = text_at(&regular, 15.0, "A/V sync test", w as f32 - 18.0, 30.0, 1.0);
            c.fill_path(&sync, DIM, FillRule::NonZero);
        } else {
            let title = text_at(&semibold, 54.0, "Countdown", w as f32 / 2.0, 168.0, 0.5);
            c.fill_path(&title, LIGHT, FillRule::NonZero);
            let line = text_at(&regular, 18.0, "A flash and a beep every second", w as f32 / 2.0, 208.0, 0.5);
            c.fill_path(&line, GREY, FillRule::NonZero);
            let spec = text_at(&regular, 15.0, "30 fps \u{b7} 48 kHz \u{b7} AV1 and FLAC", w as f32 / 2.0, 236.0, 0.5);
            c.fill_path(&spec, DIM, FillRule::NonZero);
        }
        let tc = format!("00:00:{:02}:{:02}", second, f);
        let tc = text_at(&mono, 18.0, &tc, w as f32 / 2.0, h as f32 - 14.0, 0.5);
        c.fill_path(&tc, Color::hex(0xB8C2CE), FillRule::NonZero);
    }
    drop(c);
    pixels
}

/// A film leader: rings, cross hairs, a hand sweeping round once a second
/// (`f` frames in) and the number in the middle.
fn leader(c: &mut Canvas<'_>, font: &Font<'_>, number: u32, f: u32) {
    let (cx, cy) = (WIDTH as f32 / 2.0, HEIGHT as f32 / 2.0 - 4.0);
    let (outer, inner) = (138.0, 118.0);
    let lines = Color::hex(0x252C36);
    c.draw_line(0.0, cy, WIDTH as f32, cy, 2.0, lines);
    c.draw_line(cx, 0.0, cx, HEIGHT as f32, 2.0, lines);
    // The swept part of the second.
    let angle = TAU * f as f32 / FPS as f32;
    let mut pie = Path::new();
    pie.pie(cx, cy, inner, -FRAC_PI_2, angle);
    c.fill_path(&pie, Color::rgba(0x40, 0x9C, 0xFF, 70), FillRule::NonZero);
    let mut ring = Path::new();
    ring.circle(cx, cy, outer);
    c.stroke_path(&ring, &StrokeStyle::new(3.0), Color::hex(0x4A5361));
    let mut ring = Path::new();
    ring.circle(cx, cy, inner);
    c.stroke_path(&ring, &StrokeStyle::new(2.0), Color::hex(0x2E3540));
    // Twelve ticks, like a clock's.
    for i in 0..12 {
        let a = TAU * i as f32 / 12.0;
        let (s, k) = a.sin_cos();
        let (r0, r1) = (outer + 6.0, outer + if i % 3 == 0 { 16.0 } else { 11.0 });
        c.draw_line(cx + s * r0, cy - k * r0, cx + s * r1, cy - k * r1, 2.0, Color::hex(0x4A5361));
    }
    let (s, k) = (angle - FRAC_PI_2).sin_cos();
    c.draw_line(cx, cy, cx + k * outer, cy + s * outer, 3.0, Color::hex(0x8CCBFF));
    // The number, centred on its ink.
    let text = number.to_string();
    let path = text_at(font, 200.0, &text, cx, cy, 0.5);
    let b = path.bounds().expect("digits have outlines");
    let dy = cy - (b.y0 + b.y1) / 2.0;
    let dx = cx - (b.x0 + b.x1) / 2.0;
    let path = text_at(font, 200.0, &text, cx + dx, cy + dy, 0.5);
    c.fill_path(&path, LIGHT, FillRule::NonZero);
}

/// The sound: interleaved stereo at 48 kHz, exactly [`SECONDS`] long.
pub fn sound() -> Vec<i16> {
    let frames = (RATE * SECONDS) as usize;
    let mut pcm = vec![0i16; 2 * frames];
    let release = RATE as usize / 1000;
    for k in 1..SECONDS as usize {
        let start = k * RATE as usize;
        for i in 0..BEEP_SAMPLES {
            // Full level on the very first sample (a cosine), and a
            // millisecond's release at the end.
            let fade = ((BEEP_SAMPLES - i) as f64 / release as f64).min(1.0);
            let phase = std::f64::consts::TAU * BEEP_HZ * i as f64 / RATE as f64;
            let v = (phase.cos() * BEEP_LEVEL * fade * 32767.0).round() as i16;
            pcm[2 * (start + i)] = v;
            pcm[2 * (start + i) + 1] = v;
        }
    }
    pcm
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flashes_fall_on_whole_seconds() {
        let flashes: Vec<u32> = (0..FRAMES).filter(|&f| is_flash(f)).collect();
        assert_eq!(flashes, (1..=11).map(|k| k * 30).collect::<Vec<_>>());
        assert!(picture(30).iter().all(|&p| p == 0xFFFF_FFFF));
        assert!(picture(31).iter().any(|&p| p != 0xFFFF_FFFF));
    }

    #[test]
    fn the_bars_are_75_percent() {
        let p = picture(0);
        for (i, &rgb) in BARS.iter().enumerate() {
            assert_eq!(p[100 * WIDTH + i * 80 + 40], 0xFF00_0000 | rgb);
        }
    }

    #[test]
    fn beeps_start_on_the_second() {
        let pcm = sound();
        assert_eq!(pcm.len(), 2 * 576_000);
        for k in 1..12usize {
            let at = 2 * k * 48_000;
            assert_eq!(pcm[at - 2], 0, "silence before beep {k}");
            assert_eq!(pcm[at], 8192, "beep {k} starts at full level");
            assert_eq!(pcm[at + 1], 8192, "in both channels");
            assert!(pcm[at + 2 * 2399] != 0 && pcm[at + 2 * 2400] == 0, "beep {k} lasts 50 ms");
        }
        let loud = pcm.iter().filter(|&&s| s != 0).count();
        assert!(loud <= 11 * 2 * 2400, "silent otherwise");
    }
}
