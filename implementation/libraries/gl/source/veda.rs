// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Running in Veda (feature `veda`): contexts on the GPU through the `gpu`
//! service (Veda's renderer, in the driver VM), or with the software
//! renderer on every CPU.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;
use core::ops::Range;
use core::sync::atomic::{AtomicU64, Ordering};

use ipc::IpcError;
use proto::gpu::{GpuError, ResourceSpec, gpu};
use runtime::object::{Event, Vmo};
use runtime::pool::ThreadPool;
use runtime::vm::Mapping;

use crate::backend::{External, ImportError, Memory, OutOfMemory};
use crate::soft::{SoftBackend, Workers};
use crate::virgl::{Lost, ResourceArgs, Transport, Unsupported, VirglBackend};
use crate::{Config, Context};

/// Why a context cannot render on the GPU.
#[derive(Debug)]
pub enum NoGpu {
    /// The `gpu` service is not registered: there is no GPU to render on,
    /// or its renderer is not running (the driver VM is starting, or the
    /// renderer ended).
    NoService,
    /// The service cannot be reached from this process (its sandbox), or
    /// the registry is gone.
    Unreachable,
    /// The renderer took no context: the connection closed (the renderer
    /// has the most it takes, of all processes or of this one, or it went
    /// away), or it did not answer.
    NoAnswer(IpcError),
    /// The renderer refused a context (out of memory: at its limits).
    Refused(GpuError),
    /// The renderer gave a session against the protocol's rules (why): it
    /// is not used.
    BadSession(&'static str),
    /// The session's memory cannot be mapped here.
    NoMemory,
    /// The renderer (its name) cannot do what contexts need.
    Unsupported(String, Unsupported),
}

impl fmt::Display for NoGpu {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NoGpu::NoService => f.write_str("there is no GPU service (no GPU, or its renderer is not running)"),
            NoGpu::Unreachable => f.write_str("the GPU service cannot be reached from this process"),
            NoGpu::NoAnswer(IpcError::PeerClosed) => {
                f.write_str("the GPU service closed the connection (it is at its limits, or gone)")
            }
            NoGpu::NoAnswer(IpcError::Kernel(abi::Error::TimedOut)) => {
                f.write_str("the GPU service did not answer in time")
            }
            NoGpu::NoAnswer(e) => write!(f, "the GPU service did not answer ({e})"),
            NoGpu::Refused(e) => write!(f, "the GPU refused a context ({e})"),
            NoGpu::BadSession(why) => write!(f, "the GPU service gave a context {why}"),
            NoGpu::NoMemory => f.write_str("the memory of a context on the GPU cannot be mapped"),
            NoGpu::Unsupported(renderer, why) => write!(f, "cannot render on the GPU, {renderer}: {why}"),
        }
    }
}

impl Workers for ThreadPool {
    fn threads(&self) -> usize {
        ThreadPool::threads(self)
    }

    fn run(&self, f: &(dyn Fn(usize) + Sync)) {
        ThreadPool::run(self, f);
    }
}

/// A context rendering with the software renderer on a worker per CPU
/// (just below normal priority, so that the window system stays
/// responsive).
pub fn software_context(config: Config) -> Context {
    let pool = ThreadPool::named("gl-worker", runtime::pool::cpus().min(64), Some(abi::priority::NORMAL - 2));
    Context::new(Box::new(SoftBackend::new(Box::new(pool))), config)
}

/// A context on the GPU if there is one (the `gpu` service), with the
/// software renderer otherwise (saying why, when there is a GPU service).
pub fn context(config: Config) -> Context {
    gpu_context(config).unwrap_or_else(|e| {
        if !matches!(e, NoGpu::NoService) {
            runtime::println!("{e}: rendering in software");
        }
        software_context(config)
    })
}

/// A context on the GPU, or why there is none.
pub fn gpu_context(config: Config) -> Result<Context, NoGpu> {
    backend_context(GpuTransport::connect()?, config)
}

