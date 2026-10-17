// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Which decoded picture to show.
//!
//! The decoder publishes pictures ahead of time into the slots of the frame
//! pool. For the frame about to be presented, the player knows the media
//! time it will be seen at; it shows the newest picture due by then (within
//! half a refresh, so a picture lands on the blank nearest its time), gives
//! back the ones it skipped over, and wakes again when the next is due.
//!
//! Every seek is a new *generation* (see `proto::media`). Pictures of
//! earlier generations are given back; those of a later one (a seek the
//! decoder carried out before the player heard of it) wait for the player
//! to catch up.

use alloc::vec::Vec;

use proto::media::SlotMeta;

use crate::playback::is_newer;

/// A published picture: its slot and what the decoder said of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Picture {
    pub slot: u32,
    pub meta: SlotMeta,
}

/// What to do with the pool for the next frame.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Choice {
    /// The picture to show (`None`: keep what is shown).
    pub show: Option<Picture>,
    /// Pictures to give back unseen: skipped over, or of another generation.
    pub drop: Vec<u32>,
    /// The time of the next picture after the one shown, if published.
    pub next_due_us: Option<i64>,
}

/// Chooses among the `ready` pictures (published, not taken) for a frame
/// seen at media time `target_us`, given what is `shown` now. Pictures of
/// earlier generations are dropped, those of later ones left alone. While
/// nothing of this generation is shown (after opening or seeking) its first
/// picture is shown whatever its time, so that the screen shows where
/// playback is.
pub fn choose(
    ready: &[Picture],
    shown: Option<&Picture>,
    generation: u32,
    target_us: i64,
    tolerance_us: i64,
) -> Choice {
    let mut choice = Choice::default();
    let mut current: Vec<&Picture> = Vec::new();
    for p in ready {
        if p.meta.generation == generation {
            current.push(p);
        } else if !is_newer(p.meta.generation, generation) {
            choice.drop.push(p.slot);
        }
    }
    // In the order the decoder published them (their times may repeat or,
    // in a damaged file, go back).
    current.sort_by_key(|p| p.meta.seq);
    let shown_here = shown.filter(|s| s.meta.generation == generation);
    let due = target_us.saturating_add(tolerance_us.max(0));
    let mut pick: Option<&Picture> = None;
    for p in &current {
        if p.meta.pts_us <= due
            && let Some(old) = pick.replace(p)
        {
            choice.drop.push(old.slot);
        }
    }
    if pick.is_none() && shown_here.is_none() {
        pick = current.first().copied();
    }
    if let Some(p) = pick {
        choice.show = Some(*p);
    }
    let after = choice.show.map(|p| p.meta.seq).or(shown_here.map(|s| s.meta.seq));
    choice.next_due_us =
        current.iter().filter(|p| after.is_none_or(|seq| p.meta.seq > seq)).map(|p| p.meta.pts_us).min();
    choice
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn pic(slot: u32, generation: u32, seq: u64, pts_us: i64) -> Picture {
        Picture {
            slot,
            meta: SlotMeta { generation, seq, pts_us, duration_us: 41_667, width: 64, height: 36, flags: 0 },
        }
    }

    #[test]
    fn the_newest_picture_due_is_shown() {
        let ready = vec![pic(0, 1, 1, 0), pic(1, 1, 2, 41_667), pic(2, 1, 3, 83_333)];
        // Seen at 50 ms: the picture of 41.7 ms; the first is skipped.
        let c = choose(&ready, None, 1, 50_000, 6_000);
        assert_eq!(c.show.map(|p| p.slot), Some(1));
        assert_eq!(c.drop, [0]);
        assert_eq!(c.next_due_us, Some(83_333));
        // Within half a refresh of its time: shown on that blank.
        let c = choose(&ready[2..], Some(&ready[1]), 1, 78_000, 6_667);
        assert_eq!(c.show.map(|p| p.slot), Some(2));
    }

    #[test]
    fn nothing_new_keeps_what_is_shown() {
        let shown = pic(4, 1, 7, 100_000);
        let ready = vec![pic(5, 1, 8, 141_667)];
        let c = choose(&ready, Some(&shown), 1, 120_000, 6_000);
        assert_eq!(c.show, None);
        assert!(c.drop.is_empty());
        assert_eq!(c.next_due_us, Some(141_667));
    }

    #[test]
    fn a_new_generation_shows_its_first_picture_at_once() {
        let shown = pic(0, 1, 9, 9_000_000);
        // After a seek to 2 s: the old picture's generation is gone, and
        // the first new picture is shown even though the clock is behind.
        let ready = vec![pic(1, 1, 10, 9_041_667), pic(2, 2, 11, 2_000_000), pic(3, 2, 12, 2_041_667)];
        let c = choose(&ready, Some(&shown), 2, 0, 6_000);
        assert_eq!(c.show.map(|p| p.slot), Some(2));
        assert_eq!(c.drop, [1]);
        assert_eq!(c.next_due_us, Some(2_041_667));
        // Paused on a picture, nothing newer is due: it stays.
        let c = choose(&ready[2..], Some(&ready[1]), 2, 2_000_000, 6_000);
        assert_eq!(c.show, None);
    }

    #[test]
    fn a_later_generation_waits_for_the_player() {
        // The decoder carried out seek 3 before the player heard of it:
        // generation 3's picture stays, generation 1's goes.
        let ready = vec![pic(0, 1, 5, 0), pic(1, 3, 6, 9_000_000), pic(2, 2, 7, 40_000)];
        let c = choose(&ready, None, 2, 50_000, 0);
        assert_eq!(c.show.map(|p| p.slot), Some(2));
        assert_eq!(c.drop, [0]);
        // Across the wrap of the numbers.
        let ready = vec![pic(0, u32::MAX, 1, 0), pic(1, 1, 2, 0)];
        let c = choose(&ready, None, 0, 0, 0);
        assert_eq!((c.show, c.drop), (None, vec![0]));
    }

    #[test]
    fn publication_order_wins_over_times() {
        // A damaged file whose times go back: shown in the order decoded.
        let ready = vec![pic(0, 1, 2, 500_000), pic(1, 1, 1, 400_000)];
        let c = choose(&ready, None, 1, 600_000, 0);
        assert_eq!(c.show.map(|p| p.slot), Some(0));
        assert_eq!(c.drop, [1]);
    }
}
