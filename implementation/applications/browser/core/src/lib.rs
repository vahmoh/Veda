// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `browser-core` — what the browser decides, apart from its window and its
//! engine, so that it can be tested on the host.
//!
//! * [`address`]: what the address bar's text means: an address to go to,
//!   or words to search for.
//! * [`search`]: the search engines, and the addresses of their results.
//! * [`url`]: addresses as people read them, and what they say of a page.
//! * [`keys`]: the key symbols and codes web pages get for Veda's keys.
//! * [`tabs`]: the tabs: their order, the one shown, which shows next when
//!   one closes, and how wide each is drawn.
//! * [`history`]: the pages visited, and what the address bar suggests.
//! * [`bookmarks`]: the pages kept.
//! * [`settings`]: what the browser remembers of the user's choices.
//! * [`session`]: the tabs open, to open again.
//! * [`downloads`]: what a downloaded file is called on disk.
//!
//! What it keeps in files ([`history`], [`bookmarks`], [`settings`],
//! [`session`]) is lines of text, a record a line, fields apart by tabs
//! ([`text`]), which a damaged file loses no more of than its bad lines.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod address;
pub mod bookmarks;
pub mod downloads;
pub mod history;
pub mod keys;
pub mod search;
pub mod session;
pub mod settings;
pub mod tabs;
pub mod text;
pub mod url;
