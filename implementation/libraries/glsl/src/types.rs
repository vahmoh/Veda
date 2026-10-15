// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The language's types.
//!
//! [`Basic`] is a type a keyword names: `void`, scalars, vectors, matrices
//! (of `float` only) and samplers. [`Type`] adds structures and arrays
//! (one dimension: neither GLSL ES 1.00 nor 3.00 has arrays of arrays).
//! Precision qualifiers are not part of a type's identity; they travel
//! beside it.
//!
//! Sizes and layouts are found once per structure, when it is defined
//! ([`StructInfo`]), so that every question about a type takes constant
//! time, however deeply structures nest: a hostile shader nesting forty
//! structures of two members each would otherwise make each question walk
//! 2^40 members. The semantic checks reject types of more than
//! [`MAX_COMPONENTS`] components and structures nested more than
//! [`MAX_STRUCT_DEPTH`] deep, so that no size computed for a type comes
//! near overflowing 32 bits.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use crate::intern::Symbol;

/// A scalar type.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Scalar {
    Float,
    Int,
    Uint,
    Bool,
}

impl Scalar {
    pub fn name(self) -> &'static str {
        match self {
            Scalar::Float => "float",
            Scalar::Int => "int",
            Scalar::Uint => "uint",
            Scalar::Bool => "bool",
        }
    }

    /// The prefix of this scalar's vector types (`vec`, `ivec`...).
    fn vec_prefix(self) -> &'static str {
        match self {
            Scalar::Float => "vec",
            Scalar::Int => "ivec",
            Scalar::Uint => "uvec",
            Scalar::Bool => "bvec",
        }
    }

    /// Whether arithmetic applies (not `bool`).
    pub fn is_numeric(self) -> bool {
        self != Scalar::Bool
    }

    /// `int` or `uint`.
    pub fn is_integer(self) -> bool {
        matches!(self, Scalar::Int | Scalar::Uint)
    }
}

/// What a sampler's coordinates address.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Dim {
    D2,
    D3,
    Cube,
    D2Array,
}

/// A sampler type: its dimensionality, whether it compares depth (a
/// shadow sampler) and the type of the values it returns.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Sampler {
    pub dim: Dim,
    pub shadow: bool,
    /// `Float`, `Int` (`isampler`) or `Uint` (`usampler`).
    pub ty: Scalar,
}

impl Sampler {
    pub const fn new(dim: Dim, shadow: bool, ty: Scalar) -> Sampler {
        Sampler { dim, shadow, ty }
    }

    /// Components of the texture coordinate without projection or
    /// comparison reference: 2 for 2D, 3 for 3D, cube and 2D arrays.
    pub fn coords(self) -> u8 {
        match self.dim {
            Dim::D2 => 2,
            Dim::D3 | Dim::Cube | Dim::D2Array => 3,
        }
    }

    pub fn name(self) -> &'static str {
        use Dim::*;
        match (self.ty, self.dim, self.shadow) {
            (Scalar::Float, D2, false) => "sampler2D",
            (Scalar::Float, D3, false) => "sampler3D",
            (Scalar::Float, Cube, false) => "samplerCube",
            (Scalar::Float, D2Array, false) => "sampler2DArray",
            (Scalar::Float, D2, true) => "sampler2DShadow",
            (Scalar::Float, Cube, true) => "samplerCubeShadow",
            (Scalar::Float, D2Array, true) => "sampler2DArrayShadow",
            (Scalar::Int, D2, _) => "isampler2D",
            (Scalar::Int, D3, _) => "isampler3D",
            (Scalar::Int, Cube, _) => "isamplerCube",
            (Scalar::Int, D2Array, _) => "isampler2DArray",
            (Scalar::Uint, D2, _) => "usampler2D",
            (Scalar::Uint, D3, _) => "usampler3D",
            (Scalar::Uint, Cube, _) => "usamplerCube",
            (Scalar::Uint, D2Array, _) => "usampler2DArray",
            _ => "sampler",
        }
    }
}

/// A type named by a keyword.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Basic {
    Void,
    Scalar(Scalar),
    /// A vector of 2 to 4 components.
    Vector(Scalar, u8),
    /// A `float` matrix: columns, then rows (2 to 4 each).
    Matrix(u8, u8),
    Sampler(Sampler),
}

impl Basic {
    pub const FLOAT: Basic = Basic::Scalar(Scalar::Float);
    pub const INT: Basic = Basic::Scalar(Scalar::Int);
    pub const UINT: Basic = Basic::Scalar(Scalar::Uint);
    pub const BOOL: Basic = Basic::Scalar(Scalar::Bool);

    /// A scalar (`n == 1`) or vector of `n` components.
    pub fn vector(s: Scalar, n: u8) -> Basic {
        if n == 1 { Basic::Scalar(s) } else { Basic::Vector(s, n) }
    }

