// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The virgl renderer: OpenGL ES on the GPU.
//!
//! virgl's protocol gives its client a Gallium-shaped view of a GPU: the
//! client creates resources and state objects, sends TGSI shaders and
//! draws, and Veda's renderer carries them out on Mesa's Gallium driver of
//! the GPU (and virglrenderer, in host tests, replays them with OpenGL or
//! OpenGL ES). [`Backend`] already has Gallium's shape, so this is mostly
//! translation:
//!
//! * Resources are host resources, which read as zero when new (the host's
//!   memory may hold what another context left in it). Data moves through
//!   a staging buffer in memory both sides share (`COPY_TRANSFER3D`), in
//!   command order, so an update between two draws reaches only the
//!   second. Buffers the front end lets go of are kept for new ones once
//!   the GPU is done with them (`pool`), and resources are destroyed on
//!   the host only after the commands that may name them are submitted.
//! * State objects (blend, depth-stencil, rasterizer, samplers, vertex
//!   layouts, views and surfaces) are created once per distinct state and
//!   cached, a bounded number of each kind; a draw binds them and sets only
//!   what changed since the last.
//! * Programs become two TGSI shaders ([`glsl::tgsi`]).
//! * What the host's commands cannot do directly is drawn: scissored and
//!   masked clears (`ops`). Such draws of the renderer's own stay out of
//!   the application's occlusion queries.
//! * Presenting blits the color buffer, flipped and scaled, into a BGRX
//!   image the size of the window and reads that back: the window's pixels
//!   arrive ready to copy.
//! * Nothing that would end the host is sent: on OpenGL ES hosts no slice
//!   of a 3D texture is ever bound to a framebuffer (see `flat_3d`).
//!
//! The host renders exactly as OpenGL does (row 0 of an image is its
//! bottom row, as in the guest), so no coordinate ever needs flipping
//! except once, when presenting.

pub mod caps;
mod draw;
#[cfg(test)]
pub(crate) mod fake;
mod feedback;
pub mod formats;
#[cfg(all(target_os = "linux", any(test, feature = "host-virgl")))]
pub mod gallium;
#[cfg(all(target_os = "linux", any(test, feature = "host-virgl")))]
pub mod host;
mod ops;
mod pool;
pub(crate) mod protocol;
mod transport;

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::backend::*;
use crate::format::{Class, Format};
use caps::HostCaps;
use formats::HostFormat;
use protocol::*;
pub use transport::{IMPORT_DISPLAY, IMPORT_SHARED, Lost, ResourceArgs, Transport};

/// Bytes of shared memory for query results: one 16-byte slot each, but
/// the last, which every other resource gets as its (unused) storage:
/// virglrenderer copies from a resource to staging only if the resource
/// has guest storage of its own.
const QUERY_AREA: usize = 16 * 1024;
const QUERY_SLOT: usize = 16;
/// Bytes of the buffer of zeros new buffers are copied from.
const ZERO_BYTES: usize = 256 * 1024;
/// Words kept free in the command buffer for the command that consumes a
/// staged block.
const RESERVE_WORDS: usize = 64;
/// Resources waiting to be destroyed that make the commands naming them go
/// at once.
const DOOMED: usize = 64;

/// Why the host cannot be used.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unsupported {
    /// The capability set is missing or too old.
    Caps,
    /// The host cannot copy between resources and guest memory in the
    /// command stream (`VIRGL_CAP_COPY_TRANSFER` in both directions).
    Transfers,
    /// The host has fewer draw buffers, vertex attributes or uniform blocks
    /// than the front end offers applications, or needs uniform blocks
    /// further apart.
    Limits,
    /// Too little shared memory.
    Memory,
    /// The device failed.
    Lost,
}

impl core::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Unsupported::Caps => "its capabilities are missing or too old",
            Unsupported::Transfers => "it cannot copy between resources and memory in its commands",
            Unsupported::Limits => "it has fewer draw buffers, vertex attributes or uniform blocks than contexts offer",
            Unsupported::Memory => "too little memory is shared with it, or not where queries need it",
            Unsupported::Lost => "the device failed",
        })
    }
}

/// A resource, as this renderer keeps track of it.
struct Res {
    desc: ResourceDesc,
    /// A buffer's bytes on the host (at least `desc.width`: see `pool`).
    capacity: u32,
    /// The host's format (textures and renderbuffers).
    host: Option<HostFormat>,
    /// Views and surfaces made of it (destroyed with it).
    views: Vec<ViewKey>,
    surfaces: Vec<(u32, u32)>,
    /// Buffers: their contents.
    shadow: Option<Shadow>,
}

/// A buffer's contents as the guest knows them, to find the vertices draws
/// read: OpenGL ES under ANGLE refuses a draw reading past the end of a
/// buffer, and virglrenderer then gives up the whole context.
struct Shadow {
    data: Vec<u8>,
    /// Whether `data` is current (not if the GPU wrote the buffer since).
    valid: bool,
    /// The highest index of ranges looked at, by (offset, count, index
    /// bytes, restart).
    ranges: BTreeMap<(usize, u32, u8, bool), draw::Highest>,
}

/// A sampler view's description.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct ViewKey {
    resource: u32,
    base: u32,
    max: u32,
    swizzle: [u8; 4],
}

/// An occlusion query. Clears, copies and reads generate no fragments, but
/// this renderer draws to make some of them, and the host would count
/// those draws: its query ends before the renderer's own work, and another
/// begins at the application's next draw. Samples passed if they passed in
/// any part.
struct Query {
    /// `PIPE_QUERY_*`.
    kind: u32,
    parts: Vec<QueryPart>,
    /// The fence after its end: the host writes the results once it
    /// signals.
    fence: Option<u64>,
    /// A part could not begin (no slot was free): samples passed, as far
    /// as anyone can tell.
    overflow: bool,
}

/// A query object on the host.
struct QueryPart {
    object: u32,
    /// The resource its result is written to, and that resource's slot of
    /// the shared memory.
    resource: u32,
    slot: usize,
}

