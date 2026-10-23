// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Integer pixel geometry.
//!
//! The arithmetic saturates at `i32`'s ends: a rectangle reaching past
//! them ends there, never wraps round to the other side.

/// An integer rectangle (`w`/`h` are never negative).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect { x, y, w: if w < 0 { 0 } else { w }, h: if h < 0 { 0 } else { h } }
    }

    pub const fn from_size(w: i32, h: i32) -> Rect {
        Rect::new(0, 0, w, h)
    }

    pub const fn right(&self) -> i32 {
        self.x.saturating_add(self.w)
    }

    pub const fn bottom(&self) -> i32 {
        self.y.saturating_add(self.h)
    }

    pub const fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.right() && y < self.bottom()
    }

    /// Whether `o` lies entirely inside this rectangle (an empty `o` is
    /// inside anything that is not empty itself).
    pub fn contains_rect(&self, o: &Rect) -> bool {
        !self.is_empty()
            && (o.is_empty()
                || (o.x >= self.x && o.y >= self.y && o.right() <= self.right() && o.bottom() <= self.bottom()))
    }

    /// The overlap of two rectangles. Disjoint rectangles give the empty
    /// rectangle at the origin, so that an empty result never carries
    /// coordinates outside both inputs.
    pub fn intersect(&self, o: &Rect) -> Rect {
        let x0 = self.x.max(o.x);
        let y0 = self.y.max(o.y);
        let x1 = self.right().min(o.right());
        let y1 = self.bottom().min(o.bottom());
        if x1 <= x0 || y1 <= y0 {
            return Rect::default();
        }
        Rect::new(x0, y0, x1.saturating_sub(x0), y1.saturating_sub(y0))
    }

    pub fn intersects(&self, o: &Rect) -> bool {
        !self.intersect(o).is_empty()
    }

    /// What of the rectangle lies outside all of `holes`, as rectangles
    /// that do not overlap: about each hole, the bands across it above and
    /// below the hole, and what lies beside it.
    pub fn without(&self, holes: &[Rect]) -> alloc::vec::Vec<Rect> {
        let mut parts = alloc::vec::Vec::new();
        if !self.is_empty() {
            parts.push(*self);
        }
        for hole in holes {
            let mut rest = alloc::vec::Vec::with_capacity(parts.len() + 3);
            for p in parts {
                let h = p.intersect(hole);
                if h.is_empty() {
                    rest.push(p);
                    continue;
                }
                let around = [
                    Rect::new(p.x, p.y, p.w, h.y - p.y),
                    Rect::new(p.x, h.bottom(), p.w, p.bottom() - h.bottom()),
                    Rect::new(p.x, h.y, h.x - p.x, h.h),
                    Rect::new(h.right(), h.y, p.right() - h.right(), h.h),
                ];
                rest.extend(around.into_iter().filter(|r| !r.is_empty()));
            }
            parts = rest;
        }
        parts
    }

    /// Smallest rectangle containing both (empty rectangles are ignored).
    pub fn union(&self, o: &Rect) -> Rect {
        if self.is_empty() {
            return *o;
        }
        if o.is_empty() {
            return *self;
        }
        let x0 = self.x.min(o.x);
        let y0 = self.y.min(o.y);
        let (x1, y1) = (self.right().max(o.right()), self.bottom().max(o.bottom()));
        Rect::new(x0, y0, x1.saturating_sub(x0), y1.saturating_sub(y0))
    }

    pub fn translate(&self, dx: i32, dy: i32) -> Rect {
        Rect { x: self.x.saturating_add(dx), y: self.y.saturating_add(dy), ..*self }
    }

    /// Grows (positive) or shrinks (negative) on every side.
    pub fn inflate(&self, d: i32) -> Rect {
        let twice = d.saturating_mul(2);
        Rect::new(
            self.x.saturating_sub(d),
            self.y.saturating_sub(d),
            self.w.saturating_add(twice),
            self.h.saturating_add(twice),
        )
    }

    pub fn inset(&self, left: i32, top: i32, right: i32, bottom: i32) -> Rect {
        Rect::new(
            self.x.saturating_add(left),
            self.y.saturating_add(top),
            self.w.saturating_sub(left).saturating_sub(right),
            self.h.saturating_sub(top).saturating_sub(bottom),
        )
    }

    pub fn center(&self) -> (i32, i32) {
        (self.x.saturating_add(self.w / 2), self.y.saturating_add(self.h / 2))
    }

    /// Centers a `w` x `h` rectangle inside this one.
    pub fn centered(&self, w: i32, h: i32) -> Rect {
        let (dx, dy) = (self.w.saturating_sub(w) / 2, self.h.saturating_sub(h) / 2);
        Rect::new(self.x.saturating_add(dx), self.y.saturating_add(dy), w, h)
    }

    pub fn area(&self) -> i64 {
        if self.is_empty() { 0 } else { self.w as i64 * self.h as i64 }
    }

    /// Splits off the top `h` pixels: (top, rest).
    pub fn split_top(&self, h: i32) -> (Rect, Rect) {
        let h = h.clamp(0, self.h);
        (Rect::new(self.x, self.y, self.w, h), Rect::new(self.x, self.y.saturating_add(h), self.w, self.h - h))
    }

    /// Splits off the left `w` pixels: (left, rest).
    pub fn split_left(&self, w: i32) -> (Rect, Rect) {
        let w = w.clamp(0, self.w);
        (Rect::new(self.x, self.y, w, self.h), Rect::new(self.x.saturating_add(w), self.y, self.w - w, self.h))
    }

    /// Splits off the bottom `h` pixels: (rest, bottom).
    pub fn split_bottom(&self, h: i32) -> (Rect, Rect) {
        let h = h.clamp(0, self.h);
        (Rect::new(self.x, self.y, self.w, self.h - h), Rect::new(self.x, self.bottom() - h, self.w, h))
    }

    /// Splits off the right `w` pixels: (rest, right).
    pub fn split_right(&self, w: i32) -> (Rect, Rect) {
        let w = w.clamp(0, self.w);
        (Rect::new(self.x, self.y, self.w - w, self.h), Rect::new(self.right() - w, self.y, w, self.h))
    }
}

