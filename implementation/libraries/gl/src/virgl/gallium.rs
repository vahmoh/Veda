// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Veda's renderer on the host, for tests (`GL_TEST_BACKEND=gallium`):
//! the decoder that carries out virgl command streams on a Gallium driver
//! (`implementation/guests/linux/renderer`), built with Mesa as
//! `vgallium.so` around softpipe, Mesa's reference rasterizer. In the
//! driver VM the same decoder runs the GPU for the `gpu` service's clients;
//! here the tests call it directly, a device and context of its own for
//! each of theirs.
//!
//! `GL_GALLIUM_DLL` names the library; by default it is where Mesa's build
//! for this machine leaves it (`cargo automation linux` configures that
//! build, `cargo automation test` brings the library up to date). With
//! `GL_GALLIUM_DEVICE=virgl` the decoder renders on virgl, over
//! virglrenderer's test server (`virgl_test_server`, at `VTEST_SOCKET_NAME`),
//! as the driver VM's renderer does under QEMU, and with
//! `GL_GALLIUM_DEVICE=iris` on this machine's Intel GPU (iris, through its
//! render node), as the renderer does on a PC's; on softpipe otherwise.
//! Fences are waited for as the renderer waits for them, on the file of the
//! oldest pending one (a sync_file of the GPU's driver), where the device
//! gives one.
//!
//! Memory from outside reaches softpipe at its address, as the renderer
//! maps it, and iris as a dma-buf of a memory file (`udmabuf`), as the
//! driver VM's bridge makes Veda's memory one (`External::file`).

use std::boxed::Box;
use std::ffi::{CStr, c_char, c_int, c_short, c_ulong, c_void};
use std::ops::Range;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::string::String;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use std::vec;
use std::vec::Vec;

use super::host::{load, sym};
use crate::backend::{External, ImportError, Memory, OutOfMemory};
use crate::virgl::transport::HostMemory;
use crate::virgl::{Lost, ResourceArgs, Transport};

type Device = *mut c_void;
type Ctx = *mut c_void;

/// How the library makes a device: on its own (softpipe, virgl over
/// vtest), or on the GPU of a render node it is given (an open one).
#[derive(Clone, Copy)]
enum Make {
    Plain(unsafe extern "C" fn() -> Device),
    Drm(unsafe extern "C" fn(c_int) -> Device, c_int),
}

/// The library's functions
/// (`implementation/guests/linux/renderer/decoder/renderer.h`).
struct Lib {
    make: Make,
    device_destroy: unsafe extern "C" fn(Device),
    device_caps: unsafe extern "C" fn(Device, *mut c_void, usize) -> usize,
    device_import: unsafe extern "C" fn(Device) -> c_int,
    device_imports: unsafe extern "C" fn(Device) -> u32,
    context_create: unsafe extern "C" fn(Device, *mut u8, usize) -> Ctx,
    context_destroy: unsafe extern "C" fn(Ctx),
    context_error: unsafe extern "C" fn(Ctx) -> *const c_char,
    resource_create: unsafe extern "C" fn(Ctx, *const ResourceArgs, u64, u64, *mut u32) -> c_int,
    resource_import: unsafe extern "C" fn(Ctx, *const ResourceArgs, c_int, *mut c_void, u32, *mut u32) -> c_int,
    resource_destroy: unsafe extern "C" fn(Ctx, u32),
    submit: unsafe extern "C" fn(Ctx, *const u32, usize) -> c_int,
    fence: unsafe extern "C" fn(Ctx, *mut u64) -> c_int,
    fence_signaled: unsafe extern "C" fn(Ctx) -> u64,
    fence_fd: unsafe extern "C" fn(Ctx, *mut u64) -> c_int,
    fence_wait: unsafe extern "C" fn(Ctx, u64, u64) -> c_int,
}

// SAFETY: plain function pointers.
unsafe impl Send for Lib {}
unsafe impl Sync for Lib {}

fn dll_path() -> String {
    std::env::var("GL_GALLIUM_DLL").unwrap_or_else(|_| {
        String::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../target/linux/build/mesa-host/src/gallium/targets/veda/vgallium.so"
        ))
    })
}