/// The renderer.
pub struct VirglBackend {
    t: Box<dyn Transport>,
    host: HostCaps,
    caps: Caps,
    tgsi: glsl::tgsi::Options,
    /// Commands not yet submitted.
    cmds: Vec<u32>,
    max_words: usize,
    /// The staging buffer: the start of the shared memory.
    staging: u32,
    staging_size: usize,
    /// Bytes of staging that queued commands still need.
    staging_used: usize,
    query_slots: Vec<usize>,
    resources: BTreeMap<u32, Res>,
    /// Buffers kept for new ones.
    pool: pool::Pool,
    /// Resources to destroy on the host once the commands that may name
    /// them have been submitted, with the query result slots to free then.
    doomed: Vec<(u32, Option<usize>)>,
    /// Object handles: the next new one, and destroyed ones to reuse.
    next_handle: u32,
    free_handles: Vec<u32>,
    views: BTreeMap<ViewKey, u32>,
    surfaces: BTreeMap<(u32, u32, u32), u32>,
    queries: BTreeMap<QueryId, Query>,
    next_query: QueryId,
    /// The application's active query, and whether a part of it is
    /// running on the host.
    active_query: Option<QueryId>,
    query_running: bool,
    /// How deep in its own work the renderer is (see `unqueried`).
    own_work: u32,
    programs: BTreeMap<ProgramId, draw::Program>,
    next_program: ProgramId,
    state: draw::States,
    lost: bool,
    internal: ops::Internal,
    /// The host cannot bind a slice of a 3D texture to a framebuffer: on
    /// OpenGL ES, virglrenderer binds one with `glFramebufferTexture3DOES`,
    /// which hosts without `OES_texture_3D` (ANGLE) lack, and libepoxy then
    /// aborts the whole host process. Such textures are only ever sampled
    /// and written here; reads, copies and draws into them go around
    /// ([`ops`]).
    flat_3d: bool,
    /// Whether the host records transform feedback (`None`: not found out
    /// yet; see `feedback`), and programs compiled to capture in guest
    /// memory where it does not.
    feedback_on_host: Option<bool>,
    guest_programs: BTreeMap<ProgramId, Arc<crate::soft::program::SoftProgram>>,
}

impl VirglBackend {
    /// A renderer on a virgl context.
    pub fn new(mut t: Box<dyn Transport>) -> Result<VirglBackend, Unsupported> {
        let host = HostCaps::parse(t.caps()).ok_or(Unsupported::Caps)?;
        if !host.has(CAP_COPY_TRANSFER) || !host.has2(CAP2_COPY_TRANSFER_BOTH_DIRECTIONS) {
            return Err(Unsupported::Transfers);
        }
        use crate::context::{DRAW_BUFFERS, UNIFORM_BUFFER_OFFSET_ALIGNMENT, VERTEX_ATTRIBS};
        let blocks = glsl::Options::default().limits.max_uniform_blocks;
        if !host.meets(DRAW_BUFFERS, VERTEX_ATTRIBS, blocks, UNIFORM_BUFFER_OFFSET_ALIGNMENT) {
            return Err(Unsupported::Limits);
        }
        // Query results are 16-byte records the host writes, which are read
        // a word at a time: the area they are in starts at a multiple of 16.
        let (base, shared) = t.shared();
        if shared < QUERY_AREA + 1024 * 1024 || !(base as usize).is_multiple_of(QUERY_SLOT) {
            return Err(Unsupported::Memory);
        }
        let staging_size = (shared - QUERY_AREA) / QUERY_SLOT * QUERY_SLOT;
        let args = ResourceArgs {
            target: TARGET_BUFFER,
            format: format::R8_UNORM,
            bind: BIND_STAGING,
            width: staging_size as u32,
            height: 1,
            depth: 1,
            array_size: 1,
            ..Default::default()
        };
        let staging = t.create_resource(&args, Some(0..staging_size)).map_err(|_| Unsupported::Memory)?;
        // A submission must hold the longest command (shaders are split to
        // fit one).
        let max_words = t.max_submit_words();
        if max_words <= MAX_COMMAND_WORDS {
            return Err(Unsupported::Memory);
        }
        let caps = caps_for(&host);
        let tgsi = glsl::tgsi::Options { shadow_lod: host.has2(CAP2_TEXTURE_SHADOW_LOD) };
        let flat_3d = host.has(CAP_HOST_IS_GLES);
        Ok(VirglBackend {
            t,
            host,
            caps,
            tgsi,
            cmds: Vec::with_capacity(4096),
            max_words,
            staging,
            staging_size,
            staging_used: 0,
            query_slots: (0..QUERY_AREA / QUERY_SLOT - 1).rev().collect(),
            resources: BTreeMap::new(),
            pool: pool::Pool::default(),
            doomed: Vec::new(),
            next_handle: 1,
            free_handles: Vec::new(),
            views: BTreeMap::new(),
            surfaces: BTreeMap::new(),
            queries: BTreeMap::new(),
            next_query: 1,
            active_query: None,
            query_running: false,
            own_work: 0,
            programs: BTreeMap::new(),
            next_program: 1,
            state: draw::States::default(),
            lost: false,
            internal: ops::Internal::default(),
            flat_3d,
            feedback_on_host: None,
            guest_programs: BTreeMap::new(),
        })
    }

    // ---- Commands ---------------------------------------------------------

    /// Queues a command.
    fn emit(&mut self, cmd: u32, obj: u32, words: &[u32]) {
        debug_assert!(words.len() <= MAX_COMMAND_WORDS);
        if self.cmds.len() + 1 + words.len() > self.max_words {
            self.submit();
        }
        self.cmds.push(cmd0(cmd, obj, words.len() as u32));
        self.cmds.extend_from_slice(words);
    }

    /// Sends the queued commands. Afterwards the staging buffer is free;
    /// resources they were the last to name are destroyed, and buffers
    /// kept for reuse get a fence to wait for.
    fn submit(&mut self) {
        self.send();
        if self.pool.unfenced() {
            self.fence_now();
        }
    }

    /// Sends the queued commands, and destroys what waited for them.
    fn send(&mut self) {
        if !self.cmds.is_empty() {
            if !self.lost && self.t.submit(&self.cmds).is_err() {
                self.lost = true;
            }
            self.cmds.clear();
            self.staging_used = 0;
        }
        for (id, slot) in core::mem::take(&mut self.doomed) {
            // A lost context's resources went with it.
            if !self.lost {
                self.t.destroy_resource(id);
            }
            self.query_slots.extend(slot);
        }
    }

    /// Sends the queued commands and returns a fence after them (0 if the
    /// context is lost).
    fn fence_now(&mut self) -> u64 {
        self.send();
        if self.lost {
            return 0;
        }
        match self.t.fence() {
            Ok(f) => {
                self.pool.fenced(f);
                f
            }
            Err(_) => {
                self.lost = true;
                0
            }
        }
    }

    /// Waits until the GPU has done all the work queued.
    fn sync(&mut self) {
        let f = self.fence_now();
        if !self.lost && self.t.wait(f, u64::MAX).is_err() {
            self.lost = true;
        }
    }

