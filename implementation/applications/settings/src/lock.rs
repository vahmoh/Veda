// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The Lock screen page: the password the lock screen asks for, set,
//! changed and removed through the lock service (`vproto::lock`), and how
//! long the screen waits without input before it locks by itself. The
//! service keeps a hash of the password where no other program can read
//! it, and takes a change only with the password set now, so that someone
//! who has the unlocked computer for a moment cannot change it.
//!
//! Hashing a password takes a moment on purpose, so the page asks the lock
//! service from a thread of its own ([`LockLink`]) and says what it is
//! doing meanwhile. What is typed is wiped from memory once it is sent, and
//! as the page is left.

use vproto::input::keys;
use vproto::lock::{DEFAULT_IDLE_S, IDLE_CHOICES_S, LinkError, LockLink, LockReply, LockStatus, MIN_PASSWORD};
use vui::{Align, ButtonKind, Color, Font, Icon, Rect, Ui};
use zeroize::Zeroize;

/// The fields, in the order Tab goes through them.
const FIELDS: [&str; 3] = ["lock-current", "lock-new", "lock-confirm"];

/// The times the screen may lock after, as the page names them, in the
/// order of [`IDLE_CHOICES_S`].
const IDLE_LABELS: [&str; IDLE_CHOICES_S.len()] = ["Never", "1 minute", "5 minutes", "15 minutes", "30 minutes"];

/// What the page is waiting for the lock service to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Busy {
    Setting,
    Removing,
}

/// The Lock screen page.
pub struct LockPage {
    link: Option<LockLink>,
    /// What the lock service said of the lock screen (`None` until it
    /// answers).
    status: Option<Result<LockStatus, LinkError>>,
    current: String,
    new: String,
    confirm: String,
    busy: Option<Busy>,
    /// What the last change came to: done or not, and what to say.
    said: Option<(bool, String)>,
    /// The first field takes the keyboard in the next frame (the page was
    /// shown, or a change is done).
    focus_first: bool,
    /// How long the screen waits without input before it locks (s), as
    /// the lock service said (`None` until it answers); the time being
    /// chosen, until it is taken; why the last one was not.
    idle: Option<Result<u32, LinkError>>,
    choosing: Option<u32>,
    idle_refused: Option<String>,
}

impl LockPage {
    pub fn new() -> LockPage {
        let link = LockLink::start();
        if let Some(l) = &link {
            l.request_status();
            l.request_idle_timeout();
        }
        LockPage {
            link,
            status: None,
            current: String::new(),
            new: String::new(),
            confirm: String::new(),
            busy: None,
            said: None,
            focus_first: true,
            idle: None,
            choosing: None,
            idle_refused: None,
        }
    }

    /// Signalled when the lock service has answered.
    pub fn wait_handle(&self) -> Option<vabi::RawHandle> {
        self.link.as_ref().map(LockLink::event_handle)
    }

    /// Takes the lock service's answers.
    pub fn poll(&mut self) {
        let Some(link) = &self.link else { return };
        for reply in link.take() {
            match reply {
                LockReply::Status(s) => self.status = Some(s),
                LockReply::PasswordSet(result) => {
                    let removed = self.busy == Some(Busy::Removing);
                    self.busy = None;
                    self.said = Some(match result {
                        Ok(()) if removed => (true, "The password is removed. Anyone can unlock the screen.".into()),
                        Ok(()) => (true, "The password is set. The lock screen asks for it from now on.".into()),
                        Err(e) => (false, e.to_string()),
                    });
                    self.focus_first = true;
                    link.request_status();
                    // Until a time is chosen, it follows the password.
                    link.request_idle_timeout();
                }
                LockReply::IdleTimeout(idle) => self.idle = Some(idle),
                LockReply::IdleTimeoutSet(result) => {
                    self.choosing = None;
                    match result {
                        Ok(seconds) => self.idle = Some(Ok(seconds)),
                        Err(e) => self.idle_refused = Some(e.to_string()),
                    }
                }
            }
        }
    }

