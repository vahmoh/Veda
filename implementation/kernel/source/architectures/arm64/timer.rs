// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Each CPU's timer: the generic timer's, which fires once, when the
//! counter reaches a value. The kernel's is the virtual timer at EL1, and
//! at EL2 the hypervisor's physical timer, which leaves the virtual timer
//! to guests: at EL2 the `CNTP_*_EL0` names reach `CNTHP_*_EL2`.

use super::cpu::{self, write_register};

/// `CNTx_CTL_EL0`: the timer is on; its interrupt is masked.
const ENABLE: u64 = 1 << 0;
const MASKED: u64 = 1 << 1;

/// Arms this CPU's timer to interrupt at monotonic time `deadline_ns`;
/// `now_ns` is the time now (before the deadline).
pub fn arm(deadline_ns: u64, _now_ns: u64) {
    let deadline = crate::time::ticks_at(deadline_ns);
    // SAFETY: arming this CPU's timer, whose interrupt the kernel handles.
    unsafe {
        if cpu::at_el2() {
            write_register!("cntp_cval_el0", deadline);
            write_register!("cntp_ctl_el0", ENABLE);
        } else {
            write_register!("cntv_cval_el0", deadline);
            write_register!("cntv_ctl_el0", ENABLE);
        }
    }
}

/// Quiets the timer, which fired: its interrupt is a level, and holds
/// until the timer is armed again.
pub(in crate::architecture) fn fired() {
    // SAFETY: masking this CPU's timer, which [`arm`] arms again.
    unsafe {
        if cpu::at_el2() {
            write_register!("cntp_ctl_el0", ENABLE | MASKED);
        } else {
            write_register!("cntv_ctl_el0", ENABLE | MASKED);
        }
    }
}
