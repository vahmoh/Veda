// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `terminal-core` — the Terminal's logic apart from how it draws and what
//! runs in it, so that it can be tested on the host.
//!
//! * [`screen`]: the scrollback of styled text, and the escape sequences
//!   (colours, attributes, erasing a line) that programs write to it.
//! * [`syntax`]: the command language of vsh, the Terminal's shell: words,
//!   operators, lists of pipelines with their redirections, and where a
//!   command's streams go; Wi-Fi passwords hidden in what is kept of a
//!   command line ([`syntax::redact`]).
//! * [`classify`]: what a command line may do (look, change, destroy),
//!   which decides how the voice agent may run it.
//! * [`edit`]: the line being typed, and the keys that edit it.
//! * [`text`]: the bytes commands pass on: lines and counts as the bytes
//!   arrive, binary data, bounded pipes, text for the screen.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod classify;
pub mod edit;
pub mod screen;
pub mod syntax;
pub mod text;

#[cfg(test)]
mod tests;
