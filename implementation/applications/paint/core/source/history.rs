// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Undo and redo, in tiles.
//!
//! An edit keeps the tiles of the picture it is about to change (squares of [`TILE`] pixels),
//! each the first time it touches it ([`History::touch`]), as they were. Undoing a step swaps
//! its tiles with the picture's, so the step then holds what undoing took away, which redoing
//! swaps back: a brush stroke across a large picture keeps a few tiles, not the picture. An
//! edit that changes the picture's size keeps the whole picture instead.
//!
//! Until it ends, an edit's kept tiles are the picture as it was before it, which the tools
//! work from ([`History::original`], [`History::restore`]): a stroke is composited afresh
//! over them as it grows, and a shape not yet placed is drawn again as it is moved.
//!
//! Steps are kept within a memory budget: the oldest go first, but never the last one.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use gfx::{Bitmap, Rect};

/// A tile's side (a power of two).
pub const TILE: i32 = 64;
const SHIFT: u32 = 6;

/// A tile as an edit found it (or, once undone, as undoing left it).
struct Patch {
    rect: Rect,
    pixels: Vec<u32>,
}

/// What a step changed.
enum Change {
    Tiles(Vec<Patch>),
    /// The whole picture (it changed size).
    Whole(Bitmap),
}

impl Change {
    fn bytes(&self) -> usize {
        match self {
            Change::Tiles(patches) => patches.iter().map(|p| p.pixels.len() * 4 + 32).sum(),
            Change::Whole(b) => b.pixels.len() * 4,
        }
    }

    /// Swaps what the change holds with the picture's.
    fn swap(&mut self, image: &mut Bitmap) {
        match self {
            Change::Tiles(patches) => {
                for p in patches {
                    swap_rect(image, p.rect, &mut p.pixels);
                }
            }
            Change::Whole(b) => core::mem::swap(b, image),
        }
    }

    /// Where the change is: its tiles' bounds, `None` for the whole picture.
    fn area(&self) -> Option<Rect> {
        match self {
            Change::Tiles(patches) => patches.iter().map(|p| p.rect).reduce(|a, b| a.union(&b)),
            Change::Whole(_) => None,
        }
    }
}

/// Swaps the pixels of `r` (inside `image`) with `pixels` (its rows, one after the other).
fn swap_rect(image: &mut Bitmap, r: Rect, pixels: &mut [u32]) {
    let w = image.width as usize;
    for (row, chunk) in pixels.chunks_exact_mut(r.w as usize).enumerate() {
        let start = (r.y as usize + row) * w + r.x as usize;
        image.pixels[start..start + r.w as usize].swap_with_slice(chunk);
    }
}

/// An edit or an undone one: its name, what it changed, and its number.
struct Step {
    id: u64,
    name: String,
    change: Change,
}

/// The edit under way.
struct Open {
    name: String,
    /// The picture's width in tiles, when it began.
    cols: i32,
    /// For each tile of the picture, its patch's index, once kept.
    index: Vec<Option<u32>>,
    patches: Vec<Patch>,
    /// The picture before the edit, if the edit replaces it.
    whole: Option<Bitmap>,
}

/// What undoing or redoing changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Undone {
    /// The step's name ("Brush stroke").
    pub name: String,
    /// Where the picture changed (`None`: all of it, perhaps its size).
    pub area: Option<Rect>,
}

/// The steps to undo and redo.
pub struct History {
    undo: Vec<Step>,
    redo: Vec<Step>,
    open: Option<Open>,
    budget: usize,
    used: usize,
    next_id: u64,
    /// The picture's number before the oldest step kept.
    base: u64,
    /// The number of the picture that was saved (or opened), if any.
    saved: Option<u64>,
}

impl History {
    /// History that keeps its steps within `budget` bytes.
    pub fn new(budget: usize) -> History {
        History { undo: Vec::new(), redo: Vec::new(), open: None, budget, used: 0, next_id: 1, base: 0, saved: Some(0) }
    }

    /// Forgets every step: a new picture, as saved.
    pub fn clear(&mut self) {
        let budget = self.budget;
        *self = History::new(budget);
        self.base = self.next_id;
        self.saved = Some(self.base);
    }

    /// The number of the picture as it is now.
    fn current(&self) -> u64 {
        self.undo.last().map_or(self.base, |s| s.id)
    }

    /// Begins an edit called `name` (ending one under way first).
    pub fn begin(&mut self, name: &str, image: &Bitmap) {
        self.commit();
        let cols = (image.width + TILE - 1) >> SHIFT;
        let rows = (image.height + TILE - 1) >> SHIFT;
        self.open = Some(Open {
            name: name.into(),
            cols,
            index: vec![None; (cols * rows).max(0) as usize],
            patches: Vec::new(),
            whole: None,
        });
    }

