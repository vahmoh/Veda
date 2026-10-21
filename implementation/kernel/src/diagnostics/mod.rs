// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What the kernel reports: its log, which user space reads and the serial
//! console shows ([`log`]), and the report of a panic, which stops every
//! CPU ([`panic`]).

pub mod log;
pub mod panic;
