// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Running bytecode.

use alloc::vec;
use alloc::vec::Vec;

use super::{ALL, Ins, LANES, MAX_LOOP_WORK, Mask, Program};
use crate::ir::TexOp;
use crate::ops::{self, Op, Ty};

/// The most arguments a texture lookup has: four coordinates and two
/// three-component gradients.
const MAX_TEX_ARGS: usize = 10;

/// One register: [`LANES`] 32-bit lanes.
#[derive(Clone, Copy, PartialEq, Debug)]
#[repr(C, align(64))]
pub struct Lanes(pub [u32; LANES]);

impl Lanes {
    pub const ZERO: Lanes = Lanes([0; LANES]);

    pub fn splat(bits: u32) -> Lanes {
        Lanes([bits; LANES])
    }

    pub fn from_f32(v: [f32; LANES]) -> Lanes {
        Lanes(v.map(f32::to_bits))
    }

    pub fn f32(&self, i: usize) -> f32 {
        f32::from_bits(self.0[i])
    }
}

/// Texture lookups, which the renderer provides.
pub trait Textures {
    /// Looks up a texture for the lanes in `mask`: `index` is each lane's
    /// sampler index; `args` the coordinates then the bias, level or
    /// gradients (as [`TexOp`] says); `out` receives one register per
    /// result. Fragment shaders' lanes are four 2x2 quads (top-left,
    /// top-right, bottom-left, bottom-right), from which implicit levels of
    /// detail come.
    fn sample(&self, op: &TexOp, index: &[u32; LANES], args: &[Lanes], mask: Mask, out: &mut [Lanes]);

    /// A texture's size at a level: width, height, depth or layers.
    fn size(&self, sampler: u32, lod: i32) -> [i32; 3];
}

/// Textures for programs that use none.
pub struct NoTextures;

impl Textures for NoTextures {
    fn sample(&self, _: &TexOp, _: &[u32; LANES], _: &[Lanes], _: Mask, out: &mut [Lanes]) {
        out.fill(Lanes::ZERO);
    }

    fn size(&self, _: u32, _: i32) -> [i32; 3] {
        [0; 3]
    }
}

/// What a program reads besides its inputs.
pub struct Env<'a> {
    /// Default-block uniform storage.
    pub uniforms: &'a [[u32; 4]],
    /// Each uniform block's buffer contents (by program block index).
    pub blocks: &'a [&'a [u8]],
    pub textures: &'a dyn Textures,
}

struct IfFrame {
    saved: Mask,
    else_mask: Mask,
}

struct LoopFrame {
    entry: Mask,
    brk: Mask,
    cont: Mask,
}

/// A register file and the state to run a program on it.
pub struct Exec {
    pub regs: Vec<Lanes>,
    ifs: Vec<IfFrame>,
    loops: Vec<LoopFrame>,
    /// A run went round its loops more than [`MAX_LOOP_WORK`] allows, and
    /// left them early.
    pub runaway: bool,
}

/// An index computed at run time (a signed int), clamped to `0..count`.
fn clamp_index(index: u32, count: u32) -> u32 {
    if (index as i32) < 0 { 0 } else { index.min(count.saturating_sub(1)) }
}

impl Exec {
    /// A register file for `p`.
    pub fn new(p: &Program) -> Exec {
        Exec { regs: vec![Lanes::ZERO; p.registers], ifs: Vec::new(), loops: Vec::new(), runaway: false }
    }

    /// Runs the prologue (once per draw).
    pub fn prologue(&mut self, p: &Program, env: &Env<'_>) {
        if self.regs.len() < p.registers {
            self.regs.resize(p.registers, Lanes::ZERO);
        }
        self.run_code(&p.prologue, p, ALL, env);
    }

    /// Runs the main code for the lanes in `mask` (inputs already in their
    /// registers); returns the lanes not discarded.
    pub fn run(&mut self, p: &Program, mask: Mask, env: &Env<'_>) -> Mask {
        self.run_code(&p.main, p, mask, env)
    }

