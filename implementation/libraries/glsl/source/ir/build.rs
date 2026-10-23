// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! From the checked shader to SSA.
//!
//! The builder walks `main` (and every function it calls, inlined) in
//! program order, keeping the current SSA value of every scalar component
//! of every variable in scope. Writes since the innermost open `if` or loop
//! began are kept in a journal, so that a branch's writes can be undone and
//! its state merged with the other's in time proportional to what they
//! wrote, not to every variable in scope. Merges place φ functions: after an
//! `if` where the branches disagree, at a loop's header for each variable
//! the loop may write (found from the checked shader before building), in
//! the block after a loop where its `break`s disagree. Variables are
//! forgotten when their scope ends.
//!
//! * Calls are inlined (GLSL ES has no recursion). A function whose returns
//!   are not all a final `return` is wrapped in a loop run once (with no φ
//!   functions: nothing goes back to its start): `return` breaks out of it,
//!   through enclosing loops by setting a flag that is tested after each of
//!   them.
//! * `switch` becomes a loop run once, whose clauses run while a
//!   fall-through flag is set; `continue` inside it breaks out and then
//!   continues the enclosing loop.
//! * `for` loops run their step at each `continue` and at the end of the
//!   body; `do`-`while` loops test their condition there.
//! * Inputs, uniforms and built-in inputs are loaded once, in the entry
//!   block; outputs are stored when `main` finishes. Every component is
//!   placed by [`crate::link`]'s placement functions, as the API stores it.
//! * An index that is not constant selects among an array's elements (a
//!   chain of selects for loads, one select per element for stores), except
//!   for uniforms and uniform blocks, which are loaded at a computed offset
//!   (clamped to the variable, so a stray index reads nothing else). An
//!   index that is constant only once calls are inlined was not checked
//!   against the array's length: it is clamped to the array, as the
//!   interpreter clamps an index computed at run time.
//! * Samplers are values holding their sampler index; a lookup whose index
//!   is constant (nearly always, after inlining) refers to it directly, one
//!   whose index is not to the run of samplers it may select.
//!
//! Building is bounded. Before it starts, the size and depth of `main` with
//! every call inlined are computed from the checked shader, bottom-up over
//! the calls, and a program over [`MAX_SIZE`] or [`MAX_DEPTH`] is refused
//! (calls nested `n` deep, each calling the next twice, make `2^n` copies);
//! building stops past [`MAX_INSTRUCTIONS`] instructions or [`MAX_WORK`]
//! steps of work, should that estimate fall short.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

use super::{Block, BuiltinIn, BuiltinOut, Func, IfNode, InstId, InstOp, Jump, LoopNode, Node, TexOp, Value};
use crate::Stage;
use crate::builtins::{Builtin, BuiltinVar, TexCall, TexLod};
use crate::hir::{self, BinOp, Expr, ExprKind as H, FuncId, LogicOp, ParamMode, Shader, Stmt, UnOp, VarId, VarKind};
use crate::link::{self, Linked, StageLayout};
use crate::lower::{self, Emit};
use crate::ops::{self, Op, Ty};
use crate::types::{Basic, Dim, Scalar, Structs, Type};

/// The most statements, expressions and calls nested in one another that
/// building goes through, once calls are inlined: each takes the builder a
/// level of recursion.
pub const MAX_DEPTH: u32 = 224;

/// The most statements and expressions `main` may come to with every call
/// inlined.
pub const MAX_SIZE: u64 = 1 << 20;

/// The most instructions building may create.
pub const MAX_INSTRUCTIONS: usize = 1 << 18;

/// The most steps of work building may take: statements and expressions
/// visited, instructions created, variable components merged or carried
/// into loops.
pub const MAX_WORK: u64 = 1 << 23;

/// The most components the variables private to an invocation (globals,
/// and the locals, parameters and results of the functions `main` calls)
/// may come to, all together: each is a value of its own in the compiled
/// code (each one's own size is bounded by
/// [`crate::types::MAX_PRIVATE_COMPONENTS`]).
pub const MAX_PRIVATE_TOTAL: u64 = 1 << 16;

/// Why a stage could not be built.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BuildError {
    /// Too much code once every call is inlined.
    TooLarge,
    /// Statements, expressions and calls nest too deeply once every call
    /// is inlined.
    TooDeep,
    /// Its private variables come to more than [`MAX_PRIVATE_TOTAL`]
    /// components.
    TooManyVariables,
}

impl fmt::Display for BuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BuildError::TooLarge => f.write_str("the program is too large once its function calls are inlined"),
            BuildError::TooDeep => {
                f.write_str("statements, expressions and function calls nest too deeply once calls are inlined")
            }
            BuildError::TooManyVariables => write!(
                f,
                "its global and local variables, parameters and results come to more than {MAX_PRIVATE_TOTAL} components"
            ),
        }
    }
}

/// A variable component's key: the variable, then the component.
type Key = (u32, u32);

/// The SSA state: every variable component's current value.
type Defs = BTreeMap<Key, Value>;

/// The keys written on a path since a point, with their values then (none
/// for a key forgotten).
type Changes = BTreeMap<Key, Option<Value>>;

/// What a loop context is for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LoopKind {
    Loop,
    Switch,
    Function,
}

/// What runs before a `continue` takes effect.
#[derive(Clone, Copy)]
enum Continue<'a> {
    None,
    /// A `for` loop's step.
    Step(&'a Expr),
    /// A `do`-`while` loop's condition (break if false).
    Test(&'a Expr),
}

struct LoopCtx<'a> {
    kind: LoopKind,
    /// The journal's length when the body began.
    mark: usize,
    breaks: Vec<Changes>,
    continues: Vec<Changes>,
    construct: Continue<'a>,
    /// For `Function`: the key of the return value and of the return flag.
    ret: Option<(u32, u32)>,
    /// For `Switch`: the key of the "continue the enclosing loop" flag.
    continue_flag: Option<u32>,
    /// A `return` happened inside this loop (so its exit tests the flag).
    returned: bool,
    /// A `continue` was turned into a flag inside this switch.
    continued: bool,
}

/// A loop being built ([`Builder::loop_begin`]).
struct LoopStart {
    /// The header's φ functions, by key.
    phis: Vec<(Key, InstId)>,
}

/// An `if` being built ([`Builder::if_begin`]).
struct IfStart {
    mode: IfMode,
    cond: Value,
    mark: usize,
    then: Option<Changes>,
    then_list: Vec<Node>,
}

/// How an `if` is built.
#[derive(Clone, Copy, PartialEq, Eq)]
enum IfMode {
    /// Unreachable: neither branch.
    Skip,
    /// A constant condition: only the branch it takes, in place.
    Constant(bool),
    /// Both branches, merged after.
    Branch,
}

/// Which loop variables get φ functions at the loop's header.
#[derive(Clone, Copy)]
enum Phis<'p> {
    /// None: the loop runs once (a switch, a function).
    None,
    /// Those of the variables the loop may write.
    Of(&'p LoopWrites),
    /// Every variable in scope.
    All,
}

/// An access path: the root variable and the steps (each with the type it
/// applies to).
type Path = (VarId, Vec<(Step, Type)>);

/// Where an out or inout argument goes back to once the call returns: an
/// access path (its indices evaluated before the call) and its type.
type Place = (VarId, Vec<(Step, Type)>, Type);

/// One step of an access path.
#[derive(Clone, Copy)]
enum Step {
    Field(u32),
    /// An index: constant, or an SSA value (an int or uint).
    Index(Option<u32>, Value),
    Swizzle(hir::Swizzle),
}

/// Builds the SSA form of a stage of a linked program.
pub fn build(shader: &Shader, layout: &StageLayout, linked: &Linked) -> Result<Func, BuildError> {
    let plan = Plan::of(shader)?;
    let mut b = Builder::new(shader, layout, linked, &plan);
    b.run();
    match b.failed {
        Some(e) => Err(e),
        None => Ok(b.func),
    }
}

// ---- The plan ----------------------------------------------------------------

/// What the checked shader says before anything is built: what each loop
/// may write.
struct Plan {
    /// By the loop's address in the checked shader.
    loops: BTreeMap<usize, LoopWrites>,
    /// Globals, outputs and built-in outputs written anywhere.
    globals: Vec<u32>,
}

/// What a loop may change (its φ functions).
#[derive(Default)]
struct LoopWrites {
    /// Variables declared outside the loop that it may write.
    vars: Vec<u32>,
    /// It calls functions, which may write globals and outputs.
    calls: bool,
    /// It returns from its function (writing the return value and flag).
    returns: bool,
}

/// What a walk over statements and expressions found.
#[derive(Default)]
struct Found {
    /// Statements and expressions, calls inlined.
    size: u64,
    writes: BTreeSet<u32>,
    declared: BTreeSet<u32>,
    calls: bool,
    returns: bool,
}

impl Found {
    fn absorb(&mut self, inner: Found) {
        self.size = self.size.saturating_add(inner.size);
        self.writes.extend(inner.writes);
        self.declared.extend(inner.declared);
        self.calls |= inner.calls;
        self.returns |= inner.returns;
    }
}

/// The address of a loop, its key in [`Plan::loops`].
fn loop_key(l: &hir::Loop) -> usize {
    l as *const hir::Loop as usize
}

/// The variable an l-value expression writes.
fn root(mut e: &Expr) -> Option<VarId> {
    loop {
        match &e.kind {
            H::Var(id) => return Some(*id),
            H::Field(b, _) | H::Swizzle(b, _) | H::Index(b, _) => e = b,
            _ => return None,
        }
    }
}

struct Planner<'a> {
    s: &'a Shader,
    /// Each function's size and depth with its calls inlined, once known.
    inlined: Vec<Option<(u64, u32)>>,
    loops: BTreeMap<usize, LoopWrites>,
    globals: BTreeSet<u32>,
    /// Components of the private variables declared so far.
    private: u64,
}

