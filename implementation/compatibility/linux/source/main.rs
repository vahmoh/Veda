// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `linux-compatibility`: runs the Linux program its process was started
//! for (see the library's documentation).

#![no_std]
#![no_main]

runtime::entry!(main);

fn main() -> i32 {
    linux_compatibility::start()
}