    fn run_code(&mut self, code: &[Ins], p: &Program, mask: Mask, env: &Env<'_>) -> Mask {
        self.ifs.clear();
        self.loops.clear();
        let mut exec = mask;
        let mut alive = mask;
        let mut pc = 0usize;
        // Instructions run again going round loops (see MAX_LOOP_WORK).
        let mut work = 0u64;
        while pc < code.len() {
            match code[pc] {
                Ins::If { c, else_pc } => {
                    let cm = truth(&self.regs[c as usize]) & exec;
                    self.ifs.push(IfFrame { saved: exec, else_mask: exec & !cm });
                    exec = cm;
                    if exec == 0 {
                        pc = else_pc as usize;
                        continue;
                    }
                }
                Ins::Else { end_pc } => {
                    let running = self.running(alive);
                    exec = self.ifs.last().map_or(0, |f| f.else_mask) & running;
                    if exec == 0 {
                        pc = end_pc as usize;
                        continue;
                    }
                }
                Ins::EndIf => {
                    let saved = self.ifs.pop().map_or(0, |f| f.saved);
                    exec = saved & self.running(alive);
                }
                Ins::Loop => {
                    self.loops.push(LoopFrame { entry: exec, brk: 0, cont: 0 });
                }
                Ins::EndLoop { start_pc } => {
                    let Some(l) = self.loops.last_mut() else { break };
                    let next = l.entry & alive & !l.brk;
                    l.cont = 0;
                    if next != 0 {
                        let body = pc.saturating_sub(start_pc as usize) as u64;
                        if work + body <= MAX_LOOP_WORK {
                            work += body;
                            exec = next;
                            pc = start_pc as usize + 1;
                            continue;
                        }
                        self.runaway = true;
                    }
                    let entry = l.entry;
                    self.loops.pop();
                    exec = entry & self.running(alive);
                }
                Ins::Break => {
                    if let Some(l) = self.loops.last_mut() {
                        l.brk |= exec;
                    }
                    exec = 0;
                }
                Ins::Continue => {
                    if let Some(l) = self.loops.last_mut() {
                        l.cont |= exec;
                    }
                    exec = 0;
                }
                Ins::Discard => {
                    alive &= !exec;
                    exec = 0;
                }
                ins => self.step(ins, p, exec, env),
            }
            pc += 1;
        }
        alive
    }

    /// The lanes still running in the current loop iteration.
    fn running(&self, alive: Mask) -> Mask {
        match self.loops.last() {
            Some(l) => alive & !(l.brk | l.cont),
            None => alive,
        }
    }

