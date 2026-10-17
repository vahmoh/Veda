// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The [`Font`] type: a zero-copy view of an OpenType/TrueType font file.
//!
//! The file is read by the `ttf-parser` crate: its tables, character maps, names and outlines
//! (TrueType and CFF), every read bounds-checked. What Veda decides on top of them stays
//! here: which character map and names to use, which metrics lay text out, the x-height and cap
//! height when the font does not give them, and the cached kerning.

use alloc::string::String;
use core::sync::atomic::{AtomicU32, Ordering};

use raster::{Path, Point, Rect};
use ttf_parser::cmap::Subtable as CmapSubtable;
use ttf_parser::{Face, FaceParsingError, GlyphId as RawGlyphId, OutlineBuilder, PlatformId, Tag};

use crate::kern::{KernCache, Kerning};
use crate::name::{self, name_id};
use crate::outline::{BoundsSink, OutlineSink, PathSink};
use crate::{FontError, GlyphId};

/// Source of unique font ids (see [`Font::id`]).
static NEXT_FONT_ID: AtomicU32 = AtomicU32::new(1);

/// The outline format of a font.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutlineFormat {
    /// Quadratic TrueType outlines (`glyf`/`loca`).
    TrueType,
    /// Cubic PostScript outlines (`CFF `).
    Cff,
}

/// Font-wide metrics in font units (y up: ascenders positive, descenders negative).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FontMetrics {
    /// Font units per em.
    pub units_per_em: u16,
    /// Ascender used for layout: the OS/2 typo ascender if the font sets `USE_TYPO_METRICS`,
    /// otherwise the `hhea` ascender.
    pub ascender: i16,
    /// Descender used for layout (negative), chosen like `ascender`.
    pub descender: i16,
    /// Line gap used for layout, chosen like `ascender`.
    pub line_gap: i16,
    /// `hhea` ascender.
    pub hhea_ascender: i16,
    /// `hhea` descender.
    pub hhea_descender: i16,
    /// `hhea` line gap.
    pub hhea_line_gap: i16,
    /// OS/2 typographic ascender (0 without OS/2).
    pub typo_ascender: i16,
    /// OS/2 typographic descender (0 without OS/2).
    pub typo_descender: i16,
    /// OS/2 typographic line gap (0 without OS/2).
    pub typo_line_gap: i16,
    /// OS/2 Windows ascent (0 without OS/2).
    pub win_ascent: u16,
    /// OS/2 Windows descent, positive (0 without OS/2).
    pub win_descent: u16,
    /// Height of lowercase letters (OS/2 v2+, else measured from 'x', else estimated).
    pub x_height: i16,
    /// Height of capital letters (OS/2 v2+, else measured from 'H', else estimated).
    pub cap_height: i16,
    /// Union of all glyph bounding boxes (`head`): minimum x.
    pub x_min: i16,
    /// Minimum y of all glyphs.
    pub y_min: i16,
    /// Maximum x of all glyphs.
    pub x_max: i16,
    /// Maximum y of all glyphs.
    pub y_max: i16,
    /// Weight class (400 regular, 700 bold; 400 without OS/2).
    pub weight_class: u16,
    /// Underline position (top of the underline, usually negative; from `post`).
    pub underline_position: i16,
    /// Underline thickness (from `post`).
    pub underline_thickness: i16,
    /// Strikeout position (from OS/2).
    pub strikeout_position: i16,
    /// Strikeout thickness (from OS/2).
    pub strikeout_size: i16,
    /// The font declares itself monospaced (`post.isFixedPitch`).
    pub is_monospace: bool,
}

/// A parsed font. Parsing is zero-copy: the font borrows the file data for `'a`.
///
/// Creating a `Font` validates the table directory and the tables needed for metrics, character
/// mapping and outlines; glyph data is decoded lazily and every access is bounds-checked, so
/// malformed fonts produce errors (or empty glyphs) but never panics.
///
/// `Font` uses interior mutability for its kerning cache and is meant for single-threaded use.
#[derive(Clone)]
pub struct Font<'a> {
    data: &'a [u8],
    face: Face<'a>,
    id: u32,
    metrics: FontMetrics,
    num_glyphs: u16,
    inv_upem: f32,
    /// The character map's chosen subtable, and whether it is Microsoft's symbol encoding.
    cmap: Option<(CmapSubtable<'a>, bool)>,
    format: OutlineFormat,
    kerning: Kerning<'a>,
    kern_cache: Option<KernCache>,
    ascii: [u16; 128],
}

