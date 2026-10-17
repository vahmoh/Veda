// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The vertex stage: attribute fetch (OpenGL ES 3.0 section 2.9) and the
//! vertex shader, sixteen vertices at a time.
//!
//! Attribute reads are bounds checked: an element outside its buffer
//! reads as zero, as robust buffer access would have it.

use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use glsl::interp::{Env, Exec, LANES, Lanes};
use glsl::ops::f16_to_f32;

use super::program::SoftProgram;
use crate::backend::{Attrib, AttribType, BufferRange, DrawInfo, IndexType, Mode, ResourceId};
use crate::pixels::try_filled;

/// One attribute array as the vertex stage reads it.
#[derive(Clone, Copy)]
pub struct Fetch<'a> {
    /// The buffer's bytes (`None`: the array is disabled and the current
    /// value is used).
    pub data: Option<&'a [u8]>,
    pub offset: usize,
    pub stride: usize,
    pub size: u8,
    pub ty: AttribType,
    pub normalized: bool,
    pub integer: bool,
    pub divisor: u32,
    pub current: [u32; 4],
}

/// How the vertex stage reads each attribute of a draw: `bytes` gives a
/// buffer's contents.
pub fn fetches<'a>(attribs: &[Attrib], bytes: impl Fn(ResourceId) -> Option<&'a [u8]>) -> Vec<Fetch<'a>> {
    attribs
        .iter()
        .map(|a| Fetch {
            data: a.buffer.and_then(&bytes),
            offset: a.offset,
            stride: a.stride as usize,
            size: a.size,
            ty: a.ty,
            normalized: a.normalized,
            integer: a.integer,
            divisor: a.divisor,
            current: a.current,
        })
        .collect()
}

/// Each uniform block of a program as a draw sees it: its buffer range's
/// bytes (`bytes` gives a buffer's contents), zero beyond the range and
/// the buffer, as long as the block is.
pub fn block_snapshots<'a>(
    p: &glsl::program::Program,
    ranges: &[Option<BufferRange>],
    bytes: impl Fn(ResourceId) -> Option<&'a [u8]>,
) -> Vec<Vec<u8>> {
    let blocks = p.linked.blocks.iter().enumerate();
    blocks
        .map(|(i, info)| {
            let mut v = vec![0u8; info.size as usize];
            if let Some(Some(r)) = ranges.get(i)
                && let Some(data) = bytes(r.buffer)
            {
                let start = r.offset.min(data.len());
                let end = start.saturating_add(r.size.min(info.size as usize)).min(data.len());
                v[..end - start].copy_from_slice(&data[start..end]);
            }
            v
        })
        .collect()
}

/// The vertex stream of a draw: its indices (`indices`: the element
/// buffer's bytes and their type), `None` at restarts if `restart`, or
/// consecutive vertices; `None` if there is no memory for it. Indices are
/// read as far as the element buffer goes.
pub fn stream(indices: Option<(&[u8], IndexType)>, info: &DrawInfo, restart: bool) -> Option<Vec<Option<u32>>> {
    let Some((data, ty)) = indices else {
        let mut s = Vec::new();
        s.try_reserve_exact(info.count as usize).ok()?;
        s.extend((0..info.count).map(|k| Some((info.first as u32).wrapping_add(k))));
        return Some(s);
    };
    let r = ty.restart();
    let list = read_indices(data, info.first, info.count, ty)?;
    let mut s = Vec::new();
    s.try_reserve_exact(list.len()).ok()?;
    s.extend(list.into_iter().map(|i| if restart && i == r { None } else { Some(i) }));
    Some(s)
}

