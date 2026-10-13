// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Each CPU's timer: its local APIC's, which fires once, at a TSC value
//! (TSC-deadline mode) or after a delay (one-shot mode).

use super::apic;

/// Arms this CPU's timer to interrupt at monotonic time `deadline_ns`;
/// `now_ns` is the time now (before the deadline).
pub fn arm(deadline_ns: u64, now_ns: u64) {
    apic::arm_timer(crate::time::ticks_at(deadline_ns), deadline_ns - now_ns);
}
