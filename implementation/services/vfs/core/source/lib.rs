// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `vfs-core` — the file systems of the VFS (`implementation/services/vfs`)
//! apart from the service around them, so that they can be tested on the
//! host.
//!
//! * [`tree`]: an in-memory file system, as the read-only system image and
//!   the writable file system (`/home` and `/tmp`) both are, and the pages
//!   its directories are listed in.
//! * [`snapshot`]: how `/home` is written to the home disk and read back.
//! * [`access`]: who may reach the system services' private directories,
//!   which places of the namespace stay where they are, and how many
//!   connections a program may hold.
//! * [`view`]: what a confined connection reaches (`vfs::confine`).
//! * [`loans`]: places to read that a connection lends a confined one
//!   (`vfs::lend`, `vfs::borrow`), and takes back (`vfs::take_back`).

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod access;
pub mod loans;
pub mod snapshot;
pub mod tree;
pub mod view;

/// The directory of the writable file system that the home disk keeps:
/// `/home`.
pub const HOME: &str = "home";
/// The user's home directory in it: `/home/user`.
pub const USER: &str = "user";
/// Scratch space: `/tmp`.
pub const TMP: &str = "tmp";
