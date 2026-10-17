// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The lock screen's window: what it draws, and what keys and clicks do
//! there. Its states, what moves it from one to the next and where
//! everything goes are `lock_core`'s (`screen`, `layout`); this draws them.
//!
//! * **Resting**: the wallpaper under a veil, the time large in its upper
//!   part and the date under it, and at the bottom how to unlock.
//! * **Asking**: the wallpaper blurs and dims, and the card rises: the
//!   user's picture in the ring of Veda's figure (OS1's, as the agent and
//!   the splash have it), their name, the password field with its button,
//!   and a line under it for what the user should know (the wait after
//!   wrong passwords, a wrong password, Caps Lock).
//! * **Checking**: a light runs round the ring, and round the button.
//! * **Unlocking**: the ring swells and fades and everything lifts away;
//!   then the whole lock screen fades out over the desktop.
//!
//! The field takes what is typed at the keyboard alone: nothing is copied
//! from it, and nothing pasted into it (`vui`'s `secret_input`). What is
//! typed is wiped from memory as it goes to be checked, and the field
//! keeps room for the longest password, so that its text is never moved
//! (and left behind) as it grows.

use core::f32::consts::FRAC_PI_2;

use lock_core::layout::Layout;
use lock_core::screen::{Due, Phase, Screen, Submit, Tone};
use shell_common::presence;
use vgfx::{Align, Color, Rect};
use vproto::display::{Cursor, WindowKind, WindowSpec, modifiers};
use vproto::input::keys;
use vproto::lock::MAX_PASSWORD;
use vrt::time::DateTime;
use vui::window::{Display, WindowError};
use vui::{Font, Host, Icon, Ui};
use zeroize::{Zeroize, Zeroizing};

use crate::backdrop::Backdrop;

/// The one account, as the start menu names it.
const USER: &str = "User";
/// The resting screen's hint.
const HINT: &str = "Press any key or click to unlock";
/// The colours of the line under the field: a warning (Caps Lock) and an
/// error (a wrong password, the wait), light enough to read on the dimmed
/// wallpaper.
const AMBER: Color = Color::hex(0xFFD27A);
const ROSE: Color = Color::hex(0xFFA3A6);
/// What the ring turns as a wrong password comes back.
const DANGER: Color = Color::hex(0xFF5A63);

/// How long a wallpaper chosen anew takes to come in over the one shown.
const CROSS_FADE_NS: u64 = 450_000_000;

/// What the window asks of the lock service.
pub enum Request {
    /// The password typed is to be checked (taken out of the field).
    Check(Zeroizing<String>),
    /// The dissolve is over: the screen is to be unlocked.
    Unlock,
    /// The fade is over: the window may close.
    Close,
}

/// White, `a` opaque (0 to 1).
fn white(a: f32) -> Color {
    Color::rgba(255, 255, 255, (a.clamp(0.0, 1.0) * 255.0 + 0.5) as u8)
}

/// `c` with its opacity multiplied by `a` (0 to 1).
fn faded(c: Color, a: f32) -> Color {
    let alpha = ((c.0 >> 24) as f32 * a.clamp(0.0, 1.0) + 0.5) as u32;
    Color((c.0 & 0x00FF_FFFF) | alpha << 24)
}

/// Text centred in `r`, with a soft shadow under it, legible on any
/// picture.
fn shadowed(ui: &mut Ui, r: Rect, text: &str, font: Font, size: f32, color: Color) {
    let f = ui.ctx.font(font);
    let shadow = faded(Color::rgba(0, 0, 0, 120), (color.0 >> 24) as f32 / 255.0);
    let drop = (size / 28.0).max(1.0) as i32;
    ui.ctx.text.draw_in(&mut ui.canvas, f, size, r.translate(0, drop), text, shadow, Align::Center);
    ui.ctx.text.draw_in(&mut ui.canvas, f, size, r, text, color, Align::Center);
}

/// What the window draws from, beside its host.
struct Face {
    screen: Screen,
    /// Where everything goes, on a screen of `size`.
    layout: Layout,
    size: (i32, i32),
    /// What is typed.
    field: Zeroizing<String>,
    /// How many characters went to be checked (shown as bullets meanwhile).
    sent: usize,
    caps_lock: bool,
}

/// The lock screen's window.
pub struct View {
    host: Host,
    face: Face,
    /// When it was opened, and whether it has drawn itself since.
    opened: u64,
    drawn: bool,
}