    fn new_handle(&mut self) -> u32 {
        if let Some(h) = self.free_handles.pop() {
            return h;
        }
        self.next_handle += 1;
        self.next_handle - 1
    }

    fn destroy_object(&mut self, handle: u32) {
        self.emit(CMD_DESTROY_OBJECT, 0, &[handle]);
        self.state.bound.forget_object(handle);
        self.free_handles.push(handle);
    }

    // ---- Shared memory ------------------------------------------------------

    /// Reserves `len` bytes of staging for a command about to be queued.
    fn stage(&mut self, len: usize) -> usize {
        let len = len.next_multiple_of(16);
        debug_assert!(len <= self.staging_size);
        if self.staging_used + len > self.staging_size || self.cmds.len() + RESERVE_WORDS > self.max_words {
            self.submit();
        }
        let at = self.staging_used;
        self.staging_used += len;
        at
    }

    /// The shared memory at `at`, `len` bytes (which callers keep inside
    /// it). The host does not write the staging area except while commands
    /// run, which they do not while the slice lives.
    fn shared_mut(&mut self, at: usize, len: usize) -> &mut [u8] {
        let (p, size) = self.t.shared();
        assert!(at.checked_add(len).is_some_and(|end| end <= size), "staging past the shared memory");
        // SAFETY: inside the shared memory, which the transport keeps
        // alive; `&mut self` keeps this the only slice of it.
        unsafe { core::slice::from_raw_parts_mut(p.add(at), len) }
    }

    /// A word of shared memory the host may write at any time: of a query
    /// result's record.
    fn shared_word(&self, at: usize) -> u32 {
        let (p, size) = self.t.shared();
        assert!(at + 4 <= size && at.is_multiple_of(4), "a word past the shared memory");
        // SAFETY: inside the shared memory, which starts at a multiple of
        // 16 (`new` checks), at a multiple of 4; read as the host may be
        // writing it.
        unsafe { core::ptr::read_volatile(p.add(at) as *const u32) }
    }

    /// Queues a copy between a resource and staging (`COPY_TRANSFER3D`).
    #[allow(clippy::too_many_arguments)]
    fn transfer(
        &mut self,
        res: u32,
        level: u32,
        b: [u32; 6],
        stride: u32,
        layer_stride: u32,
        at: usize,
        to_host: bool,
    ) {
        let flags = COPY_TRANSFER_SYNCHRONIZED | if to_host { 0 } else { COPY_TRANSFER_FROM_HOST };
        let w =
            [res, level, 0, stride, layer_stride, b[0], b[1], b[2], b[3], b[4], b[5], self.staging, at as u32, flags];
        self.emit(CMD_COPY_TRANSFER3D, 0, &w);
    }

    // ---- Resources ------------------------------------------------------------

    /// A resource as `desc` describes it: a buffer the front end let go
    /// of ([`VirglBackend::take_kept`]), or a new one.
    fn create(&mut self, desc: &ResourceDesc) -> Result<u32, OutOfMemory> {
        match self.take_kept(desc)? {
            Some(handle) => Ok(handle),
            None => self.create_new(desc),
        }
    }

    /// What the backend keeps of a resource: its description and, for a
    /// buffer, the guest's copy of its contents (zeros, as it is new).
    fn record(desc: &ResourceDesc, capacity: u32, host: Option<HostFormat>) -> Result<Res, OutOfMemory> {
        let shadow = if desc.target == Target::Buffer {
            let data = crate::pixels::try_zeroed(desc.width as usize).ok_or(OutOfMemory)?;
            Some(Shadow { data, valid: true, ranges: BTreeMap::new() })
        } else {
            None
        };
        Ok(Res { desc: *desc, capacity, host, views: Vec::new(), surfaces: Vec::new(), shadow })
    }

    /// A buffer of one the front end let go of, if `desc` is a buffer's and
    /// the GPU is done with one large enough. It holds what it held.
    fn take_kept(&mut self, desc: &ResourceDesc) -> Result<Option<u32>, OutOfMemory> {
        if desc.target != Target::Buffer {
            return Ok(None);
        }
        let t = &mut self.t;
        let Some((handle, capacity)) = self.pool.take(desc.width, |f| t.signaled(f)) else { return Ok(None) };
        match Self::record(desc, capacity, None) {
            Ok(record) => {
                self.resources.insert(handle, record);
                Ok(Some(handle))
            }
            Err(e) => {
                // Kept again, for another.
                for gone in self.pool.keep(handle, capacity) {
                    self.doom(gone, None);
                }
                Err(e)
            }
        }
    }

    /// A resource the host makes anew.
    fn create_new(&mut self, desc: &ResourceDesc) -> Result<u32, OutOfMemory> {
        let capacity = pool::Pool::capacity(desc.width);
        let (args, host) = match desc.target {
            Target::Buffer => (
                ResourceArgs {
                    target: TARGET_BUFFER,
                    format: format::R8_UNORM,
                    // virglrenderer takes exactly one buffer binding; a GL
                    // buffer may later be bound anywhere.
                    bind: BIND_VERTEX_BUFFER,
                    width: capacity,
                    height: 1,
                    depth: 1,
                    array_size: 1,
                    ..Default::default()
                },
                None,
            ),
            _ => {
                let hf = formats::host_format(&self.host, desc.format).ok_or(OutOfMemory)?;
                let depth_class = matches!(desc.format.class(), Class::Depth | Class::DepthStencil | Class::Stencil);
                let mut bind = if depth_class {
                    BIND_DEPTH_STENCIL
                } else if formats::gl_renderable(desc.format) && hf.renderable {
                    BIND_RENDER_TARGET
                } else {
                    0
                };
                if desc.target != Target::Renderbuffer || desc.samples == 0 {
                    bind |= BIND_SAMPLER_VIEW;
                }
                let (target, depth, array_size) = match desc.target {
                    Target::Texture3D => (TARGET_3D, desc.depth, 1),
                    Target::TextureCube => (TARGET_CUBE, 1, 6),
                    Target::Texture2DArray => (TARGET_2D_ARRAY, 1, desc.depth),
                    _ => (TARGET_2D, 1, 1),
                };
                let args = ResourceArgs {
                    target,
                    format: hf.virgl,
                    bind,
                    width: desc.width.max(1),
                    height: desc.height.max(1),
                    depth: depth.max(1),
                    array_size: array_size.max(1),
                    last_level: desc.levels.max(1) - 1,
                    nr_samples: desc.samples,
                    flags: 0,
                };
                (args, Some(hf))
            }
        };
        let record = Self::record(desc, args.width, host)?;
        let handle = self.t.create_resource(&args, Some(self.dummy_backing()))?;
        self.resources.insert(handle, record);
        Ok(handle)
    }