impl core::fmt::Debug for Font<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Font")
            .field("id", &self.id)
            .field("num_glyphs", &self.num_glyphs)
            .field("units_per_em", &self.metrics.units_per_em)
            .field("outline_format", &self.outline_format())
            .finish()
    }
}

/// Ranks a character map's encoding record; lower is better, `None` = not usable. Unicode's full
/// repertoire first, then the BMP, then Microsoft's symbol encoding.
fn cmap_rank(platform: PlatformId, encoding: u16) -> Option<u8> {
    Some(match (platform, encoding) {
        (PlatformId::Windows, 10) => 0,
        (PlatformId::Unicode, 6) => 1,
        (PlatformId::Unicode, 4) => 2,
        (PlatformId::Windows, 1) => 3,
        (PlatformId::Unicode, 3) => 4,
        (PlatformId::Unicode, _) => 5,
        (PlatformId::Windows, 0) => 6,
        _ => return None,
    })
}

/// Feeds `ttf-parser`'s outline commands to an [`OutlineSink`], counting the contours.
struct Builder<'s, S: ?Sized>(&'s mut S, usize);

impl<S: OutlineSink + ?Sized> OutlineBuilder for Builder<'_, S> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.1 += 1;
        self.0.move_to(x, y);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.0.line_to(x, y);
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.0.quad_to(x1, y1, x, y);
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.0.curve_to(x1, y1, x2, y2, x, y);
    }

    fn close(&mut self) {
        self.0.close();
    }
}

/// A big-endian `u16` at `offset` of a table.
fn u16_at(table: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_be_bytes(table.get(offset..offset + 2)?.try_into().ok()?))
}