impl View {
    /// Opens the lock screen on a `w` x `h` screen, as `screen` says it is.
    pub fn open(display: &Display, w: i32, h: i32, screen: Screen) -> Result<View, WindowError> {
        let mut spec = WindowSpec::new("Lock screen", w as u32, h as u32);
        spec.kind = WindowKind::Lock;
        spec.x = 0;
        spec.y = 0;
        spec.resizable = false;
        spec.app_id = "lock".into();
        let mut host = Host::new(display, spec)?;
        // The backdrop covers every pixel.
        host.clear = false;
        let face = Face {
            screen,
            layout: Layout::new(w, h),
            size: (w, h),
            field: Zeroizing::new(String::with_capacity(MAX_PASSWORD + 4)),
            sent: 0,
            caps_lock: false,
        };
        Ok(View { host, face, opened: vrt::time::now_ns(), drawn: false })
    }

    /// When it was opened, and whether it has drawn itself since.
    pub fn opened(&self) -> (u64, bool) {
        (self.opened, self.drawn)
    }

    pub fn host(&self) -> &Host {
        &self.host
    }

    /// Whether the window system closed the window (it went away).
    pub fn closed(&self) -> bool {
        self.host.window.closed
    }

    pub fn phase(&self) -> Phase {
        self.face.screen.phase()
    }

    /// Draws it again at the next opportunity (what it shows changed).
    pub fn invalidate(&mut self) {
        self.host.invalidate();
    }

    /// The password sent at `now` was `right`, or not.
    pub fn checked(&mut self, now: u64, right: bool) {
        self.face.screen.checked(now, right);
        self.face.sent = 0;
        self.host.invalidate();
    }

    /// A password was set or removed meanwhile.
    pub fn set_password(&mut self, password: bool) {
        self.face.screen.set_password(password);
        self.host.invalidate();
    }

    /// The screen was locked again at `now` as it unlocked: the lock screen
    /// rests again, at once.
    pub fn relock(&mut self, now: u64) {
        let password = self.face.screen.password();
        self.face.screen = Screen::relocked(now, password);
        self.face.field.zeroize();
        self.face.sent = 0;
        self.host.invalidate();
    }

    /// Takes its events and draws over `backdrop` (and `previous`, the one
    /// it replaced, and since when), `wait_s` seconds left before another
    /// password is taken: what it asks of the service.
    pub fn pump(&mut self, backdrop: &Backdrop, previous: Option<(&Backdrop, u64)>, wait_s: u64) -> Vec<Request> {
        if !self.drawn {
            // It appears from its first frame, which may have waited for the
            // wallpaper.
            self.drawn = true;
            let password = self.face.screen.password();
            self.face.screen = Screen::new(vrt::time::now_ns(), password);
        }
        let mut out = Vec::new();
        let View { host, face, .. } = self;
        host.pump(|ui| face.update(ui, backdrop, previous, wait_s, &mut out));
        out
    }
}

impl Face {
    fn update(
        &mut self,
        ui: &mut Ui,
        backdrop: &Backdrop,
        previous: Option<(&Backdrop, u64)>,
        wait_s: u64,
        out: &mut Vec<Request>,
    ) {
        let now = ui.now();
        // The window is the screen's size, which the window system says.
        if (ui.width, ui.height) != self.size {
            self.size = (ui.width, ui.height);
            self.layout = Layout::new(ui.width, ui.height);
        }
        self.input(ui, now, wait_s, out);
        if let Some(due) = self.screen.tick(now, self.field.is_empty()) {
            out.push(match due {
                Due::Unlock => Request::Unlock,
                Due::Close => Request::Close,
            });
        }

        let card = self.screen.card(now);
        let gone = self.screen.unlocking(now);
        // A wallpaper chosen anew comes in over the one it replaces.
        match previous.filter(|(_, since)| now.saturating_sub(*since) < CROSS_FADE_NS) {
            Some((old, since)) => {
                old.draw(&mut ui.canvas, card);
                let t = now.saturating_sub(since) as f32 / CROSS_FADE_NS as f32;
                backdrop.draw_over(&mut ui.canvas, card, lock_core::screen::ease_in_out(t));
                ui.repaint_at(now + lock_core::screen::FRAME_NS);
            }
            None => backdrop.draw(&mut ui.canvas, card),
        }
        self.clock(ui, gone);
        let hint = (1.0 - card) * (1.0 - gone);
        if hint > 0.01 {
            self.hint(ui, hint);
        }
        // Drawn from the frame the screen wakes in (unseen as yet), so that
        // the field takes the key that woke it.
        if card > 0.01 || self.screen.phase() != Phase::Resting {
            self.card(ui, now, card, gone, wait_s, out);
        }
        // Fading in over the window system's backdrop, or out over the
        // desktop.
        let opacity = self.screen.opacity(now);
        if opacity < 0.999 {
            let a = (opacity.clamp(0.0, 1.0) * 255.0) as u32;
            let (pixels, _) = ui.canvas.pixels_mut();
            for px in pixels.iter_mut() {
                *px = vgfx::color::scale(*px, a);
                vgfx::color::keep_scalar();
            }
        }

        if let Some(t) = self.screen.next_frame(now, self.field.is_empty()) {
            ui.repaint_at(t);
        }
        // The clock's next minute, and the wait's next second.
        let wall = vrt::time::unix_time_ns();
        ui.repaint_at(now + 60_000_000_000 - wall % 60_000_000_000);
        if wait_s > 0 {
            ui.repaint_at(now + 1_000_000_000 - wall % 1_000_000_000);
        }
    }

