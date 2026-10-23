// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Evaluating: a small interpreter of AML over a loaded namespace.
//!
//! An evaluation reads names and runs methods: integer, string, buffer and
//! package operations, buffer fields, control flow, method calls, and the
//! fields of `SystemMemory` and `PCI_Config` regions, read through
//! [`Memory`] with the accesses the fields' access types say. It changes
//! nothing outside itself: what it stores to the namespace's names, fields
//! and buffers is kept in the evaluation (`written`), over what the
//! devices' initialisation stored (the namespace's `initial`), over the
//! firmware's own; what would act on the machine (`Notify`, `Sleep`,
//! `Acquire`) does nothing.
//!
//! Every evaluation is bounded ([`Budget`]): in steps (terms evaluated,
//! statements run), in the bytes of the values it makes and copies, in how
//! deep its code nests (terms within terms, statements within statements,
//! calls) and its values nest (packages within packages), and in nested
//! calls. A value is never larger than [`MAX_BUFFER`] bytes, nor a package
//! longer than [`MAX_ELEMENTS`]. Elements are read where they are, not
//! copied with their containers.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::cell::Cell;

use super::code::{Cursor, if_else, is_name_start, search};
use super::convert::{
    bits_of, compare, concatenate, concatenate_templates, converted, element, field_source, from_bcd, int, mid,
    set_bits, set_element, size, to_bcd, to_buffer, to_decimal_string, to_hex_string, to_integer, to_string,
    within_depth,
};
use super::define::{self, Integers, Operands};
use super::{
    Bits, Bounds, Error, Field, FieldUnit, MAX_BUFFER, MAX_BYTES, MAX_CALLS, MAX_ELEMENTS, MAX_NESTING, MAX_STEPS,
    Namespace, Object, REVISION,
};
use crate::name::{NameString, Path};
use crate::value::{Place, Value};
use crate::{Memory, PciFunction};

/// What evaluations may still spend: steps and bytes; and the budget they
/// spend from too, if any (a whole load's, a question's, the namespace
/// owner's). When any runs out, the evaluation fails with
/// [`Error::Limit`].
pub(crate) struct Budget<'a> {
    steps: Cell<u64>,
    bytes: Cell<u64>,
    outer: Option<&'a Budget<'a>>,
}

impl<'a> Budget<'a> {
    pub(crate) const fn new(steps: u64, bytes: u64, outer: Option<&'a Budget<'a>>) -> Budget<'a> {
        Budget { steps: Cell::new(steps), bytes: Cell::new(bytes), outer }
    }

    /// Spends `steps` and `bytes` from this budget and those it is within,
    /// or nothing if any has not that much left.
    fn spend(&self, steps: u64, bytes: u64) -> Result<(), Error> {
        let mut b = Some(self);
        while let Some(x) = b {
            if x.steps.get() < steps || x.bytes.get() < bytes {
                return Err(Error::Limit);
            }
            b = x.outer;
        }
        let mut b = Some(self);
        while let Some(x) = b {
            x.steps.set(x.steps.get() - steps);
            x.bytes.set(x.bytes.get() - bytes);
            b = x.outer;
        }
        Ok(())
    }

    /// What is left: steps and bytes.
    pub(crate) fn left(&self) -> (u64, u64) {
        (self.steps.get(), self.bytes.get())
    }

    /// Sets what is left.
    pub(crate) fn set(&self, steps: u64, bytes: u64) {
        self.steps.set(steps);
        self.bytes.set(bytes);
    }
}

/// One method invocation: its scope, arguments, locals and the objects it
/// has defined.
pub(super) struct Frame {
    scope: Path,
    args: Vec<Value>,
    locals: [Value; 8],
    objects: BTreeMap<Path, Object>,
}

impl Frame {
    pub(super) fn new(scope: Path, args: Vec<Value>) -> Frame {
        Frame { scope, args, locals: Default::default(), objects: BTreeMap::new() }
    }
}

/// How running a list of terms ended.
enum Flow {
    Normal,
    Return(Value),
    Break,
    Continue,
}

/// What a store goes to.
#[derive(Debug, Clone)]
enum Target {
    None,
    Local(u8),
    Arg(u8),
    Named(Path),
    Element(Place, usize),
}

/// One evaluation.
pub(super) struct Evaluator<'n> {
    ns: &'n Namespace,
    memory: &'n dyn Memory,
    budget: Budget<'n>,
    /// Methods being run (and PCI functions being found), nested.
    calls: u32,
    /// How deep the code being run nests.
    nesting: u32,
    /// The namespace's names and fields as this evaluation has stored to
    /// them (the firmware's own are never written).
    written: BTreeMap<Path, Value>,
    /// The PCI functions of the `PCI_Config` regions read so far.
    pci: BTreeMap<Path, PciFunction>,
}

