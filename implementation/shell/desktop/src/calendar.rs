// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The calendar flyout opened from the taskbar clock.

use gfx::{Align, Color, Rect};
use rt::time::{DateTime, days_from_civil, days_in_month, month_name, weekday};
use ui::{Font, Icon, Ui};

use crate::{Model, chrome, taskbar};

const WIDTH: i32 = 336;
const HEIGHT: i32 = 404;
const PAD: i32 = 20;

/// Where the flyout appears: above the clock, at the right edge.
pub fn placement(screen: Rect) -> Rect {
    Rect::new(screen.w - WIDTH - 12, screen.h - taskbar::HEIGHT - 12 - HEIGHT, WIDTH, HEIGHT)
}

pub struct Calendar {
    origin: (i32, i32),
    /// Month shown (year, 1..=12).
    year: i32,
    month: u8,
}

impl Calendar {
    pub fn new(origin: (i32, i32)) -> Calendar {
        let now = DateTime::now();
        Calendar { origin, year: now.year, month: now.month }
    }

    fn step(&mut self, delta: i32) {
        let m = self.year * 12 + self.month as i32 - 1 + delta;
        self.year = m.div_euclid(12);
        self.month = (m.rem_euclid(12) + 1) as u8;
    }

    pub fn update(&mut self, ui: &mut Ui, m: &mut Model) {
        let t = ui.theme().clone();
        let w = ui.width;
        chrome::popup_background(&mut ui.canvas, &m.wallpaper.blurred, self.origin);
        if ui.input.key(proto::input::keys::ESC) {
            ui.close_window();
        }

        // Live clock.
        let now = DateTime::now();
        let time = format!("{:02}:{:02}:{:02}", now.hour, now.minute, now.second);
        ui.label(Rect::new(PAD, 18, w - 2 * PAD, 40), &time, Font::Bold, 30.0, t.text, Align::Left);
        let date = format!("{}, {} {} {}", now.weekday_name(), now.day, now.month_name(), now.year);
        ui.label(Rect::new(PAD, 58, w - 2 * PAD, 22), &date, Font::Regular, 13.5, t.accent_hover, Align::Left);
        let into_second = rt::time::unix_time_ns() % 1_000_000_000;
        let now_ns = ui.now();
        ui.repaint_at(now_ns + 1_000_000_000 - into_second);
        ui.canvas.fill_rect(Rect::new(0, 94, w, 1), Color::rgba(255, 255, 255, 18));

        // Month header with navigation.
        let title = format!("{} {}", month_name(self.month), self.year);
        ui.label(Rect::new(PAD, 106, 200, 30), &title, Font::Bold, 15.0, t.text, Align::Left);
        if ui.icon_button(Rect::new(w - PAD - 68, 106, 32, 30), Icon::ChevronUp, "Previous month") {
            self.step(-1);
        }
        if ui.icon_button(Rect::new(w - PAD - 32, 106, 32, 30), Icon::ChevronDown, "Next month") {
            self.step(1);
        }

        // Weekday names, Monday first.
        let cell_w = (w - 2 * PAD) / 7;
        let cell_h = 36;
        for (i, name) in ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"].iter().enumerate() {
            let r = Rect::new(PAD + i as i32 * cell_w, 146, cell_w, 22);
            ui.label(r, name, Font::Regular, 12.0, t.text_faint, Align::Center);
        }

        // Day grid: six weeks starting on the Monday on or before the 1st.
        let first = days_from_civil(self.year, self.month, 1);
        // Days shown from the previous month (weekdays from Sunday, 0).
        let lead = (weekday(first) as i64 + 6) % 7;
        let (py, pm) = if self.month == 1 { (self.year - 1, 12) } else { (self.year, self.month - 1) };
        let prev_len = days_in_month(py, pm) as i64;
        let len = days_in_month(self.year, self.month) as i64;
        let today = (now.year, now.month, now.day);
        for cell in 0..42i64 {
            let (col, row) = ((cell % 7) as i32, (cell / 7) as i32);
            let r = Rect::new(PAD + col * cell_w, 172 + row * cell_h, cell_w, cell_h);
            let n = cell - lead + 1;
            let (day, current) = if n < 1 {
                (prev_len + n, false)
            } else if n > len {
                (n - len, false)
            } else {
                (n, true)
            };
            let is_today = current && today == (self.year, self.month, day as u8);
            let label = format!("{day}");
            if is_today {
                let (cx, cy) = (r.x as f32 + r.w as f32 / 2.0, r.y as f32 + r.h as f32 / 2.0);
                ui.canvas.fill_circle(cx, cy, 15.0, t.accent);
                ui.label(r, &label, Font::Bold, 13.0, Color::WHITE, Align::Center);
            } else {
                let c = if current { t.text } else { t.text_faint };
                ui.label(r, &label, Font::Regular, 13.0, c, Align::Center);
            }
        }
        chrome::popup_frame(&mut ui.canvas);
    }
}
