// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Text rendering: a font collection plus a glyph cache, drawing onto a
//! [`Canvas`] with subpixel positioning.

use alloc::string::String;
use alloc::vec::Vec;

use font::{Font, FontCollection, GlyphCache, PositionedGlyph, subpixel_position};
use raster::Point;

use crate::canvas::Canvas;
use crate::color::Color;
use crate::geom::Rect;

/// Horizontal alignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

/// Vertical line metrics in pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LineMetrics {
    pub ascent: f32,
    pub descent: f32,
    pub line_height: f32,
}

/// Where the fonts of Veda's interface are in the system image.
pub mod font_files {
    /// Inter, the interface's typeface.
    pub const INTER: &str = "/system/fonts/Inter-Regular.otf";
    /// Inter SemiBold: headings, buttons, window titles.
    pub const INTER_SEMIBOLD: &str = "/system/fonts/Inter-SemiBold.otf";
    /// JetBrains Mono: code and terminals.
    pub const JETBRAINS_MONO: &str = "/system/fonts/JetBrainsMono-Regular.ttf";
    /// JetBrains Mono Bold.
    pub const JETBRAINS_MONO_BOLD: &str = "/system/fonts/JetBrainsMono-Bold.ttf";
    /// Lato, the fallback for the characters Inter lacks.
    pub const LATO: &str = "/system/fonts/Lato-Regular.ttf";
}

/// Fonts plus a glyph cache.
pub struct Text {
    fonts: FontCollection<'static>,
    cache: GlyphCache,
    scratch: Vec<PositionedGlyph>,
}

// Programs share one text context between their threads (behind a lock):
// it must stay movable between them.
const _: fn() = || {
    fn movable<T: Send>() {}
    movable::<Text>();
};

impl Default for Text {
    fn default() -> Self {
        Self::new()
    }
}

impl Text {
    pub fn new() -> Text {
        Text { fonts: FontCollection::new(), cache: GlyphCache::new(4 << 20), scratch: Vec::new() }
    }

    /// A text context with the fonts of the files `paths` (see
    /// [`font_files`]), in fallback order: characters a font lacks are
    /// looked for in the fonts after it. `read` gives a file's contents, for
    /// the rest of the program (`None` if it cannot be read). Returns the
    /// index of each file's font: `None` for a file not read, or not a font.
    pub fn with_fonts<'p>(
        paths: &[&'p str],
        mut read: impl FnMut(&'p str) -> Option<&'static [u8]>,
    ) -> (Text, Vec<Option<usize>>) {
        let mut text = Text::new();
        let indices = paths.iter().map(|path| read(path).and_then(|data| text.add_font(data))).collect();
        (text, indices)
    }

    /// Adds a font (the data must live for the rest of the program; leak a
    /// `Vec` with `Vec::leak` if needed). Returns its index. Fonts added
    /// later act as fallbacks for characters missing from earlier ones.
    pub fn add_font(&mut self, data: &'static [u8]) -> Option<usize> {
        let font = Font::from_bytes(data).ok()?;
        Some(self.fonts.add(font))
    }

    pub fn font_count(&self) -> usize {
        self.fonts.fonts().len()
    }

    pub fn font(&self, index: usize) -> Option<&Font<'static>> {
        self.fonts.font(index)
    }

    pub fn metrics(&self, font: usize, size: f32) -> LineMetrics {
        let s = self.fonts.scaled(font, size);
        LineMetrics { ascent: s.ascent(), descent: s.descent(), line_height: s.line_height() }
    }

    pub fn measure(&self, font: usize, size: f32, text: &str) -> f32 {
        self.fonts.scaled(font, size).measure(text)
    }

    /// Byte index of the character boundary nearest to `x` (caret placement).
    pub fn index_for_x(&self, font: usize, size: f32, text: &str, x: f32) -> usize {
        self.fonts.scaled(font, size).index_for_x(text, x)
    }

    /// Horizontal position of the caret before byte `index`.
    pub fn x_for_index(&self, font: usize, size: f32, text: &str, index: usize) -> f32 {
        self.fonts.scaled(font, size).x_for_index(text, index)
    }

    /// Line ranges after wrapping `text` to `max_width` pixels.
    pub fn wrap(&self, font: usize, size: f32, text: &str, max_width: f32) -> Vec<core::ops::Range<usize>> {
        self.fonts.scaled(font, size).wrap_lines(text, max_width)
    }

