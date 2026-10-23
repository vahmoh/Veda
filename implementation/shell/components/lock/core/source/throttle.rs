// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! How long the lock screen waits after wrong passwords.
//!
//! The first [`FREE`] wrong passwords cost nothing but the time each takes
//! to check. Each one after that makes the next wait: 30 seconds, then a
//! minute, five minutes, and fifteen for every one after those, so that
//! someone guessing at the keyboard tries four passwords an hour. The
//! right password clears the count. Settings' wrong passwords count too.
//!
//! The lock service keeps the count on disk ([`Throttle::record`]), so a
//! service restarted (or a machine started again) waits just as long. The
//! wait is kept as the Unix time it ends at, since the system's own clock
//! starts again with the machine; a record read back is checked
//! ([`Throttle::parse`]), and a wait longer than any there is (the clock
//! was put back) is cut to the longest.

use alloc::format;
use alloc::string::String;

/// Wrong passwords that cost nothing but the time to check them.
pub const FREE: u32 = 4;
/// The wait after each wrong password past those (seconds), the last for
/// every one after it.
const WAITS_S: [u64; 4] = [30, 60, 300, 900];
/// The longest wait (seconds).
pub const MAX_WAIT_S: u64 = WAITS_S[WAITS_S.len() - 1];

/// The wrong passwords given since the right one, and when the wait they
/// make ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Throttle {
    failures: u32,
    /// When another password is taken (Unix time, s).
    until: u64,
}

impl Throttle {
    /// No wrong password yet.
    pub fn new() -> Throttle {
        Throttle::default()
    }

    /// The wrong passwords given since the right one.
    pub fn failures(&self) -> u32 {
        self.failures
    }

    /// Seconds from `now` (Unix time, s) until another password is taken:
    /// 0 if one is now.
    pub fn wait(&self, now: u64) -> u64 {
        self.until.saturating_sub(now).min(MAX_WAIT_S)
    }

    /// A wrong password at `now`: how long the next must wait (s).
    pub fn failed(&mut self, now: u64) -> u64 {
        self.failures = self.failures.saturating_add(1);
        let wait = match self.failures.checked_sub(FREE + 1) {
            None => 0,
            Some(past) => WAITS_S[(past as usize).min(WAITS_S.len() - 1)],
        };
        self.until = now.saturating_add(wait);
        wait
    }

    /// The right password: the count starts again.
    pub fn succeeded(&mut self) {
        *self = Throttle::default();
    }

    /// What is kept on disk: the count and when the wait ends.
    pub fn record(&self) -> String {
        format!("{} {}\n", self.failures, self.until)
    }

    /// The record read back at `now`. A damaged one counts as the wrong
    /// passwords that cost nothing, so that the next one waits; a wait
    /// longer than any there is ends [`MAX_WAIT_S`] from now.
    pub fn parse(text: &str, now: u64) -> Throttle {
        let mut words = text.split_ascii_whitespace();
        let mut number = || words.next().and_then(|w| w.parse::<u64>().ok());
        match (number(), number(), number()) {
            (Some(failures), Some(until), None) if failures <= u32::MAX as u64 => {
                Throttle { failures: failures as u32, until: until.min(now.saturating_add(MAX_WAIT_S)) }
            }
            _ => Throttle { failures: FREE, until: now },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_wrong_passwords_cost_nothing_then_waits_grow() {
        let mut t = Throttle::new();
        let now = 1_000_000;
        for _ in 0..FREE {
            assert_eq!(t.failed(now), 0);
            assert_eq!(t.wait(now), 0);
        }
        let waits: std::vec::Vec<u64> = (0..6).map(|_| t.failed(now)).collect();
        assert_eq!(waits, [30, 60, 300, 900, 900, 900]);
        assert_eq!(t.wait(now), 900);
        assert_eq!(t.wait(now + 899), 1);
        assert_eq!(t.wait(now + 900), 0);
        assert_eq!(t.failures(), FREE + 6);
        // The right password: the count starts again.
        t.succeeded();
        assert_eq!((t.failures(), t.wait(now)), (0, 0));
        assert_eq!(t.failed(now), 0);
    }

    #[test]
    fn the_count_outlives_a_restart() {
        let mut t = Throttle::new();
        for _ in 0..=FREE {
            t.failed(500);
        }
        let back = Throttle::parse(&t.record(), 510);
        assert_eq!(back, t);
        assert_eq!(back.wait(510), 20);
        // The next wrong password waits longer, as it would have.
        let mut back = back;
        assert_eq!(back.failed(530), 60);
    }

    #[test]
    fn records_read_back_are_checked() {
        let now = 10_000;
        // A clock put back does not make the wait longer than any there is.
        assert_eq!(Throttle::parse("9 99999999\n", now).wait(now), MAX_WAIT_S);
        // A damaged record: the next wrong password waits.
        for damaged in ["", "x", "1", "1 2 3", "-1 5", "99999999999 5", "5 x"] {
            let mut t = Throttle::parse(damaged, now);
            assert_eq!((t.failures(), t.wait(now)), (FREE, 0), "{damaged:?}");
            assert_eq!(t.failed(now), 30, "{damaged:?}");
        }
        assert_eq!(Throttle::parse("0 0\n", now), Throttle::new());
    }

    #[test]
    fn counts_and_times_do_not_overflow() {
        let mut t = Throttle { failures: u32::MAX, until: 0 };
        assert_eq!(t.failed(u64::MAX), MAX_WAIT_S);
        assert_eq!((t.failures(), t.wait(u64::MAX)), (u32::MAX, 0));
    }
}
