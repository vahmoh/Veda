// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Loading a definition block into the namespace: the objects its terms
//! define, outside methods.
//!
//! Scopes, devices, processors, power resources and thermal zones open
//! scopes of their own; methods are kept as code; a name's data is
//! evaluated (left empty if it cannot be, and evaluated again once a later
//! table has loaded if it refers to objects not defined yet); regions,
//! fields, buffer fields, mutexes and events are `define`'s. The operands
//! of a region or a buffer field that cannot be evaluated yet are kept,
//! to be evaluated when the object is used. An `If` defines what its body
//! does when its condition holds, else what its `Else` does; a condition
//! that cannot be evaluated drops both, counted. Other code is parsed and
//! passed over.
//!
//! A load is bounded as a whole: in steps and bytes, as each evaluation it
//! makes is, and in how deep its definitions nest.

use alloc::sync::Arc;
use alloc::vec::Vec;

use super::code::{Cursor, if_else};
use super::convert::int;
use super::define::{self, Integers, Operands};
use super::eval::{Budget, Evaluator, Frame};
use super::{Code, Error, LOAD_BYTES, LOAD_STEPS, MAX_NESTING, Namespace, Object};
use crate::Memory;
use crate::name::{NameString, Path};
use crate::value::{Place, Value};

/// A name's data, to be evaluated again.
pub(super) struct Unresolved {
    path: Path,
    scope: Path,
    table: usize,
    start: usize,
}

/// A table being loaded: its code, the machine its conditions read, and
/// what loading it may still spend.
struct Load<'a> {
    table: usize,
    code: Arc<[u8]>,
    memory: &'a dyn Memory,
    budget: Budget<'static>,
}

impl Load<'_> {
    /// An evaluation for the load, of code `depth` definitions deep: what
    /// it nests counts on from there.
    fn evaluator<'e>(&'e self, ns: &'e Namespace, depth: u32) -> Evaluator<'e> {
        Evaluator::new(ns, self.memory, &self.budget).nested(depth)
    }
}

/// A list of terms a term holds, to be loaded: a scope's or a device's
/// body, in its own scope, or an `If`'s or `Else`'s, in the scope around.
struct Nested {
    start: usize,
    end: usize,
    scope: Option<Path>,
}

impl Namespace {
    /// Loads a definition block's AML (a table without its header). What
    /// it defined before an error stays defined.
    pub fn load(&mut self, aml: &[u8], memory: &dyn Memory) -> Result<(), Error> {
        let table = self.code.len();
        let code: Arc<[u8]> = Arc::from(aml);
        self.code.push(code.clone());
        let load = Load { table, code, memory, budget: Budget::new(LOAD_STEPS, LOAD_BYTES, None) };
        let loaded = self.load_list(&load, 0, load.code.len(), &Path::root(), 0);
        self.resolve_again(&load);
        loaded
    }

    /// Evaluates again the data of the names that referred to objects not
    /// defined yet; those that still do wait for the next table.
    fn resolve_again(&mut self, load: &Load) {
        for u in core::mem::take(&mut self.unresolved) {
            let value = {
                let code = self.code[u.table].clone();
                let mut c = Cursor { code: &code, pos: u.start };
                let mut f = Frame::new(u.scope.clone(), Vec::new());
                load.evaluator(self, 0).term(&mut f, &mut c, true)
            };
            match value {
                Ok(v) if !self.dangles(&v) => {
                    self.objects.insert(u.path, Object::Name(v));
                }
                _ => self.unresolved.push(u),
            }
        }
    }

    /// Whether `value` refers to an object that does not exist.
    fn dangles(&self, value: &Value) -> bool {
        match value {
            Value::Reference(p) => !self.objects.contains_key(p),
            Value::Package(items) => items.iter().any(|v| self.dangles(v)),
            _ => false,
        }
    }

    /// Counts a definition left out, and why.
    fn skip(&mut self, scope: &Path, why: Error) {
        self.skipped += 1;
        if self.skip_reasons.len() < 16 {
            self.skip_reasons.push((scope.clone(), why));
        }
    }

