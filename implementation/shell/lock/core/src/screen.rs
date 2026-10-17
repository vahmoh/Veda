// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The lock screen's states, what moves it from one to the next, and how
//! what it shows moves with time.
//!
//! ```text
//!            key or click          Enter              right
//!  Resting ───────────────▶ Asking ─────▶ Checking ─────────▶ Unlocking ──▶ Fading ──▶ Gone
//!     ▲                      │  ▲            │                 (locked)     (unlocked)
//!     └─ left alone, empty ──┘  └── wrong ───┘
//! ```
//!
//! * **Resting**: the time and the date over the wallpaper. A key or a
//!   click asks for the password, and a key that types a character types
//!   the password's first.
//! * **Asking**: the wallpaper blurs and dims, and the user's ring, the
//!   field and the line under it rise into view. With no password set,
//!   Enter (or the button) unlocks. Left alone with nothing typed for
//!   [`REST_AFTER_NS`], or Esc on an empty field, it rests again.
//! * **Checking**: the password is being hashed. A light runs round the
//!   ring and nothing typed is taken. A wrong password shakes the field
//!   and says so; too many make the next wait ([`crate::throttle`]).
//! * **Unlocking**: the ring swells and fades and the words lift away,
//!   the screen still locked; then the window system unlocks it
//!   ([`Due::Unlock`]) and the lock screen fades out over the desktop
//!   (**Fading**), and closes ([`Due::Close`]).
//!
//! A lock screen fades in over the window system's backdrop as it appears
//! ([`APPEAR_NS`]), but for one shown again over its own fading self.
//! Every animation is a function of the time, so that a frame drawn late
//! (under emulation) shows what it should then, rather than lagging.

use alloc::format;
use alloc::string::String;
use core::f32::consts::TAU;

/// How long a lock screen takes to fade in as it appears.
pub const APPEAR_NS: u64 = 240_000_000;
/// How long the card takes to rise into view (and the wallpaper to blur).
pub const ASK_NS: u64 = 320_000_000;
/// How long the card stays with nothing typed before the screen rests.
pub const REST_AFTER_NS: u64 = 30_000_000_000;
/// How long the field shakes after a wrong password, and how long the
/// line under it says so (unless the user types again first).
pub const SHAKE_NS: u64 = 480_000_000;
pub const WRONG_SHOWN_NS: u64 = 6_000_000_000;
/// How long the dissolve takes before the screen is unlocked, and the fade
/// over the desktop after.
pub const UNLOCK_NS: u64 = 420_000_000;
pub const FADE_NS: u64 = 380_000_000;
/// The pace of frames while something moves.
pub const FRAME_NS: u64 = 16_000_000;
/// How fast the light runs round the ring while a password is checked
/// (turns a second).
const CHECK_TURNS: f32 = 0.9;

/// Where the lock screen is (see the module's comment).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Resting,
    Asking,
    Checking,
    Unlocking,
    Fading,
    Gone,
}

/// What Enter (or the button) does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Submit {
    /// The password typed is to be checked.
    Check,
    /// Nothing: no password is taken now (the screen is not asking for
    /// one, nothing is typed, or too many were wrong).
    Nothing,
    /// The screen unlocks (no password is set).
    Unlock,
}

/// What is due as time passes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Due {
    /// The dissolve is over: the window system unlocks the screen.
    Unlock,
    /// The fade is over: the lock screen closes.
    Close,
}

/// How a line under the field reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Info,
    Warning,
    Error,
}

/// `t` (0 to 1) eased out: fast, then settling.
pub fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t) * (1.0 - t) * (1.0 - t)
}

/// `t` (0 to 1) eased in and out.
pub fn ease_in_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// How far `now` is through `duration` from `since`, 0 to 1.
fn progress(now: u64, since: u64, duration: u64) -> f32 {
    (now.saturating_sub(since) as f32 / duration.max(1) as f32).clamp(0.0, 1.0)
}

