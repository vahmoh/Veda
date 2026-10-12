//! When to ask the decoder to leave pictures out.
//!
//! A machine too slow for a video shows it late: pictures pile up behind the
//! clock and are dropped once decoded, which wastes the work. Watching how
//! many pictures come late, the controller asks the decoder to skip work it
//! can do without (the loop filter, then pictures nothing refers to, then
//! all but key frames), and to stop skipping once it keeps up again.

use vproto::media::SkipLevel;

/// Late pictures, as a share of those due, above which it skips more (%).
const BEHIND: u32 = 15;
/// ... and below which, for long enough, it skips less.
const CAUGHT_UP: u32 = 2;
/// Windows (of the player's choosing, about 2 s) in a row behind before
/// skipping more, and caught up before skipping less.
const BEHIND_WINDOWS: u32 = 2;
const CAUGHT_UP_WINDOWS: u32 = 3;
/// Behind again this soon (windows) after skipping less: skipping less was
/// too soon, and the next time waits twice as long (up to 16 times).
const RELAPSE_WINDOWS: u32 = 6;
const MAX_RELAPSES: u32 = 4;

#[derive(Debug, Clone)]
pub struct SkipControl {
    level: SkipLevel,
    behind: u32,
    caught_up: u32,
    /// Windows since it last skipped less, and how often that was too soon.
    since_less: u32,
    relapses: u32,
}

impl Default for SkipControl {
    fn default() -> SkipControl {
        SkipControl::new()
    }
}

fn more(level: SkipLevel) -> SkipLevel {
    match level {
        SkipLevel::Nothing => SkipLevel::LoopFilter,
        SkipLevel::LoopFilter => SkipLevel::NonReference,
        _ => SkipLevel::KeyframesOnly,
    }
}

fn less(level: SkipLevel) -> SkipLevel {
    match level {
        SkipLevel::KeyframesOnly => SkipLevel::NonReference,
        SkipLevel::NonReference => SkipLevel::LoopFilter,
        _ => SkipLevel::Nothing,
    }
}

impl SkipControl {
    pub fn new() -> SkipControl {
        SkipControl { level: SkipLevel::Nothing, behind: 0, caught_up: 0, since_less: u32::MAX, relapses: 0 }
    }

    pub fn level(&self) -> SkipLevel {
        self.level
    }

    /// Starts over (a new video, or a seek: lateness before it says
    /// nothing of what follows).
    pub fn reset(&mut self) {
        self.behind = 0;
        self.caught_up = 0;
    }

    /// One window of playback: `due` pictures were due, `late` of them came
    /// late (dropped, or shown after their time). Returns the new level if
    /// it changes.
    pub fn window(&mut self, due: u32, late: u32) -> Option<SkipLevel> {
        if due == 0 {
            return None;
        }
        self.since_less = self.since_less.saturating_add(1);
        let share = late.min(due) * 100 / due;
        if share > BEHIND {
            self.caught_up = 0;
            self.behind += 1;
            if self.behind >= BEHIND_WINDOWS && self.level != SkipLevel::KeyframesOnly {
                self.behind = 0;
                if self.since_less <= RELAPSE_WINDOWS {
                    self.relapses = (self.relapses + 1).min(MAX_RELAPSES);
                }
                self.level = more(self.level);
                return Some(self.level);
            }
        } else if share < CAUGHT_UP {
            self.behind = 0;
            self.caught_up += 1;
            if self.caught_up >= CAUGHT_UP_WINDOWS << self.relapses && self.level != SkipLevel::Nothing {
                self.caught_up = 0;
                self.since_less = 0;
                self.level = less(self.level);
                return Some(self.level);
            }
        } else {
            self.behind = 0;
            self.caught_up = 0;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn behind_skips_more_and_catching_up_less() {
        let mut c = SkipControl::new();
        assert_eq!(c.window(48, 20), None, "once is not enough");
        assert_eq!(c.window(48, 20), Some(SkipLevel::LoopFilter));
        assert_eq!(c.window(48, 20), None);
        assert_eq!(c.window(48, 20), Some(SkipLevel::NonReference));
        c.window(48, 20);
        assert_eq!(c.window(48, 20), Some(SkipLevel::KeyframesOnly));
        c.window(48, 20);
        assert_eq!(c.window(48, 20), None, "it skips no more than that");
        // In between: neither.
        assert_eq!(c.window(48, 4), None);
        for _ in 0..2 {
            assert_eq!(c.window(48, 0), None);
        }
        assert_eq!(c.window(48, 0), Some(SkipLevel::NonReference));
        assert_eq!(c.window(0, 0), None, "nothing due says nothing");
        c.reset();
        assert_eq!(c.window(48, 0), None);
    }

    #[test]
    fn skipping_less_too_soon_waits_longer_next_time() {
        let mut c = SkipControl::new();
        // Behind: the loop filter goes; caught up: it comes back.
        c.window(30, 30);
        assert_eq!(c.window(30, 30), Some(SkipLevel::LoopFilter));
        for _ in 0..2 {
            assert_eq!(c.window(30, 0), None);
        }
        assert_eq!(c.window(30, 0), Some(SkipLevel::Nothing));
        // Behind again at once: back to skipping, and now it takes six
        // windows caught up, not three, to skip less.
        c.window(30, 30);
        assert_eq!(c.window(30, 30), Some(SkipLevel::LoopFilter));
        for _ in 0..5 {
            assert_eq!(c.window(30, 0), None);
        }
        assert_eq!(c.window(30, 0), Some(SkipLevel::Nothing));
    }
}