    /// What the keys and the clicks of this frame do (the field takes its
    /// own as it is drawn).
    fn input(&mut self, ui: &mut Ui, now: u64, wait_s: u64, out: &mut Vec<Request>) {
        if let Some(k) = ui.input.keys.last() {
            self.caps_lock = k.modifiers & modifiers::CAPS_LOCK != 0;
        }
        // Any key but Esc, or a click, asks a resting screen for the
        // password: a key that types a character types its first, as the
        // field is drawn in this frame.
        let key = ui.input.keys.iter().any(|k| k.code != keys::ESC);
        let click = ui.input.pressed.iter().any(|p| *p);
        if key || click {
            self.screen.input(now);
        }
        if !self.screen.takes_keys() {
            return;
        }
        if ui.input.key(keys::ESC) {
            if self.field.is_empty() {
                self.screen.rest(now);
            } else {
                self.field.zeroize();
                self.screen.typed(now);
            }
        }
        // With no password set, Enter unlocks (the field is not there to
        // take it).
        if !self.screen.password() && (ui.input.key(keys::ENTER) || ui.input.key(keys::KPENTER)) {
            self.submit(now, wait_s, out);
        }
    }

    /// Enter, or the button: the password goes to be checked, or (none
    /// set) the screen unlocks.
    fn submit(&mut self, now: u64, wait_s: u64, out: &mut Vec<Request>) {
        if self.screen.submit(now, !self.field.is_empty(), wait_s) == Submit::Check {
            self.sent = self.field.chars().count();
            let mut password = Zeroizing::new(String::with_capacity(self.field.len()));
            password.push_str(&self.field);
            self.field.zeroize();
            out.push(Request::Check(password));
        }
    }

    /// The time and the date, lifting away as the screen unlocks.
    fn clock(&self, ui: &mut Ui, gone: f32) {
        let l = &self.layout;
        let lift = -(gone * l.rise as f32 * 1.6) as i32;
        let a = 1.0 - gone;
        let t = DateTime::now();
        let time = format!("{:02}:{:02}", t.hour, t.minute);
        shadowed(ui, l.clock.translate(0, lift), &time, Font::Regular, l.clock_size, white(0.97 * a));
        let date = format!("{} {} {}", t.weekday_name(), t.day, t.month_name());
        shadowed(ui, l.date.translate(0, lift), &date, Font::Regular, l.date_size, white(0.86 * a));
    }

    /// How to unlock, at the bottom of a resting screen.
    fn hint(&self, ui: &mut Ui, a: f32) {
        let l = &self.layout;
        let size = l.hint_size;
        let icon = (size * 1.2) as i32;
        let gap = (8.0 * l.unit) as i32;
        let text_w = ui.measure(HINT, Font::Regular, size) as i32;
        let x = (ui.width - (icon + gap + text_w)) / 2;
        let glyph = Rect::new(x, l.hint.y + (l.hint.h - icon) / 2, icon, icon);
        Icon::Lock.draw(&mut ui.canvas, glyph.translate(0, 1), icon as f32, faded(Color::rgba(0, 0, 0, 110), a));
        Icon::Lock.draw(&mut ui.canvas, glyph, icon as f32, white(0.82 * a));
        let text = Rect::new(x + icon + gap, l.hint.y, text_w + 2, l.hint.h);
        let f = ui.ctx.font(Font::Regular);
        ui.ctx.text.draw_in(
            &mut ui.canvas,
            f,
            size,
            text.translate(0, 1),
            HINT,
            faded(Color::rgba(0, 0, 0, 120), a),
            Align::Left,
        );
        ui.ctx.text.draw_in(&mut ui.canvas, f, size, text, HINT, white(0.82 * a), Align::Left);
    }

