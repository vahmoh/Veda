//! The times of sound: where a chunk starts after an exact seek, and how
//! chunks follow each other so that the sound stays continuous (the player
//! keeps time by it).

/// Microseconds of `frames` frames at `rate`.
pub fn duration_us(frames: u64, rate: u32) -> i64 {
    (frames as u128 * 1_000_000 / rate.max(1) as u128).min(i64::MAX as u128) as i64
}

/// Frames to leave out of the start of a chunk of `frames` frames heard
/// from `pts_us`, so that what is left starts at `target_us` (or just
/// after: never before).
pub fn frames_before(pts_us: i64, frames: u32, rate: u32, target_us: i64) -> u32 {
    if pts_us >= target_us {
        return 0;
    }
    let us = (target_us as i128 - pts_us as i128) as u128;
    let n = (us * rate as u128).div_ceil(1_000_000);
    n.min(frames as u128) as u32
}

/// Chunks closer than this to where the last one ended carry on from it
/// (timestamps round, and resamplers jitter).
pub const JITTER_US: i64 = 20_000;
/// Gaps up to this long are filled with silence; longer ones (or a jump
/// back) start the sound again from the new time.
pub const MAX_GAP_US: i64 = 3_000_000;

/// How a chunk heard from `pts_us` follows the sound written so far, which
/// ends at `next_us`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Follow {
    /// Right after it (its time is `next_us`).
    Continue,
    /// After this many frames of silence.
    Gap(u32),
    /// Its start overlaps what was written: leave out this many frames.
    Overlap(u32),
    /// A jump: start again from its own time.
    Jump,
}

/// How a chunk of `frames` frames at `rate`, heard from `pts_us`, follows
/// sound that ends at `next_us` (`None`: nothing written yet).
pub fn follow(next_us: Option<i64>, pts_us: i64, frames: u32, rate: u32) -> Follow {
    let Some(next) = next_us else { return Follow::Jump };
    let delta = pts_us.saturating_sub(next);
    if delta.abs() <= JITTER_US {
        Follow::Continue
    } else if delta > 0 && delta <= MAX_GAP_US {
        Follow::Gap((delta as u128 * rate as u128 / 1_000_000) as u32)
    } else if delta < 0 && -delta < duration_us(frames as u64, rate) {
        Follow::Overlap(frames_before(pts_us, frames, rate, next))
    } else {
        Follow::Jump
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    #[test]
    fn exact_seeks_start_on_the_target() {
        // 48 kHz: 20 ms is 960 frames.
        assert_eq!(frames_before(1_000_000, 4096, 48_000, 1_020_000), 960);
        assert_eq!(frames_before(1_000_000, 100, 48_000, 1_020_000), 100, "all of it is before");
        assert_eq!(frames_before(1_000_000, 100, 48_000, 999_000), 0, "it starts after the target");
        // Rounded up: never a frame before the target.
        assert_eq!(frames_before(0, 4096, 44_100, 1), 1);
        assert_eq!(frames_before(i64::MIN, 10, 48_000, i64::MAX), 10);
    }

    #[test]
    fn chunks_follow_on() {
        assert_eq!(follow(None, 5, 960, 48_000), Follow::Jump);
        assert_eq!(follow(Some(1_000_000), 1_010_000, 960, 48_000), Follow::Continue);
        assert_eq!(follow(Some(1_000_000), 1_500_000, 960, 48_000), Follow::Gap(24_000));
        assert_eq!(follow(Some(1_000_000), 990_000 - 15_000, 1_920, 48_000), Follow::Overlap(1_200));
        assert_eq!(follow(Some(10_000_000), 1_000_000, 960, 48_000), Follow::Jump);
        assert_eq!(follow(Some(0), 60_000_000, 960, 48_000), Follow::Jump, "too long a gap to fill");
        assert_eq!(duration_us(48_000, 48_000), 1_000_000);
    }
}