/// A list of damaged rectangles that merges overlapping or nearby regions.
#[derive(Debug, Clone, Default)]
pub struct Damage {
    rects: alloc::vec::Vec<Rect>,
}

impl Damage {
    pub fn new() -> Damage {
        Damage::default()
    }

    pub fn add(&mut self, r: Rect) {
        if r.is_empty() {
            return;
        }
        let mut r = r;
        // Merge with everything it overlaps (or nearly touches), repeatedly.
        loop {
            let near = r.inflate(8);
            match self.rects.iter().position(|o| o.intersects(&near)) {
                Some(i) => r = r.union(&self.rects.swap_remove(i)),
                None => break,
            }
        }
        self.rects.push(r);
        // Too many fragments cost more than one big repaint.
        if self.rects.len() > 16 {
            let all = self.rects.iter().fold(Rect::default(), |a, b| a.union(b));
            self.rects.clear();
            self.rects.push(all);
        }
    }

    pub fn rects(&self) -> &[Rect] {
        &self.rects
    }

    pub fn is_empty(&self) -> bool {
        self.rects.is_empty()
    }

    pub fn clear(&mut self) {
        self.rects.clear();
    }

    pub fn bounds(&self) -> Rect {
        self.rects.iter().fold(Rect::default(), |a, b| a.union(b))
    }

    pub fn take(&mut self) -> alloc::vec::Vec<Rect> {
        core::mem::take(&mut self.rects)
    }

