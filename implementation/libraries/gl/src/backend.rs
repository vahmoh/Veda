// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The interface between the OpenGL ES front end and a renderer.
//!
//! The front end ([`crate::Context`]) checks every call against the
//! specification, keeps the GL's objects and state, and converts pixel
//! data; a [`Backend`] only renders. Its model is Gallium's (which Mesa's
//! drivers and virglrenderer share): resources with a storage [`Format`],
//! surfaces (a level and layer of a resource), and draws described by
//! complete, resolved state ([`DrawState`]): which buffers feed which
//! attributes, which textures and sampler states feed which samplers,
//! the fixed-function state, the framebuffer.
//!
//! Two back ends implement it: the software renderer ([`crate::soft`]) and
//! the virgl renderer ([`crate::virgl`]), which runs on the GPU.

use alloc::collections::BTreeSet;
use alloc::string::String;
use alloc::sync::Arc;

use crate::format::Format;

/// A resource: a buffer, texture or renderbuffer.
pub type ResourceId = u32;
/// A compiled program.
pub type ProgramId = u32;
/// A query object.
pub type QueryId = u32;

/// An identifier `ids` holds no entry for: the next of a count from 1 (kept
/// in `next`), past those still in use once the count wraps around.
pub(crate) fn unused_id<V>(ids: &alloc::collections::BTreeMap<u32, V>, next: &mut u32) -> u32 {
    loop {
        let id = *next;
        *next = id.checked_add(1).unwrap_or(1);
        if !ids.contains_key(&id) {
            return id;
        }
    }
}

/// The renderer could not allocate memory (`GL_OUT_OF_MEMORY`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct OutOfMemory;

/// Why memory from outside was not made a resource
/// ([`Backend::import_resource`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ImportError {
    /// The renderer never takes such memory (`GL_INVALID_OPERATION`): its
    /// GPU cannot reach it so, or it is no GPU.
    Unsupported,
    /// Not now: past a limit, or out of memory (`GL_OUT_OF_MEMORY`).
    NoMemory,
}

impl From<OutOfMemory> for ImportError {
    fn from(_: OutOfMemory) -> ImportError {
        ImportError::NoMemory
    }
}

/// What a resource is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Target {
    Buffer,
    Texture2D,
    Texture3D,
    /// Six faces as layers (+X, -X, +Y, -Y, +Z, -Z).
    TextureCube,
    Texture2DArray,
    /// A render target that cannot be sampled (it may be multisampled).
    Renderbuffer,
}

/// A resource's description.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ResourceDesc {
    pub target: Target,
    pub format: Format,
    /// Texels (bytes for buffers).
    pub width: u32,
    pub height: u32,
    /// 3D depth, array layers, 6 for cube maps, 1 otherwise.
    pub depth: u32,
    pub levels: u32,
    /// Samples per pixel (0 for single-sampled).
    pub samples: u32,
}

/// The bytes a transfer of `region` ([`Backend::write`], [`Backend::read`])
/// spans: its images `image_pitch` bytes apart, their rows `row_pitch`
/// apart, each row `region.w` texels of `texel` bytes (a buffer's bytes:
/// texels of 1 byte, one row). `None` if the pitches are too small for the
/// rows and images they separate, or the size does not fit in memory.
pub fn transfer_bytes(region: &Region, texel: usize, row_pitch: usize, image_pitch: usize) -> Option<usize> {
    let row = (region.w as usize).checked_mul(texel)?;
    if region.w == 0 || region.h == 0 || region.d == 0 {
        return Some(0);
    }
    let image = (region.h as usize - 1).checked_mul(row_pitch)?.checked_add(row)?;
    if (region.h > 1 && row_pitch < row) || (region.d > 1 && image_pitch < image) {
        return None;
    }
    (region.d as usize - 1).checked_mul(image_pitch)?.checked_add(image)
}

/// A dimension of mipmap level `level` of an image `size` wide: halved per
/// level, never below 1.
pub fn minify(size: u32, level: u32) -> u32 {
    (size >> level.min(31)).max(1)
}

