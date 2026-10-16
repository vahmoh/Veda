// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The menus, drawn over the showcase (or over play, paused): the title,
//! the main menu, the career's missions with their briefings, a quick
//! chase's and a patrol's choices, the options, the pause menu and the
//! results.

use interceptor_game::career::{self, Ending, MISSIONS, Mission};
use interceptor_game::env::{TimeOfDay, Weather};
use interceptor_game::road::Section;
use interceptor_game::session::describe;
use vgfx::{Align, Canvas, Color, Rect, Text};

use crate::app::{App, Item, MAIN, Screen};
use crate::paint::{self, ACCENT, AMBER, DIM, FAINT, Fonts, GOLD, GREEN, PANEL_DARK, RED, WHITE, rect};

/// Draws the current menu (nothing in play).
pub fn draw(c: &mut Canvas, text: &mut Text, f: &Fonts, app: &mut App) {
    app.targets.clear();
    match app.screen {
        Screen::Playing => {}
        Screen::Title => title(c, text, f, app),
        Screen::Main(_) => {
            side(c, text, f, app, "", "");
            let sel = app.selected();
            let stars = format!("{} / {}", app.save.stars(), MISSIONS.len() * 3);
            let s = paint::scale(c.height());
            let (x, y) = (56.0 * s, c.height() as f32 - 132.0 * s);
            text.draw_wrapped(c, f.regular, 15.0 * s, rect(x, y, 380.0 * s, 60.0 * s), MAIN[sel].1, DIM, Align::Left);
            paint::star(c, x + 8.0 * s, y - 22.0 * s, 8.0 * s, true, GOLD);
            text.draw(c, f.bold, 14.0 * s, x + 22.0 * s, y - 17.0 * s, &stars, DIM);
        }
        Screen::Career(_) => career_screen(c, text, f, app),
        Screen::Quick(_) => side(c, text, f, app, "QUICK CHASE", "One pursuit, your way."),
        Screen::Patrol(_) => side(c, text, f, app, "PATROL", "Answer dispatch's calls."),
        Screen::Options(..) => side(c, text, f, app, "OPTIONS", "Kept for next time."),
        Screen::Paused(_) => paused(c, text, f, app),
        Screen::Results(_) => results(c, text, f, app),
    }
}

/// Darkens the left of the picture, where a menu stands.
fn shade_left(c: &mut Canvas, width: f32) {
    let h = c.height();
    let w = width as i32;
    c.fill_rect(Rect::new(0, 0, w / 2, h), Color(0xC0020408));
    c.fill_horizontal_gradient(Rect::new(w / 2, 0, w - w / 2, h), Color(0xC0020408), Color(0x00020408));
}

/// The keys' help along the bottom.
fn hints(c: &mut Canvas, text: &mut Text, f: &Fonts, s: f32, line: &str) {
    let h = c.height() as f32;
    text.draw(c, f.regular, 12.0 * s, 56.0 * s, h - 26.0 * s, line, FAINT);
}

/// A menu down the left: the logo, `title` and `sub`, and the items.
fn side(c: &mut Canvas, text: &mut Text, f: &Fonts, app: &mut App, title: &str, sub: &str) {
    let s = paint::scale(c.height());
    shade_left(c, 620.0 * s);
    let x = 56.0 * s;
    paint::logo(c, text, f, x, 92.0 * s, 44.0 * s, false);
    let mut y = 170.0 * s;
    if !title.is_empty() {
        text.draw(c, f.bold, 20.0 * s, x, y + 6.0 * s, title, ACCENT);
        text.draw(c, f.regular, 14.0 * s, x, y + 28.0 * s, sub, DIM);
        y += 58.0 * s;
    }
    let items = app.items();
    let sel = app.selected();
    list(c, text, f, app, &items, sel, x, y, 400.0 * s, 48.0 * s);
    let has_values = items.iter().any(|i| i.value.is_some());
    hints(
        c,
        text,
        f,
        s,
        if has_values {
            "Up/Down select     Left/Right change     Enter choose     Esc back"
        } else {
            "Up/Down select     Enter choose     Esc back"
        },
    );
}