impl<'a> Font<'a> {
    /// Parses a font file (`.ttf`/`.otf`; for collections the first font).
    pub fn from_bytes(data: &'a [u8]) -> Result<Font<'a>, FontError> {
        Font::from_collection(data, 0)
    }

    /// The number of fonts in `data`: the font count of a collection (`.ttc`), 1 for a plain font
    /// file and 0 if `data` is not a font.
    pub fn count(data: &[u8]) -> u32 {
        match ttf_parser::fonts_in_collection(data) {
            Some(n) => n,
            None if data.len() >= 4 && matches!(&data[..4], b"\0\x01\0\0" | b"OTTO" | b"true") => 1,
            None => 0,
        }
    }

    /// Parses font number `index` of a collection (index 0 for plain font files).
    pub fn from_collection(data: &'a [u8], index: u32) -> Result<Font<'a>, FontError> {
        let face = Face::parse(data, index).map_err(|e| match e {
            FaceParsingError::MalformedFont => FontError::Truncated,
            FaceParsingError::UnknownMagic => FontError::UnsupportedFormat,
            FaceParsingError::FaceIndexOutOfBounds => FontError::InvalidFontIndex,
            FaceParsingError::NoHeadTable => FontError::MissingTable(*b"head"),
            FaceParsingError::NoHheaTable => FontError::MissingTable(*b"hhea"),
            FaceParsingError::NoMaxpTable => FontError::MissingTable(*b"maxp"),
        })?;
        let tables = face.tables();
        if tables.hmtx.is_none() {
            return Err(FontError::MissingTable(*b"hmtx"));
        }
        let num_glyphs = face.number_of_glyphs();
        let upem = face.units_per_em();
        if !(16..=16384).contains(&upem) {
            return Err(FontError::MalformedTable(*b"head"));
        }
        let bbox = face.global_bounding_box();
        let mut m = FontMetrics {
            units_per_em: upem,
            x_min: bbox.x_min,
            y_min: bbox.y_min,
            x_max: bbox.x_max,
            y_max: bbox.y_max,
            weight_class: 400,
            hhea_ascender: tables.hhea.ascender,
            hhea_descender: tables.hhea.descender,
            hhea_line_gap: tables.hhea.line_gap,
            ..FontMetrics::default()
        };

        // OS/2
        let mut use_typo = false;
        if let Some(os2) = tables.os2 {
            m.weight_class = os2.weight().to_number();
            let strikeout = os2.strikeout_metrics();
            m.strikeout_size = strikeout.thickness;
            m.strikeout_position = strikeout.position;
            m.typo_ascender = os2.typographic_ascender();
            m.typo_descender = os2.typographic_descender();
            m.typo_line_gap = os2.typographic_line_gap();
            // Unsigned in the table, which `ttf-parser` reads as signed.
            let raw = face.raw_face().table(Tag::from_bytes(b"OS/2")).unwrap_or_default();
            m.win_ascent = u16_at(raw, 74).unwrap_or(0);
            m.win_descent = u16_at(raw, 76).unwrap_or(0);
            use_typo = os2.use_typographic_metrics();
            m.x_height = os2.x_height().unwrap_or(0);
            m.cap_height = os2.capital_height().unwrap_or(0);
        }
        // In i32: a corrupt font's values may not subtract within i16.
        let typo_height = m.typo_ascender as i32 - m.typo_descender as i32;
        if (use_typo && typo_height > 0) || (m.hhea_ascender == 0 && m.hhea_descender == 0) {
            m.ascender = m.typo_ascender;
            m.descender = m.typo_descender;
            m.line_gap = m.typo_line_gap;
        } else {
            m.ascender = m.hhea_ascender;
            m.descender = m.hhea_descender;
            m.line_gap = m.hhea_line_gap;
        }
        if m.ascender == 0 && m.descender == 0 {
            m.ascender = m.win_ascent.min(i16::MAX as u16) as i16;
            m.descender = -(m.win_descent.min(i16::MAX as u16) as i16);
        }
        if m.ascender == 0 && m.descender == 0 {
            m.ascender = (upem as i32 * 8 / 10) as i16;
            m.descender = -((upem as i32 * 2 / 10) as i16);
        }

        // post
        match tables.post {
            Some(post) => {
                m.underline_position = post.underline_metrics.position;
                m.underline_thickness = post.underline_metrics.thickness;
                m.is_monospace = post.is_monospaced;
            }
            None => {
                m.underline_position = -(upem as i32 / 10) as i16;
                m.underline_thickness = (upem as i32 / 20) as i16;
            }
        }
        if m.strikeout_size == 0 {
            m.strikeout_size = m.underline_thickness;
        }

        // Outlines
        let format = if tables.glyf.is_some() {
            OutlineFormat::TrueType
        } else if tables.cff.is_some() {
            OutlineFormat::Cff
        } else {
            return Err(FontError::MissingTable(*b"glyf"));
        };

        // The best character map subtable.
        let cmap = tables
            .cmap
            .into_iter()
            .flat_map(|c| c.subtables)
            .filter_map(|st| Some((cmap_rank(st.platform_id, st.encoding_id)?, st)))
            .min_by_key(|(rank, _)| *rank)
            .map(|(_, st)| (st, st.platform_id == PlatformId::Windows && st.encoding_id == 0));

        let kerning = Kerning::new(&face);
        let kern_cache = if kerning.is_some() { Some(KernCache::new()) } else { None };
        let mut font = Font {
            data,
            face,
            id: NEXT_FONT_ID.fetch_add(1, Ordering::Relaxed),
            metrics: m,
            num_glyphs,
            inv_upem: 1.0 / upem as f32,
            cmap,
            format,
            kerning,
            kern_cache,
            ascii: [0; 128],
        };
        for c in 0..128u32 {
            font.ascii[c as usize] = font.lookup(c).unwrap_or(0);
        }
        // Measure x-height and cap height when OS/2 does not provide them.
        if font.metrics.x_height <= 0 {
            font.metrics.x_height = font.measure_top('x').unwrap_or((upem as i32 / 2) as i16);
        }
        if font.metrics.cap_height <= 0 {
            font.metrics.cap_height = font.measure_top('H').unwrap_or((upem as i32 * 7 / 10) as i16);
        }
        Ok(font)
    }

    /// The glyph the character map gives code point `cp` (`None` for unmapped characters, glyph
    /// 0 and glyphs past the font's). Symbol fonts have their characters at U+F000 + code.
    fn lookup(&self, cp: u32) -> Option<u16> {
        let (subtable, symbol) = self.cmap.as_ref()?;
        let mapped = |cp: u32| subtable.glyph_index(cp).map(|g| g.0).filter(|&g| g != 0);
        let g = mapped(cp).or_else(|| if *symbol && cp < 0x100 { mapped(0xF000 + cp) } else { None })?;
        (g < self.num_glyphs).then_some(g)
    }

    /// The top of the outline of `c` (font units), if the font has a non-empty glyph for it.
    fn measure_top(&self, c: char) -> Option<i16> {
        let g = self.glyph_index(c)?;
        let b = self.glyph_bounds(g).ok()??;
        Some(b.y1.clamp(i16::MIN as f32, i16::MAX as f32) as i16)
    }

    /// The font file data.
    pub fn data(&self) -> &'a [u8] {
        self.data
    }

    /// A process-unique identifier of this font (clones share it). Used as the font part of
    /// glyph cache keys.
    pub fn id(&self) -> u32 {
        self.id
    }

    /// Raw access to a table by tag.
    pub fn table(&self, tag: [u8; 4]) -> Option<&'a [u8]> {
        self.face.raw_face().table(Tag::from_bytes(&tag)).filter(|d| !d.is_empty())
    }