/// This machine's Intel GPU's render node, open (for as long as the tests
/// run): the node whose Linux driver is i915 or xe.
fn intel_render_node() -> Option<c_int> {
    let mut nodes: Vec<String> = std::fs::read_dir("/sys/class/drm")
        .ok()?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("renderD"))
        .collect();
    nodes.sort();
    let node = nodes.into_iter().find(|n| {
        std::fs::read_link(std::format!("/sys/class/drm/{n}/device/driver"))
            .is_ok_and(|d| d.file_name().is_some_and(|f| f == "i915" || f == "xe"))
    })?;
    let file = std::fs::OpenOptions::new().read(true).write(true).open(std::format!("/dev/dri/{node}")).ok()?;
    Some(std::os::fd::IntoRawFd::into_raw_fd(file))
}

/// The library, loaded once; `None` if it is not there (or the device the
/// tests ask for is not).
fn lib() -> Option<&'static Lib> {
    static LIB: OnceLock<Option<Lib>> = OnceLock::new();
    LIB.get_or_init(|| {
        // SAFETY: loading the library and taking its functions as
        // renderer.h declares them.
        unsafe {
            let m = load(&dll_path());
            if m.is_null() {
                return None;
            }
            let make = match std::env::var("GL_GALLIUM_DEVICE").as_deref() {
                Ok("virgl") => Make::Plain(sym(m, "vr_device_create_vtest")),
                Ok("iris") => Make::Drm(sym(m, "vr_device_create_drm"), intel_render_node()?),
                _ => Make::Plain(sym(m, "vr_device_create_softpipe")),
            };
            Some(Lib {
                make,
                device_destroy: sym(m, "vr_device_destroy"),
                device_caps: sym(m, "vr_device_caps"),
                device_import: sym(m, "vr_device_import"),
                device_imports: sym(m, "vr_device_imports"),
                context_create: sym(m, "vr_context_create"),
                context_destroy: sym(m, "vr_context_destroy"),
                context_error: sym(m, "vr_context_error"),
                resource_create: sym(m, "vr_resource_create"),
                resource_import: sym(m, "vr_resource_import"),
                resource_destroy: sym(m, "vr_resource_destroy"),
                submit: sym(m, "vr_submit"),
                fence: sym(m, "vr_fence"),
                fence_signaled: sym(m, "vr_fence_signaled"),
                fence_fd: sym(m, "vr_fence_fd"),
                fence_wait: sym(m, "vr_fence_wait"),
            })
        }
    })
    .as_ref()
}

/// What `vr_fence_wait` returns when the fence has not signaled in time
/// (`VR_TIMEOUT`).
const TIMEOUT: c_int = -4;
/// `enum vr_import`: memory from outside as a dma-buf (a GPU's driver).
const IMPORT_FD: c_int = 1;

// The C library's, for Linux's dma-bufs of memory files and for waiting on
// fences' files.
unsafe extern "C" {
    fn open(path: *const c_char, flags: c_int, ...) -> c_int;
    fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
    fn close(fd: c_int) -> c_int;
    fn poll(fds: *mut PollFd, count: c_ulong, timeout_ms: c_int) -> c_int;
}

/// `struct udmabuf_create` (`<linux/udmabuf.h>`).
#[repr(C)]
struct UdmabufCreate {
    memfd: u32,
    flags: u32,
    offset: u64,
    size: u64,
}

/// `UDMABUF_CREATE`: `_IOW('u', 0x42, struct udmabuf_create)`.
const UDMABUF_CREATE: c_ulong = 0x4018_7542;
const UDMABUF_FLAGS_CLOEXEC: u32 = 1;
/// `O_RDWR | O_CLOEXEC`.
const OPEN_FLAGS: c_int = 2 | 0o2000000;

/// A dma-buf of the first `len` bytes (whole pages) of the memory file
/// `memfd`, which must be sealed against shrinking: a descriptor the caller
/// closes. As the driver VM's bridge gives its GPU Veda's memory.
fn udmabuf(memfd: c_int, len: usize) -> Option<c_int> {
    // SAFETY: a NUL-terminated path; the descriptor is closed below.
    let dev = unsafe { open(c"/dev/udmabuf".as_ptr(), OPEN_FLAGS) };
    if dev < 0 {
        std::eprintln!("vgallium: no /dev/udmabuf to give the GPU memory files");
        return None;
    }
    let req = UdmabufCreate { memfd: memfd as u32, flags: UDMABUF_FLAGS_CLOEXEC, offset: 0, size: len as u64 };
    // SAFETY: the request udmabuf's ioctl takes, alive for the call.
    let fd = unsafe { ioctl(dev, UDMABUF_CREATE, &req as *const UdmabufCreate) };
    // SAFETY: the descriptor opened above.
    unsafe { close(dev) };
    (fd >= 0).then_some(fd)
}

