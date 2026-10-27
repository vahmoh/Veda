// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The file systems of the VFS apart from the service around them (the
//! program, `main.rs`), so that they can be tested on the host.
//!
//! * [`tree`]: an in-memory file system, as the read-only system image and
//!   the writable file system (`/home` and `/tmp`) both are, and the pages
//!   its directories are listed in.
//! * [`snapshot`]: how `/home` is written to the home disk and read back.
//! * [`access`]: who may reach the system services' private directories,
//!   which places of the namespace stay where they are, and how many
//!   connections a program may hold.
//! * [`renamed`]: the private directories of services that were renamed,
//!   moved to their new names as the file system starts.
//! * [`view`]: what a confined connection reaches (`vfs::confine`).
//! * [`loans`]: places to read that a connection lends a confined one
//!   (`vfs::lend`, `vfs::borrow`), and takes back (`vfs::take_back`).
//! * [`locks`]: the advisory locks on files that the VFS arbitrates
//!   between processes (`file::lock`), and the processes that own them
//!   (`vfs::lock_owner`).
//! * [`shared_memory`]: `/dev/shm`, the shared memory objects, named and
//!   sized.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod access;
pub mod loans;
pub mod locks;
pub mod renamed;
pub mod shared_memory;
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

/// Whether `a` and `b` are the same bytes, compared in a time that does not
/// depend on where they differ: no answer about a secret tells how near a
/// guess came.
pub(crate) fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_are_the_same_or_not() {
        assert!(same(&[1, 2, 3], &[1, 2, 3]) && same(&[], &[]));
        assert!(!same(&[1, 2, 3], &[1, 2, 4]) && !same(&[1, 2, 3], &[1, 2]) && !same(&[0], &[]));
    }
}
