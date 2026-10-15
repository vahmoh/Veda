// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! AML: loading definition blocks into a namespace, initialising its
//! devices, and evaluating its objects.
//!
//! **Loading** ([`Namespace::load`]) walks a table's terms and records the
//! objects they define: scopes, devices, methods (whose bodies run when they
//! are evaluated), names with their data, operation regions and their
//! fields, buffer fields (`CreateDWordField` outside a method: a field of
//! a named buffer). An `If` outside any method defines what is inside it
//! only when its condition holds, as the firmware means it (that is how a
//! board leaves out the devices its settings turn off); a condition that
//! cannot be evaluated drops the block, counted in [`Namespace::skipped`].
//! Other code outside methods is skipped rather than run.
//!
//! **Initialising** ([`Namespace::initialize`]) runs the devices' `_INI`
//! methods, as an operating system does once the tables are loaded: what
//! they store (a touchpad's `_HID`, its I2C address and GPIO pin, written
//! into its resource templates) is what every later evaluation starts
//! from.
//!
//! **Evaluating** ([`Namespace::evaluate`]) reads a name or runs a method
//! with a small interpreter: integer, string, buffer and package
//! operations, buffer fields, control flow, method calls, and the fields
//! of `SystemMemory` and `PCI_Config` regions, read through [`Memory`]
//! with the accesses their access types say. It changes nothing outside
//! itself: stores to the namespace's names, fields and buffers are kept
//! for the rest of that evaluation only, and what would act on the machine
//! (`Notify`, `Sleep`, `Acquire`) does nothing. Regions in other address
//! spaces (I/O ports, the embedded controller) are not read.
//!
//! Every evaluation is bounded: in steps, in the bytes of the values it
//! makes and copies, in how deep its code and its values nest, and in
//! nested calls. A load and an initialisation are bounded as wholes too,
//! and so are the evaluations that answer one of [`crate::device`]'s
//! questions; an owner may bound a phase of its evaluations together
//! ([`Namespace::limit`]).
//!
//! The work is divided between submodules: `code` reads AML (and frames an
//! `If` and its `Else` for loading and evaluating alike), `define` parses
//! the definitions that loading and evaluating both meet, `load` loads,
//! `init` initialises, `eval` evaluates, and `convert` holds what the
//! operators make of values.

mod code;
mod convert;
mod define;
mod eval;
mod init;
mod load;

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::Cell;
use core::fmt;

use crate::name::{NameString, Path};
use crate::tables::Table;
use crate::value::{Place, Value};
use crate::{Memory, PciFunction};
use code::Cursor;
use eval::{Budget, Evaluator};
use load::Unresolved;

/// Steps one evaluation may take: terms evaluated and statements run (a
/// loop that never ends stops here).
const MAX_STEPS: u64 = 1_000_000;
/// Bytes of values one evaluation may make or copy.
const MAX_BYTES: u64 = 16 << 20;
/// What loading one table, or initialising the devices, may spend in all.
const LOAD_STEPS: u64 = 4 * MAX_STEPS;
const LOAD_BYTES: u64 = 4 * MAX_BYTES;
/// What the evaluations that answer one question may spend together.
const QUESTION_STEPS: u64 = 4 * MAX_STEPS;
const QUESTION_BYTES: u64 = 4 * MAX_BYTES;
/// Nested method calls.
const MAX_CALLS: u32 = 32;
/// How deep code may nest (definitions within scopes being loaded, and the
/// terms, statements and calls of the code they evaluate, all counted
/// together) and values may (packages within packages): what loading and
/// evaluating need of the stack stays bounded (some 250 KiB at most, in an
/// optimised build), and so does what copying and dropping a value does.
const MAX_NESTING: u32 = 256;
/// Elements a package, or bytes a buffer or string, may have.
const MAX_ELEMENTS: usize = 1 << 16;
const MAX_BUFFER: usize = 1 << 20;
/// What `Revision` answers.
const REVISION: u64 = 0x2024_0322;