/// `struct pollfd`, for `poll`.
#[repr(C)]
struct PollFd {
    fd: c_int,
    events: c_short,
    revents: c_short,
}

const POLLIN: c_short = 1;

/// Shared memory: 16 MiB of staging and the query area, as virtio-gpu's.
const SHARED_BYTES: usize = 16 * 1024 * 1024 + 16 * 1024;

/// A context of Veda's renderer, on a device of its own.
pub struct GalliumTransport {
    lib: &'static Lib,
    dev: Device,
    ctx: Ctx,
    caps: Vec<u8>,
    /// The context's shared memory, which it keeps a pointer to: freed
    /// after `drop` has destroyed the context.
    shared: HostMemory,
}

// SAFETY: the renderer's device and context are only ever used by the
// transport's owner, one thread at a time, and keep nothing of the thread
// that made them (a Gallium context is no thread's own, as a GL context
// is).
unsafe impl Send for GalliumTransport {}

impl GalliumTransport {
    pub fn new() -> Option<GalliumTransport> {
        let lib = lib()?;
        let shared = HostMemory::new(SHARED_BYTES)?;
        // SAFETY: calls as renderer.h describes; the shared memory lives as
        // long as the context (`drop` destroys the context before the
        // memory goes).
        unsafe {
            let dev = match lib.make {
                Make::Plain(make) => make(),
                Make::Drm(make, fd) => make(fd),
            };
            if dev.is_null() {
                return None;
            }
            let mut caps = vec![0u8; 4096];
            let n = (lib.device_caps)(dev, caps.as_mut_ptr() as *mut c_void, caps.len());
            let ctx = if n > 0 { (lib.context_create)(dev, shared.ptr(), shared.len()) } else { core::ptr::null_mut() };
            if ctx.is_null() {
                (lib.device_destroy)(dev);
                return None;
            }
            caps.truncate(n);
            Some(GalliumTransport { lib, dev, ctx, caps, shared })
        }
    }

    /// Why the context's last call failed.
    fn error(&self) -> String {
        // SAFETY: a NUL-terminated message the context keeps.
        unsafe { CStr::from_ptr((self.lib.context_error)(self.ctx)) }.to_string_lossy().into_owned()
    }

    /// Runs commands as [`Transport::submit`] does, saying why the renderer
    /// refused them rather than ending the test: for tests of what it
    /// refuses.
    #[cfg(test)]
    pub fn try_submit(&mut self, commands: &[u32]) -> Result<(), String> {
        // SAFETY: a context of this transport; the renderer checks the
        // commands.
        match unsafe { (self.lib.submit)(self.ctx, commands.as_ptr(), commands.len()) } {
            0 => Ok(()),
            _ => Err(self.error()),
        }
    }
}

impl Transport for GalliumTransport {
    fn caps(&self) -> &[u8] {
        &self.caps
    }

    /// Veda's renderer clears every resource it creates.
    fn zeroes_new_resources(&self) -> bool {
        true
    }

    fn shared(&self) -> (*mut u8, usize) {
        (self.shared.ptr(), self.shared.len())
    }

    fn create_resource(&mut self, args: &ResourceArgs, backing: Option<Range<usize>>) -> Result<u32, OutOfMemory> {
        let (at, len) = backing.map_or((0, 0), |r| (r.start as u64, r.len() as u64));
        let mut id = 0;
        // SAFETY: a context of this transport; the renderer checks the rest.
        let r = unsafe { (self.lib.resource_create)(self.ctx, args, at, len, &mut id) };
        if r != 0 {
            std::eprintln!("vgallium: {} ({args:?})", self.error());
            return Err(OutOfMemory);
        }
        Ok(id)
    }

    fn destroy_resource(&mut self, handle: u32) {
        // SAFETY: as above.
        unsafe { (self.lib.resource_destroy)(self.ctx, handle) }
    }

    fn imports(&self) -> u32 {
        // SAFETY: the transport's device.
        unsafe { (self.lib.device_imports)(self.dev) }
    }

