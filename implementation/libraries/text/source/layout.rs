// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! [`Layout`]: buffer lines broken into visual rows on a monospace grid
//! (soft wrap), with hit testing and vertical movement.
//!
//! Columns are display columns: every character is one cell wide except
//! tabs, which advance to the next tab stop, and characters that take no
//! cell (accents that combine with the character before, zero-width
//! characters).
//!
//! Laying out walks every character, so a layout of a [`Document`] is kept
//! and brought up to date after edits ([`Layout::update`]): only the lines
//! the edits changed are laid out again; the rows of the others are kept,
//! moved along when lines come or go.

use alloc::vec::Vec;

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthChar;

use crate::buffer::{Buffer, Pos};
use crate::document::{Document, LineChange};

/// Width in cells of `c` when it starts at display column `x`: tabs advance to the next tab
/// stop, characters of no width (`unicode-width`: combining accents, zero-width spaces and
/// joiners) take no cell, as the fonts draw them, and every other character one. (Wide East
/// Asian characters too: the fonts draw them, glyphless, in one cell.)
pub fn char_width(c: char, x: usize, tab: usize) -> usize {
    match c {
        '\t' => tab - x % tab,
        c if c.width() == Some(0) => 0,
        _ => 1,
    }
}

/// Display column of byte offset `col` in `line`.
pub fn display_col(line: &str, col: usize, tab: usize) -> usize {
    let mut x = 0;
    for c in line[..col.min(line.len())].chars() {
        x += char_width(c, x, tab.max(1));
    }
    x
}

/// One visual row: the bytes `start..end` of buffer line `line`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    pub line: usize,
    pub start: usize,
    pub end: usize,
}

/// The rows of a buffer for a given wrap width.
#[derive(Debug, Clone)]
pub struct Layout {
    rows: Vec<Row>,
    /// Index of the first row of each line.
    first_row: Vec<usize>,
    /// The width of each line's widest row, in cells.
    widths: Vec<usize>,
    /// The wrap width asked for (`None`: no wrapping).
    wrap: Option<usize>,
    tab: usize,
    /// Widest row in cells.
    max_width: usize,
    /// The document laid out and its revision, if a document's.
    source: Option<(u64, u64)>,
}

/// Lays out line `index`: its rows (no wider than `wrap` cells, if given)
/// are appended to `rows`; returns the width of the widest.
fn lay_out_line(index: usize, line: &str, wrap: Option<usize>, tab: usize, rows: &mut Vec<Row>) -> usize {
    match wrap {
        Some(cols) => wrap_line(index, line, cols.max(4), tab, rows),
        None => {
            rows.push(Row { line: index, start: 0, end: line.len() });
            display_col(line, line.len(), tab)
        }
    }
}

/// Splits one line into rows of at most `cols` cells, breaking after spaces
/// where possible. Spaces may hang past the edge rather than start a row.
/// Returns the width of the widest row.
fn wrap_line(index: usize, line: &str, cols: usize, tab: usize, rows: &mut Vec<Row>) -> usize {
    let mut widest = 0;
    let mut start = 0;
    loop {
        let mut x = 0;
        let mut brk = None;
        let mut end = line.len();
        let mut width_at_brk = 0;
        for (off, c) in line[start..].char_indices() {
            let i = start + off;
            let w = char_width(c, x, tab);
            if x + w > cols && i > start && c != ' ' {
                end = brk.unwrap_or(i);
                x = if brk.is_some() { width_at_brk } else { x };
                break;
            }
            x += w;
            if c == ' ' || c == '\t' {
                brk = Some(i + c.len_utf8());
                width_at_brk = x;
            }
        }
        widest = widest.max(x);
        rows.push(Row { line: index, start, end });
        if end >= line.len() {
            break;
        }
        start = end;
    }
    widest
}

impl Layout {
    /// Lays out `buf`, wrapping rows at `wrap` cells (`None` = no wrapping).
    pub fn new(buf: &Buffer, wrap: Option<usize>, tab: usize) -> Layout {
        let tab = tab.max(1);
        let mut rows = Vec::with_capacity(buf.line_count());
        let mut first_row = Vec::with_capacity(buf.line_count());
        let mut widths = Vec::with_capacity(buf.line_count());
        for (i, line) in buf.lines().enumerate() {
            first_row.push(rows.len());
            widths.push(lay_out_line(i, line, wrap, tab, &mut rows));
        }
        let max_width = widths.iter().copied().max().unwrap_or(0);
        Layout { rows, first_row, widths, wrap, tab, max_width, source: None }
    }

