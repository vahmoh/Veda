// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What the monitor decides on AMD64 that reaches no object of Veda's
//! (the program's own part is `implementation/services/drivervm/src/arch/
//! amd64`).
//!
//! * [`i8042`]: the PC's keyboard controller, as the guest may drive it.
//! * How many MSIs the guest's functions may have at once ([`MAX_MSIS`]).

pub mod i8042;

/// The most MSIs the guest's functions have at once: two thirds of the
/// interrupt vectors Veda's kernel gives devices on AMD64 (0x30 to 0xEF,
/// one pool for every device's MSIs and lines), the rest left for the
/// host's devices and for the lines the guest has.
pub const MAX_MSIS: u32 = 128;