    /// Whether an edit is under way.
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// Keeps the tiles `r` covers as they are, before the edit under way changes them (only
    /// those not kept yet; nothing without an edit).
    pub fn touch(&mut self, image: &Bitmap, r: Rect) {
        let Some(open) = &mut self.open else { return };
        if open.whole.is_some() {
            return;
        }
        let r = r.intersect(&image.rect());
        if r.is_empty() {
            return;
        }
        for ty in (r.y >> SHIFT)..=((r.bottom() - 1) >> SHIFT) {
            for tx in (r.x >> SHIFT)..=((r.right() - 1) >> SHIFT) {
                let slot = &mut open.index[(ty * open.cols + tx) as usize];
                if slot.is_some() {
                    continue;
                }
                let tile = Rect::new(tx << SHIFT, ty << SHIFT, TILE, TILE).intersect(&image.rect());
                let mut pixels = Vec::with_capacity((tile.w * tile.h) as usize);
                for y in tile.y..tile.bottom() {
                    let start = (y * image.width + tile.x) as usize;
                    pixels.extend_from_slice(&image.pixels[start..start + tile.w as usize]);
                }
                *slot = Some(open.patches.len() as u32);
                open.patches.push(Patch { rect: tile, pixels });
            }
        }
    }

    /// The pixel at (`x`, `y`) as it was before the edit under way (as it is, outside one, or
    /// where the edit has not touched).
    #[inline]
    pub fn original(&self, image: &Bitmap, x: i32, y: i32) -> u32 {
        if let Some(open) = &self.open
            && open.whole.is_none()
            && let Some(Some(i)) = open.index.get(((y >> SHIFT) * open.cols + (x >> SHIFT)) as usize)
        {
            let p = &open.patches[*i as usize];
            return p.pixels[((y - p.rect.y) * p.rect.w + (x - p.rect.x)) as usize];
        }
        image.get(x, y)
    }

    /// Row `y` of the picture from `x0` to `x1` as it was before the edit under way (both
    /// within the picture), into `out`.
    pub fn original_row(&self, image: &Bitmap, y: i32, x0: i32, x1: i32, out: &mut Vec<u32>) {
        out.clear();
        let mut x = x0;
        while x < x1 {
            // A tile's part of the row at a time.
            let end = (((x >> SHIFT) + 1) << SHIFT).min(x1);
            let len = (end - x) as usize;
            let patch = self.open.as_ref().filter(|o| o.whole.is_none()).and_then(|o| {
                let i = (*o.index.get(((y >> SHIFT) * o.cols + (x >> SHIFT)) as usize)?)?;
                Some(&o.patches[i as usize])
            });
            match patch {
                Some(p) => {
                    let start = ((y - p.rect.y) * p.rect.w + (x - p.rect.x)) as usize;
                    out.extend_from_slice(&p.pixels[start..start + len]);
                }
                None => {
                    let start = (y * image.width + x) as usize;
                    out.extend_from_slice(&image.pixels[start..start + len]);
                }
            }
            x = end;
        }
    }

    /// Puts the pixels of `r` back as they were before the edit under way.
    pub fn restore(&self, image: &mut Bitmap, r: Rect) {
        let Some(open) = &self.open else { return };
        if open.whole.is_some() {
            return;
        }
        let r = r.intersect(&image.rect());
        if r.is_empty() {
            return;
        }
        for ty in (r.y >> SHIFT)..=((r.bottom() - 1) >> SHIFT) {
            for tx in (r.x >> SHIFT)..=((r.right() - 1) >> SHIFT) {
                let Some(Some(i)) = open.index.get((ty * open.cols + tx) as usize) else { continue };
                let p = &open.patches[*i as usize];
                let part = p.rect.intersect(&r);
                for y in part.y..part.bottom() {
                    let src = ((y - p.rect.y) * p.rect.w + (part.x - p.rect.x)) as usize;
                    let dst = (y * image.width + part.x) as usize;
                    image.pixels[dst..dst + part.w as usize].copy_from_slice(&p.pixels[src..src + part.w as usize]);
                }
            }
        }
    }