    /// Lays out `doc`'s buffer as [`Layout::new`] does, to be kept up to
    /// date with [`Layout::update`].
    pub fn for_document(doc: &Document, wrap: Option<usize>, tab: usize) -> Layout {
        Layout { source: Some((doc.id(), doc.revision())), ..Layout::new(doc.buffer(), wrap, tab) }
    }

    /// Brings the layout up to date with `doc`, wrapping at `wrap` cells
    /// with tab stops `tab` apart: the lines changed since the revision laid
    /// out last are laid out again, and the rest are kept. A layout of
    /// another document, or of other widths, is laid out anew.
    pub fn update(&mut self, doc: &Document, wrap: Option<usize>, tab: usize) {
        let change = match self.source {
            Some((id, revision)) if id == doc.id() && wrap == self.wrap && tab.max(1) == self.tab => {
                doc.changes_since(revision)
            }
            _ => None,
        };
        match change {
            Some(change) => {
                self.relay(doc.buffer(), change);
                self.source = Some((doc.id(), doc.revision()));
            }
            None => *self = Layout::for_document(doc, wrap, tab),
        }
    }

    /// Lays out again the lines of `buf` that `change` says are new; the
    /// rows of the lines before and after them are kept.
    fn relay(&mut self, buf: &Buffer, change: LineChange) {
        let (old_lines, new_lines) = (self.first_row.len(), buf.line_count());
        // Kept lines are lines both before and after the change has.
        let before = change.kept_before.min(old_lines).min(new_lines);
        let after = change.kept_after.min(old_lines - before).min(new_lines - before);
        let (old_end, new_end) = (old_lines - after, new_lines - after);
        if before == old_end && before == new_end {
            return;
        }
        let row_at = |line: usize| self.first_row.get(line).copied().unwrap_or(self.rows.len());
        let (from, to) = (row_at(before), row_at(old_end));
        let mut rows = Vec::new();
        let mut first_row = Vec::with_capacity(new_end - before);
        let mut widths = Vec::with_capacity(new_end - before);
        let mut widest = 0;
        for i in before..new_end {
            first_row.push(from + rows.len());
            let width = lay_out_line(i, buf.line(i), self.wrap, self.tab, &mut rows);
            widest = widest.max(width);
            widths.push(width);
        }
        // Where the widest row was is only looked for again when it went
        // and no new row is as wide (it is mostly typed on, and grows).
        let widest_went = widest < self.max_width && self.widths[before..old_end].contains(&self.max_width);
        let added = rows.len();
        self.rows.splice(from..to, rows);
        self.first_row.splice(before..old_end, first_row);
        self.widths.splice(before..old_end, widths);
        // The kept lines after the change, and their rows, have moved.
        for row in &mut self.rows[from + added..] {
            row.line = row.line + new_end - old_end;
        }
        for first in &mut self.first_row[new_end..] {
            *first = *first + from + added - to;
        }
        self.max_width =
            if widest_went { self.widths.iter().copied().max().unwrap_or(0) } else { self.max_width.max(widest) };
    }

    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    pub fn row(&self, i: usize) -> Row {
        self.rows[i.min(self.rows.len() - 1)]
    }

    /// Width of the widest row in cells.
    pub fn max_width(&self) -> usize {
        self.max_width
    }

    /// The first row of `line`.
    pub fn first_row_of_line(&self, line: usize) -> usize {
        self.first_row[line.min(self.first_row.len() - 1)]
    }

    /// The last row of `line`.
    fn last_row_of_line(&self, line: usize) -> usize {
        self.first_row.get(line + 1).copied().unwrap_or(self.rows.len()) - 1
    }

    fn is_last_row_of_line(&self, r: usize) -> bool {
        r + 1 >= self.rows.len() || self.rows[r + 1].line != self.rows[r].line
    }

    /// The row containing `p`. A position at a wrap point belongs to the
    /// row that starts there.
    pub fn row_of(&self, p: Pos) -> usize {
        let line = p.line.min(self.first_row.len() - 1);
        let (first, last) = (self.first_row[line], self.last_row_of_line(line));
        let mut r = first;
        while r < last && p.col >= self.rows[r].end {
            r += 1;
        }
        r
    }

    /// Display column of `p` within its row.
    pub fn x_of(&self, buf: &Buffer, p: Pos) -> usize {
        let row = self.rows[self.row_of(p)];
        let line = buf.line(row.line);
        let mut x = 0;
        for c in line[row.start..p.col.clamp(row.start, row.end)].chars() {
            x += char_width(c, x, self.tab);
        }
        x
    }

