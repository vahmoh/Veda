// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Checks the IR's invariants. Linking checks every stage it builds
//! ([`crate::program::link`]): a stage that fails is a compiler bug, and
//! the program is refused rather than run.
//!
//! [`verify`]:
//!
//! * Lists alternate blocks and `if`/`loop` nodes, starting and ending
//!   with a block; a block that jumps ends its list.
//! * Every φ has one operand per predecessor and sits at its block's start.
//! * Operands have the types their operation takes.
//! * Every use is dominated by its definition (a φ operand by the end of
//!   the corresponding predecessor).
//!
//! [`bounds`]: everything the code reads or writes is the program's: its
//! uniform slots, blocks (inside their size), samplers, inputs and outputs,
//! so that neither back end ever reads or declares anything beyond them.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use super::{Block, Func, InstOp, Node, ty_name};
use crate::Stage;
use crate::builtins::Limits;
use crate::link::Linked;
use crate::ops::Ty;

/// Checks `f`; returns the first problem found.
pub fn verify(f: &Func) -> Result<(), String> {
    structure(f, &f.body)?;
    let preds = f.preds();
    let order = f.block_order();
    let idom = dominators(f, &preds);
    // Where each value is defined: block and position.
    let mut def_at: Vec<Option<(Block, usize)>> = vec![None; f.values.len()];
    for &b in &order {
        for (pos, &i) in f.blocks[b.0 as usize].insts.iter().enumerate() {
            for &r in &f.insts[i.0 as usize].results {
                if def_at[r.0 as usize].is_some() {
                    return Err(alloc::format!("%{} is defined twice", r.0));
                }
                def_at[r.0 as usize] = Some((b, pos));
            }
        }
    }
    // Each block's interval in a depth-first walk of the dominator tree: a
    // dominates b if b's interval lies in a's (constant time, where walking
    // up the tree from every use would take time quadratic in the blocks).
    // A block the entry does not reach counts as dominated only by itself.
    let span = dominance_intervals(f, &order, &idom);
    let dominates = |a: Block, b: Block| -> bool {
        let (sa, sb) = (span[a.0 as usize], span[b.0 as usize]);
        match sb {
            UNREACHED => a == b,
            _ => sa.0 <= sb.0 && sb.1 <= sa.1,
        }
    };
    for &b in &order {
        let reachable = idom[b.0 as usize].is_some() || Some(b) == super::first_block(&f.body);
        let insts = &f.blocks[b.0 as usize].insts;
        let mut phis_done = false;
        for (pos, &i) in insts.iter().enumerate() {
            let inst = &f.insts[i.0 as usize];
            if inst.op == InstOp::Phi {
                if phis_done {
                    return Err(alloc::format!("b{}: φ after other instructions", b.0));
                }
                let n = preds[b.0 as usize].len();
                if reachable && inst.args.len() != n {
                    return Err(alloc::format!(
                        "b{}: φ %{} has {} operands for {} predecessors",
                        b.0,
                        inst.results[0].0,
                        inst.args.len(),
                        n
                    ));
                }
                if !reachable {
                    continue;
                }
                for (k, &a) in inst.args.iter().enumerate() {
                    let Some((db, _)) = def_at[a.0 as usize] else {
                        return Err(alloc::format!("b{}: φ operand %{} is not defined", b.0, a.0));
                    };
                    let p = preds[b.0 as usize][k];
                    if !dominates(db, p) {
                        return Err(alloc::format!(
                            "b{}: φ operand %{} does not dominate predecessor b{}",
                            b.0,
                            a.0,
                            p.0
                        ));
                    }
                }
                continue;
            }
            phis_done = true;
            if !reachable {
                continue;
            }
            for &a in &inst.args {
                let Some((db, dpos)) = def_at[a.0 as usize] else {
                    return Err(alloc::format!("b{}: %{} is used but not defined", b.0, a.0));
                };
                let ok = if db == b { dpos < pos } else { dominates(db, b) };
                if !ok {
                    return Err(alloc::format!(
                        "b{}: the use of %{} is not dominated by its definition (b{})",
                        b.0,
                        a.0,
                        db.0
                    ));
                }
            }
            types(f, i)?;
        }
    }
    // If conditions are bools defined before the `if`.
    fn conds(f: &Func, list: &[Node], def_at: &[Option<(Block, usize)>]) -> Result<(), String> {
        for n in list {
            match n {
                Node::Block(_) => {}
                Node::If(i) => {
                    if f.ty(i.cond) != Ty::Bool {
                        return Err(alloc::format!("if %{} is not a bool", i.cond.0));
                    }
                    if def_at[i.cond.0 as usize].is_none() {
                        return Err(alloc::format!("if %{} is not defined", i.cond.0));
                    }
                    conds(f, &i.then, def_at)?;
                    conds(f, &i.otherwise, def_at)?;
                }
                Node::Loop(l) => conds(f, &l.body, def_at)?,
            }
        }
        Ok(())
    }
    conds(f, &f.body, &def_at)
}

