// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! A host for the virgl renderer's tests, which runs anywhere: it keeps
//! resources in memory, carries out the commands that move data
//! (transfers, copies, clears), counts the rest, and keeps track of the
//! objects and calls it was asked for. New resources hold what freed
//! memory would (`STALE`), as a GPU driver's recycled buffers do, unless a
//! command writes them.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::ops::Range;

use super::protocol::*;
use super::transport::{HostMemory, Lost, ResourceArgs, Transport};
use crate::backend::OutOfMemory;

/// What a new resource's memory holds.
pub const STALE: u8 = 0xA5;

/// One level of a fake resource: `layers` images of `height` rows.
#[derive(Clone, Copy)]
struct Level {
    offset: usize,
    width: u32,
    height: u32,
    layers: u32,
    texel: usize,
}

/// A resource the fake host keeps.
pub struct Res {
    pub args: ResourceArgs,
    levels: Vec<Level>,
    pub data: Vec<u8>,
    /// The range of the shared memory backing it.
    backing: Option<Range<usize>>,
}

/// Bytes per texel of the virgl formats the tests use (4 for the rest).
fn texel_bytes(format: u32) -> usize {
    match format {
        format::R8_UNORM | format::R8_UINT | format::R8_SINT | format::R8_SNORM => 1,
        format::R8G8_UNORM | format::Z16_UNORM | format::R16_FLOAT => 2,
        format::R16G16B16A16_FLOAT | format::R32G32_FLOAT | format::Z32_FLOAT_S8X24_UINT => 8,
        format::R32G32B32A32_FLOAT | format::R32G32B32A32_UINT | format::R32G32B32A32_SINT => 16,
        _ => 4,
    }
}

impl Res {
    fn new(args: ResourceArgs, backing: Option<Range<usize>>) -> Res {
        let mut levels = Vec::new();
        let mut total = 0;
        if args.target == TARGET_BUFFER {
            levels.push(Level { offset: 0, width: args.width, height: 1, layers: 1, texel: 1 });
            total = args.width as usize;
        } else {
            let texel = texel_bytes(args.format) * args.nr_samples.max(1) as usize;
            for l in 0..=args.last_level {
                let m = |n: u32| (n >> l).max(1);
                let layers = if args.target == TARGET_3D { m(args.depth) } else { args.array_size.max(1) };
                levels.push(Level { offset: total, width: m(args.width), height: m(args.height), layers, texel });
                total += m(args.width) as usize * m(args.height) as usize * layers as usize * texel;
            }
        }
        Res { args, levels, data: vec![STALE; total], backing }
    }

    /// The byte offset of texel `(x, y)` of image `z` of a level.
    fn at(&self, level: u32, x: u32, y: u32, z: u32) -> Option<(usize, usize)> {
        let lv = self.levels.get(level as usize)?;
        if x >= lv.width || y >= lv.height || z >= lv.layers {
            return None;
        }
        let row = lv.width as usize * lv.texel;
        let o = lv.offset + (z as usize * lv.height as usize + y as usize) * row + x as usize * lv.texel;
        Some((o, lv.texel))
    }
}

/// The fake host's state, which tests look at.
pub struct Host {
    shared: HostMemory,
    pub resources: BTreeMap<u32, Res>,
    next: u32,
    /// Calls: resources created and destroyed, submissions.
    pub creates: usize,
    pub destroys: usize,
    pub submits: usize,
    /// Draws asked for (not carried out).
    pub draws: usize,
    /// Commands carried out or counted, by command (`CMD_*`).
    pub commands: BTreeMap<u32, usize>,
    /// Live objects by handle: their kind.
    pub objects: BTreeMap<u32, u32>,
    surfaces: BTreeMap<u32, (u32, u32, u32)>,
    framebuffer: Vec<u32>,
    /// Fences queued, and how far they have signaled.
    pub fences: u64,
    pub signaled: u64,
    /// Whether fences signal as soon as they are queued.
    pub auto_signal: bool,
}

impl Host {
    fn shared(&mut self) -> &mut [u8] {
        // SAFETY: the host's memory, which only the thread running the test
        // touches, and never while the renderer holds a slice of it.
        unsafe { core::slice::from_raw_parts_mut(self.shared.ptr(), self.shared.len()) }
    }

    /// How many commands `cmd` (`CMD_*`) the host was sent.
    pub fn sent(&self, cmd: u32) -> usize {
        self.commands.get(&cmd).copied().unwrap_or(0)
    }

    /// Live objects of a kind (`OBJ_*`).
    pub fn count(&self, kind: u32) -> usize {
        self.objects.values().filter(|&&k| k == kind).count()
    }

