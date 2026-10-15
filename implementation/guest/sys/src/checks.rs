// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The checks of the guest's test programs (`bridgetest`, `pcitest`,
//! `gpiotest`, `efivartest`): each said as it is made, and at the end
//! whether all went well (`<program>: PASS`, which the test's script waits
//! for, or `<program>: FAIL (n checks)`).

/// A test program's checks.
pub struct Checks {
    program: &'static str,
    failed: u32,
}

impl Checks {
    /// The checks of `program`, which names them in what it says.
    pub fn new(program: &'static str) -> Checks {
        Checks { program, failed: 0 }
    }

    /// Says whether `what` holds (`ok`).
    pub fn check(&mut self, what: &str, ok: bool) {
        println!("{}: {} {}", self.program, if ok { "ok  " } else { "FAIL" }, what);
        if !ok {
            self.failed += 1;
        }
    }

    /// How many checks failed so far.
    pub fn failed(&self) -> u32 {
        self.failed
    }

    /// Says whether all went well.
    pub fn finish(self) {
        if self.failed == 0 {
            println!("{}: PASS", self.program);
        } else {
            println!("{}: FAIL ({} checks)", self.program, self.failed);
        }
    }
}
