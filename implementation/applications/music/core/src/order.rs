// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The order tracks play in: the library's, or shuffled with the track
//! playing first (so that "next" moves on from it); the tracks before and
//! after one; and what plays when a track ends, as repeat says.

use alloc::vec::Vec;

/// What repeats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Repeat {
    /// Nothing: playing stops after the last track.
    Off,
    /// The library: after the last track, the first.
    All,
    /// The track playing.
    One,
}

impl Repeat {
    /// Its name, as the log and the voice agent say it.
    pub fn name(self) -> &'static str {
        match self {
            Repeat::Off => "off",
            Repeat::All => "all",
            Repeat::One => "one",
        }
    }

    /// The next of the three (the repeat button cycles through them).
    pub fn cycled(self) -> Repeat {
        match self {
            Repeat::Off => Repeat::All,
            Repeat::All => Repeat::One,
            Repeat::One => Repeat::Off,
        }
    }
}

/// What to do when a track ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterEnd {
    /// Play track `i` (of the library).
    Play(usize),
    /// Stop.
    Stop,
}

/// The order `n` tracks play in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Order {
    order: Vec<usize>,
}

impl Order {
    /// The order of `n` tracks: the library's, or (`shuffle`) as
    /// `shuffled` mixes it, with track `current` moved first.
    pub fn new(n: usize, shuffle: bool, current: Option<usize>, shuffled: impl FnOnce(&mut [usize])) -> Order {
        let mut order: Vec<usize> = (0..n).collect();
        if shuffle && n > 1 {
            shuffled(&mut order);
            if let Some(c) = current
                && let Some(p) = order.iter().position(|&i| i == c)
            {
                order.swap(0, p);
            }
        }
        Order { order }
    }

    /// The tracks in the order.
    pub fn len(&self) -> usize {
        self.order.len()
    }

    /// Whether there is none.
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// The track `step` places from `current` in this order (from before
    /// the first when nothing plays), going round past either end if
    /// `wrap`.
    pub fn neighbour(&self, current: Option<usize>, step: i64, wrap: bool) -> Option<usize> {
        let n = self.order.len() as i64;
        if n == 0 {
            return None;
        }
        let at = current.and_then(|c| self.order.iter().position(|&i| i == c)).map_or(-1, |p| p as i64);
        let mut p = at.saturating_add(step);
        if !(0..n).contains(&p) {
            if !wrap {
                return None;
            }
            p = p.rem_euclid(n);
        }
        self.order.get(p as usize).copied()
    }

    /// What plays after `current` ended.
    pub fn after_end(&self, current: Option<usize>, repeat: Repeat) -> AfterEnd {
        let next = match repeat {
            Repeat::One => current,
            Repeat::All => self.neighbour(current, 1, true),
            Repeat::Off => self.neighbour(current, 1, false),
        };
        next.map_or(AfterEnd::Stop, AfterEnd::Play)
    }

    /// The tracks that play after `current` on their own, at most `count`
    /// (each once).
    pub fn up_next(&self, current: Option<usize>, repeat: Repeat, count: usize) -> Vec<usize> {
        if repeat == Repeat::One {
            return current.into_iter().collect();
        }
        let mut out = Vec::new();
        for step in 1..=count as i64 {
            match self.neighbour(current, step, repeat == Repeat::All) {
                Some(i) if Some(i) != current && !out.contains(&i) => out.push(i),
                _ => break,
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn reverse(v: &mut [usize]) {
        v.reverse();
    }

    #[test]
    fn library_order_and_neighbours() {
        let o = Order::new(4, false, Some(2), reverse);
        assert_eq!(o.order, [0, 1, 2, 3], "not shuffled");
        assert_eq!(o.neighbour(Some(2), 1, false), Some(3));
        assert_eq!(o.neighbour(Some(3), 1, false), None, "the last");
        assert_eq!(o.neighbour(Some(3), 1, true), Some(0));
        assert_eq!(o.neighbour(Some(0), -1, true), Some(3));
        assert_eq!(o.neighbour(None, 1, false), Some(0), "nothing playing: the first");
        assert_eq!(o.neighbour(Some(9), 1, false), Some(0), "a track not in the order: from the start");
        assert_eq!(Order::new(0, true, None, reverse).neighbour(None, 1, true), None);
        assert!(o.neighbour(Some(1), i64::MAX, true).is_some(), "far steps do not overflow");
    }

    #[test]
    fn shuffled_with_the_track_playing_first() {
        let o = Order::new(5, true, Some(1), reverse);
        assert_eq!(o.order, [1, 3, 2, 4, 0]);
        assert_eq!(o.neighbour(Some(1), 1, false), Some(3));
        assert_eq!(Order::new(1, true, Some(0), |_| panic!("one track is not shuffled")).order, [0]);
    }

    #[test]
    fn what_plays_when_a_track_ends() {
        let o = Order::new(3, false, None, reverse);
        assert_eq!(o.after_end(Some(1), Repeat::Off), AfterEnd::Play(2));
        assert_eq!(o.after_end(Some(2), Repeat::Off), AfterEnd::Stop);
        assert_eq!(o.after_end(Some(2), Repeat::All), AfterEnd::Play(0));
        assert_eq!(o.after_end(Some(2), Repeat::One), AfterEnd::Play(2));
        assert_eq!(o.after_end(None, Repeat::One), AfterEnd::Stop);
        assert_eq!(Repeat::Off.cycled().cycled().cycled(), Repeat::Off);
    }

    #[test]
    fn up_next() {
        let o = Order::new(3, false, None, reverse);
        assert_eq!(o.up_next(Some(0), Repeat::Off, 5), vec![1, 2]);
        assert_eq!(o.up_next(Some(1), Repeat::All, 5), vec![2, 0], "round once, without the track playing");
        assert_eq!(o.up_next(Some(1), Repeat::One, 5), vec![1]);
        assert_eq!(Order::new(1, false, None, reverse).up_next(Some(0), Repeat::All, 5), vec![]);
    }
}
