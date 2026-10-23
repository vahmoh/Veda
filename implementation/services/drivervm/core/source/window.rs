// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The bridge's window: the range of the guest's physical address space,
//! beyond its RAM, where the VMOs its programs map go (`VMO_MAP`).
//!
//! A mapping takes the free range that fits it best (the shortest one long
//! enough), from its start; unmapping frees its range again, joined with
//! the free ranges beside it. The free ranges are kept by start and by
//! length, so that each of these takes time logarithmic in the number of
//! ranges, however many mappings the guest makes.
//!
//! Unmapping is two steps, so that a range is never another mapping's
//! while the kernel unmaps it: [`Window::unuse`] takes the range out of use
//! (it is then neither used nor free: no other mapping takes it, no other
//! unmapping finds it), and once it is unmapped [`Window::free`] frees it,
//! or, if it could not be, [`Window::reuse`] puts it back in use.

use alloc::collections::{BTreeMap, BTreeSet};
use core::ops::Range;

/// The ranges of the window, free and in use.
#[derive(Debug, Clone)]
pub struct Window {
    /// The free ranges: by start, their lengths.
    free: BTreeMap<u64, u64>,
    /// The free ranges by length, then start.
    by_length: BTreeSet<(u64, u64)>,
    /// The ranges in use: by start, their lengths.
    used: BTreeMap<u64, u64>,
}

impl Window {
    /// A window of `range`, all of it free.
    pub fn new(range: Range<u64>) -> Window {
        let mut w = Window { free: BTreeMap::new(), by_length: BTreeSet::new(), used: BTreeMap::new() };
        if range.end > range.start {
            w.insert_free(range.start, range.end - range.start);
        }
        w
    }

    /// Takes `len` bytes for a mapping: their start, from the shortest free
    /// range that is long enough (`None` if none is).
    pub fn take(&mut self, len: u64) -> Option<u64> {
        if len == 0 {
            return None;
        }
        let &(length, start) = self.by_length.range((len, 0)..).next()?;
        self.remove_free(start, length);
        if length > len {
            self.insert_free(start + len, length - len);
        }
        self.used.insert(start, len);
        Some(start)
    }

    /// Takes the range in use at `start` out of use, if it is `len` bytes
    /// long (whether it was): it is then neither used nor free, until it is
    /// freed ([`Window::free`]) or used again ([`Window::reuse`]).
    pub fn unuse(&mut self, start: u64, len: u64) -> bool {
        if self.used.get(&start) != Some(&len) {
            return false;
        }
        self.used.remove(&start);
        true
    }

    /// Frees a range taken out of use: joined with the free ranges that end
    /// where it starts and start where it ends.
    pub fn free(&mut self, start: u64, len: u64) {
        let (mut start, mut len) = (start, len);
        if let Some((&before, &before_len)) = self.free.range(..start).next_back()
            && before + before_len == start
        {
            self.remove_free(before, before_len);
            (start, len) = (before, before_len + len);
        }
        if let Some(&after_len) = self.free.get(&(start + len)) {
            self.remove_free(start + len, after_len);
            len += after_len;
        }
        self.insert_free(start, len);
    }

    /// Puts a range taken out of use back in use (its mapping stayed).
    pub fn reuse(&mut self, start: u64, len: u64) {
        self.used.insert(start, len);
    }

    fn insert_free(&mut self, start: u64, len: u64) {
        self.free.insert(start, len);
        self.by_length.insert((len, start));
    }

    fn remove_free(&mut self, start: u64, len: u64) {
        self.free.remove(&start);
        self.by_length.remove(&(len, start));
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::*;

    const PAGE: u64 = 4096;

    /// A window of `pages` pages from 64 GiB.
    fn window(pages: u64) -> Window {
        Window::new((64 << 30)..(64 << 30) + pages * PAGE)
    }

    /// Unmaps: out of use, then free.
    fn unmap(w: &mut Window, start: u64, len: u64) {
        assert!(w.unuse(start, len));
        w.free(start, len);
    }

    #[test]
    fn mappings_take_the_window_from_its_start_until_it_is_full() {
        let mut w = window(4);
        let base = 64 << 30;
        assert_eq!(w.take(PAGE), Some(base));
        assert_eq!(w.take(2 * PAGE), Some(base + PAGE));
        assert_eq!(w.take(2 * PAGE), None);
        assert_eq!(w.take(PAGE), Some(base + 3 * PAGE));
        assert_eq!(w.take(PAGE), None);
        assert_eq!(w.take(0), None);
    }

    #[test]
    fn freed_ranges_join_their_neighbours() {
        let mut w = window(8);
        let a: Vec<u64> = (0..8).map(|_| w.take(PAGE).unwrap()).collect();
        // Freed out of order: the free ranges join into one again.
        for i in [1, 3, 2, 0, 7, 5, 6, 4] {
            unmap(&mut w, a[i], PAGE);
        }
        assert_eq!(w.free.len(), 1);
        assert_eq!(w.by_length.len(), 1);
        assert_eq!(w.take(8 * PAGE), Some(a[0]));
    }

    #[test]
    fn a_mapping_takes_the_free_range_that_fits_it_best() {
        let mut w = window(16);
        let a = w.take(4 * PAGE).unwrap();
        let b = w.take(PAGE).unwrap();
        let c = w.take(2 * PAGE).unwrap();
        let _d = w.take(PAGE).unwrap();
        // Free: 4 pages at a, 2 at c, and the 8 at the end.
        unmap(&mut w, a, 4 * PAGE);
        unmap(&mut w, c, 2 * PAGE);
        assert_eq!(w.take(2 * PAGE), Some(c));
        assert_eq!(w.take(3 * PAGE), Some(a));
        assert_eq!(w.take(PAGE), Some(a + 3 * PAGE));
        assert!(w.unuse(b, PAGE));
    }

    #[test]
    fn a_range_being_unmapped_is_no_one_elses() {
        let mut w = window(1);
        let a = w.take(PAGE).unwrap();
        // Only a mapping of its length is found, once.
        assert!(!w.unuse(a, 2 * PAGE));
        assert!(!w.unuse(a + PAGE, PAGE));
        assert!(w.unuse(a, PAGE));
        assert!(!w.unuse(a, PAGE), "a second unmapping of the same range finds nothing");
        // While the kernel unmaps it, no mapping takes it.
        assert_eq!(w.take(PAGE), None);
        // The kernel could not: it is the mapping's again.
        w.reuse(a, PAGE);
        assert_eq!(w.take(PAGE), None);
        unmap(&mut w, a, PAGE);
        assert_eq!(w.take(PAGE), Some(a));
    }

    #[test]
    fn many_small_mappings() {
        let mut w = window(1 << 16);
        let all: Vec<u64> = (0..1 << 16).map(|_| w.take(PAGE).unwrap()).collect();
        assert_eq!(w.take(PAGE), None);
        // Every other one freed: none of the free ranges holds two pages.
        for &a in all.iter().step_by(2) {
            unmap(&mut w, a, PAGE);
        }
        assert_eq!(w.take(2 * PAGE), None);
        for &a in all.iter().skip(1).step_by(2) {
            unmap(&mut w, a, PAGE);
        }
        assert_eq!(w.take(1 << 16 << 12), Some(all[0]));
    }
}