/// A column of items from (`x`, `y`), `w` wide and `step` apart, `sel`
/// highlighted; each a target for the pointer.
fn list(
    c: &mut Canvas,
    text: &mut Text,
    f: &Fonts,
    app: &mut App,
    items: &[Item],
    sel: usize,
    x: f32,
    y: f32,
    w: f32,
    step: f32,
) {
    let s = paint::scale(c.height());
    let size = 22.0 * s;
    for (i, item) in items.iter().enumerate() {
        let top = y + step * i as f32;
        let r = rect(x - 16.0 * s, top, w + 32.0 * s, step - 6.0 * s);
        let on = i == sel;
        if on {
            // A bar that brightens in from the left, and a marker.
            let pulse = 0.85 + 0.15 * (app.clock * 4.0).sin() as f32;
            c.fill_horizontal_gradient(r, ACCENT.fade(0.55 * pulse), ACCENT.fade(0.05));
            c.fill_rect(rect(r.x as f32, top, 4.0 * s, r.h as f32), ACCENT);
        }
        let color = if !item.enabled {
            FAINT.fade(0.6)
        } else if on {
            WHITE
        } else {
            DIM
        };
        let base = top + (step - 6.0 * s) * 0.5 + size * 0.36;
        text.draw(c, f.bold, size, x, base, &item.label, color);
        if let Some(v) = &item.value {
            let vx = x + w;
            if on {
                paint::right(c, text, f.bold, size * 0.8, vx, base, &format!("<  {v}  >"), WHITE);
            } else {
                paint::right(c, text, f.regular, size * 0.8, vx, base, v, DIM);
            }
        }
        app.targets.push((r, i));
    }
}

/// The title: the logo over the showcase, and an invitation.
fn title(c: &mut Canvas, text: &mut Text, f: &Fonts, app: &mut App) {
    let (w, h) = (c.width() as f32, c.height() as f32);
    let s = paint::scale(c.height());
    // Darker at the top and bottom, the chase between.
    c.fill_vertical_gradient(rect(0.0, 0.0, w, h * 0.45), Color(0xB0020408), Color(0x00020408));
    c.fill_vertical_gradient(rect(0.0, h * 0.7, w, h * 0.3), Color(0x00020408), Color(0xC0020408));
    let appear = (app.screen_time / 0.8).min(1.0);
    paint::logo(c, text, f, w * 0.5, h * 0.3, 78.0 * s, true);
    let blink = 0.55 + 0.45 * (app.clock * 3.0).sin() as f32;
    let pill = rect(w * 0.5 - 110.0 * s, h * 0.78 - 30.0 * s, 220.0 * s, 46.0 * s);
    c.fill_rounded_rect(pill, 23.0 * s, Color(0x90020408).fade(appear));
    paint::centered(c, text, f.bold, 18.0 * s, w * 0.5, h * 0.78, "PRESS ENTER", WHITE.fade(blink * appear));
    paint::right(c, text, f.regular, 12.0 * s, w - 24.0 * s, h - 20.0 * s, "Running on Veda", FAINT);
    app.targets.push((Rect::new(0, 0, w as i32, h as i32), 0));
}

/// Where a mission begins, in words.
fn place(m: &Mission) -> &'static str {
    match m.section {
        Section::City => "Downtown",
        Section::Hills => "The hills",
        Section::Tunnel => "The headland tunnel",
        Section::Coast => "The coast road",
        Section::Bridge => "The harbour bridge",
        Section::Port => "The port",
    }
}

fn light(t: TimeOfDay, w: Weather) -> String {
    let time = t.name().to_uppercase();
    match w {
        Weather::Clear => time,
        Weather::Rain => format!("{time} · {}", w.name().to_uppercase()),
    }
}

