// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What AML's operators make of values: integers of buffers and strings,
//! the bits of buffer fields, elements of packages, buffers and strings,
//! comparisons, concatenations and the conversions to text; and what a
//! value costs to copy, and how deep it nests.
//!
//! Nothing here makes a value larger than [`MAX_BUFFER`] bytes: a result
//! that would be is an error ([`Error::Limit`]).

use alloc::borrow::Cow;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::cmp::Ordering;
use core::fmt::Write;

use super::{Error, MAX_BUFFER};
use crate::value::{Place, Value};

pub(super) fn int(v: &Value) -> Result<u64, Error> {
    v.as_integer().ok_or(Error::Type("an integer"))
}

/// The bytes a value holds beyond its own (its string's, its buffer's, its
/// elements' and theirs): what copying it costs.
pub(super) fn size(v: &Value) -> usize {
    match v {
        Value::String(s) => s.len(),
        Value::Buffer(b) => b.len(),
        Value::Package(items) => {
            items.iter().fold(items.len().saturating_mul(size_of::<Value>()), |n, i| n.saturating_add(size(i)))
        }
        Value::Reference(p) => p.0.len() * 4,
        Value::Element(place, _) => size_of::<Place>().saturating_add(match &**place {
            Place::Temporary(t) => size(t),
            Place::Named(p) => p.0.len() * 4,
            Place::Local(_) | Place::Arg(_) => 0,
        }),
        Value::Integer(_) | Value::Uninitialized => 0,
    }
}

/// Whether `v` nests no more than `limit` deep: a package is one deeper
/// than its deepest element, and an element of a value computed on the
/// spot one deeper than that value.
pub(super) fn within_depth(v: &Value, limit: u32) -> bool {
    match v {
        Value::Package(items) => limit > 0 && items.iter().all(|i| within_depth(i, limit - 1)),
        Value::Element(place, _) => match &**place {
            Place::Temporary(t) => limit > 0 && within_depth(t, limit - 1),
            Place::Local(_) | Place::Arg(_) | Place::Named(_) => true,
        },
        _ => true,
    }
}

/// The bytes a buffer field reads of (a buffer's, a string's, an
/// integer's).
pub(super) fn field_source(v: Option<&Value>) -> Result<Cow<'_, [u8]>, Error> {
    Ok(match v {
        Some(Value::Buffer(b)) => Cow::Borrowed(b),
        Some(Value::String(s)) => Cow::Borrowed(s.as_bytes()),
        Some(Value::Integer(i)) => Cow::Owned(i.to_le_bytes().to_vec()),
        Some(Value::Uninitialized) | None => Cow::Borrowed(&[]),
        Some(_) => return Err(Error::Type("a buffer")),
    })
}

/// Bits `[bit, bit + width)` of `bytes`: an integer up to 64 bits, else a
/// buffer.
pub(super) fn bits_of(bytes: &[u8], bit: u64, width: u64) -> Result<Value, Error> {
    let end = bit.checked_add(width).ok_or(Error::Type("a field inside its buffer"))?;
    if end > bytes.len() as u64 * 8 {
        return Err(Error::Type("a field inside its buffer"));
    }
    let get = |i: u64| bytes[((bit + i) / 8) as usize] >> ((bit + i) % 8) & 1;
    if width <= 64 {
        Ok(Value::Integer((0..width).fold(0u64, |v, i| v | (get(i) as u64) << i)))
    } else {
        let mut out = vec![0u8; width.div_ceil(8) as usize];
        for i in 0..width {
            out[(i / 8) as usize] |= get(i) << (i % 8);
        }
        Ok(Value::Buffer(out))
    }
}

/// Stores `value` into bits `[bit, bit + width)` of the buffer `into`.
pub(super) fn set_bits(into: &mut Value, bit: u64, width: u64, value: &Value) -> Result<(), Error> {
    let Value::Buffer(b) = into else { return Err(Error::Type("a buffer")) };
    let end = bit.checked_add(width).ok_or(Error::Type("a field inside its buffer"))?;
    if end > b.len() as u64 * 8 {
        return Err(Error::Type("a field inside its buffer"));
    }
    let source: Cow<[u8]> = match value {
        Value::Integer(v) => Cow::Owned(v.to_le_bytes().to_vec()),
        Value::Buffer(x) => Cow::Borrowed(x),
        Value::String(s) => Cow::Borrowed(s.as_bytes()),
        Value::Uninitialized => Cow::Borrowed(&[]),
        _ => return Err(Error::Type("data")),
    };
    for i in 0..width {
        let on = source.get((i / 8) as usize).is_some_and(|&s| s >> (i % 8) & 1 != 0);
        let (at, mask) = (((bit + i) / 8) as usize, 1u8 << ((bit + i) % 8));
        if on {
            b[at] |= mask;
        } else {
            b[at] &= !mask;
        }
    }
    Ok(())
}

