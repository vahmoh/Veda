// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The Alt+Tab window switcher: while Alt is held, an overlay shows the
//! open windows (most recently used first) in a grid of cells, each with a
//! thumbnail and a title; Tab and Shift+Tab move the selection and
//! releasing Alt activates it. It shows as many windows as fit on the
//! screen, the most recently used: its size never depends on how many
//! windows there are.

use alloc::vec::Vec;

use gfx::Rect;

/// Largest thumbnail width.
pub const THUMB_W: i32 = 200;
/// Largest thumbnail height.
pub const THUMB_H: i32 = 126;
/// A window's cell, and the margin around the cells.
const CELL_W: i32 = 228;
const CELL_H: i32 = 184;
const PAD: i32 = 18;
/// What the screen keeps around the overlay.
const MARGIN: i32 = 40;

/// The size of a thumbnail of a `w` x `h` picture: as large as fits in
/// [`THUMB_W`] x [`THUMB_H`], of the same proportions, never enlarged.
pub fn thumbnail_size(w: i32, h: i32) -> (i32, i32) {
    let (w, h) = (w.max(1) as i64, h.max(1) as i64);
    let (bw, bh) = (THUMB_W as i64, THUMB_H as i64);
    let (tw, th) = if w <= bw && h <= bh {
        (w, h)
    } else if w * bh >= h * bw {
        (bw, h * bw / w)
    } else {
        (w * bh / h, bh)
    };
    (tw.max(1) as i32, th.max(1) as i32)
}

/// Where the 3 samples of each of `out` thumbnail pixels fall across
/// `size` pixels of the picture: at their middles, in whole pixels (worked
/// out once for a thumbnail, rather than with two divisions a sample).
pub fn taps(out: i32, size: i32) -> Vec<i32> {
    let (out, size) = (out.max(1) as i64, size.max(1) as i64);
    (0..out * 3).map(|i| ((2 * i + 1) * size / (6 * out)).min(size - 1) as i32).collect()
}

/// The switcher, while Alt is held.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Switcher {
    /// Candidate windows, most recently used first.
    pub windows: Vec<u32>,
    pub selected: usize,
    /// The overlay panel on screen.
    pub rect: Rect,
    per_row: i32,
    /// Which switcher this is (each one opened has its own number): what
    /// is drawn of it is drawn anew for the next.
    pub serial: u64,
}

impl Switcher {
    /// How many windows fit on `screen`.
    pub fn capacity(screen: Rect) -> usize {
        let per_row = ((screen.w - 2 * MARGIN - 2 * PAD) / CELL_W).max(1);
        let rows = ((screen.h - 2 * MARGIN - 2 * PAD) / CELL_H).max(1);
        (per_row * rows) as usize
    }

    /// A switcher of `windows` (most recently used first; as many as fit
    /// on `screen`), with the next window selected (the previous,
    /// `backwards`). `None` if there are none.
    pub fn new(mut windows: Vec<u32>, backwards: bool, screen: Rect, serial: u64) -> Option<Switcher> {
        windows.truncate(Switcher::capacity(screen));
        let n = windows.len();
        let selected = match (n, backwards) {
            (0, _) => return None,
            (1, _) => 0,
            (_, true) => n - 1,
            (_, false) => 1,
        };
        let count = n as i32;
        let per_row = count.min(((screen.w - 2 * MARGIN - 2 * PAD) / CELL_W).max(1));
        let rows = (count + per_row - 1) / per_row;
        let (w, h) = (per_row * CELL_W + 2 * PAD, rows * CELL_H + 2 * PAD);
        let rect = Rect::new(screen.x + (screen.w - w) / 2, screen.y + (screen.h - h) / 2, w, h);
        Some(Switcher { windows, selected, rect, per_row, serial })
    }

    /// Moves the selection to the next (or previous) window.
    pub fn step(&mut self, backwards: bool) {
        let n = self.windows.len().max(1);
        self.selected = if backwards { (self.selected + n - 1) % n } else { (self.selected + 1) % n };
    }

    /// The window selected.
    pub fn choice(&self) -> Option<u32> {
        self.windows.get(self.selected).copied()
    }

    /// The cell of window `i`.
    pub fn cell(&self, i: usize) -> Rect {
        let i = i as i32;
        let (col, row) = (i % self.per_row, i / self.per_row);
        Rect::new(self.rect.x + PAD + col * CELL_W, self.rect.y + PAD + row * CELL_H, CELL_W, CELL_H)
    }

    /// Where the thumbnail of window `i` sits inside its cell.
    pub fn thumb_box(&self, i: usize) -> Rect {
        let c = self.cell(i);
        Rect::new(c.x + (CELL_W - THUMB_W) / 2, c.y + 14, THUMB_W, THUMB_H)
    }

    /// Everything the overlay paints, including its shadow.
    pub fn bounds(&self) -> Rect {
        self.rect.inflate(MARGIN)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Rect = Rect::new(0, 0, 1280, 800);

    #[test]
    fn the_next_window_is_selected_first() {
        let s = Switcher::new(alloc::vec![1, 2, 3], false, SCREEN, 1).unwrap();
        assert_eq!(s.choice(), Some(2));
        let mut s = Switcher::new(alloc::vec![1, 2, 3], true, SCREEN, 1).unwrap();
        assert_eq!(s.choice(), Some(3));
        s.step(false);
        assert_eq!(s.choice(), Some(1));
        assert_eq!(Switcher::new(alloc::vec![7], false, SCREEN, 1).unwrap().choice(), Some(7));
        assert!(Switcher::new(Vec::new(), false, SCREEN, 1).is_none());
    }

    #[test]
    fn thumbnails_keep_their_proportions() {
        assert_eq!(thumbnail_size(1280, 800), (THUMB_W, 125));
        assert_eq!(thumbnail_size(400, 1200), (42, THUMB_H));
        assert_eq!(thumbnail_size(100, 50), (100, 50));
        assert_eq!(thumbnail_size(100_000, 1), (THUMB_W, 1));
        // Three samples a pixel, across the whole picture and inside it.
        let t = taps(100, 1000);
        assert_eq!((t.len(), t[0], t[299]), (300, 1, 998));
        assert!(t.windows(2).all(|p| p[0] <= p[1]));
        assert_eq!(taps(4, 2), [0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1]);
    }

    #[test]
    fn the_overlay_stays_on_the_screen_however_many_windows_there_are() {
        let s = Switcher::new((0..10_000).collect(), false, SCREEN, 1).unwrap();
        assert_eq!(s.windows.len(), Switcher::capacity(SCREEN));
        assert!(SCREEN.contains_rect(&s.rect));
        assert!(SCREEN.contains_rect(&s.cell(s.windows.len() - 1)));
        // The most recently used are the ones kept.
        assert_eq!(&s.windows[..3], &[0, 1, 2]);
    }
}