    /// The storage every resource but staging and query results gets.
    pub(super) fn dummy_backing(&self) -> core::ops::Range<usize> {
        let end = self.staging_size + QUERY_AREA;
        end - QUERY_SLOT..end
    }

    fn destroy(&mut self, id: u32) {
        let Some(r) = self.resources.remove(&id) else { return };
        if r.desc.target == Target::Buffer {
            // Kept for new buffers (still bound, it may stay so).
            for gone in self.pool.keep(id, r.capacity) {
                self.doom(gone, None);
            }
            return;
        }
        for v in r.views {
            if let Some(h) = self.views.remove(&v) {
                self.destroy_object(h);
            }
        }
        for (level, layer) in r.surfaces {
            if let Some(h) = self.surfaces.remove(&(id, level, layer)) {
                self.destroy_object(h);
            }
        }
        self.doom(id, None);
    }

    /// Destroys a resource on the host once the commands queued so far,
    /// which may name it, have been submitted (and frees a query result
    /// slot then). Bindings naming it are forgotten: the host may give its
    /// handle to another.
    fn doom(&mut self, id: u32, slot: Option<usize>) {
        self.state.bound.forget_resource(id);
        self.doomed.push((id, slot));
        if self.doomed.len() >= DOOMED {
            self.submit();
        }
    }

    /// A surface (a level and layer of a resource), made once. None (0)
    /// of a 3D slice the host cannot bind: binding one would end the host.
    fn surface(&mut self, s: Surface) -> u32 {
        if let Some(&h) = self.surfaces.get(&(s.resource, s.level, s.layer)) {
            return h;
        }
        if self.is_flat_3d(s.resource) {
            return 0;
        }
        let Some(r) = self.resources.get_mut(&s.resource) else { return 0 };
        let Some(hf) = r.host else { return 0 };
        r.surfaces.push((s.level, s.layer));
        let h = self.new_handle();
        self.emit(CMD_CREATE_OBJECT, OBJ_SURFACE, &[h, s.resource, hf.virgl, s.level, s.layer | (s.layer << 16)]);
        self.surfaces.insert((s.resource, s.level, s.layer), h);
        h
    }

    /// A sampler view of a texture, made once.
    fn view(&mut self, v: &View) -> u32 {
        let Some(r) = self.resources.get(&v.resource) else { return 0 };
        let Some(hf) = r.host else { return 0 };
        let key = ViewKey {
            resource: v.resource,
            base: v.base_level,
            max: v.max_level,
            swizzle: formats::compose(hf.swizzle, v.swizzle),
        };
        if let Some(&h) = self.views.get(&key) {
            return h;
        }
        let layers = match r.desc.target {
            Target::TextureCube => 6,
            Target::Texture2DArray => r.desc.depth,
            _ => 1,
        };
        let target = match r.desc.target {
            Target::Texture3D => TARGET_3D,
            Target::TextureCube => TARGET_CUBE,
            Target::Texture2DArray => TARGET_2D_ARRAY,
            _ => TARGET_2D,
        };
        let format = if self.host.has(CAP_TEXTURE_VIEW) { hf.virgl | (target << 24) } else { hf.virgl };
        let s = key.swizzle;
        let swizzle = u32::from(s[0]) | (u32::from(s[1]) << 3) | (u32::from(s[2]) << 6) | (u32::from(s[3]) << 9);
        let h = self.new_handle();
        let words = [h, v.resource, format, (layers - 1) << 16, key.base | (key.max << 8), swizzle];
        self.emit(CMD_CREATE_OBJECT, OBJ_SAMPLER_VIEW, &words);
        self.views.insert(key, h);
        if let Some(r) = self.resources.get_mut(&v.resource) {
            r.views.push(key);
        }
        h
    }

    // ---- Transfers ------------------------------------------------------------

    fn write_buffer(&mut self, id: u32, offset: usize, data: &[u8]) {
        if let Some(s) = self.resources.get_mut(&id).and_then(|r| r.shadow.as_mut()) {
            if let Some(d) = s.data.get_mut(offset..offset + data.len()) {
                d.copy_from_slice(data);
            }
            s.ranges.clear();
        }
        self.upload_bytes(id, offset, data.len(), |done, staged| staged.copy_from_slice(&data[done..][..staged.len()]));
    }

    /// Writes `len` bytes of a buffer from `offset` through staging:
    /// `fill(done, staged)` fills the staged bytes that go `done` bytes
    /// after `offset`.
    fn upload_bytes(&mut self, id: u32, offset: usize, len: usize, mut fill: impl FnMut(usize, &mut [u8])) {
        let mut done = 0;
        while done < len {
            let n = (len - done).min(self.staging_size);
            let at = self.stage(n);
            fill(done, self.shared_mut(at, n));
            self.transfer(id, 0, [(offset + done) as u32, 0, 0, n as u32, 1, 1], 0, 0, at, true);
            done += n;
        }
    }

    // ---- New resources read as zero -------------------------------------------

    /// Makes a new resource read as zero, as `Backend::create_resource`
    /// promises: the host's memory may hold what another context left in
    /// it, as GPU drivers recycle freed memory. Images the host can draw
    /// into are cleared (2D and cube textures: render targets, which have
    /// few images per level); the rest are written with zeros through
    /// staging; buffers are copied from a buffer of zeros.
    fn zero(&mut self, id: u32, d: &ResourceDesc) {
        match d.target {
            Target::Buffer => self.zero_buffer(id, d.width as usize),
            Target::Renderbuffer => self.zero_image(Surface { resource: id, level: 0, layer: 0 }, d),
            _ => {
                let Some(hf) = self.resources.get(&id).and_then(|r| r.host) else { return };
                let depth = matches!(d.format.class(), Class::Depth | Class::DepthStencil | Class::Stencil);
                let drawn = depth || (formats::gl_renderable(d.format) && hf.renderable);
                let clears = drawn && matches!(d.target, Target::Texture2D | Target::TextureCube);
                for level in 0..d.levels.max(1) {
                    let (w, h, layers) = d.level_size(level);
                    if clears {
                        for layer in 0..layers {
                            self.zero_image(Surface { resource: id, level, layer }, d);
                        }
                    } else {
                        self.upload(id, level, Region::new(0, 0, 0, w, h, layers), |_, _, _, row| row.fill(0));
                    }
                }
            }
        }
    }

