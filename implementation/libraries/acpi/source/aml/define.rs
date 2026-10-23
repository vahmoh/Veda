// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The definitions of named objects that loading and evaluating both meet:
//! operation regions and their fields (`Field`, `IndexField`,
//! `BankField`), data table regions, buffer fields (`CreateDWordField`,
//! `CreateField`...), mutexes and events. Parsing one gives the objects it
//! defines; what its operands are evaluated with is the caller's
//! ([`Operands`]): in a method, there and then; at load, as far as they
//! can be yet, the rest when the object is used.

use alloc::vec;
use alloc::vec::Vec;

use super::code::Cursor;
use super::{Bits, Bounds, Code, Error, Field, FieldKind, FieldUnit, Object};
use crate::name::{NameString, Path};
use crate::value::Place;

/// What a definition's integer operands came to.
pub(super) enum Integers {
    Known([u64; 2]),
    /// Kept to be evaluated when the object is used: at load, they may
    /// refer to what a later table defines.
    Deferred(Code),
    /// Evaluated, but not to integers.
    Unknown,
}

/// What a definition's operands are evaluated with.
pub(super) trait Operands {
    /// The scope the definition is in.
    fn scope(&self) -> &Path;

    /// Where a name a definition refers to points (a field's region): the
    /// object it names, else where it would be in the scope.
    fn resolve(&self, name: &NameString) -> Path;

    /// The definition's `count` (1 or 2) integer operands at `c`.
    fn integers(&mut self, c: &mut Cursor, count: usize) -> Result<Integers, Error>;

    /// A buffer field's buffer: a named one, whose stores the field's reach,
    /// or a value. `None` if it cannot be had, and the field is not defined.
    fn buffer(&mut self, c: &mut Cursor) -> Result<Option<Place>, Error>;

    /// Passes an operand without evaluating it (a bank field's bank value).
    fn skip(&mut self, c: &mut Cursor) -> Result<(), Error>;
}

/// Parses the definition at `c` if it is one of those loading and
/// evaluating both handle: the objects it defines, with their paths.
/// `None`, and `c` where it was, if it is not one.
pub(super) fn definition(c: &mut Cursor, ops: &mut impl Operands) -> Result<Option<Vec<(Path, Object)>>, Error> {
    let at = c.pos;
    let op = match c.peek()? {
        0x5B => 0x5B00 | c.peek_at(1).ok_or(Error::Truncated(at))? as u16,
        b => b as u16,
    };
    let kind = match op {
        0x8D => FieldKind::Bit,
        0x8C => FieldKind::Byte,
        0x8B => FieldKind::Word,
        0x8A => FieldKind::DWord,
        0x8F => FieldKind::QWord,
        0x5B13 => FieldKind::Bits,
        0x5B80 => return region(c, ops).map(Some),
        0x5B81 | 0x5B86 | 0x5B87 => return fields(c, op, ops).map(Some),
        // Mutex: a name and its sync level.
        0x5B01 => {
            c.pos += 2;
            let name = c.name_string()?;
            c.byte()?;
            return Ok(Some(vec![(name.in_scope(ops.scope()), Object::Mutex)]));
        }
        // Event
        0x5B02 => {
            c.pos += 2;
            let name = c.name_string()?;
            return Ok(Some(vec![(name.in_scope(ops.scope()), Object::Event)]));
        }
        // DataTableRegion: a region over a table, not read.
        0x5B88 => {
            c.pos += 2;
            let name = c.name_string()?;
            for _ in 0..3 {
                ops.skip(c)?;
            }
            let region = Object::Region { space: 0xFE, bounds: Bounds::Unknown };
            return Ok(Some(vec![(name.in_scope(ops.scope()), region)]));
        }
        _ => return Ok(None),
    };
    c.pos += if op > 0xFF { 2 } else { 1 };
    let place = ops.buffer(c)?;
    let integers = ops.integers(c, kind.operands())?;
    let name = c.name_string()?;
    let bits = match integers {
        Integers::Known(operands) => {
            let (offset, width) = kind.bits(operands);
            Bits::Known { offset, width }
        }
        Integers::Deferred(code) => Bits::Deferred { scope: ops.scope().clone(), code, kind },
        Integers::Unknown => return Err(Error::Type("an integer")),
    };
    Ok(Some(match place {
        Some(place) => vec![(name.in_scope(ops.scope()), Object::BufferField { place, bits })],
        None => Vec::new(),
    }))
}

