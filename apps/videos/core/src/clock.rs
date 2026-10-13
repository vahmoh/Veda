// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Media time: the clock pictures are shown by.
//!
//! With sound it is the sound as it is heard: the audio thread notes where
//! each piece of sound it writes to the output came from ([`AudioMap`]), and
//! the output says how much of it has been played; together they say which
//! moment of the video is heard now. That position moves in steps (a sound
//! device plays in periods) and jitters, so [`ClockModel`] smooths it into a
//! [`Clock`] that moves evenly, re-anchoring when the two drift apart. Without
//! sound (a silent video, or no sound device) the monotonic clock runs from
//! where playback started.

use alloc::collections::VecDeque;

/// Media time `base_us` at monotonic `anchor_ns`, running at normal speed or
/// standing still.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Clock {
    pub anchor_ns: u64,
    pub base_us: i64,
    pub running: bool,
    /// The seek generation it is for.
    pub generation: u32,
}

impl Clock {
    /// A clock standing at `us`.
    pub fn still(us: i64, generation: u32) -> Clock {
        Clock { anchor_ns: 0, base_us: us, running: false, generation }
    }

    /// A clock running from `us` at `now_ns`.
    pub fn running(us: i64, now_ns: u64, generation: u32) -> Clock {
        Clock { anchor_ns: now_ns, base_us: us, running: true, generation }
    }

    /// The media time at monotonic `ns` (µs).
    pub fn at(&self, ns: u64) -> i64 {
        if !self.running {
            return self.base_us;
        }
        let d = if ns >= self.anchor_ns {
            ((ns - self.anchor_ns) / 1000) as i64
        } else {
            -(((self.anchor_ns - ns) / 1000) as i64)
        };
        self.base_us.saturating_add(d)
    }

    /// When (monotonic ns) it shows `us`, if it runs.
    pub fn when(&self, us: i64) -> Option<u64> {
        if !self.running {
            return None;
        }
        let d = us.saturating_sub(self.base_us);
        Some(if d >= 0 {
            self.anchor_ns.saturating_add(d as u64 * 1000)
        } else {
            self.anchor_ns.saturating_sub(d.unsigned_abs() * 1000)
        })
    }
}

/// A run of sound written to the output: `frames` stream frames from
/// `frame` on, the first heard at media time `pts_us`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Run {
    frame: u64,
    pts_us: i64,
    frames: u64,
}

/// Where the sound written to the output stream came from.
#[derive(Debug, Clone)]
pub struct AudioMap {
    runs: VecDeque<Run>,
    rate: u32,
}

/// Microseconds of `frames` frames at `rate`.
fn duration(frames: u64, rate: u32) -> i64 {
    (frames as u128 * 1_000_000 / rate.max(1) as u128) as i64
}

/// Runs kept at most (a few seconds of records).
const MAX_RUNS: usize = 512;

impl AudioMap {
    /// A map for an output stream at `rate`.
    pub fn new(rate: u32) -> AudioMap {
        AudioMap { runs: VecDeque::new(), rate: rate.max(1) }
    }

    /// Forgets everything (the output was flushed).
    pub fn clear(&mut self) {
        self.runs.clear();
    }

    /// Notes that `frames` frames written from stream frame `frame` on are
    /// heard from media time `pts_us`.
    pub fn push(&mut self, frame: u64, pts_us: i64, frames: u64) {
        if frames == 0 {
            return;
        }
        // Continuing the last run exactly: one run.
        let rate = self.rate;
        if let Some(last) = self.runs.back_mut()
            && last.frame + last.frames == frame
            && last.pts_us.saturating_add(duration(last.frames, rate)) == pts_us
        {
            last.frames += frames;
            return;
        }
        if self.runs.len() >= MAX_RUNS {
            self.runs.pop_front();
        }
        self.runs.push_back(Run { frame, pts_us, frames });
    }

    /// The media time heard at stream frame `played`; `None` before the
    /// first run. Past the end of what was written it keeps counting from
    /// the last run (the output plays silence there).
    pub fn media_at(&self, played: u64) -> Option<i64> {
        let i = self.runs.partition_point(|r| r.frame <= played);
        let run = self.runs.get(i.checked_sub(1)?)?;
        Some(run.pts_us.saturating_add(duration(played - run.frame, self.rate)))
    }

    /// Drops runs heard completely before stream frame `played`.
    pub fn forget_before(&mut self, played: u64) {
        while self.runs.len() > 1 && self.runs[1].frame <= played {
            self.runs.pop_front();
        }
    }

    /// The stream frame where the sound written so far ends.
    pub fn end(&self) -> Option<u64> {
        self.runs.back().map(|r| r.frame + r.frames)
    }

    /// The media time where the sound written so far ends.
    pub fn end_us(&self) -> Option<i64> {
        self.runs.back().map(|r| r.pts_us.saturating_add(duration(r.frames, self.rate)))
    }
}

/// Larger differences between the clock and the sound make the clock jump
/// to the sound (µs).
pub const RESYNC_US: i64 = 40_000;
/// Smaller ones are taken in by an eighth at each reading.
const SMOOTHING: i64 = 8;
/// Readings in a row without the sound moving before the clock stops (the
/// output ran dry).
const STALL_READINGS: u32 = 3;

/// The clock, kept in step with the sound as heard.
#[derive(Debug, Clone, Default)]
pub struct ClockModel {
    clock: Clock,
    last_raw: Option<i64>,
    stalls: u32,
}

