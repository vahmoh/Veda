// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! File sizes, modification times and durations for people.

use alloc::format;
use alloc::string::{String, ToString};

use vrt::time::DateTime;

const NS_PER_SEC: u64 = 1_000_000_000;

/// A size in bytes, with one decimal in binary units: `512 B`, `1.5 KiB`,
/// `12.4 MiB` (rounded to the nearest tenth).
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut unit = 1;
    loop {
        let div = 1u128 << (10 * unit);
        let tenths = (bytes as u128 * 10 + div / 2) / div;
        // A value that rounds up to 1024 is shown in the next unit.
        if tenths < 10_240 || unit == UNITS.len() - 1 {
            return format!("{}.{} {}", tenths / 10, tenths % 10, UNITS[unit]);
        }
        unit += 1;
    }
}

/// A length of time in milliseconds as `m:ss`, or `h:mm:ss` from an hour
/// (a song's or a video's).
pub fn duration(ms: u64) -> String {
    let s = ms / 1000;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// A modification time (nanoseconds since the epoch, local time) as
/// `YYYY-MM-DD HH:MM`, or `-` when unknown (0).
pub fn format_time(ns: u64) -> String {
    if ns == 0 {
        return "-".to_string();
    }
    let d = DateTime::from_unix(ns / NS_PER_SEC);
    format!("{:04}-{:02}-{:02} {:02}:{:02}", d.year, d.month, d.day, d.hour, d.minute)
}

/// A modification time relative to now: `Today, 14:05`, `Yesterday, 09:30`
/// or `3 Oct 2026, 14:05`; `—` when unknown (0).
pub fn friendly_time(ns: u64) -> String {
    friendly_time_at(ns, vrt::time::unix_time_ns())
}

/// [`friendly_time`] with an explicit current time (both in nanoseconds
/// since the epoch).
pub fn friendly_time_at(ns: u64, now_ns: u64) -> String {
    if ns == 0 {
        return "—".to_string();
    }
    let secs = ns / NS_PER_SEC;
    let d = DateTime::from_unix(secs);
    let (day, today) = (secs / 86_400, now_ns / NS_PER_SEC / 86_400);
    if day == today {
        format!("Today, {:02}:{:02}", d.hour, d.minute)
    } else if day + 1 == today {
        format!("Yesterday, {:02}:{:02}", d.hour, d.minute)
    } else {
        format!("{} {} {}, {:02}:{:02}", d.day, &d.month_name()[..3], d.year, d.hour, d.minute)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(duration(0), "0:00");
        assert_eq!(duration(8_999), "0:08");
        assert_eq!(duration(75_000), "1:15");
        assert_eq!(duration(3_599_999), "59:59");
        assert_eq!(duration(3_600_000), "1:00:00");
        assert_eq!(duration(36_610_000), "10:10:10");
    }

    #[test]
    fn sizes() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(1024), "1.0 KiB");
        assert_eq!(human_size(1536), "1.5 KiB");
        assert_eq!(human_size(161_455), "157.7 KiB");
        // 1023.96 KiB rounds up into the next unit.
        assert_eq!(human_size(1_048_535), "1.0 MiB");
        assert_eq!(human_size(10 << 20), "10.0 MiB");
        assert_eq!(human_size((1 << 30) + (1 << 29)), "1.5 GiB");
        assert_eq!(human_size(u64::MAX), "16384.0 PiB");
    }

    #[test]
    fn times() {
        const DAY: u64 = 86_400 * NS_PER_SEC;
        const HOUR: u64 = 3_600 * NS_PER_SEC;
        assert_eq!(format_time(0), "-");
        assert_eq!(format_time(DAY + HOUR + 5 * 60 * NS_PER_SEC), "1970-01-02 01:05");
        let t = 10 * DAY + 14 * HOUR + 5 * 60 * NS_PER_SEC;
        assert_eq!(friendly_time_at(0, t), "—");
        assert_eq!(friendly_time_at(t, t + HOUR), "Today, 14:05");
        assert_eq!(friendly_time_at(t, t + DAY), "Yesterday, 14:05");
        assert_eq!(friendly_time_at(t, t + 3 * DAY), "11 Jan 1970, 14:05");
    }
}