    /// The edit under way replaces the picture (with one of another size): `old` is the picture
    /// it replaces, as it is now.
    pub fn replace_whole(&mut self, mut old: Bitmap) {
        let Some(open) = &mut self.open else { return };
        if open.whole.is_some() {
            return;
        }
        // What the edit changed so far goes back into the picture kept.
        for p in core::mem::take(&mut open.patches).iter_mut() {
            swap_rect(&mut old, p.rect, &mut p.pixels);
        }
        open.index.iter_mut().for_each(|i| *i = None);
        open.whole = Some(old);
    }

    /// Ends the edit under way, as a step if it changed anything. Returns whether it did.
    pub fn commit(&mut self) -> bool {
        let Some(open) = self.open.take() else { return false };
        let change = match open.whole {
            Some(b) => Change::Whole(b),
            None if open.patches.is_empty() => return false,
            None => Change::Tiles(open.patches),
        };
        for s in self.redo.drain(..) {
            self.used -= s.change.bytes();
        }
        let step = Step { id: self.next_id, name: open.name, change };
        self.next_id += 1;
        self.used += step.change.bytes();
        self.undo.push(step);
        while self.used > self.budget && self.undo.len() > 1 {
            let oldest = self.undo.remove(0);
            self.used -= oldest.change.bytes();
            self.base = oldest.id;
        }
        true
    }

    /// Ends the edit under way, putting back what it changed; returns where.
    pub fn abort(&mut self, image: &mut Bitmap) -> Option<Undone> {
        let open = self.open.take()?;
        let mut change = match open.whole {
            Some(b) => Change::Whole(b),
            None => Change::Tiles(open.patches),
        };
        change.swap(image);
        Some(Undone { name: open.name, area: change.area() })
    }

    /// Undoes the last step (ending an edit under way first, which is then the last step).
    pub fn undo(&mut self, image: &mut Bitmap) -> Option<Undone> {
        self.commit();
        let mut step = self.undo.pop()?;
        step.change.swap(image);
        let undone = Undone { name: step.name.clone(), area: step.change.area() };
        self.redo.push(step);
        Some(undone)
    }

    /// Redoes the last step undone.
    pub fn redo(&mut self, image: &mut Bitmap) -> Option<Undone> {
        self.commit();
        let mut step = self.redo.pop()?;
        step.change.swap(image);
        let redone = Undone { name: step.name.clone(), area: step.change.area() };
        self.undo.push(step);
        Some(redone)
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty() || self.open.as_ref().is_some_and(|o| !o.patches.is_empty() || o.whole.is_some())
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// The name of what Undo would undo.
    pub fn undo_name(&self) -> Option<&str> {
        match &self.open {
            Some(o) if !o.patches.is_empty() || o.whole.is_some() => Some(&o.name),
            _ => self.undo.last().map(|s| s.name.as_str()),
        }
    }

    /// The name of what Redo would redo.
    pub fn redo_name(&self) -> Option<&str> {
        self.redo.last().map(|s| s.name.as_str())
    }

    /// The names of the steps that can be undone, the latest last.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.undo.iter().map(|s| s.name.as_str())
    }

    /// The picture as it is now is saved.
    pub fn mark_saved(&mut self) {
        self.commit();
        self.saved = Some(self.current());
    }

    /// The number of the picture as it is now, without the edit under way (which a save made
    /// meanwhile would not have): what [`History::mark_saved_at`] takes.
    pub fn version(&self) -> u64 {
        self.current()
    }

    /// The picture numbered `version` (from [`History::version`]) is saved: the picture is
    /// unmodified when it is that one again.
    pub fn mark_saved_at(&mut self, version: u64) {
        self.saved = Some(version);
    }

    /// Whether the picture differs from the one saved (or opened).
    pub fn is_modified(&self) -> bool {
        self.open.as_ref().is_some_and(|o| !o.patches.is_empty() || o.whole.is_some())
            || self.saved != Some(self.current())
    }