/// Indices of a draw, read from its element buffer from `offset` (as far
/// as the buffer goes); `None` if there is no memory for them.
pub fn read_indices(data: &[u8], offset: usize, count: u32, ty: IndexType) -> Option<Vec<u32>> {
    let size = ty.bytes();
    let data = data.get(offset..).unwrap_or(&[]);
    let b = &data[..(count as usize).min(data.len() / size) * size];
    let mut out = Vec::new();
    out.try_reserve_exact(b.len() / size).ok()?;
    match ty {
        IndexType::U8 => out.extend(b.iter().map(|&x| u32::from(x))),
        IndexType::U16 => out.extend(b.as_chunks::<2>().0.iter().map(|c| u32::from(u16::from_le_bytes(*c)))),
        IndexType::U32 => out.extend(b.as_chunks::<4>().0.iter().map(|c| u32::from_le_bytes(*c))),
    }
    Some(out)
}

/// Vertices per primitive of the modes transform feedback draws
/// (`POINTS`, `LINES`, `TRIANGLES`).
pub fn primitive_vertices(mode: Mode) -> usize {
    match mode {
        Mode::Points => 1,
        Mode::Lines => 2,
        _ => 3,
    }
}

/// What transform feedback captures from a draw's vertices, shaded on the
/// processor: the captured components of the vertices of every whole
/// primitive of each instance, in order. `None` if there is nothing to
/// record: no memory for it, a draw of indices (a draw that captures
/// draws arrays), or a shader that ran away.
pub fn capture(p: &SoftProgram, fetch: &[Fetch<'_>], env: &Env<'_>, info: &DrawInfo) -> Option<Vec<u32>> {
    if info.indexed {
        return None;
    }
    let ncap = p.feedback.len();
    let whole = info.count as usize / primitive_vertices(info.mode) * primitive_vertices(info.mode);
    let mut ids = Vec::new();
    ids.try_reserve_exact(whole).ok()?;
    ids.extend((0..whole as u32).map(|k| (info.first as u32).wrapping_add(k)));
    let mut exec = Exec::new(&p.vs);
    if exec.regs.len() < p.registers {
        exec.regs.resize(p.registers, Lanes::ZERO);
    }
    exec.prologue(&p.vs, env);
    let prologue = exec.regs[..p.vs.uniform_registers].to_vec();
    let mut out = try_filled(whole.checked_mul(vertex_words(p))?, 0)?;
    let mut captured = try_filled(whole.checked_mul(ncap)?, 0)?;
    let mut recorded = Vec::new();
    recorded.try_reserve_exact(captured.len().checked_mul(info.instances as usize)?).ok()?;
    let abandoned = AtomicBool::new(false);
    for instance in 0..info.instances {
        exec.regs[..prologue.len()].copy_from_slice(&prologue);
        shade(p, fetch, env, &mut exec, &ids, instance, &mut out, Some(&mut captured), &abandoned);
        if abandoned.load(Ordering::Relaxed) {
            return None;
        }
        recorded.extend_from_slice(&captured);
    }
    Some(recorded)
}

/// The bytes transform feedback writes to a program's buffer `buffer`
/// (whose range holds `size` bytes): the components of `recorded` (each
/// vertex's captured components, in the program's order) that go there,
/// for as many whole vertices as fit. `None` if there is nothing to write,
/// or no memory for it.
pub fn feedback_bytes(p: &SoftProgram, buffer: usize, size: usize, recorded: &[u32]) -> Option<Vec<u8>> {
    let ncap = p.feedback.len();
    let stride = p.feedback_strides.get(buffer).copied().filter(|&s| s > 0 && ncap > 0)?;
    let comps: Vec<usize> =
        p.feedback.iter().enumerate().filter(|(_, c)| c.buffer as usize == buffer).map(|(k, _)| k).collect();
    let n = (recorded.len() / ncap).min(size / stride);
    let mut data = crate::pixels::try_zeroed(n * stride)?;
    for (v, out) in data.chunks_exact_mut(stride).enumerate() {
        for (j, &k) in comps.iter().enumerate() {
            out[j * 4..j * 4 + 4].copy_from_slice(&recorded[v * ncap + k].to_le_bytes());
        }
    }
    Some(data)
}

impl Fetch<'_> {
    /// Element `element`'s four components (as the shader's 32 bits).
    #[inline]
    pub fn element(&self, element: u32) -> [u32; 4] {
        let Some(data) = self.data else { return self.current };
        let bytes = if self.ty.packed() { 4 } else { self.ty.bytes() as usize * self.size as usize };
        let at = (element as usize).checked_mul(self.stride).and_then(|o| o.checked_add(self.offset));
        let Some(b) = at.and_then(|o| data.get(o..o.checked_add(bytes)?)) else {
            // Outside the buffer: zero.
            return [0; 4];
        };
        decode(b, self.ty, self.size, self.normalized, self.integer)
    }
}