    /// The position closest to display column `x` in row `r`.
    pub fn pos_at(&self, buf: &Buffer, r: usize, x: usize) -> Pos {
        let r = r.min(self.rows.len() - 1);
        let row = self.rows[r];
        let line = buf.line(row.line);
        let mut cx = 0;
        for (off, c) in line[row.start..row.end].char_indices() {
            let w = char_width(c, cx, self.tab);
            if x < cx + w.div_ceil(2) {
                return Pos::new(row.line, row.start + off);
            }
            cx += w;
        }
        Pos::new(row.line, self.row_end_col(buf, r))
    }

    /// Where the cursor goes for "end of row": the line end on the last row
    /// of a line, otherwise before the row's final character (so the cursor
    /// stays on that row).
    fn row_end_col(&self, buf: &Buffer, r: usize) -> usize {
        let row = self.rows[r];
        if self.is_last_row_of_line(r) {
            row.end
        } else {
            let line = buf.line(row.line);
            line[row.start..row.end].grapheme_indices(true).next_back().map_or(row.start, |(off, _)| row.start + off)
        }
    }

    /// Moves `delta` rows up (negative) or down from `p`, aiming for display
    /// column `goal`. Beyond the first/last row it goes to the start/end of
    /// the buffer.
    pub fn vertical(&self, buf: &Buffer, p: Pos, goal: usize, delta: isize) -> Pos {
        let r = self.row_of(p) as isize + delta;
        if r < 0 {
            Pos::new(0, 0)
        } else if r >= self.rows.len() as isize {
            buf.end()
        } else {
            self.pos_at(buf, r as usize, goal)
        }
    }

    /// Start of the row containing `p`.
    pub fn row_start(&self, p: Pos) -> Pos {
        let row = self.rows[self.row_of(p)];
        Pos::new(row.line, row.start)
    }

    /// End of the row containing `p`.
    pub fn row_end(&self, buf: &Buffer, p: Pos) -> Pos {
        let r = self.row_of(p);
        Pos::new(self.rows[r].line, self.row_end_col(buf, r))
    }