    fn run(&mut self, cmds: &[u32]) {
        let mut i = 0;
        while i < cmds.len() {
            let head = cmds[i];
            let len = (head >> 16) as usize;
            let (cmd, obj) = (head & 0xFF, (head >> 8) & 0xFF);
            let w = &cmds[i + 1..i + 1 + len];
            *self.commands.entry(cmd).or_default() += 1;
            match cmd {
                CMD_CREATE_OBJECT => {
                    self.objects.insert(w[0], obj);
                    if obj == OBJ_SURFACE {
                        self.surfaces.insert(w[0], (w[1], w[3], w[4] & 0xFFFF));
                    }
                }
                CMD_DESTROY_OBJECT => {
                    self.objects.remove(&w[0]);
                    self.surfaces.remove(&w[0]);
                }
                CMD_SET_FRAMEBUFFER_STATE => self.framebuffer = w.to_vec(),
                CMD_CLEAR => self.clear(w),
                CMD_DRAW_VBO => self.draws += 1,
                CMD_COPY_TRANSFER3D => self.transfer(w),
                CMD_RESOURCE_COPY_REGION => self.copy(w),
                _ => {}
            }
            i += 1 + len;
        }
    }

    fn transfer(&mut self, w: &[u32]) {
        let [res, level, _, stride, layer_stride, x, y, z, bw, bh, bd, staging, at, flags] = w[..14] else { return };
        let from_host = flags & COPY_TRANSFER_FROM_HOST != 0;
        let base = self.resources[&staging].backing.clone().expect("staging is backed").start + at as usize;
        let buffer = self.resources[&res].args.target == TARGET_BUFFER;
        for k in 0..bd {
            for j in 0..bh {
                let (o, texel) =
                    if buffer { (x as usize, 1) } else { self.resources[&res].at(level, x, y + j, z + k).unwrap() };
                let n = bw as usize * texel;
                let s = base + k as usize * layer_stride as usize + j as usize * stride as usize;
                let shared = self.shared()[s..s + n].as_ptr();
                let data = &mut self.resources.get_mut(&res).unwrap().data[o..o + n];
                // SAFETY: `shared` points at `n` bytes of the shared memory,
                // which no resource's data overlaps.
                let shared = unsafe { core::slice::from_raw_parts_mut(shared.cast_mut(), n) };
                if from_host {
                    shared.copy_from_slice(data);
                } else {
                    data.copy_from_slice(shared);
                }
            }
        }
    }

    fn copy(&mut self, w: &[u32]) {
        let [dst, dst_level, dx, dy, dz, src, src_level, sx, sy, sz, bw, bh, bd] = w[..13] else { return };
        if self.resources[&src].args.target == TARGET_BUFFER {
            let bytes = self.resources[&src].data[sx as usize..][..bw as usize].to_vec();
            self.resources.get_mut(&dst).unwrap().data[dx as usize..][..bw as usize].copy_from_slice(&bytes);
            return;
        }
        for k in 0..bd {
            for j in 0..bh {
                for i in 0..bw {
                    let s = &self.resources[&src];
                    let (o, n) = s.at(src_level, sx + i, sy + j, sz + k).unwrap();
                    let texel = s.data[o..o + n].to_vec();
                    let d = self.resources.get_mut(&dst).unwrap();
                    let (o, n) = d.at(dst_level, dx + i, dy + j, dz + k).unwrap();
                    d.data[o..o + n].copy_from_slice(&texel);
                }
            }
        }
    }

    /// Clears the bound surfaces: to zero, or (8-bit RGBA) to a color.
    fn clear(&mut self, w: &[u32]) {
        let bits = w[0];
        let color: [u32; 4] = [w[1], w[2], w[3], w[4]];
        let mut targets = Vec::new();
        if bits & (CLEAR_DEPTH | CLEAR_STENCIL) != 0 && self.framebuffer.get(1).is_some_and(|&z| z != 0) {
            targets.push((self.framebuffer[1], [0u8; 4]));
        }
        for (i, &s) in self.framebuffer.iter().skip(2).enumerate() {
            if bits & (CLEAR_COLOR0 << i) != 0 && s != 0 {
                let b = color.map(|c| (f32::from_bits(c).clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
                targets.push((s, b));
            }
        }
        for (s, value) in targets {
            let (res, level, layer) = self.surfaces[&s];
            let r = self.resources.get_mut(&res).unwrap();
            let lv = r.levels[level as usize];
            let size = lv.width as usize * lv.height as usize * lv.texel;
            let image = &mut r.data[lv.offset + layer as usize * size..][..size];
            if value == [0; 4] || lv.texel != 4 {
                image.fill(0);
            } else {
                for t in image.as_chunks_mut::<4>().0 {
                    *t = value;
                }
            }
        }
    }
}

/// The fake host, as its transport and the tests share it (the
/// transport, as every one, may move to another thread).
#[derive(Clone)]
pub struct Shared(Arc<std::sync::RwLock<Host>>);

impl Shared {
    pub fn borrow(&self) -> std::sync::RwLockReadGuard<'_, Host> {
        self.0.read().expect("the fake host")
    }

    pub fn borrow_mut(&self) -> std::sync::RwLockWriteGuard<'_, Host> {
        self.0.write().expect("the fake host")
    }
}