/// The career: the missions down the left, the one selected briefed on
/// the right.
fn career_screen(c: &mut Canvas, text: &mut Text, f: &Fonts, app: &mut App) {
    let (w, h) = (c.width() as f32, c.height() as f32);
    let s = paint::scale(c.height());
    shade_left(c, 640.0 * s);
    let x = 56.0 * s;
    paint::logo(c, text, f, x, 92.0 * s, 44.0 * s, false);
    text.draw(c, f.bold, 20.0 * s, x, 176.0 * s, "CAREER", ACCENT);
    let total = format!("{} / {}", app.save.stars(), MISSIONS.len() * 3);
    paint::star(c, x + 120.0 * s, 170.0 * s, 8.0 * s, true, GOLD);
    text.draw(c, f.bold, 14.0 * s, x + 134.0 * s, 175.0 * s, &total, DIM);
    let sel = app.selected();
    let step = 46.0 * s;
    let top = 196.0 * s;
    for (i, m) in MISSIONS.iter().enumerate() {
        let y = top + step * i as f32;
        let r = rect(x - 16.0 * s, y, 420.0 * s, step - 6.0 * s);
        let on = i == sel;
        let open = app.save.unlocked(i);
        if on {
            c.fill_horizontal_gradient(r, ACCENT.fade(0.5), ACCENT.fade(0.05));
            c.fill_rect(rect(r.x as f32, y, 4.0 * s, r.h as f32), ACCENT);
        }
        let base = y + (step - 6.0 * s) * 0.5 + 6.0 * s;
        let color = if !open {
            FAINT
        } else if on {
            WHITE
        } else {
            DIM
        };
        text.draw(c, f.mono_bold, 14.0 * s, x, base, &format!("{:02}", i + 1), if on { ACCENT } else { FAINT });
        text.draw(c, f.bold, 18.0 * s, x + 36.0 * s, base, m.name, color);
        let right = x + 392.0 * s;
        if open {
            let n = app.save.best[i].map_or(0, |b| b.stars);
            paint::stars(c, right - 62.0 * s, base - 6.0 * s, 8.0 * s, n);
        } else {
            paint::padlock(c, right - 16.0 * s, base - 15.0 * s, 17.0 * s, FAINT);
        }
        app.targets.push((r, i));
    }

    // The briefing.
    let m = &MISSIONS[sel];
    let open = app.save.unlocked(sel);
    let (bx, by) = ((560.0 * s).max(w * 0.46), 150.0 * s);
    let bw = (w - bx - 48.0 * s).min(620.0 * s);
    if bw < 200.0 * s {
        return;
    }
    // Tall enough for the briefing (measured before it is drawn).
    let lines = text.wrap(f.regular, 16.0 * s, m.brief, bw - 56.0 * s).len() as f32;
    let brief_h = lines * text.metrics(f.regular, 16.0 * s).line_height;
    let bh = (318.0 * s + brief_h).min(h - by - 60.0 * s);
    c.fill_rounded_rect(rect(bx, by, bw, bh), 14.0 * s, PANEL_DARK);
    let px = bx + 28.0 * s;
    let mut y = by + 46.0 * s;
    text.draw(c, f.mono_bold, 13.0 * s, px, y - 18.0 * s, &format!("MISSION {:02}", sel + 1), ACCENT);
    text.draw(c, f.bold, 30.0 * s, px, y + 14.0 * s, m.name, WHITE);
    y += 42.0 * s;
    let sub = format!("{}  ·  {}", light(m.time, m.weather), place(m).to_uppercase());
    text.draw(c, f.bold, 12.5 * s, px, y, &sub, AMBER);
    y += 22.0 * s;
    let used =
        text.draw_wrapped(c, f.regular, 16.0 * s, rect(px, y, bw - 56.0 * s, 120.0 * s), m.brief, WHITE, Align::Left);
    y += used as f32 + 28.0 * s;
    let row = |c: &mut Canvas, text: &mut Text, y: f32, k: &str, v: &str| {
        text.draw(c, f.regular, 14.0 * s, px, y, k, DIM);
        text.draw(c, f.bold, 14.0 * s, px + 130.0 * s, y, v, WHITE);
    };
    let mut suspect = describe(&m.wanted);
    if let Some(first) = suspect.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    row(c, text, y, "Suspect", &suspect);
    y += 26.0 * s;
    let backup = match m.backup {
        0 => String::from("None: you are on your own"),
        1 => String::from("One unit"),
        n => format!("{n} units"),
    };
    row(c, text, y, "Backup", &backup);
    y += 26.0 * s;
    let limit = m.limit.map_or(String::from("None"), career::minutes);
    row(c, text, y, "Time limit", &limit);
    y += 34.0 * s;
    // The targets, and the best so far.
    text.draw(c, f.regular, 14.0 * s, px, y, "Targets", DIM);
    let tx = px + 130.0 * s;
    paint::stars(c, tx, y - 5.0 * s, 7.0 * s, 3);
    text.draw(c, f.bold, 14.0 * s, tx + 56.0 * s, y, &format!("under {}", career::minutes(m.stars[0])), WHITE);
    paint::stars(c, tx + 160.0 * s, y - 5.0 * s, 7.0 * s, 2);
    text.draw(c, f.bold, 14.0 * s, tx + 216.0 * s, y, &format!("under {}", career::minutes(m.stars[1])), WHITE);
    y += 26.0 * s;
    text.draw(c, f.regular, 14.0 * s, px, y, "Best", DIM);
    match app.save.best[sel] {
        Some(b) => {
            paint::stars(c, tx, y - 5.0 * s, 7.0 * s, b.stars);
            let line = format!("{}   ·   {} points", career::clock(b.time), group(b.score));
            text.draw(c, f.bold, 14.0 * s, tx + 56.0 * s, y, &line, WHITE);
        }
        None => {
            text.draw(c, f.bold, 14.0 * s, tx, y, "Not yet caught", FAINT);
        }
    }
    // How to start it.
    let foot = by + bh - 26.0 * s;
    if open {
        text.draw(c, f.bold, 15.0 * s, px, foot, "ENTER  START THE PURSUIT", GREEN);
    } else {
        paint::padlock(c, px, foot - 15.0 * s, 17.0 * s, FAINT);
        text.draw(c, f.bold, 15.0 * s, px + 24.0 * s, foot, "CATCH THE SUSPECT OF THE MISSION BEFORE", FAINT);
    }
    hints(c, text, f, s, "Up/Down select     Enter start     Esc back");
}

