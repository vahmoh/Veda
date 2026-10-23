// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Pair kerning.
//!
//! Kerning values come from the GPOS table when it has PairPos lookups (lookup type 2, formats 1
//! and 2, possibly wrapped in extension lookups of type 9) referenced by the `kern` feature of the
//! preferred script (`latn`, then `DFLT`, then the first script; all `kern` features if that yields
//! nothing). Otherwise the legacy `kern` table (format 0 subtables, Microsoft and Apple headers) is
//! used, its subtables' values added up. Only the first glyph's x-advance adjustment is applied
//! (standard pair kerning).
//!
//! The tables are read by `ttf-parser`; the choice of lookups and values is Veda's. Lookups are
//! cached in a small direct-mapped table per font ([`KernCache`]).

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::cell::Cell;

use ttf_parser::gpos::{PairAdjustment, PositioningSubtable};
use ttf_parser::kern::{Format, Subtable};
use ttf_parser::opentype_layout::LayoutTable;
use ttf_parser::{Face, GlyphId, Tag};

/// Upper bound on the number of PairPos subtables considered.
const MAX_SUBTABLES: usize = 4096;

/// Kerning data of a font.
#[derive(Clone)]
pub(crate) enum Kerning<'a> {
    /// No kerning.
    None,
    /// GPOS PairPos subtables grouped by lookup.
    Gpos {
        /// The subtables, in lookup order.
        subtables: Vec<PairAdjustment<'a>>,
        /// Exclusive end index into `subtables` of every lookup.
        lookup_ends: Vec<u32>,
    },
    /// Horizontal format 0 subtables of the `kern` table.
    Kern(Vec<Subtable<'a>>),
}