impl ResourceDesc {
    /// Level `level`'s width and height (a buffer's bytes and 1) and its
    /// images: 3D slices (halved per level), array layers or cube faces
    /// (6), 1 otherwise.
    pub fn level_size(&self, level: u32) -> (u32, u32, u32) {
        let (w, h) = (minify(self.width, level), minify(self.height, level));
        match self.target {
            Target::Buffer => (self.width, 1, 1),
            Target::Texture3D => (w, h, minify(self.depth, level)),
            _ => (w, h, self.depth.max(1)),
        }
    }
}

/// One image of a resource.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Surface {
    pub resource: ResourceId,
    pub level: u32,
    /// Cube face, array layer or 3D slice.
    pub layer: u32,
}

/// A box of texels (or a byte range of a buffer: `x` and `w`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Region {
    pub x: u32,
    pub y: u32,
    pub z: u32,
    pub w: u32,
    pub h: u32,
    pub d: u32,
}

impl Region {
    pub fn new(x: u32, y: u32, z: u32, w: u32, h: u32, d: u32) -> Region {
        Region { x, y, z, w, h, d }
    }

    pub fn bytes(offset: usize, size: usize) -> Region {
        Region { x: offset as u32, y: 0, z: 0, w: size as u32, h: 1, d: 1 }
    }
}

/// A screen rectangle (pixels; y from the bottom, as GL counts).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// A comparison (depth, stencil, shadow lookups).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Func {
    Never,
    Less,
    Equal,
    LessEqual,
    Greater,
    NotEqual,
    GreaterEqual,
    Always,
}

impl Func {
    /// `a func b`.
    #[inline(always)]
    pub fn test<T: PartialOrd>(self, a: T, b: T) -> bool {
        match self {
            Func::Never => false,
            Func::Less => a < b,
            Func::Equal => a == b,
            Func::LessEqual => a <= b,
            Func::Greater => a > b,
            Func::NotEqual => a != b,
            Func::GreaterEqual => a >= b,
            Func::Always => true,
        }
    }
}

/// A stencil operation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StencilOp {
    Keep,
    Zero,
    Replace,
    Incr,
    Decr,
    Invert,
    IncrWrap,
    DecrWrap,
}

/// One face's stencil state.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Stencil {
    pub func: Func,
    pub reference: i32,
    pub value_mask: u32,
    pub write_mask: u32,
    pub fail: StencilOp,
    pub depth_fail: StencilOp,
    pub pass: StencilOp,
}

/// Depth and stencil testing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DepthStencil {
    pub depth_test: bool,
    pub depth_func: Func,
    pub depth_write: bool,
    pub stencil_test: bool,
    pub front: Stencil,
    pub back: Stencil,
}

/// A blend equation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BlendEq {
    Add,
    Subtract,
    ReverseSubtract,
    Min,
    Max,
}

/// A blend factor.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BlendFactor {
    Zero,
    One,
    SrcColor,
    OneMinusSrcColor,
    DstColor,
    OneMinusDstColor,
    SrcAlpha,
    OneMinusSrcAlpha,
    DstAlpha,
    OneMinusDstAlpha,
    ConstantColor,
    OneMinusConstantColor,
    ConstantAlpha,
    OneMinusConstantAlpha,
    SrcAlphaSaturate,
}

/// Blending and color writes.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Blend {
    pub enabled: bool,
    pub eq_rgb: BlendEq,
    pub eq_alpha: BlendEq,
    pub src_rgb: BlendFactor,
    pub dst_rgb: BlendFactor,
    pub src_alpha: BlendFactor,
    pub dst_alpha: BlendFactor,
    pub color: [f32; 4],
    /// Red, green, blue, alpha writes (all draw buffers).
    pub write_mask: [bool; 4],
    pub dither: bool,
}

/// Which faces are culled.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cull {
    Front,
    Back,
    Both,
}

/// Rasterization state.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Raster {
    pub cull: Option<Cull>,
    /// Counter-clockwise triangles face the front.
    pub front_ccw: bool,
    /// Polygon offset (factor, units) for filled triangles.
    pub polygon_offset: Option<(f32, f32)>,
    /// `GL_RASTERIZER_DISCARD`.
    pub discard: bool,
    pub line_width: f32,
    /// `GL_SAMPLE_ALPHA_TO_COVERAGE`.
    pub alpha_to_coverage: bool,
    /// `GL_SAMPLE_COVERAGE` (value, invert), if enabled.
    pub sample_coverage: Option<(f32, bool)>,
}