/// A wait as the line under the field says it: "28 seconds", "5 minutes".
pub fn duration(seconds: u64) -> String {
    match seconds {
        0 | 1 => String::from("1 second"),
        2..=59 => format!("{seconds} seconds"),
        _ => match seconds.div_ceil(60) {
            1 => String::from("1 minute"),
            m => format!("{m} minutes"),
        },
    }
}

/// The lock screen (see the module's comment).
#[derive(Debug, Clone)]
pub struct Screen {
    phase: Phase,
    /// When the phase began (ns).
    since: u64,
    /// How far the card has risen when it last changed direction, where it
    /// goes (0 resting, 1 asking), and since when.
    card_from: f32,
    card_to: f32,
    card_since: u64,
    /// When the user last pressed a key or clicked.
    last_input: u64,
    /// When the last wrong password came back, while the line says so.
    wrong_at: Option<u64>,
    /// A password is set.
    password: bool,
    /// When it appeared, fading in (`None`: shown at once).
    appeared: Option<u64>,
}

impl Screen {
    /// A lock screen appearing at `now`, resting; `password` if one is set.
    pub fn new(now: u64, password: bool) -> Screen {
        Screen {
            phase: Phase::Resting,
            since: now,
            card_from: 0.0,
            card_to: 0.0,
            card_since: now,
            last_input: now,
            wrong_at: None,
            password,
            appeared: Some(now),
        }
    }

