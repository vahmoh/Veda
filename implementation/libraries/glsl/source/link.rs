// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Linking: a vertex and a fragment shader into a program.
//!
//! The linker matches the stages' interfaces (varyings by name, type and
//! interpolation; uniforms and uniform blocks by name, which must agree in
//! both stages), assigns locations (attributes from `layout`, then
//! `glBindAttribLocation`, then the first free slots), lays out storage, and
//! builds the lists OpenGL ES reports: active attributes and uniforms (a
//! structure broken down to its members, an array listed once as
//! `name[0]`), uniform blocks with std140 offsets, fragment outputs and
//! transform feedback varyings. Implementation limits are enforced here,
//! before anything is listed: what a hostile shader declares is counted
//! from its types' sizes, never enumerated element by element.
//!
//! Storage. Uniforms of the default block live in *slots* of four 32-bit
//! words: every vector, matrix column, scalar and array element takes one
//! slot, in the order of a depth-first walk of the type ([`row_place`]).
//! Samplers take a slot too (holding the texture unit) and also get a
//! *sampler index*, which the code refers to ([`sampler_place`]). Uniform
//! blocks use the std140 layout whatever they declare (shared and packed
//! leave it to the implementation; [`std140_place`]). Varyings get vec4
//! *varying slots*, one per vector, matrix column, array element or
//! structure member ([`row_place`] too). The code generator places every
//! component with these functions, so that it reads what the API writes.
//!
//! The limits on uniforms and varyings are checked by the packing of GLSL ES
//! 1.00's appendix A ([`packs_into`]), which says which sets of variables an
//! implementation must accept; storage gives each row its own slot, so it
//! may take up to four times the rows the check counts.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;

use crate::builtins::{BuiltinVar, Limits};
use crate::diag::{Diagnostics, Loc, error};
use crate::hir::{BlockLayout, Shader, UniformBlock, VarId, VarKind};
use crate::types::{Basic, Element, Sampler, Scalar, StructId, Structs, Type};
use crate::{Stage, Version};

/// Program-wide state that linking reads: attribute bindings and the
/// transform feedback varyings the application asked for.
#[derive(Clone, Debug, Default)]
pub struct Bindings {
    /// `glBindAttribLocation(name, index)`.
    pub attributes: Vec<(String, u32)>,
    /// `glTransformFeedbackVaryings` names.
    pub feedback: Vec<String>,
    /// `GL_SEPARATE_ATTRIBS` (one buffer per varying) or interleaved.
    pub feedback_separate: bool,
}

/// An active vertex attribute.
#[derive(Clone, Debug)]
pub struct Attribute {
    pub name: String,
    pub ty: Type,
    /// The first location (matrices take one per column).
    pub location: u32,
    pub var: VarId,
}

/// An active uniform as `glGetActiveUniform` reports it: a basic type,
/// possibly an array (`size` elements, the name ending in `[0]`).
#[derive(Clone, Debug)]
pub struct Uniform {
    pub name: String,
    pub ty: Basic,
    /// Array length (1 for a single value).
    pub size: u32,
    pub is_array: bool,
    /// Default block: the first slot of element 0. Blocks: unused.
    pub slot: u32,
    /// For samplers: the sampler index of element 0.
    pub sampler: Option<u32>,
    /// The uniform block it belongs to, with its std140 placement.
    pub block: Option<BlockPlacement>,
    /// A built-in uniform (`gl_DepthRange.*`), filled in by the GL.
    pub builtin: bool,
}

/// Where a block member is in the block's buffer.
#[derive(Clone, Copy, Debug)]
pub struct BlockPlacement {
    pub block: u32,
    pub offset: u32,
    /// Bytes between array elements (0 if not an array).
    pub array_stride: u32,
    /// Bytes between matrix columns (rows if row-major), 0 if not a matrix.
    pub matrix_stride: u32,
    pub row_major: bool,
}

/// A uniform location: which active uniform, and which array element.
#[derive(Clone, Copy, Debug)]
pub struct Location {
    pub uniform: u32,
    pub element: u32,
}

/// An active uniform block.
#[derive(Clone, Debug)]
pub struct BlockInfo {
    pub name: String,
    /// The array element, for arrays of blocks (`name[2]`).
    pub element: Option<u32>,
    /// Bytes of buffer the block needs.
    pub size: u32,
    /// The active uniforms (indices into [`Linked::uniforms`]) it holds.
    pub uniforms: Vec<u32>,
    pub vertex: bool,
    pub fragment: bool,
}

/// A sampler: its type and the slot holding its texture unit.
#[derive(Clone, Copy, Debug)]
pub struct SamplerInfo {
    pub sampler: Sampler,
    pub slot: u32,
    pub vertex: bool,
    pub fragment: bool,
}

/// How a varying is interpolated.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Interpolation {
    Smooth,
    Flat,
    Centroid,
}

/// A varying passed from the vertex to the fragment shader.
#[derive(Clone, Debug)]
pub struct Varying {
    pub name: String,
    /// The vertex shader's type (a structure's id is the vertex shader's).
    pub ty: Type,
    pub interpolation: Interpolation,
    /// The first varying slot, and how many it takes.
    pub slot: u32,
    pub slots: u32,
}

/// A fragment output.
#[derive(Clone, Debug)]
pub struct Output {
    pub name: String,
    pub location: u32,
    /// Array outputs take consecutive locations.
    pub count: u32,
    /// `float`, `int` or `uint`, and components.
    pub scalar: Scalar,
    pub components: u8,
    /// GLSL ES 1.00's `gl_FragColor`: written to every draw buffer.
    pub broadcast: bool,
}

/// A varying captured by transform feedback.
#[derive(Clone, Debug)]
pub struct Feedback {
    pub name: String,
    pub ty: Basic,
    pub size: u32,
    /// The varying slot and component it starts at, and its components.
    pub slot: u32,
    pub components: u32,
    /// `gl_Position`.
    pub position: bool,
    /// `gl_PointSize`.
    pub point_size: bool,
}

