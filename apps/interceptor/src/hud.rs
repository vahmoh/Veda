//! What is drawn over the frames with `vgfx`: the loading screen, the
//! dashboard (speedometer, gear, revs, siren) and the statistics.

use alloc::format;

use interceptor_game::game::Game;
use vgfx::{Canvas, Color, LineCap, LineJoin, Path, Rect, StrokeStyle, Text};
use vmath::FloatExt;

/// The fonts, by role.
pub struct Fonts {
    pub regular: usize,
    pub bold: usize,
    pub mono: usize,
    pub mono_bold: usize,
}

const WHITE: Color = Color(0xFFFFFFFF);
const DIM: Color = Color(0xB0E6ECF5);
const ACCENT: Color = Color(0xFF3D8BFF);
const RED: Color = Color(0xFFFF3B3B);

/// Draws `s` centred on `x`.
fn centered(c: &mut Canvas, text: &mut Text, font: usize, size: f32, x: f32, y: f32, s: &str, color: Color) {
    let w = text.measure(font, size, s);
    text.draw(c, font, size, x - w * 0.5, y, s, color);
}

/// The screen while Veda Bay is made.
pub fn loading(c: &mut Canvas, text: &mut Text, f: &Fonts) {
    let (w, h) = (c.width(), c.height());
    c.fill_vertical_gradient(Rect::new(0, 0, w, h), Color(0xFF0B1324), Color(0xFF020306));
    let (cx, cy) = (w as f32 * 0.5, h as f32 * 0.5);
    let s = (h as f32 / 720.0).max(0.5);
    // A light bar's glow either side of the title.
    c.fill_rounded_rect(
        Rect::new((cx - 150.0 * s) as i32, (cy - 70.0 * s) as i32, (140.0 * s) as i32, (6.0 * s) as i32),
        3.0 * s,
        RED,
    );
    c.fill_rounded_rect(
        Rect::new((cx + 10.0 * s) as i32, (cy - 70.0 * s) as i32, (140.0 * s) as i32, (6.0 * s) as i32),
        3.0 * s,
        ACCENT,
    );
    centered(c, text, f.bold, 54.0 * s, cx, cy, "INTERCEPTOR", WHITE);
    centered(c, text, f.regular, 18.0 * s, cx, cy + 34.0 * s, "Veda Bay Highway Patrol", DIM);
    centered(c, text, f.regular, 14.0 * s, cx, cy + 92.0 * s, "Building Veda Bay\u{2026}", Color(0x90E6ECF5));
}

/// An arc of the dial, from fraction `a` to `b` of its sweep.
fn dial_arc(cx: f32, cy: f32, r: f32, a: f32, b: f32) -> Path {
    let start = 0.75 * core::f32::consts::PI;
    let sweep = 1.5 * core::f32::consts::PI;
    let mut p = Path::new();
    p.arc(cx, cy, r, start + sweep * a, sweep * (b - a));
    p
}

/// The point at fraction `t` of the dial's sweep, `r` from its middle.
fn dial_point(cx: f32, cy: f32, r: f32, t: f32) -> (f32, f32) {
    let a = 0.75 * core::f32::consts::PI + 1.5 * core::f32::consts::PI * t;
    (cx + a.cos() * r, cy + a.sin() * r)
}