/// `OperationRegion (name, space, offset, length)`.
fn region(c: &mut Cursor, ops: &mut impl Operands) -> Result<Vec<(Path, Object)>, Error> {
    c.pos += 2;
    let name = c.name_string()?;
    let space = c.byte()?;
    let bounds = match ops.integers(c, 2)? {
        Integers::Known([offset, length]) => Bounds::Known { offset, length },
        Integers::Deferred(code) => Bounds::Deferred { scope: ops.scope().clone(), code },
        Integers::Unknown => Bounds::Unknown,
    };
    Ok(vec![(name.in_scope(ops.scope()), Object::Region { space, bounds })])
}

/// `Field`, `IndexField` and `BankField`: their field lists.
fn fields(c: &mut Cursor, op: u16, ops: &mut impl Operands) -> Result<Vec<(Path, Object)>, Error> {
    c.pos += 2;
    let list_end = c.pkg_end()?;
    // Nothing of the definition is read past its own end.
    let mut l = Cursor { code: &c.code[..list_end], pos: c.pos };
    let first = l.name_string()?;
    let unit = match op {
        0x5B81 => FieldUnit::Region(ops.resolve(&first)),
        0x5B86 => {
            l.name_string()?;
            FieldUnit::Index
        }
        _ => {
            l.name_string()?;
            ops.skip(&mut l)?;
            FieldUnit::Bank
        }
    };
    let flags = l.byte()?;
    let mut defined = Vec::new();
    field_list(&mut l, flags, &unit, ops.scope(), &mut defined)?;
    c.pos = list_end;
    Ok(defined)
}

/// Parses a field list up to the end of `c`'s code, adding each named
/// field to `defined`.
fn field_list(
    c: &mut Cursor,
    flags: u8,
    unit: &FieldUnit,
    scope: &Path,
    defined: &mut Vec<(Path, Object)>,
) -> Result<(), Error> {
    // The access type's width: ByteAcc's, and AnyAcc's and BufferAcc's as
    // ACPICA makes them (bytes); WordAcc, DWordAcc and QWordAcc.
    let access = |kind: u8| match kind & 0xF {
        2 => 16,
        3 => 32,
        4 => 64,
        _ => 8,
    };
    let mut access_bits = access(flags);
    let mut bit = 0u64;
    while c.pos < c.code.len() {
        match c.peek()? {
            // Reserved bits
            0x00 => {
                c.pos += 1;
                bit = bit.saturating_add(c.pkg_length()? as u64);
            }
            // AccessField, ExtendedAccessField
            0x01 => {
                c.pos += 1;
                access_bits = access(c.byte()?);
                c.byte()?;
            }
            0x03 => {
                c.pos += 1;
                access_bits = access(c.byte()?);
                c.bytes(2)?;
            }
            // ConnectField: a name or a buffer (a GPIO or serial bus connection).
            0x02 => {
                c.pos += 1;
                if c.peek()? == 0x11 {
                    c.pos += 1;
                    c.pos = c.pkg_end()?;
                } else {
                    c.name_string()?;
                }
            }
            _ => {
                let seg = c.name_seg()?;
                let width = c.pkg_length()? as u64;
                let field = Field { unit: unit.clone(), bit_offset: bit, bit_width: width, access_bits };
                defined.push((scope.child(seg), Object::Field(field)));
                bit = bit.saturating_add(width);
            }
        }
    }
    Ok(())
}