impl Plan {
    /// Walks every function `main` reaches, callees first; refuses a
    /// program too large or too deep once its calls are inlined.
    fn of(s: &Shader) -> Result<Plan, BuildError> {
        let n = s.functions.len();
        let mut p = Planner { s, inlined: vec![None; n], loops: BTreeMap::new(), globals: BTreeSet::new(), private: 0 };
        for v in s.vars.iter().filter(|v| v.kind == VarKind::Global) {
            p.private = p.private.saturating_add(u64::from(v.ty.flat_components(&s.structs)));
        }
        // Callees first: a depth-first walk of the calls, without recursion
        // (chains of calls may be long). Recursion was refused before.
        let mut order = Vec::new();
        if let Some(main) = s.main {
            let mut state = vec![0u8; n]; // 0 unvisited, 1 on the stack, 2 done
            let mut stack = vec![(main.0 as usize, 0usize)];
            state[main.0 as usize] = 1;
            while let Some(&mut (f, ref mut next)) = stack.last_mut() {
                let calls = &s.functions[f].calls;
                if let Some(c) = calls.get(*next) {
                    *next += 1;
                    let c = c.0 as usize;
                    if state[c] == 0 {
                        state[c] = 1;
                        stack.push((c, 0));
                    }
                } else {
                    state[f] = 2;
                    order.push(f);
                    stack.pop();
                }
            }
        }
        for f in order {
            let func = &s.functions[f];
            let Some(body) = &func.body else { continue };
            let mut found = Found::default();
            let depth = p.stmts(body, &mut found);
            p.inlined[f] = Some((found.size, depth));
            for t in func.params.iter().map(|&v| s.var(v).ty).chain([func.ret]) {
                p.private = p.private.saturating_add(u64::from(t.flat_components(&s.structs)));
            }
        }
        let mut found = Found::default();
        let init_depth = p.stmts(&s.init, &mut found);
        let (size, depth) = match s.main {
            Some(m) => p.inlined[m.0 as usize].unwrap_or((0, 0)),
            None => (0, 0),
        };
        if size.saturating_add(found.size) > MAX_SIZE {
            return Err(BuildError::TooLarge);
        }
        if p.private > MAX_PRIVATE_TOTAL {
            return Err(BuildError::TooManyVariables);
        }
        // `main` is inlined too (a level).
        if depth.saturating_add(1).max(init_depth) > MAX_DEPTH {
            return Err(BuildError::TooDeep);
        }
        Ok(Plan { loops: p.loops, globals: p.globals.into_iter().collect() })
    }
}

impl<'a> Planner<'a> {
    /// The deepest of a list of statements (each as deep as the builder
    /// recurses for it).
    fn stmts(&mut self, list: &'a [Stmt], f: &mut Found) -> u32 {
        list.iter().map(|s| self.stmt(s, f)).max().unwrap_or(0)
    }

    fn stmt(&mut self, s: &'a Stmt, f: &mut Found) -> u32 {
        f.size = f.size.saturating_add(1);
        let inner = match s {
            Stmt::Expr(e) => self.expr(e, f),
            Stmt::Decl(id, init) => {
                f.declared.insert(id.0);
                let v = self.s.var(*id);
                if v.kind == VarKind::Local {
                    self.private = self.private.saturating_add(u64::from(v.ty.flat_components(&self.s.structs)));
                }
                init.as_ref().map_or(0, |e| self.expr(e, f))
            }
            Stmt::Block(list) => self.stmts(list, f),
            Stmt::If(c, t, e) => self.expr(c, f).max(self.stmts(t, f)).max(self.stmts(e, f)),
            Stmt::Loop(l) => {
                let mut inner = Found::default();
                let mut d = 0;
                if let Some((v, init)) = &l.cond_var {
                    inner.declared.insert(v.0);
                    d = d.max(self.expr(init, &mut inner));
                }
                if let Some(c) = &l.cond {
                    d = d.max(self.expr(c, &mut inner));
                }
                if let Some(step) = &l.step {
                    d = d.max(self.expr(step, &mut inner));
                }
                d = d.max(self.stmts(&l.body, &mut inner));
                let vars = inner.writes.difference(&inner.declared).copied().collect();
                self.loops.insert(loop_key(l), LoopWrites { vars, calls: inner.calls, returns: inner.returns });
                f.absorb(inner);
                d
            }
            Stmt::Switch(sel, clauses) => {
                let mut d = self.expr(sel, f);
                for c in clauses {
                    d = d.max(self.stmts(&c.body, f));
                }
                d
            }
            Stmt::Return(v) => {
                f.returns = true;
                v.as_ref().map_or(0, |e| self.expr(e, f))
            }
            Stmt::Break | Stmt::Continue | Stmt::Discard => 0,
        };
        inner.saturating_add(1)
    }

    fn expr(&mut self, e: &'a Expr, f: &mut Found) -> u32 {
        f.size = f.size.saturating_add(1);
        let inner = match &e.kind {
            H::Const(_) | H::Var(_) => 0,
            H::Unary(_, a) | H::Swizzle(a, _) | H::Field(a, _) => self.expr(a, f),
            H::Binary(_, a, b) | H::Logic(_, a, b) | H::Index(a, b) | H::Sequence(a, b) => {
                self.expr(a, f).max(self.expr(b, f))
            }
            H::Ternary(c, a, b) => self.expr(c, f).max(self.expr(a, f)).max(self.expr(b, f)),
            H::Assign(t, v) | H::CompoundAssign(_, t, v) => {
                self.write(t, f);
                self.expr(t, f).max(self.expr(v, f))
            }
            H::IncDec { target, .. } => {
                self.write(target, f);
                self.expr(target, f)
            }
            H::Construct(args) => args.iter().map(|a| self.expr(a, f)).max().unwrap_or(0),
            H::Builtin(b, args) => {
                if *b == Builtin::Modf
                    && let Some(out) = args.get(1)
                {
                    self.write(out, f);
                }
                args.iter().map(|a| self.expr(a, f)).max().unwrap_or(0)
            }
            H::Call(id, args) => {
                f.calls = true;
                let s = self.s;
                let func = s.function(*id);
                for (p, a) in func.params.iter().zip(args) {
                    if matches!(s.var(*p).kind, VarKind::Param(ParamMode::Out | ParamMode::InOut)) {
                        self.write(a, f);
                    }
                }
                let d = args.iter().map(|a| self.expr(a, f)).max().unwrap_or(0);
                let (size, depth) = self.inlined.get(id.0 as usize).copied().flatten().unwrap_or((0, 0));
                f.size = f.size.saturating_add(size);
                // The callee's body is built a level deeper still.
                d.max(depth.saturating_add(1))
            }
        };
        inner.saturating_add(1)
    }

    fn write(&mut self, target: &Expr, f: &mut Found) {
        if let Some(v) = root(target) {
            f.writes.insert(v.0);
            if matches!(self.s.var(v).kind, VarKind::Global | VarKind::Output | VarKind::Builtin(_)) {
                self.globals.insert(v.0);
            }
        }
    }
}

// ---- The builder -------------------------------------------------------------

struct Builder<'a> {
    s: &'a Shader,
    structs: &'a Structs,
    layout: &'a StageLayout,
    linked: &'a Linked,
    plan: &'a Plan,
    func: Func,
    /// Where instructions go.
    cur: Block,
    entry: Block,
    /// Lists being built, innermost last.
    lists: Vec<Vec<Node>>,
    defs: Defs,
    /// The old values of the keys written since the outermost open `if` or
    /// loop began, the latest last (see [`Builder::mark`]).
    journal: Vec<(Key, Option<Value>)>,
    /// `if`s and loops open.
    open: u32,
    /// The current point cannot be reached (after a jump), or building
    /// has failed.
    dead: bool,
    loops: Vec<LoopCtx<'a>>,
    /// Loads cached in the entry block.
    cache: BTreeMap<CacheKey, Value>,
    /// Keys for temporaries (beyond the shader's variables).
    next_temp: u32,
    /// Constants already in the entry block, by type and bits.
    constants: BTreeMap<(Ty, u32), Value>,
    /// Uniform blocks' member offsets, by the shader's block index.
    block_offsets: BTreeMap<u32, Vec<u32>>,
    /// Steps of work done (see [`MAX_WORK`]).
    work: u64,
    /// Statements, expressions and calls being built, one in another.
    depth: u32,
    failed: Option<BuildError>,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum CacheKey {
    Input(u32, u8),
    Uniform(u32, u8),
    Block(u32, u32),
    Builtin(BuiltinIn),
}

impl Emit for Builder<'_> {
    type V = Value;

    fn konst(&mut self, v: ops::Value) -> Value {
        self.konst_value(v)
    }

    fn op(&mut self, op: Op, args: &[Value], ret: Ty) -> Value {
        // Fold constants as the code is built.
        let consts: Option<Vec<ops::Value>> = args.iter().map(|&a| self.func.constant(a)).collect();
        if let Some(c) = consts {
            return self.konst_value(ops::eval(op, &c, ret));
        }
        self.emit(InstOp::Op(op), args.to_vec(), ret)
    }

    fn select(&mut self, c: Value, a: Value, b: Value, ty: Ty) -> Value {
        if let Some(k) = self.func.constant(c) {
            return if k.bits() != 0 { a } else { b };
        }
        if a == b {
            return a;
        }
        self.emit(InstOp::Select, vec![c, a, b], ty)
    }
}