    /// Where the picture `new` differs from `old`, both `width` x `height`
    /// pixels with rows `stride` apart: each band of changed rows as one
    /// rectangle, as wide as its changes. Empty when nothing changed; the
    /// whole picture when the buffers are too short for it.
    ///
    /// Rows are compared two pixels at a time, from either end, so that a
    /// row costs as far as its first and last change.
    pub fn of_changes(old: &[u32], new: &[u32], width: i32, height: i32, stride: usize) -> Damage {
        Damage::of_changes_outside(old, new, width, height, stride, Rect::default())
    }

    /// Like [`Damage::of_changes`], but `except` is left out: what lies there
    /// is not compared (what changed of it is known otherwise, as of a
    /// picture a window keeps). A band of rows that changed on both sides of
    /// it spans it.
    pub fn of_changes_outside(
        old: &[u32],
        new: &[u32],
        width: i32,
        height: i32,
        stride: usize,
        except: Rect,
    ) -> Damage {
        let mut damage = Damage::new();
        let Some((w, h)) = whole_rows(old, new, width, height, stride) else {
            damage.add(Rect::new(0, 0, width, height));
            return damage;
        };
        // The columns left out of the rows that cross `except`.
        let except = except.intersect(&Rect::new(0, 0, width, height));
        let (left, right) = (except.x.max(0) as usize, except.right().max(0) as usize);
        damage.add_bands(0..h, |y| {
            let (old, new) = (&old[y * stride..][..w], &new[y * stride..][..w]);
            if except.is_empty() || (y as i32) < except.y || (y as i32) >= except.bottom() {
                return changed_columns(old, new);
            }
            let before = changed_columns(&old[..left], &new[..left]);
            let after = changed_columns(&old[right..], &new[right..]).map(|(a, b)| (a + right, b + right));
            match (before, after) {
                (Some((a, _)), Some((_, b))) => Some((a, b)),
                (before, after) => before.or(after),
            }
        });
        damage
    }

    /// Like [`Damage::of_changes`], but of what lies in `within` alone: only
    /// its pixels are compared, as where a picture a window keeps may have
    /// changed (what is outside it has not).
    pub fn of_changes_within(old: &[u32], new: &[u32], width: i32, height: i32, stride: usize, within: Rect) -> Damage {
        let mut damage = Damage::new();
        let within = within.intersect(&Rect::new(0, 0, width, height));
        if within.is_empty() {
            return damage;
        }
        if whole_rows(old, new, width, height, stride).is_none() {
            damage.add(within);
            return damage;
        }
        let (x0, x1) = (within.x as usize, within.right() as usize);
        damage.add_bands(within.y as usize..within.bottom() as usize, |y| {
            let row = y * stride;
            changed_columns(&old[row + x0..row + x1], &new[row + x0..row + x1]).map(|(a, b)| (a + x0, b + x0))
        });
        damage
    }

    /// Adds each band of `rows` whose rows changed, by `changes` (the
    /// columns from a row's first change to its last), as one rectangle as
    /// wide as its changes.
    fn add_bands(&mut self, rows: core::ops::Range<usize>, mut changes: impl FnMut(usize) -> Option<(usize, usize)>) {
        // The band of changed rows being gathered: its first row and columns.
        let mut band: Option<(usize, usize, usize)> = None;
        for y in rows.start..=rows.end {
            let change = (y < rows.end).then(|| changes(y)).flatten();
            band = match (band, change) {
                (Some((top, l, r)), Some((a, b))) => Some((top, l.min(a), r.max(b))),
                (None, Some((a, b))) => Some((y, a, b)),
                (Some((top, l, r)), None) => {
                    self.add(Rect::new(l as i32, top as i32, (r - l) as i32, (y - top) as i32));
                    None
                }
                (None, None) => None,
            };
        }
    }
}

/// The width and height of two pictures of `width` x `height` pixels with
/// rows `stride` apart, if both buffers hold them whole.
fn whole_rows(old: &[u32], new: &[u32], width: i32, height: i32, stride: usize) -> Option<(usize, usize)> {
    let (w, h) = (width.max(0) as usize, height.max(0) as usize);
    let needed = if h == 0 { 0 } else { stride.saturating_mul(h - 1).saturating_add(w) };
    (stride >= w && old.len() >= needed && new.len() >= needed).then_some((w, h))
}

