// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `renderer` — Veda's renderer, in the driver VM: the OpenGL ES command
//! streams of Veda's applications (vgl's, in virglrenderer's protocol)
//! carried out on the GPU by Mesa's Gallium driver for it, over the GPU's
//! Linux driver: iris on i915 or xe (Intel's GPUs), virgl on virtio-gpu
//! (QEMU's, which renders on the host's GPU); or on softpipe, Mesa's
//! reference rasterizer, for tests.
//!
//! The program is Mesa's build of the decoder (`decoder/`: a Gallium screen
//! per device, a context per client, every command checked before it
//! reaches the driver) and of its libdrm (`drm/`), around this, its Rust
//! half, which serves the `gpu` protocol over the bridge.
//!
//! Each connection gets a context and a block of Veda's memory both sides
//! map: a page whose first word is the number of the last fence that
//! signaled, the command area submissions are read from, and the shared
//! area resources may use as their storage (vgl's staging and query
//! results). Commands are copied out of the command area before they are
//! decoded, so a client changing them meanwhile cannot get them past the
//! checks. Memory a client gives to render into (a display's picture)
//! reaches a GPU's driver as a dma-buf of the VMO, which the GPU reads and
//! writes through the IOMMU, and softpipe as the VMO mapped here.
//!
//! The renderer's clients are Veda's processes, which it knows by the
//! identity the registry gives their connections: each process has a few
//! connections at most, and its contexts' memory and the memory it gives
//! to render into are bounded (so that one cannot take what the others,
//! and the driver VM's other drivers, need); so are all of them together.
//!
//! `renderer [softpipe] [trace | trace=commands]`: softpipe rather than the
//! GPU, and what to log.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::os::fd::{AsRawFd, IntoRawFd};
use std::ptr::{NonNull, null_mut};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use std::collections::BTreeMap;

use guest_sys::idle;
use vabi::{Rights, map_flags, signals};
use vipc::{Bytes, WaitSet};
use vproto::gpu::{GpuError, ResourceSpec, Session, gpu};
use vrt::object::{Channel, Event, Vmo};
use vrt::vm::Mapping;

/// A session's memory: its header page (the fence word, first), the
/// command area, then the shared area.
const HEADER: usize = 4096;
const COMMANDS: usize = 1024 * 1024;
const MIN_SHARED: u64 = 1024 * 1024;
const MAX_SHARED: u64 = 64 * 1024 * 1024;
/// The most connections, of all processes, and of one.
const MAX_CLIENTS: usize = 64;
const PROCESS_CLIENTS: usize = 8;
/// The most bytes of contexts' memory of one process, and of all.
const PROCESS_MEMORY: u64 = 256 << 20;
const TOTAL_MEMORY: u64 = 1 << 30;
/// The most pieces of memory from outside (a display's pictures) a
/// process gives to render into, and their bytes; and the bytes of all.
const PROCESS_IMPORTS: usize = 16;
const PROCESS_IMPORT_BYTES: u64 = 512 << 20;
const TOTAL_IMPORT_BYTES: u64 = 1 << 30;
/// The most pieces of memory from outside a context may have softpipe
/// render into (mapped here until the context goes, as the decoder needs).
const MAX_IMPORTS: usize = 4;
/// How often signaling fences are looked for while some are pending.
const POLL_NS: u64 = 1_000_000;
/// How long a renderer started again tries for the service's name, which
/// the one before it holds until Veda sees it gone.
const REGISTER_FOR: Duration = Duration::from_secs(10);

/// The decoder's interface (`decoder/renderer.h`).
mod ffi {
    use std::ffi::{c_char, c_int, c_void};

    pub enum Device {}
    pub enum Context {}

    /// `struct vr_resource_args`.
    #[repr(C)]
    pub struct ResourceArgs {
        pub target: u32,
        pub format: u32,
        pub bind: u32,
        pub width: u32,
        pub height: u32,
        pub depth: u32,
        pub array_size: u32,
        pub last_level: u32,
        pub nr_samples: u32,
        pub flags: u32,
    }