/// Where one stage's variables are, for code generation.
#[derive(Clone, Debug, Default)]
pub struct StageLayout {
    /// Shader inputs: attribute locations (vertex) or varying slots
    /// (fragment).
    pub inputs: BTreeMap<VarId, u32>,
    /// Shader outputs: varying slots (vertex) or output locations
    /// (fragment).
    pub outputs: BTreeMap<VarId, u32>,
    /// Default-block uniforms: their first slot.
    pub uniforms: BTreeMap<VarId, u32>,
    /// Uniforms holding samplers: their first sampler index.
    pub samplers: BTreeMap<VarId, u32>,
    /// Uniform blocks: the shader's block index to the program's block
    /// index (of element 0 for arrays of blocks).
    pub blocks: BTreeMap<u32, u32>,
}

/// A linked program, before code generation.
#[derive(Debug)]
pub struct Linked {
    pub version: Version,
    pub attributes: Vec<Attribute>,
    pub uniforms: Vec<Uniform>,
    pub locations: Vec<Location>,
    /// Slots of default-block storage.
    pub slots: u32,
    pub blocks: Vec<BlockInfo>,
    pub samplers: Vec<SamplerInfo>,
    pub varyings: Vec<Varying>,
    /// Varying slots in use.
    pub varying_slots: u32,
    pub outputs: Vec<Output>,
    pub feedback: Vec<Feedback>,
    pub feedback_separate: bool,
    pub vertex: StageLayout,
    pub fragment: StageLayout,
    /// The vertex shader writes `gl_PointSize`.
    pub writes_point_size: bool,
}

// ---- Placement ---------------------------------------------------------------

/// Where flat component `c` of a value of type `t` is when every vector,
/// matrix column, scalar, sampler and array element takes a row of four
/// words of its own, in the order of a depth-first walk of the type
/// (default-block uniforms and varyings): its row, counting from the
/// value's first, and the component in the row.
pub fn row_place(t: Type, c: u32, structs: &Structs) -> Option<(u32, u8)> {
    let (mut ty, mut c, mut row) = (t, c, 0u32);
    loop {
        if ty.is_array() {
            let elem = ty.element_type();
            let per = elem.flat_components(structs);
            if per == 0 || c >= per.checked_mul(ty.elements())? {
                return None;
            }
            row = row.checked_add((c / per).checked_mul(elem.rows(structs))?)?;
            c %= per;
            ty = elem;
            continue;
        }
        match ty.element {
            Element::Basic(Basic::Matrix(cols, rows)) => {
                let rows = u32::from(rows);
                if c >= u32::from(cols) * rows {
                    return None;
                }
                return Some((row.checked_add(c / rows)?, (c % rows) as u8));
            }
            Element::Basic(b) => {
                return (c < b.components().max(1)).then_some((row, c as u8));
            }
            Element::Struct(id) => {
                let def = structs.get(id);
                let i = def.info.member_at(c)?;
                let start = def.info.members[i];
                c -= start.component;
                row = row.checked_add(start.row)?;
                ty = def.fields[i].ty;
            }
        }
    }
}

/// Whether flat component `c` of a value of type `t` is a sampler, and if
/// so how many samplers come before it in a depth-first walk of the type
/// (its sampler index, counting from the value's first).
pub fn sampler_place(t: Type, c: u32, structs: &Structs) -> Option<u32> {
    let (mut ty, mut c, mut index) = (t, c, 0u32);
    loop {
        if ty.is_array() {
            let elem = ty.element_type();
            let per = elem.flat_components(structs);
            if per == 0 || c >= per.checked_mul(ty.elements())? {
                return None;
            }
            index = index.checked_add((c / per).checked_mul(elem.sampler_count(structs))?)?;
            c %= per;
            ty = elem;
            continue;
        }
        match ty.element {
            Element::Basic(b) => return (b.is_sampler() && c == 0).then_some(index),
            Element::Struct(id) => {
                let def = structs.get(id);
                let i = def.info.member_at(c)?;
                let start = def.info.members[i];
                c -= start.component;
                index = index.checked_add(start.sampler)?;
                ty = def.fields[i].ty;
            }
        }
    }
}

/// The std140 byte offset of flat component `c` of a value of type `t`
/// (row-major matrices if `row_major`), from the value's start.
pub fn std140_place(t: Type, row_major: bool, c: u32, structs: &Structs) -> Option<u32> {
    let (mut ty, mut c, mut offset) = (t, c, 0u32);
    loop {
        if ty.is_array() {
            let elem = ty.element_type();
            let per = elem.flat_components(structs);
            if per == 0 || c >= per.checked_mul(ty.elements())? {
                return None;
            }
            let stride = ty.std140_stride(row_major, structs);
            offset = offset.checked_add((c / per).checked_mul(stride)?)?;
            c %= per;
            ty = elem;
            continue;
        }
        match ty.element {
            Element::Basic(Basic::Matrix(cols, rows)) => {
                let rows = u32::from(rows);
                if c >= u32::from(cols) * rows {
                    return None;
                }
                let (col, row) = (c / rows, c % rows);
                // Columns (rows if row-major) are vec4-aligned vectors.
                let at = if row_major { row * 16 + col * 4 } else { col * 16 + row * 4 };
                return offset.checked_add(at);
            }
            Element::Basic(b) => {
                return if c < b.components().max(1) { offset.checked_add(c * 4) } else { None };
            }
            Element::Struct(id) => {
                let def = structs.get(id);
                let i = def.info.member_at(c)?;
                let start = def.info.members[i];
                c -= start.component;
                offset = offset.checked_add(start.std140[usize::from(row_major)])?;
                ty = def.fields[i].ty;
            }
        }
    }
}