/// A number with its thousands apart ("12 450").
pub fn group(n: u32) -> String {
    if n >= 1000 { format!("{} {:03}", n / 1000, n % 1000) } else { format!("{n}") }
}

/// A panel in the middle of the screen, `w` by `h`, the frame dimmed.
fn centre_panel(c: &mut Canvas, s: f32, w: f32, h: f32) -> (f32, f32) {
    let (cw, ch) = (c.width() as f32, c.height() as f32);
    c.fill_rect(Rect::new(0, 0, c.width(), c.height()), Color(0x90000000));
    let (x, y) = ((cw - w) * 0.5, (ch - h) * 0.5);
    c.fill_rounded_rect(rect(x, y, w, h), 16.0 * s, PANEL_DARK);
    (x, y)
}

/// Buttons side by side from (`x`, `y`), `w` across in all.
fn buttons(c: &mut Canvas, text: &mut Text, f: &Fonts, app: &mut App, x: f32, y: f32, w: f32) {
    let s = paint::scale(c.height());
    let items = app.items();
    let n = items.len().max(1) as f32;
    let gap = 14.0 * s;
    let bw = (w - gap * (n - 1.0)) / n;
    let bh = 46.0 * s;
    for (i, item) in items.iter().enumerate() {
        let bx = x + i as f32 * (bw + gap);
        let r = rect(bx, y, bw, bh);
        let on = i == app.selected();
        c.fill_rounded_rect(r, 10.0 * s, if on { ACCENT } else { Color(0x40FFFFFF) });
        paint::centered(c, text, f.bold, 16.0 * s, bx + bw * 0.5, y + bh * 0.5 + 6.0 * s, &item.label, WHITE);
        app.targets.push((r, i));
    }
}