/// Why loading or evaluating failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The code ends early, or a length points past its end (at this
    /// offset in the table's AML).
    Truncated(usize),
    /// A malformed name at this offset.
    BadName(usize),
    /// An opcode that is unknown, or that cannot appear where it does
    /// (`0x5Bxx` for extended opcodes), at this offset.
    Opcode(u16, usize),
    /// Something the interpreter does not do.
    Unsupported(&'static str),
    /// A name with no object.
    NoSuchName(Path),
    /// An operand of the wrong type.
    Type(&'static str),
    /// Firmware memory that could not be read.
    Memory(u64),
    /// A PCI function's configuration space that could not be read (at
    /// this offset).
    PciConfig(PciFunction, u16),
    /// Too many steps or bytes, calls or code nested too deep, or a value
    /// too large or nested too deep.
    Limit,
    DivideByZero,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Truncated(at) => write!(f, "the code ends early (at {:#x})", at),
            Error::BadName(at) => write!(f, "a malformed name at {:#x}", at),
            Error::Opcode(op, at) => write!(f, "opcode {:#x} at {:#x}", op, at),
            Error::Unsupported(what) => write!(f, "{} is not supported", what),
            Error::NoSuchName(p) => write!(f, "no object {}", p),
            Error::Type(what) => write!(f, "{} expected", what),
            Error::Memory(a) => write!(f, "firmware memory at {:#x} cannot be read", a),
            Error::PciConfig(function, offset) => {
                write!(f, "the configuration space of PCI function {} cannot be read at {:#x}", function, offset)
            }
            Error::Limit => f.write_str("the evaluation went beyond its bounds (of time, memory or nesting)"),
            Error::DivideByZero => f.write_str("division by zero"),
        }
    }
}

/// Where code is: in which loaded table, between which offsets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Code {
    pub table: usize,
    pub start: usize,
    pub end: usize,
}

/// An object in the namespace.
#[derive(Debug, Clone)]
pub enum Object {
    /// A scope with no object of its own (`\_SB`).
    Scope,
    Device,
    Processor,
    PowerResource,
    ThermalZone,
    Method {
        args: u8,
        code: Code,
    },
    Name(Value),
    /// An operation region: its address space (0 memory, 1 I/O ports,
    /// 2 PCI configuration space...) and where it is.
    Region {
        space: u8,
        bounds: Bounds,
    },
    Field(Field),
    /// A field of a buffer (`CreateDWordField`): the buffer, and where its
    /// bits are in it.
    BufferField {
        place: Place,
        bits: Bits,
    },
    Mutex,
    Event,
    Alias(Path),
    /// `\_OSI`, which the interpreter answers itself.
    Osi,
}

/// Where an operation region is.
#[derive(Debug, Clone)]
pub enum Bounds {
    Known {
        offset: u64,
        length: u64,
    },
    /// Evaluated when a field of the region is read, as its address may
    /// depend on objects a later table defines: the code of its offset and
    /// length, and the scope it is in.
    Deferred {
        scope: Path,
        code: Code,
    },
    Unknown,
}

/// Where a buffer field's bits are in its buffer.
#[derive(Debug, Clone)]
pub enum Bits {
    Known {
        /// Its first bit.
        offset: u64,
        /// How many bits it has.
        width: u64,
    },
    /// Evaluated when the field is used, as its offset may depend on
    /// objects a later table defines.
    Deferred {
        /// The scope its definition is in.
        scope: Path,
        /// The code of its definition's operands (its offset, and its
        /// width if it is a `CreateField`).
        code: Code,
        kind: FieldKind,
    },
}

/// What a buffer field's definition makes of its operands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    /// `CreateBitField`: a bit at a bit offset.
    Bit,
    /// `CreateByteField`: 8 bits at a byte offset.
    Byte,
    /// `CreateWordField`: 16 bits at a byte offset.
    Word,
    /// `CreateDWordField`: 32 bits at a byte offset.
    DWord,
    /// `CreateQWordField`: 64 bits at a byte offset.
    QWord,
    /// `CreateField`: a bit offset and a width in bits.
    Bits,
}

impl FieldKind {
    /// How many integer operands its definition has.
    fn operands(self) -> usize {
        if self == FieldKind::Bits { 2 } else { 1 }
    }