    /// The memory the steps take (bytes).
    pub fn bytes(&self) -> usize {
        self.used
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(w: i32, h: i32) -> Bitmap {
        let mut b = Bitmap::new(w, h);
        for (i, p) in b.pixels.iter_mut().enumerate() {
            *p = 0xFF00_0000 | i as u32;
        }
        b
    }

    fn paint(h: &mut History, b: &mut Bitmap, r: Rect, px: u32) {
        h.touch(b, r);
        for y in r.y..r.bottom() {
            for x in r.x..r.right() {
                b.pixels[(y * b.width + x) as usize] = px;
            }
        }
    }

    #[test]
    fn undo_and_redo_swap_tiles() {
        let mut b = picture(150, 100);
        let before = b.clone();
        let mut h = History::new(1 << 20);
        assert!(!h.is_modified() && !h.can_undo());
        h.begin("Stroke", &b);
        paint(&mut h, &mut b, Rect::new(60, 10, 10, 70), 0xFFFF_0000);
        // Two tiles across, two down: four kept.
        assert_eq!(h.open.as_ref().map(|o| o.patches.len()), Some(4));
        assert!(h.commit());
        let after = b.clone();
        assert!(h.is_modified());
        assert_eq!(h.undo_name(), Some("Stroke"));
        let undone = h.undo(&mut b).unwrap();
        assert_eq!(undone.area, Some(Rect::new(0, 0, 128, 100)));
        assert_eq!(b.pixels, before.pixels);
        assert!(!h.is_modified());
        assert_eq!(h.redo(&mut b).unwrap().name, "Stroke");
        assert_eq!(b.pixels, after.pixels);
        // A new edit forgets what was undone.
        h.undo(&mut b);
        h.begin("Fill", &b);
        paint(&mut h, &mut b, Rect::new(0, 0, 5, 5), 0xFF00_FF00);
        h.commit();
        assert!(!h.can_redo());
        // An edit that changes nothing is no step.
        h.begin("Nothing", &b);
        assert!(!h.commit());
        assert_eq!(h.names().collect::<Vec<_>>(), ["Fill"]);
    }

    #[test]
    fn the_original_shows_through_an_edit() {
        let mut b = picture(130, 70);
        let before = b.clone();
        let mut h = History::new(1 << 20);
        h.begin("Shape", &b);
        paint(&mut h, &mut b, Rect::new(60, 30, 20, 20), 0xFF12_3456);
        for (x, y) in [(60, 30), (79, 49), (0, 0), (129, 69), (64, 64)] {
            assert_eq!(h.original(&b, x, y), before.get(x, y), "({x}, {y})");
        }
        let mut row = Vec::new();
        h.original_row(&b, 35, 50, 129, &mut row);
        assert_eq!(row, &before.pixels[(35 * 130 + 50) as usize..(35 * 130 + 129) as usize]);
        h.restore(&mut b, Rect::new(0, 0, 70, 70));
        assert_eq!(b.get(65, 35), before.get(65, 35), "restored");
        assert_eq!(b.get(75, 35), 0xFF12_3456, "outside what was restored");
        h.abort(&mut b);
        assert_eq!(b.pixels, before.pixels);
        assert!(!h.is_modified());
    }

    #[test]
    fn a_new_size_keeps_the_whole_picture() {
        let mut b = picture(40, 30);
        let before = b.clone();
        let mut h = History::new(1 << 20);
        h.begin("Crop", &b);
        paint(&mut h, &mut b, Rect::new(0, 0, 4, 4), 0);
        let cropped = b.crop(Rect::new(0, 0, 20, 10));
        h.replace_whole(core::mem::replace(&mut b, cropped));
        h.commit();
        assert_eq!((b.width, b.height), (20, 10));
        assert_eq!(h.undo(&mut b).unwrap().area, None);
        assert_eq!(b.pixels, before.pixels, "the picture before the whole edit");
        h.redo(&mut b);
        assert_eq!((b.width, b.height), (20, 10));
    }

    #[test]
    fn the_budget_drops_the_oldest_steps() {
        let mut b = picture(256, 256);
        // Room for about three steps of one tile each.
        let mut h = History::new(3 * (64 * 64 * 4 + 32));
        for i in 0..6 {
            h.begin("Dot", &b);
            paint(&mut h, &mut b, Rect::new(i * 40, 0, 1, 1), 0);
            h.commit();
        }
        assert_eq!(h.names().count(), 3);
        assert!(h.bytes() <= 3 * (64 * 64 * 4 + 32));
        // The saved picture is gone from the history: it is modified for good.
        assert!(h.is_modified());
        while h.undo(&mut b).is_some() {}
        assert!(h.is_modified());
        // One step larger than the budget is still kept.
        let mut h = History::new(10);
        h.begin("Big", &b);
        paint(&mut h, &mut b, Rect::new(0, 0, 200, 200), 1);
        h.commit();
        assert!(h.can_undo());
    }

    #[test]
    fn saving_marks_the_picture() {
        let mut b = picture(10, 10);
        let mut h = History::new(1 << 20);
        h.begin("A", &b);
        paint(&mut h, &mut b, Rect::new(0, 0, 1, 1), 0);
        h.mark_saved();
        assert!(!h.is_modified());
        h.undo(&mut b);
        assert!(h.is_modified());
        h.redo(&mut b);
        assert!(!h.is_modified());
        h.clear();
        assert!(!h.is_modified() && !h.can_undo());
    }
}
