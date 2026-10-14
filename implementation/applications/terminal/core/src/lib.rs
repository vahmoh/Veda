// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `terminal-core` — what the Terminal shows, apart from how it draws it
//! and what runs in it, so that it can be tested on the host.
//!
//! * [`screen`]: the scrollback of styled text, and the escape sequences
//!   (colours, attributes, erasing a line) that programs write to it.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod screen;

#[cfg(test)]
mod tests;