/// The std140 offsets of a uniform block's members (each with its own
/// matrix order), and the block's size: the end of its last member,
/// rounded up to a vec4 as a structure's is. Saturates at `u32::MAX`.
pub fn block_layout(b: &UniformBlock, structs: &Structs) -> (Vec<u32>, u32) {
    let mut offsets = Vec::with_capacity(b.members.len());
    let mut end = 0u32;
    for m in &b.members {
        let (align, size) = m.ty.std140(m.row_major, structs);
        let offset = end.checked_next_multiple_of(align.max(1)).unwrap_or(u32::MAX);
        offsets.push(offset);
        end = offset.saturating_add(size);
    }
    let size = end.checked_next_multiple_of(16).unwrap_or(u32::MAX).max(16);
    (offsets, size)
}

// ---- Packing -------------------------------------------------------------------

/// One kind of variable for [`packs_into`]: `count` variables of `rows`
/// rows (per element) of `width` components, with `elements` elements.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Packed {
    /// The order of GLSL ES 1.00's appendix A: mat4 and the like, mat2,
    /// vec4, mat3 and the like, vec3, vec2, scalars.
    order: u8,
    width: u32,
    rows: u32,
    elements: u32,
    count: u64,
}

impl Packed {
    /// Rows one of these variables takes.
    fn height(&self) -> u64 {
        u64::from(self.rows) * u64::from(self.elements)
    }
}

/// The packing class of a basic type: (order, width, rows), as ANGLE's
/// `VariablePacker` counts them (matrices that are not square take the
/// rows and columns of the next larger square one).
fn packing_class(b: Basic) -> Option<(u8, u32, u32)> {
    Some(match b {
        Basic::Matrix(4, _) | Basic::Matrix(_, 4) => (0, 4, 4),
        Basic::Matrix(2, 2) => (1, 4, 2),
        Basic::Vector(_, 4) => (2, 4, 1),
        Basic::Matrix(..) => (3, 3, 3),
        Basic::Vector(_, 3) => (4, 3, 1),
        Basic::Vector(_, 2) => (5, 2, 1),
        Basic::Scalar(_) => (6, 1, 1),
        _ => return None,
    })
}

/// Adds `v` to `out`, counting it with an equal kind already there.
fn add_kind(out: &mut Vec<Packed>, v: Packed) {
    let same = |p: &&mut Packed| (p.order, p.width, p.rows, p.elements) == (v.order, v.width, v.rows, v.elements);
    match out.iter_mut().find(same) {
        Some(p) => p.count = p.count.saturating_add(v.count),
        None => out.push(v),
    }
}

/// Adds the variables `count` values of type `t` amount to, structures
/// broken down to their members (samplers count for nothing). Each
/// structure's members are counted once, by kind: a structure nested in
/// another many times over is not walked each time.
fn add_packed(
    t: Type,
    count: u64,
    structs: &Structs,
    memo: &mut BTreeMap<StructId, Vec<Packed>>,
    out: &mut Vec<Packed>,
) {
    match t.element {
        Element::Basic(b) => {
            if let Some((order, width, rows)) = packing_class(b) {
                add_kind(out, Packed { order, width, rows, elements: t.elements(), count });
            }
        }
        Element::Struct(id) => {
            if !memo.contains_key(&id) {
                let mut kinds = Vec::new();
                for f in &structs.get(id).fields {
                    add_packed(f.ty, 1, structs, memo, &mut kinds);
                }
                memo.insert(id, kinds);
            }
            // Each element's members are variables of their own.
            let count = count.saturating_mul(u64::from(t.elements()));
            for &k in memo.get(&id).into_iter().flatten() {
                add_kind(out, Packed { count: k.count.saturating_mul(count), ..k });
            }
        }
    }
}

/// Whether values of these types (each with how many there are) fit in a
/// grid of `rows` rows of four components, packed as GLSL ES 1.00's
/// appendix A says (and ANGLE's `VariablePacker` does): every variable a
/// contiguous rectangle, an array's elements in successive rows of one
/// column group; whole rows first, then three columns, two, and one, in
/// that order. Structures are broken down to their members. A set this
/// accepts is one an implementation must accept.
pub fn packs_into(types: &[(Type, u32)], structs: &Structs, rows: u32) -> bool {
    let mut vars = Vec::new();
    let mut memo = BTreeMap::new();
    for &(t, count) in types {
        add_packed(t, u64::from(count), structs, &mut memo, &mut vars);
    }
    let rows64 = u64::from(rows);
    // A variable taller than the grid can never fit.
    if vars.iter().any(|v| v.count > 0 && v.height() > rows64) {
        return false;
    }
    vars.sort_by(|a, b| a.order.cmp(&b.order).then(b.elements.cmp(&a.elements)));
    let total = |width: u32| -> u64 {
        vars.iter().filter(|v| v.width == width).fold(0u64, |n, v| n.saturating_add(v.height().saturating_mul(v.count)))
    };
    // Whole rows from the top, then three columns of the rows after them.
    let top = total(4);
    let three = total(3);
    if top.saturating_add(three) > rows64 {
        return false;
    }
    let mut grid = alloc::vec![0u8; rows as usize];
    let fill = |grid: &mut [u8], from: u64, n: u64, mask: u8| {
        for r in &mut grid[from as usize..(from + n) as usize] {
            *r |= mask;
        }
    };
    fill(&mut grid, 0, top, 0b1111);
    fill(&mut grid, top, three, 0b0111);
    // Two columns: down the first two columns after those rows, then up
    // the last two from the bottom. Each variable takes a row at least,
    // so a hostile number of them fails after the grid's rows.
    let first2 = top + three;
    let available = rows64 - first2;
    let (mut free01, mut free23) = (available, available);
    for v in vars.iter().filter(|v| v.width == 2) {
        for _ in 0..v.count {
            let h = v.height();
            if h <= free01 {
                free01 -= h;
            } else if h <= free23 {
                free23 -= h;
            } else {
                return false;
            }
        }
    }
    fill(&mut grid, first2, available - free01, 0b0011);
    fill(&mut grid, rows64 - (available - free23), available - free23, 0b1100);
    // One column: each variable in the column whose free run that holds
    // it is the shortest, at that run's top. Four variables at most fit in
    // a row, so a hostile number of them fails after that many.
    for v in vars.iter().filter(|v| v.width == 1) {
        let h = v.height() as usize;
        for _ in 0..v.count {
            let mut best: Option<(usize, usize, usize)> = None; // (run, column, row)
            for col in 0..4 {
                let bit = 1u8 << col;
                let mut row = 0;
                while row < grid.len() {
                    if grid[row] & bit != 0 {
                        row += 1;
                        continue;
                    }
                    let start = row;
                    while row < grid.len() && grid[row] & bit == 0 {
                        row += 1;
                    }
                    let run = row - start;
                    if run >= h && best.is_none_or(|(b, _, _)| run < b) {
                        best = Some((run, col, start));
                    }
                }
            }
            let Some((_, col, start)) = best else { return false };
            for r in &mut grid[start..start + h] {
                *r |= 1 << col;
            }
        }
    }
    true
}

