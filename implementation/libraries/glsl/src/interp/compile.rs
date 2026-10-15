// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! IR to bytecode.
//!
//! 1. Uniform values (constants, uniforms, and pure operations on uniform
//!    values) go to the prologue.
//! 2. The rest is laid out in program order: a block's instructions, the
//!    copies into its successors' φs, then the structure's instruction
//!    (`If`, `Else`, `EndLoop`, a jump).
//! 3. A value defined in a loop and used after it is written only in the
//!    active lanes (through a scratch register and a masked copy).
//! 4. Registers: inputs get registers of their own; everything else is
//!    allocated by linear scan over the code's positions, a value used in
//!    a loop it was defined before being kept to the loop's end. A program
//!    [`crate::program::link`] accepts never needs more registers than
//!    [`Reg`] numbers; one that would (which nothing links) compiles to a
//!    program that does nothing, rather than to registers that overlap.

use alloc::collections::{BTreeMap, BTreeSet, BinaryHeap};
use alloc::vec;
use alloc::vec::Vec;
use core::cmp::Reverse;

use super::{Input, Ins, Output, Program, Reg, TexIns, TexSizeIns};
use crate::ir::{Block, CopyStep, Func, InstId, InstOp, Jump, Node, Value};
use crate::ops::Ty;

/// Compiles a stage's (optimised) IR.
pub fn compile(f: &Func) -> Program {
    let mut c = Compiler::new(f);
    c.uniformity();
    c.prologue();
    c.main();
    c.finish()
}

/// A bytecode operand before register allocation: an IR value, or a
/// scratch register for a masked write.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Slot {
    V(Value),
    Scratch(u32),
}

/// An instruction with symbolic operands.
#[derive(Clone, Debug)]
enum Sym {
    Op {
        op: crate::ops::Op,
        ty: Ty,
        d: Slot,
        a: Slot,
        b: Slot,
    },
    Select {
        d: Slot,
        c: Slot,
        a: Slot,
        b: Slot,
    },
    Const {
        d: Slot,
        bits: u32,
    },
    Copy {
        d: Slot,
        s: Slot,
    },
    CopyMasked {
        d: Slot,
        s: Slot,
    },
    LoadUniform {
        d: Slot,
        slot: u32,
        comp: u8,
    },
    LoadUniformIndexed {
        d: Slot,
        base: u32,
        comp: u8,
        stride: u32,
        count: u32,
        index: Slot,
    },
    LoadBlock {
        d: Slot,
        block: u32,
        offset: u32,
    },
    LoadBlockIndexed {
        d: Slot,
        block: u32,
        offset: u32,
        stride: u32,
        count: u32,
        index: Slot,
    },
    Tex {
        op: crate::ir::TexOp,
        index: Option<Slot>,
        args: Vec<Slot>,
        results: Vec<Slot>,
    },
    TexSize {
        sampler: crate::types::Sampler,
        sampler_index: u32,
        count: u32,
        index: Option<Slot>,
        lod: Slot,
        results: Vec<Slot>,
    },
    Deriv {
        d: Slot,
        a: Slot,
        y: bool,
    },
    If {
        c: Slot,
    },
    Else,
    EndIf,
    Loop,
    EndLoop,
    Break,
    Continue,
    Discard,
}

impl Sym {
    fn defs(&self) -> Vec<Slot> {
        match self {
            Sym::Op { d, .. }
            | Sym::Select { d, .. }
            | Sym::Const { d, .. }
            | Sym::Copy { d, .. }
            | Sym::CopyMasked { d, .. }
            | Sym::LoadUniform { d, .. }
            | Sym::LoadUniformIndexed { d, .. }
            | Sym::LoadBlock { d, .. }
            | Sym::LoadBlockIndexed { d, .. }
            | Sym::Deriv { d, .. } => vec![*d],
            Sym::Tex { results, .. } | Sym::TexSize { results, .. } => results.clone(),
            _ => Vec::new(),
        }
    }