    /// The field's first bit and width, from its definition's operands. An
    /// offset beyond 64 bits saturates: such a field is outside any buffer.
    fn bits(self, operands: [u64; 2]) -> (u64, u64) {
        let bytes = |at: u64| at.saturating_mul(8);
        match self {
            FieldKind::Bit => (operands[0], 1),
            FieldKind::Byte => (bytes(operands[0]), 8),
            FieldKind::Word => (bytes(operands[0]), 16),
            FieldKind::DWord => (bytes(operands[0]), 32),
            FieldKind::QWord => (bytes(operands[0]), 64),
            FieldKind::Bits => (operands[0], operands[1]),
        }
    }
}

/// A named field of an operation region.
#[derive(Debug, Clone)]
pub struct Field {
    pub unit: FieldUnit,
    pub bit_offset: u64,
    pub bit_width: u64,
    /// Bits per access (8, 16, 32 or 64).
    pub access_bits: u32,
}

/// What a field is a field of.
#[derive(Debug, Clone)]
pub enum FieldUnit {
    Region(Path),
    /// An index/data pair (`IndexField`): not read by the interpreter.
    Index,
    /// A banked region (`BankField`): not read by the interpreter.
    Bank,
}

/// What initialising the devices did ([`Namespace::initialize`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Initialized {
    /// The `_INI` methods that ran.
    pub ran: usize,
    /// How many `_INI` and `_STA` methods failed.
    pub failures: usize,
    /// The first of them, and why.
    pub failed: Vec<(Path, Error)>,
}

/// The ACPI namespace: every object the loaded tables define.
pub struct Namespace {
    code: Vec<Arc<[u8]>>,
    objects: BTreeMap<Path, Object>,
    /// Methods declared `External` (defined in another table), with their
    /// argument counts: needed to parse calls to them.
    externals: BTreeMap<Path, u8>,
    /// Integers are 32 bits wide under a DSDT of revision 1.
    integer_bits: u32,
    /// Conditional definitions dropped because their condition could not
    /// be evaluated (and buffer fields whose buffer could not be).
    pub skipped: usize,
    /// Why the first of them were (the scope and the error).
    pub skip_reasons: Vec<(Path, Error)>,
    /// Names whose data refers to objects not defined when they were (a
    /// package naming a device further on, or in a later table): they are
    /// evaluated again once a table has loaded.
    unresolved: Vec<Unresolved>,
    /// The devices, processors and thermal zones (and `\_SB`, `\_TZ`) in
    /// the order they were defined, as ACPICA initialises them.
    order: BTreeMap<Path, u32>,
    /// The namespace's names, fields and buffers as the devices'
    /// initialisation stored them: what every evaluation starts from.
    initial: BTreeMap<Path, Value>,
    /// What all evaluations together may still spend, while the owner
    /// bounds them ([`Namespace::limit`]).
    bound: Budget<'static>,
    bounded: Cell<bool>,
}

impl Default for Namespace {
    fn default() -> Self {
        Self::new()
    }
}

impl Namespace {
    /// An empty namespace with the objects every one has.
    pub fn new() -> Namespace {
        let mut ns = Namespace {
            code: Vec::new(),
            objects: BTreeMap::new(),
            externals: BTreeMap::new(),
            integer_bits: 64,
            skipped: 0,
            skip_reasons: Vec::new(),
            unresolved: Vec::new(),
            order: BTreeMap::new(),
            initial: BTreeMap::new(),
            bound: Budget::new(u64::MAX, u64::MAX, None),
            bounded: Cell::new(false),
        };
        ns.objects.insert(Path::root(), Object::Scope);
        for s in ["_GPE", "_PR", "_SB", "_SI", "_TZ"] {
            ns.objects.insert(Path::root().join(s).unwrap(), Object::Scope);
        }
        // ACPICA has these two be devices: they are initialised as devices
        // are.
        for s in ["_SB", "_TZ"] {
            let order = ns.order.len() as u32;
            ns.order.insert(Path::root().join(s).unwrap(), order);
        }
        ns.objects
            .insert(Path::root().join("_OS").unwrap(), Object::Name(Value::String("Microsoft Windows NT".into())));
        ns.objects.insert(Path::root().join("_REV").unwrap(), Object::Name(Value::Integer(2)));
        ns.objects.insert(Path::root().join("_GL").unwrap(), Object::Mutex);
        ns.objects.insert(Path::root().join("_OSI").unwrap(), Object::Osi);
        ns
    }

