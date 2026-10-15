// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Covers: what a track's generated cover is, and the few drawn covers
//! kept.
//!
//! A cover is drawn from a style, a hue and a seed ([`CoverSpec`]): tracks
//! made by `musicgen` carry an `art` tag (`style:hue:seed`, the hue in
//! degrees) chosen to fit the music; other files get one derived from a
//! hash of their title. A spec is all integers, so that two specs compare
//! as equal exactly when they draw the same cover: a cover drawn is known
//! by its spec, whatever the library's order.
//!
//! Drawn covers take memory (a large one some 200 KiB), so only the few
//! recently used are kept ([`Recent`]): the large ones of the tracks
//! played lately, the small ones of the rows seen lately.

use alloc::vec::Vec;

/// Cover art styles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Style {
    Synthwave,
    Lofi,
    Aurora,
    Pixel,
    Pulse,
    Waves,
}

const STYLES: [Style; 6] = [Style::Synthwave, Style::Lofi, Style::Aurora, Style::Pixel, Style::Pulse, Style::Waves];

impl Style {
    fn from_name(s: &str) -> Option<Style> {
        Some(match s {
            "synthwave" => Style::Synthwave,
            "lofi" => Style::Lofi,
            "aurora" => Style::Aurora,
            "pixel" => Style::Pixel,
            "pulse" => Style::Pulse,
            "waves" => Style::Waves,
            _ => return None,
        })
    }
}

/// Thousandths of a degree in a turn.
const TURN: u32 = 360_000;

/// What to draw for a track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CoverSpec {
    pub style: Style,
    /// Base hue, in thousandths of a degree (0 .. 360 000).
    hue: u32,
    pub seed: u64,
}

impl CoverSpec {
    /// From an `art` tag (`style:hue_degrees:seed`) or, failing that, from
    /// `hash`. A hue that is not a finite number (or is missing) is the
    /// hash's; others are turned into the first turn.
    pub fn new(tag: Option<&str>, hash: u64) -> CoverSpec {
        let hashed = (((hash >> 8) % 360) as u32) * 1000;
        if let Some(tag) = tag {
            let mut it = tag.split(':');
            if let Some(style) = it.next().and_then(Style::from_name) {
                let hue = it
                    .next()
                    .and_then(|h| h.trim().parse::<f64>().ok())
                    .filter(|h| h.is_finite())
                    .map_or(hashed, |h| ((h * 1000.0) as i64).rem_euclid(TURN as i64) as u32);
                let seed = it.next().and_then(|s| s.parse::<u64>().ok()).unwrap_or(hash);
                return CoverSpec { style, hue, seed };
            }
        }
        CoverSpec { style: STYLES[(hash % 6) as usize], hue: hashed, seed: hash }
    }

    /// The base hue in turns (0 ..< 1).
    pub fn hue(&self) -> f32 {
        self.hue as f32 / TURN as f32
    }
}

/// The few most recently used of some drawn things (`V`), each known by a
/// key (`K`): at most `capacity`, the least recently used going first.
#[derive(Debug, Clone)]
pub struct Recent<K, V> {
    capacity: usize,
    /// Least recently used first.
    items: Vec<(K, V)>,
}

impl<K: PartialEq, V> Recent<K, V> {
    /// Keeping at most `capacity` (at least one).
    pub fn new(capacity: usize) -> Recent<K, V> {
        Recent { capacity: capacity.max(1), items: Vec::new() }
    }

    /// How many are kept.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether none is kept.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Whether `key` is kept (it is not marked as used).
    pub fn contains(&self, key: &K) -> bool {
        self.items.iter().any(|(k, _)| k == key)
    }

    /// The thing kept for `key`, marked as used just now.
    pub fn get(&mut self, key: &K) -> Option<&V> {
        let at = self.items.iter().rposition(|(k, _)| k == key)?;
        let item = self.items.remove(at);
        self.items.push(item);
        self.items.last().map(|(_, v)| v)
    }

    /// Keeps `value` for `key` (used just now), letting the least recently
    /// used go beyond the capacity.
    pub fn put(&mut self, key: K, value: V) {
        self.items.retain(|(k, _)| *k != key);
        if self.items.len() >= self.capacity {
            self.items.remove(0);
        }
        self.items.push((key, value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_and_hashes() {
        let s = CoverSpec::new(Some("aurora:210:42"), 7);
        assert_eq!((s.style, s.hue, s.seed), (Style::Aurora, 210_000, 42));
        assert!((s.hue() - 210.0 / 360.0).abs() < 1e-6);
        // Hues wrap into the first turn.
        assert_eq!(CoverSpec::new(Some("pixel:-30:1"), 7).hue, 330_000);
        assert_eq!(CoverSpec::new(Some("pixel:720.5:1"), 7).hue, 500);
        // No tag, or a style it does not know: from the hash.
        let h = CoverSpec::new(None, 0x1234_5678);
        assert_eq!(h, CoverSpec::new(Some("cubism:10:1"), 0x1234_5678));
        assert_eq!(h.seed, 0x1234_5678);
    }

    #[test]
    fn a_hue_that_is_no_number_is_the_hashs() {
        // "nan" parses as a number that equals nothing, not even itself: a
        // cover drawn for it was never the one wanted, and was drawn again
        // for ever.
        for bad in ["nan", "NaN", "inf", "-inf", "", "x"] {
            // The hash's hue: (0x1234_5600 >> 8) % 360 = 6 degrees.
            let s = CoverSpec::new(Some(&alloc::format!("lofi:{bad}:3")), 0x1234_5600);
            assert_eq!(s, s, "{bad}");
            assert_eq!(s.hue, 6_000, "{bad}");
            assert!(s.hue().is_finite());
        }
        // Huge but finite: still a hue of the first turn.
        assert!(CoverSpec::new(Some("waves:1e300:1"), 0).hue < TURN);
    }

    #[test]
    fn the_least_recently_used_goes_first() {
        let mut r = Recent::new(2);
        r.put(1, "one");
        r.put(2, "two");
        assert_eq!(r.get(&1), Some(&"one"));
        r.put(3, "three");
        assert!(!r.contains(&2), "2 was used least recently");
        assert!(r.contains(&1) && r.contains(&3) && r.len() == 2);
        r.put(3, "three again");
        assert_eq!(r.len(), 2);
        assert_eq!(r.get(&3), Some(&"three again"));
        assert_eq!(r.get(&9), None);
        assert!(Recent::<u8, u8>::new(0).is_empty());
    }
}