    /// `enum vr_result`'s failures that are not the client's.
    pub const NO_MEMORY: c_int = -2;
    pub const LOST: c_int = -3;
    /// `enum vr_import`.
    pub const IMPORT_FD: c_int = 1;
    pub const IMPORT_MEMORY: c_int = 2;

    unsafe extern "C" {
        pub fn vr_device_create_softpipe() -> *mut Device;
        pub fn vr_device_create_drm(fd: c_int) -> *mut Device;
        pub fn vr_device_name(dev: *mut Device) -> *const c_char;
        pub fn vr_device_caps(dev: *mut Device, out: *mut c_void, len: usize) -> usize;
        pub fn vr_device_import(dev: *mut Device) -> c_int;
        pub fn vr_context_create(dev: *mut Device, shared: *mut u8, len: usize) -> *mut Context;
        pub fn vr_context_destroy(ctx: *mut Context);
        pub fn vr_context_error(ctx: *const Context) -> *const c_char;
        pub fn vr_resource_create(
            ctx: *mut Context,
            args: *const ResourceArgs,
            backing_offset: u64,
            backing_len: u64,
            id: *mut u32,
        ) -> c_int;
        pub fn vr_resource_destroy(ctx: *mut Context, id: u32);
        pub fn vr_resource_import(
            ctx: *mut Context,
            args: *const ResourceArgs,
            fd: c_int,
            memory: *mut c_void,
            stride: u32,
            id: *mut u32,
        ) -> c_int;
        pub fn vr_submit(ctx: *mut Context, words: *const u32, count: usize) -> c_int;
        pub fn vr_fence(ctx: *mut Context, seq: *mut u64) -> c_int;
        pub fn vr_fence_signaled(ctx: *mut Context) -> u64;
    }
}

/// The device every client renders on.
struct Device {
    dev: NonNull<ffi::Device>,
    name: String,
    /// Its capability set 2 (`struct virgl_caps_v2`), for clients.
    caps: Vec<u8>,
    /// How it takes memory from outside to render into.
    import: c_int,
}

/// A client's context, and the memory it shares with the client.
struct Context {
    ctx: NonNull<ffi::Context>,
    memory: Mapping,
    fences: Event,
    /// The last fence queued, and the last written to the fence word.
    queued: u64,
    signaled: u64,
    /// The memory from outside its resources render into, by resource:
    /// its bytes, and, for softpipe, its mapping here.
    imports: BTreeMap<u32, (u64, Option<Mapping>)>,
    /// softpipe's mappings of what its destroyed resources rendered into:
    /// kept while the context lasts (what it bound may still use them).
    retired: Vec<Mapping>,
}

impl Context {
    /// Why its last call failed, for the log.
    fn error(&self) -> String {
        // SAFETY: the decoder's message for a live context.
        unsafe { CStr::from_ptr(ffi::vr_context_error(self.ctx.as_ptr())) }.to_string_lossy().into_owned()
    }

    fn pending(&self) -> bool {
        self.signaled < self.queued
    }

    /// What it uses: its memory, the memory it renders into.
    fn usage(&self) -> Usage {
        let retired: u64 = self.retired.iter().map(|m| m.len() as u64).sum();
        Usage {
            clients: 1,
            memory: self.memory.len() as u64,
            imports: self.imports.len(),
            import_bytes: self.imports.values().map(|(bytes, _)| bytes).sum::<u64>() + retired,
        }
    }

    /// softpipe's mappings of memory from outside.
    fn mappings(&self) -> usize {
        self.imports.values().filter(|(_, m)| m.is_some()).count() + self.retired.len()
    }

    /// Writes the number of the last fence that signaled into the fence
    /// word, and tells the client when it has moved.
    fn publish(&mut self) {
        // SAFETY: a live context.
        let done = unsafe { ffi::vr_fence_signaled(self.ctx.as_ptr()) };
        if done <= self.signaled {
            return;
        }
        self.signaled = done;
        // SAFETY: the fence word is the mapping's first, which is page
        // aligned and outlives this.
        unsafe { (*self.memory.as_ptr().cast::<AtomicU64>()).store(done, Ordering::Release) };
        let _ = self.fences.signal();
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        // Its resources go before the memory they use (the fields).
        // SAFETY: a context the decoder made, destroyed once.
        unsafe { ffi::vr_context_destroy(self.ctx.as_ptr()) };
    }
}