impl<'n> Evaluator<'n> {
    /// An evaluation of the namespace's, spending from `outer` too.
    pub(super) fn new(ns: &'n Namespace, memory: &'n dyn Memory, outer: &'n Budget<'n>) -> Evaluator<'n> {
        Self::with_limits(ns, memory, outer, MAX_STEPS, MAX_BYTES)
    }

    /// An evaluation that may take `steps` steps and make `bytes` bytes of
    /// values (and no more than `outer` has left).
    pub(super) fn with_limits(
        ns: &'n Namespace,
        memory: &'n dyn Memory,
        outer: &'n Budget<'n>,
        steps: u64,
        bytes: u64,
    ) -> Evaluator<'n> {
        Evaluator {
            ns,
            memory,
            budget: Budget::new(steps, bytes, Some(outer)),
            calls: 0,
            nesting: 0,
            written: BTreeMap::new(),
            pci: BTreeMap::new(),
        }
    }

    /// The evaluation of code `depth` levels deep already (in definitions
    /// being loaded): what it nests counts on from there.
    pub(super) fn nested(mut self, depth: u32) -> Evaluator<'n> {
        self.nesting = depth;
        self
    }

    /// What the evaluation stored to the namespace's objects.
    pub(super) fn into_written(self) -> BTreeMap<Path, Value> {
        self.written
    }

    /// Reads the object at `path`, or calls it with `args` if it is a
    /// method.
    pub(super) fn evaluate(&mut self, path: &Path, args: Vec<Value>) -> Result<Value, Error> {
        if !args.iter().all(|a| within_depth(a, MAX_NESTING)) {
            return Err(Error::Limit);
        }
        match self.ns.get(path) {
            Some(Object::Method { .. }) => self.call(path, args),
            Some(Object::Osi) => Ok(self.osi(args.first())),
            Some(_) => {
                let mut f = Frame::new(path.parent().unwrap_or_default(), Vec::new());
                self.read_name(&mut f, path)
            }
            None => Err(Error::NoSuchName(path.clone())),
        }
    }

    fn tick(&self) -> Result<(), Error> {
        self.budget.spend(1, 0)
    }

    /// Spends what making or copying `bytes` bytes of values costs.
    fn charge(&self, bytes: usize) -> Result<(), Error> {
        self.budget.spend(0, bytes as u64)
    }

    /// A copy of `v`, paid for.
    fn copy(&self, v: &Value) -> Result<Value, Error> {
        self.charge(size(v))?;
        Ok(v.clone())
    }

    /// One level deeper into the code (out again with `leave`).
    fn enter(&mut self) -> Result<(), Error> {
        if self.nesting >= MAX_NESTING {
            return Err(Error::Limit);
        }
        self.nesting += 1;
        Ok(())
    }

    fn leave(&mut self) {
        self.nesting -= 1;
    }

    fn mask(&self, v: u64) -> u64 {
        v & self.ns.ones()
    }

    fn truth(&self, b: bool) -> Value {
        Value::Integer(if b { self.ns.ones() } else { 0 })
    }

    fn object<'a>(&'a self, f: &'a Frame, path: &Path) -> Option<&'a Object> {
        match f.objects.get(path) {
            Some(o) => Some(o),
            None => self.ns.get(path),
        }
    }

    fn lookup(&self, f: &Frame, name: &NameString) -> Option<Path> {
        search(&f.scope, name, |p| f.objects.contains_key(p) || self.ns.objects.contains_key(p))
    }

    /// Where a name points and, if it is a method, how many arguments a
    /// call to it takes.
    fn arity(&self, f: &Frame, name: &NameString) -> (Option<Path>, Option<u8>) {
        if let Some(p) = self.lookup(f, name) {
            let n = match self.object(f, &p) {
                Some(Object::Method { args, .. }) => Some(*args),
                Some(Object::Osi) => Some(1),
                _ => None,
            };
            return (Some(p), n);
        }
        match search(&f.scope, name, |p| self.ns.externals.contains_key(p)) {
            Some(p) => {
                let n = self.ns.externals.get(&p).copied();
                (Some(p), n)
            }
            None => (None, None),
        }
    }

    /// Evaluates (`run`) or only parses one term.
    pub(super) fn term(&mut self, f: &mut Frame, c: &mut Cursor, run: bool) -> Result<Value, Error> {
        self.tick()?;
        self.enter()?;
        let v = self.term_in(f, c, run);
        self.leave();
        v
    }

    fn term_in(&mut self, f: &mut Frame, c: &mut Cursor, run: bool) -> Result<Value, Error> {
        let at = c.pos;
        let op = c.byte()?;
        Ok(match op {
            0x00 => Value::Integer(0),
            0x01 => Value::Integer(1),
            0xFF => Value::Integer(self.ns.ones()),
            0x0A => Value::Integer(c.byte()? as u64),
            0x0B => Value::Integer(c.word()? as u64),
            0x0C => Value::Integer(c.dword()? as u64),
            0x0E => Value::Integer(c.qword()?),
            0x0D if run => {
                let s = c.string()?;
                self.charge(s.len())?;
                Value::String(s)
            }
            0x0D => {
                c.skip_string()?;
                Value::Uninitialized
            }
            0x11 => Value::Buffer(self.buffer(f, c, run)?),
            0x12 => Value::Package(self.package(f, c, run, false)?),
            0x13 => Value::Package(self.package(f, c, run, true)?),
            0x60..=0x67 if run => self.copy(&f.locals[(op - 0x60) as usize])?,
            0x68..=0x6E if run => match f.args.get((op - 0x68) as usize) {
                Some(v) => self.copy(v)?,
                None => Value::Uninitialized,
            },
            0x60..=0x6E => Value::Uninitialized,
            b if is_name_start(b) => {
                c.pos = at;
                return self.name_term(f, c, run);
            }
            0x5B => return self.extended(f, c, at, run),
            _ => return self.operator(f, c, op, at, run),
        })
    }

    /// A name in a term: a call if it is a method, else its value.
    fn name_term(&mut self, f: &mut Frame, c: &mut Cursor, run: bool) -> Result<Value, Error> {
        let name = c.name_string()?;
        let (path, arity) = self.arity(f, &name);
        if let Some(n) = arity {
            let mut args = Vec::with_capacity(n as usize);
            for _ in 0..n {
                args.push(self.term(f, c, run)?);
            }
            if !run {
                return Ok(Value::Uninitialized);
            }
            let path = path.unwrap_or_else(|| name.in_scope(&f.scope));
            return match self.object(f, &path) {
                Some(Object::Osi) => Ok(self.osi(args.first())),
                Some(Object::Method { .. }) => self.call(&path, args),
                _ => Err(Error::NoSuchName(path)),
            };
        }
        if !run {
            return Ok(Value::Uninitialized);
        }
        let path = path.ok_or_else(|| Error::NoSuchName(name.in_scope(&f.scope)))?;
        self.read_name(f, &path)
    }

    /// The data of a name: the method's own, then what this evaluation and
    /// the devices' initialisation stored, then the firmware's.
    fn data<'a>(&'a self, f: &'a Frame, path: &Path) -> Option<&'a Value> {
        if let Some(Object::Name(v)) = f.objects.get(path) {
            return Some(v);
        }
        let ns = self.ns;
        let path = ns.canonical(path);
        if let Some(v) = self.written.get(path).or_else(|| ns.initial.get(path)) {
            return Some(v);
        }
        match ns.objects.get(path) {
            Some(Object::Name(v)) => Some(v),
            _ => None,
        }
    }

    /// The value of a named object.
    pub(super) fn read_name(&mut self, f: &mut Frame, path: &Path) -> Result<Value, Error> {
        if let Some(o) = f.objects.get(path) {
            return match o {
                Object::Name(v) => self.copy(v),
                Object::BufferField { place, bits } => {
                    let (place, bits) = (place.clone(), bits.clone());
                    self.read_buffer_field(f, &place, &bits)
                }
                Object::Field(field) => match self.written.get(path) {
                    Some(v) => self.copy(v),
                    None => {
                        let field = field.clone();
                        self.read_field(f, &field)
                    }
                },
                _ => Ok(Value::Reference(path.clone())),
            };
        }
        let ns = self.ns;
        let path = ns.canonical(path);
        if let Some(v) = self.written.get(path).or_else(|| ns.initial.get(path)) {
            return self.copy(v);
        }
        match ns.objects.get(path) {
            Some(Object::Name(v)) => self.copy(v),
            Some(Object::Field(field)) => self.read_field(f, field),
            Some(Object::BufferField { place, bits }) => self.read_buffer_field(f, place, bits),
            Some(Object::Method { args: 0, .. }) => self.call(path, Vec::new()),
            Some(_) => Ok(Value::Reference(path.clone())),
            None => Err(Error::NoSuchName(path.clone())),
        }
    }

    /// Reads a buffer field: its bits of its buffer.
    fn read_buffer_field(&mut self, f: &mut Frame, place: &Place, bits: &Bits) -> Result<Value, Error> {
        let (offset, width) = self.buffer_bits(bits)?;
        let v = bits_of(&field_source(self.place_value(f, place))?, offset, width)?;
        self.charge(size(&v))?;
        Ok(v)
    }

    /// Where a buffer field's bits are, its operands evaluated now if they
    /// were left for later.
    fn buffer_bits(&mut self, bits: &Bits) -> Result<(u64, u64), Error> {
        match bits {
            Bits::Known { offset, width } => Ok((*offset, *width)),
            Bits::Deferred { scope, code, kind } => {
                let bytes = self.ns.code.get(code.table).ok_or(Error::Truncated(code.start))?.clone();
                let mut c =
                    Cursor { code: bytes.get(..code.end).ok_or(Error::Truncated(code.start))?, pos: code.start };
                let mut f = Frame::new(scope.clone(), Vec::new());
                let mut operands = [0; 2];
                for slot in operands.iter_mut().take(kind.operands()) {
                    *slot = int(&self.term(&mut f, &mut c, true)?)?;
                }
                Ok(kind.bits(operands))
            }
        }
    }

    /// Reads a field of a `SystemMemory` or a `PCI_Config` region.
    fn read_field(&mut self, f: &Frame, field: &Field) -> Result<Value, Error> {
        let FieldUnit::Region(region) = &field.unit else {
            return Err(Error::Unsupported("reading an index or bank field"));
        };
        let Some(Object::Region { space, bounds }) = self.object(f, region).cloned() else {
            return Err(Error::NoSuchName(region.clone()));
        };
        match space {
            0 | 2 => {}
            1 => return Err(Error::Unsupported("reading I/O ports")),
            3 => return Err(Error::Unsupported("reading the embedded controller")),
            _ => return Err(Error::Unsupported("reading this address space")),
        }
        let (base, length) = match bounds {
            Bounds::Known { offset, length } => (offset, length),
            Bounds::Deferred { scope, code } => {
                let bytes = self.ns.code.get(code.table).ok_or(Error::Truncated(code.start))?.clone();
                let mut c =
                    Cursor { code: bytes.get(..code.end).ok_or(Error::Truncated(code.start))?, pos: code.start };
                self.region_bounds(&scope, &mut c)?
            }
            Bounds::Unknown => return Err(Error::Unsupported("a region whose address cannot be evaluated")),
        };
        // Whole accesses of the field's width (aligned within the region),
        // as the hardware may need, and as ACPI's machine makes them.
        let unit = (field.access_bits / 8).max(1) as u64;
        let end = field.bit_offset.checked_add(field.bit_width).ok_or(Error::Type("a field inside its region"))?;
        let first = field.bit_offset / 8 / unit * unit;
        let last = end.div_ceil(8).div_ceil(unit) * unit;
        if last > length || last - first > MAX_BUFFER as u64 {
            return Err(Error::Type("a field inside its region"));
        }
        self.charge((last - first) as usize)?;
        let mut bytes = alloc::vec![0u8; (last - first) as usize];
        let address = base.checked_add(first).ok_or(Error::Memory(base))?;
        if space == 2 {
            // An offset in the function's configuration space (4 KiB).
            let function = self.pci_function(f, region)?;
            let offset = u16::try_from(address)
                .ok()
                .filter(|&o| o as usize + bytes.len() <= 0x1000)
                .ok_or(Error::Type("a field inside configuration space"))?;
            if !self.memory.read_pci_field(function, offset, unit as usize, &mut bytes) {
                return Err(Error::PciConfig(function, offset));
            }
        } else if !self.memory.read_field(address, unit as usize, &mut bytes) {
            return Err(Error::Memory(address));
        }
        bits_of(&bytes, field.bit_offset - first * 8, field.bit_width)
    }

    /// The PCI function whose configuration space the `PCI_Config` region
    /// at `region` is, as ACPI has it (and ACPICA finds it): the one at the
    /// `_ADR` of the device the region is in, on the bus of the PCI root
    /// bridge above it (its `_SEG` and `_BBN`, 0 without them), or of the
    /// last PCI-to-PCI bridge between the two (the secondary bus its
    /// configuration space holds).
    fn pci_function(&mut self, f: &Frame, region: &Path) -> Result<PciFunction, Error> {
        if let Some(&function) = self.pci.get(region) {
            return Ok(function);
        }
        // Finding it evaluates the devices' objects, which could read such
        // a region in turn: bounded as calls are.
        if self.calls >= MAX_CALLS {
            return Err(Error::Limit);
        }
        self.enter()?;
        self.calls += 1;
        let found = self.find_pci_function(f, region);
        self.calls -= 1;
        self.leave();
        let function = found?;
        self.pci.insert(region.clone(), function);
        Ok(function)
    }

    fn find_pci_function(&mut self, f: &Frame, region: &Path) -> Result<PciFunction, Error> {
        // The devices the region is in up to the root bridge, nearest first.
        let mut devices = Vec::new();
        let mut root = None;
        let mut at = region.parent();
        while let Some(p) = at {
            if matches!(self.object(f, &p), Some(Object::Device)) {
                let is_root = self.child_value(&p, "_HID")?.is_some_and(|v| is_pci_root_id(&v))
                    || self.child_value(&p, "_CID")?.is_some_and(|v| is_pci_root_id(&v));
                if is_root {
                    root = Some(p);
                    break;
                }
                devices.push(p.clone());
            }
            at = p.parent();
        }
        let mut function = PciFunction::default();
        if let Some(root) = &root {
            function.segment = self.child_integer(root, "_SEG")?.unwrap_or(0) as u16;
            function.bus = self.child_integer(root, "_BBN")?.unwrap_or(0) as u8;
            if devices.is_empty() {
                // A region of the root bridge's own.
                set_pci_address(&mut function, self.child_integer(root, "_ADR")?.unwrap_or(0));
            }
        }
        // From the root bridge down to the region's device: a bridge between
        // them (its header type 1, or 2 for CardBus) puts the devices below
        // it on its secondary bus.
        let mut bus = function.bus;
        for (i, d) in devices.iter().enumerate().rev() {
            let Some(adr) = self.child_integer(d, "_ADR")? else { continue };
            set_pci_address(&mut function, adr);
            function.bus = bus;
            if i > 0 && matches!(self.config_byte(function, 0x0E)? & 0x7F, 1 | 2) {
                bus = self.config_byte(function, 0x19)?;
            }
        }
        Ok(function)
    }

    /// The value of `device`'s object `name` (`_ADR`) in this evaluation,
    /// if it has one that evaluates. Only running out of the budget fails.
    fn child_value(&mut self, device: &Path, name: &str) -> Result<Option<Value>, Error> {
        let Some(path) = device.join(name) else { return Ok(None) };
        if self.ns.get(&path).is_none() {
            return Ok(None);
        }
        match self.read_name(&mut Frame::new(device.clone(), Vec::new()), &path) {
            Ok(v) => Ok(Some(v)),
            Err(Error::Limit) => Err(Error::Limit),
            Err(_) => Ok(None),
        }
    }

    fn child_integer(&mut self, device: &Path, name: &str) -> Result<Option<u64>, Error> {
        Ok(self.child_value(device, name)?.and_then(|v| v.as_integer()))
    }

    fn config_byte(&self, function: PciFunction, offset: u16) -> Result<u8, Error> {
        let mut b = [0u8];
        if self.memory.read_pci_field(function, offset, 1, &mut b) {
            Ok(b[0])
        } else {
            Err(Error::PciConfig(function, offset))
        }
    }

    /// The value held in a place, where it is.
    fn place_value<'a>(&'a self, f: &'a Frame, place: &'a Place) -> Option<&'a Value> {
        match place {
            Place::Local(n) => f.locals.get(*n as usize),
            Place::Arg(n) => f.args.get(*n as usize),
            Place::Named(p) => self.data(f, p),
            Place::Temporary(v) => Some(v),
        }
    }

    /// Element `i` of the value in `place`.
    fn element_of(&self, f: &Frame, place: &Place, i: usize) -> Result<Value, Error> {
        let container = self.place_value(f, place).ok_or(Error::Type("a package, buffer or string"))?;
        let e = element(container, i)?;
        self.charge(size(&e))?;
        Ok(e)
    }

    /// Changes the value held in a place.
    fn update_place(
        &mut self,
        f: &mut Frame,
        place: &Place,
        change: impl FnOnce(&mut Value) -> Result<(), Error>,
    ) -> Result<(), Error> {
        match place {
            Place::Local(n) => change(f.locals.get_mut(*n as usize).ok_or(Error::Type("a local"))?),
            Place::Arg(n) => change(f.args.get_mut(*n as usize).ok_or(Error::Type("an argument"))?),
            Place::Named(p) => {
                if let Some(Object::Name(v)) = f.objects.get_mut(p) {
                    return change(v);
                }
                let ns = self.ns;
                let p = ns.canonical(p);
                if !self.written.contains_key(p) {
                    // The first change copies the namespace's value into the
                    // evaluation's.
                    let current = match ns.initial.get(p) {
                        Some(v) => v,
                        None => match ns.objects.get(p) {
                            Some(Object::Name(v)) => v,
                            _ => return Err(Error::Type("a named buffer or package")),
                        },
                    };
                    let copy = self.copy(current)?;
                    self.written.insert(p.clone(), copy);
                }
                change(self.written.get_mut(p).ok_or(Error::Type("a named buffer or package"))?)
            }
            // A value computed on the spot: the change goes nowhere.
            Place::Temporary(v) => change(&mut self.copy(v)?),
        }
    }

    fn store(&mut self, f: &mut Frame, target: &Target, value: Value) -> Result<(), Error> {
        match target {
            Target::None => Ok(()),
            Target::Local(n) => {
                *f.locals.get_mut(*n as usize).ok_or(Error::Type("a local"))? = value;
                Ok(())
            }
            Target::Arg(n) => {
                let n = *n as usize;
                if f.args.len() <= n {
                    f.args.resize(n + 1, Value::Uninitialized);
                }
                f.args[n] = value;
                Ok(())
            }
            Target::Element(place, i) => {
                let i = *i;
                self.update_place(f, place, |v| set_element(v, i, value, MAX_NESTING - 1))
            }
            Target::Named(path) => self.store_named(f, path, value),
        }
    }

    /// Stores the result `r` of an operator to its target, if it has one,
    /// and gives it back.
    fn store_result(&mut self, f: &mut Frame, target: &Target, r: Value) -> Result<Value, Error> {
        if !matches!(target, Target::None) {
            let copy = self.copy(&r)?;
            self.store(f, target, copy)?;
        }
        Ok(r)
    }

    fn store_named(&mut self, f: &mut Frame, path: &Path, value: Value) -> Result<(), Error> {
        if let Some(o) = f.objects.get_mut(path) {
            match o {
                Object::Name(v) => {
                    let new = converted(v, value);
                    *v = new;
                    return Ok(());
                }
                Object::BufferField { place, bits } => {
                    let (place, bits) = (place.clone(), bits.clone());
                    let (offset, width) = self.buffer_bits(&bits)?;
                    return self.update_place(f, &place, |b| set_bits(b, offset, width, &value));
                }
                Object::Field(_) => {
                    self.written.insert(path.clone(), value);
                    return Ok(());
                }
                _ => return Err(Error::Unsupported("storing to this kind of object")),
            }
        }
        let ns = self.ns;
        let path = ns.canonical(path);
        match ns.objects.get(path) {
            Some(Object::Name(v)) => {
                let current = self.written.get(path).or_else(|| ns.initial.get(path)).unwrap_or(v);
                let new = converted(current, value);
                if let Value::Buffer(b) = &new {
                    self.charge(b.len())?;
                }
                self.written.insert(path.clone(), new);
                Ok(())
            }
            // Kept for this evaluation; the firmware's field is not written.
            Some(Object::Field(_)) => {
                self.written.insert(path.clone(), value);
                Ok(())
            }
            Some(Object::BufferField { place, bits }) => {
                let (offset, width) = self.buffer_bits(bits)?;
                self.update_place(f, place, |b| set_bits(b, offset, width, &value))
            }
            Some(_) => Err(Error::Unsupported("storing to this kind of object")),
            None => Err(Error::NoSuchName(path.clone())),
        }
    }

    fn read_target(&mut self, f: &mut Frame, target: &Target) -> Result<Value, Error> {
        match target {
            Target::None => Ok(Value::Uninitialized),
            Target::Local(n) => self.copy(f.locals.get(*n as usize).ok_or(Error::Type("a local"))?),
            Target::Arg(n) => match f.args.get(*n as usize) {
                Some(v) => self.copy(v),
                None => Ok(Value::Uninitialized),
            },
            Target::Named(p) => self.read_name(f, p),
            Target::Element(place, i) => self.element_of(f, place, *i),
        }
    }

    /// The data a target holds where it is: a local's, an argument's, a
    /// name's (`None` for what has to be evaluated: a field, an element).
    fn target_value<'a>(&'a self, f: &'a Frame, target: &Target) -> Option<&'a Value> {
        match target {
            Target::None => None,
            Target::Local(n) => f.locals.get(*n as usize),
            Target::Arg(n) => f.args.get(*n as usize),
            Target::Named(p) => self.data(f, p),
            Target::Element(..) => None,
        }
    }

    /// `g` of the data a target holds (or, if `follow`, of what a
    /// reference in it refers to), read where it is if it can be.
    fn with_target<R>(
        &mut self,
        f: &mut Frame,
        target: &Target,
        follow: bool,
        g: impl FnOnce(&Value) -> Result<R, Error>,
    ) -> Result<R, Error> {
        let referred = match self.target_value(f, target) {
            Some(Value::Reference(p)) if follow => p.clone(),
            Some(v) => return g(v),
            None => match self.read_target(f, target)? {
                Value::Reference(p) if follow => p,
                v => return g(&v),
            },
        };
        let v = self.read_name(f, &referred)?;
        g(&v)
    }

    /// A SuperName or Target: where a result goes.
    fn target(&mut self, f: &mut Frame, c: &mut Cursor, run: bool) -> Result<Target, Error> {
        let at = c.pos;
        match c.peek()? {
            0x00 => {
                c.pos += 1;
                Ok(Target::None)
            }
            op @ 0x60..=0x67 => {
                c.pos += 1;
                Ok(Target::Local(op - 0x60))
            }
            op @ 0x68..=0x6E => {
                c.pos += 1;
                Ok(Target::Arg(op - 0x68))
            }
            // Debug
            0x5B if c.peek_at(1) == Some(0x31) => {
                c.pos += 2;
                Ok(Target::None)
            }
            b if is_name_start(b) => {
                let name = c.name_string()?;
                match self.arity(f, &name) {
                    // A method that returns a reference.
                    (_, Some(n)) if n > 0 => {
                        c.pos = at;
                        let v = self.term(f, c, run)?;
                        reference_target(v, run)
                    }
                    (Some(p), _) => Ok(Target::Named(p)),
                    (None, _) => Ok(Target::Named(name.in_scope(&f.scope))),
                }
            }
            // Index, DerefOf, RefOf
            _ => {
                let v = self.term(f, c, run)?;
                reference_target(v, run)
            }
        }
    }

    /// An operand whose place matters (`Index`, `CreateDWordField`).
    pub(super) fn operand_place(&mut self, f: &mut Frame, c: &mut Cursor, run: bool) -> Result<Place, Error> {
        let at = c.pos;
        match c.peek()? {
            op @ 0x60..=0x67 => {
                c.pos += 1;
                Ok(Place::Local(op - 0x60))
            }
            op @ 0x68..=0x6E => {
                c.pos += 1;
                Ok(Place::Arg(op - 0x68))
            }
            b if is_name_start(b) => {
                let name = c.name_string()?;
                if let (Some(p), None) = self.arity(f, &name)
                    && matches!(self.object(f, &p), Some(Object::Name(_)))
                {
                    return Ok(Place::Named(p));
                }
                c.pos = at;
                Ok(Place::Temporary(self.term(f, c, run)?))
            }
            _ => Ok(Place::Temporary(self.term(f, c, run)?)),
        }
    }

    fn buffer(&mut self, f: &mut Frame, c: &mut Cursor, run: bool) -> Result<Vec<u8>, Error> {
        let end = c.pkg_end()?;
        let size = self.term(f, c, run)?;
        if c.pos > end {
            return Err(Error::Truncated(c.pos));
        }
        let code = c.code;
        let init = &code[c.pos..end];
        c.pos = end;
        if !run {
            return Ok(Vec::new());
        }
        let size = int(&size)?;
        if size > MAX_BUFFER as u64 || init.len() > MAX_BUFFER {
            return Err(Error::Limit);
        }
        let len = (size as usize).max(init.len());
        self.charge(len)?;
        let mut b = alloc::vec![0u8; len];
        b[..init.len()].copy_from_slice(init);
        Ok(b)
    }

    fn package(&mut self, f: &mut Frame, c: &mut Cursor, run: bool, var: bool) -> Result<Vec<Value>, Error> {
        let end = c.pkg_end()?;
        let count = if var { int(&self.term(f, c, run)?)? } else { c.byte()? as u64 };
        let mut items = Vec::new();
        while c.pos < end {
            // A name in a package is a reference to the object, not its value.
            let item = if is_name_start(c.peek()?) {
                let n = c.name_string()?;
                Value::Reference(self.lookup(f, &n).unwrap_or_else(|| n.in_scope(&f.scope)))
            } else {
                self.term(f, c, run)?
            };
            if items.len() >= MAX_ELEMENTS {
                return Err(Error::Limit);
            }
            if run {
                if !within_depth(&item, MAX_NESTING - 1) {
                    return Err(Error::Limit);
                }
                self.charge(size_of::<Value>())?;
            }
            items.push(item);
        }
        if c.pos != end {
            return Err(Error::Truncated(end));
        }
        if run && (items.len() as u64) < count {
            // At most as many as a package may have, which the list is not
            // longer than.
            let n = count.min(MAX_ELEMENTS as u64) as usize;
            self.charge((n - items.len()) * size_of::<Value>())?;
            items.resize(n, Value::Uninitialized);
        }
        Ok(items)
    }

    /// Evaluates an operation region's offset and length.
    pub(super) fn region_bounds(&mut self, scope: &Path, c: &mut Cursor) -> Result<(u64, u64), Error> {
        let mut f = Frame::new(scope.clone(), Vec::new());
        let offset = int(&self.term(&mut f, c, true)?)?;
        let length = int(&self.term(&mut f, c, true)?)?;
        Ok((offset, length))
    }

    /// `\_OSI`'s answer.
    pub(super) fn osi(&self, arg: Option<&Value>) -> Value {
        let s = arg.and_then(Value::as_str).unwrap_or("");
        // As Windows answers (the firmware is written for it), and the
        // features every interpreter has.
        let yes = s.starts_with("Windows ")
            || matches!(
                s,
                "Module Device"
                    | "Processor Device"
                    | "3.0 Thermal Model"
                    | "3.0 _SCP Extensions"
                    | "Processor Aggregator Device"
                    | "Extended Address Space Descriptor"
            );
        self.truth(yes)
    }

    pub(super) fn call(&mut self, path: &Path, args: Vec<Value>) -> Result<Value, Error> {
        let Some(Object::Method { code, .. }) = self.ns.get(path) else {
            return Err(Error::NoSuchName(path.clone()));
        };
        if self.calls >= MAX_CALLS {
            return Err(Error::Limit);
        }
        let code = *code;
        let bytes = self.ns.code.get(code.table).ok_or(Error::Truncated(code.start))?.clone();
        let mut c = Cursor { code: bytes.get(..code.end).ok_or(Error::Truncated(code.start))?, pos: code.start };
        let mut f = Frame::new(path.clone(), args);
        self.enter()?;
        self.calls += 1;
        let flow = self.run_list(&mut f, &mut c, code.end);
        self.calls -= 1;
        self.leave();
        Ok(match flow? {
            // A reference into the method's own objects outlives them as a
            // value.
            Flow::Return(Value::Element(place, i)) => {
                let own = match &*place {
                    Place::Local(_) | Place::Arg(_) => true,
                    Place::Named(p) => f.objects.contains_key(p),
                    Place::Temporary(_) => false,
                };
                if own {
                    let container = match self.place_value(&f, &place) {
                        Some(v) => self.copy(v)?,
                        None => Value::Uninitialized,
                    };
                    if !within_depth(&container, MAX_NESTING - 1) {
                        return Err(Error::Limit);
                    }
                    Value::Element(Box::new(Place::Temporary(container)), i)
                } else {
                    Value::Element(place, i)
                }
            }
            Flow::Return(v) => v,
            _ => Value::Uninitialized,
        })
    }

    fn run_list(&mut self, f: &mut Frame, c: &mut Cursor, end: usize) -> Result<Flow, Error> {
        while c.pos < end {
            match self.statement(f, c, end)? {
                Flow::Normal => {}
                other => return Ok(other),
            }
        }
        Ok(Flow::Normal)
    }

    /// Runs one statement of a list that ends at `list_end`.
    fn statement(&mut self, f: &mut Frame, c: &mut Cursor, list_end: usize) -> Result<Flow, Error> {
        self.tick()?;
        self.enter()?;
        let flow = self.statement_in(f, c, list_end);
        self.leave();
        flow
    }

    fn statement_in(&mut self, f: &mut Frame, c: &mut Cursor, list_end: usize) -> Result<Flow, Error> {
        match c.peek()? {
            // If, with an optional Else.
            0xA0 => {
                c.pos += 1;
                let frame = if_else(c, list_end)?;
                let taken = int(&self.term(f, c, true)?)? != 0;
                if c.pos > frame.then_end {
                    return Err(Error::Truncated(c.pos));
                }
                let flow = if taken {
                    self.run_list(f, c, frame.then_end)?
                } else if let Some((start, end)) = frame.otherwise {
                    c.pos = start;
                    self.run_list(f, c, end)?
                } else {
                    Flow::Normal
                };
                c.pos = frame.end;
                Ok(flow)
            }
            // While
            0xA2 => {
                c.pos += 1;
                let at = c.pos;
                let end = c.pkg_end()?;
                if end > list_end {
                    return Err(Error::Truncated(at));
                }
                let condition = c.pos;
                loop {
                    c.pos = condition;
                    if int(&self.term(f, c, true)?)? == 0 {
                        break;
                    }
                    if c.pos > end {
                        return Err(Error::Truncated(c.pos));
                    }
                    match self.run_list(f, c, end)? {
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        Flow::Break => break,
                        Flow::Normal | Flow::Continue => {}
                    }
                }
                c.pos = end;
                Ok(Flow::Normal)
            }
            0xA4 => {
                c.pos += 1;
                Ok(Flow::Return(self.term(f, c, true)?))
            }
            0xA5 => {
                c.pos += 1;
                Ok(Flow::Break)
            }
            0x9F => {
                c.pos += 1;
                Ok(Flow::Continue)
            }
            0xA3 | 0xCC => {
                c.pos += 1;
                Ok(Flow::Normal)
            }
            // Name: an object of this invocation.
            0x08 => {
                c.pos += 1;
                let name = c.name_string()?;
                let value = self.term(f, c, true)?;
                f.objects.insert(name.in_scope(&f.scope), Object::Name(value));
                Ok(Flow::Normal)
            }
            0x14 | 0x10 => Err(Error::Unsupported("a method or scope defined inside a method")),
            0x5B if matches!(c.peek_at(1), Some(0x82..=0x85)) => {
                Err(Error::Unsupported("a device defined inside a method"))
            }
            _ => {
                // A region, a field, a buffer field...: an object of this
                // invocation; else an expression, for what it does.
                match define::definition(c, &mut InMethod { ev: self, f })? {
                    Some(objects) => f.objects.extend(objects),
                    None => {
                        self.term(f, c, true)?;
                    }
                }
                Ok(Flow::Normal)
            }
        }
    }

    /// Two operands and a target, for the binary operators.
    fn binary(&mut self, f: &mut Frame, c: &mut Cursor, run: bool) -> Result<(Value, Value, Target), Error> {
        let a = self.term(f, c, run)?;
        let b = self.term(f, c, run)?;
        let t = self.target(f, c, run)?;
        Ok((a, b, t))
    }

    fn operator(&mut self, f: &mut Frame, c: &mut Cursor, op: u8, at: usize, run: bool) -> Result<Value, Error> {
        match op {
            // Store
            0x70 => {
                let v = self.term(f, c, run)?;
                let t = self.target(f, c, run)?;
                if !run {
                    return Ok(v);
                }
                self.store_result(f, &t, v)
            }
            // RefOf
            0x71 => {
                let t = self.target(f, c, run)?;
                Ok(match t {
                    Target::Named(p) => Value::Reference(p),
                    Target::Element(place, i) => Value::Element(Box::new(place), i),
                    _ => Value::Uninitialized,
                })
            }
            // Add, Subtract, Multiply, ShiftLeft, ShiftRight, And, Nand, Or, Nor, Xor, Mod
            0x72 | 0x74 | 0x77 | 0x79 | 0x7A | 0x7B | 0x7C | 0x7D | 0x7E | 0x7F | 0x85 => {
                let (a, b, t) = self.binary(f, c, run)?;
                if !run {
                    return Ok(Value::Uninitialized);
                }
                let (a, b) = (int(&a)?, int(&b)?);
                let r = match op {
                    0x72 => a.wrapping_add(b),
                    0x74 => a.wrapping_sub(b),
                    0x77 => a.wrapping_mul(b),
                    0x79 => a.checked_shl(b.min(64) as u32).unwrap_or(0),
                    0x7A => a.checked_shr(b.min(64) as u32).unwrap_or(0),
                    0x7B => a & b,
                    0x7C => !(a & b),
                    0x7D => a | b,
                    0x7E => !(a | b),
                    0x7F => a ^ b,
                    _ => a.checked_rem(b).ok_or(Error::DivideByZero)?,
                };
                let r = Value::Integer(self.mask(r));
                self.store_result(f, &t, r)
            }
            // Concatenate, ConcatenateResTemplate
            0x73 | 0x84 => {
                let (a, b, t) = self.binary(f, c, run)?;
                if !run {
                    return Ok(Value::Uninitialized);
                }
                let r = if op == 0x73 { concatenate(&a, &b)? } else { concatenate_templates(&a, &b)? };
                self.charge(size(&r))?;
                self.store_result(f, &t, r)
            }
            // Increment, Decrement
            0x75 | 0x76 => {
                let t = self.target(f, c, run)?;
                if !run {
                    return Ok(Value::Uninitialized);
                }
                let v = int(&self.read_target(f, &t)?)?;
                let r = Value::Integer(self.mask(if op == 0x75 { v.wrapping_add(1) } else { v.wrapping_sub(1) }));
                self.store(f, &t, r.clone())?;
                Ok(r)
            }
            // Divide: remainder, then quotient.
            0x78 => {
                let a = self.term(f, c, run)?;
                let b = self.term(f, c, run)?;
                let remainder = self.target(f, c, run)?;
                let quotient = self.target(f, c, run)?;
                if !run {
                    return Ok(Value::Uninitialized);
                }
                let (a, b) = (int(&a)?, int(&b)?);
                if b == 0 {
                    return Err(Error::DivideByZero);
                }
                self.store(f, &remainder, Value::Integer(a % b))?;
                self.store(f, &quotient, Value::Integer(a / b))?;
                Ok(Value::Integer(a / b))
            }
            // Not, FindSetLeftBit, FindSetRightBit
            0x80..=0x82 => {
                let a = self.term(f, c, run)?;
                let t = self.target(f, c, run)?;
                if !run {
                    return Ok(Value::Uninitialized);
                }
                let a = int(&a)?;
                let r = match op {
                    0x80 => self.mask(!a),
                    0x81 => 64 - a.leading_zeros() as u64,
                    _ => {
                        if a == 0 {
                            0
                        } else {
                            a.trailing_zeros() as u64 + 1
                        }
                    }
                };
                self.store_result(f, &t, Value::Integer(r))
            }
            // DerefOf
            0x83 => {
                let v = self.term(f, c, run)?;
                if !run {
                    return Ok(Value::Uninitialized);
                }
                match v {
                    Value::Reference(p) => self.read_name(f, &p),
                    Value::Element(place, i) => self.element_of(f, &place, i),
                    Value::String(s) => {
                        let name = NameString::parse(&s).ok_or(Error::Type("a name"))?;
                        let p = self.lookup(f, &name).ok_or_else(|| Error::NoSuchName(name.in_scope(&f.scope)))?;
                        self.read_name(f, &p)
                    }
                    _ => Err(Error::Type("a reference")),
                }
            }
            // Notify
            0x86 => {
                self.target(f, c, run)?;
                self.term(f, c, run)?;
                Ok(Value::Uninitialized)
            }
            // SizeOf
            0x87 => {
                let t = self.target(f, c, run)?;
                if !run {
                    return Ok(Value::Uninitialized);
                }
                let n = self.with_target(f, &t, true, |v| match v {
                    Value::String(s) => Ok(s.len() as u64),
                    Value::Buffer(b) => Ok(b.len() as u64),
                    Value::Package(p) => Ok(p.len() as u64),
                    _ => Err(Error::Type("a string, buffer or package")),
                })?;
                Ok(Value::Integer(n))
            }
            // Index
            0x88 => {
                let place = self.operand_place(f, c, run)?;
                let i = self.term(f, c, run)?;
                let t = self.target(f, c, run)?;
                if !run {
                    return Ok(Value::Uninitialized);
                }
                if let Place::Temporary(v) = &place
                    && !within_depth(v, MAX_NESTING - 1)
                {
                    return Err(Error::Limit);
                }
                let i = usize::try_from(int(&i)?).unwrap_or(usize::MAX);
                self.store_result(f, &t, Value::Element(Box::new(place), i))
            }
            // Match
            0x89 => {
                let package = self.term(f, c, run)?;
                let op1 = c.byte()?;
                let v1 = self.term(f, c, run)?;
                let op2 = c.byte()?;
                let v2 = self.term(f, c, run)?;
                let start = self.term(f, c, run)?;
                if !run {
                    return Ok(Value::Uninitialized);
                }
                let Value::Package(items) = package else { return Err(Error::Type("a package")) };
                let (v1, v2) = (int(&v1)?, int(&v2)?);
                let test = |op: u8, x: u64, v: u64| match op {
                    0 => true,
                    1 => x == v,
                    2 => x <= v,
                    3 => x < v,
                    4 => x >= v,
                    _ => x > v,
                };
                let start = usize::try_from(int(&start)?).unwrap_or(usize::MAX);
                for (i, item) in items.iter().enumerate().skip(start) {
                    if let Some(x) = item.as_integer()
                        && test(op1, x, v1)
                        && test(op2, x, v2)
                    {
                        return Ok(Value::Integer(i as u64));
                    }
                }
                Ok(Value::Integer(self.ns.ones()))
            }
            // ObjectType
            0x8E => {
                let t = self.target(f, c, run)?;
                if !run {
                    return Ok(Value::Uninitialized);
                }
                // A name's object, but for data, whose value has the type.
                let object = match &t {
                    Target::Named(p) => self.object(f, p).map(|o| match o {
                        Object::Name(_) => None,
                        Object::Field(_) => Some(5),
                        Object::Device => Some(6),
                        Object::Event => Some(7),
                        Object::Method { .. } | Object::Osi => Some(8),
                        Object::Mutex => Some(9),
                        Object::Region { .. } => Some(10),
                        Object::PowerResource => Some(11),
                        Object::Processor => Some(12),
                        Object::ThermalZone => Some(13),
                        Object::BufferField { .. } => Some(14),
                        Object::Scope | Object::Alias(_) => Some(0),
                    }),
                    _ => Some(None),
                };
                let code = match object {
                    Some(Some(code)) => code,
                    Some(None) => self.with_target(f, &t, false, |v| Ok(v.type_code()))?,
                    None => 0,
                };
                Ok(Value::Integer(code))
            }
            // LAnd, LOr
            0x90 | 0x91 => {
                let a = self.term(f, c, run)?;
                let b = self.term(f, c, run)?;
                if !run {
                    return Ok(Value::Uninitialized);
                }
                let (a, b) = (int(&a)? != 0, int(&b)? != 0);
                Ok(self.truth(if op == 0x90 { a && b } else { a || b }))
            }
            // LNot
            0x92 => {
                let a = self.term(f, c, run)?;
                if !run {
                    return Ok(Value::Uninitialized);
                }
                Ok(self.truth(int(&a)? == 0))
            }
            // LEqual, LGreater, LLess
            0x93..=0x95 => {
                let a = self.term(f, c, run)?;
                let b = self.term(f, c, run)?;
                if !run {
                    return Ok(Value::Uninitialized);
                }
                let order = compare(&a, &b)?;
                Ok(self.truth(match op {
                    0x93 => order.is_eq(),
                    0x94 => order.is_gt(),
                    _ => order.is_lt(),
                }))
            }
            // ToBuffer, ToDecimalString, ToHexString, ToInteger
            0x96..=0x99 => {
                let a = self.term(f, c, run)?;
                let t = self.target(f, c, run)?;
                if !run {
                    return Ok(Value::Uninitialized);
                }
                let r = match op {
                    0x96 => Value::Buffer(to_buffer(&a)?),
                    0x97 => Value::String(to_decimal_string(&a)?),
                    0x98 => Value::String(to_hex_string(&a)?),
                    _ => Value::Integer(to_integer(&a)?),
                };
                self.charge(size(&r))?;
                self.store_result(f, &t, r)
            }
            // ToString
            0x9C => {
                let a = self.term(f, c, run)?;
                let len = self.term(f, c, run)?;
                let t = self.target(f, c, run)?;
                if !run {
                    return Ok(Value::Uninitialized);
                }
                let r = Value::String(to_string(&a, int(&len)?)?);
                self.charge(size(&r))?;
                self.store_result(f, &t, r)
            }
            // CopyObject
            0x9D => {
                let v = self.term(f, c, run)?;
                let t = self.target(f, c, run)?;
                if run {
                    let copy = self.copy(&v)?;
                    match &t {
                        Target::Named(p) if f.objects.contains_key(p) => {
                            f.objects.insert(p.clone(), Object::Name(copy));
                        }
                        Target::Named(p) => {
                            let ns = self.ns;
                            self.written.insert(ns.canonical(p).clone(), copy);
                        }
                        other => self.store(f, other, copy)?,
                    }
                }
                Ok(v)
            }
            // Mid
            0x9E => {
                let a = self.term(f, c, run)?;
                let index = self.term(f, c, run)?;
                let len = self.term(f, c, run)?;
                let t = self.target(f, c, run)?;
                if !run {
                    return Ok(Value::Uninitialized);
                }
                let r = mid(&a, int(&index)?, int(&len)?)?;
                self.charge(size(&r))?;
                self.store_result(f, &t, r)
            }
            // Statements met while only parsing (code outside methods).
            0xA0..=0xA2 if !run => {
                c.pos = c.pkg_end()?;
                Ok(Value::Uninitialized)
            }
            _ => Err(Error::Opcode(op as u16, at)),
        }
    }

    fn extended(&mut self, f: &mut Frame, c: &mut Cursor, at: usize, run: bool) -> Result<Value, Error> {
        let op = c.byte()?;
        match op {
            // CondRefOf: the name may not exist.
            0x12 => {
                let found = if is_name_start(c.peek()?) {
                    let name = c.name_string()?;
                    self.lookup(f, &name).map(Value::Reference)
                } else {
                    match self.target(f, c, run)? {
                        Target::Named(p) => Some(Value::Reference(p)),
                        _ => None,
                    }
                };
                let t = self.target(f, c, run)?;
                if !run {
                    return Ok(Value::Uninitialized);
                }
                match found {
                    Some(r) => {
                        self.store(f, &t, r)?;
                        Ok(self.truth(true))
                    }
                    None => Ok(Value::Integer(0)),
                }
            }
            // LoadTable, Load, Unload
            0x1F => {
                for _ in 0..6 {
                    self.term(f, c, run)?;
                }
                if run { Err(Error::Unsupported("loading tables")) } else { Ok(Value::Uninitialized) }
            }
            0x20 => {
                c.name_string()?;
                self.target(f, c, run)?;
                if run { Err(Error::Unsupported("loading tables")) } else { Ok(Value::Uninitialized) }
            }
            0x2A => {
                self.target(f, c, run)?;
                if run { Err(Error::Unsupported("unloading tables")) } else { Ok(Value::Uninitialized) }
            }
            // Stall, Sleep: nothing to wait for.
            0x21 | 0x22 => {
                self.term(f, c, run)?;
                Ok(Value::Uninitialized)
            }
            // Acquire (always acquired), Wait (always signalled)
            0x23 => {
                self.target(f, c, run)?;
                c.word()?;
                Ok(Value::Integer(0))
            }
            0x25 => {
                self.target(f, c, run)?;
                self.term(f, c, run)?;
                Ok(Value::Integer(0))
            }
            // Signal, Reset, Release
            0x24 | 0x26 | 0x27 => {
                self.target(f, c, run)?;
                Ok(Value::Uninitialized)
            }
            // FromBCD, ToBCD
            0x28 | 0x29 => {
                let a = self.term(f, c, run)?;
                let t = self.target(f, c, run)?;
                if !run {
                    return Ok(Value::Uninitialized);
                }
                let a = int(&a)?;
                let r = Value::Integer(if op == 0x28 { from_bcd(a) } else { to_bcd(a) });
                self.store_result(f, &t, r)
            }
            0x30 => Ok(Value::Integer(REVISION)),
            0x31 => Ok(Value::Uninitialized),
            // Fatal
            0x32 => {
                c.bytes(5)?;
                self.term(f, c, run)?;
                if run { Err(Error::Unsupported("Fatal")) } else { Ok(Value::Uninitialized) }
            }
            0x33 => Ok(Value::Integer(0)),
            _ => Err(Error::Opcode(0x5B00 | op as u16, at)),
        }
    }
}

