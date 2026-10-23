// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `timertest` — checks, inside the driver VM's Linux, that its timers end
//! on time. Linux runs them in high-resolution mode on the processors'
//! local APIC timers, the platform's only ones, but only with a timer that
//! keeps running while a processor idles (`cpuid`'s ARAT, which Veda's
//! virtual processors have); otherwise it keeps a periodic tick, every
//! 4 ms, ends every timer on one, and its idle processors take it. It says
//! how late sleeps of a millisecond end, and `timertest: PASS` when all is
//! well.

use std::time::{Duration, Instant};

use guest_sys::checks::Checks;
use rustix::time::{ClockId, clock_getres};

/// How many sleeps it measures, and how long each asks for.
const SLEEPS: usize = 200;
const SLEEP: Duration = Duration::from_millis(1);

fn main() {
    let mut c = Checks::new("timertest");
    let timer = std::fs::read_to_string("/sys/devices/system/clockevents/clockevent0/current_device");
    let resolution = clock_getres(ClockId::Monotonic);
    let resolution = resolution.tv_sec as u64 * 1_000_000_000 + resolution.tv_nsec as u64;
    c.check(
        &format!(
            "timers have high resolution (on {}: {resolution} ns)",
            timer.as_deref().map_or("no timer", str::trim)
        ),
        resolution == 1,
    );

    // How late each sleep ends (us), sorted.
    let mut late: Vec<u64> = (0..SLEEPS)
        .map(|_| {
            let start = Instant::now();
            std::thread::sleep(SLEEP);
            start.elapsed().saturating_sub(SLEEP).as_micros() as u64
        })
        .collect();
    late.sort_unstable();
    let median = late[SLEEPS / 2];
    println!(
        "timertest: {SLEEPS} sleeps of 1 ms ended {median} us late at the median, {} us at the 90th percentile, \
         {} us at the latest",
        late[SLEEPS * 9 / 10],
        late[SLEEPS - 1]
    );
    c.check("sleeps of 1 ms end on time (half of them within a millisecond)", median < 1000);
    c.finish();
}