// ---- Linking -----------------------------------------------------------------

/// The result of linking: the program, or nothing and the log.
pub struct LinkResult {
    pub linked: Option<Linked>,
    pub log: String,
}

/// Links a vertex and a fragment shader.
pub fn link(vs: &Shader, fs: &Shader, bindings: &Bindings, limits: &Limits) -> LinkResult {
    let mut diags = Diagnostics::new();
    let linked = Linker { vs, fs, bindings, limits, diags: &mut diags }.run();
    let ok = !diags.has_errors();
    LinkResult { linked: if ok { linked } else { None }, log: diags.log() }
}

struct Linker<'a> {
    vs: &'a Shader,
    fs: &'a Shader,
    bindings: &'a Bindings,
    limits: &'a Limits,
    diags: &'a mut Diagnostics,
}

/// One stage's view of a uniform name.
#[derive(Clone, Copy)]
struct StageUniform {
    var: VarId,
    ty: Type,
}

impl Linker<'_> {
    fn err(&mut self, msg: String) {
        error!(self.diags, Loc::NONE, "{msg}");
    }

    fn run(&mut self) -> Option<Linked> {
        let (vs, fs) = (self.vs, self.fs);
        if vs.stage != Stage::Vertex || fs.stage != Stage::Fragment {
            self.err("a program needs a vertex shader and a fragment shader".into());
            return None;
        }
        if vs.version != fs.version {
            self.err("the vertex and fragment shaders are written in different GLSL ES versions".into());
            return None;
        }
        let mut out = Linked {
            version: vs.version,
            attributes: Vec::new(),
            uniforms: Vec::new(),
            locations: Vec::new(),
            slots: 0,
            blocks: Vec::new(),
            samplers: Vec::new(),
            varyings: Vec::new(),
            varying_slots: 0,
            outputs: Vec::new(),
            feedback: Vec::new(),
            feedback_separate: self.bindings.feedback_separate,
            vertex: StageLayout::default(),
            fragment: StageLayout::default(),
            writes_point_size: false,
        };
        self.attributes(&mut out);
        self.varyings(&mut out);
        self.uniforms(&mut out);
        self.blocks(&mut out);
        self.outputs(&mut out);
        self.feedback(&mut out);
        out.writes_point_size = vs.vars.iter().any(|v| v.kind == VarKind::Builtin(BuiltinVar::PointSize) && v.used);
        Some(out)
    }

    fn type_name(&self, s: &Shader, t: Type) -> String {
        let names = |n| -> String { s.name(n).into() };
        alloc::format!("{}", t.display(&s.structs, &names))
    }

    // ---- Attributes ----------------------------------------------------

    fn attributes(&mut self, out: &mut Linked) {
        let vs = self.vs;
        let max = self.limits.max_vertex_attribs;
        let mut taken = alloc::vec![false; max as usize];
        let mut pending = Vec::new();
        // The last binding of a name counts.
        let bound: BTreeMap<&str, u32> = self.bindings.attributes.iter().map(|(n, l)| (n.as_str(), *l)).collect();
        for (i, v) in vs.vars.iter().enumerate() {
            if v.kind != VarKind::Input || !v.used {
                continue;
            }
            let name = String::from(vs.name(v.name));
            let columns = v.ty.rows(&vs.structs);
            match v.location.or_else(|| bound.get(name.as_str()).copied()) {
                Some(l) => {
                    if l.checked_add(columns).is_none_or(|end| end > max) {
                        self.err(alloc::format!("attribute '{name}' at location {l} is beyond the {max} locations"));
                        continue;
                    }
                    for c in l..l + columns {
                        if taken[c as usize] && vs.version == Version::V300 {
                            self.err(alloc::format!("attribute '{name}' shares location {c} with another"));
                        }
                        taken[c as usize] = true;
                    }
                    out.attributes.push(Attribute { name, ty: v.ty, location: l, var: VarId(i as u32) });
                }
                None => pending.push((i, name, columns)),
            }
        }
        for (i, name, columns) in pending {
            let v = &vs.vars[i];
            let free = (0..max).find(|&l| {
                l.checked_add(columns).is_some_and(|end| end <= max) && (l..l + columns).all(|c| !taken[c as usize])
            });
            match free {
                Some(l) => {
                    for c in l..l + columns {
                        taken[c as usize] = true;
                    }
                    out.attributes.push(Attribute { name, ty: v.ty, location: l, var: VarId(i as u32) });
                }
                None => {
                    self.err(alloc::format!("too many vertex attributes (the limit is {max})"));
                    break;
                }
            }
        }
        out.attributes.sort_by_key(|a| a.location);
        for a in &out.attributes {
            out.vertex.inputs.insert(a.var, a.location);
        }
    }

    // ---- Varyings ------------------------------------------------------

    fn varyings(&mut self, out: &mut Linked) {
        let (vs, fs) = (self.vs, self.fs);
        let mut slot = 0u32;
        // The vertex shader's outputs by name.
        let outputs: BTreeMap<&str, usize> = vs
            .vars
            .iter()
            .enumerate()
            .filter(|(_, v)| v.kind == VarKind::Output)
            .map(|(i, v)| (vs.name(v.name), i))
            .collect();
        let mut matcher = TypeMatch::new(vs, fs, false);
        // What the fragment shader reads, for the limit.
        let mut read: Vec<(Type, u32)> = Vec::new();
        // Every fragment input the fragment shader uses needs a vertex
        // output of the same name and type.
        for (fi, f) in fs.vars.iter().enumerate() {
            if f.kind != VarKind::Input {
                continue;
            }
            let name = String::from(fs.name(f.name));
            let Some(&vi) = outputs.get(name.as_str()) else {
                if f.used {
                    self.err(alloc::format!("fragment shader input '{name}' is not declared by the vertex shader"));
                }
                continue;
            };
            let v = &vs.vars[vi];
            if !matcher.same(v.ty, f.ty) {
                let (a, b) = (self.type_name(vs, v.ty), self.type_name(fs, f.ty));
                self.err(alloc::format!(
                    "varying '{name}' is a '{a}' in the vertex shader and a '{b}' in the fragment shader"
                ));
                continue;
            }
            if vs.version == Version::V300 && v.interp != f.interp {
                self.err(alloc::format!("varying '{name}' has different interpolation qualifiers in the two shaders"));
                continue;
            }
            if f.invariant && !v.invariant {
                self.err(alloc::format!("varying '{name}' is invariant in the fragment shader only"));
                continue;
            }
            let slots = v.ty.rows(&vs.structs);
            let interpolation = match (f.interp, f.centroid || v.centroid) {
                (crate::ast::Interp::Flat, _) => Interpolation::Flat,
                (_, true) => Interpolation::Centroid,
                _ => Interpolation::Smooth,
            };
            out.varyings.push(Varying { name, ty: v.ty, interpolation, slot, slots });
            out.vertex.outputs.insert(VarId(vi as u32), slot);
            out.fragment.inputs.insert(VarId(fi as u32), slot);
            read.push((v.ty, 1));
            slot = slot.saturating_add(slots);
        }
        // Vertex outputs nothing reads still get slots (transform feedback
        // may capture them).
        for (vi, v) in vs.vars.iter().enumerate() {
            if v.kind == VarKind::Output && !out.vertex.outputs.contains_key(&VarId(vi as u32)) {
                let slots = v.ty.rows(&vs.structs);
                out.vertex.outputs.insert(VarId(vi as u32), slot);
                out.varyings.push(Varying {
                    name: String::from(vs.name(v.name)),
                    ty: v.ty,
                    interpolation: Interpolation::Smooth,
                    slot,
                    slots,
                });
                slot = slot.saturating_add(slots);
            }
        }
        // Built-in invariance rules (GLSL ES 1.00 4.6.4).
        let inv = |s: &Shader, b: BuiltinVar| s.vars.iter().any(|v| v.kind == VarKind::Builtin(b) && v.invariant);
        if inv(fs, BuiltinVar::FragCoord) && !inv(vs, BuiltinVar::Position) {
            self.err("gl_FragCoord is invariant but gl_Position is not".into());
        }
        if inv(fs, BuiltinVar::PointCoord) && !inv(vs, BuiltinVar::PointSize) {
            self.err("gl_PointCoord is invariant but gl_PointSize is not".into());
        }
        // The limit, counted as GLSL ES packs varyings into rows of four.
        let max = match vs.version {
            Version::V100 => self.limits.max_varying_vectors,
            Version::V300 => self.limits.max_vertex_output_vectors.min(self.limits.max_fragment_input_vectors),
        };
        if !packs_into(&read, &vs.structs, max) {
            self.err(alloc::format!("too many varyings (they do not fit in {max} vectors)"));
        }
        out.varying_slots = slot;
    }

    // ---- Uniforms ------------------------------------------------------

    fn uniforms(&mut self, out: &mut Linked) {
        let (vs, fs) = (self.vs, self.fs);
        // The limits first (in vectors, as GLSL ES counts them), from the
        // types alone: nothing is listed for a program that fails them.
        let mut fits = true;
        for (s, limit, what) in [
            (vs, self.limits.max_vertex_uniform_vectors, "vertex"),
            (fs, self.limits.max_fragment_uniform_vectors, "fragment"),
        ] {
            let used = || s.vars.iter().filter(|v| v.kind == VarKind::Uniform && v.used);
            let types: Vec<(Type, u32)> =
                used().filter(|v| !s.name(v.name).starts_with("gl_")).map(|v| (v.ty, 1)).collect();
            if !packs_into(&types, &s.structs, limit) {
                self.err(alloc::format!("too many uniforms in the {what} shader (they do not fit in {limit} vectors)"));
                fits = false;
            }
            let samplers = used().fold(0u32, |n, v| n.saturating_add(v.ty.sampler_count(&s.structs)));
            let max = if what == "vertex" {
                self.limits.max_vertex_texture_image_units
            } else {
                self.limits.max_texture_image_units
            };
            if samplers > max {
                self.err(alloc::format!("too many samplers in the {what} shader (the limit is {max})"));
                fits = false;
            }
        }
        if !fits {
            return;
        }
        // Default-block uniforms by name, in declaration order (vertex
        // shader first).
        let mut names: Vec<(String, Option<StageUniform>, Option<StageUniform>)> = Vec::new();
        let mut index: BTreeMap<&str, usize> = BTreeMap::new();
        for (stage, s) in [(0, vs), (1, fs)] {
            for (i, v) in s.vars.iter().enumerate() {
                if v.kind != VarKind::Uniform {
                    continue;
                }
                let name = s.name(v.name);
                let su = StageUniform { var: VarId(i as u32), ty: v.ty };
                match index.get(name) {
                    Some(&k) => {
                        if stage == 0 {
                            names[k].1 = Some(su);
                        } else {
                            names[k].2 = Some(su);
                        }
                    }
                    None => {
                        index.insert(name, names.len());
                        names.push((name.into(), (stage == 0).then_some(su), (stage == 1).then_some(su)));
                    }
                }
            }
        }
        let mut matcher = TypeMatch::new(vs, fs, true);
        let mut slot = 0u32;
        let mut sampler = 0u32;
        for (name, v, f) in names {
            let used_v = v.filter(|u| vs.vars[u.var.0 as usize].used);
            let used_f = f.filter(|u| fs.vars[u.var.0 as usize].used);
            if let (Some(a), Some(b)) = (v, f)
                && !matcher.same(a.ty, b.ty)
            {
                self.err(alloc::format!("uniform '{name}' has different types in the two shaders"));
                continue;
            }
            if let (Some(a), Some(b)) = (v, f) {
                let (pa, pb) = (vs.vars[a.var.0 as usize].precision, fs.vars[b.var.0 as usize].precision);
                if pa != pb && (used_v.is_some() && used_f.is_some()) {
                    self.err(alloc::format!("uniform '{name}' has different precisions in the two shaders"));
                    continue;
                }
            }
            if used_v.is_none() && used_f.is_none() {
                continue;
            }
            let (shader, u) = match (v, f) {
                (Some(a), _) => (vs, a),
                (None, Some(b)) => (fs, b),
                _ => continue,
            };
            let builtin = name.starts_with("gl_");
            let first_slot = slot;
            let first_sampler = sampler;
            let stages = (used_v.is_some(), used_f.is_some());
            self.flatten(&name, u.ty, shader, out, &mut slot, &mut sampler, builtin, stages);
            if let Some(a) = v {
                out.vertex.uniforms.insert(a.var, first_slot);
                if sampler > first_sampler {
                    out.vertex.samplers.insert(a.var, first_sampler);
                }
            }
            if let Some(b) = f {
                out.fragment.uniforms.insert(b.var, first_slot);
                if sampler > first_sampler {
                    out.fragment.samplers.insert(b.var, first_sampler);
                }
            }
        }
        out.slots = slot;
    }

    /// Lists the leaves of a uniform of type `t` named `name`, giving each
    /// its slots, samplers and locations (in the order of [`row_place`] and
    /// [`sampler_place`]). `stages` says which stages use it.
    #[allow(clippy::too_many_arguments)]
    fn flatten(
        &mut self,
        name: &str,
        t: Type,
        s: &Shader,
        out: &mut Linked,
        slot: &mut u32,
        sampler: &mut u32,
        builtin: bool,
        stages: (bool, bool),
    ) {
        match t.element {
            Element::Struct(id) => {
                let n = t.elements();
                for e in 0..n {
                    let base = if t.array.is_some() { alloc::format!("{name}[{e}]") } else { String::from(name) };
                    for f in &s.structs.get(id).fields {
                        let member = alloc::format!("{base}.{}", s.name(f.name));
                        self.flatten(&member, f.ty, s, out, slot, sampler, builtin, stages);
                    }
                }
            }
            Element::Basic(b) => {
                let size = t.elements();
                let is_array = t.array.is_some();
                let index = out.uniforms.len() as u32;
                let sampler_index = if let Basic::Sampler(smp) = b {
                    let first = *sampler;
                    for e in 0..size {
                        out.samplers.push(SamplerInfo {
                            sampler: smp,
                            slot: *slot + e,
                            vertex: stages.0,
                            fragment: stages.1,
                        });
                    }
                    *sampler += size;
                    Some(first)
                } else {
                    None
                };
                out.uniforms.push(Uniform {
                    name: if is_array { alloc::format!("{name}[0]") } else { String::from(name) },
                    ty: b,
                    size,
                    is_array,
                    slot: *slot,
                    sampler: sampler_index,
                    block: None,
                    builtin,
                });
                for e in 0..size {
                    out.locations.push(Location { uniform: index, element: e });
                }
                *slot += t.rows(&s.structs);
            }
        }
    }

    // ---- Uniform blocks ------------------------------------------------

    fn blocks(&mut self, out: &mut Linked) {
        let (vs, fs) = (self.vs, self.fs);
        let mut names: Vec<(String, Option<u32>, Option<u32>)> = Vec::new();
        let mut index: BTreeMap<&str, usize> = BTreeMap::new();
        for (stage, s) in [(0, vs), (1, fs)] {
            for (i, b) in s.blocks.iter().enumerate() {
                let name = s.name(b.name);
                match index.get(name) {
                    Some(&k) => {
                        if stage == 0 {
                            names[k].1 = Some(i as u32);
                        } else {
                            names[k].2 = Some(i as u32);
                        }
                    }
                    None => {
                        index.insert(name, names.len());
                        names.push((name.into(), (stage == 0).then_some(i as u32), (stage == 1).then_some(i as u32)));
                    }
                }
            }
        }
        // The blocks that are active, and the limits, before any is
        // listed.
        let mut matcher = TypeMatch::new(vs, fs, true);
        let mut active = Vec::new();
        let mut per_stage = [0u32; 2];
        let max_size = self.limits.max_uniform_block_size;
        let mut fits = true;
        for (name, v, f) in names {
            let vb = v.map(|i| &vs.blocks[i as usize]);
            let fb = f.map(|i| &fs.blocks[i as usize]);
            if let (Some(a), Some(b)) = (vb, fb)
                && !same_block(&mut matcher, a, b)
            {
                self.err(alloc::format!("uniform block '{name}' differs between the two shaders"));
                continue;
            }
            let (s, b) = match (vb, fb) {
                (Some(a), _) => (vs, a),
                (None, Some(b)) => (fs, b),
                _ => continue,
            };
            // std140 and shared blocks are active even if unused.
            let used = vb.is_some_and(|x| x.used) || fb.is_some_and(|x| x.used);
            if !used && b.layout == BlockLayout::Packed {
                continue;
            }
            let elements = b.array.unwrap_or(1);
            if v.is_some() {
                per_stage[0] = per_stage[0].saturating_add(elements);
            }
            if f.is_some() {
                per_stage[1] = per_stage[1].saturating_add(elements);
            }
            let (offsets, size) = block_layout(b, &s.structs);
            if size > max_size && fits {
                self.err(alloc::format!("uniform block '{name}' is larger than {max_size} bytes"));
                fits = false;
            }
            active.push((name, v, f, s, b, offsets, size));
        }
        let max = self.limits.max_uniform_blocks;
        for (n, what) in [(per_stage[0], "vertex"), (per_stage[1], "fragment")] {
            if n > max {
                self.err(alloc::format!("too many uniform blocks in the {what} shader (the limit is {max})"));
                fits = false;
            }
        }
        if !fits {
            return;
        }
        for (name, v, f, s, b, offsets, size) in active {
            let first = out.blocks.len() as u32;
            for e in 0..b.array.unwrap_or(1) {
                let index = out.blocks.len() as u32;
                let block_name = match b.array {
                    Some(_) => alloc::format!("{name}[{e}]"),
                    None => name.clone(),
                };
                let mut uniforms = Vec::new();
                for (m, &offset) in b.members.iter().zip(&offsets) {
                    let prefix = match b.instance {
                        Some(_) => alloc::format!("{name}.{}", s.name(m.name)),
                        None => String::from(s.name(m.name)),
                    };
                    self.block_leaves(&prefix, m.ty, m.row_major, offset, index, s, out, &mut uniforms);
                }
                out.blocks.push(BlockInfo {
                    name: block_name,
                    element: b.array.map(|_| e),
                    size,
                    uniforms,
                    vertex: v.is_some(),
                    fragment: f.is_some(),
                });
            }
            if let Some(i) = v {
                out.vertex.blocks.insert(i, first);
            }
            if let Some(i) = f {
                out.fragment.blocks.insert(i, first);
            }
        }
    }

    /// Lists a block member's leaves as active uniforms.
    #[allow(clippy::too_many_arguments)]
    fn block_leaves(
        &mut self,
        name: &str,
        t: Type,
        row_major: bool,
        offset: u32,
        block: u32,
        s: &Shader,
        out: &mut Linked,
        list: &mut Vec<u32>,
    ) {
        match t.element {
            Element::Struct(id) => {
                let stride = t.std140_stride(row_major, &s.structs);
                let def = s.structs.get(id);
                for e in 0..t.elements() {
                    let base = match t.array {
                        Some(_) => alloc::format!("{name}[{e}]"),
                        None => String::from(name),
                    };
                    let element_offset = offset + e * stride;
                    for (f, start) in def.fields.iter().zip(&def.info.members) {
                        let member = alloc::format!("{base}.{}", s.name(f.name));
                        let o = element_offset + start.std140[usize::from(row_major)];
                        self.block_leaves(&member, f.ty, row_major, o, block, s, out, list);
                    }
                }
            }
            Element::Basic(b) => {
                let array_stride = if t.array.is_some() { t.std140_stride(row_major, &s.structs) } else { 0 };
                let matrix_stride = if b.is_matrix() { 16 } else { 0 };
                list.push(out.uniforms.len() as u32);
                out.uniforms.push(Uniform {
                    name: if t.array.is_some() { alloc::format!("{name}[0]") } else { String::from(name) },
                    ty: b,
                    size: t.elements(),
                    is_array: t.array.is_some(),
                    slot: 0,
                    sampler: None,
                    block: Some(BlockPlacement {
                        block,
                        offset,
                        array_stride,
                        matrix_stride,
                        row_major: row_major && b.is_matrix(),
                    }),
                    builtin: false,
                });
            }
        }
    }

    // ---- Fragment outputs ----------------------------------------------

    fn outputs(&mut self, out: &mut Linked) {
        let fs = self.fs;
        let max = self.limits.max_draw_buffers;
        let mut taken = alloc::vec![false; max as usize];
        for (i, v) in fs.vars.iter().enumerate() {
            let (location, count, broadcast) = match v.kind {
                VarKind::Output => (v.location.unwrap_or(0), v.ty.elements(), false),
                VarKind::Builtin(BuiltinVar::FragColor) if v.used => (0, 1, true),
                VarKind::Builtin(BuiltinVar::FragData) if v.used => (0, v.ty.elements(), false),
                _ => continue,
            };
            let name = String::from(fs.name(v.name));
            if location.checked_add(count).is_none_or(|end| end > max) {
                self.err(alloc::format!("output '{name}' is beyond the {max} draw buffers"));
                continue;
            }
            for l in location..location + count {
                if taken[l as usize] {
                    self.err(alloc::format!("output '{name}' shares location {l} with another output"));
                }
                taken[l as usize] = true;
            }
            let b = v.ty.element_type().as_basic().unwrap_or(Basic::Vector(Scalar::Float, 4));
            out.outputs.push(Output {
                name,
                location,
                count,
                scalar: b.scalar().unwrap_or(Scalar::Float),
                components: b.components() as u8,
                broadcast,
            });
            out.fragment.outputs.insert(VarId(i as u32), location);
        }
    }

    // ---- Transform feedback --------------------------------------------

    fn feedback(&mut self, out: &mut Linked) {
        let vs = self.vs;
        let names = &self.bindings.feedback;
        let outputs: BTreeMap<&str, usize> = vs
            .vars
            .iter()
            .enumerate()
            .filter(|(_, v)| v.kind == VarKind::Output)
            .map(|(i, v)| (vs.name(v.name), i))
            .collect();
        let mut seen = BTreeSet::new();
        let mut twice = false;
        let mut total = 0u32;
        for n in names {
            twice |= !seen.insert(n.as_str());
            // `name`, `name[i]`, or a structure's member: `name.member`.
            let (path, element) = match n.find('[') {
                Some(i) if n.ends_with(']') => (&n[..i], n[i + 1..n.len() - 1].parse::<u32>().ok()),
                _ => (n.as_str(), None),
            };
            if n.contains('[') && element.is_none() {
                self.err(alloc::format!("transform feedback varying '{n}' is not a valid name"));
                continue;
            }
            if path == "gl_Position" || path == "gl_PointSize" {
                let position = path == "gl_Position";
                let (ty, comps) = if position { (Basic::Vector(Scalar::Float, 4), 4) } else { (Basic::FLOAT, 1) };
                out.feedback.push(Feedback {
                    name: n.clone(),
                    ty,
                    size: 1,
                    slot: 0,
                    components: comps,
                    position,
                    point_size: !position,
                });
                total = total.saturating_add(comps);
                continue;
            }
            let (base, member) = match path.split_once('.') {
                Some((b, m)) => (b, Some(m)),
                None => (path, None),
            };
            let Some(&vi) = outputs.get(base) else {
                self.err(alloc::format!("transform feedback varying '{n}' is not an output of the vertex shader"));
                continue;
            };
            let v = &vs.vars[vi];
            let Some(&first_slot) = out.vertex.outputs.get(&VarId(vi as u32)) else { continue };
            // A member of a structure output: its type and first slot.
            let (ty, slot) = match (member, v.ty.as_struct()) {
                (None, _) => (v.ty, first_slot),
                (Some(m), Some(id)) => {
                    let def = vs.structs.get(id);
                    match vs.interner.lookup(m).and_then(|m| def.field(m)) {
                        Some(k) => (def.fields[k].ty, first_slot + def.info.members[k].row),
                        None => {
                            self.err(alloc::format!("transform feedback varying '{n}' is not an output member"));
                            continue;
                        }
                    }
                }
                (Some(_), None) => {
                    self.err(alloc::format!("transform feedback varying '{n}' is not a structure's member"));
                    continue;
                }
            };
            let Some(b) = ty.element_type().as_basic() else {
                self.err(alloc::format!("transform feedback varying '{n}' is a structure: name its members"));
                continue;
            };
            let per = ty.element_type().rows(&vs.structs);
            let (first, count) = match (ty.array, element) {
                (Some(len), Some(e)) if e < len => (e, 1),
                (Some(_), Some(e)) => {
                    self.err(alloc::format!("transform feedback varying '{n}': index {e} is out of range"));
                    continue;
                }
                (None, Some(_)) => {
                    self.err(alloc::format!("transform feedback varying '{n}' is not an array"));
                    continue;
                }
                (len, None) => (0, len.unwrap_or(1)),
            };
            let components = b.components() * count;
            out.feedback.push(Feedback {
                name: n.clone(),
                ty: b,
                size: count,
                slot: slot + first * per,
                components,
                position: false,
                point_size: false,
            });
            total = total.saturating_add(components);
            if self.bindings.feedback_separate && components > self.limits.max_transform_feedback_separate_components {
                self.err(alloc::format!("transform feedback varying '{n}' has too many components"));
            }
        }
        if twice {
            self.err("a transform feedback varying is listed twice".into());
        }
        if self.bindings.feedback_separate {
            if out.feedback.len() as u32 > self.limits.max_transform_feedback_separate_attribs {
                self.err("too many separate transform feedback varyings".into());
            }
        } else if total > self.limits.max_transform_feedback_interleaved_components {
            self.err("too many interleaved transform feedback components".into());
        }
    }
}