    /// Font units per em.
    #[inline]
    pub fn units_per_em(&self) -> u16 {
        self.metrics.units_per_em
    }

    /// `1 / units_per_em`.
    #[inline]
    pub(crate) fn inv_upem(&self) -> f32 {
        self.inv_upem
    }

    /// Number of glyphs (`maxp`).
    #[inline]
    pub fn num_glyphs(&self) -> u16 {
        self.num_glyphs
    }

    /// The outline format.
    pub fn outline_format(&self) -> OutlineFormat {
        self.format
    }

    /// Font-wide metrics in font units.
    #[inline]
    pub fn metrics(&self) -> &FontMetrics {
        &self.metrics
    }

    /// Maps a character to a glyph (`None` if the font has no glyph for it).
    #[inline]
    pub fn glyph_index(&self, c: char) -> Option<GlyphId> {
        let cp = c as u32;
        if cp < 128 {
            let g = self.ascii[cp as usize];
            return if g != 0 { Some(GlyphId(g)) } else { None };
        }
        self.lookup(cp).map(GlyphId)
    }

    /// Returns `true` if the font has a glyph for `c`.
    #[inline]
    pub fn has_glyph(&self, c: char) -> bool {
        self.glyph_index(c).is_some()
    }

    /// The advance width of a glyph in font units (0 for invalid glyph ids).
    #[inline]
    pub fn advance_width(&self, glyph: GlyphId) -> u16 {
        if glyph.0 >= self.num_glyphs {
            return 0;
        }
        self.face.glyph_hor_advance(RawGlyphId(glyph.0)).unwrap_or(0)
    }

    /// The left side bearing of a glyph in font units.
    pub fn left_side_bearing(&self, glyph: GlyphId) -> i16 {
        if glyph.0 >= self.num_glyphs {
            return 0;
        }
        self.face.glyph_hor_side_bearing(RawGlyphId(glyph.0)).unwrap_or(0)
    }

    /// Returns `true` if the font has pair kerning (GPOS `kern` feature or `kern` table).
    pub fn has_kerning(&self) -> bool {
        self.kerning.is_some()
    }

    /// The kerning adjustment between two glyphs in font units (negative = closer). Cached.
    #[inline]
    pub fn kerning(&self, left: GlyphId, right: GlyphId) -> i16 {
        let Some(cache) = &self.kern_cache else { return 0 };
        let key = ((left.0 as u32) << 16) | right.0 as u32;
        if let Some(v) = cache.get(key) {
            return v;
        }
        let v = self.kerning.lookup(left.0, right.0);
        cache.put(key, v);
        v
    }

    /// Kerning without the cache (mainly for tests and diagnostics).
    pub fn kerning_uncached(&self, left: GlyphId, right: GlyphId) -> i16 {
        self.kerning.lookup(left.0, right.0)
    }