    /// Zeroes the first `len` bytes of a buffer: copies of a buffer of
    /// zeros, made once (or, if there is no memory for it, zeros through
    /// staging).
    fn zero_buffer(&mut self, id: u32, len: usize) {
        let zero = match self.internal.zero {
            Some(z) => Some(z),
            None => {
                let desc = ResourceDesc {
                    target: Target::Buffer,
                    format: Format::R8Uint,
                    width: ZERO_BYTES as u32,
                    height: 1,
                    depth: 1,
                    levels: 1,
                    samples: 0,
                };
                let z = self.create(&desc).ok();
                if let Some(z) = z {
                    self.upload_bytes(z, 0, ZERO_BYTES, |_, staged| staged.fill(0));
                }
                self.internal.zero = z;
                z
            }
        };
        let Some(zero) = zero else {
            return self.upload_bytes(id, 0, len, |_, staged| staged.fill(0));
        };
        let mut at = 0;
        while at < len {
            let n = (len - at).min(ZERO_BYTES);
            self.emit(CMD_RESOURCE_COPY_REGION, 0, &[id, 0, at as u32, 0, 0, zero, 0, 0, 0, 0, n as u32, 1, 1]);
            at += n;
        }
    }

    fn read_buffer(&mut self, id: u32, offset: usize, out: &mut [u8]) {
        let mut done = 0;
        while done < out.len() {
            let n = (out.len() - done).min(self.staging_size);
            self.submit();
            let at = self.stage(n);
            self.transfer(id, 0, [(offset + done) as u32, 0, 0, n as u32, 1, 1], 0, 0, at, false);
            self.submit();
            out[done..done + n].copy_from_slice(self.shared_mut(at, n));
            done += n;
        }
    }

    /// Copies bytes between buffers (on the host), keeping the contents
    /// known.
    fn copy_buffer_region(&mut self, src: u32, src_offset: usize, dst: u32, dst_offset: usize, size: usize) {
        if size == 0 {
            return;
        }
        let w = [dst, 0, dst_offset as u32, 0, 0, src, 0, src_offset as u32, 0, 0, size as u32, 1, 1];
        self.emit(CMD_RESOURCE_COPY_REGION, 0, &w);
        let bytes = self
            .resources
            .get(&src)
            .and_then(|r| r.shadow.as_ref())
            .filter(|s| s.valid)
            .and_then(|s| s.data.get(src_offset..src_offset + size))
            .map(<[u8]>::to_vec);
        if let Some(s) = self.resources.get_mut(&dst).and_then(|r| r.shadow.as_mut()) {
            match (bytes, s.data.get_mut(dst_offset..dst_offset + size)) {
                (Some(b), Some(d)) => d.copy_from_slice(&b),
                _ => s.valid = false,
            }
            s.ranges.clear();
        }
    }

    /// Splits a region into pieces whose texels fit in staging: whole
    /// slices when they fit, otherwise bands of rows.
    fn pieces(&self, region: Region, row_bytes: usize) -> Vec<Region> {
        let stride = row_bytes.next_multiple_of(4).max(4);
        let slice = stride * region.h.max(1) as usize;
        let mut out = Vec::new();
        if slice <= self.staging_size {
            let per = (self.staging_size / slice).max(1) as u32;
            let mut z = 0;
            while z < region.d {
                let d = per.min(region.d - z);
                out.push(Region { z: region.z + z, d, ..region });
                z += d;
            }
        } else {
            let rows = (self.staging_size / stride).max(1) as u32;
            for z in 0..region.d {
                let mut y = 0;
                while y < region.h {
                    let h = rows.min(region.h - y);
                    out.push(Region { y: region.y + y, z: region.z + z, h, d: 1, ..region });
                    y += h;
                }
            }
        }
        out
    }

    fn write_texture(
        &mut self,
        id: u32,
        level: u32,
        region: Region,
        data: &[u8],
        row_pitch: usize,
        image_pitch: usize,
    ) {
        let Some(fmt) = self.resources.get(&id).map(|r| r.desc.format) else { return };
        let sb = fmt.bytes();
        self.upload(id, level, region, |tf, z, y, row| {
            let src = &data[z * image_pitch + y * row_pitch..][..region.w as usize * sb];
            if tf == fmt {
                row.copy_from_slice(src);
            } else {
                for (s, t) in src.chunks_exact(sb).zip(row.chunks_exact_mut(tf.bytes())) {
                    tf.encode(&fmt.decode(s), t);
                }
            }
        });
    }

    /// Writes a region of a level as the host keeps it, through staging:
    /// `fill(format, z, y, row)` fills the staged texels of row `y` of
    /// image `z` of the region (both counted from its corner), in the
    /// format transfers use.
    fn upload(&mut self, id: u32, level: u32, region: Region, mut fill: impl FnMut(Format, usize, usize, &mut [u8])) {
        let Some(r) = self.resources.get(&id) else { return };
        let (Some(hf), fmt) = (r.host, r.desc.format) else { return };
        let tf = hf.transfer_format(fmt);
        let row_bytes = region.w as usize * tf.bytes();
        let stride = row_bytes.next_multiple_of(4).max(4);
        for p in self.pieces(region, row_bytes) {
            let image = stride * p.h as usize;
            let at = self.stage(image * p.d as usize);
            let dst = self.shared_mut(at, image * p.d as usize);
            for z in 0..p.d as usize {
                for y in 0..p.h as usize {
                    let (sz, sy) = ((p.z - region.z) as usize + z, (p.y - region.y) as usize + y);
                    fill(tf, sz, sy, &mut dst[z * image + y * stride..][..row_bytes]);
                }
            }
            let b = [p.x, p.y, p.z, p.w, p.h, p.d];
            self.transfer(id, level, b, stride as u32, image as u32, at, true);
        }
    }