impl ClockModel {
    pub fn new() -> ClockModel {
        ClockModel::default()
    }

    pub fn clock(&self) -> Clock {
        self.clock
    }

    /// Stands the clock at `us` (a seek, or paused at a picture).
    pub fn stand(&mut self, us: i64, generation: u32) {
        self.clock = Clock::still(us, generation);
        self.last_raw = None;
        self.stalls = 0;
    }

    /// Stops the clock where it is now.
    pub fn freeze(&mut self, now_ns: u64) {
        let at = self.clock.at(now_ns);
        self.clock = Clock::still(at, self.clock.generation);
        self.last_raw = None;
    }

    /// A reading while playing: media time `raw_us` is heard at `now_ns`
    /// (`None`: no sound heard yet). Returns the clock to publish.
    pub fn update(&mut self, now_ns: u64, raw_us: Option<i64>, generation: u32) -> Clock {
        let Some(raw) = raw_us else { return self.clock };
        let moved = self.last_raw != Some(raw);
        self.last_raw = Some(raw);
        if !moved {
            self.stalls += 1;
            if self.stalls >= STALL_READINGS && self.clock.running {
                // The output stopped (ran dry): so does the clock, at the
                // sound.
                self.clock = Clock::still(raw, generation);
            }
            return self.clock;
        }
        self.stalls = 0;
        if !self.clock.running || self.clock.generation != generation {
            self.clock = Clock::running(raw, now_ns, generation);
            return self.clock;
        }
        let error = raw.saturating_sub(self.clock.at(now_ns));
        if error.abs() > RESYNC_US {
            self.clock = Clock::running(raw, now_ns, generation);
        } else {
            self.clock.base_us = self.clock.base_us.saturating_add(error / SMOOTHING);
        }
        self.clock
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1_000_000;

    #[test]
    fn clocks_run_and_stand() {
        let c = Clock::running(5_000_000, 100 * MS, 3);
        assert_eq!(c.at(100 * MS), 5_000_000);
        assert_eq!(c.at(150 * MS), 5_050_000);
        assert_eq!(c.at(90 * MS), 4_990_000);
        assert_eq!(c.when(5_100_000), Some(200 * MS));
        let s = Clock::still(7, 3);
        assert_eq!((s.at(u64::MAX), s.when(8)), (7, None));
    }

    #[test]
    fn the_map_says_what_is_heard() {
        let mut m = AudioMap::new(48_000);
        assert_eq!(m.media_at(0), None);
        m.push(0, 1_000_000, 48_000);
        // Continuous: one run.
        m.push(48_000, 2_000_000, 24_000);
        assert_eq!(m.runs.len(), 1);
        assert_eq!(m.media_at(24_000), Some(1_500_000));
        // A jump (a gap the decoder did not fill).
        m.push(72_000, 9_000_000, 48_000);
        assert_eq!(m.media_at(72_000), Some(9_000_000));
        assert_eq!(m.media_at(71_999), Some(2_499_979));
        assert_eq!(m.end(), Some(120_000));
        assert_eq!(m.end_us(), Some(10_000_000));
        // Silence after the end counts on.
        assert_eq!(m.media_at(120_000 + 4_800), Some(10_100_000));
        m.forget_before(100_000);
        assert_eq!(m.runs.len(), 1);
        assert_eq!(m.media_at(10), None, "before what is kept: forgotten");
        assert_eq!(m.media_at(100_000), Some(9_583_333));
        m.clear();
        assert_eq!(m.media_at(100_000), None);
    }

    #[test]
    fn the_clock_follows_the_sound_smoothly() {
        let mut model = ClockModel::new();
        model.stand(0, 1);
        // The first reading starts it.
        let c = model.update(1000 * MS, Some(0), 1);
        assert!(c.running);
        // Readings that step by a device period (10 ms) around the truth:
        // the clock stays within a few ms of it.
        let mut worst = 0;
        for k in 1..200u64 {
            let now = 1000 * MS + k * 5 * MS;
            let truth = (k * 5_000) as i64;
            let heard = truth - (truth % 10_000);
            let c = model.update(now, Some(heard), 1);
            worst = worst.max((c.at(now) - truth).abs());
        }
        assert!(worst < 12_000, "within a device period: {worst} µs");
        // A jump of the sound beyond the threshold: the clock jumps too.
        let now = 3000 * MS;
        let c = model.update(now, Some(60_000_000), 1);
        assert_eq!(c.at(now), 60_000_000);
    }

    #[test]
    fn the_clock_stops_when_the_sound_does() {
        let mut model = ClockModel::new();
        model.update(0, Some(0), 1);
        model.update(10 * MS, Some(10_000), 1);
        for k in 2..6 {
            model.update(k * 10 * MS, Some(10_000), 1);
        }
        let c = model.clock();
        assert!(!c.running && c.at(u64::MAX) == 10_000);
        // It moves again: it runs again.
        let c = model.update(100 * MS, Some(20_000), 1);
        assert!(c.running && c.at(100 * MS) == 20_000);
        // A new generation (a seek) starts from its own sound.
        let c = model.update(110 * MS, Some(5_000_000), 2);
        assert_eq!((c.generation, c.at(110 * MS)), (2, 5_000_000));
        model.freeze(120 * MS);
        assert!(!model.clock().running && model.clock().at(0) == 5_010_000);
    }
}