/// A method's definitions' operands: evaluated there and then.
struct InMethod<'e, 'n> {
    ev: &'e mut Evaluator<'n>,
    f: &'e mut Frame,
}

impl Operands for InMethod<'_, '_> {
    fn scope(&self) -> &Path {
        &self.f.scope
    }

    fn resolve(&self, name: &NameString) -> Path {
        self.ev.lookup(self.f, name).unwrap_or_else(|| name.in_scope(&self.f.scope))
    }

    fn integers(&mut self, c: &mut Cursor, count: usize) -> Result<Integers, Error> {
        let mut operands = [0; 2];
        let mut known = true;
        for slot in operands.iter_mut().take(count) {
            match self.ev.term(self.f, c, true)?.as_integer() {
                Some(v) => *slot = v,
                None => known = false,
            }
        }
        Ok(if known { Integers::Known(operands) } else { Integers::Unknown })
    }

    fn buffer(&mut self, c: &mut Cursor) -> Result<Option<Place>, Error> {
        self.ev.operand_place(self.f, c, true).map(Some)
    }

    fn skip(&mut self, c: &mut Cursor) -> Result<(), Error> {
        self.ev.term(self.f, c, false).map(drop)
    }
}

fn reference_target(v: Value, run: bool) -> Result<Target, Error> {
    match v {
        Value::Element(place, i) => Ok(Target::Element(*place, i)),
        Value::Reference(p) => Ok(Target::Named(p)),
        _ if !run => Ok(Target::None),
        _ => Err(Error::Type("a reference")),
    }
}

/// `PNP0A03` or `PNP0A08`, a PCI root bridge's id, as a compressed EISA
/// id or a string, or among a package of ids (`_CID`).
fn is_pci_root_id(v: &Value) -> bool {
    match v {
        Value::Integer(i) => matches!(*i, 0x030A_D041 | 0x080A_D041),
        Value::String(s) => s == "PNP0A03" || s == "PNP0A08",
        Value::Package(ids) => ids.iter().any(is_pci_root_id),
        _ => false,
    }
}

/// A PCI `_ADR`: the device in the high word, the function in the low one
/// (`0xFFFF` for every function, which no configuration space is).
fn set_pci_address(function: &mut PciFunction, adr: u64) {
    function.device = (adr >> 16) as u8;
    function.function = (adr & 0xFFFF).min(0xFF) as u8;
}