    fn read_texture(
        &mut self,
        id: u32,
        level: u32,
        region: Region,
        out: &mut [u8],
        row_pitch: usize,
        image_pitch: usize,
    ) {
        let Some(r) = self.resources.get(&id) else { return };
        let (Some(hf), fmt) = (r.host, r.desc.format) else { return };
        if self.is_flat_3d(id) {
            return self.read_3d(id, level, region, out, row_pitch, image_pitch);
        }
        if !self.host.can_read_back(hf.virgl) || r.desc.samples > 0 {
            return self.read_through_copy(id, level, region, out, row_pitch, image_pitch);
        }
        let tf = hf.transfer_format(fmt);
        let (sb, tb) = (fmt.bytes(), tf.bytes());
        let row_bytes = region.w as usize * tb;
        let stride = row_bytes.next_multiple_of(4).max(4);
        // The GPU finishes drawing first, and this context waits for it on
        // its fence: a read would have the renderer wait instead, keeping
        // its other clients waiting too (the compositor among them, where
        // the GPU composes the screen), as present_image says.
        self.sync();
        for p in self.pieces(region, row_bytes) {
            let image = stride * p.h as usize;
            let len = image * p.d as usize;
            self.submit();
            let at = self.stage(len);
            // An image per transfer: virglrenderer on desktop OpenGL reads
            // the slices after the first of a 3D texture as they were
            // before writes earlier in the same submission.
            for z in 0..p.d {
                let b = [p.x, p.y, p.z + z, p.w, p.h, 1];
                self.transfer(id, level, b, stride as u32, image as u32, at + z as usize * image, false);
            }
            self.submit();
            let src = self.shared_mut(at, len);
            for z in 0..p.d as usize {
                for y in 0..p.h as usize {
                    let dz = (p.z - region.z) as usize + z;
                    let dy = (p.y - region.y) as usize + y;
                    let s = &src[(z * p.h as usize + y) * stride..][..row_bytes];
                    let d = &mut out[dz * image_pitch + dy * row_pitch..][..region.w as usize * sb];
                    if tf == fmt {
                        d.copy_from_slice(s);
                    } else {
                        for (s, t) in s.chunks_exact(tb).zip(d.chunks_exact_mut(sb)) {
                            fmt.encode(&tf.decode(s), t);
                        }
                    }
                }
            }
        }
    }

    /// Reads an image the host cannot read back directly (or a
    /// multisampled one): copies it into a 32-bit-per-component image of
    /// its class first.
    fn read_through_copy(
        &mut self,
        id: u32,
        level: u32,
        region: Region,
        out: &mut [u8],
        row_pitch: usize,
        image_pitch: usize,
    ) {
        let Some(r) = self.resources.get(&id) else { return };
        let fmt = r.desc.format;
        let wide = match fmt.class() {
            Class::Uint => Format::Rgba32Uint,
            Class::Sint => Format::Rgba32Sint,
            Class::Unorm | Class::Snorm | Class::Float => Format::Rgba32Float,
            // Depth and stencil cannot be read in OpenGL ES.
            _ => return,
        };
        let desc = ResourceDesc {
            target: Target::Renderbuffer,
            format: wide,
            width: region.w,
            height: region.h,
            depth: 1,
            levels: 1,
            samples: 0,
        };
        let row = region.w as usize * 16;
        let Some(mut texels) = crate::pixels::try_zeroed(row * region.h as usize) else { return };
        let Ok(tmp) = self.create(&desc) else { return };
        for z in 0..region.d {
            let src = Surface { resource: id, level, layer: region.z + z };
            let dst = Surface { resource: tmp, level: 0, layer: 0 };
            let from = [region.x as i32, region.y as i32, region.w as i32, region.h as i32];
            let to = [0, 0, region.w as i32, region.h as i32];
            self.blit_surface(src, from, dst, to, MASK_RGBA, false, None);
            self.read_texture(
                tmp,
                0,
                Region::new(0, 0, 0, region.w, region.h, 1),
                &mut texels,
                row,
                row * region.h as usize,
            );
            for y in 0..region.h as usize {
                let d = &mut out[z as usize * image_pitch + y * row_pitch..][..region.w as usize * fmt.bytes()];
                for (s, t) in texels[y * row..][..row].as_chunks::<16>().0.iter().zip(d.chunks_exact_mut(fmt.bytes())) {
                    fmt.encode(&wide.decode(s), t);
                }
            }
        }
        self.destroy(tmp);
    }

    // ---- Queries ----------------------------------------------------------------

    fn begin(&mut self, kind: QueryKind) -> QueryId {
        let kind = match kind {
            QueryKind::AnySamplesPassedConservative => QUERY_OCCLUSION_PREDICATE_CONSERVATIVE,
            _ => QUERY_OCCLUSION_PREDICATE,
        };
        let id = unused_id(&self.queries, &mut self.next_query);
        self.queries.insert(id, Query { kind, parts: Vec::new(), fence: None, overflow: false });
        // Its first part begins at the first draw.
        self.active_query = Some(id);
        self.query_running = false;
        id
    }

    /// Before a draw of the application's: a part of its query runs.
    pub(super) fn run_query(&mut self) {
        if self.query_running || self.own_work > 0 {
            return;
        }
        let Some((id, kind)) = self.active_query.and_then(|id| self.queries.get(&id).map(|q| (id, q.kind))) else {
            return;
        };
        let part = self.query_part(kind);
        let Some(q) = self.queries.get_mut(&id) else { return };
        match part {
            Some(p) => {
                let object = p.object;
                q.parts.push(p);
                self.emit(CMD_BEGIN_QUERY, 0, &[object]);
                self.query_running = true;
            }
            None => q.overflow = true,
        }
    }

    /// A query object, with a slot of shared memory for its result.
    fn query_part(&mut self, kind: u32) -> Option<QueryPart> {
        let slot = self.query_slots.pop()?;
        let at = self.staging_size + slot * QUERY_SLOT;
        self.shared_mut(at, QUERY_SLOT).fill(0);
        let args = ResourceArgs {
            target: TARGET_BUFFER,
            format: format::R8_UNORM,
            bind: BIND_CUSTOM,
            width: QUERY_SLOT as u32,
            height: 1,
            depth: 1,
            array_size: 1,
            ..Default::default()
        };
        let Ok(resource) = self.t.create_resource(&args, Some(at..at + QUERY_SLOT)) else {
            self.query_slots.push(slot);
            return None;
        };
        let object = self.new_handle();
        self.emit(CMD_CREATE_OBJECT, OBJ_QUERY, &[object, kind, 0, resource]);
        Some(QueryPart { object, resource, slot })
    }

    /// Ends the running part of the active query, if one runs.
    fn stop_query(&mut self) {
        if !self.query_running {
            return;
        }
        self.query_running = false;
        let last = self.active_query.and_then(|id| self.queries.get(&id)).and_then(|q| q.parts.last());
        if let Some(object) = last.map(|p| p.object) {
            self.emit(CMD_END_QUERY, 0, &[object]);
        }
    }