/// Element `i` of a package, buffer or string (a byte of the latter two).
pub(super) fn element(container: &Value, i: usize) -> Result<Value, Error> {
    match container {
        Value::Package(p) => p.get(i).cloned().ok_or(Error::Type("an index inside the package")),
        Value::Buffer(b) => {
            b.get(i).map(|&x| Value::Integer(x as u64)).ok_or(Error::Type("an index inside the buffer"))
        }
        Value::String(s) => {
            s.as_bytes().get(i).map(|&x| Value::Integer(x as u64)).ok_or(Error::Type("an index inside the string"))
        }
        _ => Err(Error::Type("a package, buffer or string")),
    }
}

/// Stores `value` as element `i` of a package or buffer. A package's
/// element may nest `depth` deep at most.
pub(super) fn set_element(container: &mut Value, i: usize, value: Value, depth: u32) -> Result<(), Error> {
    match container {
        Value::Package(p) => {
            let slot = p.get_mut(i).ok_or(Error::Type("an index inside the package"))?;
            if !within_depth(&value, depth) {
                return Err(Error::Limit);
            }
            *slot = value;
            Ok(())
        }
        Value::Buffer(b) => {
            *b.get_mut(i).ok_or(Error::Type("an index inside the buffer"))? = int(&value)? as u8;
            Ok(())
        }
        _ => Err(Error::Type("a package or buffer")),
    }
}

/// A value stored to a named object takes the object's type.
pub(super) fn converted(current: &Value, value: Value) -> Value {
    match (current, &value) {
        (Value::Integer(_), Value::Buffer(_) | Value::String(_)) => value.as_integer().map_or(value, Value::Integer),
        (Value::Buffer(old), Value::Integer(_) | Value::String(_) | Value::Buffer(_)) => {
            let integer;
            let bytes: &[u8] = match &value {
                Value::Integer(v) => {
                    integer = v.to_le_bytes();
                    &integer
                }
                Value::String(s) => s.as_bytes(),
                Value::Buffer(b) => b,
                _ => &[],
            };
            let mut b = vec![0u8; old.len().max(1)];
            let n = bytes.len().min(b.len());
            b[..n].copy_from_slice(&bytes[..n]);
            Value::Buffer(b)
        }
        (Value::String(_), Value::Integer(v)) => Value::String(format!("{:X}", v)),
        _ => value,
    }
}

pub(super) fn compare(a: &Value, b: &Value) -> Result<Ordering, Error> {
    match a {
        Value::Integer(_) | Value::Uninitialized => Ok(int(a)?.cmp(&int(b)?)),
        Value::String(s) => {
            let y: Cow<[u8]> = match b {
                Value::Integer(v) => Cow::Owned(format!("{:X}", v).into_bytes()),
                other => bytes_of(other)?,
            };
            Ok(s.as_bytes().cmp(&y))
        }
        Value::Buffer(x) => Ok(x.as_slice().cmp(&bytes_of(b)?)),
        _ => Err(Error::Type("an integer, string or buffer")),
    }
}

/// The bytes of a buffer, string or integer, as comparisons and
/// concatenations take them.
fn bytes_of(v: &Value) -> Result<Cow<'_, [u8]>, Error> {
    Ok(match v {
        Value::Buffer(b) => Cow::Borrowed(b),
        Value::String(s) => Cow::Borrowed(s.as_bytes()),
        Value::Integer(i) => Cow::Owned(i.to_le_bytes().to_vec()),
        Value::Uninitialized => Cow::Borrowed(&[]),
        _ => return Err(Error::Type("data")),
    })
}

/// Fails if a result would have `len` bytes, more than any value may.
fn fits(len: usize) -> Result<(), Error> {
    if len > MAX_BUFFER { Err(Error::Limit) } else { Ok(()) }
}

pub(super) fn concatenate(a: &Value, b: &Value) -> Result<Value, Error> {
    Ok(match a {
        Value::Integer(x) => {
            let tail = match b {
                Value::Integer(y) => Cow::Owned(y.to_le_bytes().to_vec()),
                other => bytes_of(other)?,
            };
            fits(8 + tail.len())?;
            let mut v = x.to_le_bytes().to_vec();
            v.extend_from_slice(&tail);
            Value::Buffer(v)
        }
        Value::String(s) => {
            let tail = match b {
                Value::String(t) => Cow::Borrowed(t.as_str()),
                Value::Integer(v) => Cow::Owned(format!("{:X}", v)),
                other => Cow::Owned(to_hex_string(other)?),
            };
            fits(s.len().saturating_add(tail.len()))?;
            let mut v = String::with_capacity(s.len() + tail.len());
            v.push_str(s);
            v.push_str(&tail);
            Value::String(v)
        }
        Value::Buffer(x) => {
            let tail = bytes_of(b)?;
            fits(x.len().saturating_add(tail.len()))?;
            let mut v = Vec::with_capacity(x.len() + tail.len());
            v.extend_from_slice(x);
            v.extend_from_slice(&tail);
            Value::Buffer(v)
        }
        _ => return Err(Error::Type("an integer, string or buffer")),
    })
}