    /// The lock screen shown again at `now` (the screen was locked again as
    /// it unlocked): resting, at once.
    pub fn relocked(now: u64, password: bool) -> Screen {
        Screen { appeared: None, ..Screen::new(now, password) }
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// Whether a password is set (the screen asks for it).
    pub fn password(&self) -> bool {
        self.password
    }

    /// A password was set or removed meanwhile.
    pub fn set_password(&mut self, password: bool) {
        self.password = password;
    }

    /// Whether the field takes what is typed.
    pub fn takes_keys(&self) -> bool {
        self.phase == Phase::Asking
    }

    fn enter(&mut self, phase: Phase, now: u64) {
        self.phase = phase;
        self.since = now;
    }

    fn card_towards(&mut self, to: f32, now: u64) {
        if to != self.card_to {
            self.card_from = self.card(now);
            self.card_to = to;
            self.card_since = now;
        }
    }

    /// The user pressed a key or clicked at `now`: a resting screen asks
    /// for the password.
    pub fn input(&mut self, now: u64) {
        self.last_input = now;
        if self.phase == Phase::Resting {
            self.enter(Phase::Asking, now);
            self.card_towards(1.0, now);
        }
    }

    /// The user typed into the field (or emptied it) at `now`: the line no
    /// longer says the last password was wrong.
    pub fn typed(&mut self, now: u64) {
        self.input(now);
        self.wrong_at = None;
    }

    /// Esc on an empty field at `now`: the screen rests.
    pub fn rest(&mut self, now: u64) {
        if self.phase == Phase::Asking {
            self.enter(Phase::Resting, now);
            self.card_towards(0.0, now);
            self.wrong_at = None;
        }
    }

    /// Enter (or the button) at `now`, with something `typed` (or not) and
    /// `wait_s` seconds left before another password is taken.
    pub fn submit(&mut self, now: u64, typed: bool, wait_s: u64) -> Submit {
        self.last_input = now;
        if self.phase != Phase::Asking {
            return Submit::Nothing;
        }
        if !self.password {
            self.enter(Phase::Unlocking, now);
            return Submit::Unlock;
        }
        if wait_s > 0 || !typed {
            return Submit::Nothing;
        }
        self.enter(Phase::Checking, now);
        Submit::Check
    }

    /// The password checked came back at `now`, `right` or not.
    pub fn checked(&mut self, now: u64, right: bool) {
        if self.phase != Phase::Checking {
            return;
        }
        if right {
            self.wrong_at = None;
            self.enter(Phase::Unlocking, now);
        } else {
            self.wrong_at = Some(now);
            self.last_input = now;
            self.enter(Phase::Asking, now);
        }
    }

    /// Moves on to `now` (the field `empty` or not): what is due.
    pub fn tick(&mut self, now: u64, empty: bool) -> Option<Due> {
        match self.phase {
            Phase::Asking if empty && now.saturating_sub(self.last_input) >= REST_AFTER_NS => {
                self.rest(now);
                None
            }
            Phase::Unlocking if now.saturating_sub(self.since) >= UNLOCK_NS => {
                self.enter(Phase::Fading, now);
                Some(Due::Unlock)
            }
            Phase::Fading if now.saturating_sub(self.since) >= FADE_NS => {
                self.enter(Phase::Gone, now);
                Some(Due::Close)
            }
            _ => None,
        }
    }

    /// When a frame is next wanted at `now` (`None`: nothing moves until
    /// the user does something), the field `empty` or not.
    pub fn next_frame(&self, now: u64, empty: bool) -> Option<u64> {
        let card_moving = self.card_from != self.card_to && now.saturating_sub(self.card_since) < ASK_NS;
        let moving = card_moving
            || self.appeared.is_some_and(|at| now.saturating_sub(at) < APPEAR_NS)
            || self.wrong_at.is_some_and(|at| now.saturating_sub(at) < SHAKE_NS)
            || matches!(self.phase, Phase::Checking | Phase::Unlocking | Phase::Fading);
        if moving {
            return Some(now + FRAME_NS);
        }
        let mut due: Option<u64> = None;
        let mut at = |t: u64| due = Some(due.map_or(t, |d| d.min(t)));
        if let Some(w) = self.wrong_at {
            at(w + WRONG_SHOWN_NS);
        }
        if self.phase == Phase::Asking && empty {
            at(self.last_input + REST_AFTER_NS);
        }
        due
    }

    /// How far the card has risen into view (0 resting, 1 asking).
    pub fn card(&self, now: u64) -> f32 {
        let k = ease_out(progress(now, self.card_since, ASK_NS));
        self.card_from + (self.card_to - self.card_from) * k
    }

    /// The field's sideways offset after a wrong password, as a fraction of
    /// how far it shakes (-1 to 1): a few swings, dying away.
    pub fn shake(&self, now: u64) -> f32 {
        let Some(at) = self.wrong_at else { return 0.0 };
        let t = progress(now, at, SHAKE_NS);
        if t >= 1.0 {
            return 0.0;
        }
        libm::sinf(t * 3.0 * TAU) * (1.0 - t) * (1.0 - t)
    }

    /// How strongly the ring shows that the last password was wrong (1 as
    /// it comes back, dying away with the shake).
    pub fn flash(&self, now: u64) -> f32 {
        let Some(at) = self.wrong_at else { return 0.0 };
        let t = progress(now, at, SHAKE_NS);
        (1.0 - t) * (1.0 - t)
    }

    /// Whether the line says the last password was wrong at `now`.
    pub fn wrong(&self, now: u64) -> bool {
        self.wrong_at.is_some_and(|at| now.saturating_sub(at) < WRONG_SHOWN_NS)
    }

    /// Where the light running round the ring is while a password is
    /// checked (radians, from the top, clockwise), and how bright (0 to 1,
    /// coming up as it starts).
    pub fn checking(&self, now: u64) -> Option<(f32, f32)> {
        if self.phase != Phase::Checking {
            return None;
        }
        let s = now.saturating_sub(self.since) as f32 / 1e9;
        let turns = s * CHECK_TURNS;
        Some(((turns - libm::floorf(turns)) * TAU, ease_out(s / 0.25)))
    }

    /// How far the dissolve has gone (0 to 1).
    pub fn unlocking(&self, now: u64) -> f32 {
        match self.phase {
            Phase::Unlocking => ease_in_out(progress(now, self.since, UNLOCK_NS)),
            Phase::Fading | Phase::Gone => 1.0,
            _ => 0.0,
        }
    }

    /// How opaque the lock screen is: fading in as it appears, then 1 until
    /// it fades out over the desktop.
    pub fn opacity(&self, now: u64) -> f32 {
        let shown = self.appeared.map_or(1.0, |at| ease_out(progress(now, at, APPEAR_NS)));
        match self.phase {
            Phase::Fading => shown.min(1.0 - ease_in_out(progress(now, self.since, FADE_NS))),
            Phase::Gone => 0.0,
            _ => shown,
        }
    }

    /// What the line under the field says at `now`, if anything, with
    /// `wait_s` seconds left before another password is taken and Caps Lock
    /// on or not: the wait, that the last password was wrong, that Caps
    /// Lock is on, or that no password is set (in that order).
    pub fn message(&self, now: u64, wait_s: u64, caps_lock: bool) -> Option<(Tone, String)> {
        if !matches!(self.phase, Phase::Asking | Phase::Checking) {
            return None;
        }
        if !self.password {
            return Some((Tone::Info, String::from("No password is set. Press Enter to unlock.")));
        }
        if wait_s > 0 {
            return Some((Tone::Error, format!("Too many tries. Try again in {}.", duration(wait_s))));
        }
        if self.wrong(now) {
            return Some((Tone::Error, String::from("That password isn't right. Try again.")));
        }
        if caps_lock {
            return Some((Tone::Warning, String::from("Caps Lock is on")));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: u64 = 1_000_000_000;

    #[test]
    fn a_key_asks_and_the_right_password_unlocks() {
        let mut s = Screen::new(0, true);
        assert_eq!(s.phase(), Phase::Resting);
        assert!(!s.takes_keys() && s.card(0) == 0.0);
        s.input(S);
        assert!(s.takes_keys());
        assert!(s.card(S + ASK_NS / 4) > 0.3 && s.card(S + ASK_NS) == 1.0);
        // Nothing typed: Enter does nothing.
        assert_eq!(s.submit(2 * S, false, 0), Submit::Nothing);
        assert_eq!(s.submit(2 * S, true, 0), Submit::Check);
        assert_eq!(s.phase(), Phase::Checking);
        // Nothing more is taken while it is checked.
        assert!(!s.takes_keys());
        assert_eq!(s.submit(2 * S + 1, true, 0), Submit::Nothing);
        assert!(s.checking(2 * S + S / 2).is_some());
        s.checked(3 * S, true);
        assert_eq!(s.phase(), Phase::Unlocking);
        // Still locked through the dissolve; then unlocked, faded, closed.
        assert_eq!(s.tick(3 * S + UNLOCK_NS - 1, true), None);
        assert!(s.unlocking(3 * S + UNLOCK_NS / 2) > 0.0 && s.opacity(3 * S + UNLOCK_NS / 2) == 1.0);
        assert_eq!(s.tick(3 * S + UNLOCK_NS, true), Some(Due::Unlock));
        let faded = 3 * S + UNLOCK_NS;
        assert!(s.opacity(faded + FADE_NS / 2) < 1.0);
        assert_eq!(s.tick(faded + FADE_NS, true), Some(Due::Close));
        assert_eq!((s.phase(), s.opacity(faded + FADE_NS)), (Phase::Gone, 0.0));
        assert_eq!(s.tick(faded + 2 * FADE_NS, true), None);
    }

    #[test]
    fn a_wrong_password_shakes_and_says_so_until_the_user_types() {
        let mut s = Screen::new(0, true);
        s.input(0);
        assert_eq!(s.submit(S, true, 0), Submit::Check);
        s.checked(2 * S, false);
        assert!(s.takes_keys());
        assert!(s.shake(2 * S + SHAKE_NS / 8).abs() > 0.2);
        assert_eq!(s.shake(2 * S + SHAKE_NS), 0.0);
        let said = s.message(2 * S + S, 0, false).unwrap();
        assert_eq!(said.0, Tone::Error);
        assert!(said.1.contains("isn't right"), "{}", said.1);
        // Gone as the user types again, or after a while.
        assert!(s.message(2 * S + WRONG_SHOWN_NS, 0, false).is_none());
        s.checked(2 * S, false);
        assert!(s.wrong(3 * S));
        s.typed(3 * S);
        assert!(!s.wrong(3 * S));
    }

    #[test]
    fn too_many_wrong_passwords_wait_and_say_how_long() {
        let mut s = Screen::new(0, true);
        s.input(0);
        assert_eq!(s.submit(S, true, 30), Submit::Nothing);
        assert_eq!(s.phase(), Phase::Asking);
        let (tone, text) = s.message(S, 28, true).unwrap();
        // The wait comes before Caps Lock.
        assert_eq!((tone, text.as_str()), (Tone::Error, "Too many tries. Try again in 28 seconds."));
        assert_eq!(s.message(S, 0, true), Some((Tone::Warning, String::from("Caps Lock is on"))));
        assert_eq!(s.message(S, 0, false), None);
    }

    #[test]
    fn without_a_password_enter_unlocks() {
        let mut s = Screen::new(0, false);
        // Resting, Enter only wakes it.
        assert_eq!(s.submit(0, false, 0), Submit::Nothing);
        s.input(0);
        assert_eq!(s.message(0, 0, true).map(|m| m.0), Some(Tone::Info));
        assert_eq!(s.submit(S, false, 0), Submit::Unlock);
        assert_eq!(s.phase(), Phase::Unlocking);
    }

    #[test]
    fn left_alone_with_nothing_typed_it_rests() {
        let mut s = Screen::new(0, true);
        s.input(S);
        // Something typed keeps it asking.
        assert_eq!(s.tick(S + REST_AFTER_NS, false), None);
        assert_eq!(s.phase(), Phase::Asking);
        assert_eq!(s.next_frame(S + REST_AFTER_NS / 2, true), Some(S + REST_AFTER_NS));
        s.tick(S + REST_AFTER_NS, true);
        assert_eq!(s.phase(), Phase::Resting);
        // The card goes back down from where it was.
        let t = S + REST_AFTER_NS;
        assert!(s.card(t + ASK_NS / 2) < 1.0 && s.card(t + ASK_NS) == 0.0);
        // Esc on an empty field rests it at once.
        s.input(t + S);
        s.rest(t + S);
        assert_eq!(s.phase(), Phase::Resting);
    }

    #[test]
    fn a_resting_screen_wants_no_frames() {
        let mut s = Screen::new(0, true);
        assert_eq!(s.next_frame(APPEAR_NS, true), None);
        s.input(S);
        // The card rising, then nothing but the rest that is due.
        assert_eq!(s.next_frame(S, true), Some(S + FRAME_NS));
        assert_eq!(s.next_frame(S + ASK_NS, false), None);
    }

    #[test]
    fn it_fades_in_as_it_appears_but_not_when_shown_again() {
        let s = Screen::new(S, true);
        assert_eq!(s.opacity(S), 0.0);
        assert!(s.opacity(S + APPEAR_NS / 2) > 0.5);
        assert_eq!(s.opacity(S + APPEAR_NS), 1.0);
        assert_eq!(s.next_frame(S, true), Some(S + FRAME_NS));
        let again = Screen::relocked(S, true);
        assert_eq!(again.opacity(S), 1.0);
        assert_eq!(again.next_frame(S, true), None);
    }

    #[test]
    fn a_wrong_password_flashes_the_ring() {
        let mut s = Screen::new(0, true);
        assert_eq!(s.flash(0), 0.0);
        s.input(0);
        s.submit(S, true, 0);
        s.checked(2 * S, false);
        assert_eq!(s.flash(2 * S), 1.0);
        assert!(s.flash(2 * S + SHAKE_NS / 2) < 0.5);
        assert_eq!(s.flash(2 * S + SHAKE_NS), 0.0);
    }

    #[test]
    fn waits_read_as_people_say_them() {
        assert_eq!(duration(1), "1 second");
        assert_eq!(duration(28), "28 seconds");
        assert_eq!(duration(60), "1 minute");
        assert_eq!(duration(61), "2 minutes");
        assert_eq!(duration(900), "15 minutes");
    }
}