fn structure(f: &Func, list: &[Node]) -> Result<(), String> {
    if list.is_empty() {
        return Err("an empty list".into());
    }
    for (i, n) in list.iter().enumerate() {
        let is_block = matches!(n, Node::Block(_));
        if (i % 2 == 0) != is_block {
            return Err("lists must alternate blocks and if/loop nodes".into());
        }
        match n {
            Node::Block(b) => {
                if f.blocks[b.0 as usize].jump.is_some() && i + 1 != list.len() {
                    return Err(alloc::format!("b{} jumps but is not the last of its list", b.0));
                }
            }
            Node::If(x) => {
                structure(f, &x.then)?;
                structure(f, &x.otherwise)?;
            }
            Node::Loop(l) => structure(f, &l.body)?,
        }
    }
    if !matches!(list.last(), Some(Node::Block(_))) {
        return Err("a list must end with a block".into());
    }
    Ok(())
}

/// Immediate dominators, by Lengauer and Tarjan's method (with path
/// compression), which takes time near linear in the blocks and edges
/// whatever the code's shape: the simpler iterative method takes time
/// quadratic in them for a loop that many `break`s leave. Blocks the entry
/// does not reach, and the entry, have none.
pub fn dominators(f: &Func, preds: &[Vec<Block>]) -> Vec<Option<Block>> {
    let mut idom: Vec<Option<Block>> = vec![None; f.blocks.len()];
    let Some(entry) = super::first_block(&f.body) else { return idom };
    let succs = super::successors(preds);
    // Number the blocks the entry reaches in depth-first order, without
    // recursion (the walk may go as deep as there are blocks). From here
    // on, a block is known by its number.
    let mut number = vec![NONE; f.blocks.len()];
    let mut block: Vec<Block> = vec![entry];
    let mut parent: Vec<u32> = vec![NONE];
    number[entry.0 as usize] = 0;
    let mut stack: Vec<(Block, usize)> = vec![(entry, 0)];
    while let Some(&mut (b, ref mut next)) = stack.last_mut() {
        let Some(&(s, _)) = succs[b.0 as usize].get(*next) else {
            stack.pop();
            continue;
        };
        *next += 1;
        if number[s.0 as usize] == NONE {
            number[s.0 as usize] = block.len() as u32;
            parent.push(number[b.0 as usize]);
            block.push(s);
            stack.push((s, 0));
        }
    }
    let n = block.len();
    let mut semi: Vec<u32> = (0..n as u32).collect();
    let mut forest = Forest { ancestor: vec![NONE; n], label: (0..n as u32).collect(), path: Vec::new() };
    let mut dom: Vec<u32> = vec![0; n];
    let mut bucket: Vec<Vec<u32>> = vec![Vec::new(); n];
    for w in (1..n).rev() {
        for p in &preds[block[w].0 as usize] {
            let v = number[p.0 as usize];
            if v != NONE {
                let u = forest.eval(v, &semi);
                semi[w] = semi[w].min(semi[u as usize]);
            }
        }
        bucket[semi[w] as usize].push(w as u32);
        let p = parent[w];
        forest.ancestor[w] = p;
        for v in core::mem::take(&mut bucket[p as usize]) {
            let u = forest.eval(v, &semi);
            dom[v as usize] = if semi[u as usize] < semi[v as usize] { u } else { p };
        }
    }
    for w in 1..n {
        if dom[w] != semi[w] {
            dom[w] = dom[dom[w] as usize];
        }
        idom[block[w].0 as usize] = Some(block[dom[w] as usize]);
    }
    idom
}

/// No block (number).
const NONE: u32 = u32::MAX;

/// The forest of blocks [`dominators`] has linked so far.
struct Forest {
    ancestor: Vec<u32>,
    /// The block of least semidominator on the (compressed) path above.
    label: Vec<u32>,
    /// Scratch space for [`Forest::eval`].
    path: Vec<u32>,
}

impl Forest {
    /// The block of least semidominator on the path from `v` up to (not
    /// including) its tree's root; the path is compressed on the way, so
    /// that later questions take fewer steps.
    fn eval(&mut self, v: u32, semi: &[u32]) -> u32 {
        if self.ancestor[v as usize] == NONE {
            return v;
        }
        // Every block on the path up from `v` whose ancestor is not the
        // root is pointed straight at the root, from the top down, taking
        // the least label above it on the way (iteratively, where the
        // method is usually written recursively).
        self.path.clear();
        let mut x = v;
        while self.ancestor[self.ancestor[x as usize] as usize] != NONE {
            self.path.push(x);
            x = self.ancestor[x as usize];
        }
        for &y in self.path.iter().rev() {
            let a = self.ancestor[y as usize];
            if semi[self.label[a as usize] as usize] < semi[self.label[y as usize] as usize] {
                self.label[y as usize] = self.label[a as usize];
            }
            self.ancestor[y as usize] = self.ancestor[a as usize];
        }
        self.label[v as usize]
    }
}

