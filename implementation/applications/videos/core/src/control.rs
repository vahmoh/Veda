// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What the voice agent's requests to a player come to: a volume (a level,
//! a change, or both), and a position to go to (from the start, or from
//! where it is). Videos and Music take the same requests within the same
//! limits.

/// Why a position asked for cannot be gone to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeekError {
    /// Counted from the start, a position cannot be before it.
    BeforeStart,
    /// It is past the end.
    PastEnd,
    /// Not a number of seconds.
    NotANumber,
}

/// The volume (percent) a request asks for: `level` (percent) if given,
/// else `current`, moved by `change` (points); `None` if it names neither.
/// Within 0 ..= 100.
pub fn volume(current: u32, level: Option<i64>, change: Option<i64>) -> Option<u32> {
    if level.is_none() && change.is_none() {
        return None;
    }
    let v = level.unwrap_or(current as i64).saturating_add(change.unwrap_or(0));
    Some(v.clamp(0, 100) as u32)
}

/// Where a request to go `seconds` on goes (ms): from the start, or from
/// `position_ms` when `relative` (back when negative, no further than the
/// start). It must be before the end of `duration_ms`, and goes no closer
/// to it than `margin_ms`.
pub fn seek_target(
    position_ms: u64,
    duration_ms: u64,
    seconds: f64,
    relative: bool,
    margin_ms: u64,
) -> Result<u64, SeekError> {
    if !seconds.is_finite() {
        return Err(SeekError::NotANumber);
    }
    if !relative && seconds < 0.0 {
        return Err(SeekError::BeforeStart);
    }
    let from = if relative { position_ms as f64 } else { 0.0 };
    let target = from + seconds * 1000.0;
    if target >= duration_ms as f64 {
        return Err(SeekError::PastEnd);
    }
    Ok((target.max(0.0) as u64).min(duration_ms.saturating_sub(margin_ms)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volumes() {
        assert_eq!(volume(40, None, None), None);
        assert_eq!(volume(40, Some(75), None), Some(75));
        assert_eq!(volume(40, None, Some(-10)), Some(30));
        assert_eq!(volume(40, Some(50), Some(10)), Some(60));
        assert_eq!(volume(40, Some(250), None), Some(100));
        assert_eq!(volume(40, None, Some(i64::MIN)), Some(0));
        assert_eq!(volume(40, Some(i64::MAX), Some(i64::MAX)), Some(100));
    }

    #[test]
    fn seek_targets() {
        assert_eq!(seek_target(5_000, 60_000, 30.0, false, 0), Ok(30_000));
        assert_eq!(seek_target(5_000, 60_000, 30.0, true, 0), Ok(35_000));
        assert_eq!(seek_target(5_000, 60_000, -10.0, true, 0), Ok(0), "no further back than the start");
        assert_eq!(seek_target(5_000, 60_000, -1.0, false, 0), Err(SeekError::BeforeStart));
        assert_eq!(seek_target(5_000, 60_000, 60.0, false, 0), Err(SeekError::PastEnd));
        assert_eq!(seek_target(5_000, 60_000, 59.9, false, 500), Ok(59_500), "kept off the very end");
        assert_eq!(seek_target(5_000, 60_000, f64::NAN, false, 0), Err(SeekError::NotANumber));
        assert_eq!(seek_target(5_000, 60_000, f64::INFINITY, true, 0), Err(SeekError::NotANumber));
        assert_eq!(seek_target(0, 0, 0.0, false, 0), Err(SeekError::PastEnd), "nothing to go to");
    }
}