/// The viewport transform.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Viewport {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub near: f32,
    pub far: f32,
}

/// What is drawn into.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Framebuffer {
    /// Color attachments by attachment number.
    pub colors: [Option<Surface>; 8],
    pub depth: Option<Surface>,
    pub stencil: Option<Surface>,
    pub width: u32,
    pub height: u32,
    pub samples: u32,
    /// Fragment output location → color attachment (`glDrawBuffers`).
    pub draw_buffers: [Option<u8>; 8],
}

/// A texture's filter.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Filter {
    Nearest,
    Linear,
}

/// How texture coordinates outside [0, 1] wrap.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Wrap {
    Repeat,
    ClampToEdge,
    MirroredRepeat,
}

/// Sampler state.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct SamplerState {
    pub min: Filter,
    pub mag: Filter,
    /// Filtering between mipmap levels, or none (no mipmapping).
    pub mip: Option<Filter>,
    pub wrap: [Wrap; 3],
    pub min_lod: f32,
    pub max_lod: f32,
    /// Depth comparison (shadow samplers).
    pub compare: Option<Func>,
    pub max_anisotropy: f32,
}

/// A texture as a sampler sees it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct View {
    pub resource: ResourceId,
    pub target: Target,
    /// The levels the sampler may use.
    pub base_level: u32,
    pub max_level: u32,
    /// For each of red, green, blue and alpha: 0-3 a component, 4 zero,
    /// 5 one (`GL_TEXTURE_SWIZZLE_*`).
    pub swizzle: [u8; 4],
}

/// A sampler's binding: a complete texture and its sampler state, or
/// nothing (an incomplete texture samples as (0, 0, 0, 1)).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct TextureBinding {
    pub view: Option<View>,
    pub sampler: SamplerState,
}

/// A vertex attribute's data type.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AttribType {
    Byte,
    UnsignedByte,
    Short,
    UnsignedShort,
    Int,
    UnsignedInt,
    HalfFloat,
    Float,
    Fixed,
    Int2101010Rev,
    UnsignedInt2101010Rev,
}

impl AttribType {
    /// Bytes per component (the whole value for packed types).
    pub fn bytes(self) -> u32 {
        match self {
            AttribType::Byte | AttribType::UnsignedByte => 1,
            AttribType::Short | AttribType::UnsignedShort | AttribType::HalfFloat => 2,
            _ => 4,
        }
    }

    pub fn packed(self) -> bool {
        matches!(self, AttribType::Int2101010Rev | AttribType::UnsignedInt2101010Rev)
    }
}

/// Where a vertex attribute comes from.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Attrib {
    /// The buffer, if the array is enabled; otherwise the current value.
    pub buffer: Option<ResourceId>,
    pub offset: usize,
    /// Bytes from one element to the next (never 0: the front end
    /// computes tightly packed strides).
    pub stride: u32,
    pub size: u8,
    pub ty: AttribType,
    pub normalized: bool,
    /// `VertexAttribIPointer`: integers reach the shader unconverted.
    pub integer: bool,
    /// Instances per element (0: per vertex).
    pub divisor: u32,
    /// The current value's 32-bit components (when no array is enabled).
    pub current: [u32; 4],
}

/// A range of a buffer (uniform blocks, transform feedback).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BufferRange {
    pub buffer: ResourceId,
    pub offset: usize,
    pub size: usize,
}

/// Primitive kinds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Points,
    Lines,
    LineLoop,
    LineStrip,
    Triangles,
    TriangleStrip,
    TriangleFan,
}

/// Index types.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IndexType {
    U8,
    U16,
    U32,
}

impl IndexType {
    pub fn bytes(self) -> usize {
        match self {
            IndexType::U8 => 1,
            IndexType::U16 => 2,
            IndexType::U32 => 4,
        }
    }