    fn uses(&self) -> Vec<Slot> {
        match self {
            Sym::Op { a, b, .. } => vec![*a, *b],
            Sym::Select { c, a, b, .. } => vec![*c, *a, *b],
            Sym::Copy { s, .. } => vec![*s],
            // A masked copy keeps the inactive lanes: it reads its target.
            Sym::CopyMasked { d, s } => vec![*s, *d],
            Sym::LoadUniformIndexed { index, .. } | Sym::LoadBlockIndexed { index, .. } => vec![*index],
            Sym::Tex { index, args, .. } => index.iter().chain(args).copied().collect(),
            Sym::TexSize { index, lod, .. } => index.iter().copied().chain([*lod]).collect(),
            Sym::Deriv { a, .. } => vec![*a],
            Sym::If { c } => vec![*c],
            _ => Vec::new(),
        }
    }
}

struct Compiler<'a> {
    f: &'a Func,
    preds: Vec<Vec<Block>>,
    /// Each block's successors ([`crate::ir::successors`]).
    succs: Vec<Vec<(Block, usize)>>,
    uniform: Vec<bool>,
    prologue: Vec<Sym>,
    main: Vec<Sym>,
    inputs: Vec<(Input, Value)>,
    outputs: Vec<(Output, Value)>,
    /// Values written only in the active lanes (used after their loop).
    masked: Vec<bool>,
    scratch: u32,
    /// Loop spans in `main`: (start, end) positions.
    loops: Vec<(usize, usize)>,
    discards: bool,
    derivatives: bool,
}