impl<'a> Kerning<'a> {
    /// Builds the kerning data from the font's GPOS or `kern` table.
    pub(crate) fn new(face: &Face<'a>) -> Kerning<'a> {
        let tables = face.tables();
        gpos(tables.gpos).or_else(|| kern(tables.kern)).unwrap_or(Kerning::None)
    }

    /// Returns `true` if the font has any kerning data.
    pub(crate) fn is_some(&self) -> bool {
        !matches!(self, Kerning::None)
    }

    /// The kerning adjustment (font units) between `left` and `right`.
    pub(crate) fn lookup(&self, left: u16, right: u16) -> i16 {
        let (left, right) = (GlyphId(left), GlyphId(right));
        let total: i32 = match self {
            Kerning::None => 0,
            Kerning::Gpos { subtables, lookup_ends } => {
                let mut total = 0i32;
                let mut start = 0usize;
                for &end in lookup_ends {
                    let end = end as usize;
                    // The first subtable of a lookup that has the pair applies.
                    if let Some(v) = subtables[start..end].iter().find_map(|st| pair_value(st, left, right)) {
                        total += v as i32;
                    }
                    start = end;
                }
                total
            }
            Kerning::Kern(subtables) => {
                subtables.iter().filter_map(|st| st.glyphs_kerning(left, right)).map(i32::from).sum()
            }
        };
        total.clamp(i16::MIN as i32, i16::MAX as i32) as i16
    }
}

/// Feature indices of the preferred script's default language system (or its first).
fn preferred_features(table: &LayoutTable<'_>) -> Option<Vec<u16>> {
    let scripts = &table.scripts;
    let script = scripts
        .find(Tag::from_bytes(b"latn"))
        .or_else(|| scripts.find(Tag::from_bytes(b"DFLT")))
        .or_else(|| scripts.get(0))?;
    let lang = script.default_language.or_else(|| script.languages.get(0))?;
    Some(lang.required_feature.into_iter().chain(lang.feature_indices).collect())
}

/// Appends the lookup indices of feature `index` to `out` if it is a `kern` feature.
fn kern_feature_lookups(table: &LayoutTable<'_>, index: u16, out: &mut Vec<u16>) {
    if let Some(feature) = table.features.get(index)
        && feature.tag == Tag::from_bytes(b"kern")
    {
        out.extend(feature.lookup_indices);
    }
}

fn gpos(table: Option<LayoutTable<'_>>) -> Option<Kerning<'_>> {
    let table = table?;
    let mut lookups = Vec::new();
    for index in preferred_features(&table).unwrap_or_default() {
        kern_feature_lookups(&table, index, &mut lookups);
    }
    if lookups.is_empty() {
        for index in 0..table.features.len() {
            kern_feature_lookups(&table, index, &mut lookups);
        }
    }
    lookups.sort_unstable();
    lookups.dedup();
    let mut subtables = Vec::new();
    let mut lookup_ends = Vec::new();
    for index in lookups {
        let Some(lookup) = table.lookups.get(index) else { continue };
        let before = subtables.len();
        for i in 0..lookup.subtables.len() {
            if subtables.len() >= MAX_SUBTABLES {
                break;
            }
            if let Some(PositioningSubtable::Pair(pair)) = lookup.subtables.get::<PositioningSubtable>(i) {
                subtables.push(pair);
            }
        }
        if subtables.len() > before {
            lookup_ends.push(subtables.len() as u32);
        }
    }
    if subtables.is_empty() {
        return None;
    }
    Some(Kerning::Gpos { subtables, lookup_ends })
}

fn kern(table: Option<ttf_parser::kern::Table<'_>>) -> Option<Kerning<'_>> {
    let subtables: Vec<Subtable<'_>> = table?
        .subtables
        .into_iter()
        .take(64)
        .filter(|st| st.horizontal && !st.variable && !st.has_cross_stream && !st.has_state_machine)
        .filter(|st| matches!(st.format, Format::Format0(_)))
        .collect();
    if subtables.is_empty() { None } else { Some(Kerning::Kern(subtables)) }
}

/// The x-advance of the first glyph if the PairPos subtable applies to the pair.
fn pair_value(subtable: &PairAdjustment<'_>, left: GlyphId, right: GlyphId) -> Option<i16> {
    let (first, _) = match subtable {
        PairAdjustment::Format1 { coverage, sets } => sets.get(coverage.get(left)?)?.get(right)?,
        PairAdjustment::Format2 { coverage, classes, matrix } => {
            coverage.get(left)?;
            matrix.get((classes.0.get(left), classes.1.get(right)))?
        }
    };
    Some(first.x_advance)
}

/// Number of entries of the per-font kerning cache (power of two).
const CACHE_SLOTS: usize = 1024;

/// A direct-mapped cache of kerning values keyed by glyph pair. Uses interior mutability so that
/// lookups work through shared font references (fonts are used single-threaded).
#[derive(Clone)]
pub(crate) struct KernCache {
    slots: Box<[Cell<u64>]>,
}

impl KernCache {
    pub(crate) fn new() -> Self {
        KernCache { slots: (0..CACHE_SLOTS).map(|_| Cell::new(0)).collect() }
    }

    #[inline]
    fn slot(key: u32) -> usize {
        (key.wrapping_mul(0x9E37_79B1) >> (32 - CACHE_SLOTS.trailing_zeros())) as usize
    }

    /// Entry layout: key in the high 32 bits, value in bits 16..32, bit 0 = valid.
    #[inline]
    pub(crate) fn get(&self, key: u32) -> Option<i16> {
        let e = self.slots[Self::slot(key)].get();
        if e & 1 != 0 && (e >> 32) as u32 == key { Some((e >> 16) as u16 as i16) } else { None }
    }

    #[inline]
    pub(crate) fn put(&self, key: u32, v: i16) {
        self.slots[Self::slot(key)].set(((key as u64) << 32) | ((v as u16 as u64) << 16) | 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::ALL;

    #[test]
    fn kern_table_format0() {
        // Microsoft kern table with one horizontal format 0 subtable: (1,2) = -50, (3,4) = 20.
        let mut d = Vec::new();
        for v in [0u16, 1, 0, 14 + 12, 0x0001, 2, 12, 1, 0] {
            d.extend_from_slice(&v.to_be_bytes());
        }
        for (l, r, v) in [(1u16, 2u16, -50i16), (3, 4, 20)] {
            d.extend_from_slice(&l.to_be_bytes());
            d.extend_from_slice(&r.to_be_bytes());
            d.extend_from_slice(&v.to_be_bytes());
        }
        let k = kern(ttf_parser::kern::Table::parse(&d)).unwrap();
        assert!(k.is_some());
        assert_eq!(k.lookup(1, 2), -50);
        assert_eq!(k.lookup(3, 4), 20);
        assert_eq!(k.lookup(2, 1), 0);
        // Truncated tables never panic.
        for n in 0..d.len() {
            if let Some(k) = kern(ttf_parser::kern::Table::parse(&d[..n])) {
                let _ = k.lookup(1, 2);
            }
        }
    }

    #[test]
    fn cache_roundtrip() {
        let c = KernCache::new();
        assert_eq!(c.get(0x0001_0002), None);
        c.put(0x0001_0002, -123);
        assert_eq!(c.get(0x0001_0002), Some(-123));
        c.put(0x0001_0002, 0);
        assert_eq!(c.get(0x0001_0002), Some(0));
    }

    /// Reports the lookups and PairPos formats of the bundled fonts' kerning.
    #[test]
    fn bundled_gpos_structure() {
        let u16_at = |d: &[u8], o: usize| u16::from_be_bytes([d[o], d[o + 1]]);
        for (name, data) in ALL {
            let face = Face::parse(data, 0).unwrap();
            let Kerning::Gpos { subtables, lookup_ends } = Kerning::new(&face) else {
                std::println!("{name}: no kern lookups");
                continue;
            };
            let f1 = subtables.iter().filter(|st| matches!(st, PairAdjustment::Format1 { .. })).count();
            let f2 = subtables.len() - f1;
            // The lookup types of the kern feature's lookups, from the table's bytes.
            let table = face.tables().gpos.unwrap();
            let mut lookups = Vec::new();
            for index in preferred_features(&table).unwrap() {
                kern_feature_lookups(&table, index, &mut lookups);
            }
            let gpos = face.raw_face().table(Tag::from_bytes(b"GPOS")).unwrap();
            let list = u16_at(gpos, 8) as usize;
            let types: Vec<u16> = lookups
                .iter()
                .map(|&i| u16_at(gpos, list + u16_at(gpos, list + 2 + i as usize * 2) as usize))
                .collect();
            std::println!(
                "{name}: {} kern lookups (types {types:?}), {} subtables (fmt1 {f1}, fmt2 {f2})",
                lookup_ends.len(),
                subtables.len()
            );
            assert!(f1 > 0 && f2 > 0, "{name}: both PairPos formats are used");
            if name.starts_with("Lato") {
                assert_eq!(types, [9], "Lato wraps its kern lookup in an extension lookup");
            }
        }
    }
}
