// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Reading AML: a cursor over a table's code (package lengths, names,
//! strings), the frame of an `If` and its `Else`, which loading and
//! evaluating share, and the search for a name through the scopes around
//! it.

use alloc::string::String;

use super::Error;
use crate::name::{NameSeg, NameString, Path};

/// A position in a table's AML.
pub(super) struct Cursor<'a> {
    pub code: &'a [u8],
    pub pos: usize,
}

impl<'a> Cursor<'a> {
    pub fn peek(&self) -> Result<u8, Error> {
        self.code.get(self.pos).copied().ok_or(Error::Truncated(self.pos))
    }

    pub fn peek_at(&self, ahead: usize) -> Option<u8> {
        self.code.get(self.pos.checked_add(ahead)?).copied()
    }

    pub fn byte(&mut self) -> Result<u8, Error> {
        let b = self.peek()?;
        self.pos += 1;
        Ok(b)
    }

    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let end = self.pos.checked_add(n).filter(|&e| e <= self.code.len()).ok_or(Error::Truncated(self.pos))?;
        let s = &self.code[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    pub fn word(&mut self) -> Result<u16, Error> {
        let b = self.bytes(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    pub fn dword(&mut self) -> Result<u32, Error> {
        let b = self.bytes(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn qword(&mut self) -> Result<u64, Error> {
        Ok(self.dword()? as u64 | (self.dword()? as u64) << 32)
    }

    /// A PkgLength's value.
    pub fn pkg_length(&mut self) -> Result<usize, Error> {
        let lead = self.byte()?;
        let follow = (lead >> 6) as usize;
        if follow == 0 {
            return Ok((lead & 0x3F) as usize);
        }
        let mut len = (lead & 0x0F) as usize;
        for i in 0..follow {
            len |= (self.byte()? as usize) << (4 + 8 * i);
        }
        Ok(len)
    }

    /// A PkgLength, as the offset where the package ends (inside the code
    /// the cursor reads).
    pub fn pkg_end(&mut self) -> Result<usize, Error> {
        let start = self.pos;
        let end = start.checked_add(self.pkg_length()?).ok_or(Error::Truncated(start))?;
        if end > self.code.len() || end < self.pos {
            return Err(Error::Truncated(start));
        }
        Ok(end)
    }

    pub fn name_seg(&mut self) -> Result<NameSeg, Error> {
        let at = self.pos;
        let b = self.bytes(4)?;
        let seg = [b[0], b[1], b[2], b[3]];
        if !crate::name::is_seg(&seg) {
            return Err(Error::BadName(at));
        }
        Ok(seg)
    }

    pub fn name_string(&mut self) -> Result<NameString, Error> {
        let mut n = NameString::default();
        if self.peek()? == b'\\' {
            n.absolute = true;
            self.pos += 1;
        } else {
            while self.peek()? == b'^' {
                n.parents += 1;
                self.pos += 1;
            }
        }
        match self.peek()? {
            0x00 => self.pos += 1,
            0x2E => {
                self.pos += 1;
                n.segs.push(self.name_seg()?);
                n.segs.push(self.name_seg()?);
            }
            0x2F => {
                self.pos += 1;
                for _ in 0..self.byte()? {
                    n.segs.push(self.name_seg()?);
                }
            }
            _ => n.segs.push(self.name_seg()?),
        }
        Ok(n)
    }

    /// A string's characters, up to and including its terminating NUL.
    pub fn string(&mut self) -> Result<String, Error> {
        let at = self.pos;
        let len = self.skip_string()?;
        Ok(String::from_utf8_lossy(&self.code[at..at + len]).into_owned())
    }

    /// Passes a string, its NUL included: how many characters it has.
    pub fn skip_string(&mut self) -> Result<usize, Error> {
        let at = self.pos;
        let rest = self.code.get(at..).unwrap_or_default();
        let len = rest.iter().position(|&b| b == 0).ok_or(Error::Truncated(at))?;
        self.pos = at + len + 1;
        Ok(len)
    }
}

/// Whether `b` starts a name.
pub(super) fn is_name_start(b: u8) -> bool {
    matches!(b, b'\\' | b'^' | 0x2E | 0x2F | b'A'..=b'Z' | b'_')
}

/// Where the parts of an `If` are, and of the `Else` that goes with it.
pub(super) struct IfElse {
    /// Where the `If`'s body ends (its predicate, then its body, start at
    /// the cursor).
    pub then_end: usize,
    /// The `Else`'s body, if the `If` has one.
    pub otherwise: Option<(usize, usize)>,
    /// Where the whole ends.
    pub end: usize,
}

/// Frames the `If` whose opcode `c` has just read, in a list of terms that
/// ends at `list_end` (a method's body, a scope's, another `If`'s): the
/// cursor stays at its predicate. An `Else` right after the `If`'s body is
/// its own only inside the list: an `If` that is the last term of the body
/// of an `If` around it ends where that body does, and the `Else` that
/// follows is the outer one's.
pub(super) fn if_else(c: &mut Cursor, list_end: usize) -> Result<IfElse, Error> {
    let at = c.pos;
    let then_end = c.pkg_end()?;
    let code = c.code.get(..list_end).ok_or(Error::Truncated(at))?;
    if then_end > list_end {
        return Err(Error::Truncated(at));
    }
    let mut frame = IfElse { then_end, otherwise: None, end: then_end };
    if code.get(then_end) == Some(&0xA1) {
        let mut e = Cursor { code, pos: then_end + 1 };
        let else_end = e.pkg_end()?;
        frame.otherwise = Some((e.pos, else_end));
        frame.end = else_end;
    }
    Ok(frame)
}

/// Searches for a name from `scope` as AML does: a single segment without
/// a prefix in the scope and then in each enclosing one; anything else
/// exactly where it points.
pub(super) fn search(scope: &Path, name: &NameString, exists: impl Fn(&Path) -> bool) -> Option<Path> {
    if !name.searches() {
        let p = name.in_scope(scope);
        return exists(&p).then_some(p);
    }
    let seg = name.segs[0];
    let mut s = scope.clone();
    loop {
        let p = s.child(seg);
        if exists(&p) {
            return Some(p);
        }
        s.0.pop()?;
    }
}
