// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The resolution the scene is rendered at in software: as high as the
//! frame rate allows.
//!
//! Frames running slower than the target make it try the next lower
//! resolution; a change is kept only if frames got faster, since what
//! limits them may be something no resolution changes (the number of
//! draws, the readback of each frame, another window). With room to
//! spare, it tries the next higher one, and keeps it if frames stay fast
//! enough. A change that did not pay is not tried again for a while.

/// Internal resolution divisors to choose from.
pub const SCALES: [f32; 7] = [1.0, 1.15, 1.33, 1.6, 2.0, 2.5, 3.0];

/// What is being tried.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Trial {
    None,
    /// A step to `from`'s neighbour, judged against the frame time before.
    Down {
        from: usize,
        before: f32,
    },
    Up {
        from: usize,
    },
}

pub struct Scale {
    pub level: usize,
    /// The lowest resolution it may go to.
    max_level: usize,
    /// The frame time to keep under (ms).
    target_ms: f32,
    trial: Trial,
    /// Seconds before trying again after a change that did not pay.
    cooldown: f32,
}

impl Scale {
    /// Starting at `level`, keeping frames under `target_ms`, never below
    /// the resolution of `max_level`.
    pub fn new(level: usize, max_level: usize, target_ms: f32) -> Scale {
        Scale { level, max_level: max_level.min(SCALES.len() - 1), target_ms, trial: Trial::None, cooldown: 0.0 }
    }

    /// The divisor now.
    pub fn divisor(&self) -> f32 {
        SCALES[self.level]
    }

    /// Once every couple of seconds, with the mean frame time over them
    /// (ms): returns true when the level changed.
    pub fn judge(&mut self, frame_ms: f32, secs: f32) -> bool {
        self.cooldown = (self.cooldown - secs).max(0.0);
        match self.trial {
            Trial::Down { from, before } => {
                self.trial = Trial::None;
                if frame_ms > before * 0.92 {
                    // It did not help: back, and leave it a while.
                    self.level = from;
                    self.cooldown = 30.0;
                    return true;
                }
                false
            }
            Trial::Up { from } => {
                self.trial = Trial::None;
                if frame_ms > self.target_ms {
                    self.level = from;
                    self.cooldown = 20.0;
                    return true;
                }
                false
            }
            Trial::None if self.cooldown > 0.0 => false,
            Trial::None => {
                if frame_ms > self.target_ms && self.level < self.max_level {
                    self.trial = Trial::Down { from: self.level, before: frame_ms };
                    self.level += 1;
                    true
                } else if frame_ms < self.target_ms * 0.7 && self.level > 0 {
                    self.trial = Trial::Up { from: self.level };
                    self.level -= 1;
                    true
                } else {
                    false
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_a_lower_resolution_only_if_it_helps() {
        // Slow frames that a lower resolution speeds up: it goes down.
        let mut s = Scale::new(0, 6, 18.0);
        assert!(s.judge(25.0, 2.0));
        assert_eq!(s.level, 1);
        assert!(!s.judge(19.0, 2.0));
        assert_eq!(s.level, 1);
        // Slow frames that it does not speed up: back up, and no more
        // tries for a while.
        let mut s = Scale::new(0, 6, 18.0);
        assert!(s.judge(25.0, 2.0));
        assert!(s.judge(24.5, 2.0));
        assert_eq!(s.level, 0);
        assert!(!s.judge(25.0, 2.0));
        // Room to spare: up, kept while fast enough.
        let mut s = Scale::new(3, 6, 18.0);
        assert!(s.judge(9.0, 2.0));
        assert_eq!(s.level, 2);
        assert!(!s.judge(12.0, 2.0));
        assert_eq!(s.level, 2);
    }
}