/// Joins two resource templates: the first without its end tag.
pub(super) fn concatenate_templates(a: &Value, b: &Value) -> Result<Value, Error> {
    let (Some(x), Some(y)) = (a.as_buffer(), b.as_buffer()) else {
        return Err(Error::Type("resource templates"));
    };
    let mut x = x;
    if x.len() >= 2 && x[x.len() - 2] == 0x79 {
        x = &x[..x.len() - 2];
    }
    let end: &[u8] = if y.is_empty() { &[0x79, 0] } else { &[] };
    fits(x.len().saturating_add(y.len()).saturating_add(end.len()))?;
    Ok(Value::Buffer([x, y, end].concat()))
}

pub(super) fn to_decimal_string(v: &Value) -> Result<String, Error> {
    Ok(match v {
        Value::Integer(x) => format!("{}", x),
        Value::String(s) => s.clone(),
        Value::Buffer(b) => {
            // Up to three digits and a comma each.
            fits(b.len().saturating_mul(4))?;
            let mut s = String::with_capacity(b.len() * 4);
            for (i, x) in b.iter().enumerate() {
                let _ = write!(s, "{}{}", if i > 0 { "," } else { "" }, x);
            }
            s
        }
        _ => return Err(Error::Type("data")),
    })
}

pub(super) fn to_hex_string(v: &Value) -> Result<String, Error> {
    Ok(match v {
        Value::Integer(x) => format!("{:X}", x),
        Value::String(s) => s.clone(),
        Value::Buffer(b) => {
            // `0xNN` and a comma each.
            fits(b.len().saturating_mul(5))?;
            let mut s = String::with_capacity(b.len() * 5);
            for (i, x) in b.iter().enumerate() {
                let _ = write!(s, "{}0x{:02X}", if i > 0 { "," } else { "" }, x);
            }
            s
        }
        _ => return Err(Error::Type("data")),
    })
}

pub(super) fn to_integer(v: &Value) -> Result<u64, Error> {
    match v {
        Value::String(s) => {
            let s = s.trim();
            match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
                Some(hex) => u64::from_str_radix(hex, 16).map_err(|_| Error::Type("a number")),
                None => s.parse::<u64>().map_err(|_| Error::Type("a number")),
            }
        }
        other => int(other),
    }
}

/// `ToString`: the characters of a buffer up to its first NUL, at most
/// `len` of them.
pub(super) fn to_string(v: &Value, len: u64) -> Result<String, Error> {
    let bytes = bytes_of(v).map_err(|_| Error::Type("a buffer"))?;
    let len = usize::try_from(len).unwrap_or(usize::MAX);
    let n = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len()).min(len);
    Ok(String::from_utf8_lossy(&bytes[..n]).into_owned())
}

/// `ToBuffer`.
pub(super) fn to_buffer(v: &Value) -> Result<Vec<u8>, Error> {
    Ok(bytes_of(v).map_err(|_| Error::Type("data"))?.into_owned())
}

/// `Mid`: `len` bytes of a string or buffer from `index` (as many as
/// there are).
pub(super) fn mid(v: &Value, index: u64, len: u64) -> Result<Value, Error> {
    let range = |n: usize| {
        let start = usize::try_from(index).unwrap_or(usize::MAX).min(n);
        let end = start.saturating_add(usize::try_from(len).unwrap_or(usize::MAX)).min(n);
        start..end
    };
    Ok(match v {
        Value::String(s) => Value::String(String::from_utf8_lossy(&s.as_bytes()[range(s.len())]).into_owned()),
        Value::Buffer(b) => Value::Buffer(b[range(b.len())].to_vec()),
        _ => return Err(Error::Type("a string or buffer")),
    })
}

pub(super) fn from_bcd(v: u64) -> u64 {
    // Sixteen digits of at most 15 each stay below 2^64.
    (0..16).rev().fold(0, |acc, i| acc * 10 + (v >> (i * 4) & 0xF))
}

pub(super) fn to_bcd(mut v: u64) -> u64 {
    let mut r = 0;
    for i in 0..16 {
        r |= (v % 10) << (i * 4);
        v /= 10;
    }
    r
}