/// Compares types of the two shaders: structures by name and members
/// (they are different types in each shader), each pair of structures
/// once, however often they nest in each other.
struct TypeMatch<'s> {
    a: &'s Shader,
    b: &'s Shader,
    /// Members' precisions must agree too (uniforms; varyings need not).
    precision: bool,
    known: BTreeMap<(StructId, StructId), bool>,
}

impl<'s> TypeMatch<'s> {
    fn new(a: &'s Shader, b: &'s Shader, precision: bool) -> TypeMatch<'s> {
        TypeMatch { a, b, precision, known: BTreeMap::new() }
    }

    fn same(&mut self, ta: Type, tb: Type) -> bool {
        if ta.array != tb.array {
            return false;
        }
        match (ta.element, tb.element) {
            (Element::Basic(x), Element::Basic(y)) => x == y,
            (Element::Struct(x), Element::Struct(y)) => {
                if let Some(&r) = self.known.get(&(x, y)) {
                    return r;
                }
                let (a, b) = (self.a, self.b);
                let (sx, sy) = (a.structs.get(x), b.structs.get(y));
                let r = sx.name.map(|n| a.name(n)) == sy.name.map(|n| b.name(n))
                    && sx.fields.len() == sy.fields.len()
                    && sx.fields.iter().zip(&sy.fields).all(|(f, g)| {
                        a.name(f.name) == b.name(g.name)
                            && (!self.precision || f.precision == g.precision)
                            && self.same(f.ty, g.ty)
                    });
                self.known.insert((x, y), r);
                r
            }
            _ => false,
        }
    }
}

fn same_block(m: &mut TypeMatch<'_>, x: &UniformBlock, y: &UniformBlock) -> bool {
    let (a, b) = (m.a, m.b);
    x.array == y.array
        && x.layout == y.layout
        && x.instance.map(|n| a.name(n)) == y.instance.map(|n| b.name(n))
        && x.members.len() == y.members.len()
        && x.members
            .iter()
            .zip(&y.members)
            .all(|(p, q)| a.name(p.name) == b.name(q.name) && p.row_major == q.row_major && m.same(p.ty, q.ty))
}