    /// Does the renderer's own work, which may draw, outside the
    /// application's query.
    pub(super) fn unqueried<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        self.stop_query();
        self.own_work += 1;
        let r = f(self);
        self.own_work -= 1;
        r
    }

    fn end(&mut self, id: QueryId) {
        if self.active_query == Some(id) {
            self.stop_query();
            self.active_query = None;
        }
        self.request_results(id);
    }

    /// Asks the host for the results of a query's parts it has not written
    /// yet, then fences: it writes them to the parts' slots when it has
    /// them, which it looks at as fences signal.
    fn request_results(&mut self, id: QueryId) {
        let Some(q) = self.queries.get(&id) else { return };
        let pending: Vec<u32> = q.parts.iter().filter(|p| !self.part_done(p)).map(|p| p.object).collect();
        if pending.is_empty() {
            return;
        }
        for o in pending {
            self.emit(CMD_GET_QUERY_RESULT, 0, &[o, 0]);
        }
        let f = self.fence_now();
        if let Some(q) = self.queries.get_mut(&id) {
            q.fence = Some(f);
        }
    }

    /// Whether the host has written a part's result
    /// (`struct virgl_host_query_state { u32 state; u32 size; u64 result; }`).
    fn part_done(&self, p: &QueryPart) -> bool {
        self.shared_word(self.staging_size + p.slot * QUERY_SLOT) == QUERY_STATE_DONE
    }

    /// Whether samples passed, if the parts' results say yet.
    fn passed(&self, q: &Query) -> Option<bool> {
        let mut known = true;
        for p in &q.parts {
            let at = self.staging_size + p.slot * QUERY_SLOT;
            if !self.part_done(p) {
                known = false;
            } else if self.shared_word(at + 8) != 0 || self.shared_word(at + 12) != 0 {
                return Some(true);
            }
        }
        known.then_some(false)
    }

    fn result(&mut self, id: QueryId, wait: bool) -> Option<u64> {
        let q = self.queries.get(&id)?;
        if q.overflow {
            return Some(1);
        }
        if let Some(p) = self.passed(q) {
            return Some(u64::from(p));
        }
        let fence = q.fence;
        if wait {
            let pending: Vec<u32> = q.parts.iter().filter(|p| !self.part_done(p)).map(|p| p.object).collect();
            for o in pending {
                self.emit(CMD_GET_QUERY_RESULT, 0, &[o, 1]);
            }
            self.submit();
        } else {
            // The host writes the results (as `end` asked) when the fence
            // after the query signals.
            match fence {
                Some(f) if self.t.signaled(f) => {}
                _ => return None,
            }
        }
        match self.queries.get(&id).and_then(|q| self.passed(q)) {
            Some(p) => Some(u64::from(p)),
            // The host could not produce them (the device failed): report
            // that samples passed, the safe answer.
            None if wait => Some(1),
            None => {
                // The host looked when the fence signaled, before it had
                // them (zink has them a moment after), and looks again only
                // as fences signal: ask again, behind a fence of its own.
                self.request_results(id);
                None
            }
        }
    }

    fn drop_query(&mut self, id: QueryId) {
        if self.active_query == Some(id) {
            self.stop_query();
            self.active_query = None;
        }
        let Some(q) = self.queries.remove(&id) else { return };
        for p in q.parts {
            self.destroy_object(p.object);
            self.doom(p.resource, Some(p.slot));
        }
    }

    fn drop_program(&mut self, id: ProgramId) {
        self.guest_programs.remove(&id);
        if let Some(p) = self.programs.remove(&id) {
            self.destroy_object(p.vs);
            self.destroy_object(p.fs);
            for h in p.passes {
                self.destroy_object(h);
            }
        }
    }

    /// Makes a buffer's contents as this renderer keeps them current: read
    /// back if the GPU has written the buffer since.
    fn current_shadow(&mut self, id: u32) {
        let stale = self.resources.get(&id).and_then(|r| r.shadow.as_ref()).filter(|s| !s.valid);
        let Some(n) = stale.map(|s| s.data.len()) else { return };
        let Some(mut data) = crate::pixels::try_zeroed(n) else { return };
        self.read_buffer(id, 0, &mut data);
        if let Some(s) = self.resources.get_mut(&id).and_then(|r| r.shadow.as_mut()) {
            s.data = data;
            s.valid = true;
            s.ranges.clear();
        }
    }
}

/// What the front end may use, from the host's limits.
fn caps_for(h: &HostCaps) -> Caps {
    let max_2d = h.max_texture_2d_size.clamp(2048, 16384);
    let renders = |f: u32| h.can_render(f);
    // Renderbuffers may have 4 samples in the formats the host multisamples
    // as it keeps them (`formats`), and GL_MAX_SAMPLES says 4 only where
    // those are every format OpenGL ES 3.0 asks it of.
    let multisampled: BTreeSet<Format> = crate::format::SIZED
        .iter()
        .map(|i| i.format)
        .filter(|&f| h.max_samples >= 4 && formats::host_format(h, f).is_some_and(|hf| h.can_multisample(hf.virgl)))
        .collect();
    let mut required = crate::format::SIZED.iter().filter(|i| i.multisample_required());
    let max_samples = if required.all(|i| multisampled.contains(&i.format)) { 4 } else { 0 };
    Caps {
        // Mesa's driver of the GPU's ("softpipe", "Mesa Intel(R) Graphics
        // (ADL GT2)", "virgl (...)" under QEMU).
        name: alloc::string::String::from(h.renderer_name()),
        max_texture_size: max_2d,
        max_3d_texture_size: h.max_texture_3d_size.clamp(256, 2048),
        max_cube_map_size: h.max_texture_cube_size.clamp(2048, 16384),
        max_array_layers: h.max_texture_array_layers.clamp(256, 2048),
        max_renderbuffer_size: max_2d,
        max_samples,
        multisampled,
        max_viewport: max_2d,
        color_buffer_float: [
            format::R16_FLOAT,
            format::R16G16_FLOAT,
            format::R16G16B16A16_FLOAT,
            format::R32_FLOAT,
            format::R32G32_FLOAT,
            format::R32G32B32A32_FLOAT,
            format::R11G11B10_FLOAT,
        ]
        .into_iter()
        .all(renders),
        float_linear: false,
        line_width: (1.0, h.line_width.1.max(1.0)),
        point_size: (1.0, h.point_size.1.max(1.0)),
        max_anisotropy: h.max_anisotropy.max(1.0),
    }
}

impl Backend for VirglBackend {
    fn caps(&self) -> &Caps {
        &self.caps
    }

    fn create_resource(&mut self, desc: &ResourceDesc) -> Result<ResourceId, OutOfMemory> {
        // A buffer kept holds what it held; a new resource reads as zero
        // only where the host makes it so.
        let (id, zeroed) = match self.take_kept(desc)? {
            Some(id) => (id, false),
            None => (self.create_new(desc)?, self.t.zeroes_new_resources()),
        };
        if !zeroed {
            // Clearing may draw: outside the application's queries.
            self.unqueried(|b| b.zero(id, desc));
        }
        Ok(id)
    }