    /// The card: the ring and the user's picture, their name, the field and
    /// the line under it, `card` of the way into view and `gone` of the way
    /// out.
    fn card(&mut self, ui: &mut Ui, now: u64, card: f32, gone: f32, wait_s: u64, out: &mut Vec<Request>) {
        let l = self.layout;
        let rise = ((1.0 - card) * l.rise as f32) as i32 - (gone * l.rise as f32) as i32;
        let a = card * (1.0 - gone);
        let th = ui.theme().clone();
        let (cx, cy, r) = l.avatar;
        let (fx, fy) = (cx as f32, (cy + rise) as f32);

        // A soft light around the ring while it asks; the ring itself,
        // turning red for a wrong password, swelling as it unlocks, with a
        // light running round it while the password is checked.
        presence::glow(&mut ui.canvas, fx, fy, l.ring, l.ring_width, l.ring * 0.34, 0.16 * a);
        match self.screen.checking(now) {
            Some((angle, strength)) => {
                let peak = (255.0 * a) as u8;
                let base = (90.0 * a + 60.0 * a * (1.0 - strength)) as u8;
                let w = l.ring_width * 1.6;
                presence::ring_with_light(&mut ui.canvas, fx, fy, l.ring, w, angle - FRAC_PI_2, base, peak);
            }
            None => {
                let flash = self.screen.flash(now);
                let color = white(1.0).lerp(DANGER, flash);
                let swell = 1.0 + 0.32 * gone;
                let ring = faded(color, (0.78 + 0.22 * flash) * a * (1.0 - gone * 0.4));
                let w = l.ring_width * (1.0 + 0.6 * flash + gone);
                presence::ring(&mut ui.canvas, fx, fy, l.ring * swell, w, 0.0, 0.0, ring);
            }
        }

        // The user's picture: the accent's gradient and the user's glyph, as
        // in the start menu.
        let pic = Rect::new(cx - r, cy + rise - r, 2 * r, 2 * r);
        let unit = l.unit;
        ui.canvas.draw_shadow(
            pic.translate(0, (6.0 * unit) as i32),
            r,
            (10.0 * unit) as i32,
            faded(Color::BLACK, 0.22 * a),
        );
        ui.canvas.fill_rounded_rect_gradient(pic, r as f32, faded(th.accent, a), faded(th.accent2, a));
        Icon::User.draw(&mut ui.canvas, pic, r as f32 * 1.05, white(a));

        shadowed(ui, l.name.translate(0, rise), USER, Font::Bold, l.name_size, white(a));
        if self.screen.password() {
            self.field(ui, now, rise, a, wait_s, out);
        } else {
            self.unlock_button(ui, now, rise, a, wait_s, out);
        }
        if let Some((tone, text)) = self.screen.message(now, wait_s, self.caps_lock) {
            let color = match tone {
                Tone::Info => white(0.82),
                Tone::Warning => AMBER,
                Tone::Error => ROSE,
            };
            shadowed(ui, l.message.translate(0, rise), &text, Font::Regular, l.message_size, faded(color, a));
        }
    }