    /// Loads a DSDT or SSDT. A DSDT of revision 1 makes integers 32 bits
    /// wide.
    pub fn load_table(&mut self, table: &Table, memory: &dyn Memory) -> Result<(), Error> {
        if table.is(b"DSDT") && table.data.get(8).is_some_and(|&revision| revision < 2) {
            self.integer_bits = 32;
        }
        self.load(table.aml(), memory)
    }

    /// The object at `path` (aliases followed).
    pub fn get(&self, path: &Path) -> Option<&Object> {
        let mut path = path;
        for _ in 0..8 {
            match self.objects.get(path)? {
                Object::Alias(target) => path = target,
                other => return Some(other),
            }
        }
        None
    }

    /// `path`, or where the aliases it names lead.
    fn canonical<'a>(&'a self, path: &'a Path) -> &'a Path {
        let mut p = path;
        for _ in 0..8 {
            match self.objects.get(p) {
                Some(Object::Alias(target)) => p = target,
                _ => break,
            }
        }
        p
    }

    /// The objects directly below `path`.
    pub fn children<'a>(&'a self, path: &'a Path) -> impl Iterator<Item = (&'a Path, &'a Object)> + 'a {
        let depth = path.0.len();
        self.objects
            .range(path.clone()..)
            .skip(1)
            .take_while(move |(p, _)| p.0.starts_with(&path.0))
            .filter(move |(p, _)| p.0.len() == depth + 1)
    }

    /// Every device, in path order.
    pub fn devices(&self) -> impl Iterator<Item = &Path> {
        self.objects.iter().filter(|(_, o)| matches!(o, Object::Device)).map(|(p, _)| p)
    }

    /// Finds the object a name refers to from `scope`, searching the
    /// enclosing scopes for a single-segment name as AML does.
    pub fn resolve(&self, scope: &Path, name: &NameString) -> Option<Path> {
        code::search(scope, name, |p| self.objects.contains_key(p))
    }

    /// Reads the object at `path`, or calls it with `args` if it is a
    /// method.
    pub fn evaluate(&self, path: &Path, args: &[Value], memory: &dyn Memory) -> Result<Value, Error> {
        Evaluator::new(self, memory, &self.bound).evaluate(path, args.to_vec())
    }

    /// Calls method `first` with `first_args`, then evaluates `path` as
    /// [`Namespace::evaluate`] does, in the same evaluation: what the first
    /// stores holds for the second (`\_PIC (1)`, the interrupt model the
    /// firmware's `_PRT` methods answer for). Without a method `first`,
    /// just evaluates `path`.
    pub fn evaluate_after(
        &self,
        first: &Path,
        first_args: &[Value],
        path: &Path,
        args: &[Value],
        memory: &dyn Memory,
    ) -> Result<Value, Error> {
        Question::new(self, memory).evaluate_after(first, first_args, path, args)
    }

    /// Evaluates `name` (`"_HID"`) of `device`: `None` if it has none.
    pub fn evaluate_child(&self, device: &Path, name: &str, memory: &dyn Memory) -> Option<Result<Value, Error>> {
        let path = device.join(name)?;
        self.objects.contains_key(&path).then(|| self.evaluate(&path, &[], memory))
    }

    /// Bounds what every evaluation from now on spends, all of them
    /// together (as well as each on its own): `limit` is the steps and the
    /// bytes of values they may still take; `None` lifts the bound. For an
    /// owner's phase of work, such as a device manager's questions as it
    /// starts, so that a firmware whose objects each take long cannot make
    /// all of them take long.
    pub fn limit(&self, limit: Option<(u64, u64)>) {
        let (steps, bytes) = limit.unwrap_or((u64::MAX, u64::MAX));
        self.bound.set(steps, bytes);
        self.bounded.set(limit.is_some());
    }

    /// What is left of the bound [`Namespace::limit`] set, if one is set:
    /// steps and bytes.
    pub fn left(&self) -> Option<(u64, u64)> {
        self.bounded.get().then(|| self.bound.left())
    }

