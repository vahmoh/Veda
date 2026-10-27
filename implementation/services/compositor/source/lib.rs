// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What the window system decides, apart from how it draws and whom it
//! talks to (the program, `main.rs`), so that it can be tested on the host.
//!
//! * [`wm`]: the window manager ([`Wm`]): windows, stacking and focus,
//!   window states and snapping, popups, who may do what, clients that do
//!   not answer, and what each client hears.
//! * [`lock`]: the locked screen, which shows and hears the lock screen
//!   alone; [`idle`]: locking it once no input has come for a while,
//!   unless a window keeps the screen awake.
//! * [`pointer`] and [`keys`]: the pointer and the keyboard, and the window
//!   system's shortcuts; [`input`]: input drivers' events, and what each
//!   holds down; [`keymap`]: the keyboard's layout, modifiers and repeat;
//!   [`switcher`]: the Alt+Tab switcher.
//! * [`window`]: a window and its geometry; [`buffers`]: its client's
//!   buffers, checked against the memory they are in; [`subsurfaces`]: the
//!   pictures of their own it shows in parts of it, and where; [`outbox`]:
//!   the events on their way to it.
//! * [`clients`]: who each connection is; [`limits`]: what a client may
//!   ask for, and how much of it.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod buffers;
pub mod clients;
pub mod idle;
pub mod input;
pub mod keymap;
pub mod keys;
pub mod limits;
pub mod lock;
pub mod outbox;
pub mod pointer;
pub mod subsurfaces;
pub mod switcher;
pub mod window;
pub mod wm;

pub use wm::Wm;

#[cfg(test)]
mod tests;