struct Client {
    channel: Channel,
    context: Option<Context>,
    /// Its process (its id; 0 if the registry did not say).
    process: u64,
}

/// What clients use of the renderer: connections, their contexts' memory,
/// the memory they give to render into.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Usage {
    clients: usize,
    memory: u64,
    imports: usize,
    import_bytes: u64,
}

impl Usage {
    fn add(self, other: Usage) -> Usage {
        Usage {
            clients: self.clients + other.clients,
            memory: self.memory + other.memory,
            imports: self.imports + other.imports,
            import_bytes: self.import_bytes + other.import_bytes,
        }
    }
}

/// What the clients use but client `except`, of process `process` and of
/// all.
fn usage(clients: &[Client], except: usize, process: u64) -> (Usage, Usage) {
    let mut of_process = Usage::default();
    let mut of_all = Usage::default();
    for (_, c) in clients.iter().enumerate().filter(|(i, _)| *i != except) {
        let used = c.context.as_ref().map_or(Usage { clients: 1, ..Usage::default() }, Context::usage);
        of_all = of_all.add(used);
        if c.process == process {
            of_process = of_process.add(used);
        }
    }
    (of_process, of_all)
}

/// A client's requests, carried out on the device.
struct Serve<'a> {
    device: &'a Device,
    context: &'a mut Option<Context>,
    /// What the client's process's other clients use, and all the other
    /// clients.
    process: Usage,
    all: Usage,
    /// Where commands are copied before they are decoded.
    commands: &'a mut [u32],
    trace: bool,
}

impl Serve<'_> {
    fn context(&mut self) -> Result<&mut Context, GpuError> {
        self.context.as_mut().ok_or(GpuError::State)
    }
}

fn args(spec: &ResourceSpec) -> ffi::ResourceArgs {
    ffi::ResourceArgs {
        target: spec.target,
        format: spec.format,
        bind: spec.bind,
        width: spec.width,
        height: spec.height,
        depth: spec.depth,
        array_size: spec.array_size,
        last_level: spec.last_level,
        nr_samples: spec.nr_samples,
        flags: spec.flags,
    }
}

fn error_of(e: c_int) -> GpuError {
    match e {
        ffi::NO_MEMORY => GpuError::NoMemory,
        ffi::LOST => GpuError::Unavailable,
        _ => GpuError::Invalid,
    }
}