/// A transport to the fake host.
pub struct Fake {
    pub host: Shared,
    /// The capability set it reports (which tests may change first).
    pub caps: Vec<u8>,
}

/// The shared memory's size: a MiB of staging and the query area.
const SHARED: usize = 1024 * 1024 + 16 * 1024;

impl Fake {
    /// A fake host that can render to and read back every format, and the
    /// state the tests look at.
    pub fn new() -> (Fake, Shared) {
        let host = Shared(Arc::new(std::sync::RwLock::new(Host {
            shared: HostMemory::new(SHARED).expect("memory for the fake host"),
            resources: BTreeMap::new(),
            next: 1,
            creates: 0,
            destroys: 0,
            submits: 0,
            draws: 0,
            commands: BTreeMap::new(),
            objects: BTreeMap::new(),
            surfaces: BTreeMap::new(),
            framebuffer: Vec::new(),
            fences: 0,
            signaled: 0,
            auto_signal: true,
        })));
        (Fake { host: host.clone(), caps: caps() }, host)
    }
}

/// A capability set 2 of a host that has everything the renderer needs.
fn caps() -> Vec<u8> {
    let mut b = vec![0u8; 1024];
    let mut put = |o: usize, v: u32| b[o..o + 4].copy_from_slice(&v.to_le_bytes());
    put(0, 2);
    for i in 0..16 {
        // Sampling, rendering and reading back.
        put(4 + 4 * i, !0);
        put(68 + 4 * i, !0);
        put(560 + 4 * i, !0);
    }
    put(268, 2048);
    put(280, 8);
    put(284, 4);
    put(296, 14);
    put(356, 16);
    put(384, 64);
    put(312, 64f32.to_bits());
    put(328, 8f32.to_bits());
    put(392, CAP_COPY_TRANSFER | CAP_COPY_IMAGE);
    put(484, 8192);
    put(488, 2048);
    put(492, 8192);
    put(688, CAP2_COPY_TRANSFER_BOTH_DIRECTIONS);
    b[696..700].copy_from_slice(b"fake");
    b
}

impl Transport for Fake {
    fn caps(&self) -> &[u8] {
        &self.caps
    }

    fn shared(&self) -> (*mut u8, usize) {
        let h = self.host.borrow();
        (h.shared.ptr(), h.shared.len())
    }

    fn create_resource(&mut self, args: &ResourceArgs, backing: Option<Range<usize>>) -> Result<u32, OutOfMemory> {
        let mut h = self.host.borrow_mut();
        let id = h.next;
        h.next += 1;
        h.creates += 1;
        h.resources.insert(id, Res::new(*args, backing));
        Ok(id)
    }

    fn destroy_resource(&mut self, handle: u32) {
        let mut h = self.host.borrow_mut();
        h.destroys += 1;
        assert!(h.resources.remove(&handle).is_some(), "resource {handle} destroyed twice");
    }

    fn max_submit_words(&self) -> usize {
        256 * 1024
    }

    fn submit(&mut self, commands: &[u32]) -> Result<(), Lost> {
        let mut h = self.host.borrow_mut();
        h.submits += 1;
        h.run(commands);
        Ok(())
    }

    fn fence(&mut self) -> Result<u64, Lost> {
        let mut h = self.host.borrow_mut();
        h.fences += 1;
        if h.auto_signal {
            h.signaled = h.fences;
        }
        Ok(h.fences)
    }

    fn signaled(&mut self, fence: u64) -> bool {
        self.host.borrow().signaled >= fence
    }

    fn wait(&mut self, fence: u64, _timeout_ns: u64) -> Result<bool, Lost> {
        // The fake host's fences signal when a test says.
        Ok(self.host.borrow().signaled >= fence)
    }
}

/// A context of the virgl renderer on a fake host, and the host.
pub fn context(config: crate::Config) -> (crate::Context, Shared) {
    let (t, host) = Fake::new();
    let b = super::VirglBackend::new(Box::new(t)).expect("the fake host suits the renderer");
    (crate::Context::new(Box::new(b), config), host)
}