/// Converts one attribute element (section 2.9.1): to floats (normalized
/// or not) for `VertexAttribPointer` arrays, to integers for
/// `VertexAttribIPointer` ones. Missing components are 0, 0, 1.
pub fn decode(b: &[u8], ty: AttribType, size: u8, normalized: bool, integer: bool) -> [u32; 4] {
    let n = size as usize;
    if integer {
        let mut v = [0u32, 0, 0, 1];
        for (i, c) in v.iter_mut().enumerate().take(n) {
            *c = match ty {
                AttribType::Byte => i32::from(b[i] as i8) as u32,
                AttribType::UnsignedByte => u32::from(b[i]),
                AttribType::Short => i32::from(i16::from_le_bytes([b[2 * i], b[2 * i + 1]])) as u32,
                AttribType::UnsignedShort => u32::from(u16::from_le_bytes([b[2 * i], b[2 * i + 1]])),
                _ => u32::from_le_bytes([b[4 * i], b[4 * i + 1], b[4 * i + 2], b[4 * i + 3]]),
            };
        }
        return v;
    }
    let mut v = [0.0f32, 0.0, 0.0, 1.0];
    let word = |i: usize| u32::from_le_bytes([b[4 * i], b[4 * i + 1], b[4 * i + 2], b[4 * i + 3]]);
    match ty {
        AttribType::Int2101010Rev | AttribType::UnsignedInt2101010Rev => {
            let w = word(0);
            let fields = [w & 0x3FF, (w >> 10) & 0x3FF, (w >> 20) & 0x3FF, w >> 30];
            for (i, c) in v.iter_mut().enumerate() {
                let bits = if i == 3 { 2 } else { 10 };
                *c = if ty == AttribType::Int2101010Rev {
                    let s = ((fields[i] << (32 - bits)) as i32) >> (32 - bits);
                    if normalized { (s as f32 / ((1 << (bits - 1)) - 1) as f32).max(-1.0) } else { s as f32 }
                } else if normalized {
                    fields[i] as f32 / ((1u32 << bits) - 1) as f32
                } else {
                    fields[i] as f32
                };
            }
        }
        _ => {
            for (i, c) in v.iter_mut().enumerate().take(n) {
                *c = match ty {
                    AttribType::Byte => {
                        let x = f32::from(b[i] as i8);
                        if normalized { (x / 127.0).max(-1.0) } else { x }
                    }
                    AttribType::UnsignedByte => {
                        let x = f32::from(b[i]);
                        if normalized { x / 255.0 } else { x }
                    }
                    AttribType::Short => {
                        let x = f32::from(i16::from_le_bytes([b[2 * i], b[2 * i + 1]]));
                        if normalized { (x / 32767.0).max(-1.0) } else { x }
                    }
                    AttribType::UnsignedShort => {
                        let x = f32::from(u16::from_le_bytes([b[2 * i], b[2 * i + 1]]));
                        if normalized { x / 65535.0 } else { x }
                    }
                    AttribType::Int => {
                        let x = word(i) as i32;
                        if normalized { (f64::from(x) / 2_147_483_647.0).max(-1.0) as f32 } else { x as f32 }
                    }
                    AttribType::UnsignedInt => {
                        let x = word(i);
                        if normalized { (f64::from(x) / 4_294_967_295.0) as f32 } else { x as f32 }
                    }
                    AttribType::Fixed => word(i) as i32 as f32 / 65536.0,
                    AttribType::HalfFloat => f16_to_f32(u16::from_le_bytes([b[2 * i], b[2 * i + 1]])),
                    AttribType::Float => f32::from_bits(word(i)),
                    _ => 0.0,
                };
            }
        }
    }
    v.map(f32::to_bits)
}