    fn step(&mut self, ins: Ins, p: &Program, exec: Mask, env: &Env<'_>) {
        match ins {
            Ins::Op { op, ty, d, a, b } => {
                let (x, y) = (self.regs[a as usize].0, self.regs[b as usize].0);
                self.regs[d as usize].0 = apply(op, ty, &x, &y);
            }
            Ins::Select { d, c, a, b } => {
                let (cv, x, y) = (self.regs[c as usize].0, self.regs[a as usize].0, self.regs[b as usize].0);
                let mut out = [0u32; LANES];
                for i in 0..LANES {
                    // Bools are all ones or zero.
                    let m = 0u32.wrapping_sub(u32::from(cv[i] != 0));
                    out[i] = (x[i] & m) | (y[i] & !m);
                }
                self.regs[d as usize].0 = out;
            }
            Ins::Const { d, bits } => self.regs[d as usize] = Lanes::splat(bits),
            Ins::Copy { d, s } => self.regs[d as usize] = self.regs[s as usize],
            Ins::CopyMasked { d, s } => {
                let (src, dst) = (self.regs[s as usize].0, self.regs[d as usize].0);
                let mut out = [0u32; LANES];
                for i in 0..LANES {
                    let m = 0u32.wrapping_sub((exec >> i) & 1);
                    out[i] = (src[i] & m) | (dst[i] & !m);
                }
                self.regs[d as usize].0 = out;
            }
            Ins::LoadUniform { d, slot, comp } => {
                let v = env.uniforms.get(slot as usize).map_or(0, |s| s[comp as usize & 3]);
                self.regs[d as usize] = Lanes::splat(v);
            }
            Ins::LoadUniformIndexed { d, base, comp, stride, count, index } => {
                let idx = self.regs[index as usize].0;
                let mut out = [0u32; LANES];
                for i in 0..LANES {
                    // Clamped to the variable, as a signed index.
                    let slot = u64::from(base) + u64::from(clamp_index(idx[i], count)) * u64::from(stride);
                    let row = usize::try_from(slot).ok().and_then(|s| env.uniforms.get(s));
                    out[i] = row.map_or(0, |s| s[comp as usize & 3]);
                }
                self.regs[d as usize].0 = out;
            }
            Ins::LoadBlock { d, block, offset } => {
                let data = env.blocks.get(block as usize).copied().unwrap_or(&[]);
                self.regs[d as usize] = Lanes::splat(read_word(data, u64::from(offset)));
            }
            Ins::LoadBlockIndexed { d, block, offset, stride, count, index } => {
                let data = env.blocks.get(block as usize).copied().unwrap_or(&[]);
                let idx = self.regs[index as usize].0;
                let mut out = [0u32; LANES];
                for i in 0..LANES {
                    // The byte offset, clamped inside the block.
                    let at = u64::from(offset) + u64::from(clamp_index(idx[i], count)) * u64::from(stride);
                    out[i] = read_word(data, at);
                }
                self.regs[d as usize].0 = out;
            }
            Ins::Tex { op } => {
                let t = &p.tex[op as usize];
                // A computed index is into the run of `count` samplers from
                // `index`.
                let index = match t.index {
                    Some(r) => self.regs[r as usize].0.map(|x| t.op.index.saturating_add(clamp_index(x, t.op.count))),
                    None => [t.op.index; LANES],
                };
                // Coordinates (up to four), then a bias, a level or two
                // gradients of up to three components.
                let mut args = [Lanes::ZERO; MAX_TEX_ARGS];
                let na = t.args.len().min(MAX_TEX_ARGS);
                for (a, &r) in args.iter_mut().zip(&t.args[..na]) {
                    *a = self.regs[r as usize];
                }
                let mut out = [Lanes::ZERO; 4];
                let n = t.results.len().min(4);
                env.textures.sample(&t.op, &index, &args[..na], exec, &mut out[..n]);
                for (k, &r) in t.results.iter().enumerate().take(4) {
                    self.regs[r as usize] = out[k];
                }
            }
            Ins::TexSize { op } => {
                let t = &p.tex_size[op as usize];
                let lod = self.regs[t.lod as usize].0;
                let index = t.index.map(|r| self.regs[r as usize].0);
                let mut out = [[0u32; LANES]; 3];
                for i in 0..LANES {
                    if exec & (1 << i) == 0 {
                        continue;
                    }
                    let s =
                        index.map_or(t.sampler_index, |v| t.sampler_index.saturating_add(clamp_index(v[i], t.count)));
                    let size = env.textures.size(s, lod[i] as i32);
                    for c in 0..3 {
                        out[c][i] = size[c] as u32;
                    }
                }
                for (k, &r) in t.results.iter().enumerate().take(3) {
                    self.regs[r as usize].0 = out[k];
                }
            }
            Ins::Deriv { d, a, y } => {
                let v = self.regs[a as usize].0;
                let mut out = [0u32; LANES];
                for q in 0..LANES / 4 {
                    let b = q * 4;
                    let p = |i: usize| f32::from_bits(v[b + i]);
                    let (d0, d1) = if y {
                        // Columns: bottom minus top.
                        (p(2) - p(0), p(3) - p(1))
                    } else {
                        // Rows: right minus left.
                        (p(1) - p(0), p(3) - p(2))
                    };
                    if y {
                        out[b] = d0.to_bits();
                        out[b + 2] = d0.to_bits();
                        out[b + 1] = d1.to_bits();
                        out[b + 3] = d1.to_bits();
                    } else {
                        out[b] = d0.to_bits();
                        out[b + 1] = d0.to_bits();
                        out[b + 2] = d1.to_bits();
                        out[b + 3] = d1.to_bits();
                    }
                }
                self.regs[d as usize].0 = out;
            }
            _ => {}
        }
    }
}