    /// The primitive restart index (`GL_PRIMITIVE_RESTART_FIXED_INDEX`).
    pub fn restart(self) -> u32 {
        match self {
            IndexType::U8 => 0xFF,
            IndexType::U16 => 0xFFFF,
            IndexType::U32 => 0xFFFF_FFFF,
        }
    }
}

/// Everything a draw needs.
#[derive(Clone, Copy)]
pub struct DrawState<'a> {
    pub framebuffer: &'a Framebuffer,
    pub viewport: Viewport,
    pub scissor: Option<Rect>,
    pub raster: Raster,
    pub depth_stencil: DepthStencil,
    pub blend: Blend,
    pub program: ProgramId,
    /// Default-block uniform storage.
    pub uniforms: &'a [[u32; 4]],
    /// The buffer range of each uniform block (by program block index).
    pub blocks: &'a [Option<BufferRange>],
    /// The texture of each sampler (by sampler index).
    pub textures: &'a [TextureBinding],
    /// Attributes by location.
    pub attribs: &'a [Attrib],
    /// The index buffer, for indexed draws.
    pub index: Option<(ResourceId, IndexType)>,
    pub primitive_restart: bool,
    /// Transform feedback buffers, if feedback is active.
    pub feedback: Option<&'a [BufferRange]>,
}

/// One draw call.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DrawInfo {
    pub mode: Mode,
    /// First vertex (non-indexed) or byte offset into the index buffer.
    pub first: usize,
    pub count: u32,
    pub indexed: bool,
    pub instances: u32,
}

/// What `glClear`/`glClearBuffer*` clear.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Clear {
    /// Color attachments to clear, by attachment number, with their value
    /// (32-bit components: floats, or integers for integer formats).
    pub colors: [Option<[u32; 4]>; 8],
    pub depth: Option<f32>,
    pub stencil: Option<i32>,
    pub scissor: Option<Rect>,
    pub color_mask: [bool; 4],
    pub stencil_mask: u32,
}

/// A framebuffer blit.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Blit {
    pub src: Framebuffer,
    /// The read buffer (color attachment number).
    pub src_color: Option<u8>,
    pub dst: Framebuffer,
    /// Source and destination rectangles (x0, y0, x1, y1): mirrored when
    /// x1 < x0.
    pub src_rect: [i32; 4],
    pub dst_rect: [i32; 4],
    pub color: bool,
    pub depth: bool,
    pub stencil: bool,
    pub filter: Filter,
    pub scissor: Option<Rect>,
}

/// A query's kind.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum QueryKind {
    AnySamplesPassed,
    AnySamplesPassedConservative,
    PrimitivesWritten,
}

/// Memory from outside that a resource is made of, which the GPU renders
/// into or samples in place: a display's picture ([`External::scanout`]),
/// or a picture processors read and write too (a page WebKit renders into
/// the memory the window system shows it from, which the window system
/// samples). Rows of 32-bit pixels.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct External {
    memory: Memory,
    size: usize,
    stride: u32,
    bgr: bool,
    scanout: bool,
}

/// Where the memory of an [`External`] is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Memory {
    /// A memory object: a Veda handle, which the renderer duplicates (the
    /// caller keeps its own).
    Handle(u32),
    /// An address in this process (host tests, whose renderer draws into
    /// it from its own threads).
    Address(usize),
    /// A memory file of the host's (`memfd_create`, sealed against
    /// shrinking) mapped at an address in this process (host tests): a GPU's
    /// driver takes it as a dma-buf (`udmabuf`), softpipe at the address.
    #[cfg(all(target_os = "linux", any(test, feature = "host-virgl")))]
    File(i32, usize),
}

impl External {
    /// `size` bytes of the memory object `handle`, rows of pixels `stride`
    /// bytes apart, with blue in the low byte of each (`0xAARRGGBB`) if
    /// `bgr`, red otherwise. Without the right to write, the GPU only
    /// samples it.
    pub fn handle(handle: u32, size: usize, stride: u32, bgr: bool) -> External {
        External { memory: Memory::Handle(handle), size, stride, bgr, scanout: false }
    }