    fn destroy_resource(&mut self, id: ResourceId) {
        self.destroy(id);
    }

    fn import_resource(&mut self, desc: &ResourceDesc, memory: &External) -> Result<ResourceId, ImportError> {
        // A renderbuffer is drawn into (and read: copies, blits from it), a
        // texture sampled alone; a display shows what is drawn.
        let bind = match desc.target {
            Target::Renderbuffer if desc.samples == 0 => BIND_RENDER_TARGET | BIND_SAMPLER_VIEW,
            Target::Texture2D if !memory.is_scanout() => BIND_SAMPLER_VIEW,
            _ => return Err(ImportError::Unsupported),
        };
        let bind = if memory.is_scanout() { bind | BIND_SCANOUT } else { bind };
        let takes = if memory.is_scanout() { IMPORT_DISPLAY } else { IMPORT_SHARED };
        if desc.depth != 1 || desc.levels != 1 || self.t.imports() & takes == 0 {
            return Err(ImportError::Unsupported);
        }
        // Kept in the memory's own order, which the host renders and samples.
        let virgl = match (desc.format, memory.bgr()) {
            (Format::Rgbx8Unorm, true) => format::B8G8R8X8_UNORM,
            (Format::Rgbx8Unorm, false) => format::R8G8B8X8_UNORM,
            (Format::Rgba8Unorm, true) => format::B8G8R8A8_UNORM,
            (Format::Rgba8Unorm, false) => format::R8G8B8A8_UNORM,
            _ => return Err(ImportError::Unsupported),
        };
        let args = ResourceArgs {
            target: TARGET_2D,
            format: virgl,
            bind,
            width: desc.width,
            height: desc.height,
            depth: 1,
            array_size: 1,
            ..Default::default()
        };
        let handle = self.t.import_resource(&args, memory)?;
        let host = formats::HostFormat { virgl, layout: None, swizzle: [0, 1, 2, 3], renderable: true };
        let record =
            Res { desc: *desc, capacity: 0, host: Some(host), views: Vec::new(), surfaces: Vec::new(), shadow: None };
        self.resources.insert(handle, record);
        Ok(handle)
    }

    fn write(&mut self, id: ResourceId, level: u32, region: Region, data: &[u8], row_pitch: usize, image_pitch: usize) {
        let Some(d) = self.resources.get(&id).map(|r| r.desc) else { return };
        let texel = if d.target == Target::Buffer { 1 } else { d.format.bytes() };
        if transfer_bytes(&region, texel, row_pitch, image_pitch).is_none_or(|n| data.len() < n) {
            return;
        }
        match d.target {
            Target::Buffer => self.write_buffer(id, region.x as usize, &data[..region.w as usize]),
            _ => self.write_texture(id, level, region, data, row_pitch, image_pitch),
        }
    }

    fn read(
        &mut self,
        id: ResourceId,
        level: u32,
        region: Region,
        out: &mut [u8],
        row_pitch: usize,
        image_pitch: usize,
    ) {
        let Some(d) = self.resources.get(&id).map(|r| r.desc) else { return };
        let texel = if d.target == Target::Buffer { 1 } else { d.format.bytes() };
        if transfer_bytes(&region, texel, row_pitch, image_pitch).is_none_or(|n| out.len() < n) {
            return;
        }
        match d.target {
            Target::Buffer => self.read_buffer(id, region.x as usize, &mut out[..region.w as usize]),
            _ => self.unqueried(|b| b.read_texture(id, level, region, out, row_pitch, image_pitch)),
        }
    }

    fn copy_buffer(&mut self, src: ResourceId, src_offset: usize, dst: ResourceId, dst_offset: usize, size: usize) {
        self.copy_buffer_region(src, src_offset, dst, dst_offset, size);
    }

    fn copy_region(
        &mut self,
        src: ResourceId,
        src_level: u32,
        region: Region,
        dst: ResourceId,
        dst_level: u32,
        x: u32,
        y: u32,
        z: u32,
    ) {
        self.unqueried(|b| b.copy_box(src, src_level, region, dst, dst_level, [x, y, z]));
    }

    fn copy_to_texture(&mut self, src: &Framebuffer, read: u8, rect: Rect, dst: Surface, x: u32, y: u32) {
        self.unqueried(|b| b.copy_framebuffer(src, read, rect, dst, x, y));
    }

    fn blit(&mut self, blit: &Blit) {
        self.unqueried(|b| b.blit_framebuffers(blit));
    }

    fn generate_mipmap(&mut self, id: ResourceId, base: u32, last: u32) {
        self.unqueried(|b| b.mipmap(id, base, last));
    }

    fn create_program(&mut self, program: Arc<glsl::program::Program>) -> ProgramId {
        self.program(program)
    }

    fn destroy_program(&mut self, id: ProgramId) {
        self.drop_program(id);
    }

    fn draw(&mut self, state: &DrawState<'_>, info: &DrawInfo) {
        self.draw_call(state, info);
    }

    fn clear(&mut self, framebuffer: &Framebuffer, clear: &Clear) {
        self.unqueried(|b| b.clear_buffers(framebuffer, clear));
    }

    fn begin_query(&mut self, kind: QueryKind) -> QueryId {
        self.begin(kind)
    }

    fn end_query(&mut self, id: QueryId) {
        self.end(id);
    }

    fn destroy_query(&mut self, id: QueryId) {
        self.drop_query(id);
    }

    fn query_result(&mut self, id: QueryId, wait: bool) -> Option<u64> {
        self.result(id, wait)
    }

    fn flush(&mut self) {
        self.submit();
    }

    fn finish(&mut self) {
        self.sync();
    }

    fn present(&mut self, color: ResourceId, width: u32, height: u32, dst: &mut Present<'_>) {
        self.unqueried(|b| b.present_image(color, width, height, dst));
    }

    fn format_of(&self, id: ResourceId) -> Option<Format> {
        self.resources.get(&id).map(|r| r.desc.format)
    }

    fn fence(&mut self) -> u64 {
        self.fence_now()
    }

    fn wait_fence(&mut self, fence: u64, timeout_ns: u64) -> bool {
        if self.lost || self.t.signaled(fence) {
            return true;
        }
        if timeout_ns == 0 {
            return false;
        }
        match self.t.wait(fence, timeout_ns) {
            Ok(signaled) => signaled,
            // Nothing will ever happen on a lost context.
            Err(Lost) => {
                self.lost = true;
                true
            }
        }
    }
}