    fn ones(&self) -> u64 {
        if self.integer_bits == 32 { u32::MAX as u64 } else { u64::MAX }
    }

    /// Defines `object` at `path`. The first definition of a name stands
    /// (a scope opened before the object is replaced by it).
    fn define(&mut self, path: Path, object: Object) {
        match self.objects.get(&path) {
            None | Some(Object::Scope) => {
                if matches!(object, Object::Device | Object::Processor | Object::ThermalZone) {
                    let order = self.order.len() as u32;
                    self.order.entry(path.clone()).or_insert(order);
                }
                self.objects.insert(path, object);
            }
            Some(_) => {}
        }
    }
}

/// Evaluations that answer one question together (one of
/// [`crate::device`]'s: a device's ids, the PCI root bridges, where an
/// interrupt goes): they spend from one budget, as well as each from its
/// own.
pub(crate) struct Question<'n> {
    ns: &'n Namespace,
    memory: &'n dyn Memory,
    budget: Budget<'n>,
}

impl<'n> Question<'n> {
    pub(crate) fn new(ns: &'n Namespace, memory: &'n dyn Memory) -> Question<'n> {
        Question { ns, memory, budget: Budget::new(QUESTION_STEPS, QUESTION_BYTES, Some(&ns.bound)) }
    }

    /// As [`Namespace::evaluate`].
    pub(crate) fn evaluate(&self, path: &Path, args: &[Value]) -> Result<Value, Error> {
        Evaluator::new(self.ns, self.memory, &self.budget).evaluate(path, args.to_vec())
    }

    /// As [`Namespace::evaluate_after`].
    pub(crate) fn evaluate_after(
        &self,
        first: &Path,
        first_args: &[Value],
        path: &Path,
        args: &[Value],
    ) -> Result<Value, Error> {
        let mut ev = Evaluator::new(self.ns, self.memory, &self.budget);
        if let Some(Object::Method { .. }) = self.ns.get(first) {
            ev.evaluate(first, first_args.to_vec())?;
        }
        ev.evaluate(path, args.to_vec())
    }

    /// As [`Namespace::evaluate_child`].
    pub(crate) fn child(&self, device: &Path, name: &str) -> Option<Result<Value, Error>> {
        let path = device.join(name)?;
        self.ns.objects.contains_key(&path).then(|| self.evaluate(&path, &[]))
    }
}

/// The sleep type values (SLP_TYPa, SLP_TYPb) of system state `\_S<state>`
/// as a DSDT or SSDT defines them: `Name (\_S5, Package () { a, b, ... })`,
/// the name rooted or not, wherever the definition is. For the kernel,
/// which turns the machine off without loading the namespace (devmgr's
/// [`Namespace`] reads the object like any other). `None` if the table
/// does not define it with integer constants.
pub fn sleep_type(aml: &[u8], state: u8) -> Option<(u16, u16)> {
    let name = [b'_', b'S', b'0'.checked_add(state)?, b'_'];
    (0..aml.len()).find_map(|i| {
        let after = match &aml[i..] {
            [0x08, b'\\', n @ ..] if n.starts_with(&name) => i + 6,
            [0x08, n @ ..] if n.starts_with(&name) => i + 5,
            _ => return None,
        };
        let mut c = Cursor { code: aml, pos: after };
        if c.byte().ok()? != 0x12 {
            return None;
        }
        let end = c.pkg_end().ok()?;
        let _count = c.byte().ok()?;
        let mut c = Cursor { code: &aml[..end], pos: c.pos };
        let a = constant(&mut c)?;
        // A package of one element leaves SLP_TYPb at 0.
        let b = if c.pos < end { constant(&mut c)? } else { 0 };
        Some((a as u16, b as u16))
    })
}

/// An integer constant: Zero, One, Ones, or a byte, word, dword or qword.
fn constant(c: &mut Cursor) -> Option<u64> {
    Some(match c.byte().ok()? {
        0x00 => 0,
        0x01 => 1,
        0xFF => u64::MAX,
        0x0A => c.byte().ok()? as u64,
        0x0B => c.word().ok()? as u64,
        0x0C => c.dword().ok()? as u64,
        0x0E => c.qword().ok()?,
        _ => return None,
    })
}