/// Words per post-transform vertex: position, point size, varyings.
pub fn vertex_words(p: &SoftProgram) -> usize {
    5 + p.varyings.len()
}

/// Runs the vertex shader on `ids` (vertex indices, `gl_VertexID`) of
/// instance `instance`: writes each vertex's position, point size and
/// varyings to `out` (`vertex_words` words each), and its captured
/// transform feedback components to `captured` if given. A shader that
/// runs away (`glsl`'s `Exec::runaway`) sets `abandoned`, and no batch
/// is shaded once it is set: the draw is to be left out.
#[allow(clippy::too_many_arguments)]
pub fn shade(
    p: &SoftProgram,
    fetch: &[Fetch<'_>],
    env: &Env<'_>,
    exec: &mut Exec,
    ids: &[u32],
    instance: u32,
    out: &mut [u32],
    mut captured: Option<&mut [u32]>,
    abandoned: &AtomicBool,
) {
    let words = vertex_words(p);
    let ncap = p.feedback.len();
    let vs = &p.vs;
    for (batch, chunk) in ids.chunks(LANES).enumerate() {
        if abandoned.load(Ordering::Relaxed) {
            return;
        }
        let n = chunk.len();
        let mask = if n == LANES { glsl::interp::ALL } else { (1u32 << n) - 1 };
        // Inputs.
        for &(location, regs) in &p.attribs {
            let f = fetch.get(location as usize).copied();
            let mut values = [[0u32; 4]; LANES];
            if let Some(f) = f {
                for (lane, &id) in chunk.iter().enumerate() {
                    // Per vertex, or per `divisor` instances.
                    let element = instance.checked_div(f.divisor).unwrap_or(id);
                    values[lane] = f.element(element);
                }
            }
            for (c, r) in regs.iter().enumerate() {
                if let Some(r) = r {
                    let reg = &mut exec.regs[*r as usize].0;
                    for lane in 0..n {
                        reg[lane] = values[lane][c];
                    }
                }
            }
        }
        if let Some(r) = p.vertex_id {
            let reg = &mut exec.regs[r as usize].0;
            for (lane, &id) in chunk.iter().enumerate() {
                reg[lane] = id;
            }
        }
        if let Some(r) = p.instance_id {
            exec.regs[r as usize] = Lanes::splat(instance);
        }
        exec.run(vs, mask, env);
        if core::mem::take(&mut exec.runaway) {
            abandoned.store(true, Ordering::Relaxed);
            return;
        }
        // Outputs.
        let base = batch * LANES;
        let reg = |r: Option<u16>| r.map(|r| exec.regs[r as usize].0);
        let pos = p.position.map(reg);
        let psize = reg(p.point_size);
        for lane in 0..n {
            let o = (base + lane) * words;
            let v = &mut out[o..o + words];
            for c in 0..4 {
                v[c] = pos[c].map_or(if c == 3 { 1.0f32.to_bits() } else { 0 }, |r| r[lane]);
            }
            v[4] = psize.map_or(1.0f32.to_bits(), |r| r[lane]);
        }
        for (k, var) in p.varyings.iter().enumerate() {
            match var.vs {
                Some(r) => {
                    let regv = exec.regs[r as usize].0;
                    for lane in 0..n {
                        out[(base + lane) * words + 5 + k] = regv[lane];
                    }
                }
                None => {
                    for lane in 0..n {
                        out[(base + lane) * words + 5 + k] = 0;
                    }
                }
            }
        }
        if let Some(cap) = captured.as_deref_mut() {
            for (k, c) in p.feedback.iter().enumerate() {
                let values = c.reg.map(|r| exec.regs[r as usize].0);
                for lane in 0..n {
                    cap[(base + lane) * ncap + k] = values.map_or(0, |v| v[lane]);
                }
            }
        }
    }
}