    /// The password field, a pill of frosted glass, with its button.
    fn field(&mut self, ui: &mut Ui, now: u64, rise: i32, a: f32, wait_s: u64, out: &mut Vec<Request>) {
        let l = self.layout;
        let shake = (self.screen.shake(now) * l.shake as f32) as i32;
        let f = l.field.translate(shake, rise);
        let radius = f.h as f32 / 2.0;
        let asking = self.screen.takes_keys() && wait_s == 0;
        let checking = self.screen.phase() == Phase::Checking;
        ui.canvas.draw_shadow(
            f.translate(0, (3.0 * l.unit) as i32),
            f.h / 2,
            (12.0 * l.unit) as i32,
            faded(Color::BLACK, 0.22 * a),
        );
        ui.canvas.fill_rounded_rect(f, radius, faded(Color::rgba(255, 255, 255, 34), a));
        let border = if asking { 0.62 } else { 0.28 };
        ui.canvas.stroke_rounded_rect(f, radius, (1.5 * l.unit).max(1.0), white(border * a));

        let submit = l.submit.translate(shake, rise);
        let pad = (12.0 * l.unit) as i32;
        let text = Rect::new(f.x + pad, f.y, submit.x - f.x - pad - (4.0 * l.unit) as i32, f.h);
        if asking {
            // The toolkit's text box, made see-through: the pill is its box.
            let theme = ui.ctx.theme.clone();
            {
                let t = &mut ui.ctx.theme;
                for c in [&mut t.bg, &mut t.control, &mut t.control_hover, &mut t.border, &mut t.accent] {
                    *c = Color::TRANSPARENT;
                }
                t.text = white(a);
                t.text_faint = white(0.5 * a);
                t.selection = Color::rgba(255, 255, 255, 56);
                t.font_size = l.text_size;
                t.radius = 0.0;
            }
            ui.focus_text_input("password");
            let resp = ui.secret_input(text, "password", &mut self.field, "Password");
            ui.ctx.theme = theme;
            if resp.changed {
                self.screen.typed(now);
            }
            if resp.submitted {
                self.submit(now, wait_s, out);
            }
        } else {
            // Being checked: the bullets that went; waiting: nothing taken.
            let bullets: String = core::iter::repeat_n('\u{2022}', if checking { self.sent } else { 0 }).collect();
            let (shown, color) =
                if bullets.is_empty() { ("Password", white(0.32 * a)) } else { (bullets.as_str(), white(0.75 * a)) };
            let font = ui.ctx.font(Font::Regular);
            let inner = text.inset((10.0 * l.unit).min(10.0) as i32, 0, 0, 0);
            ui.ctx.text.draw_in(&mut ui.canvas, font, l.text_size, inner, shown, color, Align::Left);
        }

        // The button: bright once there is something to check.
        let resp = ui.interact(ui.id("submit"), submit);
        let ready = asking && !self.field.is_empty();
        let (scx, scy) = (submit.x as f32 + submit.w as f32 / 2.0, submit.y as f32 + submit.h as f32 / 2.0);
        let th = ui.theme().clone();
        let bg = match (ready, resp.hovered) {
            (true, true) => th.accent_hover,
            (true, false) => th.accent,
            (false, _) => Color::rgba(255, 255, 255, 30),
        };
        ui.canvas.fill_circle(scx, scy, submit.w as f32 / 2.0, faded(bg, a));
        match self.screen.checking(now) {
            Some((angle, _)) => {
                let (r, w) = (submit.w as f32 * 0.24, (2.0 * l.unit).max(1.5));
                presence::ring_with_light(
                    &mut ui.canvas,
                    scx,
                    scy,
                    r,
                    w,
                    angle - FRAC_PI_2,
                    (60.0 * a) as u8,
                    (255.0 * a) as u8,
                );
            }
            None => {
                let glyph = white(if ready { 1.0 } else { 0.55 } * a);
                Icon::ChevronRight.draw(&mut ui.canvas, submit, submit.w as f32 * 0.55, glyph);
            }
        }
        if resp.hovered && ready {
            ui.set_cursor(Cursor::Hand);
        }
        if resp.clicked && ready {
            self.submit(now, wait_s, out);
        }
    }

    /// With no password set: a button that unlocks (Enter does too).
    fn unlock_button(&mut self, ui: &mut Ui, now: u64, rise: i32, a: f32, wait_s: u64, out: &mut Vec<Request>) {
        let l = self.layout;
        let w = l.field.w.min((220.0 * l.unit) as i32);
        let r = Rect::new(l.field.x + (l.field.w - w) / 2, l.field.y + rise, w, l.field.h);
        let resp = ui.interact(ui.id("unlock"), r);
        let th = ui.theme().clone();
        let (top, bottom) = if resp.hovered { (th.accent_hover, th.accent2) } else { (th.accent, th.accent2) };
        ui.canvas.draw_shadow(
            r.translate(0, (3.0 * l.unit) as i32),
            r.h / 2,
            (12.0 * l.unit) as i32,
            faded(Color::BLACK, 0.25 * a),
        );
        ui.canvas.fill_rounded_rect_gradient(r, r.h as f32 / 2.0, faded(top, a), faded(bottom, a));
        ui.label(r, "Unlock", Font::Bold, l.text_size, white(a), Align::Center);
        if resp.hovered {
            ui.set_cursor(Cursor::Hand);
        }
        if resp.clicked {
            self.submit(now, wait_s, out);
        }
    }
}
