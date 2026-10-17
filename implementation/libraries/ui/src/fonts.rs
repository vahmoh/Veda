// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The interface's fonts: their files, read once per process, and the text
//! context ([`SharedText`]: the parsed fonts and a glyph cache) that all the
//! windows of a process draw with.
//!
//! A text context costs its fonts' parsing and the glyphs rasterized into
//! its cache, up to 4 MiB. The shell has a window for each of its popups,
//! tooltips and notifications; were each window to have a context of its
//! own, each would parse the fonts and rasterize the same glyphs again, and
//! memory would grow with the windows.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ops::Range;

use gfx::text::font_files;
use gfx::{Align, Canvas, Color, LineMetrics, Rect, Text};
use rt::sync::Mutex;

/// The interface's font files in fallback order; the first four are the
/// [`Font`](crate::Font) roles: Inter, Inter SemiBold, JetBrains Mono and
/// JetBrains Mono Bold, then Lato.
const FONT_FILES: [&str; 5] = [
    font_files::INTER,
    font_files::INTER_SEMIBOLD,
    font_files::JETBRAINS_MONO,
    font_files::JETBRAINS_MONO_BOLD,
    font_files::LATO,
];

/// The font files' contents, read once per process and kept for its life
/// (`None` until they are read; a file that could not be is `None` in it).
static FONT_DATA: Mutex<Option<[Option<&'static [u8]>; FONT_FILES.len()]>> = Mutex::new(None);

/// The text context of the process, made by its first window, and the
/// indices of the font roles in it.
static PROCESS_TEXT: Mutex<Option<(SharedText, [usize; 4])>> = Mutex::new(None);

/// The font files' contents, read from the system image the first time.
fn font_data() -> [Option<&'static [u8]>; FONT_FILES.len()] {
    *FONT_DATA.lock().get_or_insert_with(|| {
        let vfs = proto::connect(proto::vfs::NAME).ok().map(proto::vfs::Client::new);
        FONT_FILES.map(|path| {
            let vfs = vfs.as_ref()?;
            let (vmo, len) = vfs.read_file(path.into()).ok()?.ok()?;
            let mut buf = alloc::vec![0u8; len as usize];
            vmo.read(0, &mut buf).ok()?;
            Some(&*buf.leak())
        })
    })
}

/// The indices of the four [`Font`](crate::Font) roles, given the index of
/// each file's font (a role whose font is missing takes font 0).
fn roles(indices: &[Option<usize>]) -> [usize; 4] {
    core::array::from_fn(|i| indices.get(i).copied().flatten().unwrap_or(0))
}

/// Loads the standard fonts into a text context of its own (programs that
/// draw without a [`Host`](crate::Host), such as games, use it; windows
/// share [`SharedText::process`]'s). Order defines the fallback chain:
/// Inter, Inter SemiBold, JetBrains Mono (+ Bold), Lato. Returns the text
/// renderer and the indices of the [`crate::Font`] roles.
pub fn load_fonts() -> (Text, [usize; 4]) {
    let data = font_data();
    let (text, indices) =
        Text::with_fonts(&FONT_FILES, |path| FONT_FILES.iter().position(|p| *p == path).and_then(|i| data[i]));
    (text, roles(&indices))
}

/// A text context ([`Text`]: fonts and a glyph cache) shared by every
/// handle cloned from it; the windows of a process share
/// [`SharedText::process`]'s.
///
/// Its methods are those of [`Text`]. Each holds the context's lock while it
/// works, so threads take turns; [`SharedText::with`] gives the context
/// itself for anything else.
#[derive(Clone)]
pub struct SharedText(Arc<Mutex<Text>>);

impl SharedText {
    /// Shares `text`.
    pub fn new(text: Text) -> SharedText {
        SharedText(Arc::new(Mutex::new(text)))
    }

    /// The process's text context, with the interface's fonts (read by the
    /// first window that asks), and the indices of the [`crate::Font`] roles.
    pub fn process() -> (SharedText, [usize; 4]) {
        let mut shared = PROCESS_TEXT.lock();
        let (text, roles) = shared.get_or_insert_with(|| {
            let (text, roles) = load_fonts();
            (SharedText::new(text), roles)
        });
        (text.clone(), *roles)
    }

    /// Runs `f` with the text context itself. `f` must not use this shared
    /// context (or a clone): it would wait for itself.
    pub fn with<R>(&self, f: impl FnOnce(&mut Text) -> R) -> R {
        f(&mut self.0.lock())
    }

    /// Whether `other` is a handle to the same context.
    pub fn same(&self, other: &SharedText) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// See [`Text::add_font`]: every window of the process sees the font.
    pub fn add_font(&self, data: &'static [u8]) -> Option<usize> {
        self.with(|t| t.add_font(data))
    }

    /// See [`Text::font_count`].
    pub fn font_count(&self) -> usize {
        self.with(|t| t.font_count())
    }

    /// See [`Text::metrics`].
    pub fn metrics(&self, font: usize, size: f32) -> LineMetrics {
        self.with(|t| t.metrics(font, size))
    }

    /// See [`Text::measure`].
    pub fn measure(&self, font: usize, size: f32, text: &str) -> f32 {
        self.with(|t| t.measure(font, size, text))
    }

    /// See [`Text::index_for_x`].
    pub fn index_for_x(&self, font: usize, size: f32, text: &str, x: f32) -> usize {
        self.with(|t| t.index_for_x(font, size, text, x))
    }

    /// See [`Text::x_for_index`].
    pub fn x_for_index(&self, font: usize, size: f32, text: &str, index: usize) -> f32 {
        self.with(|t| t.x_for_index(font, size, text, index))
    }

    /// See [`Text::wrap`].
    pub fn wrap(&self, font: usize, size: f32, text: &str, max_width: f32) -> Vec<Range<usize>> {
        self.with(|t| t.wrap(font, size, text, max_width))
    }

    /// See [`Text::draw`].
    #[allow(clippy::too_many_arguments)]
    pub fn draw(&self, c: &mut Canvas, font: usize, size: f32, x: f32, y: f32, text: &str, color: Color) -> f32 {
        self.with(|t| t.draw(c, font, size, x, y, text, color))
    }

    /// See [`Text::ellipsize`].
    pub fn ellipsize(&self, font: usize, size: f32, text: &str, max_width: f32) -> String {
        self.with(|t| t.ellipsize(font, size, text, max_width))
    }

    /// See [`Text::draw_in`].
    #[allow(clippy::too_many_arguments)]
    pub fn draw_in(&self, c: &mut Canvas, font: usize, size: f32, r: Rect, text: &str, color: Color, align: Align) {
        self.with(|t| t.draw_in(c, font, size, r, text, color, align))
    }

    /// See [`Text::draw_wrapped`].
    #[allow(clippy::too_many_arguments)]
    pub fn draw_wrapped(
        &self,
        c: &mut Canvas,
        font: usize,
        size: f32,
        r: Rect,
        text: &str,
        color: Color,
        align: Align,
    ) -> i32 {
        self.with(|t| t.draw_wrapped(c, font, size, r, text, color, align))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INTER: &[u8] = include_bytes!("../../../ports/fonts/Inter-Regular.otf");

    #[test]
    fn clones_share_fonts_and_glyphs() {
        let a = SharedText::new(Text::new());
        let b = a.clone();
        assert!(a.same(&b) && !a.same(&SharedText::new(Text::new())));
        assert_eq!(a.add_font(INTER), Some(0));
        assert_eq!(b.font_count(), 1);
        assert!(b.measure(0, 14.0, "Shared") > 0.0);
        assert_eq!(roles(&[Some(2), None, Some(1)]), [2, 0, 1, 0]);
    }
}