    /// Whether row `r` continues a wrapped line.
    pub fn is_continuation(&self, r: usize) -> bool {
        self.rows[r].start > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Selection;

    /// Checks that `l`, kept up to date, is what laying `doc` out anew gives.
    fn assert_fresh(l: &Layout, doc: &Document, what: &str) {
        let fresh = Layout::new(doc.buffer(), l.wrap, l.tab);
        let rows = |l: &Layout| (0..l.row_count()).map(|i| l.row(i)).collect::<Vec<_>>();
        assert_eq!(rows(l), rows(&fresh), "{what}");
        assert_eq!(l.first_row, fresh.first_row, "{what}");
        assert_eq!(l.widths, fresh.widths, "{what}");
        assert_eq!(l.max_width(), fresh.max_width(), "{what}");
    }

    #[test]
    fn layouts_kept_up_to_date_match_new_ones() {
        let text = "fn main() {\n    let words = \"the quick brown fox jumps over the lazy dog\";\n\n\tlong_line_without_spaces_to_break_anywhere_at_all\n}\n";
        for wrap in [None, Some(12), Some(30)] {
            let mut doc = Document::from_text(text);
            let mut l = Layout::for_document(&doc, wrap, 4);
            // Typing, line breaks, deleting across lines, pasting many lines,
            // replacing everywhere, undoing and redoing.
            let edits: &[fn(&mut Document)] = &[
                |d| d.set_cursor(Pos::new(1, 8), false),
                |d| d.type_text("x"),
                |d| d.type_text("yz "),
                |d| d.newline(),
                |d| d.paste("one\ntwo three four five six seven\nthree"),
                |d| d.set_selection(Selection::new(Pos::new(0, 3), Pos::new(4, 2))),
                |d| d.delete_selection(),
                |d| {
                    d.replace_all("e", "EEEE", true);
                },
                |d| {
                    d.undo();
                },
                |d| {
                    d.undo();
                    d.undo();
                },
                |d| {
                    d.redo();
                },
                |d| d.select_all(),
                |d| d.paste(""),
                |d| d.type_text("a\nb\nc"),
            ];
            for (i, edit) in edits.iter().enumerate() {
                edit(&mut doc);
                l.update(&doc, wrap, 4);
                assert_fresh(&l, &doc, &alloc::format!("wrap {wrap:?} after edit {i}"));
            }
            // Another document, or other widths, are laid out anew.
            let other = Document::from_text("x\ny");
            l.update(&other, wrap, 4);
            assert_fresh(&l, &other, "another document");
            l.update(&other, Some(8), 2);
            assert_eq!((l.wrap, l.tab), (Some(8), 2));
            assert_fresh(&l, &other, "other widths");
        }
    }

    #[test]
    fn many_edits_between_updates_are_recorded_together() {
        let mut doc = Document::from_text("a\nb\nc\nd\ne\nf\ng\nh");
        let mut l = Layout::for_document(&doc, Some(10), 4);
        // More edits than are recorded apart, far down and near the top.
        for i in 0..100 {
            doc.set_cursor(Pos::new(if i % 2 == 0 { 7 } else { 1 }, 0), false);
            doc.type_text("word ");
        }
        l.update(&doc, Some(10), 4);
        assert_fresh(&l, &doc, "after 100 edits");
        let copy = doc.clone();
        assert_ne!(copy.id(), doc.id());
    }

    #[test]
    fn no_wrap_rows_are_lines() {
        let b = Buffer::from_text("a\tb\n\nlonger line");
        let l = Layout::new(&b, None, 4);
        assert_eq!(l.row_count(), 3);
        assert_eq!(l.max_width(), 11);
        assert_eq!(l.x_of(&b, Pos::new(0, 2)), 4);
        assert_eq!(l.pos_at(&b, 0, 3), Pos::new(0, 2));
        assert_eq!(l.pos_at(&b, 0, 1), Pos::new(0, 1));
        assert_eq!(l.pos_at(&b, 2, 99), Pos::new(2, 11));
        assert_eq!(l.vertical(&b, Pos::new(0, 2), 4, 2), Pos::new(2, 4));
        assert_eq!(l.vertical(&b, Pos::new(0, 2), 4, -1), Pos::new(0, 0));
        assert_eq!(l.vertical(&b, Pos::new(2, 2), 4, 1), Pos::new(2, 11));
    }

    #[test]
    fn wraps_at_spaces() {
        let b = Buffer::from_text("the quick brown fox");
        let l = Layout::new(&b, Some(10), 4);
        let rows: Vec<&str> = (0..l.row_count()).map(|i| l.row(i)).map(|r| &b.line(0)[r.start..r.end]).collect();
        assert_eq!(rows, ["the quick ", "brown fox"]);
        // The wrap point belongs to the second row.
        assert_eq!(l.row_of(Pos::new(0, 10)), 1);
        assert_eq!(l.row_of(Pos::new(0, 9)), 0);
        assert_eq!(l.row_end(&b, Pos::new(0, 2)), Pos::new(0, 9));
        assert_eq!(l.row_start(Pos::new(0, 12)), Pos::new(0, 10));
        assert_eq!(l.vertical(&b, Pos::new(0, 2), 2, 1), Pos::new(0, 12));
        assert!(l.is_continuation(1));
    }

    #[test]
    fn hard_breaks_long_words() {
        let b = Buffer::from_text("abcdefghij");
        let l = Layout::new(&b, Some(4), 4);
        let rows: Vec<(usize, usize)> = (0..l.row_count()).map(|i| (l.row(i).start, l.row(i).end)).collect();
        assert_eq!(rows, [(0, 4), (4, 8), (8, 10)]);
        assert_eq!(l.pos_at(&b, 2, 9), Pos::new(0, 10));
        // Spaces hang instead of starting a row.
        let b = Buffer::from_text("abcd    efgh");
        let l = Layout::new(&b, Some(4), 4);
        assert_eq!(l.row(1).start, 8);
    }

    #[test]
    fn display_columns() {
        assert_eq!(display_col("\tx", 1, 4), 4);
        assert_eq!(display_col("ab\tx", 3, 4), 4);
        assert_eq!(display_col("é", 2, 4), 1);
        // A combining accent and a zero-width space take no cell, as the fonts draw them.
        let s = "e\u{301}x\u{200b}y";
        assert_eq!(display_col(s, s.find('x').unwrap(), 4), 1);
        assert_eq!(display_col(s, s.len(), 4), 3);
    }

    #[test]
    fn clicks_land_between_characters_with_their_accents() {
        let b = Buffer::from_text("ce\u{301}x");
        let l = Layout::new(&b, None, 4);
        let x_of = |col| l.x_of(&b, Pos::new(0, col));
        // c, é (two code points, one cell), x.
        assert_eq!((x_of(0), x_of(1), x_of(4), x_of(5)), (0, 1, 2, 3));
        for x in 0..4 {
            let col = l.pos_at(&b, 0, x).col;
            assert!([0, 1, 4, 5].contains(&col), "column {x} gave byte {col}");
        }
    }
}