/// The dashboard: the speedometer at the bottom right with the gear and
/// the revs, the siren's state, and the keys at the bottom left.
pub fn dashboard(c: &mut Canvas, text: &mut Text, f: &Fonts, game: &Game) {
    let (w, h) = (c.width() as f32, c.height() as f32);
    let s = (h / 720.0).max(0.6);
    let car = &game.player().vehicle;
    let kmh = (car.speed().abs() * 3.6).round();
    let top = 280.0;
    let r = 92.0 * s;
    let (cx, cy) = (w - r - 36.0 * s, h - r - 30.0 * s);
    // The dial's face.
    c.fill_circle(cx, cy, r + 10.0 * s, Color(0x9A06090F));
    let thin = StrokeStyle { width: 6.0 * s, cap: LineCap::Round, join: LineJoin::Round, miter_limit: 4.0 };
    c.stroke_path(&dial_arc(cx, cy, r - 4.0 * s, 0.0, 1.0), &thin, Color(0x40FFFFFF));
    let t = (kmh / top).clamp(0.0, 1.0);
    let hot = if kmh > 200.0 { RED } else { ACCENT };
    if t > 0.002 {
        c.stroke_path(&dial_arc(cx, cy, r - 4.0 * s, 0.0, t), &thin, hot);
    }
    // Ticks every 20 km/h, numbered every 40.
    for k in 0..=14 {
        let v = k as f32 * 20.0;
        let tt = v / top;
        let major = k % 2 == 0;
        let (x0, y0) = dial_point(cx, cy, r - 14.0 * s, tt);
        let (x1, y1) = dial_point(cx, cy, r - if major { 24.0 } else { 19.0 } * s, tt);
        c.draw_line(x0, y0, x1, y1, if major { 2.0 * s } else { 1.2 * s }, Color(0xC0FFFFFF));
        if major && k < 14 {
            let (lx, ly) = dial_point(cx, cy, r - 38.0 * s, tt);
            centered(c, text, f.regular, 10.5 * s, lx, ly + 4.0 * s, &format!("{}", v as u32), DIM);
        }
    }
    // The revs, inside: amber, red near the limit.
    let spec = &car.spec;
    let revs = ((car.rpm - spec.rpm_idle) / (spec.rpm_red - spec.rpm_idle)).clamp(0.0, 1.0);
    let rev_style = StrokeStyle { width: 3.0 * s, cap: LineCap::Butt, join: LineJoin::Round, miter_limit: 4.0 };
    c.stroke_path(&dial_arc(cx, cy, r - 50.0 * s, 0.0, 1.0), &rev_style, Color(0x30FFFFFF));
    if revs > 0.01 {
        let color = if revs > 0.9 { RED } else { Color(0xFFFFB13D) };
        c.stroke_path(&dial_arc(cx, cy, r - 50.0 * s, 0.0, revs), &rev_style, color);
    }
    // The needle.
    let (nx, ny) = dial_point(cx, cy, r - 10.0 * s, t);
    c.draw_line(cx, cy, nx, ny, 2.5 * s, Color(0xFFFF5A3C));
    c.fill_circle(cx, cy, 6.0 * s, Color(0xFF1A1E26));
    // The speed and the gear.
    centered(c, text, f.mono_bold, 30.0 * s, cx, cy + 34.0 * s, &format!("{}", kmh as u32), WHITE);
    centered(c, text, f.regular, 11.0 * s, cx, cy + 50.0 * s, "km/h", DIM);
    let gear = match car.gear {
        g if g < 0 => alloc::string::String::from("R"),
        g => format!("{g}"),
    };
    centered(c, text, f.mono_bold, 16.0 * s, cx, cy + 74.0 * s, &gear, Color(0xFFFFB13D));

    // The siren.
    if game.player().siren {
        let blink = (game.clock * 3.2).fract() < 0.5;
        let (lx, ly) = (cx - r - 18.0 * s, cy + r - 6.0 * s);
        c.fill_rounded_rect(
            Rect::new((lx - 70.0 * s) as i32, (ly - 16.0 * s) as i32, (70.0 * s) as i32, (24.0 * s) as i32),
            6.0 * s,
            Color(0x9A06090F),
        );
        let color = if blink { RED } else { ACCENT };
        c.fill_circle(lx - 56.0 * s, ly - 4.0 * s, 5.0 * s, color);
        text.draw(c, f.bold, 12.0 * s, lx - 46.0 * s, ly + 0.0 * s, "SIREN", WHITE);
    }

    // The keys.
    let help = "Arrows/WASD drive   Space handbrake   Q siren   C camera   R reset   F11 full screen   Esc quit";
    text.draw(c, f.regular, 12.0 * s, 18.0 * s, h - 16.0 * s, help, Color(0x90E6ECF5));
}

/// Statistics in a panel at the top right.
pub fn stats(c: &mut Canvas, text: &mut Text, f: &Fonts, lines: &[alloc::string::String]) {
    let w = c.width();
    let (pw, ph) = (300, 18 * lines.len() as i32 + 16);
    let (px, py) = (w - pw - 16, 16);
    c.fill_rounded_rect(Rect::new(px, py, pw, ph), 8.0, Color(0xB0101418));
    for (i, l) in lines.iter().enumerate() {
        text.draw(c, f.mono, 12.0, (px + 10) as f32, (py + 22 + 18 * i as i32) as f32, l, Color(0xFFE8F0F8));
    }
}