    /// `size` bytes at `address` in this process, laid out as for
    /// [`External::handle`].
    ///
    /// # Safety
    ///
    /// The memory must be valid for reads and writes of `size` bytes, from
    /// any thread, for as long as a resource made of it lives, and not be
    /// accessed otherwise meanwhile but to read what was drawn once a fence
    /// after the drawing has signaled.
    pub unsafe fn address(address: *mut u8, size: usize, stride: u32, bgr: bool) -> External {
        External { memory: Memory::Address(address as usize), size, stride, bgr, scanout: false }
    }

    /// The memory file `fd`, `size` bytes of it mapped at `address`, laid
    /// out as for [`External::handle`].
    ///
    /// # Safety
    ///
    /// As for [`External::address`], of the mapping; the file stays open
    /// while a resource made of it lives.
    #[cfg(all(target_os = "linux", any(test, feature = "host-virgl")))]
    pub unsafe fn file(fd: i32, address: *mut u8, size: usize, stride: u32, bgr: bool) -> External {
        External { memory: Memory::File(fd, address as usize), size, stride, bgr, scanout: false }
    }

    /// The same memory as a display's picture: what the GPU draws into it
    /// reaches the memory, where a display reads it, rather than staying
    /// in caches a display does not read.
    pub fn scanout(self) -> External {
        External { scanout: true, ..self }
    }

    /// Where the memory is.
    pub fn memory(&self) -> Memory {
        self.memory
    }

    /// Whether it is a display's picture ([`External::scanout`]).
    pub fn is_scanout(&self) -> bool {
        self.scanout
    }

    /// Its bytes.
    pub fn size(&self) -> usize {
        self.size
    }

    /// Bytes from one row to the next.
    pub fn stride(&self) -> u32 {
        self.stride
    }

    /// Blue in the low byte of each pixel (`0xXXRRGGBB`); otherwise red.
    pub fn bgr(&self) -> bool {
        self.bgr
    }
}

/// What a renderer can do.
#[derive(Clone, Debug)]
pub struct Caps {
    /// `GL_RENDERER`.
    pub name: String,
    pub max_texture_size: u32,
    pub max_3d_texture_size: u32,
    pub max_cube_map_size: u32,
    pub max_array_layers: u32,
    pub max_renderbuffer_size: u32,
    /// `GL_MAX_SAMPLES`: 4 if renderbuffers may have 4 samples in every
    /// format OpenGL ES 3.0 asks it of
    /// ([`Internal::multisample_required`](crate::format::Internal::multisample_required)),
    /// otherwise 0.
    pub max_samples: u32,
    /// The storage formats renderbuffers of which may have 4 samples, if
    /// `max_samples` is 4: those, and any others the renderer multisamples.
    pub multisampled: BTreeSet<Format>,
    pub max_viewport: u32,
    /// `EXT_color_buffer_float`.
    pub color_buffer_float: bool,
    /// `OES_texture_float_linear`.
    pub float_linear: bool,
    /// `GL_ALIASED_LINE_WIDTH_RANGE` (min, max).
    pub line_width: (f32, f32),
    /// `GL_ALIASED_POINT_SIZE_RANGE` (min, max).
    pub point_size: (f32, f32),
    pub max_anisotropy: f32,
}

/// A renderer.
pub trait Backend: Send {
    fn caps(&self) -> &Caps;

    /// Creates a resource. A new resource reads as zero, every byte of
    /// every level and layer, whatever the memory it is made of held
    /// before: never another context's (or application's) old data.
    fn create_resource(&mut self, desc: &ResourceDesc) -> Result<ResourceId, OutOfMemory>;
    fn destroy_resource(&mut self, id: ResourceId);
    /// A resource `desc` whose storage is `memory`, in its pixels' order: a
    /// renderbuffer of 8-bit RGB or RGBA, single sampled, which the GPU
    /// renders into (and reads from), or a 2D texture of one level of them,
    /// which it only samples. Renderers that cannot refuse: `Unsupported`
    /// if they never can, `NoMemory` if not now (past a limit).
    fn import_resource(&mut self, desc: &ResourceDesc, memory: &External) -> Result<ResourceId, ImportError> {
        let _ = (desc, memory);
        Err(ImportError::Unsupported)
    }