    /// The scalar type of a scalar, vector or matrix's components.
    pub fn scalar(self) -> Option<Scalar> {
        match self {
            Basic::Scalar(s) | Basic::Vector(s, _) => Some(s),
            Basic::Matrix(..) => Some(Scalar::Float),
            _ => None,
        }
    }

    /// Number of scalar components (0 for void and samplers).
    pub fn components(self) -> u32 {
        match self {
            Basic::Scalar(_) => 1,
            Basic::Vector(_, n) => n as u32,
            Basic::Matrix(c, r) => c as u32 * r as u32,
            _ => 0,
        }
    }

    pub fn is_scalar(self) -> bool {
        matches!(self, Basic::Scalar(_))
    }

    pub fn is_matrix(self) -> bool {
        matches!(self, Basic::Matrix(..))
    }

    pub fn is_sampler(self) -> bool {
        matches!(self, Basic::Sampler(_))
    }

    /// Scalars and vectors: what an `if`, swizzle or component-wise
    /// built-in function works on.
    pub fn is_scalar_or_vector(self) -> bool {
        matches!(self, Basic::Scalar(_) | Basic::Vector(..))
    }

    /// The type's name as written in a shader.
    pub fn name(self) -> String {
        match self {
            Basic::Void => "void".into(),
            Basic::Scalar(s) => s.name().into(),
            Basic::Vector(s, n) => alloc::format!("{}{}", s.vec_prefix(), n),
            Basic::Matrix(c, r) if c == r => alloc::format!("mat{c}"),
            Basic::Matrix(c, r) => alloc::format!("mat{c}x{r}"),
            Basic::Sampler(s) => s.name().into(),
        }
    }
}

/// A precision qualifier.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Precision {
    Low,
    Medium,
    High,
}

impl Precision {
    pub fn name(self) -> &'static str {
        match self {
            Precision::Low => "lowp",
            Precision::Medium => "mediump",
            Precision::High => "highp",
        }
    }
}

/// The most scalar components (a sampler counting as one) a type may have:
/// as many as the largest array a shader may declare, of `float`.
pub const MAX_COMPONENTS: u32 = 1 << 16;

/// The deepest nesting of structures (a structure with a member of
/// structure type is one deeper than that member's) accepted.
pub const MAX_STRUCT_DEPTH: u32 = 32;

/// The most components a variable private to an invocation (a global or
/// local variable, a parameter, a function's result) may have: 64 kB, as
/// ANGLE allows. Each is a value of its own in the compiled code, and an
/// index computed at run time chooses among all of them.
pub const MAX_PRIVATE_COMPONENTS: u32 = 1 << 14;

/// Index of a structure type in [`Structs`].
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct StructId(pub u32);

/// A structure's member.
#[derive(Clone, PartialEq, Debug)]
pub struct Field {
    pub name: Symbol,
    pub ty: Type,
    pub precision: Option<Precision>,
}

/// A structure type. Two structures are the same type only if they are
/// the same definition (name equivalence), except across shader stages,
/// where linking compares names and members.
#[derive(Clone, PartialEq, Debug)]
pub struct StructDef {
    /// `None` for an anonymous structure.
    pub name: Option<Symbol>,
    pub fields: Vec<Field>,
    /// Its sizes, found from its members' when it was defined.
    pub info: StructInfo,
    /// Each member's index by name (the first's, should two share a name,
    /// which the semantic checks report), so that finding a member does
    /// not take time in proportion to their number.
    by_name: BTreeMap<Symbol, u32>,
}

impl StructDef {
    /// The index of the member called `name`.
    pub fn field(&self, name: Symbol) -> Option<usize> {
        self.by_name.get(&name).map(|&k| k as usize)
    }
}

/// What a structure's members add up to, and where each member starts.
/// Sizes saturate at `u32::MAX` (a structure that large is an error, which
/// the semantic checks report).
#[derive(Clone, PartialEq, Debug)]
pub struct StructInfo {
    /// Scalar components, a sampler counting as one.
    pub components: u32,
    /// Rows of four 32-bit words, when every vector, matrix column, scalar
    /// and sampler takes one ([`Type::rows`]).
    pub rows: u32,
    /// Samplers.
    pub samplers: u32,
    /// A member is or holds an array.
    pub contains_array: bool,
    /// 1 if no member is a structure, else one more than the deepest
    /// member's.
    pub depth: u32,
    /// The std140 base alignment and size, with column-major and with
    /// row-major matrices.
    pub std140: [(u32, u32); 2],
    /// Where each member starts: its first component, row and sampler, and
    /// its std140 offsets in bytes (column-major, row-major).
    pub members: Vec<MemberStart>,
}

/// Where a structure's member starts (see [`StructInfo`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct MemberStart {
    pub component: u32,
    pub row: u32,
    pub sampler: u32,
    pub std140: [u32; 2],
}

