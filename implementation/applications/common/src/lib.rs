// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `apps-common` — what the built-in applications share.
//!
//! * [`process`]: the running processes, and which of them are the
//!   system's rather than applications' (the Terminal's `ps` and `kill`,
//!   Task Manager).
//! * [`format`]: names and uptimes as people read them.
//! * [`icons`]: the icon and colour of a file's type (Files, the desktop).
//! * [`clipboard`]: copying text, and saying when it did not fit.
//! * [`store`]: an application's own small files, for programs on the
//!   standard library.
//!
//! These belong with the libraries they extend: the process lists with
//! `vproto`'s launcher protocol, the formats with `vrt` and `vfiles`, the
//! icons and the clipboard with `vui`. The pure parts are tested on the
//! host.

#![no_std]

extern crate alloc;
#[cfg(any(test, target_os = "veda"))]
extern crate std;

pub mod clipboard;
pub mod format;
pub mod icons;
pub mod process;
#[cfg(any(test, target_os = "veda"))]
pub mod store;