    /// Emits the outline of `glyph` in font units (y up) to `sink`. `ttf-parser` decodes it
    /// without allocating.
    ///
    /// Glyphs without contours (e.g. space) emit nothing. On error the sink may have received a
    /// partial outline and should be discarded.
    pub fn outline<S: OutlineSink + ?Sized>(&self, glyph: GlyphId, sink: &mut S) -> Result<(), FontError> {
        if glyph.0 >= self.num_glyphs {
            return Err(FontError::InvalidGlyph);
        }
        let id = RawGlyphId(glyph.0);
        let mut builder = Builder(sink, 0);
        if self.face.outline_glyph(id, &mut builder).is_some() {
            return Ok(());
        }
        // No outline: an empty glyph (a space), unless the glyph has data, which was malformed
        // or too deeply composed: a TrueType glyph's header box, or contours a CFF glyph began.
        let has_data = match self.face.tables().glyf {
            Some(glyf) => glyf.bbox(id).is_some(),
            None => builder.1 > 0,
        };
        if has_data { Err(FontError::MalformedGlyph) } else { Ok(()) }
    }

    /// The control box of a glyph's outline in font units (`None` for empty glyphs). For
    /// TrueType glyphs this equals the bounding box of all points.
    pub fn glyph_bounds(&self, glyph: GlyphId) -> Result<Option<Rect>, FontError> {
        let mut b = BoundsSink::default();
        self.outline(glyph, &mut b)?;
        Ok(b.bounds)
    }

    /// The bounding box stored in a TrueType glyph header (`[x_min, y_min, x_max, y_max]`);
    /// `None` for CFF fonts and empty glyphs.
    pub fn glyph_header_bbox(&self, glyph: GlyphId) -> Option<[i16; 4]> {
        let b = self.face.tables().glyf?.bbox(RawGlyphId(glyph.0))?;
        Some([b.x_min, b.y_min, b.x_max, b.y_max])
    }

    /// The outline of `glyph` as a path in pixels for a top-down raster: scaled to `size_px`
    /// (pixels per em), y flipped, with the glyph origin (pen position on the baseline) at
    /// `(0, 0)`.
    pub fn glyph_path(&self, glyph: GlyphId, size_px: f32) -> Result<Path, FontError> {
        let mut path = Path::new();
        self.append_glyph_path(glyph, size_px, Point::ZERO, &mut path)?;
        Ok(path)
    }

    /// Appends the outline of `glyph` scaled to `size_px` with its origin at `origin` (pixels,
    /// y down) to `path`.
    pub fn append_glyph_path(
        &self,
        glyph: GlyphId,
        size_px: f32,
        origin: Point,
        path: &mut Path,
    ) -> Result<(), FontError> {
        let scale = size_px * self.inv_upem;
        let mut sink = PathSink::new(path, scale, origin);
        self.outline(glyph, &mut sink)
    }

    /// Looks up a `name` table string by name id (see [`crate::name_id`]).
    pub fn name(&self, id: u16) -> Option<String> {
        name::lookup(&self.face, id)
    }

    /// The family name (typographic family if present, else the legacy family).
    pub fn family_name(&self) -> Option<String> {
        self.name(name_id::TYPOGRAPHIC_FAMILY).or_else(|| self.name(name_id::FAMILY))
    }

    /// The style name (typographic subfamily if present, else the legacy subfamily).
    pub fn subfamily_name(&self) -> Option<String> {
        self.name(name_id::TYPOGRAPHIC_SUBFAMILY).or_else(|| self.name(name_id::SUBFAMILY))
    }

    /// The full font name.
    pub fn full_name(&self) -> Option<String> {
        self.name(name_id::FULL_NAME)
    }

    /// The PostScript name.
    pub fn postscript_name(&self) -> Option<String> {
        self.name(name_id::POSTSCRIPT_NAME)
    }

    /// The version string.
    pub fn version(&self) -> Option<String> {
        self.name(name_id::VERSION)
    }

    /// The copyright notice (name id 0).
    pub fn copyright(&self) -> Option<String> {
        self.name(name_id::COPYRIGHT)
    }

    /// The license description (name id 13).
    pub fn license(&self) -> Option<String> {
        self.name(name_id::LICENSE)
    }

    /// The license URL (name id 14).
    pub fn license_url(&self) -> Option<String> {
        self.name(name_id::LICENSE_URL)
    }
}