    /// Chooses how long the screen waits without input before it locks.
    fn choose_idle(&mut self, seconds: u32, label: &str) {
        let Some(link) = &self.link else { return };
        link.set_idle_timeout(seconds);
        self.choosing = Some(seconds);
        self.idle_refused = None;
        println!("lock screen: locking automatically: {}", label.to_lowercase());
    }

    /// The page is left: what was typed goes (and the first field has the
    /// keyboard when it is shown again).
    pub fn leave(&mut self) {
        self.current.zeroize();
        self.new.zeroize();
        self.confirm.zeroize();
        self.focus_first = true;
    }

    /// Sends the change: the password set now (empty when none is) and the
    /// new one (empty to remove it).
    fn send(&mut self, busy: Busy) {
        let Some(link) = &self.link else { return };
        let new = if busy == Busy::Removing { String::new() } else { self.new.clone() };
        link.set_password(self.current.clone(), new);
        self.busy = Some(busy);
        self.said = None;
        self.leave();
        println!(
            "lock screen: {}",
            if busy == Busy::Removing { "removing the password" } else { "setting the password" }
        );
    }

    /// Draws the page in `r`; returns the height used.
    pub fn draw(&mut self, ui: &mut Ui, r: Rect) -> i32 {
        let t = ui.theme().clone();
        let mut y = crate::Settings::page_header(
            ui,
            r,
            "Lock screen",
            "The password that unlocks this computer, and how to lock it.",
        );
        let status = match &self.status {
            None => {
                ui.label(
                    Rect::new(r.x, y, r.w, 24),
                    "Asking the lock screen\u{2026}",
                    Font::Regular,
                    t.font_size,
                    t.text_dim,
                    Align::Left,
                );
                return y + 40 - r.y;
            }
            Some(Err(e)) => {
                let e = e.to_string();
                ui.icon(Rect::new(r.x, y, 20, 24), Icon::Warning, 16.0, t.warning);
                ui.label(Rect::new(r.x + 30, y, r.w - 30, 24), &e, Font::Regular, t.font_size, t.text_dim, Align::Left);
                if ui.button(Rect::new(r.x, y + 40, 120, 34), "Try again")
                    && let Some(l) = &self.link
                {
                    self.status = None;
                    self.idle = None;
                    l.request_status();
                    l.request_idle_timeout();
                }
                return y + 90 - r.y;
            }
            Some(Ok(s)) => *s,
        };

        // Whether a password is set, and what that means.
        let card = Rect::new(r.x, y, r.w, 96);
        ui.card(card);
        let badge = Rect::new(card.x + 22, card.y + 24, 48, 48);
        let (top, bottom) =
            if status.password { (t.accent, t.accent2) } else { (Color::hex(0x6E6E7A), Color::hex(0x45454F)) };
        ui.canvas.fill_rounded_rect_gradient(badge, 24.0, top, bottom);
        ui.icon(badge, Icon::Lock, 22.0, Color::WHITE);
        let (title, detail) = if status.password {
            (
                "A password is set",
                "The lock screen asks for it when the computer starts and whenever the screen is locked.",
            )
        } else {
            (
                "No password is set",
                "Anyone at this computer can unlock the screen. Set a password to keep your files to yourself.",
            )
        };
        let tx = badge.right() + 18;
        ui.label(
            Rect::new(tx, card.y + 18, card.right() - tx - 20, 24),
            title,
            Font::Bold,
            t.font_size + 1.0,
            t.text,
            Align::Left,
        );
        ui.paragraph(Rect::new(tx, card.y + 46, card.right() - tx - 22, 44), detail, t.font_size - 1.0, t.text_dim);
        y = card.bottom() + 28;

        // The form: the password set now (if one is), the new one twice.
        let heading = if status.password { "Change the password" } else { "Set a password" };
        crate::Settings::section_heading(ui, r.x, y, r.w, heading);
        y += 36;
        let busy = self.busy.is_some();
        if core::mem::take(&mut self.focus_first) {
            ui.focus_text_input(FIELDS[if status.password { 0 } else { 1 }]);
        }
        // The labels at the left, the fields beside them (narrower in a
        // narrow window).
        let label_w = 150;
        let field_w = (r.w - label_w).min(360);
        let mut focused = None;
        let row = |y: i32| (Rect::new(r.x, y, label_w, 36), Rect::new(r.x + label_w, y, field_w, 36));
        let mut submitted = false;
        if status.password {
            let (l, f) = row(y);
            ui.label(l, "Current password", Font::Regular, t.font_size, t.text_dim, Align::Left);
            let resp = ui.password_input(f, FIELDS[0], &mut self.current, "The password set now", false);
            focused = focused.or(resp.focused.then_some(0));
            submitted |= resp.submitted;
            y += 46;
        }
        let (l, f) = row(y);
        ui.label(l, "New password", Font::Regular, t.font_size, t.text_dim, Align::Left);
        let placeholder = format!("At least {MIN_PASSWORD} characters");
        let resp = ui.password_input(f, FIELDS[1], &mut self.new, &placeholder, false);
        focused = focused.or(resp.focused.then_some(1));
        submitted |= resp.submitted;
        y += 46;
        let (l, f) = row(y);
        ui.label(l, "Confirm", Font::Regular, t.font_size, t.text_dim, Align::Left);
        let resp = ui.password_input(f, FIELDS[2], &mut self.confirm, "The new password again", false);
        focused = focused.or(resp.focused.then_some(2));
        submitted |= resp.submitted;
        y += 50;
        // Tab goes from field to field.
        if let Some(i) = focused
            && ui.input.key(keys::TAB)
        {
            let first = if status.password { 0 } else { 1 };
            let next = if i + 1 < FIELDS.len() { i + 1 } else { first };
            ui.focus_text_input(FIELDS[next]);
        }

        // What is wrong with what is typed, before it is sent.
        let problem = if self.new.is_empty() && self.confirm.is_empty() {
            None
        } else if self.new.chars().count() < MIN_PASSWORD {
            Some(format!("Use at least {MIN_PASSWORD} characters."))
        } else if self.new != self.confirm {
            Some("The two new passwords are not the same.".into())
        } else if status.password && self.current.is_empty() {
            Some("Type the current password too.".into())
        } else {
            None
        };
        let ready = !busy && status.wait_s == 0 && problem.is_none() && !self.new.is_empty();
        let label = match (busy, status.password) {
            (true, _) => "Saving\u{2026}",
            (false, true) => "Change password",
            (false, false) => "Set password",
        };
        let set = Rect::new(r.x + label_w, y, 170, 38);
        let clicked = ui.button_full(
            set,
            Some(Icon::Check),
            label,
            if ready { ButtonKind::Primary } else { ButtonKind::Secondary },
        );
        if ready && (clicked || submitted) {
            self.send(Busy::Setting);
        }
        if status.password {
            // Beside it, or under it where there is no room.
            let remove = if set.right() + 12 + set.w <= r.right() {
                Rect::new(set.right() + 12, y, set.w, set.h)
            } else {
                y += set.h + 10;
                Rect::new(set.x, y, set.w, set.h)
            };
            let can_remove = !busy && status.wait_s == 0 && !self.current.is_empty();
            let kind = if can_remove { ButtonKind::Danger } else { ButtonKind::Secondary };
            if ui.button_full(remove, Some(Icon::Trash), "Remove password", kind) && can_remove {
                self.send(Busy::Removing);
            }
        }
        y += 52;

        // What the user should know: the wait, what is wrong, what was done.
        let note = if status.wait_s > 0 {
            Some((false, format!("Too many wrong passwords were given. Try again in {} s.", status.wait_s)))
        } else if let Some(p) = problem {
            Some((false, p))
        } else {
            self.said.clone()
        };
        if let Some((ok, text)) = note {
            let (icon, color) = if ok { (Icon::Check, t.success) } else { (Icon::Warning, t.warning) };
            ui.icon(Rect::new(r.x + label_w, y, 20, 24), icon, 16.0, color);
            let text_r = Rect::new(r.x + label_w + 28, y + 3, r.w - label_w - 28, 48);
            let used = ui.paragraph(text_r, &text, t.font_size - 0.5, t.text);
            y += used.max(24) + 12;
        }
        if status.wait_s > 0 {
            ui.repaint_at(ui.now() + 1_000_000_000);
        }

        // When the screen locks by itself.
        y += 10;
        crate::Settings::section_heading(ui, r.x, y, r.w, "Lock automatically after");
        y += 36;
        y = self.draw_idle(ui, r, y);

        // How to lock.
        y += 10;
        ui.icon(Rect::new(r.x, y, 20, 24), Icon::Info, 16.0, t.text_faint);
        ui.paragraph(
            Rect::new(r.x + 30, y + 3, r.w - 30, 44),
            "Lock the screen at any time with Super+L, from the start menu, or by asking the agent. Wrong passwords make the lock screen wait before it takes another.",
            t.font_size - 1.0,
            t.text_dim,
        );
        y + 60 - r.y
    }