    /// Writes data laid out in the resource's format (rows `row_pitch`
    /// bytes apart, images `image_pitch`; a buffer's bytes are one row)
    /// into a region of a level. `data` holds at least the bytes
    /// [`transfer_bytes`] gives; a call with less, or with pitches too
    /// small for the region, does nothing.
    fn write(&mut self, id: ResourceId, level: u32, region: Region, data: &[u8], row_pitch: usize, image_pitch: usize);
    /// Reads a region of a level, laid out as for [`Backend::write`] (and
    /// as it says, `out` is long enough or nothing is read).
    fn read(
        &mut self,
        id: ResourceId,
        level: u32,
        region: Region,
        out: &mut [u8],
        row_pitch: usize,
        image_pitch: usize,
    );
    /// Copies bytes between buffers (or within one).
    fn copy_buffer(&mut self, src: ResourceId, src_offset: usize, dst: ResourceId, dst_offset: usize, size: usize);
    /// Copies a box of texels (`region` of level `src_level`) to level
    /// `dst_level` of another resource of the same format, at `(x, y, z)`.
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
    );
    /// Copies a framebuffer rectangle into a texture image
    /// (`CopyTexSubImage`), converting formats.
    fn copy_to_texture(&mut self, src: &Framebuffer, read: u8, rect: Rect, dst: Surface, x: u32, y: u32);
    fn blit(&mut self, blit: &Blit);
    /// Fills levels `base + 1..=last` from `base` (box filtered).
    fn generate_mipmap(&mut self, id: ResourceId, base: u32, last: u32);

    /// Compiles a linked program for this renderer.
    fn create_program(&mut self, program: Arc<glsl::program::Program>) -> ProgramId;
    fn destroy_program(&mut self, id: ProgramId);

    fn draw(&mut self, state: &DrawState<'_>, info: &DrawInfo);
    fn clear(&mut self, framebuffer: &Framebuffer, clear: &Clear);

    fn begin_query(&mut self, kind: QueryKind) -> QueryId;
    fn end_query(&mut self, id: QueryId);
    fn destroy_query(&mut self, id: QueryId);
    /// A query's result, if available (waiting for it with `wait`).
    fn query_result(&mut self, id: QueryId, wait: bool) -> Option<u64>;

    /// Starts the work queued so far.
    fn flush(&mut self);
    /// Waits for all work to be done.
    fn finish(&mut self);
    /// Shows a color buffer (a 2D image `width` x `height`, resolved if
    /// multisampled) in a window's pixels, scaled to them (bilinearly) if
    /// their size differs.
    fn present(&mut self, color: ResourceId, width: u32, height: u32, dst: &mut Present<'_>) {
        present_by_reading(self, color, width, height, dst);
    }
    /// The format of a resource.
    fn format_of(&self, id: ResourceId) -> Option<Format>;
    /// A fence that signals when the work queued so far is done.
    fn fence(&mut self) -> u64;
    /// Whether `fence` has signaled, waiting for it up to `timeout_ns`.
    fn wait_fence(&mut self, fence: u64, timeout_ns: u64) -> bool;
}

/// A window's pixels, as [`Backend::present`] fills them: `height` rows of
/// `width` 0xAARRGGBB words, from the top down, `stride` words apart. The
/// rows never overlap and all lie in the slice ([`Present::new`] sees to
/// it).
pub struct Present<'a> {
    pixels: &'a mut [u32],
    stride: usize,
    width: u32,
    height: u32,
    opaque: bool,
}