    /// Draws one line with its baseline at `y`, starting at `x`. Returns the
    /// advance width.
    pub fn draw(&mut self, c: &mut Canvas, font: usize, size: f32, x: f32, y: f32, text: &str, color: Color) -> f32 {
        let scaled = self.fonts.scaled(font, size);
        self.scratch.clear();
        // The layout gives where the line ends.
        let width = scaled.layout_line_into(text, Point::new(x, y), &mut self.scratch) - x;
        let clip = c.clip_rect();
        // No glyph reaches further than this from its origin (layout takes
        // no size above 4096).
        let reach = size.clamp(0.0, 4096.0) as i32 * 2;
        let outside = |at: i32, from: i32, to: i32| at.saturating_sub(reach) > to || at.saturating_add(reach) < from;
        for g in &self.scratch {
            let Some(f) = self.fonts.font(g.font as usize) else { continue };
            let (ix, bin) = subpixel_position(g.x);
            let iy = g.y as i32;
            // Skip glyphs that are obviously outside the clip, before their
            // bitmaps are looked up (or rasterized).
            if outside(iy, clip.y, clip.bottom()) || outside(ix, clip.x, clip.right()) {
                continue;
            }
            let bm = self.cache.get(f, g.glyph, size, bin);
            if bm.width == 0 || bm.height == 0 {
                continue;
            }
            c.fill_mask(ix + bm.left, iy - bm.top, bm.width as i32, bm.height as i32, &bm.data, color);
        }
        width
    }

    /// Truncates `text` with an ellipsis so that it fits `max_width`.
    pub fn ellipsize(&self, font: usize, size: f32, text: &str, max_width: f32) -> String {
        if self.measure(font, size, text) <= max_width {
            return String::from(text);
        }
        let ell = "…";
        let budget = max_width - self.measure(font, size, ell);
        let mut end = self.index_for_x(font, size, text, budget.max(0.0));
        while end > 0 && self.measure(font, size, &text[..end]) > budget {
            end = text[..end].char_indices().last().map(|(i, _)| i).unwrap_or(0);
        }
        let mut s = String::from(text[..end].trim_end());
        s.push_str(ell);
        s
    }

    /// Draws a single line inside `r`, vertically centred, aligned
    /// horizontally, truncated with an ellipsis if necessary.
    pub fn draw_in(&mut self, c: &mut Canvas, font: usize, size: f32, r: Rect, text: &str, color: Color, align: Align) {
        let m = self.metrics(font, size);
        let fitted = self.ellipsize(font, size, text, r.w as f32);
        let w = self.measure(font, size, &fitted);
        let x = match align {
            Align::Left => r.x as f32,
            Align::Center => r.x as f32 + (r.w as f32 - w) / 2.0,
            Align::Right => r.right() as f32 - w,
        };
        // `descent` is positive (distance below the baseline).
        let text_h = m.ascent + m.descent;
        let baseline = r.y as f32 + (r.h as f32 - text_h) / 2.0 + m.ascent;
        c.save();
        c.clip_to(r);
        self.draw(c, font, size, x.round_down(), baseline.round_down(), &fitted, color);
        c.restore();
    }

    /// Draws word-wrapped text from the top of `r`; returns the height used.
    pub fn draw_wrapped(
        &mut self,
        c: &mut Canvas,
        font: usize,
        size: f32,
        r: Rect,
        text: &str,
        color: Color,
        align: Align,
    ) -> i32 {
        let m = self.metrics(font, size);
        let lines = self.wrap(font, size, text, r.w as f32);
        let mut y = r.y as f32 + m.ascent;
        for range in lines {
            let line = text[range].trim_end();
            let w = self.measure(font, size, line);
            let x = match align {
                Align::Left => r.x as f32,
                Align::Center => r.x as f32 + (r.w as f32 - w) / 2.0,
                Align::Right => r.right() as f32 - w,
            };
            self.draw(c, font, size, x, y.round_down(), line, color);
            y += m.line_height;
        }
        (y - m.ascent - r.y as f32) as i32
    }
}

trait RoundDown {
    fn round_down(self) -> f32;
}

impl RoundDown for f32 {
    /// Snaps to whole pixels (keeps glyph stems crisp on baselines).
    fn round_down(self) -> f32 {
        libm::floorf(self + 0.5)
    }
}