impl gpu::Server for Serve<'_> {
    fn open(&mut self, shared: u64) -> Result<Session, GpuError> {
        if self.context.is_some() {
            return Err(GpuError::State);
        }
        let shared = shared.clamp(MIN_SHARED, MAX_SHARED).next_multiple_of(4096) as usize;
        let size = HEADER + COMMANDS + shared;
        if self.process.memory + size as u64 > PROCESS_MEMORY || self.all.memory + size as u64 > TOTAL_MEMORY {
            return Err(GpuError::NoMemory);
        }
        let no_memory = |_| GpuError::NoMemory;
        let vmo = Vmo::create(size).map_err(no_memory)?;
        let rights = Rights::TRANSFER | Rights::READ | Rights::WRITE | Rights::MAP | Rights::GET_INFO;
        let memory_theirs = vmo.0.duplicate(Some(rights)).map_err(no_memory)?;
        let memory = Mapping::new(vmo, size, map_flags::READ | map_flags::WRITE).map_err(no_memory)?;
        let fences = Event::create().map_err(no_memory)?;
        let fences_theirs =
            fences.0.duplicate(Some(Rights::TRANSFER | Rights::WAIT | Rights::SIGNAL)).map_err(no_memory)?;
        // SAFETY: the shared area is the mapping's end, which outlives the
        // context (Context's fields drop after it).
        let ctx =
            unsafe { ffi::vr_context_create(self.device.dev.as_ptr(), memory.as_ptr().add(HEADER + COMMANDS), shared) };
        let ctx = NonNull::new(ctx).ok_or(GpuError::Unavailable)?;
        *self.context = Some(Context {
            ctx,
            memory,
            fences,
            queued: 0,
            signaled: 0,
            imports: BTreeMap::new(),
            retired: Vec::new(),
        });
        Ok(Session {
            caps: Bytes(self.device.caps.clone()),
            memory: Vmo::from_handle(memory_theirs),
            command_offset: HEADER as u64,
            command_size: COMMANDS as u64,
            shared_offset: (HEADER + COMMANDS) as u64,
            shared_size: shared as u64,
            fence_offset: 0,
            fences: Event::from_handle(fences_theirs),
            renderer: self.device.name.clone(),
        })
    }

    fn create(&mut self, spec: ResourceSpec, backing_offset: u64, backing_len: u64) -> Result<u32, GpuError> {
        let ctx = self.context()?;
        let mut id = 0;
        // SAFETY: a live context; the decoder checks the range.
        let e =
            unsafe { ffi::vr_resource_create(ctx.ctx.as_ptr(), &args(&spec), backing_offset, backing_len, &mut id) };
        if e != 0 {
            println!("renderer: a resource refused: {}", ctx.error());
            return Err(error_of(e));
        }
        Ok(id)
    }

    fn destroy(&mut self, resource: u32) {
        if let Ok(ctx) = self.context() {
            // SAFETY: a live context; the decoder checks the handle.
            unsafe { ffi::vr_resource_destroy(ctx.ctx.as_ptr(), resource) };
            // What it rendered into is the client's again; softpipe's
            // mapping of it stays while the context lasts (what the context
            // bound may still draw into it: the decoder's contract).
            if let Some((_, mapping)) = ctx.imports.remove(&resource) {
                ctx.retired.extend(mapping);
            }
        }
    }

    fn submit(&mut self, words: u32) -> Result<(), GpuError> {
        let words = words as usize;
        let trace = self.trace;
        let commands: *mut u32 = self.commands.as_mut_ptr();
        let ctx = self.context()?;
        if words * 4 > COMMANDS {
            return Err(GpuError::Invalid);
        }
        // Copied out first: the client may be changing them.
        // SAFETY: the command area lies in the mapping, and the copy holds
        // COMMANDS bytes.
        unsafe { std::ptr::copy_nonoverlapping(ctx.memory.as_ptr().add(HEADER).cast::<u32>(), commands, words) };
        // SAFETY: as just written.
        let words = unsafe { std::slice::from_raw_parts(commands, words) };
        if trace {
            println!("renderer: a submission of {} words: {:08x?}", words.len(), &words[..words.len().min(4)]);
        }
        // SAFETY: a live context, and the commands as copied.
        let e = unsafe { ffi::vr_submit(ctx.ctx.as_ptr(), words.as_ptr(), words.len()) };
        if e != 0 {
            println!("renderer: commands refused: {}", ctx.error());
            return Err(error_of(e));
        }
        Ok(())
    }

    fn fence(&mut self) -> Result<u64, GpuError> {
        let ctx = self.context()?;
        let mut seq = 0;
        // SAFETY: a live context.
        let e = unsafe { ffi::vr_fence(ctx.ctx.as_ptr(), &mut seq) };
        if e != 0 {
            return Err(error_of(e));
        }
        ctx.queued = seq;
        ctx.publish();
        Ok(seq)
    }

    fn import(&mut self, spec: ResourceSpec, stride: u32, memory: Vmo) -> Result<u32, GpuError> {
        let import = self.device.import;
        let (process, all) = (self.process, self.all);
        let ctx = self.context()?;
        let len = stride as u64 * spec.height as u64;
        if len == 0 || len > 1 << 30 {
            return Err(GpuError::Invalid);
        }
        let len = len.next_multiple_of(4096);
        let used = process.add(ctx.usage());
        if used.imports >= PROCESS_IMPORTS
            || used.import_bytes + len > PROCESS_IMPORT_BYTES
            || all.import_bytes + ctx.usage().import_bytes + len > TOTAL_IMPORT_BYTES
        {
            return Err(GpuError::NoMemory);
        }
        let mut id = 0;
        let (e, mapping) = match import {
            ffi::IMPORT_FD => {
                // The GPU's driver takes the memory as a dma-buf, which keeps
                // it mapped into the guest; what the driver keeps of it
                // outlives the descriptor and the handle.
                let fd = vrt::guest::dmabuf(memory.raw(), 0, len, true).map_err(|_| GpuError::NoMemory)?;
                // SAFETY: a live context, and a dma-buf of the memory.
                let e = unsafe {
                    ffi::vr_resource_import(ctx.ctx.as_ptr(), &args(&spec), fd.as_raw_fd(), null_mut(), stride, &mut id)
                };
                (e, None)
            }
            ffi::IMPORT_MEMORY if ctx.mappings() < MAX_IMPORTS => {
                // softpipe draws into it as this program maps it.
                let map = Mapping::new(memory, len as usize, map_flags::READ | map_flags::WRITE)
                    .map_err(|_| GpuError::NoMemory)?;
                let at = map.as_ptr().cast::<c_void>();
                // SAFETY: a live context, and memory mapped while the
                // context lasts (kept below, if the import is made).
                let e = unsafe { ffi::vr_resource_import(ctx.ctx.as_ptr(), &args(&spec), -1, at, stride, &mut id) };
                (e, Some(map))
            }
            ffi::IMPORT_MEMORY => return Err(GpuError::NoMemory),
            _ => return Err(GpuError::Unavailable),
        };
        if e != 0 {
            // Nothing of it is kept (a mapping made for it goes).
            println!("renderer: memory to render into refused: {}", ctx.error());
            return Err(error_of(e));
        }
        ctx.imports.insert(id, (len, mapping));
        Ok(id)
    }
}

