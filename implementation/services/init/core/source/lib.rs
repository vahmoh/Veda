// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `init-core` — what the launcher in `init` (`implementation/services/init`)
//! decides, apart from the service around it, so that it can be tested on
//! the host.
//!
//! * [`apps`]: the applications' manifests, the system image's and the
//!   installed packages', and which of the packages' applications the
//!   launcher lists and starts.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod apps;