    /// Loads the terms between `start` and `end`, in `scope`, `depth`
    /// definitions deep. Each term is loaded by a function of its own, out
    /// of the frames the recursion keeps: what is nested deeper takes as
    /// little stack as can be.
    fn load_list(&mut self, l: &Load, start: usize, end: usize, scope: &Path, depth: u32) -> Result<(), Error> {
        if depth >= MAX_NESTING {
            return Err(Error::Limit);
        }
        let mut c = Cursor { code: &l.code[..end], pos: start };
        while c.pos < end {
            if let Some(nested) = self.load_term(l, &mut c, end, scope, depth)? {
                let scope = nested.scope.as_ref().unwrap_or(scope);
                self.load_list(l, nested.start, nested.end, scope, depth + 1)?;
            }
        }
        Ok(())
    }

    /// Loads the term at `c`, in a list that ends at `end`, and passes it:
    /// the list it holds, if it holds one to load.
    #[inline(never)]
    fn load_term(
        &mut self,
        l: &Load,
        c: &mut Cursor,
        end: usize,
        scope: &Path,
        depth: u32,
    ) -> Result<Option<Nested>, Error> {
        let at = c.pos;
        match c.byte()? {
            // Scope: the name refers to an existing object.
            0x10 => {
                let body = c.pkg_end()?;
                let name = c.name_string()?;
                let path = self.resolve(scope, &name).unwrap_or_else(|| name.in_scope(scope));
                self.objects.entry(path.clone()).or_insert(Object::Scope);
                let nested = Nested { start: c.pos, end: body, scope: Some(path) };
                c.pos = body;
                return Ok(Some(nested));
            }
            // Method
            0x14 => {
                let body = c.pkg_end()?;
                let name = c.name_string()?;
                let flags = c.byte()?;
                let code = Code { table: l.table, start: c.pos, end: body };
                self.define(name.in_scope(scope), Object::Method { args: flags & 7, code });
                c.pos = body;
            }
            // Name (a value that cannot be evaluated is left empty)
            0x08 => {
                let name = c.name_string()?;
                let at = c.pos;
                let mut f = Frame::new(scope.clone(), Vec::new());
                let value = match l.evaluator(self, depth).term(&mut f, c, true) {
                    Ok(v) => v,
                    Err(_) => {
                        c.pos = at;
                        l.evaluator(self, depth).term(&mut f, c, false)?;
                        Value::Uninitialized
                    }
                };
                let path = name.in_scope(scope);
                if self.dangles(&value) && !self.objects.contains_key(&path) {
                    let unresolved = Unresolved { path: path.clone(), scope: scope.clone(), table: l.table, start: at };
                    self.unresolved.push(unresolved);
                }
                self.define(path, Object::Name(value));
            }
            // Alias
            0x06 => {
                let source = c.name_string()?;
                let alias = c.name_string()?;
                let target = self.resolve(scope, &source).unwrap_or_else(|| source.in_scope(scope));
                self.define(alias.in_scope(scope), Object::Alias(target));
            }
            // External: only methods matter (their argument counts).
            0x15 => {
                let name = c.name_string()?;
                let kind = c.byte()?;
                let args = c.byte()?;
                if kind == 8 {
                    self.externals.insert(name.in_scope(scope), args & 7);
                }
            }
            // If, with an optional Else: the body of whichever its condition
            // says.
            0xA0 => {
                let frame = if_else(c, end)?;
                let condition = {
                    let mut f = Frame::new(scope.clone(), Vec::new());
                    l.evaluator(self, depth).term(&mut f, c, true).and_then(|v| int(&v))
                };
                // A predicate past the body is malformed code: dropped too.
                let condition =
                    condition.and_then(|v| if c.pos > frame.then_end { Err(Error::Truncated(c.pos)) } else { Ok(v) });
                let nested = match condition {
                    Ok(v) if v != 0 => Some(Nested { start: c.pos, end: frame.then_end, scope: None }),
                    Ok(_) => frame.otherwise.map(|(start, end)| Nested { start, end, scope: None }),
                    Err(e) => {
                        self.skip(scope, e);
                        None
                    }
                };
                c.pos = frame.end;
                return Ok(nested);
            }
            // An Else without its If.
            0xA1 => c.pos = c.pkg_end()?,
            // Device, Processor, PowerResource, ThermalZone
            0x5B if matches!(c.peek_at(0), Some(0x82..=0x85)) => {
                let op = c.byte()?;
                let body = c.pkg_end()?;
                let name = c.name_string()?;
                let object = match op {
                    0x82 => Object::Device,
                    0x83 => {
                        c.bytes(6)?; // processor id, PBlk address and length
                        Object::Processor
                    }
                    0x84 => {
                        c.bytes(3)?; // system level, resource order
                        Object::PowerResource
                    }
                    _ => Object::ThermalZone,
                };
                let path = name.in_scope(scope);
                self.define(path.clone(), object);
                let nested = Nested { start: c.pos, end: body, scope: Some(path) };
                c.pos = body;
                return Ok(Some(nested));
            }
            _ => {
                c.pos = at;
                if !self.load_definition(l, c, scope, depth)? {
                    self.skip_code(l, scope, c, depth)?;
                }
            }
        }
        Ok(None)
    }