impl<'a> Compiler<'a> {
    fn new(f: &'a Func) -> Compiler<'a> {
        let preds = f.preds();
        Compiler {
            f,
            succs: crate::ir::successors(&preds),
            preds,
            uniform: vec![false; f.values.len()],
            prologue: Vec::new(),
            main: Vec::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            masked: vec![false; f.values.len()],
            scratch: 0,
            loops: Vec::new(),
            discards: false,
            derivatives: false,
        }
    }

    /// Which values are the same in every lane of a draw.
    fn uniformity(&mut self) {
        let order = self.f.block_order();
        for &b in &order {
            for &i in &self.f.blocks[b.0 as usize].insts {
                let inst = self.f.inst(i);
                let all_uniform = inst.args.iter().all(|a| self.uniform[a.0 as usize]);
                let u = match &inst.op {
                    InstOp::Const(_) | InstOp::LoadUniform { .. } => true,
                    InstOp::LoadBlock { .. } | InstOp::LoadUniformIndexed { .. } | InstOp::Op(_) | InstOp::Select => {
                        all_uniform
                    }
                    _ => false,
                };
                for &r in &inst.results {
                    self.uniform[r.0 as usize] = u;
                }
            }
        }
    }

    fn slot(&self, v: Value) -> Slot {
        Slot::V(v)
    }

    /// The symbolic form of a pure instruction writing `d`.
    fn translate(&mut self, i: InstId, d: Slot) -> Option<Sym> {
        let inst = self.f.inst(i);
        let a = |k: usize| Slot::V(inst.args[k]);
        Some(match &inst.op {
            InstOp::Const(c) => Sym::Const { d, bits: c.bits() },
            InstOp::Op(op) => {
                let b = if inst.args.len() > 1 { a(1) } else { a(0) };
                Sym::Op { op: *op, ty: self.f.ty(inst.results[0]), d, a: a(0), b }
            }
            InstOp::Select => Sym::Select { d, c: a(0), a: a(1), b: a(2) },
            InstOp::LoadUniform { slot, comp } => Sym::LoadUniform { d, slot: *slot, comp: *comp },
            InstOp::LoadUniformIndexed { base, comp, stride, count } => {
                Sym::LoadUniformIndexed { d, base: *base, comp: *comp, stride: *stride, count: *count, index: a(0) }
            }
            InstOp::LoadBlock { block, offset, .. } if inst.args.is_empty() => {
                Sym::LoadBlock { d, block: *block, offset: *offset }
            }
            InstOp::LoadBlock { block, offset, stride, count } => {
                Sym::LoadBlockIndexed { d, block: *block, offset: *offset, stride: *stride, count: *count, index: a(0) }
            }
            InstOp::Deriv { y } => {
                self.derivatives = true;
                Sym::Deriv { d, a: a(0), y: *y }
            }
            _ => return None,
        })
    }

    fn prologue(&mut self) {
        let order = self.f.block_order();
        for &b in &order {
            for &i in &self.f.blocks[b.0 as usize].insts {
                let inst = self.f.inst(i);
                let Some(&r) = inst.results.first() else { continue };
                if !self.uniform[r.0 as usize] {
                    continue;
                }
                if let Some(s) = self.translate(i, Slot::V(r)) {
                    self.prologue.push(s);
                }
            }
        }
    }

    // ---- The main code -----------------------------------------------------

    fn main(&mut self) {
        self.find_masked();
        let f = self.f;
        self.list(&f.body, &Ctx::TOP);
    }

    /// Marks values defined in a loop and used outside it.
    fn find_masked(&mut self) {
        // Loops are numbered in the order they start, so that the loops
        // inside loop `l` are numbered `l + 1` to `last[l]`; each block
        // records only its innermost loop (rather than all the loops around
        // it, which would take memory in proportion to blocks times depth).
        let mut innermost: Vec<Option<u32>> = vec![None; self.f.blocks.len()];
        let mut last: Vec<u32> = Vec::new();
        fn walk(list: &[Node], inside: Option<u32>, innermost: &mut [Option<u32>], last: &mut Vec<u32>) {
            for n in list {
                match n {
                    Node::Block(b) => innermost[b.0 as usize] = inside,
                    Node::If(i) => {
                        walk(&i.then, inside, innermost, last);
                        walk(&i.otherwise, inside, innermost, last);
                    }
                    Node::Loop(l) => {
                        let number = last.len() as u32;
                        last.push(number);
                        walk(&l.body, Some(number), innermost, last);
                        let end = last.len() as u32 - 1;
                        last[number as usize] = end;
                    }
                }
            }
        }
        walk(&self.f.body, None, &mut innermost, &mut last);
        let mut def_block = vec![Block(0); self.f.values.len()];
        for b in self.f.block_order() {
            for &i in &self.f.blocks[b.0 as usize].insts {
                for &r in &self.f.inst(i).results {
                    def_block[r.0 as usize] = b;
                }
            }
        }
        let mark = |v: Value, use_block: Block, masked: &mut Vec<bool>| {
            if self.uniform[v.0 as usize] {
                return;
            }
            // Used outside the innermost loop it is defined in.
            if let Some(l) = innermost[def_block[v.0 as usize].0 as usize] {
                let inside = innermost[use_block.0 as usize].is_some_and(|u| l <= u && u <= last[l as usize]);
                if !inside {
                    masked[v.0 as usize] = true;
                }
            }
        };
        let mut masked = core::mem::take(&mut self.masked);
        for b in self.f.block_order() {
            for &i in &self.f.blocks[b.0 as usize].insts {
                let inst = self.f.inst(i);
                if inst.op == InstOp::Phi {
                    // A φ operand is used at the end of its predecessor.
                    for (j, &a) in inst.args.iter().enumerate() {
                        if let Some(&p) = self.preds[b.0 as usize].get(j) {
                            mark(a, p, &mut masked);
                        }
                    }
                } else {
                    for &a in &inst.args {
                        mark(a, b, &mut masked);
                    }
                }
            }
        }
        fn conds(list: &[Node], f: &mut dyn FnMut(Value, Block)) {
            let mut prev = None;
            for n in list {
                match n {
                    Node::Block(b) => prev = Some(*b),
                    Node::If(i) => {
                        if let Some(p) = prev {
                            f(i.cond, p);
                        }
                        conds(&i.then, f);
                        conds(&i.otherwise, f);
                    }
                    Node::Loop(l) => conds(&l.body, f),
                }
            }
        }
        conds(&self.f.body, &mut |v, b| mark(v, b, &mut masked));
        self.masked = masked;
    }

    fn emit_value_inst(&mut self, i: InstId) {
        let inst = self.f.inst(i);
        match &inst.op {
            InstOp::Phi => {}
            InstOp::LoadInput { slot, comp } => {
                self.inputs.push((Input::Value { slot: *slot, comp: *comp }, inst.results[0]));
            }
            InstOp::LoadBuiltin(b) => self.inputs.push((Input::Builtin(*b), inst.results[0])),
            InstOp::StoreOutput { slot, comp } => {
                self.outputs.push((Output::Value { slot: *slot, comp: *comp }, inst.args[0]));
            }
            InstOp::StoreBuiltin(b) => self.outputs.push((Output::Builtin(*b), inst.args[0])),
            InstOp::Tex(op) => {
                if op.lod == crate::builtins::TexLod::Implicit {
                    self.derivatives = true;
                }
                let (index, args) = if op.dynamic {
                    (Some(Slot::V(inst.args[0])), inst.args[1..].iter().map(|&v| Slot::V(v)).collect())
                } else {
                    (None, inst.args.iter().map(|&v| Slot::V(v)).collect())
                };
                let results = self.def_slots(&inst.results);
                self.main.push(Sym::Tex { op: *op, index, args, results: results.clone() });
                self.finish_masked(&inst.results, &results);
            }
            InstOp::TexSize { sampler, index, dynamic, count } => {
                let (idx, lod) = if *dynamic {
                    (Some(Slot::V(inst.args[0])), Slot::V(inst.args[1]))
                } else {
                    (None, Slot::V(inst.args[0]))
                };
                let results = self.def_slots(&inst.results);
                self.main.push(Sym::TexSize {
                    sampler: *sampler,
                    sampler_index: *index,
                    count: *count,
                    index: idx,
                    lod,
                    results: results.clone(),
                });
                self.finish_masked(&inst.results, &results);
            }
            _ => {
                let Some(&r) = inst.results.first() else { return };
                if self.uniform[r.0 as usize] {
                    return;
                }
                let d = self.def_slots(&inst.results)[0];
                if let Some(s) = self.translate(i, d) {
                    self.main.push(s);
                    self.finish_masked(&inst.results, &[d]);
                }
            }
        }
    }

    /// Where results are first written: their own slot, or a scratch slot
    /// for masked values.
    fn def_slots(&mut self, results: &[Value]) -> Vec<Slot> {
        results
            .iter()
            .map(|&r| {
                if self.masked[r.0 as usize] {
                    self.scratch += 1;
                    Slot::Scratch(self.scratch - 1)
                } else {
                    Slot::V(r)
                }
            })
            .collect()
    }

    fn finish_masked(&mut self, results: &[Value], written: &[Slot]) {
        for (&r, &w) in results.iter().zip(written) {
            if w != Slot::V(r) {
                self.main.push(Sym::CopyMasked { d: Slot::V(r), s: w });
            }
        }
    }

    /// Copies into the φs of `succ` for the edge from `from`.
    fn phi_copies(&mut self, from: Block, succ: Block) {
        let Some(k) = crate::ir::edge_index(&self.succs, from, succ) else { return };
        let mut copies: Vec<(Value, Value)> = Vec::new();
        for &i in &self.f.blocks[succ.0 as usize].insts {
            let inst = self.f.inst(i);
            if inst.op != InstOp::Phi {
                break;
            }
            if let Some(&src) = inst.args.get(k) {
                let dst = inst.results[0];
                if src != dst {
                    copies.push((dst, src));
                }
            }
        }
        self.parallel_copies(copies);
    }

    /// Masked parallel copies, ordered so that no source is overwritten
    /// before it is read; a cycle saves one value in a scratch register.
    fn parallel_copies(&mut self, copies: Vec<(Value, Value)>) {
        let copies: Vec<(Slot, Slot)> = copies.into_iter().map(|(d, s)| (Slot::V(d), Slot::V(s))).collect();
        crate::ir::sequence_copies(&copies, |step| match step {
            CopyStep::Copy(d, s) => {
                self.main.push(Sym::CopyMasked { d, s });
                d
            }
            CopyStep::Save(d) => {
                self.scratch += 1;
                let t = Slot::Scratch(self.scratch - 1);
                self.main.push(Sym::Copy { d: t, s: d });
                t
            }
        });
    }

    /// Lays out a list of the structure. It recurses once per level of
    /// nesting (which may be deep once calls are inlined), so a block's work
    /// is done out of line, keeping each level's frame small.
    fn list(&mut self, list: &[Node], ctx: &Ctx) {
        for (k, n) in list.iter().enumerate() {
            match n {
                Node::Block(b) => self.block(*b, list.get(k + 1), ctx),
                Node::If(i) => {
                    let merge = match list.get(k + 1) {
                        Some(Node::Block(m)) => *m,
                        _ => Block(u32::MAX),
                    };
                    let inner = ctx.branch(merge);
                    self.main.push(Sym::If { c: self.slot(i.cond) });
                    self.list(&i.then, &inner);
                    self.main.push(Sym::Else);
                    self.list(&i.otherwise, &inner);
                    self.main.push(Sym::EndIf);
                }
                Node::Loop(l) => {
                    let header = crate::ir::first_block(&l.body).unwrap_or(Block(u32::MAX));
                    let exit = match list.get(k + 1) {
                        Some(Node::Block(m)) => *m,
                        _ => Block(u32::MAX),
                    };
                    let start = self.main.len();
                    self.main.push(Sym::Loop);
                    let inner = Ctx { end: End::Header(header), lp: Some((header, exit)) };
                    self.list(&l.body, &inner);
                    self.main.push(Sym::EndLoop);
                    self.loops.push((start, self.main.len() - 1));
                }
            }
        }
    }

    /// A block's instructions, its copies into its successors' φs and its
    /// jump.
    #[inline(never)]
    fn block(&mut self, b: Block, next: Option<&Node>, ctx: &Ctx) {
        let f = self.f;
        for &i in &f.blocks[b.0 as usize].insts {
            self.emit_value_inst(i);
        }
        match f.blocks[b.0 as usize].jump {
            Some(Jump::Break) => {
                if let Some(exit) = ctx.loop_exit() {
                    self.phi_copies(b, exit);
                }
                self.main.push(Sym::Break);
            }
            Some(Jump::Continue) => {
                if let Some(h) = ctx.loop_header() {
                    self.phi_copies(b, h);
                }
                self.main.push(Sym::Continue);
            }
            Some(Jump::Discard) => {
                self.discards = true;
                self.main.push(Sym::Discard);
            }
            None => match next {
                Some(Node::Loop(l)) => {
                    if let Some(h) = crate::ir::first_block(&l.body) {
                        self.phi_copies(b, h);
                    }
                }
                Some(_) => {}
                // The end of a list: into the merge block, or back to the
                // loop's header.
                None => match ctx.end {
                    End::Merge(merge) => self.phi_copies(b, merge),
                    End::Header(header) => self.phi_copies(b, header),
                    End::Top => {}
                },
            },
        }
    }

    // ---- Registers -----------------------------------------------------------

    fn finish(self) -> Program {
        let f = self.f;
        // Prologue registers: linear scan over the prologue, keeping the
        // values the main code (or an output) reads.
        let mut kept: Vec<bool> = vec![false; f.values.len()];
        for s in &self.main {
            for u in s.uses() {
                if let Slot::V(v) = u
                    && self.uniform[v.0 as usize]
                {
                    kept[v.0 as usize] = true;
                }
            }
        }
        for &(_, v) in &self.outputs {
            if self.uniform[v.0 as usize] {
                kept[v.0 as usize] = true;
            }
        }
        let mut regs: BTreeMap<Slot, usize> = BTreeMap::new();
        let pro_intervals = intervals(&self.prologue, &[], |s| matches!(s, Slot::V(v) if kept[v.0 as usize]));
        let uniform_registers = allocate(&pro_intervals, 0, &mut regs);
        // Inputs: registers of their own after the prologue's.
        let mut next = uniform_registers;
        for &(_, v) in &self.inputs {
            regs.entry(Slot::V(v)).or_insert_with(|| {
                next += 1;
                next - 1
            });
        }
        // Main code: linear scan, with values read by outputs kept to the
        // end.
        let outputs: BTreeSet<Value> = self.outputs.iter().map(|&(_, v)| v).collect();
        let main_intervals = intervals(&self.main, &self.loops, |s| match s {
            Slot::V(v) => outputs.contains(v),
            _ => false,
        });
        let main_intervals: Vec<(Slot, usize, usize)> =
            main_intervals.into_iter().filter(|(s, _, _)| !regs.contains_key(s)).collect();
        let registers = allocate(&main_intervals, next, &mut regs).max(next).max(1);
        // Register and lookup numbers must fit their 16 bits (they do in
        // every program linking accepts).
        let lookups = self.main.iter().filter(|s| matches!(s, Sym::Tex { .. } | Sym::TexSize { .. })).count();
        if registers > usize::from(Reg::MAX) + 1 || lookups > usize::from(u16::MAX) + 1 {
            return Program::empty();
        }
        let reg = |s: &Slot| -> Reg { regs.get(s).map_or(0, |&r| r as Reg) };
        let mut tex = Vec::new();
        let mut tex_size = Vec::new();
        let lower = |syms: &[Sym], tex: &mut Vec<TexIns>, tex_size: &mut Vec<TexSizeIns>| -> Vec<Ins> {
            // First pass: positions of structure instructions (for jump
            // targets).
            let mut out: Vec<Ins> = Vec::with_capacity(syms.len());
            let mut if_stack: Vec<(usize, Option<usize>)> = Vec::new();
            let mut loop_stack: Vec<usize> = Vec::new();
            for s in syms {
                let pc = out.len();
                let ins = match s {
                    Sym::Op { op, ty, d, a, b } => Ins::Op { op: *op, ty: *ty, d: reg(d), a: reg(a), b: reg(b) },
                    Sym::Select { d, c, a, b } => Ins::Select { d: reg(d), c: reg(c), a: reg(a), b: reg(b) },
                    Sym::Const { d, bits } => Ins::Const { d: reg(d), bits: *bits },
                    Sym::Copy { d, s } => Ins::Copy { d: reg(d), s: reg(s) },
                    Sym::CopyMasked { d, s } => Ins::CopyMasked { d: reg(d), s: reg(s) },
                    Sym::LoadUniform { d, slot, comp } => Ins::LoadUniform { d: reg(d), slot: *slot, comp: *comp },
                    Sym::LoadUniformIndexed { d, base, comp, stride, count, index } => Ins::LoadUniformIndexed {
                        d: reg(d),
                        base: *base,
                        comp: *comp,
                        stride: *stride,
                        count: *count,
                        index: reg(index),
                    },
                    Sym::LoadBlock { d, block, offset } => Ins::LoadBlock { d: reg(d), block: *block, offset: *offset },
                    Sym::LoadBlockIndexed { d, block, offset, stride, count, index } => Ins::LoadBlockIndexed {
                        d: reg(d),
                        block: *block,
                        offset: *offset,
                        stride: *stride,
                        count: *count,
                        index: reg(index),
                    },
                    Sym::Tex { op, index, args, results } => {
                        tex.push(TexIns {
                            op: *op,
                            index: index.as_ref().map(reg),
                            args: args.iter().map(reg).collect(),
                            results: results.iter().map(reg).collect(),
                        });
                        Ins::Tex { op: (tex.len() - 1) as u16 }
                    }
                    Sym::TexSize { sampler, sampler_index, count, index, lod, results } => {
                        tex_size.push(TexSizeIns {
                            sampler: *sampler,
                            sampler_index: *sampler_index,
                            count: *count,
                            index: index.as_ref().map(reg),
                            lod: reg(lod),
                            results: results.iter().map(reg).collect(),
                        });
                        Ins::TexSize { op: (tex_size.len() - 1) as u16 }
                    }
                    Sym::Deriv { d, a, y } => Ins::Deriv { d: reg(d), a: reg(a), y: *y },
                    Sym::If { c } => {
                        if_stack.push((pc, None));
                        Ins::If { c: reg(c), else_pc: 0 }
                    }
                    Sym::Else => {
                        if let Some(top) = if_stack.last_mut() {
                            top.1 = Some(pc);
                        }
                        Ins::Else { end_pc: 0 }
                    }
                    Sym::EndIf => {
                        if let Some((if_pc, Some(else_pc))) = if_stack.pop() {
                            // The If jumps past its Else (into the else
                            // branch); the Else jumps to this EndIf.
                            if let Ins::If { else_pc: t, .. } = &mut out[if_pc] {
                                *t = else_pc as u32;
                            }
                            if let Ins::Else { end_pc } = &mut out[else_pc] {
                                *end_pc = pc as u32;
                            }
                        }
                        Ins::EndIf
                    }
                    Sym::Loop => {
                        loop_stack.push(pc);
                        Ins::Loop
                    }
                    Sym::EndLoop => {
                        let start = loop_stack.pop().unwrap_or(0);
                        Ins::EndLoop { start_pc: start as u32 }
                    }
                    Sym::Break => Ins::Break,
                    Sym::Continue => Ins::Continue,
                    Sym::Discard => Ins::Discard,
                };
                out.push(ins);
            }
            out
        };
        let prologue = lower(&self.prologue, &mut tex, &mut tex_size);
        let main = lower(&self.main, &mut tex, &mut tex_size);
        let inputs = self.inputs.iter().map(|&(i, v)| (i, reg(&Slot::V(v)))).collect();
        let outputs = self.outputs.iter().map(|&(o, v)| (o, reg(&Slot::V(v)))).collect();
        Program {
            prologue,
            main,
            tex,
            tex_size,
            registers,
            uniform_registers,
            inputs,
            outputs,
            discards: self.discards,
            derivatives: self.derivatives,
        }
    }
}

/// Where a list ends, and the innermost loop around it (for `break` and
/// `continue`, however deep in branches they are).
#[derive(Clone, Copy)]
struct Ctx {
    end: End,
    /// The innermost loop's header and exit blocks.
    lp: Option<(Block, Block)>,
}

/// Where control goes at the end of a list.
#[derive(Clone, Copy)]
enum End {
    /// The function's end.
    Top,
    /// An if's merge block.
    Merge(Block),
    /// Back to a loop's header.
    Header(Block),
}

impl Ctx {
    const TOP: Ctx = Ctx { end: End::Top, lp: None };

