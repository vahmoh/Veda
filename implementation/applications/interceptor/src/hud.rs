// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What is drawn over play with `vgfx`: the dashboard (speedometer,
//! revs, gear, siren, nitrous), the pursuit (the suspect's distance, its
//! car's state, the time and any limit, the warnings), the map of the bay,
//! the radio's words, the countdown, a patrol's tally; and the loading
//! screen, the frame rate with what renders, and the statistics.

use interceptor_game::career::{self, Units};
use interceptor_game::driver::Role;
use interceptor_game::game::Game;
use interceptor_game::pursuit::{self, Pursuit, State};
use interceptor_game::session::{self, Mode, Phase, Session};
use interceptor_game::world::World;
use vgfx::{Align, Bitmap, Canvas, Color, LineCap, LineJoin, Path, Rect, StrokeStyle, Text};

use crate::paint::{self, ACCENT, AMBER, DIM, FAINT, Fonts, GREEN, PANEL, RED, WHITE, centered, rect};
use crate::perf::{HISTORY, Perf};

/// The screen while Veda Bay is made.
pub fn loading(c: &mut Canvas, text: &mut Text, f: &Fonts) {
    let (w, h) = (c.width(), c.height());
    c.fill_vertical_gradient(Rect::new(0, 0, w, h), Color(0xFF0B1324), Color(0xFF020306));
    let (cx, cy) = (w as f32 * 0.5, h as f32 * 0.5);
    let s = paint::scale(h);
    paint::logo(c, text, f, cx, cy, 58.0 * s, true);
    centered(c, text, f.regular, 14.0 * s, cx, cy + 110.0 * s, "Building Veda Bay\u{2026}", Color(0x90E6ECF5));
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
/// the revs, the siren's state and the nitrous beside it, and the keys at
/// the bottom left.
pub fn dashboard(c: &mut Canvas, text: &mut Text, f: &Fonts, game: &Game, units: Units) {
    let (w, h) = (c.width() as f32, c.height() as f32);
    let s = paint::scale(c.height());
    let car = &game.player().vehicle;
    let speed = units.of(car.speed().abs()).round();
    // The dial's top and its ticks: 280 km/h every 20, or 180 mph every 15.
    let (top, tick) = if units == Units::Mph { (180.0, 15.0) } else { (280.0, 20.0) };
    let r = 92.0 * s;
    let (cx, cy) = (w - r - 36.0 * s, h - r - 30.0 * s);
    // The dial's face.
    c.fill_circle(cx, cy, r + 10.0 * s, Color(0x9A06090F));
    let thin = StrokeStyle { width: 6.0 * s, cap: LineCap::Round, join: LineJoin::Round, miter_limit: 4.0 };
    c.stroke_path(&dial_arc(cx, cy, r - 4.0 * s, 0.0, 1.0), &thin, Color(0x40FFFFFF));
    let t = (speed / top).clamp(0.0, 1.0);
    let hot = if t > 0.72 { RED } else { ACCENT };
    if t > 0.002 {
        c.stroke_path(&dial_arc(cx, cy, r - 4.0 * s, 0.0, t), &thin, hot);
    }
    let ticks = (top / tick) as u32;
    for k in 0..=ticks {
        let v = k as f32 * tick;
        let tt = v / top;
        let major = k % 2 == 0;
        let (x0, y0) = dial_point(cx, cy, r - 14.0 * s, tt);
        let (x1, y1) = dial_point(cx, cy, r - if major { 24.0 } else { 19.0 } * s, tt);
        c.draw_line(x0, y0, x1, y1, if major { 2.0 * s } else { 1.2 * s }, Color(0xC0FFFFFF));
        if major && k < ticks {
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
        let color = if revs > 0.9 { RED } else { AMBER };
        c.stroke_path(&dial_arc(cx, cy, r - 50.0 * s, 0.0, revs), &rev_style, color);
    }
    // The needle.
    let (nx, ny) = dial_point(cx, cy, r - 10.0 * s, t);
    c.draw_line(cx, cy, nx, ny, 2.5 * s, Color(0xFFFF5A3C));
    c.fill_circle(cx, cy, 6.0 * s, Color(0xFF1A1E26));
    // The speed and the gear.
    centered(c, text, f.mono_bold, 30.0 * s, cx, cy + 34.0 * s, &format!("{}", speed as u32), WHITE);
    centered(c, text, f.regular, 11.0 * s, cx, cy + 50.0 * s, units.name(), DIM);
    let gear = match car.gear {
        g if g < 0 => String::from("R"),
        g => format!("{g}"),
    };
    centered(c, text, f.mono_bold, 16.0 * s, cx, cy + 74.0 * s, &gear, AMBER);

    // The siren, above the nitrous.
    if game.player().siren {
        let blink = (game.clock() * 3.2) % 1.0 < 0.5;
        let (lx, ly) = (cx - r - 6.0 * s, cy - r - 2.0 * s);
        c.fill_rounded_rect(rect(lx - 70.0 * s, ly - 16.0 * s, 70.0 * s, 24.0 * s), 6.0 * s, Color(0x9A06090F));
        c.fill_circle(lx - 56.0 * s, ly - 4.0 * s, 5.0 * s, if blink { RED } else { ACCENT });
        text.draw(c, f.bold, 12.0 * s, lx - 46.0 * s, ly, "SIREN", WHITE);
    }

    // The nitrous, left of the speedometer.
    let level = game.boost.level.clamp(0.0, 1.0);
    let (bx, by) = (w - 2.0 * r - 70.0 * s, h - 48.0 * s - 90.0 * s);
    let (bw, bh) = (10.0 * s, 90.0 * s);
    c.fill_rounded_rect(rect(bx, by, bw, bh), 4.0 * s, Color(0x60000000));
    let fill = bh * level;
    let color = if game.boost.on { Color(0xFF7FE3FF) } else { ACCENT };
    c.fill_rounded_rect(rect(bx, by + bh - fill, bw, fill), 4.0 * s, color);
    centered(c, text, f.bold, 9.0 * s, bx + bw * 0.5, by + bh + 13.0 * s, "N2O", DIM);

    // The keys.
    let help =
        "Arrows/WASD drive   Space handbrake   Shift nitrous   Q siren   H horn   C camera   R reset   Esc pause";
    text.draw(c, f.regular, 12.0 * s, 18.0 * s, h - 16.0 * s, help, Color(0x90E6ECF5));
}

/// The readout of the frame rate: its type sizes (at a scale of 1).
const RATE_SIZE: f32 = 17.0;
const TEXT_SIZE: f32 = 11.5;
const LABEL_SIZE: f32 = 9.5;

/// The readout at scale `s`: its words, the widths it is laid out by
/// (the rate's, "FPS"'s and the GPU's or CPU's label), and its size.
struct Readout {
    fps: String,
    ms: String,
    kind: &'static str,
    widths: [f32; 3],
    size: (f32, f32),
}

impl Readout {
    fn new(text: &mut Text, f: &Fonts, perf: &Perf, s: f32) -> Readout {
        // Nothing to say until the first second has been counted.
        let measured = perf.fps > 0.0;
        let fps = if measured { format!("{:.0}", perf.fps) } else { String::from("--") };
        let ms = if measured { format!("{:.1} ms", perf.frame_ms) } else { String::new() };
        let kind = if perf.cpu { "CPU" } else { "GPU" };
        let widths = [
            text.measure(f.mono_bold, RATE_SIZE * s, &fps),
            text.measure(f.bold, LABEL_SIZE * s, "FPS"),
            text.measure(f.bold, LABEL_SIZE * s, kind),
        ];
        let first = widths[0] + 5.0 * s + widths[1] + 10.0 * s + text.measure(f.mono, TEXT_SIZE * s, &ms);
        let second = widths[2] + 6.0 * s + text.measure(f.regular, TEXT_SIZE * s, &perf.device);
        Readout { fps, ms, kind, widths, size: (first.max(second) + 20.0 * s, 50.0 * s) }
    }
}

/// Where the readout of the frame rate goes in play: the top left corner,
/// under a patrol's tally, or under the chase's panel where the window is
/// too narrow for both side by side.
pub fn performance_in_play(c: &Canvas, text: &mut Text, f: &Fonts, perf: &Perf, session: &Session) -> (f32, f32) {
    let s = paint::scale(c.height());
    let width = Readout::new(text, f, perf, s).size.0;
    let y = if let Mode::Patrol { .. } = session.mode {
        84.0 * s
    } else if session.chase.is_some() && (16.0 + 8.0) * s + width > c.width() as f32 * 0.5 - PURSUIT_HALF * s {
        (16.0 + PURSUIT_HEIGHT + 8.0) * s
    } else {
        16.0 * s
    };
    (16.0 * s, y)
}

/// The frame rate and what renders (the GPU, or the processor), in a
/// small panel whose top left corner is at (`x`, `y`), or its top right
/// if `right`. Returns its height.
pub fn performance(c: &mut Canvas, text: &mut Text, f: &Fonts, perf: &Perf, x: f32, y: f32, right: bool) -> f32 {
    let s = paint::scale(c.height());
    let r = Readout::new(text, f, perf, s);
    let (pw, ph) = r.size;
    let px = if right { x - pw } else { x };
    c.fill_rounded_rect(rect(px, y, pw, ph), 8.0 * s, PANEL);
    // Green while smooth, amber under 50 a second, red under 30.
    let color = match perf.fps {
        r if r <= 0.0 => DIM,
        r if r >= 50.0 => GREEN,
        r if r >= 30.0 => AMBER,
        _ => RED,
    };
    let base = y + 22.0 * s;
    let mut tx = px + 10.0 * s;
    text.draw(c, f.mono_bold, RATE_SIZE * s, tx, base, &r.fps, color);
    tx += r.widths[0] + 5.0 * s;
    text.draw(c, f.bold, LABEL_SIZE * s, tx, base, "FPS", DIM);
    tx += r.widths[1] + 10.0 * s;
    text.draw(c, f.mono, TEXT_SIZE * s, tx, base, &r.ms, FAINT);
    let base = y + 40.0 * s;
    let kind_color = if perf.cpu { AMBER } else { ACCENT };
    text.draw(c, f.bold, LABEL_SIZE * s, px + 10.0 * s, base, r.kind, kind_color);
    text.draw(c, f.regular, TEXT_SIZE * s, px + (10.0 + 6.0) * s + r.widths[2], base, &perf.device, DIM);
    ph
}

/// The renderer's statistics in a panel whose top right corner is at
/// (`right`, `y`): `lines`, the last frames' lengths as a graph (the faint
/// lines at 60 and 30 a second), and the renderer's own name.
pub fn stats(c: &mut Canvas, text: &mut Text, f: &Fonts, perf: &Perf, lines: &[String], right: f32, y: f32) {
    let s = paint::scale(c.height());
    let pad = 10.0 * s;
    let (gw, gh) = (280.0 * s, 44.0 * s);
    let (pw, ph) = (gw + pad * 2.0, (18 * lines.len() + 40) as f32 * s + gh + 18.0 * s);
    let px = right - pw;
    c.fill_rounded_rect(rect(px, y, pw, ph), 8.0 * s, Color(0xB0101418));
    let mut base = y + 22.0 * s;
    for l in lines {
        text.draw(c, f.mono, 12.0 * s, px + pad, base, l, Color(0xFFE8F0F8));
        base += 18.0 * s;
    }
    let longest = format!("frames: the longest {:.0} ms", perf.longest_shown());
    text.draw(c, f.mono, 12.0 * s, px + pad, base, &longest, Color(0xFFE8F0F8));
    // The graph: up to 50 ms, a bar a frame, the latest at the right.
    let (gx, gy) = (px + pad, base + 8.0 * s);
    c.fill_rect(rect(gx, gy, gw, gh), Color(0x40000000));
    for ms in [1000.0 / 60.0, 1000.0 / 30.0] {
        c.fill_rect(rect(gx, gy + gh * (1.0 - ms / 50.0), gw, 1.0), Color(0x30FFFFFF));
    }
    let bar = gw / HISTORY as f32;
    let start = HISTORY - perf.history.len();
    for (i, &ms) in perf.history.iter().enumerate() {
        let h = (ms / 50.0).clamp(0.02, 1.0) * gh;
        let color = if ms <= 20.0 {
            ACCENT
        } else if ms <= 34.0 {
            AMBER
        } else {
            RED
        };
        let x0 = gx + (start + i) as f32 * bar;
        c.fill_rect(rect(x0, gy + gh - h, (bar - 0.4 * s).max(1.0), h), color);
    }
    text.draw(c, f.regular, 11.0 * s, px + pad, gy + gh + 18.0 * s, &perf.renderer, DIM);
}

/// The pursuit's panel: half its width (its middle is the screen's) and
/// its height, at the top.
const PURSUIT_HALF: f32 = 170.0;
const PURSUIT_HEIGHT: f32 = 66.0;

/// The pursuit: the suspect's car's state and how far ahead it is (top
/// middle), with the time (or the time left), and a warning while it is
/// getting away or being boxed in; the outcome as it ends.
fn pursuit(c: &mut Canvas, text: &mut Text, f: &Fonts, game: &Game, chase: &Pursuit, left: Option<f32>) {
    let (w, h) = (c.width() as f32, c.height() as f32);
    let s = paint::scale(c.height());
    let cx = w * 0.5;
    let (pw, ph) = (PURSUIT_HALF * 2.0 * s, PURSUIT_HEIGHT * s);
    let (px, py) = (cx - pw * 0.5, 16.0 * s);
    c.fill_rounded_rect(rect(px, py, pw, ph), 10.0 * s, Color(0xA006090F));
    text.draw(c, f.bold, 13.0 * s, px + 14.0 * s, py + 22.0 * s, "SUSPECT", RED);
    let gap = chase.gap.max(0.0);
    let dist = if gap >= 1000.0 { format!("{:.1} km", gap / 1000.0) } else { format!("{} m", gap as u32) };
    paint::right(c, text, f.mono_bold, 13.0 * s, px + pw - 14.0 * s, py + 22.0 * s, &dist, WHITE);
    // The time, or the time left (urgent at the end).
    let (clock, color) = match left {
        Some(l) => {
            let urgent = l < 20.0 && (l * 2.0).fract() < 0.5;
            (format!("{} LEFT", career::minutes(l.ceil())), if urgent { RED } else { AMBER })
        }
        None => (career::minutes(chase.time), DIM),
    };
    centered(c, text, f.mono_bold, 12.0 * s, cx, py + 22.0 * s, &clock, color);
    // Its car's integrity.
    let integrity = chase.integrity(game).clamp(0.0, 1.0);
    let (bx, by, bw, bh) = (px + 14.0 * s, py + 34.0 * s, pw - 28.0 * s, 12.0 * s);
    c.fill_rounded_rect(rect(bx, by, bw, bh), 4.0 * s, Color(0x40FFFFFF));
    if integrity > 0.0 {
        let color = if integrity < 0.3 {
            RED
        } else if integrity < 0.6 {
            AMBER
        } else {
            GREEN
        };
        c.fill_rounded_rect(rect(bx, by, bw * integrity, bh), 4.0 * s, color);
    }
    text.draw(c, f.regular, 10.5 * s, bx, by + bh + 13.0 * s, "Ram it to a stop, or box it in", FAINT);

    // The warnings.
    let banner = |c: &mut Canvas, text: &mut Text, msg: &str, color: Color, y: f32| {
        let size = 22.0 * s;
        let tw = text.measure(f.bold, size, msg);
        c.fill_rounded_rect(
            rect(cx - tw * 0.5 - 18.0 * s, y - size, tw + 36.0 * s, size * 1.5),
            8.0 * s,
            Color(0xA006090F),
        );
        text.draw(c, f.bold, size, cx - tw * 0.5, y, msg, color);
    };
    match chase.state {
        State::Chasing { escaping, busting } => {
            if escaping > 0.0 {
                let left = (pursuit::ESCAPE_TIME - escaping).ceil().max(0.0) as u32;
                banner(c, text, &format!("SUSPECT GETTING AWAY  {left}"), AMBER, py + ph + 46.0 * s);
            } else if busting > 0.0 {
                let pct = (busting / pursuit::BUST_TIME * 100.0).min(100.0) as u32;
                banner(c, text, &format!("BUSTING  {pct}%"), GREEN, py + ph + 46.0 * s);
            }
        }
        State::TakenDown => banner(c, text, "TAKEDOWN!", GREEN, h * 0.36),
        State::Busted => banner(c, text, "BUSTED!", GREEN, h * 0.36),
        State::Escaped => banner(c, text, "SUSPECT ESCAPED", RED, h * 0.36),
    }
}

/// The map of Veda Bay: the loop, and the cars that matter on it. Its
/// panel and the road change only with the size it is drawn at, so they
/// are drawn once into a picture of their own, which each frame draws with
/// the cars over it.
pub struct MiniMap {
    /// The road's centre line every 16 m, and its extent.
    points: Vec<(f32, f32)>,
    min: (f32, f32),
    max: (f32, f32),
    /// The panel and the road as last drawn, for a map of that size at
    /// that scale.
    drawn: Option<((i32, i32, u32), Bitmap)>,
}

impl MiniMap {
    pub fn new(world: &World) -> MiniMap {
        let points: Vec<(f32, f32)> = world.road.samples().iter().step_by(8).map(|s| (s.pos.x, s.pos.z)).collect();
        let fold =
            |f: fn(f32, f32) -> f32, init: f32| points.iter().fold((init, init), |(a, b), &(x, z)| (f(a, x), f(b, z)));
        let (min, max) = (fold(f32::min, f32::MAX), fold(f32::max, f32::MIN));
        MiniMap { points, min, max, drawn: None }
    }

    /// Where a point of the world `(x, z)` is on a map `w` x `h` at scale
    /// `s` whose top left corner is at `(x0, y0)`.
    fn mapping(&self, x0: f32, y0: f32, w: f32, h: f32, s: f32) -> impl Fn(f32, f32) -> (f32, f32) + use<> {
        let pad = 14.0 * s;
        let (ex, ez) = (self.max.0 - self.min.0, self.max.1 - self.min.1);
        let k = ((w - 2.0 * pad) / ex).min((h - 2.0 * pad) / ez);
        let (ox, oy) = (x0 + (w - ex * k) * 0.5, y0 + (h - ez * k) * 0.5);
        let min = self.min;
        move |x: f32, z: f32| (ox + (x - min.0) * k, oy + (z - min.1) * k)
    }

    /// The panel and the road, `w` x `h` at scale `s`.
    fn paint(&self, w: i32, h: i32, s: f32) -> Bitmap {
        let mut b = Bitmap::new(w, h);
        let mut c = Canvas::for_bitmap(&mut b);
        c.fill_rounded_rect(Rect::new(0, 0, w, h), 12.0 * s, PANEL);
        let at = self.mapping(0.0, 0.0, w as f32, h as f32, s);
        let mut road = Path::new();
        for (i, &(x, z)) in self.points.iter().enumerate() {
            let (px, py) = at(x, z);
            if i == 0 {
                road.move_to(px, py);
            } else {
                road.line_to(px, py);
            }
        }
        road.close();
        let wide = StrokeStyle { width: 5.0 * s, cap: LineCap::Round, join: LineJoin::Round, miter_limit: 4.0 };
        let thin = StrokeStyle { width: 2.2 * s, ..wide };
        c.stroke_path(&road, &wide, Color(0x60000000));
        c.stroke_path(&road, &thin, Color(0xB0C8D2E0));
        b
    }

    /// Draws it in `r` (`clock` the seconds its marks pulse by).
    fn draw(&mut self, c: &mut Canvas, r: Rect, game: &Game, chase: Option<&Pursuit>, clock: f64) {
        let s = paint::scale(c.height());
        let size = (r.w, r.h, s.to_bits());
        if self.drawn.as_ref().is_none_or(|d| d.0 != size) {
            self.drawn = Some((size, self.paint(r.w, r.h, s)));
        }
        if let Some((_, panel)) = &self.drawn {
            c.draw_bitmap(panel, r.x, r.y, 255);
        }
        let at = self.mapping(r.x as f32, r.y as f32, r.w as f32, r.h as f32, s);
        // The other police, the suspect (pulsing), and the player.
        for (i, car) in game.cars.iter().enumerate() {
            if i != game.player && matches!(car.role, Role::Police(_)) {
                let (px, py) = at(car.vehicle.pos.x, car.vehicle.pos.z);
                c.fill_circle(px, py, 3.5 * s, ACCENT);
            }
        }
        if let Some(suspect) = chase.filter(|c| c.on()).and_then(|chase| game.car(chase.suspect)) {
            let v = &suspect.vehicle;
            let (px, py) = at(v.pos.x, v.pos.z);
            let pulse = ((clock * 3.0) % 1.0) as f32;
            c.fill_circle(px, py, (4.0 + 8.0 * pulse) * s, RED.fade(0.5 * (1.0 - pulse)));
            c.fill_circle(px, py, 4.5 * s, RED);
        }
        let v = &game.player().vehicle;
        let (px, py) = at(v.pos.x, v.pos.z);
        let (fx, fz) = (v.yaw.sin(), v.yaw.cos());
        let (lx, lz) = (fz, -fx);
        let size = 7.0 * s;
        let mut arrow = Path::new();
        arrow.move_to(px + fx * size, py + fz * size);
        arrow.line_to(px - fx * size * 0.7 + lx * size * 0.65, py - fz * size * 0.7 + lz * size * 0.65);
        arrow.line_to(px - fx * size * 0.35, py - fz * size * 0.35);
        arrow.line_to(px - fx * size * 0.7 - lx * size * 0.65, py - fz * size * 0.7 - lz * size * 0.65);
        arrow.close();
        c.fill_path(&arrow, WHITE, vgfx::FillRule::NonZero);
    }
}

/// The radio's words, low in the middle, fading in and out.
fn radio(c: &mut Canvas, text: &mut Text, f: &Fonts, words: &str, age: f32) {
    let alpha = (age / 0.15).min(1.0) * ((session::RADIO_SECS - age) / 0.6).clamp(0.0, 1.0);
    if alpha <= 0.0 {
        return;
    }
    let (w, h) = (c.width() as f32, c.height() as f32);
    let s = paint::scale(c.height());
    let bw = (640.0 * s).min(w - 520.0 * s).max(320.0 * s);
    let (bx, by) = ((w - bw) * 0.5, h - 160.0 * s);
    c.fill_rounded_rect(rect(bx, by, bw, 70.0 * s), 10.0 * s, Color(0xC006090F).fade(alpha));
    c.fill_rect(rect(bx, by + 10.0 * s, 3.0 * s, 50.0 * s), ACCENT.fade(alpha));
    text.draw(c, f.bold, 11.0 * s, bx + 16.0 * s, by + 22.0 * s, "DISPATCH", ACCENT.fade(alpha));
    text.draw_wrapped(
        c,
        f.regular,
        15.0 * s,
        rect(bx + 16.0 * s, by + 28.0 * s, bw - 32.0 * s, 40.0 * s),
        words,
        WHITE.fade(alpha),
        Align::Left,
    );
}

/// The countdown's number (or the go), big in the middle.
fn countdown(c: &mut Canvas, text: &mut Text, f: &Fonts, label: &str, t: f32) {
    let (w, h) = (c.width() as f32, c.height() as f32);
    let s = paint::scale(c.height());
    // Each pops in and fades.
    let pop = 1.0 + 0.35 * (1.0 - (t / 0.25).min(1.0));
    let alpha = 1.0 - ((t - 0.6) / 0.4).clamp(0.0, 1.0);
    let color = if label == "GO!" { GREEN } else { WHITE };
    centered(c, text, f.bold, 110.0 * s * pop, w * 0.5, h * 0.42, label, color.fade(alpha));
}

/// Everything drawn over play (`clock` the seconds its marks pulse by).
pub fn play(
    c: &mut Canvas,
    text: &mut Text,
    f: &Fonts,
    session: &Session,
    map: &mut MiniMap,
    units: Units,
    clock: f64,
) {
    let game = &session.game;
    let s = paint::scale(c.height());
    let w = c.width() as f32;
    dashboard(c, text, f, game, units);
    let chase = session.chase.as_ref();
    map.draw(c, rect(w - 256.0 * s, 16.0 * s, 240.0 * s, 180.0 * s), game, chase, clock);
    if let Some(chase) = chase {
        pursuit(c, text, f, game, chase, session.time_left());
    }
    if let Mode::Patrol { .. } = session.mode {
        let line = format!("PATROL     {} caught     {} lost", session.arrests, session.escapes);
        c.fill_rounded_rect(rect(16.0 * s, 16.0 * s, 290.0 * s, 34.0 * s), 8.0 * s, PANEL);
        text.draw(c, f.bold, 13.0 * s, 30.0 * s, 38.0 * s, &line, WHITE);
        if chase.is_none() {
            text.draw(c, f.regular, 12.0 * s, 30.0 * s, 70.0 * s, "Awaiting dispatch's next call", DIM);
        }
    }
    if let Some((words, age)) = &session.radio {
        radio(c, text, f, words, *age);
    }
    match session.phase {
        Phase::Countdown(t) => {
            let n = t.ceil().max(1.0);
            countdown(c, text, f, &format!("{}", n as u32), n - t);
        }
        Phase::Driving => {
            if let Some(chase) = chase.filter(|c| c.time < 1.0 && !matches!(session.mode, Mode::Patrol { .. })) {
                countdown(c, text, f, "GO!", chase.time);
            }
        }
        Phase::Over(_) => {}
    }
}