    /// Loads the definition at `c` if it is one of `define`'s (a region,
    /// fields, a buffer field...): false if it is not.
    fn load_definition(&mut self, l: &Load, c: &mut Cursor, scope: &Path, depth: u32) -> Result<bool, Error> {
        let mut ops = AtLoad { ns: self, load: l, scope, depth, left_out: None };
        let defined = define::definition(c, &mut ops)?;
        let left_out = ops.left_out.take();
        let Some(objects) = defined else { return Ok(false) };
        for (path, object) in objects {
            self.define(path, object);
        }
        if let Some(why) = left_out {
            self.skip(scope, why);
        }
        Ok(true)
    }

    /// Skips one term of code outside a method, without running it.
    fn skip_code(&self, l: &Load, scope: &Path, c: &mut Cursor, depth: u32) -> Result<(), Error> {
        let mut f = Frame::new(scope.clone(), Vec::new());
        match c.peek()? {
            // While, Return, Break, Continue, Noop, Breakpoint
            0xA2 => {
                c.pos += 1;
                c.pos = c.pkg_end()?;
            }
            0xA4 => {
                c.pos += 1;
                l.evaluator(self, depth).term(&mut f, c, false)?;
            }
            0xA5 | 0x9F | 0xA3 | 0xCC => c.pos += 1,
            _ => {
                l.evaluator(self, depth).term(&mut f, c, false)?;
            }
        }
        Ok(())
    }
}

/// A definition's operands at load: evaluated if they can be yet, else
/// kept for when the object is used.
struct AtLoad<'a> {
    ns: &'a Namespace,
    load: &'a Load<'a>,
    scope: &'a Path,
    depth: u32,
    /// Why the definition is not made, if it is not (a buffer field whose
    /// buffer cannot be had).
    left_out: Option<Error>,
}

impl AtLoad<'_> {
    fn frame(&self) -> Frame {
        Frame::new(self.scope.clone(), Vec::new())
    }
}

impl Operands for AtLoad<'_> {
    fn scope(&self) -> &Path {
        self.scope
    }

    fn resolve(&self, name: &NameString) -> Path {
        self.ns.resolve(self.scope, name).unwrap_or_else(|| name.in_scope(self.scope))
    }

    fn integers(&mut self, c: &mut Cursor, count: usize) -> Result<Integers, Error> {
        let start = c.pos;
        let mut f = self.frame();
        let mut ev = self.load.evaluator(self.ns, self.depth);
        let mut operands = [0; 2];
        let mut known = true;
        for slot in operands.iter_mut().take(count) {
            match ev.term(&mut f, c, true).map(|v| v.as_integer()) {
                Ok(Some(v)) => *slot = v,
                _ => {
                    known = false;
                    break;
                }
            }
        }
        if known {
            return Ok(Integers::Known(operands));
        }
        c.pos = start;
        let mut ev = self.load.evaluator(self.ns, self.depth);
        for _ in 0..count {
            ev.term(&mut f, c, false)?;
        }
        Ok(Integers::Deferred(Code { table: self.load.table, start, end: c.pos }))
    }

    fn buffer(&mut self, c: &mut Cursor) -> Result<Option<Place>, Error> {
        let at = c.pos;
        let mut f = self.frame();
        match self.load.evaluator(self.ns, self.depth).operand_place(&mut f, c, true) {
            Ok(place) => Ok(Some(place)),
            Err(why) => {
                c.pos = at;
                self.load.evaluator(self.ns, self.depth).operand_place(&mut f, c, false)?;
                self.left_out = Some(why);
                Ok(None)
            }
        }
    }

    fn skip(&mut self, c: &mut Cursor) -> Result<(), Error> {
        let mut f = self.frame();
        self.load.evaluator(self.ns, self.depth).term(&mut f, c, false).map(drop)
    }
}