impl StructInfo {
    fn of(fields: &[Field], structs: &Structs) -> StructInfo {
        let mut info = StructInfo {
            components: 0,
            rows: 0,
            samplers: 0,
            contains_array: false,
            depth: 1,
            std140: [(16, 0); 2],
            members: Vec::with_capacity(fields.len()),
        };
        let mut ends = [0u32; 2];
        for f in fields {
            let mut start =
                MemberStart { component: info.components, row: info.rows, sampler: info.samplers, std140: [0; 2] };
            info.components = info.components.saturating_add(f.ty.flat_components(structs));
            info.rows = info.rows.saturating_add(f.ty.rows(structs));
            info.samplers = info.samplers.saturating_add(f.ty.sampler_count(structs));
            info.contains_array |= f.ty.contains_array(structs);
            if let Element::Struct(id) = f.ty.element {
                info.depth = info.depth.max(structs.get(id).info.depth.saturating_add(1));
            }
            for (rm, end) in ends.iter_mut().enumerate() {
                let (align, size) = f.ty.std140(rm == 1, structs);
                info.std140[rm].0 = info.std140[rm].0.max(align);
                start.std140[rm] = round_up(*end, align);
                *end = start.std140[rm].saturating_add(size);
            }
            info.members.push(start);
        }
        for (rm, end) in ends.into_iter().enumerate() {
            info.std140[rm].1 = round_up(end, 16);
        }
        info
    }

    /// The member holding flat component `c`, if one does.
    pub fn member_at(&self, c: u32) -> Option<usize> {
        if c >= self.components {
            return None;
        }
        self.members.partition_point(|m| m.component <= c).checked_sub(1)
    }
}

/// `x` rounded up to a multiple of `align` (a power of two), saturating.
fn round_up(x: u32, align: u32) -> u32 {
    x.checked_next_multiple_of(align.max(1)).unwrap_or(u32::MAX)
}

/// The structures a shader defines.
#[derive(Clone, Default, Debug)]
pub struct Structs {
    pub defs: Vec<StructDef>,
}

impl Structs {
    /// Defines a structure from its members (all of types defined before).
    pub fn add(&mut self, name: Option<Symbol>, fields: Vec<Field>) -> StructId {
        let info = StructInfo::of(&fields, self);
        let mut by_name = BTreeMap::new();
        for (k, f) in fields.iter().enumerate() {
            by_name.entry(f.name).or_insert(k as u32);
        }
        self.defs.push(StructDef { name, fields, info, by_name });
        StructId(self.defs.len() as u32 - 1)
    }

    pub fn get(&self, id: StructId) -> &StructDef {
        &self.defs[id.0 as usize]
    }
}

/// What a value's type is, apart from arrays.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Element {
    Basic(Basic),
    Struct(StructId),
}

/// A complete type: a basic type or structure, possibly an array of them.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Type {
    pub element: Element,
    /// The array's length, if the type is an array.
    pub array: Option<u32>,
}

impl Type {
    pub const VOID: Type = Type::basic(Basic::Void);
    pub const FLOAT: Type = Type::basic(Basic::FLOAT);
    pub const INT: Type = Type::basic(Basic::INT);
    pub const UINT: Type = Type::basic(Basic::UINT);
    pub const BOOL: Type = Type::basic(Basic::BOOL);

    pub const fn basic(b: Basic) -> Type {
        Type { element: Element::Basic(b), array: None }
    }

    pub const fn structure(id: StructId) -> Type {
        Type { element: Element::Struct(id), array: None }
    }

    pub fn vector(s: Scalar, n: u8) -> Type {
        Type::basic(Basic::vector(s, n))
    }

    pub fn array_of(self, len: u32) -> Type {
        Type { element: self.element, array: Some(len) }
    }

    /// The type of one element of an array (the type itself otherwise).
    pub fn element_type(self) -> Type {
        Type { element: self.element, array: None }
    }

    pub fn is_array(self) -> bool {
        self.array.is_some()
    }

    /// The basic type, if not an array or structure.
    pub fn as_basic(self) -> Option<Basic> {
        match (self.element, self.array) {
            (Element::Basic(b), None) => Some(b),
            _ => None,
        }
    }

    pub fn as_struct(self) -> Option<StructId> {
        match (self.element, self.array) {
            (Element::Struct(s), None) => Some(s),
            _ => None,
        }
    }

    pub fn is_void(self) -> bool {
        self == Type::VOID
    }

    /// Whether the type is or contains a sampler.
    pub fn contains_sampler(self, structs: &Structs) -> bool {
        self.sampler_count(structs) > 0
    }

