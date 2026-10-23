// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Text on a picture: a box of text as it is typed, wrapped to the box's width, drawn in one
//! of the interface's fonts.

use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;

use gfx::{Bitmap, Canvas, Color, Rect, Text};

/// A box of text on the picture.
#[derive(Debug, Clone, PartialEq)]
pub struct TextBox {
    pub text: String,
    /// The caret (a byte offset at a character's start).
    pub cursor: usize,
    /// The box's top left corner (picture pixels).
    pub at: (i32, i32),
    /// The width lines wrap at (picture pixels).
    pub width: i32,
    /// The text's size (pixels).
    pub size: f32,
    /// Its font, an index in the text context.
    pub font: usize,
    pub color: Color,
    /// A colour behind the text, if any.
    pub background: Option<Color>,
}

impl TextBox {
    /// An empty box at `at`, `width` wide.
    pub fn new(at: (i32, i32), width: i32, size: f32, font: usize, color: Color) -> TextBox {
        TextBox { text: String::new(), cursor: 0, at, width: width.max(8), size, font, color, background: None }
    }

    pub fn insert(&mut self, s: &str) {
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    pub fn backspace(&mut self) {
        if let Some(c) = self.text[..self.cursor].chars().next_back() {
            self.cursor -= c.len_utf8();
            self.text.remove(self.cursor);
        }
    }

    pub fn delete(&mut self) {
        if self.cursor < self.text.len() {
            self.text.remove(self.cursor);
        }
    }

    pub fn left(&mut self) {
        if let Some(c) = self.text[..self.cursor].chars().next_back() {
            self.cursor -= c.len_utf8();
        }
    }

    pub fn right(&mut self) {
        if let Some(c) = self.text[self.cursor..].chars().next() {
            self.cursor += c.len_utf8();
        }
    }

    /// To the start of the caret's line.
    pub fn home(&mut self) {
        self.cursor = self.text[..self.cursor].rfind('\n').map_or(0, |i| i + 1);
    }

    /// To the end of the caret's line.
    pub fn end(&mut self) {
        self.cursor = self.text[self.cursor..].find('\n').map_or(self.text.len(), |i| self.cursor + i);
    }

    /// The lines as wrapped (byte ranges of the text).
    pub fn lines(&self, text: &Text) -> Vec<Range<usize>> {
        text.wrap(self.font, self.size, &self.text, self.width as f32)
    }

    /// The height of a line (pixels).
    pub fn line_height(&self, text: &Text) -> f32 {
        text.metrics(self.font, self.size).line_height.max(1.0)
    }

    /// The box: its width, and as high as its lines (one at least).
    pub fn bounds(&self, text: &Text) -> Rect {
        let lines = self.lines(text).len().max(1);
        let m = text.metrics(self.font, self.size);
        let h = libm::ceilf(lines as f32 * m.line_height.max(1.0) + m.descent.max(0.0)) as i32;
        Rect::new(self.at.0, self.at.1, self.width, h.max(1))
    }

    /// The pixels the text can paint: the box, and a little around it for glyphs that reach
    /// past their advance.
    pub fn reach(&self, text: &Text) -> Rect {
        let pad = libm::ceilf(self.size * 0.3) as i32 + 2;
        self.bounds(text).inflate(pad)
    }

    /// Where the caret is: its top, in picture pixels, and its height.
    pub fn caret(&self, text: &Text) -> (f32, f32, f32) {
        let lines = self.lines(text);
        let h = self.line_height(text);
        // The line the caret is in: the last that starts at or before it.
        let i = lines.iter().rposition(|l| l.start <= self.cursor).unwrap_or(0);
        let line = lines.get(i).cloned().unwrap_or(0..0);
        let upto = self.cursor.clamp(line.start, line.end.max(line.start));
        let x =
            text.x_for_index(self.font, self.size, &self.text[line.start..line.end.max(line.start)], upto - line.start);
        (self.at.0 as f32 + x, self.at.1 as f32 + i as f32 * h, h)
    }

    /// The caret's position for a click at (`x`, `y`) in picture pixels.
    pub fn index_at(&self, text: &Text, x: f32, y: f32) -> usize {
        let lines = self.lines(text);
        let h = self.line_height(text);
        let i = ((y - self.at.1 as f32) / h).max(0.0) as usize;
        let Some(line) = lines.get(i.min(lines.len().saturating_sub(1))).cloned() else { return 0 };
        line.start + text.index_for_x(self.font, self.size, &self.text[line.clone()], x - self.at.0 as f32)
    }

    /// Draws the text (and its background) into `picture`, within `clip`.
    pub fn draw(&self, text: &mut Text, picture: &mut Bitmap, clip: Rect) {
        let mut canvas = Canvas::for_bitmap(picture);
        canvas.clip_to(clip);
        if let Some(bg) = self.background {
            canvas.fill_rect(self.bounds(text), bg);
        }
        let m = text.metrics(self.font, self.size);
        let h = m.line_height.max(1.0);
        for (i, range) in self.lines(text).into_iter().enumerate() {
            let line = self.text[range].trim_end_matches(['\n', '\r']);
            let baseline = libm::floorf(self.at.1 as f32 + m.ascent + i as f32 * h + 0.5);
            text.draw(&mut canvas, self.font, self.size, self.at.0 as f32, baseline, line, self.color);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brush::filled;

    const INTER: &[u8] = include_bytes!("../../../../ports/fonts/Inter-Regular.otf");

    fn text() -> Text {
        let mut t = Text::new();
        t.add_font(INTER);
        t
    }

    #[test]
    fn typing_and_moving() {
        let mut b = TextBox::new((10, 10), 200, 20.0, 0, Color::BLACK);
        b.insert("Hello");
        b.insert("\nWorld");
        b.home();
        assert_eq!(b.cursor, 6);
        b.backspace();
        assert_eq!(b.text, "HelloWorld");
        b.end();
        b.left();
        b.delete();
        assert_eq!(b.text, "HelloWorl");
        b.right();
        assert_eq!(b.cursor, b.text.len());
        b.insert("é");
        b.left();
        b.delete();
        assert_eq!(b.text, "HelloWorl");
    }

    #[test]
    fn text_wraps_and_draws_in_its_box() {
        let mut t = text();
        let mut b = TextBox::new((5, 5), 60, 16.0, 0, Color::BLACK);
        b.insert("one two three four five");
        let lines = b.lines(&t);
        assert!(lines.len() >= 3, "{lines:?}");
        let bounds = b.bounds(&t);
        assert_eq!((bounds.x, bounds.y, bounds.w), (5, 5, 60));
        let mut p = filled(100, 120, 0xFFFF_FFFF);
        let reach = b.reach(&t);
        b.draw(&mut t, &mut p, reach);
        let inked: Vec<(i32, i32)> =
            (0..120).flat_map(|y| (0..100).map(move |x| (x, y))).filter(|&(x, y)| p.get(x, y) != 0xFFFF_FFFF).collect();
        assert!(inked.len() > 50);
        assert!(inked.iter().all(|&(x, y)| reach.contains(x, y)));
        // The caret at the end is on the last line.
        let (_, cy, h) = b.caret(&t);
        assert!((cy - (5.0 + (lines.len() - 1) as f32 * h)).abs() < 0.01);
        // A click near the start puts the caret there.
        assert_eq!(b.index_at(&t, 5.0, 6.0), 0);
        b.background = Some(Color::hex(0x00FF00));
        let mut p = filled(100, 120, 0xFFFF_FFFF);
        b.draw(&mut t, &mut p, reach);
        assert_eq!(p.get(6, 6), 0xFF00_FF00);
    }
}