/// The interval [`dominance_intervals`] gives a block the entry does not
/// reach: empty, so that it contains no other block's.
const UNREACHED: (usize, usize) = (usize::MAX, 0);

/// Each block's interval in a depth-first walk of the dominator tree: its
/// own number and the last number of the blocks it dominates.
fn dominance_intervals(f: &Func, order: &[Block], idom: &[Option<Block>]) -> Vec<(usize, usize)> {
    let mut children: Vec<Vec<Block>> = vec![Vec::new(); f.blocks.len()];
    for &b in order {
        if let Some(p) = idom[b.0 as usize] {
            children[p.0 as usize].push(b);
        }
    }
    let mut span = vec![UNREACHED; f.blocks.len()];
    let Some(entry) = super::first_block(&f.body) else { return span };
    let mut counter = 0;
    // Without recursion: the tree may be as deep as there are blocks.
    let mut stack: Vec<(Block, usize)> = vec![(entry, 0)];
    span[entry.0 as usize].0 = counter;
    while let Some(&mut (b, ref mut next)) = stack.last_mut() {
        if let Some(&c) = children[b.0 as usize].get(*next) {
            *next += 1;
            counter += 1;
            span[c.0 as usize].0 = counter;
            stack.push((c, 0));
        } else {
            span[b.0 as usize].1 = counter;
            stack.pop();
        }
    }
    span
}

fn types(f: &Func, id: super::InstId) -> Result<(), String> {
    let inst = &f.insts[id.0 as usize];
    let bad = |what: &str| Err(alloc::format!("instruction %{:?}: {what}", inst.results.first().map(|r| r.0)));
    match &inst.op {
        InstOp::Op(op) => {
            let sig = op.signature();
            if inst.args.len() != sig.args.len() {
                return bad("wrong number of operands");
            }
            for (&a, &t) in inst.args.iter().zip(sig.args) {
                let at = f.ty(a);
                let ok = match t {
                    Ty::I32 => matches!(at, Ty::I32 | Ty::U32) || *op == crate::ops::Op::Bitcast,
                    other => at == other,
                };
                if !ok {
                    return bad(&alloc::format!("{op:?} takes {} but has {}", ty_name(t), ty_name(at)));
                }
            }
        }
        InstOp::Select
            if inst.args.len() != 3 || f.ty(inst.args[0]) != Ty::Bool || f.ty(inst.args[1]) != f.ty(inst.args[2]) =>
        {
            return bad("malformed select");
        }
        _ => {}
    }
    Ok(())
}

/// Checks that every uniform slot, block offset, sampler, input and output
/// the code of `f` names is within the linked program's.
pub fn bounds(f: &Func, linked: &Linked, limits: &Limits) -> Result<(), String> {
    let vertex = f.stage == Stage::Vertex;
    // The last index a run of `count` from `first`, `stride` apart, reaches.
    let last = |first: u32, stride: u32, count: u32| -> Option<u32> {
        count.checked_sub(1)?.checked_mul(stride)?.checked_add(first)
    };
    for b in &f.blocks {
        for &i in &b.insts {
            let ok = match f.inst(i).op {
                InstOp::LoadUniform { slot, comp } => comp < 4 && slot < linked.slots,
                InstOp::LoadUniformIndexed { base, comp, stride, count } => {
                    comp < 4 && last(base, stride, count).is_some_and(|l| l < linked.slots)
                }
                InstOp::LoadBlock { block, offset, stride, count } => {
                    let size = linked.blocks.get(block as usize).map_or(0, |b| b.size);
                    let end = if count == 0 { Some(offset) } else { last(offset, stride, count) };
                    end.and_then(|e| e.checked_add(4)).is_some_and(|e| e <= size)
                }
                InstOp::Tex(op) => last(op.index, 1, op.count).is_some_and(|l| (l as usize) < linked.samplers.len()),
                InstOp::TexSize { index, count, .. } => {
                    last(index, 1, count).is_some_and(|l| (l as usize) < linked.samplers.len())
                }
                InstOp::LoadInput { slot, comp } => {
                    comp < 4 && if vertex { slot < limits.max_vertex_attribs } else { slot < linked.varying_slots }
                }
                InstOp::StoreOutput { slot, comp } => {
                    comp < 4 && if vertex { slot < linked.varying_slots } else { slot < limits.max_draw_buffers }
                }
                _ => true,
            };
            if !ok {
                return Err(alloc::format!("{:?} is outside the program's storage", f.inst(i).op));
            }
        }
    }
    Ok(())
}
