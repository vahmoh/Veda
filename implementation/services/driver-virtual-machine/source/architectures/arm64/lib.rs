// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What the monitor decides on ARM64 that reaches no object of Veda's (the
//! program's own part is `mod.rs`, beside this): how many MSIs the guest's
//! functions may have at once ([`MAX_MSIS`]).

/// The most MSIs the guest's functions have at once: fewer than the
/// interrupts of the guest's GIC or the host's vectors (255, one pool for
/// every device's MSIs and lines), the rest left for the host's devices and
/// the guest's lines.
pub const MAX_MSIS: u32 = 128;