    fn branch(&self, merge: Block) -> Ctx {
        Ctx { end: End::Merge(merge), lp: self.lp }
    }

    fn loop_exit(&self) -> Option<Block> {
        self.lp.map(|(_, e)| e)
    }

    fn loop_header(&self) -> Option<Block> {
        self.lp.map(|(h, _)| h)
    }
}

/// Live intervals (first and last position) of every slot in `code`;
/// `keep` slots live to the end. A slot live into a loop it was defined
/// before (or used across iterations) lives to the loop's end, and one
/// defined in a loop and used after it lives through the whole loop
/// (lanes that left early keep their value while the others go on).
fn intervals(code: &[Sym], loops: &[(usize, usize)], keep: impl Fn(&Slot) -> bool) -> Vec<(Slot, usize, usize)> {
    let mut span: BTreeMap<Slot, (usize, usize)> = BTreeMap::new();
    for (pc, s) in code.iter().enumerate() {
        for x in s.defs().into_iter().chain(s.uses()) {
            let e = span.entry(x).or_insert((pc, pc));
            e.0 = e.0.min(pc);
            e.1 = e.1.max(pc);
        }
    }
    let end = code.len();
    // The innermost loop around each position, and each loop's parent
    // (loops nest: they are the structure's).
    let mut order: Vec<usize> = (0..loops.len()).collect();
    order.sort_by_key(|&i| loops[i].0);
    let mut innermost: Vec<Option<usize>> = vec![None; end];
    let mut parent: Vec<Option<usize>> = vec![None; loops.len()];
    let mut open: Vec<usize> = Vec::new();
    let mut next = 0;
    for (pc, slot) in innermost.iter_mut().enumerate() {
        while open.last().is_some_and(|&l| loops[l].1 < pc) {
            open.pop();
        }
        while let Some(&l) = order.get(next).filter(|&&l| loops[l].0 == pc) {
            parent[l] = open.last().copied();
            open.push(l);
            next += 1;
        }
        *slot = open.last().copied();
    }
    // The outermost loop around position `p` that is not around `q`.
    let outermost = |p: usize, q: usize| -> Option<usize> {
        let mut found = None;
        let mut l = innermost.get(p).copied().flatten();
        while let Some(x) = l {
            let (ls, le) = loops[x];
            if ls <= q && q <= le {
                break;
            }
            found = Some(x);
            l = parent[x];
        }
        found
    };
    let mut out: Vec<(Slot, usize, usize)> = span
        .into_iter()
        .map(|(s, (a, b))| {
            let b = if keep(&s) { end } else { b };
            // Each end moves out of the loops around it that are not
            // around the other; that is already where the other's moves
            // leave it.
            let nb = outermost(b, a).map_or(b, |l| loops[l].1);
            let na = outermost(a, b).map_or(a, |l| loops[l].0);
            (s, na, nb)
        })
        .collect();
    out.sort_by_key(|&(_, a, _)| a);
    out
}

/// Linear-scan allocation from register `first`; returns one past the
/// highest register used.
fn allocate(intervals: &[(Slot, usize, usize)], first: usize, regs: &mut BTreeMap<Slot, usize>) -> usize {
    // Intervals still live, by end (earliest first), and registers free.
    let mut active: BinaryHeap<Reverse<(usize, usize)>> = BinaryHeap::new();
    let mut free: Vec<usize> = Vec::new();
    let mut next = first;
    for &(slot, start, end) in intervals {
        // Expire intervals that ended before this one starts.
        while let Some(&Reverse((e, r))) = active.peek() {
            if e >= start {
                break;
            }
            active.pop();
            free.push(r);
        }
        let r = free.pop().unwrap_or_else(|| {
            next += 1;
            next - 1
        });
        regs.insert(slot, r);
        active.push(Reverse((end, r)));
    }
    next
}