/// The 32-bit little-endian word at byte `at` of a block's buffer (0 past
/// its end).
fn read_word(data: &[u8], at: u64) -> u32 {
    let word = usize::try_from(at).ok().and_then(|o| data.get(o..o.checked_add(4)?));
    word.map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// The lanes whose bool register is true.
fn truth(r: &Lanes) -> Mask {
    let mut m = 0;
    for i in 0..LANES {
        m |= u32::from(r.0[i] != 0) << i;
    }
    m
}

type L = [u32; LANES];

#[inline(always)]
fn map1(a: &L, g: impl Fn(u32) -> u32) -> L {
    let mut d = [0u32; LANES];
    for i in 0..LANES {
        d[i] = g(a[i]);
    }
    d
}

#[inline(always)]
fn map2(a: &L, c: &L, g: impl Fn(u32, u32) -> u32) -> L {
    let mut d = [0u32; LANES];
    for i in 0..LANES {
        d[i] = g(a[i], c[i]);
    }
    d
}

/// `op` on every lane: [`ops::bits`]' definition of it, applied lane by
/// lane (the operation chosen once, not per lane).
pub(crate) fn apply(op: Op, ty: Ty, a: &L, c: &L) -> L {
    use Op::*;
    use ops::bits::*;
    match op {
        FNeg => map1(a, f_neg),
        FAbs => map1(a, f_abs),
        FSign => map1(a, f_sign),
        FFloor => map1(a, f_floor),
        FCeil => map1(a, f_ceil),
        FTrunc => map1(a, f_trunc),
        FRoundEven => map1(a, f_round_even),
        FFract => map1(a, f_fract),
        FSqrt => map1(a, f_sqrt),
        FRsq => map1(a, f_rsq),
        FExp => map1(a, f_exp),
        FLog => map1(a, f_log),
        FExp2 => map1(a, f_exp2),
        FLog2 => map1(a, f_log2),
        FSin => map1(a, f_sin),
        FCos => map1(a, f_cos),
        FTan => map1(a, f_tan),
        FAsin => map1(a, f_asin),
        FAcos => map1(a, f_acos),
        FAtan => map1(a, f_atan),
        FSinh => map1(a, f_sinh),
        FCosh => map1(a, f_cosh),
        FTanh => map1(a, f_tanh),
        FAsinh => map1(a, f_asinh),
        FAcosh => map1(a, f_acosh),
        FAtanh => map1(a, f_atanh),
        FAdd => map2(a, c, f_add),
        FSub => map2(a, c, f_sub),
        FMul => map2(a, c, f_mul),
        FDiv => map2(a, c, f_div),
        FMin => map2(a, c, f_min),
        FMax => map2(a, c, f_max),
        FPow => map2(a, c, f_pow),
        FAtan2 => map2(a, c, f_atan2),
        FLt => map2(a, c, f_lt),
        FLe => map2(a, c, f_le),
        FEq => map2(a, c, f_eq),
        FNe => map2(a, c, f_ne),
        FIsNan => map1(a, f_is_nan),
        FIsInf => map1(a, f_is_inf),
        INeg => map1(a, i_neg),
        INot => map1(a, i_not),
        IAbs => map1(a, i_abs),
        ISign => map1(a, i_sign),
        IAdd => map2(a, c, i_add),
        ISub => map2(a, c, i_sub),
        IMul => map2(a, c, i_mul),
        IAnd => map2(a, c, i_and),
        IOr => map2(a, c, i_or),
        IXor => map2(a, c, i_xor),
        IShl => map2(a, c, i_shl),
        IDiv => map2(a, c, i_div),
        UDiv => map2(a, c, u_div),
        IRem => map2(a, c, i_rem),
        URem => map2(a, c, u_rem),
        IShr => map2(a, c, i_shr),
        UShr => map2(a, c, u_shr),
        IMin => map2(a, c, i_min),
        UMin => map2(a, c, u_min),
        IMax => map2(a, c, i_max),
        UMax => map2(a, c, u_max),
        IEq => map2(a, c, i_eq),
        INe => map2(a, c, i_ne),
        ILt => map2(a, c, i_lt),
        ULt => map2(a, c, u_lt),
        ILe => map2(a, c, i_le),
        ULe => map2(a, c, u_le),
        BNot => map1(a, b_not),
        BAnd => map2(a, c, b_and),
        BOr => map2(a, c, b_or),
        BXor => map2(a, c, b_xor),
        BEq => map2(a, c, b_eq),
        FToI => map1(a, f_to_i_bits),
        FToU => map1(a, f_to_u_bits),
        IToF => map1(a, i_to_f),
        UToF => map1(a, u_to_f),
        BToF => map1(a, b_to_f),
        BToI => map1(a, b_to_i),
        FToB => map1(a, f_to_b),
        IToB => map1(a, i_to_b),
        Bitcast => map1(a, |x| bitcast(ty, x)),
        FToHalf => map1(a, f_to_half),
        HalfToF => map1(a, half_to_f),
    }
}
