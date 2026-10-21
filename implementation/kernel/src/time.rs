// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Timekeeping.
//!
//! The monotonic clock counts the ticks of the processor's clock counter
//! (`architecture::clock`), which every CPU reads alike, from the moment
//! the architecture starts it ([`start`]), once it knows the counter's
//! frequency. Wall-clock time is the firmware RTC reading taken by the
//! loader, advanced by the monotonic clock. The RTC keeps UTC (unless the
//! firmware says otherwise); local time is UTC plus the offset of the `tz=`
//! boot option.

use core::sync::atomic::{AtomicI64, AtomicU64, Ordering};

use crate::architecture::clock::ticks;

/// The counter's reading when the monotonic clock read 0.
static TICKS_AT_ZERO: AtomicU64 = AtomicU64::new(0);
static TICKS_HZ: AtomicU64 = AtomicU64::new(0);
/// Nanoseconds per tick in 32.32 fixed point.
static NS_PER_TICK_FP: AtomicU64 = AtomicU64::new(0);
/// Seconds since the Unix epoch (local time) at `TICKS_AT_ZERO`.
static BOOT_EPOCH_SECS: AtomicI64 = AtomicI64::new(0);
/// Local time minus UTC, in seconds.
static UTC_OFFSET_S: AtomicI64 = AtomicI64::new(0);

/// Nanoseconds since the clock was started (0 before that).
#[inline]
pub fn now_ns() -> u64 {
    let mult = NS_PER_TICK_FP.load(Ordering::Relaxed);
    if mult == 0 {
        return 0;
    }
    let ticks = ticks().saturating_sub(TICKS_AT_ZERO.load(Ordering::Relaxed));
    ((ticks as u128 * mult as u128) >> 32) as u64
}

/// How the clocks follow the counter, which user space reads too. Programs
/// ask for it once and read the clocks themselves from then on
/// (`rt::time`): it must not change once the clock has started (a clock
/// set later would need a version programs check as they read).
pub fn info() -> abi::ClockInfo {
    let realtime = (BOOT_EPOCH_SECS.load(Ordering::Relaxed).max(0) as u64) * 1_000_000_000;
    let offset = UTC_OFFSET_S.load(Ordering::Relaxed) * 1_000_000_000;
    abi::ClockInfo {
        ticks_at_zero: TICKS_AT_ZERO.load(Ordering::Relaxed),
        ns_per_tick: NS_PER_TICK_FP.load(Ordering::Relaxed),
        ticks_hz: TICKS_HZ.load(Ordering::Relaxed),
        realtime_at_zero: realtime,
        utc_at_zero: (realtime as i64).saturating_sub(offset).max(0) as u64,
    }
}

/// The monotonic time (ns) at which the counter reads `ticks` (0 before
/// the clock started).
pub fn monotonic_ns_at(ticks: u64) -> u64 {
    let ticks = ticks.saturating_sub(TICKS_AT_ZERO.load(Ordering::Relaxed));
    ((ticks as u128 * NS_PER_TICK_FP.load(Ordering::Relaxed) as u128) >> 32) as u64
}

/// The counter's reading at monotonic time `ns`.
pub fn ticks_at(ns: u64) -> u64 {
    let hz = TICKS_HZ.load(Ordering::Relaxed);
    TICKS_AT_ZERO.load(Ordering::Relaxed).saturating_add(((ns as u128 * hz as u128) / 1_000_000_000) as u64)
}

/// The counter's frequency.
pub fn ticks_hz() -> u64 {
    TICKS_HZ.load(Ordering::Relaxed)
}

/// Starts the monotonic clock, at 0 now: the counter runs at `hz`.
pub fn start(hz: u64) {
    TICKS_HZ.store(hz, Ordering::Relaxed);
    TICKS_AT_ZERO.store(ticks(), Ordering::Relaxed);
    NS_PER_TICK_FP.store(((1_000_000_000u128 << 32) / hz as u128) as u64, Ordering::Relaxed);
}

/// Wall-clock time in nanoseconds since the Unix epoch, in local time.
pub fn realtime_ns() -> u64 {
    (BOOT_EPOCH_SECS.load(Ordering::Relaxed).max(0) as u64) * 1_000_000_000 + now_ns()
}

/// Wall-clock time in nanoseconds since the Unix epoch, in UTC.
pub fn utc_ns() -> u64 {
    let offset = UTC_OFFSET_S.load(Ordering::Relaxed) * 1_000_000_000;
    (realtime_ns() as i64).saturating_sub(offset).max(0) as u64
}

/// Parses a time zone offset: `+02:00`, `-05:30`, `+0100`, `+2`, `UTC+2`.
pub fn parse_utc_offset(s: &str) -> Option<i64> {
    let s = s.strip_prefix("UTC").or_else(|| s.strip_prefix("utc")).unwrap_or(s);
    let (sign, rest) = match s.as_bytes().first()? {
        b'+' => (1, &s[1..]),
        b'-' => (-1, &s[1..]),
        _ => (1, s),
    };
    let (h, m) = match rest.split_once(':') {
        Some((h, m)) => (h.parse::<i64>().ok()?, m.parse::<i64>().ok()?),
        None if rest.len() == 4 => (rest[..2].parse::<i64>().ok()?, rest[2..].parse::<i64>().ok()?),
        None => (rest.parse::<i64>().ok()?, 0),
    };
    if h > 14 || m >= 60 {
        return None;
    }
    Some(sign * (h * 3600 + m * 60))
}

/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Sets the wall clock from the firmware's reading. `tz` (seconds east of
/// UTC, from the `tz=` boot option) gives local time; without it, the
/// firmware's own time zone is used, or UTC.
pub fn set_boot_time(t: &bootinfo::BootTime, tz: Option<i64>) {
    if t.valid == 0 {
        return;
    }
    let days = days_from_civil(t.year as i64, t.month as i64, t.day as i64);
    let secs = days * 86_400 + t.hour as i64 * 3600 + t.minute as i64 * 60 + t.second as i64;
    let firmware_offset = if t.utc_offset_minutes == i16::MAX { 0 } else { t.utc_offset_minutes as i64 * 60 };
    let offset = tz.unwrap_or(firmware_offset);
    UTC_OFFSET_S.store(offset, Ordering::Relaxed);
    BOOT_EPOCH_SECS.store(secs - firmware_offset + offset, Ordering::Relaxed);
    let sign = if offset < 0 { '-' } else { '+' };
    crate::kinfo!(
        "time: {:04}-{:02}-{:02} {:02}:{:02} (firmware), local time UTC{}{:02}:{:02}",
        t.year,
        t.month,
        t.day,
        t.hour,
        t.minute,
        sign,
        offset.abs() / 3600,
        offset.abs() / 60 % 60
    );
}

/// Busy-waits for `us` microseconds on the counter.
pub fn udelay(us: u64) {
    let end = ticks() + us * TICKS_HZ.load(Ordering::Relaxed) / 1_000_000;
    while ticks() < end {
        core::hint::spin_loop();
    }
}

/// Ticks in nanoseconds (for the log).
pub fn ticks_to_ns(ticks: i64) -> i64 {
    let hz = TICKS_HZ.load(Ordering::Relaxed).max(1) as i128;
    (ticks as i128 * 1_000_000_000 / hz) as i64
}