/// The GPU's renderer, if contexts can render on one now: the `gpu`
/// service is registered (this does not wait for it, even as the system
/// starts) and has what contexts need. For a program that would rather draw
/// on the CPU in its own way than with OpenGL ES in software.
pub fn gpu_renderer() -> Option<String> {
    let registered = proto::with_registry(|r| r.list()).ok()?.ok()?.iter().any(|s| s == gpu::NAME);
    if !registered {
        return None;
    }
    let t = GpuTransport::connect_to(gpu::NAME, CALL_NS).ok()?;
    let renderer = String::from(t.renderer());
    VirglBackend::new(Box::new(t)).ok().map(|_| renderer)
}

fn backend_context(t: GpuTransport, config: Config) -> Result<Context, NoGpu> {
    let renderer = String::from(t.renderer());
    match VirglBackend::new(Box::new(t)) {
        Ok(b) => {
            let mut c = Context::new(Box::new(b), config);
            // A framebuffer without the buffers asked for draws, but wrongly
            // (with no depth buffer every depth test passes): say so. (A
            // surfaceless context has none to make.)
            for (bits, pname, what) in [
                (config.depth_bits, crate::gl::DEPTH_BITS, "depth"),
                (config.stencil_bits, crate::gl::STENCIL_BITS, "stencil"),
            ] {
                if bits > 0 && !config.surfaceless && c.get_integer(pname) == 0 {
                    runtime::println!("{renderer}: the window has no {what} buffer: the renderer could not make one");
                }
            }
            Ok(c)
        }
        Err(e) => Err(NoGpu::Unsupported(renderer, e)),
    }
}

/// Shared memory to ask for: staging for a full-HD frame and then some,
/// and the query results.
const SHARED: u64 = 16 * 1024 * 1024 + 16 * 1024;
/// Programs started with the system may connect before the renderer has
/// registered: this long after boot, they wait for it a little.
const BOOT_NS: u64 = 60_000_000_000;
const BOOT_WAIT_NS: u64 = 5_000_000_000;
/// How long a call to the renderer may take before it counts as hung.
const CALL_NS: u64 = 10_000_000_000;

/// A virgl context through the `gpu` service.
pub struct GpuTransport {
    client: gpu::Client,
    caps: Vec<u8>,
    map: Arc<Mapping>,
    command: Range<usize>,
    shared: Range<usize>,
    fence_word: usize,
    fences: Arc<Event>,
    renderer: String,
    /// What memory from outside the renderer takes (`proto::gpu::IMPORT_*`).
    imports: u32,
}

impl GpuTransport {
    /// Opens a context through the GPU service (waiting a little for it as
    /// the system starts), or says why there is none.
    pub fn connect() -> Result<GpuTransport, NoGpu> {
        let services = proto::with_registry(|r| r.list()).map_err(|_| NoGpu::Unreachable)?;
        let registered = services.map_err(|_| NoGpu::Unreachable)?.iter().any(|s| s == gpu::NAME);
        let booting = runtime::time::now_ns() < BOOT_NS;
        if !registered && !booting {
            return Err(NoGpu::NoService);
        }
        GpuTransport::connect_to(gpu::NAME, if registered { CALL_NS } else { BOOT_WAIT_NS })
            // It did not come in the time it was given.
            .map_err(|e| if registered { e } else { NoGpu::NoService })
    }

    /// Opens a context through the service `name`, giving it `wait_ns` to
    /// answer (connections to a service not registered yet wait for it).
    ///
    /// The renderer is not trusted with this process's memory: a session
    /// whose areas break the protocol's rules (`proto::gpu::Session`), or
    /// whose memory is smaller than they say, is refused before anything of
    /// it is mapped.
    pub fn connect_to(name: &str, wait_ns: u64) -> Result<GpuTransport, NoGpu> {
        let client = gpu::Client::new(proto::connect(name).map_err(|_| NoGpu::Unreachable)?);
        client.set_timeout(wait_ns);
        let session = client.open(SHARED).map_err(NoGpu::NoAnswer)?.map_err(NoGpu::Refused)?;
        client.set_timeout(CALL_NS);
        let Some(layout) = session.areas().layout(SHARED) else {
            return Err(NoGpu::BadSession("whose memory is laid out against the protocol's rules"));
        };
        if session.memory.size().ok().is_none_or(|size| size < layout.size) {
            return Err(NoGpu::BadSession("with less memory than its areas take"));
        }
        let map = Mapping::new(session.memory, layout.size, abi::map_flags::READ | abi::map_flags::WRITE)
            .map_err(|_| NoGpu::NoMemory)?;
        Ok(GpuTransport {
            client,
            caps: session.caps.0,
            map: Arc::new(map),
            command: layout.command,
            shared: layout.shared,
            fence_word: layout.fence,
            fences: Arc::new(session.fences),
            renderer: session.renderer,
            imports: session.imports,
        })
    }