    fn import_resource(&mut self, args: &ResourceArgs, memory: &External) -> Result<u32, ImportError> {
        // SAFETY: the transport's device.
        let by_fd = unsafe { (self.lib.device_import)(self.dev) } == IMPORT_FD;
        let (fd, address) = match memory.memory() {
            Memory::Address(address) if !by_fd => (-1, address),
            Memory::File(file, address) if by_fd => {
                let len = memory.size().next_multiple_of(4096);
                (udmabuf(file, len).ok_or(ImportError::NoMemory)?, address)
            }
            Memory::File(_, address) => (-1, address),
            _ => return Err(ImportError::Unsupported),
        };
        let mut id = 0;
        // SAFETY: a context of this transport, which checks the rest; the
        // memory stays valid while the resource lives, as
        // `External::address` and `External::file` require of their makers
        // (softpipe draws into it), and a dma-buf of it is the driver's to
        // keep (it takes what it needs; the descriptor is closed below).
        let r =
            unsafe { (self.lib.resource_import)(self.ctx, args, fd, address as *mut c_void, memory.stride(), &mut id) };
        if fd >= 0 {
            // SAFETY: the dma-buf made above.
            unsafe { close(fd) };
        }
        if r != 0 {
            std::eprintln!("vgallium: {} ({args:?})", self.error());
            return Err(ImportError::Unsupported);
        }
        Ok(id)
    }

    fn max_submit_words(&self) -> usize {
        256 * 1024
    }

    fn submit(&mut self, commands: &[u32]) -> Result<(), Lost> {
        // SAFETY: as above.
        let r = unsafe { (self.lib.submit)(self.ctx, commands.as_ptr(), commands.len()) };
        if r != 0 {
            panic!("the renderer rejected the command stream ({r}): {}", self.error());
        }
        Ok(())
    }

    fn fence(&mut self) -> Result<u64, Lost> {
        let mut seq = 0;
        // SAFETY: as above.
        match unsafe { (self.lib.fence)(self.ctx, &mut seq) } {
            0 => Ok(seq),
            _ => Err(Lost),
        }
    }

    fn signaled(&mut self, fence: u64) -> bool {
        // SAFETY: as above.
        unsafe { (self.lib.fence_signaled)(self.ctx) >= fence }
    }

    fn wait(&mut self, fence: u64, timeout_ns: u64) -> Result<bool, Lost> {
        let until = Instant::now().checked_add(Duration::from_nanos(timeout_ns));
        loop {
            // SAFETY: a context of this transport.
            if unsafe { (self.lib.fence_signaled)(self.ctx) } >= fence {
                return Ok(true);
            }
            let left = until.map(|u| u.saturating_duration_since(Instant::now()));
            if left.is_some_and(|l| l.is_zero()) {
                return Ok(false);
            }
            let mut seq = 0;
            // SAFETY: as above.
            let fd = unsafe { (self.lib.fence_fd)(self.ctx, &mut seq) };
            if fd >= 0 {
                // SAFETY: a descriptor the library made for its caller.
                let file = unsafe { OwnedFd::from_raw_fd(fd) };
                // In milliseconds, rounded up: what is left of one is waited
                // for too.
                let ms = left.map_or(-1, |l| l.as_micros().div_ceil(1000).min(c_int::MAX as u128) as c_int);
                let mut entry = PollFd { fd: file.as_raw_fd(), events: POLLIN, revents: 0 };
                // SAFETY: the one entry given.
                if unsafe { poll(&mut entry, 1, ms) } >= 0 {
                    continue;
                }
            }
            // The device's own wait where it gives no files (virgl over
            // vtest) or `poll` failed; and for a fence never queued, which it
            // refuses.
            let ns = left.map_or(u64::MAX, |l| u64::try_from(l.as_nanos()).unwrap_or(u64::MAX));
            // SAFETY: a context of this transport.
            return match unsafe { (self.lib.fence_wait)(self.ctx, fence, ns) } {
                0 => Ok(true),
                TIMEOUT => Ok(false),
                _ => Err(Lost),
            };
        }
    }
}

impl Drop for GalliumTransport {
    fn drop(&mut self) {
        // SAFETY: the context and device made in `new`, the context first.
        unsafe {
            (self.lib.context_destroy)(self.ctx);
            (self.lib.device_destroy)(self.dev);
        }
    }
}

/// A context rendering through Veda's renderer on softpipe, if the library
/// has been built.
pub fn context(config: crate::Config) -> Option<crate::Context> {
    let t = GalliumTransport::new()?;
    let b = super::VirglBackend::new(Box::new(t)).ok()?;
    Some(crate::Context::new(Box::new(b), config))
}