    /// Whether the type is or contains an array.
    pub fn contains_array(self, structs: &Structs) -> bool {
        self.array.is_some()
            || match self.element {
                Element::Basic(_) => false,
                Element::Struct(id) => structs.get(id).info.contains_array,
            }
    }

    /// The number of elements: the array's length, or 1.
    pub fn elements(self) -> u32 {
        self.array.unwrap_or(1)
    }

    /// Scalar components of one element (arrays and structures flattened;
    /// samplers count as one).
    pub fn element_components(self, structs: &Structs) -> u32 {
        match self.element {
            Element::Basic(Basic::Sampler(_)) => 1,
            Element::Basic(b) => b.components(),
            Element::Struct(id) => structs.get(id).info.components,
        }
    }

    /// Scalar components in the whole value (arrays and structures
    /// flattened; samplers count as one), saturating.
    pub fn flat_components(self, structs: &Structs) -> u32 {
        self.element_components(structs).saturating_mul(self.elements())
    }

    /// Rows of four 32-bit words the value takes when every vector, matrix
    /// column, scalar and sampler takes one (how default-block uniforms
    /// and varyings are stored), saturating.
    pub fn rows(self, structs: &Structs) -> u32 {
        let one = match self.element {
            Element::Basic(Basic::Matrix(c, _)) => u32::from(c),
            Element::Basic(_) => 1,
            Element::Struct(id) => structs.get(id).info.rows,
        };
        one.saturating_mul(self.elements())
    }

    /// Samplers the value holds, saturating.
    pub fn sampler_count(self, structs: &Structs) -> u32 {
        let one = match self.element {
            Element::Basic(b) => u32::from(b.is_sampler()),
            Element::Struct(id) => structs.get(id).info.samplers,
        };
        one.saturating_mul(self.elements())
    }

    /// The structures nested in the type: 0 for a basic type.
    pub fn struct_depth(self, structs: &Structs) -> u32 {
        match self.element {
            Element::Basic(_) => 0,
            Element::Struct(id) => structs.get(id).info.depth,
        }
    }

    /// The std140 base alignment and size of the value (row-major matrices
    /// if `row_major`), saturating.
    pub fn std140(self, row_major: bool, structs: &Structs) -> (u32, u32) {
        let (align, size) = match self.element {
            Element::Basic(b) => match b {
                Basic::Scalar(_) => (4, 4),
                Basic::Vector(_, 2) => (8, 8),
                Basic::Vector(_, 3) => (16, 12),
                Basic::Vector(_, _) => (16, 16),
                Basic::Matrix(c, r) => {
                    // An array of columns (rows if row-major), each a
                    // vec4-aligned vector.
                    let n = if row_major { r } else { c };
                    (16, 16 * u32::from(n))
                }
                _ => (4, 4),
            },
            Element::Struct(id) => structs.get(id).info.std140[usize::from(row_major)],
        };
        match self.array {
            Some(n) => {
                let stride = round_up(size, 16).max(round_up(align, 16));
                (16.max(align), stride.saturating_mul(n))
            }
            None => (align, size),
        }
    }

    /// The std140 stride of the elements of an array of this type's
    /// element type.
    pub fn std140_stride(self, row_major: bool, structs: &Structs) -> u32 {
        let (align, size) = self.element_type().std140(row_major, structs);
        round_up(size, 16).max(round_up(align, 16))
    }

    /// The basic type of flat component `c` of the value (a sampler for
    /// its one component), if there is one.
    pub fn component_basic(self, mut c: u32, structs: &Structs) -> Option<Basic> {
        let mut ty = self;
        loop {
            let per = ty.element_components(structs);
            if c >= per.saturating_mul(ty.elements()) {
                return None;
            }
            c %= per.max(1);
            match ty.element {
                Element::Basic(b) => return Some(b),
                Element::Struct(id) => {
                    let def = structs.get(id);
                    let i = def.info.member_at(c)?;
                    c -= def.info.members[i].component;
                    ty = def.fields[i].ty;
                }
            }
        }
    }

    /// The type's name as written in a shader.
    pub fn display<'a>(&'a self, structs: &'a Structs, names: &'a dyn Fn(Symbol) -> String) -> TypeDisplay<'a> {
        TypeDisplay { ty: self, structs, names }
    }
}

/// Formats a [`Type`] for messages.
pub struct TypeDisplay<'a> {
    ty: &'a Type,
    structs: &'a Structs,
    names: &'a dyn Fn(Symbol) -> String,
}

impl fmt::Display for TypeDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.ty.element {
            Element::Basic(b) => f.write_str(&b.name())?,
            Element::Struct(id) => match self.structs.get(id).name {
                Some(n) => f.write_str(&(self.names)(n))?,
                None => f.write_str("<anonymous struct>")?,
            },
        }
        if let Some(n) = self.ty.array {
            write!(f, "[{n}]")?;
        }
        Ok(())
    }
}