/// The pause menu over the frozen game.
fn paused(c: &mut Canvas, text: &mut Text, f: &Fonts, app: &mut App) {
    let s = paint::scale(c.height());
    let items = app.items();
    let (w, h) = (420.0 * s, 150.0 * s + items.len() as f32 * 50.0 * s);
    let (x, y) = centre_panel(c, s, w, h);
    paint::centered(c, text, f.bold, 30.0 * s, x + w * 0.5, y + 52.0 * s, "PAUSED", WHITE);
    let what = match app.session.mode {
        interceptor_game::session::Mode::Career(m) => format!("Mission {:02}: {}", m + 1, MISSIONS[m].name),
        interceptor_game::session::Mode::Quick(_) => String::from("Quick chase"),
        _ => format!("Patrol: {} caught, {} lost", app.session.arrests, app.session.escapes),
    };
    paint::centered(c, text, f.regular, 14.0 * s, x + w * 0.5, y + 78.0 * s, &what, DIM);
    let sel = app.selected();
    list(c, text, f, app, &items, sel, x + 48.0 * s, y + 104.0 * s, w - 96.0 * s, 50.0 * s);
}

/// The result of a chase: the headline, the stars, the numbers and what
/// next.
fn results(c: &mut Canvas, text: &mut Text, f: &Fonts, app: &mut App) {
    let Some(r) = app.result else { return };
    let s = paint::scale(c.height());
    let o = r.outcome;
    let (w, h) = (560.0 * s, 470.0 * s);
    let (x, y) = centre_panel(c, s, w, h);
    let cx = x + w * 0.5;
    let caught = o.ending.caught();
    let what = match r.mission {
        Some(m) => format!("MISSION {:02}  ·  {}", m + 1, MISSIONS[m].name.to_uppercase()),
        None => String::from("QUICK CHASE"),
    };
    paint::centered(c, text, f.bold, 13.0 * s, cx, y + 40.0 * s, &what, ACCENT);
    let color = if caught { GREEN } else { RED };
    paint::centered(c, text, f.bold, 40.0 * s, cx, y + 92.0 * s, o.ending.title(), color);
    // The stars, one after another.
    let t = app.screen_time;
    let r_star = 22.0 * s;
    for k in 0..3u8 {
        let at = 0.5 + 0.35 * k as f32;
        let sx = cx + (k as f32 - 1.0) * r_star * 2.6;
        let sy = y + 142.0 * s;
        if k < o.stars && t > at {
            // A pop as it lands.
            let pop = 1.0 + 0.4 * (1.0 - ((t - at) / 0.25).min(1.0));
            paint::star(c, sx, sy, r_star * pop, true, GOLD);
        } else {
            paint::star(c, sx, sy, r_star, false, Color(0x50E6ECF5));
        }
    }
    // The numbers.
    let units = app.save.settings.units;
    let rows = [
        ("Time", career::clock(o.time)),
        ("Hits landed", format!("{}", o.hits)),
        ("Damage taken", format!("{}%", (o.damage * 100.0).round() as u32)),
        ("Top speed", format!("{} {}", units.of(o.top_speed).round() as u32, units.name())),
    ];
    let (lx, rx) = (x + 70.0 * s, x + w - 70.0 * s);
    let mut ry = y + 202.0 * s;
    for (k, v) in rows {
        text.draw(c, f.regular, 15.0 * s, lx, ry, k, DIM);
        paint::right(c, text, f.bold, 15.0 * s, rx, ry, &v, WHITE);
        ry += 28.0 * s;
    }
    c.fill_rect(rect(lx, ry - 10.0 * s, rx - lx, 1.0 * s), Color(0x30FFFFFF));
    ry += 18.0 * s;
    text.draw(c, f.bold, 18.0 * s, lx, ry, "SCORE", WHITE);
    // The score counts up.
    let shown = (o.score as f32 * ((t - 0.3) / 1.2).clamp(0.0, 1.0)) as u32;
    paint::right(c, text, f.mono_bold, 22.0 * s, rx, ry + 2.0 * s, &group(shown), GOLD);
    if r.best && t > 1.6 {
        paint::centered(c, text, f.bold, 14.0 * s, cx, ry + 30.0 * s, "NEW BEST", AMBER);
    } else if !caught && o.ending == Ending::OutOfTime {
        paint::centered(c, text, f.regular, 14.0 * s, cx, ry + 30.0 * s, "Faster next time: use the nitrous", DIM);
    }
    buttons(c, text, f, app, x + 40.0 * s, y + h - 76.0 * s, w - 80.0 * s);
}
