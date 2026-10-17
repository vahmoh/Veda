// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! How long the screen waits without input before it locks: which times
//! are taken, what it is until the user chooses one, and the record of the
//! choice.
//!
//! * **The times** are `vproto::lock`'s `IDLE_CHOICES_S`: never, or 1, 5,
//!   15 or 30 minutes. Nothing else is taken, from Settings or from disk.
//! * **Until the user chooses**, it follows the password: `DEFAULT_IDLE_S`
//!   (5 minutes) while one is set, never while none is. A password says
//!   the computer is to be kept from others, which an unattended unlocked
//!   screen defeats (the major desktops lock an idle one by default);
//!   without one, the lock screen keeps nothing from anyone (Enter unlocks
//!   it), and locking it unasked would only interrupt. A time chosen holds
//!   whatever becomes of the password.
//! * **The record** is the time chosen, in seconds. One that is not a time
//!   taken (damaged) counts as none chosen, so that the screen locks as it
//!   does by default rather than never.

use alloc::format;
use alloc::string::String;

use vproto::lock::{DEFAULT_IDLE_S, IDLE_CHOICES_S, LockError};

/// Whether the screen may wait `seconds` without input before it locks.
pub fn acceptable(seconds: u32) -> Result<(), LockError> {
    if IDLE_CHOICES_S.contains(&seconds) { Ok(()) } else { Err(LockError::Invalid) }
}

/// How long the screen waits without input before it locks (seconds; 0:
/// never): the time `chosen` or, until one is, the default for whether a
/// `password` is set.
pub fn timeout(chosen: Option<u32>, password: bool) -> u32 {
    chosen.unwrap_or(if password { DEFAULT_IDLE_S } else { 0 })
}

/// The record of `seconds` chosen.
pub fn record(seconds: u32) -> String {
    format!("{seconds}\n")
}

/// The time a record read back says was chosen: `None` for one that is
/// not a time taken.
pub fn parse(text: &str) -> Option<u32> {
    text.trim().parse::<u32>().ok().filter(|s| acceptable(*s).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn until_a_time_is_chosen_it_follows_the_password() {
        assert_eq!(timeout(None, true), 300);
        assert_eq!(timeout(None, false), 0);
        // A time chosen holds, password or not: never, too.
        assert_eq!(timeout(Some(60), false), 60);
        assert_eq!(timeout(Some(0), true), 0);
        assert_eq!(timeout(Some(1800), true), 1800);
    }

    #[test]
    fn only_the_times_offered_are_taken() {
        for s in IDLE_CHOICES_S {
            assert_eq!(acceptable(s), Ok(()));
        }
        for s in [1, 59, 61, 120, 299, 3600, u32::MAX] {
            assert_eq!(acceptable(s), Err(LockError::Invalid), "{s}");
        }
    }

    #[test]
    fn a_choice_outlives_a_restart() {
        for s in IDLE_CHOICES_S {
            assert_eq!(parse(&record(s)), Some(s));
        }
        assert_eq!(record(900), "900\n");
    }

    #[test]
    fn records_read_back_are_checked() {
        // Damaged, or a time not offered: as if none were chosen.
        for damaged in ["", "\n", "x", "-60", "60 300", "61\n", "99999999999", "0x3c", "1e3"] {
            assert_eq!(parse(damaged), None, "{damaged:?}");
        }
        assert_eq!(timeout(parse("7"), true), 300);
        assert_eq!(parse(" 1800 \n"), Some(1800));
    }
}