impl<'a> Present<'a> {
    /// `width` x `height` pixels in `pixels`, rows `stride` words apart,
    /// opaque (alpha is 255) or else premultiplied by their alpha; `None`
    /// unless every row fits, apart from the others: `stride` at least
    /// `width`, and `pixels` long enough for the last row.
    pub fn new(pixels: &'a mut [u32], stride: usize, width: u32, height: u32, opaque: bool) -> Option<Present<'a>> {
        let w = width as usize;
        let end = (height as usize).checked_sub(1).map_or(Some(0), |last| last.checked_mul(stride)?.checked_add(w));
        (stride >= w && end.is_some_and(|end| end <= pixels.len())).then_some(Present {
            pixels,
            stride,
            width,
            height,
            opaque,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// Words from one row to the next.
    pub fn stride(&self) -> usize {
        self.stride
    }

    /// Whether alpha is 255 (otherwise colors are premultiplied by it).
    pub fn opaque(&self) -> bool {
        self.opaque
    }

    /// The pixels: row `y` starts at `y * stride`.
    pub fn pixels(&mut self) -> &mut [u32] {
        self.pixels
    }

    /// Row `y` (0 at the top): its `width` pixels.
    pub fn row(&mut self, y: usize) -> &mut [u32] {
        &mut self.pixels[y * self.stride..][..self.width as usize]
    }
}

/// A color as a 0xAARRGGBB word: opaque, or premultiplied by its alpha.
#[inline]
pub fn to_argb(c: [f32; 4], opaque: bool) -> u32 {
    let u = |x: f32| (crate::format::clamp01(x) * 255.0 + 0.5) as u32;
    let a = if opaque { 1.0 } else { c[3] };
    let k = if opaque { 1.0 } else { a };
    (u(a) << 24) | (u(c[0] * k) << 16) | (u(c[1] * k) << 8) | u(c[2] * k)
}

/// Presents by reading the color buffer back row by row and converting
/// and scaling on the CPU (the default [`Backend::present`]).
pub fn present_by_reading<B: Backend + ?Sized>(
    b: &mut B,
    color: ResourceId,
    width: u32,
    height: u32,
    dst: &mut Present<'_>,
) {
    let Some(format) = b.format_of(color) else { return };
    let tb = format.bytes();
    let row = width as usize * tb;
    let (Some(mut texels), Some(mut image)) =
        (crate::pixels::try_zeroed(row), crate::pixels::try_filled(width as usize * height as usize, 0u32))
    else {
        return;
    };
    for y in 0..height as usize {
        // Row 0 of a GL image is the bottom one.
        let src = height as usize - 1 - y;
        b.read(color, 0, Region::new(0, src as u32, 0, width, 1, 1), &mut texels, row, row);
        let line = &mut image[y * width as usize..(y + 1) * width as usize];
        for (d, t) in line.iter_mut().zip(texels.chunks_exact(tb)) {
            *d = to_argb(crate::pixels::decode_raw(format, t).as_float(), dst.opaque);
        }
    }
    let rows = 0..dst.height as usize;
    crate::present::scale(&image, width as usize, height as usize, dst, rows);
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::BTreeMap;
    use alloc::vec::Vec;

    #[test]
    fn identifiers_step_over_those_in_use() {
        let mut ids = BTreeMap::new();
        let mut next = 1;
        for _ in 0..3 {
            let id = unused_id(&ids, &mut next);
            ids.insert(id, ());
        }
        assert_eq!(ids.keys().copied().collect::<Vec<_>>(), [1, 2, 3]);
        // Wrapping around, the count skips 1 to 3, which live on.
        next = u32::MAX;
        assert_eq!(unused_id(&ids, &mut next), u32::MAX);
        assert_eq!(unused_id(&ids, &mut next), 4);
    }

    #[test]
    fn transfers_span_their_rows_and_images() {
        let r = Region::new(0, 0, 0, 3, 2, 2);
        // Rows of 12 bytes 16 apart, images 40 apart: the last row ends at
        // 40 + 16 + 12.
        assert_eq!(transfer_bytes(&r, 4, 16, 40), Some(68));
        // Rows closer than they are long, images closer than their rows.
        assert_eq!(transfer_bytes(&r, 4, 8, 40), None);
        assert_eq!(transfer_bytes(&r, 4, 16, 20), None);
        assert_eq!(transfer_bytes(&Region::bytes(5, 9), 1, 9, 9), Some(9));
        assert_eq!(transfer_bytes(&Region::new(0, 0, 0, 0, 4, 4), 4, 0, 0), Some(0));
        assert_eq!(transfer_bytes(&Region::new(0, 0, 0, u32::MAX, u32::MAX, 3), 16, usize::MAX, usize::MAX), None);
    }
}