    /// The host's renderer.
    pub fn renderer(&self) -> &str {
        &self.renderer
    }

    /// How long a call may take before the context counts as lost (10 s
    /// unless set).
    pub fn set_call_timeout(&self, ns: u64) {
        self.client.set_timeout(ns);
    }

    /// A watch over this context's fences, for a program that waits for
    /// them among other things (it shares the context's event and memory).
    pub fn watch(&self) -> FenceWatch {
        FenceWatch { event: self.fences.clone(), map: self.map.clone(), word: self.fence_word }
    }

    /// The last fence that signaled.
    fn last_fence(&self) -> u64 {
        fence_word(&self.map, self.fence_word)
    }
}

/// The number in a session's fence word at `at`, which the renderer stores
/// atomically (with release ordering: what it wrote before, such as query
/// results, is visible once the number is).
fn fence_word(map: &Mapping, at: usize) -> u64 {
    // SAFETY: `connect_to` checked that the word lies inside the mapping
    // (which `map` keeps) at a multiple of 8 from its page-aligned start;
    // the memory is only ever accessed atomically as a whole word.
    unsafe { AtomicU64::from_ptr(map.as_ptr().add(at).cast::<u64>()) }.load(Ordering::Acquire)
}

impl Transport for GpuTransport {
    fn caps(&self) -> &[u8] {
        &self.caps
    }

    /// Veda's renderer clears every resource it creates.
    fn zeroes_new_resources(&self) -> bool {
        true
    }

    fn imports(&self) -> u32 {
        self.imports
    }

    fn shared(&self) -> (*mut u8, usize) {
        // SAFETY: `connect_to` checked that the shared area lies inside the
        // mapping.
        (unsafe { self.map.as_ptr().add(self.shared.start) }, self.shared.len())
    }

    fn create_resource(&mut self, args: &ResourceArgs, backing: Option<Range<usize>>) -> Result<u32, OutOfMemory> {
        let a = args;
        let spec = ResourceSpec {
            target: a.target,
            format: a.format,
            bind: a.bind,
            width: a.width,
            height: a.height,
            depth: a.depth,
            array_size: a.array_size,
            last_level: a.last_level,
            nr_samples: a.nr_samples,
            flags: a.flags,
        };
        let (off, len) = backing.map_or((0, 0), |r| (r.start as u64, r.len() as u64));
        match self.client.create(spec, off, len) {
            Ok(Ok(h)) => Ok(h),
            _ => Err(OutOfMemory),
        }
    }

    fn destroy_resource(&mut self, handle: u32) {
        let _ = self.client.destroy(handle);
    }

    fn import_resource(&mut self, args: &ResourceArgs, memory: &External) -> Result<u32, ImportError> {
        let a = args;
        let spec = ResourceSpec {
            target: a.target,
            format: a.format,
            bind: a.bind,
            width: a.width,
            height: a.height,
            depth: a.depth,
            array_size: a.array_size,
            last_level: a.last_level,
            nr_samples: a.nr_samples,
            flags: a.flags,
        };
        let Memory::Handle(handle) = memory.memory() else { return Err(ImportError::Unsupported) };
        // The caller keeps its handle: the renderer gets one of its own.
        let theirs = {
            // SAFETY: the caller's handle, borrowed for the duplication and
            // never closed here (`ManuallyDrop`).
            let h = core::mem::ManuallyDrop::new(unsafe { runtime::object::Handle::from_raw(handle) });
            Vmo::from_handle(h.duplicate(None).map_err(|_| ImportError::NoMemory)?)
        };
        match self.client.import(spec, memory.stride(), theirs) {
            Ok(Ok(h)) => Ok(h),
            Ok(Err(GpuError::NoMemory)) => Err(ImportError::NoMemory),
            Ok(Err(e)) => {
                runtime::println!("{}: cannot render into or sample memory it is given: {}", self.renderer, e);
                Err(ImportError::Unsupported)
            }
            Err(_) => Err(ImportError::NoMemory),
        }
    }