/// The columns from the first pixel that differs between two rows to the
/// last, if any does.
fn changed_columns(old: &[u32], new: &[u32]) -> Option<(usize, usize)> {
    let n = old.len().min(new.len());
    let (old_pairs, _) = old[..n].as_chunks::<2>();
    let (new_pairs, _) = new[..n].as_chunks::<2>();
    let differs = |i: usize| old[i] != new[i];
    // A row of an odd width ends with a pixel of no pair.
    let odd_differs = n % 2 == 1 && differs(n - 1);
    let first = match old_pairs.iter().zip(new_pairs).position(|(a, b)| a != b) {
        Some(p) if differs(2 * p) => 2 * p,
        Some(p) => 2 * p + 1,
        None if odd_differs => n - 1,
        None => return None,
    };
    let last = if odd_differs {
        n - 1
    } else {
        // A pair differs, since the odd pixel does not: the last one.
        let p = old_pairs.iter().zip(new_pairs).rposition(|(a, b)| a != b).unwrap_or(first / 2);
        if differs(2 * p + 1) { 2 * p + 1 } else { 2 * p }
    };
    Some((first, last + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rectangles_near_the_ends_of_i32_do_not_wrap() {
        let far = Rect::new(i32::MAX - 10, i32::MAX - 10, 100, 100);
        assert_eq!((far.right(), far.bottom()), (i32::MAX, i32::MAX));
        assert!(far.contains(i32::MAX - 1, i32::MAX - 1));
        assert!(!far.contains(0, 0));
        let screen = Rect::new(0, 0, 1280, 800);
        assert!(far.intersect(&screen).is_empty());
        assert_eq!(screen.translate(i32::MAX, 0).x, i32::MAX);
        assert_eq!(Rect::new(i32::MIN, 0, 10, 10).inflate(5).x, i32::MIN);
        assert_eq!(screen.inflate(i32::MAX).w, i32::MAX);
        assert_eq!(screen.inset(i32::MIN, 0, 0, 0).w, i32::MAX);
        assert_eq!(Rect::new(i32::MIN, i32::MIN, i32::MAX, i32::MAX).union(&far).w, i32::MAX);
        // Within range, as before.
        assert_eq!(screen.intersect(&Rect::new(1200, 700, 200, 200)), Rect::new(1200, 700, 80, 100));
        assert_eq!(screen.centered(80, 40), Rect::new(600, 380, 80, 40));
        assert_eq!(screen.inset(10, 20, 30, 40), Rect::new(10, 20, 1240, 740));
    }

    #[test]
    fn what_lies_outside_holes_is_covered_once() {
        let r = Rect::new(2, 3, 20, 16);
        let cases: [&[Rect]; 6] = [
            &[],
            &[Rect::new(6, 7, 5, 4)],
            // Across an edge, and beyond it altogether.
            &[Rect::new(-4, 10, 10, 30), Rect::new(40, 0, 5, 5)],
            // Several, side by side and one in another's band.
            &[Rect::new(4, 4, 4, 4), Rect::new(8, 4, 6, 10), Rect::new(16, 14, 3, 3)],
            // The same twice, and all of it.
            &[Rect::new(6, 7, 5, 4), Rect::new(6, 7, 5, 4)],
            &[Rect::new(0, 0, 30, 30)],
        ];
        for holes in cases {
            let parts = r.without(holes);
            assert!(parts.iter().all(|p| !p.is_empty()));
            for y in -2..24 {
                for x in -2..26 {
                    let outside = r.contains(x, y) && !holes.iter().any(|h| h.contains(x, y));
                    let n = parts.iter().filter(|p| p.contains(x, y)).count();
                    assert_eq!(n, usize::from(outside), "({x}, {y}) without {holes:?}: {parts:?}");
                }
            }
        }
        assert!(Rect::default().without(&[]).is_empty());
    }
}