/// The first render node, once Linux's GPU driver has made it; none after
/// a while (a display Linux drives without a GPU, such as QEMU's VGA, has
/// none).
fn render_node() -> Option<std::fs::File> {
    for _ in 0..50 {
        let mut nodes: Vec<_> = std::fs::read_dir("/dev/dri")
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.file_name().is_some_and(|n| n.as_encoded_bytes().starts_with(b"renderD")))
            .collect();
        nodes.sort();
        if let Some(path) = nodes.first() {
            return std::fs::File::options()
                .read(true)
                .write(true)
                .open(path)
                .map_err(|e| println!("renderer: {}: {e}", path.display()))
                .ok();
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    println!("renderer: no GPU to render on (no render node)");
    None
}

/// The device to render on: softpipe, or the GPU of the first render node.
fn open_device(softpipe: bool) -> Option<Device> {
    let dev = if softpipe {
        // SAFETY: no arguments.
        unsafe { ffi::vr_device_create_softpipe() }
    } else {
        // The device keeps the descriptor.
        let fd = render_node()?.into_raw_fd();
        // SAFETY: a render node's descriptor, which the device takes.
        unsafe { ffi::vr_device_create_drm(fd) }
    };
    let Some(dev) = NonNull::new(dev) else {
        println!("renderer: no Gallium driver of Mesa's for this GPU, or it did not start");
        return None;
    };
    // SAFETY: a live device, and a buffer of the length given.
    let (name, caps, import) = unsafe {
        let name = CStr::from_ptr(ffi::vr_device_name(dev.as_ptr())).to_string_lossy().into_owned();
        let mut caps = vec![0u8; 4096];
        let len = ffi::vr_device_caps(dev.as_ptr(), caps.as_mut_ptr().cast(), caps.len());
        caps.truncate(len);
        (name, caps, ffi::vr_device_import(dev.as_ptr()))
    };
    Some(Device { dev, name, caps, import })
}

/// Carries out what client `i` has sent; false if it is to be dropped.
fn read_client(device: &Device, clients: &mut [Client], i: usize, commands: &mut [u32], trace: bool) -> bool {
    loop {
        let msg = match clients[i].channel.read() {
            Ok(msg) => msg,
            Err(vabi::Error::ShouldWait) => return true,
            Err(_) => return false,
        };
        let (process, all) = usage(clients, i, clients[i].process);
        let client = &mut clients[i];
        let mut serve = Serve { device, context: &mut client.context, process, all, commands, trace };
        match gpu::dispatch(&mut serve, msg) {
            Ok(reply) => {
                if reply.send(&client.channel).is_err() {
                    return false;
                }
            }
            Err(e) => {
                println!("renderer: a bad request: {e}");
                return false;
            }
        }
    }
}

/// Serves `gpu` on `device` for ever.
fn serve(device: &Device, trace: bool) -> ! {
    let since = std::time::Instant::now();
    let listener = loop {
        match vproto::register(gpu::NAME) {
            Ok(l) => break l,
            Err(_) if since.elapsed() < REGISTER_FOR => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => {
                println!("renderer: cannot provide the gpu service: {e:?}");
                idle();
            }
        }
    };
    println!("renderer: serving gpu on {}", device.name);
    let mut commands = vec![0u32; COMMANDS / 4];
    let mut clients: Vec<Client> = Vec::new();
    // Whether a wait was refused (said once).
    let mut refused = false;
    const LISTENER: u64 = u64::MAX;
    loop {
        let mut ws = WaitSet::new();
        ws.add(listener.raw(), signals::READABLE, LISTENER);
        for (i, c) in clients.iter().enumerate() {
            ws.add(c.channel.raw(), signals::READABLE | signals::PEER_CLOSED, i as u64);
        }
        let pending = clients.iter().any(|c| c.context.as_ref().is_some_and(Context::pending));
        let deadline = if pending { vrt::time::now_ns() + POLL_NS } else { vabi::DEADLINE_INFINITE };
        let ready = match ws.wait(deadline) {
            Ok(ready) => ready,
            // The monitor did not keep the wait (at its limits): not again
            // at once.
            Err(e) => {
                if !refused {
                    println!("renderer: cannot wait ({e}); waiting a little");
                    refused = true;
                }
                std::thread::sleep(Duration::from_millis(100));
                Vec::new()
            }
        };
        for ctx in clients.iter_mut().filter_map(|c| c.context.as_mut()) {
            if ctx.pending() {
                ctx.publish();
            }
        }
        // Clients, last to first (dropping one moves the last into its
        // place), then new connections. What a closing client sent before
        // it closed is carried out.
        let mut ready_clients: Vec<(usize, u32)> =
            ready.iter().filter(|(k, _)| *k != LISTENER).map(|&(k, o)| (k as usize, o)).collect();
        ready_clients.sort_by_key(|&(i, _)| std::cmp::Reverse(i));
        for (i, observed) in ready_clients {
            let keep = observed & signals::READABLE == 0 || read_client(device, &mut clients, i, &mut commands, trace);
            if !keep || observed & signals::PEER_CLOSED != 0 {
                clients.swap_remove(i);
            }
        }
        if ready.iter().any(|&(k, _)| k == LISTENER) {
            // Within the most connections, of all and of the process.
            while let Some((channel, who)) = vproto::accept_with_identity(&listener) {
                let its = clients.iter().filter(|c| c.process == who.koid).count();
                if clients.len() < MAX_CLIENTS && its < PROCESS_CLIENTS {
                    clients.push(Client { channel, context: None, process: who.koid });
                } else {
                    println!("renderer: {} (process {}) has the most connections; refused one", who.name, who.koid);
                }
            }
        }
    }
}

/// The program, which `main` (`decoder/main.c`) runs.
///
/// # Safety
/// `argv` holds `argc` C strings, as the C library gives `main`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn renderer_main(argc: c_int, argv: *const *const c_char) -> c_int {
    // SAFETY: as the caller promises.
    let args: Vec<String> = (1..argc.max(1) as usize)
        .map(|i| unsafe { CStr::from_ptr(*argv.add(i)) }.to_string_lossy().into_owned())
        .collect();
    if args.iter().any(|a| a == "trace=commands") {
        // SAFETY: before any thread starts.
        unsafe { std::env::set_var("VR_TRACE", "1") };
    }
    let trace = args.iter().any(|a| a.starts_with("trace"));
    match open_device(args.iter().any(|a| a == "softpipe")) {
        Some(device) => serve(&device, trace),
        None => idle(),
    }
}