impl<'a> Builder<'a> {
    fn new(s: &'a Shader, layout: &'a StageLayout, linked: &'a Linked, plan: &'a Plan) -> Builder<'a> {
        let mut func = Func::new(s.stage);
        let entry = func.new_block();
        Builder {
            s,
            structs: &s.structs,
            layout,
            linked,
            plan,
            func,
            cur: entry,
            entry,
            lists: vec![vec![Node::Block(entry)]],
            defs: Defs::new(),
            journal: Vec::new(),
            open: 0,
            dead: false,
            loops: Vec::new(),
            cache: BTreeMap::new(),
            next_temp: s.vars.len() as u32,
            constants: BTreeMap::new(),
            block_offsets: BTreeMap::new(),
            work: 0,
            depth: 0,
            failed: None,
        }
    }

    // ---- Bounds ----------------------------------------------------------

    /// Stops building: nothing more is built, and [`build`] fails.
    fn fail(&mut self, e: BuildError) {
        if self.failed.is_none() {
            self.failed = Some(e);
        }
        self.dead = true;
    }

    /// Whether code can be built at the current point.
    fn live(&self) -> bool {
        !self.dead
    }

    /// Marks the current point unreachable or not (never reachable again
    /// once building has failed).
    fn set_dead(&mut self, dead: bool) {
        self.dead = dead || self.failed.is_some();
    }

    /// Counts `n` steps of work.
    fn charge(&mut self, n: u64) {
        self.work = self.work.saturating_add(n);
        if self.work > MAX_WORK {
            self.fail(BuildError::TooLarge);
        }
    }

    /// Goes a level deeper (a statement, an expression, a call), if the
    /// bound allows.
    fn enter(&mut self) -> bool {
        if self.depth >= MAX_DEPTH {
            self.fail(BuildError::TooDeep);
            return false;
        }
        self.depth += 1;
        true
    }

    fn leave(&mut self) {
        self.depth -= 1;
    }

    // ---- Instructions ----------------------------------------------------

    fn create(&mut self, op: InstOp, args: Vec<Value>, tys: &[Ty]) -> InstId {
        if self.func.insts.len() >= MAX_INSTRUCTIONS {
            self.fail(BuildError::TooLarge);
        }
        self.charge(1);
        self.func.create(op, args, tys)
    }

    fn emit(&mut self, op: InstOp, args: Vec<Value>, ty: Ty) -> Value {
        let id = self.create(op, args, &[ty]);
        self.func.push(self.cur, id);
        self.func.insts[id.0 as usize].results[0]
    }

    /// An instruction with no result (a store).
    fn emit_void(&mut self, op: InstOp, args: Vec<Value>) {
        let id = self.create(op, args, &[]);
        self.func.push(self.cur, id);
    }

    fn emit_multi(&mut self, op: InstOp, args: Vec<Value>, tys: &[Ty]) -> Vec<Value> {
        let id = self.create(op, args, tys);
        self.func.push(self.cur, id);
        self.func.insts[id.0 as usize].results.clone()
    }

    fn konst_value(&mut self, v: ops::Value) -> Value {
        // Constants live in the entry block (shared, and so they dominate
        // every use), once each.
        let key = (v.ty(), v.bits());
        if let Some(&c) = self.constants.get(&key) {
            return c;
        }
        let id = self.create(InstOp::Const(v), Vec::new(), &[v.ty()]);
        let entry = self.entry;
        self.func.push(entry, id);
        let c = self.func.insts[id.0 as usize].results[0];
        self.constants.insert(key, c);
        c
    }

    fn zero(&mut self, ty: Ty) -> Value {
        self.konst_value(ops::Value::from_bits(ty, 0))
    }

    fn int(&mut self, v: i32) -> Value {
        self.konst_value(ops::Value::I(v))
    }

    /// Zeros of every component of a value of type `t`.
    fn zeros(&mut self, t: Type) -> Vec<Value> {
        let mut tys = Vec::new();
        lower::component_types(t, self.structs, &mut tys);
        tys.iter().map(|&ty| self.zero(ty)).collect()
    }

    /// The scalar type of flat component `c` of a value of type `t` (a
    /// sampler is its sampler index).
    fn component_ty(&self, t: Type, c: u32) -> Ty {
        match t.component_basic(c, self.structs) {
            Some(Basic::Sampler(_)) => Ty::I32,
            Some(b) => b.scalar().map_or(Ty::F32, lower::ty),
            None => Ty::F32,
        }
    }

    /// A load placed in the entry block, emitted once.
    fn cached(&mut self, key: CacheKey, op: InstOp, ty: Ty) -> Value {
        if let Some(&v) = self.cache.get(&key) {
            return v;
        }
        let id = self.create(op, Vec::new(), &[ty]);
        let entry = self.entry;
        self.func.push(entry, id);
        let v = self.func.insts[id.0 as usize].results[0];
        self.cache.insert(key, v);
        v
    }

    fn temp(&mut self) -> u32 {
        self.next_temp = self.next_temp.saturating_add(1);
        self.next_temp - 1
    }

    // ---- State -------------------------------------------------------------

    /// Sets a variable component's value.
    fn set(&mut self, k: Key, v: Value) {
        let old = self.defs.insert(k, v);
        if self.open > 0 {
            self.journal.push((k, old));
        }
    }

    /// Forgets a variable component (its scope ended).
    fn unset(&mut self, k: Key) {
        let old = self.defs.remove(&k);
        if self.open > 0 && old.is_some() {
            self.journal.push((k, old));
        }
    }

    /// Forgets every component of variable (or temporary) `var`.
    fn forget(&mut self, var: u32) {
        let keys: Vec<Key> = self.defs.range((var, 0)..=(var, u32::MAX)).map(|(&k, _)| k).collect();
        self.charge(keys.len() as u64);
        for k in keys {
            self.unset(k);
        }
    }

    /// Forgets the local variables a list of statements declares, at the
    /// end of their scope.
    fn forget_locals(&mut self, list: &'a [Stmt]) {
        for st in list {
            if let Stmt::Decl(id, _) = st
                && self.s.var(*id).kind == VarKind::Local
            {
                self.forget(id.0);
            }
        }
    }

    /// Opens a point to come back to: the journal's length.
    fn mark(&mut self) -> usize {
        self.open += 1;
        self.journal.len()
    }

    /// The keys written since `mark` and their values now.
    fn changes(&mut self, mark: usize) -> Changes {
        let written = self.journal.get(mark..).unwrap_or(&[]);
        let mut out = Changes::new();
        for &(k, _) in written {
            out.entry(k).or_insert_with(|| self.defs.get(&k).copied());
        }
        self.charge(written.len() as u64);
        out
    }

    /// Undoes the writes made since `mark`.
    fn rollback(&mut self, mark: usize) {
        while self.journal.len() > mark {
            let Some((k, old)) = self.journal.pop() else { break };
            match old {
                Some(v) => self.defs.insert(k, v),
                None => self.defs.remove(&k),
            };
        }
    }

    /// Closes the point [`Self::mark`] opened (after rolling back to it).
    fn release(&mut self) {
        self.open = self.open.saturating_sub(1);
        if self.open == 0 {
            self.journal.clear();
        }
    }

    /// Merges states reaching `block` (each the changes from the current
    /// one, which is the state they all started from), with φ functions at
    /// `block` where they disagree. A key that did not exist before is
    /// local to the code that made it, and forgotten.
    fn merge(&mut self, block: Block, states: &[Changes]) {
        let keys: BTreeSet<Key> = states.iter().flat_map(|s| s.keys().copied()).collect();
        self.charge(keys.len() as u64 * states.len() as u64);
        for k in keys {
            let Some(&base) = self.defs.get(&k) else { continue };
            let vals: Vec<Value> = states.iter().map(|s| s.get(&k).copied().flatten().unwrap_or(base)).collect();
            let first = vals[0];
            if vals.iter().all(|&v| v == first) {
                self.set(k, first);
                continue;
            }
            let ty = self.func.ty(first);
            // φs go first in their block, which is new: in order.
            let id = self.create(InstOp::Phi, vals, &[ty]);
            self.func.blocks[block.0 as usize].insts.push(id);
            let v = self.func.insts[id.0 as usize].results[0];
            self.set(k, v);
        }
    }

    // ---- Structure -------------------------------------------------------

    /// Ends the current block with a jump; what follows is unreachable.
    fn jump(&mut self, j: Jump) {
        if self.dead {
            return;
        }
        self.func.blocks[self.cur.0 as usize].jump = Some(j);
        self.dead = true;
    }

    /// Starts a new list (a branch or loop body) with a fresh block.
    fn open_list(&mut self) -> Block {
        let b = self.func.new_block();
        self.lists.push(vec![Node::Block(b)]);
        self.cur = b;
        b
    }

    fn close_list(&mut self) -> Vec<Node> {
        self.lists.pop().unwrap_or_default()
    }

    /// Appends a node to the current list, then a new block after it.
    fn append_node(&mut self, n: Node) -> Block {
        let b = self.func.new_block();
        if let Some(list) = self.lists.last_mut() {
            list.push(n);
            list.push(Node::Block(b));
        }
        self.cur = b;
        b
    }

    /// Starts `if (cond)`: what is built next is its `then` branch, up to
    /// [`Self::if_else`], then its `else` branch, up to [`Self::if_end`]. A
    /// constant condition builds only the branch it takes, in place.
    fn if_begin(&mut self, cond: Value) -> IfStart {
        let mut st = IfStart { mode: IfMode::Skip, cond, mark: 0, then: None, then_list: Vec::new() };
        if !self.live() {
            return st;
        }
        if let Some(k) = self.func.constant(cond) {
            st.mode = IfMode::Constant(k.bits() != 0);
            return st;
        }
        st.mode = IfMode::Branch;
        st.mark = self.mark();
        self.open_list();
        st
    }

    /// Whether the `then` branch of an `if` is to be built.
    fn builds_then(&self, st: &IfStart) -> bool {
        self.live() && matches!(st.mode, IfMode::Constant(true) | IfMode::Branch)
    }

    fn if_else(&mut self, st: &mut IfStart) {
        if st.mode != IfMode::Branch {
            return;
        }
        st.then = self.live().then(|| self.changes(st.mark));
        st.then_list = self.close_list();
        self.rollback(st.mark);
        self.set_dead(false);
        self.open_list();
    }

    /// Whether the `else` branch of an `if` is to be built.
    fn builds_else(&self, st: &IfStart) -> bool {
        self.live() && matches!(st.mode, IfMode::Constant(false) | IfMode::Branch)
    }

    fn if_end(&mut self, st: IfStart) {
        if st.mode != IfMode::Branch {
            return;
        }
        let otherwise = self.live().then(|| self.changes(st.mark));
        let else_list = self.close_list();
        self.rollback(st.mark);
        self.release();
        let node = IfNode { cond: st.cond, then: st.then_list, otherwise: else_list };
        let merge = self.append_node(Node::If(alloc::boxed::Box::new(node)));
        let states: Vec<Changes> = [st.then, otherwise].into_iter().flatten().collect();
        self.set_dead(states.is_empty());
        if !states.is_empty() {
            self.merge(merge, &states);
        }
    }

    /// `if (cond) { then }`, for a `then` that builds nothing recursively.
    fn if_then(&mut self, cond: Value, then: impl FnOnce(&mut Self)) {
        let mut st = self.if_begin(cond);
        if self.builds_then(&st) {
            then(self);
        }
        self.if_else(&mut st);
        self.if_end(st);
    }

    /// Starts a loop of `kind`: what is built next is its body, up to
    /// [`Self::loop_end`]. `phis` says which components get a φ at the
    /// header, with the preheader's value first.
    fn loop_begin(
        &mut self,
        kind: LoopKind,
        construct: Continue<'a>,
        ret: Option<(u32, u32)>,
        phis: Phis<'_>,
    ) -> Option<LoopStart> {
        if !self.live() {
            return None;
        }
        // The flag a switch's `continue` sets exists before it, so that it
        // survives the switch.
        let continue_flag = if kind == LoopKind::Switch {
            let flag = self.temp();
            let f = self.konst_value(ops::Value::B(false));
            self.set((flag, 0), f);
            Some(flag)
        } else {
            None
        };
        let header = self.open_list();
        // The components to give φs, with their values before the loop.
        let current: Vec<(Key, Value)> = match phis {
            Phis::None => Vec::new(),
            Phis::All => self.defs.iter().map(|(&k, &v)| (k, v)).collect(),
            Phis::Of(w) => {
                let mut vars: Vec<u32> = w.vars.clone();
                if w.calls {
                    vars.extend_from_slice(&self.plan.globals);
                }
                if w.returns
                    && let Some((ret_key, flag_key)) = self.function_ret()
                {
                    vars.extend([ret_key, flag_key]);
                }
                vars.sort_unstable();
                vars.dedup();
                vars.iter()
                    .flat_map(|&var| self.defs.range((var, 0)..=(var, u32::MAX)).map(|(&k, &v)| (k, v)))
                    .collect()
            }
        };
        self.charge(current.len() as u64);
        let mut phis = Vec::with_capacity(current.len());
        for (k, v) in current {
            let ty = self.func.ty(v);
            let id = self.create(InstOp::Phi, vec![v], &[ty]);
            self.func.blocks[header.0 as usize].insts.push(id);
            let phi = self.func.insts[id.0 as usize].results[0];
            self.set(k, phi);
            phis.push((k, id));
        }
        let mark = self.mark();
        self.loops.push(LoopCtx {
            kind,
            mark,
            breaks: Vec::new(),
            continues: Vec::new(),
            construct,
            ret,
            continue_flag,
            returned: false,
            continued: false,
        });
        Some(LoopStart { phis })
    }

    fn loop_end(&mut self, st: Option<LoopStart>) {
        let Some(st) = st else { return };
        // Falling off the end of the body is a `continue`.
        let end = if self.live() {
            self.run_continue_construct();
            let mark = self.loops.last().map_or(0, |l| l.mark);
            self.live().then(|| self.changes(mark))
        } else {
            None
        };
        let Some(ctx) = self.loops.pop() else { return };
        let list = self.close_list();
        self.rollback(ctx.mark);
        self.release();
        // Back edges: the end of the body first, then each `continue`
        // (the order of `Func::preds`).
        let back: Vec<&Changes> = end.iter().chain(&ctx.continues).collect();
        self.charge((st.phis.len() * back.len()) as u64);
        for (k, id) in &st.phis {
            let own = self.func.insts[id.0 as usize].results[0];
            let ops: Vec<Value> = back.iter().map(|s| s.get(k).copied().flatten().unwrap_or(own)).collect();
            self.func.insts[id.0 as usize].args.extend(ops);
        }
        let exit = self.append_node(Node::Loop(alloc::boxed::Box::new(LoopNode { body: list })));
        self.set_dead(ctx.breaks.is_empty());
        if self.dead {
            if let Some(flag) = ctx.continue_flag {
                self.forget(flag);
            }
            return;
        }
        self.merge(exit, &ctx.breaks);
        // Propagate a `return` or a switch's `continue` outward.
        if ctx.returned
            && let Some(outer) = self.loops.last()
        {
            let (_, flag) = self.function_ret().unwrap_or((0, 0));
            let flag_v = self.defs.get(&(flag, 0)).copied();
            if let Some(f) = flag_v {
                if outer.kind != LoopKind::Function
                    && let Some(o) = self.loops.last_mut()
                {
                    o.returned = true;
                }
                self.if_then(f, |b| b.break_stmt());
            }
        }
        if let Some(flag) = ctx.continue_flag {
            if ctx.continued
                && let Some(&f) = self.defs.get(&(flag, 0))
            {
                self.if_then(f, |b| b.continue_stmt());
            }
            self.forget(flag);
        }
    }

    /// The innermost function's (return value key, return flag key).
    fn function_ret(&self) -> Option<(u32, u32)> {
        self.loops.iter().rev().find(|l| l.kind == LoopKind::Function).and_then(|l| l.ret)
    }

    /// Runs the innermost real loop's continue construct (a `for` step, a
    /// `do`-`while` test).
    fn run_continue_construct(&mut self) {
        let construct = self.loops.last().map_or(Continue::None, |l| l.construct);
        match construct {
            Continue::None => {}
            Continue::Step(e) => {
                self.expr(e);
            }
            Continue::Test(e) => {
                let c = self.expr(e);
                if let Some(&c) = c.first() {
                    let nc = self.op(Op::BNot, &[c], Ty::Bool);
                    self.if_then(nc, |b| b.break_stmt());
                }
            }
        }
    }

    fn break_stmt(&mut self) {
        if self.dead {
            return;
        }
        let mark = self.loops.last().map_or(0, |l| l.mark);
        let state = self.changes(mark);
        if let Some(l) = self.loops.last_mut() {
            l.breaks.push(state);
        }
        self.jump(Jump::Break);
    }

    fn continue_stmt(&mut self) {
        if self.dead {
            return;
        }
        match self.loops.last().map(|l| l.kind) {
            Some(LoopKind::Switch) => {
                // Leave the switch, then continue the enclosing loop.
                let flag = self.loops.last().and_then(|l| l.continue_flag).unwrap_or(0);
                let t = self.konst_value(ops::Value::B(true));
                self.set((flag, 0), t);
                if let Some(l) = self.loops.last_mut() {
                    l.continued = true;
                }
                self.break_stmt();
            }
            _ => {
                self.run_continue_construct();
                if self.dead {
                    return;
                }
                let mark = self.loops.last().map_or(0, |l| l.mark);
                let state = self.changes(mark);
                if let Some(l) = self.loops.last_mut() {
                    l.continues.push(state);
                }
                self.jump(Jump::Continue);
            }
        }
    }

    fn return_stmt(&mut self, value: Option<&'a Expr>) {
        if self.dead {
            return;
        }
        let Some((ret_key, flag_key)) = self.function_ret() else {
            // A final return of a simple function is handled by its caller.
            return;
        };
        if let Some(e) = value {
            let vals = self.expr(e);
            for (c, v) in vals.into_iter().enumerate() {
                self.set((ret_key, c as u32), v);
            }
        }
        let t = self.konst_value(ops::Value::B(true));
        self.set((flag_key, 0), t);
        if let Some(l) = self.loops.last_mut()
            && l.kind != LoopKind::Function
        {
            l.returned = true;
        }
        self.break_stmt();
    }

    // ---- The program -----------------------------------------------------

    fn run(&mut self) {
        // Every global and output starts as zero (GLSL leaves them
        // undefined); their keys must exist for the loops' φs.
        for (i, v) in self.s.vars.iter().enumerate() {
            let starts = match v.kind {
                VarKind::Global | VarKind::Output => true,
                VarKind::Builtin(b) => b.is_output(),
                _ => false,
            };
            if starts {
                self.define_zero(VarId(i as u32), v.ty);
            }
        }
        let s = self.s;
        self.stmts(&s.init);
        if let Some(main) = s.main {
            self.inline_body(main, &[]);
        }
        if !self.dead {
            self.store_outputs();
        }
        let list = self.lists.pop().unwrap_or_default();
        self.func.body = list;
    }

    fn define_zero(&mut self, id: VarId, ty: Type) {
        let mut tys = Vec::new();
        lower::component_types(ty, self.structs, &mut tys);
        self.charge(tys.len() as u64);
        for (c, t) in tys.into_iter().enumerate() {
            let z = self.zero(t);
            self.set((id.0, c as u32), z);
        }
    }

    fn store_outputs(&mut self) {
        let s = self.s;
        for (i, v) in s.vars.iter().enumerate() {
            let id = VarId(i as u32);
            let n = v.ty.flat_components(self.structs);
            match v.kind {
                VarKind::Output => {
                    let Some(&slot) = self.layout.outputs.get(&id) else { continue };
                    for c in 0..n {
                        let Some(&val) = self.defs.get(&(id.0, c)) else { continue };
                        let Some((row, comp)) = link::row_place(v.ty, c, self.structs) else { continue };
                        self.emit_void(InstOp::StoreOutput { slot: slot + row, comp }, vec![val]);
                    }
                }
                VarKind::Builtin(b) if b.is_output() && v.used => {
                    for c in 0..n {
                        let Some(&val) = self.defs.get(&(id.0, c)) else { continue };
                        let op = match b {
                            BuiltinVar::Position => InstOp::StoreBuiltin(BuiltinOut::Position(c as u8)),
                            BuiltinVar::PointSize => InstOp::StoreBuiltin(BuiltinOut::PointSize),
                            BuiltinVar::FragDepth => InstOp::StoreBuiltin(BuiltinOut::FragDepth),
                            BuiltinVar::FragColor => InstOp::StoreOutput { slot: 0, comp: c as u8 },
                            BuiltinVar::FragData => InstOp::StoreOutput { slot: c / 4, comp: (c % 4) as u8 },
                            _ => continue,
                        };
                        self.emit_void(op, vec![val]);
                    }
                }
                _ => {}
            }
        }
    }

    // ---- Functions -------------------------------------------------------

    /// Whether a function's only `return` is its last statement.
    fn simple_returns(body: &[Stmt]) -> bool {
        fn has_return(stmts: &[Stmt]) -> bool {
            stmts.iter().any(|s| match s {
                Stmt::Return(_) => true,
                Stmt::Block(b) => has_return(b),
                Stmt::If(_, t, e) => has_return(t) || has_return(e),
                Stmt::Loop(l) => has_return(&l.body),
                Stmt::Switch(_, c) => c.iter().any(|c| has_return(&c.body)),
                _ => false,
            })
        }
        match body.split_last() {
            Some((Stmt::Return(_), rest)) => !has_return(rest),
            Some(_) => !has_return(body),
            None => true,
        }
    }

    /// Runs a function's body (its parameters already defined), then writes
    /// its out and inout parameters back to their places; returns its
    /// return value's components. Its parameters and locals are forgotten
    /// after.
    fn inline_body(&mut self, f: FuncId, outs: &[(VarId, Place)]) -> Vec<Value> {
        let s = self.s;
        let func = s.function(f);
        let ret_ty = func.ret;
        let Some(body) = &func.body else { return Vec::new() };
        if self.dead || !self.enter() {
            return self.zeros(ret_ty);
        }
        let result = if Self::simple_returns(body) {
            let (last, rest) = match body.split_last() {
                Some((Stmt::Return(v), rest)) => (Some(v.as_ref()), rest),
                _ => (None, &body[..]),
            };
            self.run_stmts(rest);
            match last {
                Some(Some(e)) if !self.dead => self.expr(e),
                _ => self.zeros(ret_ty),
            }
        } else {
            self.wrapped_body(body, ret_ty)
        };
        self.forget_locals(body);
        for (p, (root, steps, ty)) in outs {
            let v = self.read_var(*p);
            self.write_path(*root, steps, *ty, &v);
        }
        for p in &func.params {
            self.forget(p.0);
        }
        self.leave();
        result
    }

    /// A function body with returns other than a final one, in a loop run
    /// once: `return` breaks out of it.
    fn wrapped_body(&mut self, body: &'a [Stmt], ret_ty: Type) -> Vec<Value> {
        let ret_key = self.temp();
        let flag_key = self.temp();
        let mut tys = Vec::new();
        lower::component_types(ret_ty, self.structs, &mut tys);
        for (c, &t) in tys.iter().enumerate() {
            let z = self.zero(t);
            self.set((ret_key, c as u32), z);
        }
        let f0 = self.konst_value(ops::Value::B(false));
        self.set((flag_key, 0), f0);
        let st = self.loop_begin(LoopKind::Function, Continue::None, Some((ret_key, flag_key)), Phis::None);
        if st.is_some() {
            self.run_stmts(body);
            self.break_stmt();
        }
        self.loop_end(st);
        let result = (0..tys.len() as u32)
            .map(|c| match self.defs.get(&(ret_key, c)) {
                Some(&v) => v,
                None => self.zero(tys[c as usize]),
            })
            .collect();
        self.forget(ret_key);
        self.forget(flag_key);
        result
    }

    // ---- Statements --------------------------------------------------------

    /// A list of statements, which is a scope: its locals are forgotten at
    /// its end.
    fn stmts(&mut self, list: &'a [Stmt]) {
        self.run_stmts(list);
        self.forget_locals(list);
    }

    /// A list of statements, its locals left for the caller to forget.
    fn run_stmts(&mut self, list: &'a [Stmt]) {
        for s in list {
            if self.dead {
                break;
            }
            self.stmt(s);
        }
    }

    fn stmt(&mut self, s: &'a Stmt) {
        if self.dead || !self.enter() {
            return;
        }
        self.charge(1);
        match s {
            Stmt::Expr(e) => {
                self.expr(e);
            }
            Stmt::Decl(id, init) => match init {
                Some(e) => {
                    let vals = self.expr(e);
                    self.write_whole(*id, &vals);
                }
                None => self.define_zero(*id, self.s.var(*id).ty),
            },
            Stmt::Block(list) => self.stmts(list),
            Stmt::If(c, t, e) => self.if_stmt(c, t, e),
            Stmt::Loop(l) => self.loop_stmt(l),
            Stmt::Switch(sel, clauses) => self.switch(sel, clauses),
            Stmt::Break => self.break_stmt(),
            Stmt::Continue => self.continue_stmt(),
            Stmt::Return(v) => self.return_stmt(v.as_ref()),
            Stmt::Discard => self.jump(Jump::Discard),
        }
        self.leave();
    }

    fn if_stmt(&mut self, c: &'a Expr, t: &'a [Stmt], e: &'a [Stmt]) {
        let cv = self.expr(c);
        let Some(&cv) = cv.first() else { return };
        let mut st = self.if_begin(cv);
        if self.builds_then(&st) {
            self.stmts(t);
        }
        self.if_else(&mut st);
        if self.builds_else(&st) {
            self.stmts(e);
        }
        self.if_end(st);
    }

    fn loop_stmt(&mut self, l: &'a hir::Loop) {
        let construct = match (&l.step, l.do_while) {
            (Some(step), _) => Continue::Step(step),
            (None, true) => l.cond.as_ref().map_or(Continue::None, Continue::Test),
            _ => Continue::None,
        };
        let plan = self.plan;
        // Every loop was planned; if one were not, every variable in scope
        // would get a φ (slower, never wrong).
        let phis = plan.loops.get(&loop_key(l)).map_or(Phis::All, Phis::Of);
        let st = self.loop_begin(LoopKind::Loop, construct, None, phis);
        if st.is_some() {
            if !l.do_while {
                if let Some((var, init)) = &l.cond_var {
                    let v = self.expr(init);
                    self.write_whole(*var, &v);
                }
                if let Some(c) = &l.cond {
                    let cv = self.expr(c);
                    if let Some(&cv) = cv.first() {
                        let nc = self.op(Op::BNot, &[cv], Ty::Bool);
                        self.if_then(nc, |b| b.break_stmt());
                    }
                }
            }
            self.stmts(&l.body);
        }
        self.loop_end(st);
        if let Some((var, _)) = &l.cond_var {
            self.forget(var.0);
        }
    }

    fn switch(&mut self, sel: &'a Expr, clauses: &'a [hir::Clause]) {
        let sv = self.expr(sel);
        let Some(&sv) = sv.first() else { return };
        // Whether the selector matches no label (for `default`).
        let mut matched_any = self.konst_value(ops::Value::B(false));
        for l in clauses.iter().flat_map(|c| c.labels.iter().flatten()) {
            let k = self.konst_value(*l);
            let eq = self.op(Op::IEq, &[sv, k], Ty::Bool);
            matched_any = self.op(Op::BOr, &[matched_any, eq], Ty::Bool);
        }
        let is_default = self.op(Op::BNot, &[matched_any], Ty::Bool);
        // The clauses share a scope: a variable declared in one is seen by
        // those it falls through to.
        for c in clauses {
            for st in &c.body {
                if let Stmt::Decl(id, _) = st {
                    self.define_zero(*id, self.s.var(*id).ty);
                }
            }
        }
        let ft_key = self.temp();
        let f0 = self.konst_value(ops::Value::B(false));
        self.set((ft_key, 0), f0);
        let st = self.loop_begin(LoopKind::Switch, Continue::None, None, Phis::None);
        if st.is_some() {
            for c in clauses {
                let mut cond = self.defs.get(&(ft_key, 0)).copied().unwrap_or(f0);
                for l in &c.labels {
                    let m = match l {
                        Some(v) => {
                            let k = self.konst_value(*v);
                            self.op(Op::IEq, &[sv, k], Ty::Bool)
                        }
                        None => is_default,
                    };
                    cond = self.op(Op::BOr, &[cond, m], Ty::Bool);
                }
                let mut branch = self.if_begin(cond);
                if self.builds_then(&branch) {
                    let t = self.konst_value(ops::Value::B(true));
                    self.set((ft_key, 0), t);
                    for st in &c.body {
                        if self.dead {
                            break;
                        }
                        self.stmt(st);
                    }
                }
                self.if_else(&mut branch);
                self.if_end(branch);
                if self.dead {
                    break;
                }
            }
            self.break_stmt();
        }
        self.loop_end(st);
        self.forget(ft_key);
        for c in clauses {
            self.forget_locals(&c.body);
        }
    }

    // ---- Variables -------------------------------------------------------

    /// The components of a whole variable.
    fn read_var(&mut self, id: VarId) -> Vec<Value> {
        let v = self.s.var(id);
        let n = v.ty.flat_components(self.structs);
        self.charge(u64::from(n));
        (0..n)
            .map(|c| {
                let ty = self.component_ty(v.ty, c);
                self.read_component(id, v.kind, c, ty)
            })
            .collect()
    }

    fn read_component(&mut self, id: VarId, kind: VarKind, c: u32, ty: Ty) -> Value {
        match kind {
            VarKind::Input => self.input_component(id, c, ty),
            VarKind::Uniform => self.uniform_component(id, c, ty),
            VarKind::BlockInstance(_) | VarKind::BlockMember(..) => self.block_component(id, c, ty),
            VarKind::Builtin(b) if !b.is_output() => self.builtin_input(b, c, ty),
            VarKind::Const => {
                let v = self.s.var(id).value.as_ref().and_then(|v| v.get(c as usize).copied());
                match v {
                    Some(x) => self.konst_value(x),
                    None => self.zero(ty),
                }
            }
            _ => match self.defs.get(&(id.0, c)) {
                Some(&v) => v,
                None => self.zero(ty),
            },
        }
    }

    fn input_component(&mut self, id: VarId, c: u32, ty: Ty) -> Value {
        let v = self.s.var(id);
        let Some(&base) = self.layout.inputs.get(&id) else { return self.zero(ty) };
        let Some((row, comp)) = link::row_place(v.ty, c, self.structs) else { return self.zero(ty) };
        let slot = base.saturating_add(row);
        let raw = self.cached(CacheKey::Input(slot, comp), InstOp::LoadInput { slot, comp }, Ty::F32);
        self.retype(raw, ty)
    }

    /// Reinterprets a loaded 32-bit value (stored as `F32` bits) as `ty`.
    fn retype(&mut self, raw: Value, ty: Ty) -> Value {
        if ty == self.func.ty(raw) {
            return raw;
        }
        match ty {
            Ty::Bool => {
                let as_int = self.op(Op::Bitcast, &[raw], Ty::I32);
                self.op(Op::IToB, &[as_int], Ty::Bool)
            }
            _ => self.op(Op::Bitcast, &[raw], ty),
        }
    }

    fn builtin_input(&mut self, b: BuiltinVar, c: u32, ty: Ty) -> Value {
        let which = match b {
            BuiltinVar::VertexId => BuiltinIn::VertexId,
            BuiltinVar::InstanceId => BuiltinIn::InstanceId,
            BuiltinVar::FragCoord => BuiltinIn::FragCoord(c as u8),
            BuiltinVar::FrontFacing => BuiltinIn::FrontFacing,
            BuiltinVar::PointCoord => BuiltinIn::PointCoord(c as u8),
            _ => return self.zero(ty),
        };
        self.cached(CacheKey::Builtin(which), InstOp::LoadBuiltin(which), ty)
    }

    fn uniform_component(&mut self, id: VarId, c: u32, ty: Ty) -> Value {
        let v = self.s.var(id);
        let Some(&base) = self.layout.uniforms.get(&id) else { return self.zero(ty) };
        // A sampler component is its sampler index.
        if let Some(&first) = self.layout.samplers.get(&id)
            && let Some(k) = link::sampler_place(v.ty, c, self.structs)
        {
            return self.int(first.saturating_add(k) as i32);
        }
        let Some((row, comp)) = link::row_place(v.ty, c, self.structs) else { return self.zero(ty) };
        let slot = base.saturating_add(row);
        let raw = self.cached(CacheKey::Uniform(slot, comp), InstOp::LoadUniform { slot, comp }, Ty::U32);
        self.uniform_retype(raw, ty)
    }

    /// Uniform storage holds 32 bits: floats as floats, ints, and bools as
    /// 0 or 1.
    fn uniform_retype(&mut self, raw: Value, ty: Ty) -> Value {
        match ty {
            Ty::U32 => raw,
            Ty::Bool => self.op(Op::IToB, &[raw], Ty::Bool),
            _ => self.op(Op::Bitcast, &[raw], ty),
        }
    }

    /// The std140 offset of member `m` of the shader's block `index`.
    fn block_member_offset(&mut self, index: u32, m: usize) -> Option<u32> {
        let block = self.s.blocks.get(index as usize)?;
        let structs = self.structs;
        let offsets = self.block_offsets.entry(index).or_insert_with(|| link::block_layout(block, structs).0);
        offsets.get(m).copied()
    }

    /// The program's uniform block and std140 byte offset of flat component
    /// `c` of a block variable.
    fn block_place(&mut self, id: VarId, c: u32) -> Option<(u32, u32)> {
        let v = self.s.var(id);
        let structs = self.structs;
        let (index, member) = match v.kind {
            VarKind::BlockInstance(b) => (b, None),
            VarKind::BlockMember(b, m) => (b, Some(m as usize)),
            _ => return None,
        };
        let block = self.s.blocks.get(index as usize)?;
        let program_block = *self.layout.blocks.get(&index)?;
        let (element, m, c) = match member {
            Some(m) => (0, m, c),
            None => {
                // An instance array: the element is a block of its own.
                let per = block.struct_ty.flat_components(structs).max(1);
                let (element, c) = if block.array.is_some() { (c / per, c % per) } else { (0, c) };
                let def = structs.get(block.struct_ty.as_struct()?);
                let i = def.info.member_at(c)?;
                (element, i, c - def.info.members[i].component)
            }
        };
        let mem = block.members.get(m)?;
        let start = self.block_member_offset(index, m)?;
        let offset = start.checked_add(link::std140_place(mem.ty, mem.row_major, c, structs)?)?;
        Some((program_block.checked_add(element)?, offset))
    }

    /// The bytes of a program's uniform block.
    fn block_size(&self, block: u32) -> u32 {
        self.linked.blocks.get(block as usize).map_or(0, |b| b.size)
    }

    fn block_component(&mut self, id: VarId, c: u32, ty: Ty) -> Value {
        let Some((block, offset)) = self.block_place(id, c) else { return self.zero(ty) };
        if offset.saturating_add(4) > self.block_size(block) {
            return self.zero(ty);
        }
        let raw = self.cached(
            CacheKey::Block(block, offset),
            InstOp::LoadBlock { block, offset, stride: 0, count: 0 },
            Ty::U32,
        );
        self.uniform_retype(raw, ty)
    }

    /// Writes a whole variable.
    fn write_whole(&mut self, id: VarId, vals: &[Value]) {
        for (c, &v) in vals.iter().enumerate() {
            self.set((id.0, c as u32), v);
        }
    }

    // ---- Access paths ------------------------------------------------------

    /// Splits an l-value or variable expression into its root variable and
    /// steps, evaluating its indices (from the root out); `None` for other
    /// expressions.
    fn path(&mut self, e: &'a Expr) -> Option<Path> {
        let mut chain: Vec<&'a Expr> = Vec::new();
        let mut at = e;
        let root = loop {
            match &at.kind {
                H::Var(id) => break *id,
                H::Field(b, _) | H::Swizzle(b, _) | H::Index(b, _) => {
                    chain.push(at);
                    at = b;
                }
                _ => return None,
            }
        };
        let mut steps = Vec::with_capacity(chain.len());
        for node in chain.into_iter().rev() {
            match &node.kind {
                H::Field(b, i) => steps.push((Step::Field(*i), b.ty)),
                H::Swizzle(b, s) => steps.push((Step::Swizzle(*s), b.ty)),
                H::Index(b, i) => {
                    let iv = self.expr(i);
                    let iv = *iv.first()?;
                    let k = self.func.constant(iv).map(|c| c.bits());
                    steps.push((Step::Index(k, iv), b.ty));
                }
                _ => {}
            }
        }
        Some((root, steps))
    }

    /// The flat component offsets a path selects (from its static steps),
    /// and its dynamic parts: (index value, element stride in components,
    /// element count).
    fn path_offsets(&self, steps: &[(Step, Type)], leaf: Type) -> (Vec<u32>, Vec<(Value, u32, u32)>) {
        let structs = self.structs;
        let mut base = 0u32;
        let mut dynamic = Vec::new();
        let mut swizzle: Option<hir::Swizzle> = None;
        for (step, ty) in steps {
            match step {
                Step::Field(i) => {
                    if let Some(id) = ty.as_struct() {
                        let start = structs.get(id).info.members.get(*i as usize).map_or(0, |m| m.component);
                        base = base.saturating_add(start);
                    }
                }
                Step::Index(k, v) => {
                    let (stride, count) = match (ty.array, ty.element) {
                        (Some(n), _) => (ty.element_components(structs), n),
                        (None, crate::types::Element::Basic(Basic::Vector(_, n))) => (1, u32::from(n)),
                        (None, crate::types::Element::Basic(Basic::Matrix(c, r))) => (u32::from(r), u32::from(c)),
                        _ => (1, 1),
                    };
                    match k {
                        // An index constant only once calls are inlined was
                        // not checked against the length: clamped to the
                        // array, as one computed at run time is.
                        Some(i) => {
                            let i = (*i).min(count.saturating_sub(1));
                            base = base.saturating_add(i.saturating_mul(stride));
                        }
                        None => dynamic.push((*v, stride, count)),
                    }
                }
                Step::Swizzle(s) => swizzle = Some(*s),
            }
        }
        let comps: Vec<u32> = match swizzle {
            Some(s) => s.components().iter().map(|&c| base.saturating_add(u32::from(c))).collect(),
            None => {
                let n = leaf.flat_components(structs);
                (base..base.saturating_add(n)).collect()
            }
        };
        (comps, dynamic)
    }

    /// Reads through a path (any variable kind).
    fn read_path(&mut self, root: VarId, steps: &[(Step, Type)], leaf: Type) -> Vec<Value> {
        let (comps, dynamic) = self.path_offsets(steps, leaf);
        let var = self.s.var(root);
        self.charge(comps.len() as u64);
        if dynamic.is_empty() {
            return comps
                .iter()
                .map(|&c| {
                    let ty = self.component_ty(var.ty, c);
                    self.read_component(root, var.kind, c, ty)
                })
                .collect();
        }
        // A computed index: uniforms and blocks load at a computed offset;
        // everything else selects among the possible elements.
        match var.kind {
            VarKind::Uniform if !self.layout.samplers.contains_key(&root) => {
                self.read_uniform_dynamic(root, &comps, &dynamic)
            }
            VarKind::BlockMember(..) => self.read_block_dynamic(root, &comps, &dynamic),
            VarKind::BlockInstance(b) if self.s.blocks.get(b as usize).is_some_and(|b| b.array.is_none()) => {
                self.read_block_dynamic(root, &comps, &dynamic)
            }
            _ => {
                let total = var.ty.flat_components(self.structs);
                comps
                    .iter()
                    .map(|&c| {
                        let ty = self.component_ty(var.ty, c);
                        self.select_component(root, var.kind, c, &dynamic, total, ty)
                    })
                    .collect()
            }
        }
    }

    /// Every place component `c` (static part) plus the dynamic offsets
    /// may be, with the (index value, element) pairs that select it.
    fn places(&mut self, c: u32, dynamic: &[(Value, u32, u32)], total: u32) -> Vec<(u32, Vec<(Value, u32)>)> {
        let mut places: Vec<(u32, Vec<(Value, u32)>)> = vec![(c, Vec::new())];
        for &(v, stride, count) in dynamic {
            let mut next = Vec::new();
            for (base, conds) in &places {
                for i in 0..count {
                    let place = base.saturating_add(i.saturating_mul(stride));
                    if place >= total {
                        break;
                    }
                    let mut cs = conds.clone();
                    cs.push((v, i));
                    next.push((place, cs));
                }
            }
            self.charge(next.len() as u64);
            places = next;
        }
        places
    }

    /// Component `c` (static part) plus the dynamic offsets, as a chain of
    /// selects over every place it can be.
    fn select_component(
        &mut self,
        root: VarId,
        kind: VarKind,
        c: u32,
        dynamic: &[(Value, u32, u32)],
        total: u32,
        ty: Ty,
    ) -> Value {
        let places = self.places(c, dynamic, total);
        let mut result: Option<Value> = None;
        for (place, conds) in places.into_iter().rev() {
            let value = self.read_component(root, kind, place, ty);
            result = Some(match result {
                // The last candidate is the default (an out-of-range index
                // reads it: undefined in GLSL, harmless here).
                None => value,
                Some(rest) => {
                    let cond = self.index_matches(&conds);
                    self.select(cond, value, rest, ty)
                }
            });
        }
        result.unwrap_or_else(|| self.zero(ty))
    }

    /// The condition that each (index value, element) pair matches.
    fn index_matches(&mut self, conds: &[(Value, u32)]) -> Value {
        let mut acc: Option<Value> = None;
        for &(v, i) in conds {
            let k = self.konst_value(ops::Value::from_bits(self.func.ty(v), i));
            let eq = self.op(Op::IEq, &[v, k], Ty::Bool);
            acc = Some(match acc {
                None => eq,
                Some(a) => self.op(Op::BAnd, &[a, eq], Ty::Bool),
            });
        }
        acc.unwrap_or_else(|| self.konst_value(ops::Value::B(true)))
    }

    /// The sum of each dynamic index times its stride (in whatever unit),
    /// as a signed int.
    fn dynamic_offset(&mut self, dynamic: &[(Value, u32, u32)], strides: &[u32]) -> Value {
        let mut off: Option<Value> = None;
        for (&(v, _, _), &stride) in dynamic.iter().zip(strides) {
            let iv = if self.func.ty(v) == Ty::U32 { self.op(Op::Bitcast, &[v], Ty::I32) } else { v };
            let s = self.int(stride as i32);
            let term = self.op(Op::IMul, &[iv, s], Ty::I32);
            off = Some(match off {
                None => term,
                Some(a) => self.op(Op::IAdd, &[a, term], Ty::I32),
            });
        }
        off.unwrap_or_else(|| self.int(0))
    }

    fn read_uniform_dynamic(&mut self, root: VarId, comps: &[u32], dynamic: &[(Value, u32, u32)]) -> Vec<Value> {
        let var = self.s.var(root);
        let structs = self.structs;
        let Some(&base) = self.layout.uniforms.get(&root) else {
            return comps.iter().map(|&c| self.zero(self.component_ty(var.ty, c))).collect();
        };
        let total_rows = var.ty.rows(structs);
        // The rows one step of each dynamic index moves: the distance
        // between the first selected component and the same component of
        // the next element (layouts are regular within arrays).
        let c0 = comps.first().copied().unwrap_or(0);
        let total = var.ty.flat_components(structs);
        let strides: Vec<u32> = dynamic
            .iter()
            .map(|&(_, stride, count)| {
                if count < 2 || c0.saturating_add(stride) >= total {
                    return 0;
                }
                match (link::row_place(var.ty, c0, structs), link::row_place(var.ty, c0 + stride, structs)) {
                    (Some(a), Some(b)) => b.0.saturating_sub(a.0),
                    _ => 0,
                }
            })
            .collect();
        let off = self.dynamic_offset(dynamic, &strides);
        comps
            .iter()
            .map(|&c| {
                let ty = self.component_ty(var.ty, c);
                let Some((row, comp)) = link::row_place(var.ty, c, structs) else { return self.zero(ty) };
                // Clamped so that the row stays inside the variable.
                let count = total_rows.saturating_sub(row).max(1);
                let op = InstOp::LoadUniformIndexed { base: base.saturating_add(row), comp, stride: 1, count };
                let raw = self.emit(op, vec![off], Ty::U32);
                self.uniform_retype(raw, ty)
            })
            .collect()
    }

    fn read_block_dynamic(&mut self, root: VarId, comps: &[u32], dynamic: &[(Value, u32, u32)]) -> Vec<Value> {
        let var = self.s.var(root);
        // Byte strides of the dynamic indices: the distance between the
        // first selected component and the same one of the next element.
        let c0 = comps.first().copied().unwrap_or(0);
        let total = var.ty.flat_components(self.structs);
        let first = self.block_place(root, c0).map(|p| p.1);
        let mut strides = Vec::with_capacity(dynamic.len());
        for &(_, stride, count) in dynamic {
            let next = if count < 2 || c0.saturating_add(stride) >= total {
                None
            } else {
                self.block_place(root, c0 + stride).map(|p| p.1)
            };
            strides.push(match (first, next) {
                (Some(a), Some(b)) => b.saturating_sub(a),
                _ => 0,
            });
        }
        let off = self.dynamic_offset(dynamic, &strides);
        comps
            .iter()
            .map(|&c| {
                let ty = self.component_ty(var.ty, c);
                let Some((blk, offset)) = self.block_place(root, c) else { return self.zero(ty) };
                let size = self.block_size(blk);
                if offset.saturating_add(4) > size {
                    return self.zero(ty);
                }
                // The byte offset is clamped so that the word read stays in
                // the block.
                let op = InstOp::LoadBlock { block: blk, offset, stride: 1, count: size - 3 - offset };
                let raw = self.emit(op, vec![off], Ty::U32);
                self.uniform_retype(raw, ty)
            })
            .collect()
    }

    /// Writes `vals` through a path.
    fn write_path(&mut self, root: VarId, steps: &[(Step, Type)], leaf: Type, vals: &[Value]) {
        let (comps, dynamic) = self.path_offsets(steps, leaf);
        let var = self.s.var(root);
        let total = var.ty.flat_components(self.structs);
        if dynamic.is_empty() {
            for (&c, &v) in comps.iter().zip(vals) {
                if c < total {
                    self.set((root.0, c), v);
                }
            }
            return;
        }
        // Every place the value may go keeps its value unless the index
        // matches it.
        for (&c, &v) in comps.iter().zip(vals) {
            for (place, conds) in self.places(c, &dynamic, total) {
                let ty = self.component_ty(var.ty, place);
                let old = self.defs.get(&(root.0, place)).copied().unwrap_or_else(|| self.zero(ty));
                let cond = self.index_matches(&conds);
                let new = self.select(cond, v, old, ty);
                self.set((root.0, place), new);
            }
        }
    }

    // ---- Expressions -------------------------------------------------------

    /// Whether evaluating `e` can change state (so it must not be evaluated
    /// speculatively).
    fn has_effects(e: &Expr) -> bool {
        match &e.kind {
            H::Const(_) | H::Var(_) => false,
            H::Assign(..) | H::CompoundAssign(..) | H::IncDec { .. } | H::Call(..) => true,
            H::Builtin(Builtin::Modf, _) => true,
            H::Unary(_, a) | H::Swizzle(a, _) | H::Field(a, _) => Self::has_effects(a),
            H::Binary(_, a, b) | H::Logic(_, a, b) | H::Index(a, b) | H::Sequence(a, b) => {
                Self::has_effects(a) || Self::has_effects(b)
            }
            H::Ternary(c, a, b) => Self::has_effects(c) || Self::has_effects(a) || Self::has_effects(b),
            H::Construct(args) | H::Builtin(_, args) => args.iter().any(Self::has_effects),
        }
    }

    /// Evaluates an expression to its components.
    fn expr(&mut self, e: &'a Expr) -> Vec<Value> {
        if self.dead || !self.enter() {
            return self.zeros(e.ty);
        }
        self.charge(1);
        let v = match &e.kind {
            H::Const(c) => {
                self.charge(c.len() as u64);
                c.iter().map(|&v| self.konst_value(v)).collect()
            }
            H::Var(id) => self.read_var(*id),
            H::Field(..) | H::Swizzle(..) | H::Index(..) => self.access(e),
            H::Unary(op, a) => {
                let v = self.expr(a);
                self.unary(*op, a.ty, &v)
            }
            H::Binary(op, a, b) => {
                let l = self.expr(a);
                let r = self.expr(b);
                self.binary(*op, a.ty, &l, b.ty, &r)
            }
            H::Logic(op, a, b) => self.logic(*op, a, b),
            H::Ternary(c, a, b) => self.ternary(c, a, b, e.ty),
            H::Assign(target, value) => self.assign(target, value),
            H::CompoundAssign(op, target, value) => self.compound_assign(*op, target, value),
            H::IncDec { target, increment, prefix } => self.inc_dec(target, *increment, *prefix),
            H::Construct(args) => self.construct(args, e.ty),
            H::Call(f, args) => self.call(*f, args),
            H::Builtin(b, args) => self.builtin(*b, args, e.ty),
            H::Sequence(a, b) => {
                self.expr(a);
                self.expr(b)
            }
        };
        self.leave();
        v
    }

    #[inline(never)]
    fn unary(&mut self, op: UnOp, t: Type, v: &[Value]) -> Vec<Value> {
        lower::unary(self, op, t, v)
    }

    #[inline(never)]
    fn binary(&mut self, op: BinOp, lt: Type, l: &[Value], rt: Type, r: &[Value]) -> Vec<Value> {
        let structs = self.structs;
        lower::binary(self, op, lt, l, rt, r, structs)
    }

    /// A field, swizzle or index: through a variable, or of a value.
    fn access(&mut self, e: &'a Expr) -> Vec<Value> {
        if let Some((root, steps)) = self.path(e) {
            return self.read_path(root, &steps, e.ty);
        }
        self.rvalue_access(e)
    }

    /// `a && b` and `a || b`.
    fn logic(&mut self, op: LogicOp, a: &'a Expr, b: &'a Expr) -> Vec<Value> {
        let l = self.expr(a)[0];
        if !Self::has_effects(b) {
            let r = self.expr(b)[0];
            let o = if op == LogicOp::And { Op::BAnd } else { Op::BOr };
            return vec![self.op(o, &[l, r], Ty::Bool)];
        }
        // Short-circuit for real.
        let key = self.temp();
        self.set((key, 0), l);
        let cond = if op == LogicOp::And { l } else { self.op(Op::BNot, &[l], Ty::Bool) };
        let mut st = self.if_begin(cond);
        if self.builds_then(&st) {
            let r = self.expr(b)[0];
            self.set((key, 0), r);
        }
        self.if_else(&mut st);
        self.if_end(st);
        let v = self.defs.get(&(key, 0)).copied().unwrap_or(l);
        self.forget(key);
        vec![v]
    }

    /// `c ? a : b`.
    fn ternary(&mut self, c: &'a Expr, a: &'a Expr, b: &'a Expr, ty: Type) -> Vec<Value> {
        let cv = self.expr(c)[0];
        let mut tys = Vec::new();
        lower::component_types(ty, self.structs, &mut tys);
        if !Self::has_effects(a) && !Self::has_effects(b) {
            let x = self.expr(a);
            let y = self.expr(b);
            return x.iter().zip(&y).zip(&tys).map(|((&p, &q), &t)| self.select(cv, p, q, t)).collect();
        }
        let key = self.temp();
        for (i, &t) in tys.iter().enumerate() {
            let z = self.zero(t);
            self.set((key, i as u32), z);
        }
        let mut st = self.if_begin(cv);
        if self.builds_then(&st) {
            let v = self.expr(a);
            for (i, x) in v.into_iter().enumerate() {
                self.set((key, i as u32), x);
            }
        }
        self.if_else(&mut st);
        if self.builds_else(&st) {
            let v = self.expr(b);
            for (i, x) in v.into_iter().enumerate() {
                self.set((key, i as u32), x);
            }
        }
        self.if_end(st);
        let out = (0..tys.len() as u32)
            .map(|i| self.defs.get(&(key, i)).copied().unwrap_or_else(|| self.zero(tys[i as usize])))
            .collect();
        self.forget(key);
        out
    }

    fn assign(&mut self, target: &'a Expr, value: &'a Expr) -> Vec<Value> {
        // The target's indices are evaluated once, before the value.
        let place = self.path(target);
        let v = self.expr(value);
        if let Some((root, steps)) = place {
            self.write_path(root, &steps, target.ty, &v);
        }
        v
    }

    fn compound_assign(&mut self, op: BinOp, target: &'a Expr, value: &'a Expr) -> Vec<Value> {
        let Some((root, steps)) = self.path(target) else { return self.expr(value) };
        let r = self.expr(value);
        let l = self.read_path(root, &steps, target.ty);
        let v = self.binary(op, target.ty, &l, value.ty, &r);
        self.write_path(root, &steps, target.ty, &v);
        v
    }

    fn inc_dec(&mut self, target: &'a Expr, increment: bool, prefix: bool) -> Vec<Value> {
        let Some((root, steps)) = self.path(target) else { return Vec::new() };
        let old = self.read_path(root, &steps, target.ty);
        let s = target.ty.as_basic().and_then(Basic::scalar).unwrap_or(Scalar::Float);
        let one = match s {
            Scalar::Float => self.konst_value(ops::Value::F(1.0)),
            Scalar::Uint => self.konst_value(ops::Value::U(1)),
            _ => self.int(1),
        };
        let one_ty = Type::basic(Basic::Scalar(s));
        let op = if increment { BinOp::Add } else { BinOp::Sub };
        let new = self.binary(op, target.ty, &old, one_ty, &[one]);
        self.write_path(root, &steps, target.ty, &new);
        if prefix { new } else { old }
    }

    fn construct(&mut self, args: &'a [Expr], ty: Type) -> Vec<Value> {
        let vals: Vec<(Type, Vec<Value>)> = args.iter().map(|a| (a.ty, self.expr(a))).collect();
        self.construct_from(ty, &vals)
    }

    #[inline(never)]
    fn construct_from(&mut self, ty: Type, vals: &[(Type, Vec<Value>)]) -> Vec<Value> {
        lower::construct(self, ty, vals)
    }

    /// Field, swizzle or index of a value that is not a variable.
    fn rvalue_access(&mut self, e: &'a Expr) -> Vec<Value> {
        match &e.kind {
            H::Field(b, i) => {
                let v = self.expr(b);
                let Some(id) = b.ty.as_struct() else { return v };
                let def = self.structs.get(id);
                let start = def.info.members.get(*i as usize).map_or(0, |m| m.component) as usize;
                let n = def.fields.get(*i as usize).map_or(0, |f| f.ty.flat_components(self.structs)) as usize;
                v.get(start..start + n).map_or_else(|| self.zeros(e.ty), <[Value]>::to_vec)
            }
            H::Swizzle(b, s) => {
                let v = self.expr(b);
                s.components().iter().map(|&c| v.get(c as usize).copied().unwrap_or(v[0])).collect()
            }
            H::Index(b, i) => {
                let v = self.expr(b);
                let iv = self.expr(i)[0];
                let size = e.ty.flat_components(self.structs).max(1) as usize;
                let count = v.len() / size;
                if count == 0 {
                    return self.zeros(e.ty);
                }
                if let Some(k) = self.func.constant(iv) {
                    let k = (k.bits() as usize).min(count - 1);
                    return v[k * size..(k + 1) * size].to_vec();
                }
                self.charge((count * size) as u64);
                (0..size)
                    .map(|c| {
                        let ty = self.component_ty(e.ty, c as u32);
                        let mut result = v[(count - 1) * size + c];
                        for k in (0..count - 1).rev() {
                            let cond = self.index_matches(&[(iv, k as u32)]);
                            result = self.select(cond, v[k * size + c], result, ty);
                        }
                        result
                    })
                    .collect()
            }
            _ => self.expr(e),
        }
    }

    /// Inlines a call.
    fn call(&mut self, f: FuncId, args: &'a [Expr]) -> Vec<Value> {
        let s = self.s;
        let func = s.function(f);
        // Evaluate the arguments left to right; out and inout arguments
        // remember their place (its indices evaluated now, once).
        let mut outs: Vec<(VarId, Place)> = Vec::new();
        let mut values: Vec<(VarId, Vec<Value>)> = Vec::new();
        for (&p, a) in func.params.iter().zip(args) {
            let mode = match s.var(p).kind {
                VarKind::Param(m) => m,
                _ => ParamMode::In,
            };
            match mode {
                ParamMode::In => {
                    let v = self.expr(a);
                    values.push((p, v));
                }
                ParamMode::Out | ParamMode::InOut => {
                    let Some((root, steps)) = self.path(a) else { continue };
                    let v = if mode == ParamMode::InOut {
                        self.read_path(root, &steps, a.ty)
                    } else {
                        self.zeros(s.var(p).ty)
                    };
                    outs.push((p, (root, steps, a.ty)));
                    values.push((p, v));
                }
            }
        }
        for (p, v) in values {
            self.write_whole(p, &v);
        }
        self.inline_body(f, &outs)
    }

    fn builtin(&mut self, b: Builtin, args: &'a [Expr], ret: Type) -> Vec<Value> {
        match b {
            Builtin::Texture(t) => return self.texture(t, args),
            Builtin::TextureSize => return self.texture_size(args),
            Builtin::DFdx | Builtin::DFdy | Builtin::Fwidth => {
                let v = self.expr(&args[0]);
                return self.derivatives(b, &v);
            }
            _ => {}
        }
        if b == Builtin::Modf {
            // modf(x, out i): the place of i is evaluated once.
            let x = self.expr(&args[0]);
            let place = self.path(&args[1]);
            return self.modf(&x, args[0].ty, args[1].ty, ret, place);
        }
        let vals: Vec<(Type, Vec<Value>)> = args.iter().map(|a| (a.ty, self.expr(a))).collect();
        self.builtin_from(b, &vals, ret)
    }

    #[inline(never)]
    fn builtin_from(&mut self, b: Builtin, vals: &[(Type, Vec<Value>)], ret: Type) -> Vec<Value> {
        lower::builtin(self, b, vals, ret).0
    }

    #[inline(never)]
    fn modf(&mut self, x: &[Value], xt: Type, it: Type, ret: Type, place: Option<Path>) -> Vec<Value> {
        let zeros: Vec<Value> = x.iter().map(|_| self.konst_value(ops::Value::F(0.0))).collect();
        let vals = [(xt, x.to_vec()), (it, zeros)];
        let (result, out) = lower::builtin(self, Builtin::Modf, &vals, ret);
        if let (Some(o), Some((root, steps))) = (out, place) {
            self.write_path(root, &steps, it, &o);
        }
        result
    }

    #[inline(never)]
    fn derivatives(&mut self, b: Builtin, v: &[Value]) -> Vec<Value> {
        v.iter()
            .map(|&x| {
                if self.func.constant(x).is_some() {
                    return self.konst_value(ops::Value::F(0.0));
                }
                match b {
                    Builtin::DFdx => self.emit(InstOp::Deriv { y: false }, vec![x], Ty::F32),
                    Builtin::DFdy => self.emit(InstOp::Deriv { y: true }, vec![x], Ty::F32),
                    _ => {
                        let dx = self.emit(InstOp::Deriv { y: false }, vec![x], Ty::F32);
                        let dy = self.emit(InstOp::Deriv { y: true }, vec![x], Ty::F32);
                        let ax = self.op(Op::FAbs, &[dx], Ty::F32);
                        let ay = self.op(Op::FAbs, &[dy], Ty::F32);
                        self.op(Op::FAdd, &[ax, ay], Ty::F32)
                    }
                }
            })
            .collect()
    }

    // ---- Textures ----------------------------------------------------------

    /// The program sampler indices a sampler value may hold: the constants
    /// a chain of selects (dynamic indexing) chooses among. `None` if it is
    /// anything else.
    fn sampler_candidates(&self, v: Value) -> Option<Vec<u32>> {
        let mut out = Vec::new();
        let mut work = vec![v];
        while let Some(v) = work.pop() {
            if out.len() > self.linked.samplers.len() {
                return None;
            }
            let inst = self.func.def(v);
            match inst.op {
                InstOp::Const(c) => out.push(c.bits()),
                InstOp::Select => work.extend_from_slice(&inst.args[1..]),
                _ => return None,
            }
        }
        out.sort_unstable();
        out.dedup();
        Some(out)
    }

    /// What a lookup's sampler argument selects: a constant sampler index,
    /// or the first of a run of samplers of type `ty` (contiguous program
    /// indices) and the index into the run; or, for any other set, the
    /// candidates one by one.
    fn sampler_choice(&mut self, e: &'a Expr, ty: crate::types::Sampler) -> SamplerChoice {
        let v = self.expr(e);
        let Some(&index) = v.first() else { return SamplerChoice::Fixed(0) };
        if let Some(k) = self.func.constant(index) {
            return SamplerChoice::Fixed(k.bits());
        }
        let samplers = &self.linked.samplers;
        let of_type = |k: u32| samplers.get(k as usize).is_some_and(|s| s.sampler == ty);
        let candidates = match self.sampler_candidates(index) {
            Some(c) => c,
            // Not a selection of constants: any sampler of the type.
            None => (0..samplers.len() as u32).filter(|&k| of_type(k)).collect(),
        };
        let candidates: Vec<u32> = candidates.into_iter().filter(|&k| of_type(k)).collect();
        let (Some(&first), Some(&last)) = (candidates.first(), candidates.last()) else {
            return SamplerChoice::Fixed(0);
        };
        if last - first + 1 == candidates.len() as u32 {
            let base = self.int(first as i32);
            let rel = self.op(Op::ISub, &[index, base], Ty::I32);
            return SamplerChoice::Run { first, count: candidates.len() as u32, index: rel };
        }
        SamplerChoice::Each { index, candidates }
    }

    fn texture(&mut self, t: TexCall, args: &'a [Expr]) -> Vec<Value> {
        let choice = self.sampler_choice(&args[0], t.sampler);
        let coord = self.expr(&args[1]);
        let rest: Vec<Vec<Value>> = args[2..].iter().map(|a| self.expr(a)).collect();
        self.texture_from(t, choice, coord, rest)
    }

    #[inline(never)]
    fn texture_from(
        &mut self,
        t: TexCall,
        choice: SamplerChoice,
        coord: Vec<Value>,
        mut rest: Vec<Vec<Value>>,
    ) -> Vec<Value> {
        let s = t.sampler;
        if coord.is_empty() {
            return self.zeros(if s.shadow { Type::FLOAT } else { Type::vector(s.ty, 4) });
        }
        // Projection: divide the coordinates (and the reference) by q.
        let mut coords: Vec<Value> = if t.proj {
            let q = coord[coord.len() - 1];
            let one = self.konst_value(ops::Value::F(1.0));
            let inv = self.op(Op::FDiv, &[one, q], Ty::F32);
            // Projection exists for 2D and 3D lookups: s, t (r) and the
            // shadow reference are divided. For a 2D lookup with a vec4, z
            // is ignored.
            let n = s.coords() as usize + usize::from(s.shadow);
            let take: Vec<usize> = if s.dim == Dim::D2 && !s.shadow { vec![0, 1] } else { (0..n).collect() };
            take.iter()
                .map(|&i| coord.get(i).copied().unwrap_or(q))
                .map(|c| self.op(Op::FMul, &[c, inv], Ty::F32))
                .collect()
        } else {
            coord.clone()
        };
        if t.lod == TexLod::Fetch {
            coords = coord;
        }
        let mut extra: Vec<Value> = Vec::new();
        let mut offset = [0i8; 3];
        // Normalised order: [bias | lod | dPdx dPdy], offset.
        if t.offset
            && let Some(o) = rest.pop()
        {
            for (i, &v) in o.iter().enumerate().take(3) {
                offset[i] = self.func.constant(v).map_or(0, |c| c.bits() as i32 as i8);
            }
        }
        let lod = match t.lod {
            TexLod::Implicit if t.bias => {
                extra.extend(rest.first().cloned().unwrap_or_default());
                TexLod::Implicit
            }
            TexLod::Lod | TexLod::Fetch => {
                extra.extend(rest.first().cloned().unwrap_or_default());
                t.lod
            }
            TexLod::Grad => {
                for g in rest.iter().take(2) {
                    extra.extend(g.iter().copied());
                }
                TexLod::Grad
            }
            l => l,
        };
        // A vertex shader's implicit lookups use the base level.
        let (lod, bias) = if self.s.stage == Stage::Vertex && lod == TexLod::Implicit {
            extra.clear();
            extra.push(self.konst_value(ops::Value::F(0.0)));
            (TexLod::Lod, false)
        } else {
            (lod, t.bias)
        };
        let ncoords = coords.len() as u8;
        let ty = lower::ty(s.ty);
        let tys: Vec<Ty> = if s.shadow { vec![Ty::F32] } else { vec![ty; 4] };
        let lookup = |b: &mut Self, index: u32, count: u32, dynamic: Option<Value>| {
            let op = TexOp { sampler: s, index, dynamic: dynamic.is_some(), count, lod, bias, offset, coords: ncoords };
            let mut targs: Vec<Value> = dynamic.into_iter().collect();
            targs.extend(&coords);
            targs.extend(&extra);
            b.emit_multi(InstOp::Tex(op), targs, &tys)
        };
        match choice {
            SamplerChoice::Fixed(k) => lookup(self, k, 1, None),
            SamplerChoice::Run { first, count, index } => lookup(self, first, count, Some(index)),
            SamplerChoice::Each { index, candidates } => {
                // A lookup for each sampler the index may select; the one it
                // does is chosen after.
                let mut result: Option<Vec<Value>> = None;
                for &k in candidates.iter().rev() {
                    let r = lookup(self, k, 1, None);
                    result = Some(match result {
                        None => r,
                        Some(rest) => {
                            let kk = self.int(k as i32);
                            let is = self.op(Op::IEq, &[index, kk], Ty::Bool);
                            r.iter().zip(&rest).zip(&tys).map(|((&a, &b), &t)| self.select(is, a, b, t)).collect()
                        }
                    });
                }
                result.unwrap_or_default()
            }
        }
    }

    fn texture_size(&mut self, args: &'a [Expr]) -> Vec<Value> {
        let Some(Basic::Sampler(s)) = args[0].ty.as_basic() else { return Vec::new() };
        let choice = self.sampler_choice(&args[0], s);
        let lod = self.expr(&args[1])[0];
        let n = match s.dim {
            Dim::D2 | Dim::Cube => 2,
            _ => 3,
        };
        let tys = vec![Ty::I32; n];
        let query = |b: &mut Self, index: u32, count: u32, dynamic: Option<Value>| {
            let op = InstOp::TexSize { sampler: s, index, dynamic: dynamic.is_some(), count };
            let mut targs: Vec<Value> = dynamic.into_iter().collect();
            targs.push(lod);
            b.emit_multi(op, targs, &tys)
        };
        match choice {
            SamplerChoice::Fixed(k) => query(self, k, 1, None),
            SamplerChoice::Run { first, count, index } => query(self, first, count, Some(index)),
            SamplerChoice::Each { index, candidates } => {
                let mut result: Option<Vec<Value>> = None;
                for &k in candidates.iter().rev() {
                    let r = query(self, k, 1, None);
                    result = Some(match result {
                        None => r,
                        Some(rest) => {
                            let kk = self.int(k as i32);
                            let is = self.op(Op::IEq, &[index, kk], Ty::Bool);
                            r.iter().zip(&rest).map(|(&a, &b)| self.select(is, a, b, Ty::I32)).collect()
                        }
                    });
                }
                result.unwrap_or_default()
            }
        }
    }
}

/// What a lookup's sampler argument selects ([`Builder::sampler_choice`]).
enum SamplerChoice {
    /// One sampler.
    Fixed(u32),
    /// `count` samplers from `first`, `index` (an int) choosing among them.
    Run { first: u32, count: u32, index: Value },
    /// Any of these samplers, as the program's sampler index `index` says.
    Each { index: Value, candidates: Vec<u32> },
}