    /// The choice of how long the screen waits without input before it
    /// locks, from `y`; returns the y below it.
    fn draw_idle(&mut self, ui: &mut Ui, r: Rect, mut y: i32) -> i32 {
        let t = ui.theme().clone();
        let shown = match &self.idle {
            None => {
                let wait = "Asking the lock screen\u{2026}";
                ui.label(Rect::new(r.x, y, r.w, 24), wait, Font::Regular, t.font_size, t.text_dim, Align::Left);
                return y + 34;
            }
            Some(Err(e)) => {
                let e = e.to_string();
                ui.icon(Rect::new(r.x, y, 20, 24), Icon::Warning, 16.0, t.warning);
                ui.label(Rect::new(r.x + 30, y, r.w - 30, 24), &e, Font::Regular, t.font_size, t.text_dim, Align::Left);
                return y + 34;
            }
            Some(Ok(seconds)) => *seconds,
        };
        // The time being chosen shows as chosen while the lock service
        // takes it.
        let selected = self.choosing.unwrap_or(shown);
        let mut x = r.x;
        for (seconds, label) in IDLE_CHOICES_S.into_iter().zip(IDLE_LABELS) {
            let w = ui.measure(label, Font::Bold, t.font_size) as i32 + 32;
            // On the next row, where there is no room.
            if x > r.x && x + w > r.right() {
                x = r.x;
                y += 44;
            }
            let kind = if seconds == selected { ButtonKind::Primary } else { ButtonKind::Secondary };
            let clicked = ui.button_full(Rect::new(x, y, w, 36), None, label, kind);
            if clicked && seconds != selected && self.choosing.is_none() {
                self.choose_idle(seconds, label);
            }
            x += w + 8;
        }
        y += 50;
        if let Some(e) = &self.idle_refused {
            ui.icon(Rect::new(r.x, y, 20, 24), Icon::Warning, 16.0, t.warning);
            let used = ui.paragraph(Rect::new(r.x + 30, y + 3, r.w - 30, 48), e, t.font_size - 0.5, t.text);
            y += used.max(24) + 12;
        }
        let detail = format!(
            "The screen locks by itself once the keyboard and the pointer have rested this long, but not while a \
             video, a slideshow or a game plays in view. Until you choose, it locks after {} minutes when a password \
             is set, and never when none is.",
            DEFAULT_IDLE_S / 60
        );
        let used = ui.paragraph(Rect::new(r.x, y, r.w, 66), &detail, t.font_size - 1.0, t.text_dim);
        y + used.max(24) + 18
    }
}
