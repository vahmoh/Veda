// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `lock-core` — what the lock screen decides, apart from how it draws and
//! whom it talks to, so that it can be tested on the host.
//!
//! * [`password`]: the password's record, a salted Argon2id hash in the
//!   PHC string format, checked as it is read back; which new passwords
//!   are taken.
//! * [`throttle`]: how long the lock screen waits after wrong passwords,
//!   and the record of it that outlives a restart.
//! * [`idle`]: how long the screen waits without input before it locks,
//!   what it is until the user chooses, and the record of the choice.
//! * [`screen`]: the lock screen's states (resting, asking, checking,
//!   unlocking), what moves it from one to the next, and its animations
//!   as functions of time.
//! * [`layout`]: where everything goes on a screen of any size.
//!
//! The lock service (`implementation/shell/components/lock`) keeps the
//! records in its private directory, hashes on a thread of its own and
//! draws what these say.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod idle;
pub mod layout;
pub mod password;
pub mod screen;
pub mod throttle;
