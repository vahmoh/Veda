// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Clocks, durations and sleeping.
//!
//! The clocks are read here, without a system call: the kernel's clocks
//! are the processor's clock counter scaled, by values it sets as it starts
//! and never changes ([`abi::ClockInfo`]), which a program asks for once.
//! A system call takes the kernel's lock, which every processor shares,
//! and programs read the time often (WebKit, GLib and media pipelines
//! thousands of times a second): reading it here keeps them off it.

pub use core::time::Duration;

use abi::{clock, nr};

use crate::sync::Lazy;
use crate::sys::{call, clock_get};

/// Nanoseconds since boot.
pub fn now_ns() -> u64 {
    clock_get(clock::MONOTONIC)
}

/// How the clocks follow the processor's clock counter.
pub fn clock_info() -> Result<abi::ClockInfo, abi::Error> {
    let mut info = abi::ClockInfo::default();
    call(nr::CLOCK_INFO, [&mut info as *mut _ as usize, 0, 0, 0, 0, 0])?;
    Ok(info)
}

/// The kernel's clocks' scale, asked for once (`None` if it has none to
/// give, when the system call reads them).
static CLOCKS: Lazy<Option<abi::ClockInfo>> = Lazy::new(|| clock_info().ok().filter(|info| info.ns_per_tick != 0));

/// Clock `id` read here as the kernel reads it, or `None` for a clock only
/// the kernel can read.
pub(crate) fn read_clock(id: usize) -> Option<u64> {
    let info = CLOCKS.as_ref()?;
    let ticks = crate::arch::clock_ticks().saturating_sub(info.ticks_at_zero);
    let now = ((ticks as u128 * info.ns_per_tick as u128) >> 32) as u64;
    match id {
        clock::MONOTONIC => Some(now),
        clock::REALTIME => Some(info.realtime_at_zero.saturating_add(now)),
        clock::UTC => Some(info.utc_at_zero.saturating_add(now)),
        _ => None,
    }
}

/// A monotonic timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Instant(u64);

impl Instant {
    pub fn now() -> Instant {
        Instant(now_ns())
    }

    pub fn from_nanos(ns: u64) -> Instant {
        Instant(ns)
    }

    pub fn as_nanos(&self) -> u64 {
        self.0
    }

    pub fn elapsed(&self) -> Duration {
        Duration::from_nanos(now_ns().saturating_sub(self.0))
    }

    pub fn duration_since(&self, earlier: Instant) -> Duration {
        Duration::from_nanos(self.0.saturating_sub(earlier.0))
    }

    /// Deadline value for system calls.
    pub fn deadline(&self) -> u64 {
        self.0
    }
}

impl core::ops::Add<Duration> for Instant {
    type Output = Instant;
    fn add(self, d: Duration) -> Instant {
        Instant(self.0.saturating_add(d.as_nanos() as u64))
    }
}

impl core::ops::Sub for Instant {
    type Output = Duration;
    fn sub(self, other: Instant) -> Duration {
        self.duration_since(other)
    }
}

/// Deadline `d` from now, for system calls.
pub fn deadline_after(d: Duration) -> u64 {
    now_ns().saturating_add(d.as_nanos() as u64)
}

pub fn sleep(d: Duration) {
    sleep_until(deadline_after(d));
}

pub fn sleep_until(deadline: u64) {
    let _ = call(nr::SLEEP, [deadline as usize, 0, 0, 0, 0, 0]);
}

/// Wall-clock time: nanoseconds since 1970-01-01 in local time.
pub fn unix_time_ns() -> u64 {
    clock_get(clock::REALTIME)
}

/// Wall-clock time: nanoseconds since 1970-01-01 00:00 UTC (for protocols
/// and certificates; people see [`unix_time_ns`]).
pub fn utc_time_ns() -> u64 {
    clock_get(clock::UTC)
}

/// The name of month `month` (1 to 12, January to December), in English;
/// a month outside them is taken for the nearest.
pub fn month_name(month: u8) -> &'static str {
    const NAMES: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    NAMES[(month.clamp(1, 12) - 1) as usize]
}

/// How many days month `month` (1 to 12) of `year` has, in the proleptic
/// Gregorian calendar.
pub fn days_in_month(year: i32, month: u8) -> u8 {
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// The weekday of the day `days` days after 1970-01-01 (a Thursday): 0 is
/// Sunday.
pub fn weekday(days: i64) -> u8 {
    (days + 4).rem_euclid(7) as u8
}

/// Days since 1970-01-01 of a date in the proleptic Gregorian calendar
/// (negative before it): the inverse of [`DateTime::from_unix`]'s
/// civil-from-days (Howard Hinnant's algorithms).
pub fn days_from_civil(year: i32, month: u8, day: u8) -> i64 {
    let y = if month <= 2 { year as i64 - 1 } else { year as i64 };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// A broken-down local date and time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateTime {
    pub year: i32,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    /// 0 = Sunday.
    pub weekday: u8,
}

impl DateTime {
    pub fn now() -> DateTime {
        DateTime::from_unix(unix_time_ns() / 1_000_000_000)
    }

    /// Converts seconds since the epoch (civil-from-days algorithm).
    pub fn from_unix(secs: u64) -> DateTime {
        let days = (secs / 86_400) as i64;
        let rem = secs % 86_400;
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        DateTime {
            year: (if m <= 2 { y + 1 } else { y }) as i32,
            month: m as u8,
            day: d as u8,
            hour: (rem / 3600) as u8,
            minute: ((rem / 60) % 60) as u8,
            second: (rem % 60) as u8,
            weekday: weekday(days),
        }
    }

    /// The month's name ([`month_name`]).
    pub fn month_name(&self) -> &'static str {
        month_name(self.month)
    }

    /// The weekday's name, in English.
    pub fn weekday_name(&self) -> &'static str {
        const NAMES: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
        NAMES[self.weekday as usize % 7]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn month_names() {
        assert_eq!(month_name(1), "January");
        assert_eq!(month_name(9), "September");
        assert_eq!(month_name(12), "December");
        assert_eq!((month_name(0), month_name(13)), ("January", "December"));
        assert_eq!(DateTime::from_unix(0).month_name(), "January");
    }

    #[test]
    fn months_have_their_days() {
        assert_eq!(days_in_month(2026, 1), 31);
        assert_eq!(days_in_month(2026, 4), 30);
        assert_eq!((days_in_month(2024, 2), days_in_month(2026, 2)), (29, 28));
        // Centuries are leap years only every fourth.
        assert_eq!((days_in_month(1900, 2), days_in_month(2000, 2)), (28, 29));
        assert_eq!(weekday(days_from_civil(2026, 10, 15)), 4);
        assert_eq!(weekday(-1), 3);
    }

    #[test]
    fn dates_round_trip() {
        // 2026-10-15 12:34:56 UTC, a Thursday.
        let d = DateTime::from_unix(1_792_067_696);
        assert_eq!((d.year, d.month, d.day, d.hour, d.minute, d.second), (2026, 10, 15, 12, 34, 56));
        assert_eq!(d.weekday_name(), "Thursday");
        assert_eq!(days_from_civil(2026, 10, 15), 1_792_067_696 / 86_400);
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(1969, 12, 31), -1);
    }
}