    fn max_submit_words(&self) -> usize {
        self.command.len() / 4
    }

    fn submit(&mut self, commands: &[u32]) -> Result<(), Lost> {
        let Some(bytes) = commands.len().checked_mul(4).filter(|&b| b <= self.command.len()) else {
            return Err(Lost);
        };
        // SAFETY: `bytes` fit in the command area, which `connect_to`
        // checked lies inside the mapping; the renderer reads it only
        // during the call (and copies it before it decodes it).
        unsafe {
            core::ptr::copy_nonoverlapping(
                commands.as_ptr() as *const u8,
                self.map.as_ptr().add(self.command.start),
                bytes,
            );
        }
        match self.client.submit(commands.len() as u32) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => lost(&self.renderer, format_args!("refused commands ({e})")),
            Err(e) => lost(&self.renderer, format_args!("did not answer ({e:?})")),
        }
    }

    fn fence(&mut self) -> Result<u64, Lost> {
        match self.client.fence() {
            Ok(Ok(f)) => Ok(f),
            Ok(Err(e)) => lost(&self.renderer, format_args!("refused a fence ({e})")),
            Err(e) => lost(&self.renderer, format_args!("did not answer ({e:?})")),
        }
    }

    fn signaled(&mut self, fence: u64) -> bool {
        self.last_fence() >= fence
    }

    fn wait(&mut self, fence: u64, timeout_ns: u64) -> Result<bool, Lost> {
        // A fence the renderer leaves unsignaled for longer than a call may
        // take, it never will: it has hung.
        let start = runtime::time::now_ns();
        let hung = start.saturating_add(CALL_NS);
        let until = start.saturating_add(timeout_ns).min(hung);
        loop {
            // Clear, then look: a fence signaling in between leaves the
            // event signaled again.
            let _ = self.fences.clear();
            if self.last_fence() >= fence {
                return Ok(true);
            }
            if runtime::time::now_ns() >= until {
                // The caller's time is up, or the renderer's.
                return if until < hung {
                    Ok(false)
                } else {
                    lost(&self.renderer, format_args!("left fence {fence} unsignaled"))
                };
            }
            let woken = self.fences.wait(abi::signals::SIGNALED, until);
            if woken.is_err() && runtime::time::now_ns() < until {
                // Not timed out: the event is gone, and the renderer with it.
                return lost(&self.renderer, format_args!("took its fences' event away"));
            }
        }
    }
}

/// A context's fences, as a program that waits for several things at once
/// watches them: the event the GPU's driver signals as fences signal (for
/// its wait set), and the number of the last that did.
pub struct FenceWatch {
    event: Arc<Event>,
    map: Arc<Mapping>,
    word: usize,
}

impl FenceWatch {
    /// The event: signaled whenever a fence signals.
    pub fn event(&self) -> abi::RawHandle {
        self.event.raw()
    }

    /// Clears the event; look at [`signaled`](Self::signaled) after.
    pub fn clear(&self) {
        let _ = self.event.clear();
    }

    /// The last fence that signaled.
    pub fn signaled(&self) -> u64 {
        fence_word(&self.map, self.word)
    }
}

/// Says why the GPU's context is lost (the renderer draws nothing from
/// then on).
fn lost<T>(renderer: &str, why: core::fmt::Arguments) -> Result<T, Lost> {
    runtime::println!("the GPU service ({renderer}) {why}: rendering stops");
    Err(Lost)
}
