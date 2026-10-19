// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Memory from outside, drawn into and sampled in place, as a page's frames
//! go from WebKit to the window system without copies: WebKit's context
//! renders into the memory file the window system shows the page from
//! (`renderbuffer_storage_external`), and the window system's context
//! samples the same memory as a texture (`tex_storage_external`).
//!
//! What the renderer asks its host for is checked on a fake host, anywhere;
//! the whole path on Veda's renderer (`GL_TEST_BACKEND=gallium`): on
//! softpipe, and with `GL_GALLIUM_DEVICE=iris` on this machine's Intel GPU,
//! which takes the memory as a dma-buf of a memory file, as the driver VM's
//! bridge makes one of Veda's memory.

use std::ffi::{c_char, c_int, c_uint, c_void};
use std::println;
use std::vec::Vec;

use super::fbo::full;
use super::*;
use crate::backend::External;
use crate::virgl::protocol::{
    BIND_RENDER_TARGET, BIND_SAMPLER_VIEW, BIND_SCANOUT, CMD_CLEAR, CMD_CREATE_OBJECT, OBJ_SURFACE, TARGET_2D, format,
};
use crate::virgl::{IMPORT_DISPLAY, IMPORT_SHARED, fake};

/// Memory a fake host never touches, for its imports.
fn memory_for_fake(words: &mut [u32], stride: u32) -> External {
    // SAFETY: the fake host keeps resources of its own memory and never
    // reads or writes this.
    unsafe { External::address(words.as_mut_ptr().cast(), words.len() * 4, stride, true) }
}

#[test]
fn renderers_that_take_no_memory_from_outside_are_not_asked() {
    let (mut c, host) = fake::context(Config { width: 4, height: 4, ..Config::default() });
    let mut words = vec![0u32; 64];
    let memory = memory_for_fake(&mut words, 32);
    let rb = c.gen_renderbuffer();
    c.bind_renderbuffer(gl::RENDERBUFFER, rb);
    c.renderbuffer_storage_external(gl::RENDERBUFFER, gl::RGBA8, 8, 8, &memory);
    assert_eq!(c.get_error(), gl::INVALID_OPERATION);
    let t = c.gen_texture();
    c.bind_texture(gl::TEXTURE_2D, t);
    c.tex_storage_external(gl::TEXTURE_2D, gl::RGBA8, 8, 8, &memory);
    assert_eq!(c.get_error(), gl::INVALID_OPERATION);
    // A host that takes only a display's pictures is asked for none other,
    // nor one that takes only other pictures for a display's.
    host.borrow_mut().imports = IMPORT_DISPLAY;
    c.renderbuffer_storage_external(gl::RENDERBUFFER, gl::RGBA8, 8, 8, &memory);
    assert_eq!(c.get_error(), gl::INVALID_OPERATION);
    host.borrow_mut().imports = IMPORT_SHARED;
    c.renderbuffer_storage_external(gl::RENDERBUFFER, gl::RGB8, 8, 8, &memory.scanout());
    assert_eq!(c.get_error(), gl::INVALID_OPERATION);
    // Nor for a display's texture, which nothing would draw.
    host.borrow_mut().imports = IMPORT_DISPLAY | IMPORT_SHARED;
    c.tex_storage_external(gl::TEXTURE_2D, gl::RGBA8, 8, 8, &memory.scanout());
    assert_eq!(c.get_error(), gl::INVALID_OPERATION);
    assert!(host.borrow().imported.is_empty());
    // Memory too small for the picture, or rows too short, is no one's.
    c.renderbuffer_storage_external(gl::RENDERBUFFER, gl::RGBA8, 8, 9, &memory);
    assert_eq!(c.get_error(), gl::INVALID_VALUE);
    c.tex_storage_external(gl::TEXTURE_2D, gl::RGBA8, 9, 8, &memory);
    assert_eq!(c.get_error(), gl::INVALID_VALUE);
    assert!(host.borrow().imported.is_empty());
}

#[test]
fn memory_is_drawn_into_sampled_or_shown_as_its_host_is_asked() {
    let (mut c, host) = fake::context(Config { width: 4, height: 4, ..Config::default() });
    host.borrow_mut().imports = IMPORT_DISPLAY | IMPORT_SHARED;
    let mut words = vec![0u32; 12 * 6];
    let memory = memory_for_fake(&mut words, 48);
    // A page WebKit renders: drawn into (and read: copies, blits), in its
    // bytes' order (B, G, R, A), its alpha kept.
    let rb = c.gen_renderbuffer();
    c.bind_renderbuffer(gl::RENDERBUFFER, rb);
    c.renderbuffer_storage_external(gl::RENDERBUFFER, gl::RGBA8, 10, 6, &memory);
    no_error(&mut c);
    // The window system's view of it: sampled alone.
    let t = c.gen_texture();
    c.bind_texture(gl::TEXTURE_2D, t);
    c.tex_storage_external(gl::TEXTURE_2D, gl::RGBA8, 10, 6, &memory);
    no_error(&mut c);
    assert_eq!(c.get_tex_parameteri(gl::TEXTURE_2D, gl::TEXTURE_IMMUTABLE_FORMAT), 1);
    assert_eq!(c.get_tex_parameteri(gl::TEXTURE_2D, gl::TEXTURE_IMMUTABLE_LEVELS), 1);
    // A display's picture: drawn into, read in copies, shown.
    c.renderbuffer_storage_external(gl::RENDERBUFFER, gl::RGB8, 10, 6, &memory.scanout());
    no_error(&mut c);
    let imported: Vec<(u32, u32, u32, u32)> =
        host.borrow().imported.iter().map(|(a, stride)| (a.format, a.bind, a.target, *stride)).collect();
    assert_eq!(
        imported,
        [
            (format::B8G8R8A8_UNORM, BIND_RENDER_TARGET | BIND_SAMPLER_VIEW, TARGET_2D, 48),
            (format::B8G8R8A8_UNORM, BIND_SAMPLER_VIEW, TARGET_2D, 48),
            (format::B8G8R8X8_UNORM, BIND_RENDER_TARGET | BIND_SAMPLER_VIEW | BIND_SCANOUT, TARGET_2D, 48),
        ]
    );
    for (args, _) in &host.borrow().imported {
        assert_eq!((args.width, args.height, args.depth, args.array_size, args.last_level), (10, 6, 1, 1, 0));
    }
}

#[test]
fn memory_only_sampled_is_never_drawn_into_or_written() {
    let (mut c, host) = fake::context(Config { width: 4, height: 4, ..Config::default() });
    host.borrow_mut().imports = IMPORT_SHARED;
    let mut words = vec![0u32; 8 * 8];
    let memory = memory_for_fake(&mut words, 32);
    let t = c.gen_texture();
    c.bind_texture(gl::TEXTURE_2D, t);
    c.tex_storage_external(gl::TEXTURE_2D, gl::RGBA8, 8, 8, &memory);
    no_error(&mut c);
    c.finish();
    let (surfaces, clears) = {
        let h = host.borrow();
        (h.count(OBJ_SURFACE), h.sent(CMD_CLEAR))
    };
    // Its pixels are not the context's to change...
    c.tex_sub_image_2d(gl::TEXTURE_2D, 0, 0, 0, 1, 1, gl::RGBA, gl::UNSIGNED_BYTE, Pixels::Data(&[0u8; 4]));
    assert_eq!(c.get_error(), gl::INVALID_OPERATION);
    c.copy_tex_sub_image_2d(gl::TEXTURE_2D, 0, 0, 0, 0, 0, 1, 1);
    assert_eq!(c.get_error(), gl::INVALID_OPERATION);
    c.generate_mipmap(gl::TEXTURE_2D);
    assert_eq!(c.get_error(), gl::INVALID_OPERATION);
    // ... nor its storage to replace (it is immutable).
    c.tex_storage_2d(gl::TEXTURE_2D, 1, gl::RGBA8, 8, 8);
    assert_eq!(c.get_error(), gl::INVALID_OPERATION);
    // A framebuffer drawing into it is incomplete; read from, it is not.
    let f = c.gen_framebuffer();
    c.bind_framebuffer(gl::FRAMEBUFFER, f);
    c.framebuffer_texture_2d(gl::FRAMEBUFFER, gl::COLOR_ATTACHMENT0, gl::TEXTURE_2D, t, 0);
    no_error(&mut c);
    assert_eq!(c.check_framebuffer_status(gl::DRAW_FRAMEBUFFER), gl::FRAMEBUFFER_INCOMPLETE_ATTACHMENT);
    assert_eq!(c.check_framebuffer_status(gl::READ_FRAMEBUFFER), gl::FRAMEBUFFER_COMPLETE);
    c.clear(gl::COLOR_BUFFER_BIT);
    assert_eq!(c.get_error(), gl::INVALID_FRAMEBUFFER_OPERATION);
    let mut out = [0u8; 4];
    c.read_pixels(0, 0, 1, 1, gl::RGBA, gl::UNSIGNED_BYTE, &mut out[..]);
    no_error(&mut c);
    // Nothing went to the host to draw into it or clear it.
    c.finish();
    let h = host.borrow();
    assert_eq!((h.count(OBJ_SURFACE), h.sent(CMD_CLEAR)), (surfaces, clears));
}

// ---- On Veda's renderer -----------------------------------------------------

// The C library's, for memory files.
unsafe extern "C" {
    fn memfd_create(name: *const c_char, flags: c_uint) -> c_int;
    fn ftruncate(fd: c_int, len: i64) -> c_int;
    fn fcntl(fd: c_int, cmd: c_int, ...) -> c_int;
    fn mmap(addr: *mut c_void, len: usize, prot: c_int, flags: c_int, fd: c_int, offset: i64) -> *mut c_void;
    fn munmap(addr: *mut c_void, len: usize) -> c_int;
    fn close(fd: c_int) -> c_int;
}

/// A picture in a memory file, as WebKit's pages are: `w` x `h` pixels,
/// rows `stride` pixels apart, mapped here. It outlives the contexts using
/// it (declared before them, it goes after them).
struct Picture {
    fd: c_int,
    at: *mut u32,
    len: usize,
    w: u32,
    h: u32,
    stride: u32,
}

impl Picture {
    /// Every pixel `fill`.
    fn new(w: u32, h: u32, stride: u32, fill: u32) -> Picture {
        let len = (stride as usize * h as usize * 4).next_multiple_of(4096);
        // SAFETY: plain calls of the C library; the results are checked.
        unsafe {
            let fd = memfd_create(c"picture".as_ptr(), 1 | 2);
            assert!(fd >= 0, "memfd_create");
            assert_eq!(ftruncate(fd, len as i64), 0);
            // F_ADD_SEALS, F_SEAL_SHRINK: as a dma-buf of it needs.
            assert_eq!(fcntl(fd, 1033, 2), 0);
            let at = mmap(core::ptr::null_mut(), len, 1 | 2, 1, fd, 0);
            assert!(at as isize != -1, "mmap");
            let p = Picture { fd, at: at.cast(), len, w, h, stride };
            for y in 0..h {
                for x in 0..stride {
                    p.set(x, y, fill);
                }
            }
            p
        }
    }

    /// The memory, for a context (pixels `0xAARRGGBB`).
    fn external(&self) -> External {
        // SAFETY: the mapping and the file live as long as `self`, which
        // outlives the contexts given it; it is read and written here only
        // while no work of theirs on it is unfinished.
        unsafe { External::file(self.fd, self.at.cast(), self.len, self.stride * 4, true) }
    }

    fn get(&self, x: u32, y: u32) -> u32 {
        // SAFETY: a pixel of the mapping; read as a value, as a GPU may have
        // written it.
        unsafe { self.at.add((y * self.stride + x) as usize).read_volatile() }
    }

    fn set(&self, x: u32, y: u32, px: u32) {
        // SAFETY: as in `get`.
        unsafe { self.at.add((y * self.stride + x) as usize).write_volatile(px) }
    }
}

impl Drop for Picture {
    fn drop(&mut self) {
        // SAFETY: the mapping and file made in `new`, which nothing uses
        // any more.
        unsafe {
            munmap(self.at.cast(), self.len);
            close(self.fd);
        }
    }
}

/// A context on Veda's renderer, if the tests run on it, and whether it
/// takes pictures that processors read too; `None` (the test skipped)
/// otherwise.
fn renderer() -> Option<Context> {
    let gallium = std::env::var("GL_TEST_BACKEND").is_ok_and(|v| v == "gallium");
    let device = std::env::var("GL_GALLIUM_DEVICE").unwrap_or_default();
    if !gallium || device == "virgl" {
        println!("skipped: only Veda's renderer on softpipe or a GPU of its own takes memory from outside");
        return None;
    }
    // A GPU takes memory files as dma-bufs, which udmabuf makes of them.
    if device == "iris" && std::fs::OpenOptions::new().read(true).write(true).open("/dev/udmabuf").is_err() {
        println!("skipped: no /dev/udmabuf here to give the GPU memory files, as the driver VM gives it Veda's memory");
        return None;
    }
    virgl_context(Config { width: 1, height: 1, depth_bits: 0, stencil_bits: 0, ..Config::default() })
}

/// A framebuffer drawing into `picture`'s memory (as WebKit's frames).
fn draw_into(c: &mut Context, picture: &Picture) -> u32 {
    let rb = c.gen_renderbuffer();
    c.bind_renderbuffer(gl::RENDERBUFFER, rb);
    c.renderbuffer_storage_external(
        gl::RENDERBUFFER,
        gl::RGBA8,
        picture.w as i32,
        picture.h as i32,
        &picture.external(),
    );
    no_error(c);
    let f = c.gen_framebuffer();
    c.bind_framebuffer(gl::FRAMEBUFFER, f);
    c.framebuffer_renderbuffer(gl::FRAMEBUFFER, gl::COLOR_ATTACHMENT0, gl::RENDERBUFFER, rb);
    assert_eq!(c.check_framebuffer_status(gl::FRAMEBUFFER), gl::FRAMEBUFFER_COMPLETE);
    c.viewport(0, 0, picture.w as i32, picture.h as i32);
    f
}

/// Draws `picture`'s memory, sampled in place (as the window system draws
/// a page), into a framebuffer of the context's own of the same size, and
/// reads that back: RGBA bytes, row 0 the memory's first.
fn sample(c: &mut Context, picture: &Picture) -> Vec<u8> {
    let t = c.gen_texture();
    c.bind_texture(gl::TEXTURE_2D, t);
    c.tex_storage_external(gl::TEXTURE_2D, gl::RGBA8, picture.w as i32, picture.h as i32, &picture.external());
    no_error(c);
    c.tex_parameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::NEAREST as i32);
    c.tex_parameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::NEAREST as i32);
    let rb = c.gen_renderbuffer();
    c.bind_renderbuffer(gl::RENDERBUFFER, rb);
    c.renderbuffer_storage(gl::RENDERBUFFER, gl::RGBA8, picture.w as i32, picture.h as i32);
    let f = c.gen_framebuffer();
    c.bind_framebuffer(gl::FRAMEBUFFER, f);
    c.framebuffer_renderbuffer(gl::FRAMEBUFFER, gl::COLOR_ATTACHMENT0, gl::RENDERBUFFER, rb);
    c.viewport(0, 0, picture.w as i32, picture.h as i32);
    full(
        c,
        "#version 300 es\nprecision highp float; uniform sampler2D t; in vec2 uv; out vec4 o;\nvoid main() { o = texture(t, uv); }",
    );
    c.draw_arrays(gl::TRIANGLES, 0, 6);
    let mut out = vec![0u8; (picture.w * picture.h * 4) as usize];
    c.read_pixels(0, 0, picture.w as i32, picture.h as i32, gl::RGBA, gl::UNSIGNED_BYTE, &mut out[..]);
    no_error(c);
    c.delete_framebuffers(&[f]);
    c.delete_renderbuffers(&[rb]);
    c.delete_textures(&[t]);
    out
}

/// `0xAARRGGBB` as RGBA bytes.
fn rgba(px: u32) -> [u8; 4] {
    [(px >> 16) as u8, (px >> 8) as u8, px as u8, (px >> 24) as u8]
}

/// Whether two `0xAARRGGBB` pixels are the same, each channel within a GPU's
/// rounding.
fn near(a: u32, b: u32) -> bool {
    (0..4).all(|i| ((a >> (8 * i)) as u8).abs_diff((b >> (8 * i)) as u8) <= 1)
}

/// Clears `picture`'s framebuffer to `px`, and the rectangle `r` (x, y, w,
/// h) to `inner`, then waits for the GPU: a frame, as WebKit's
/// compositor draws one and waits before the window system shows it.
fn draw_frame(c: &mut Context, f: u32, px: u32, r: (i32, i32, i32, i32), inner: u32) {
    let ch = |p: u32, s: u32| ((p >> s) & 0xFF) as f32 / 255.0;
    c.bind_framebuffer(gl::FRAMEBUFFER, f);
    c.disable(gl::SCISSOR_TEST);
    c.clear_color(ch(px, 16), ch(px, 8), ch(px, 0), ch(px, 24));
    c.clear(gl::COLOR_BUFFER_BIT);
    c.enable(gl::SCISSOR_TEST);
    c.scissor(r.0, r.1, r.2, r.3);
    c.clear_color(ch(inner, 16), ch(inner, 8), ch(inner, 0), ch(inner, 24));
    c.clear(gl::COLOR_BUFFER_BIT);
    c.disable(gl::SCISSOR_TEST);
    c.finish();
    no_error(c);
}

#[test]
fn a_page_drawn_into_its_memory_is_what_the_processor_reads() {
    // Rows of 20 pixels, 24 apart: the memory outlives the context.
    let picture = Picture::new(20, 8, 24, 0xDEAD_BEEF);
    let Some(mut w) = renderer() else { return };
    let f = draw_into(&mut w, &picture);
    // Translucent where a page's background would be, opaque in a box.
    let (outer, inner) = (0x8040_2010, 0xFF33_6699);
    draw_frame(&mut w, f, outer, (4, 2, 8, 3), inner);
    for y in 0..picture.h {
        for x in 0..picture.stride {
            let px = picture.get(x, y);
            if x >= picture.w {
                assert_eq!(px, 0xDEAD_BEEF, "({x}, {y}) is past the row");
                continue;
            }
            let want = if (4..12).contains(&x) && (2..5).contains(&y) { inner } else { outer };
            assert!(near(px, want), "({x}, {y}): {px:#010x}, not {want:#010x}");
        }
    }
}

#[test]
fn memory_is_sampled_as_the_processor_last_wrote_it() {
    // A page painted on the processor, which the window system samples.
    let picture = Picture::new(16, 8, 16, 0);
    for y in 0..picture.h {
        for x in 0..picture.w {
            picture.set(x, y, 0xFF00_0000 | (x * 16) << 16 | (y * 32) << 8 | 0x40);
        }
    }
    let Some(mut c) = renderer() else { return };
    let img = sample(&mut c, &picture);
    for (x, y) in [(0, 0), (15, 0), (3, 5), (15, 7)] {
        assert_close(&px(&img, picture.w, x, y), &rgba(picture.get(x, y)));
    }
    // Painted again: the next frame samples what is there now, not what a
    // cache kept.
    for y in 0..picture.h {
        for x in 0..picture.w {
            picture.set(x, y, 0xFF11_2233);
        }
    }
    let img = sample(&mut c, &picture);
    for (x, y) in [(0, 0), (15, 0), (3, 5), (15, 7)] {
        assert_close(&px(&img, picture.w, x, y), &rgba(0xFF11_2233));
    }
}

#[test]
fn a_page_reaches_the_compositor_without_copies() {
    let picture = Picture::new(32, 16, 40, 0);
    // WebKit's context and the window system's: two contexts, each with
    // its own view of the same memory (on the GPU, two dma-bufs of it).
    let Some(mut web) = renderer() else { return };
    let Some(mut window) = renderer() else { return };
    let f = draw_into(&mut web, &picture);
    for (i, (outer, inner)) in [(0xFFFF_FFFFu32, 0xFF20_40C0u32), (0xFF10_1010, 0xFFF0_8000)].into_iter().enumerate() {
        let r = (2 + 8 * i as i32, 3, 9, 5);
        draw_frame(&mut web, f, outer, r, inner);
        // The window system samples what WebKit drew: this frame, never
        // the one before.
        let img = sample(&mut window, &picture);
        for y in 0..picture.h {
            for x in 0..picture.w {
                let inside = (r.0..r.0 + r.2).contains(&(x as i32)) && (r.1..r.1 + r.3).contains(&(y as i32));
                let want = rgba(if inside { inner } else { outer });
                assert_close(&px(&img, picture.w, x, y), &want);
            }
        }
    }
}

#[test]
fn the_renderer_refuses_to_write_memory_it_only_samples() {
    use crate::virgl::Transport;
    use crate::virgl::gallium::GalliumTransport;
    if renderer().is_none() {
        return;
    }
    let picture = Picture::new(8, 8, 8, 0x1234_5678);
    let mut t = GalliumTransport::new().expect("Veda's renderer");
    assert_eq!(t.imports() & (IMPORT_DISPLAY | IMPORT_SHARED), IMPORT_DISPLAY | IMPORT_SHARED);
    let args = crate::virgl::ResourceArgs {
        target: TARGET_2D,
        format: format::B8G8R8A8_UNORM,
        bind: BIND_SAMPLER_VIEW,
        width: 8,
        height: 8,
        depth: 1,
        array_size: 1,
        ..Default::default()
    };
    let id = t.import_resource(&args, &picture.external()).expect("memory to sample");
    // A surface of it, to draw into: refused, and the context with it.
    let words = [(5 << 16) | (OBJ_SURFACE << 8) | CMD_CREATE_OBJECT, 1, id, format::B8G8R8A8_UNORM, 0, 0];
    let refused = t.try_submit(&words).expect_err("a surface of memory only sampled");
    assert!(refused.contains("only sampled"), "{refused}");
    // Bound for a display, it must be drawn into; for nothing, it is no
    // resource.
    let mut t = GalliumTransport::new().expect("Veda's renderer");
    for bind in [BIND_SAMPLER_VIEW | BIND_SCANOUT, 0] {
        assert!(t.import_resource(&crate::virgl::ResourceArgs { bind, ..args }, &picture.external()).is_err());
    }
    for y in 0..8 {
        for x in 0..8 {
            assert_eq!(picture.get(x, y), 0x1234_5678);
        }
    }
}

// ---- What a page's frames cost --------------------------------------------

/// The processor's time this process has used (all its threads: the
/// renderer's decoder and the GPU's driver run here in the host's tests).
fn cpu_ns() -> u64 {
    #[repr(C)]
    struct Timespec {
        sec: i64,
        nsec: i64,
    }
    unsafe extern "C" {
        fn clock_gettime(clock: c_int, ts: *mut Timespec) -> c_int;
    }
    let mut ts = Timespec { sec: 0, nsec: 0 };
    // SAFETY: CLOCK_PROCESS_CPUTIME_ID, and room for the time.
    unsafe { clock_gettime(2, &mut ts) };
    ts.sec as u64 * 1_000_000_000 + ts.nsec as u64
}

/// A page's frames, scrolling, as WebKit draws them and the window system
/// shows them, both ways, on Veda's renderer (`GL_TEST_BACKEND=gallium`,
/// meant for `GL_GALLIUM_DEVICE=iris`): read back into the memory file and
/// copied into the window system's texture (as under QEMU), or drawn into
/// the memory file and sampled there. Prints what a frame takes:
///
/// `GL_TEST_BACKEND=gallium GL_GALLIUM_DEVICE=iris cargo test -p gl --release bench_page_frames -- --ignored --nocapture`
#[test]
#[ignore]
fn bench_page_frames() {
    // The host's page that scrolls itself, at the size measured in QEMU:
    // 1600 x 834, its text column changing each frame (2.5 MB).
    let (w, h) = (1600i32, 834i32);
    let column = Rect { x: 400, y: 0, w: 760, h };
    if renderer().is_none() {
        return;
    }
    const FRAMES: u32 = 300;
    const WARM: u32 = 30;
    for in_place in [false, true] {
        let picture = Picture::new(w as u32, h as u32, w as u32, 0xFF00_0000);
        let Some(mut web) = renderer() else { return };
        let Some(mut window) = renderer() else { return };
        // The page's content: taller than the view, scrolled through.
        let content = page_content(&mut web, w, 2 * h);
        let target = if in_place { draw_into(&mut web, &picture) } else { own_target(&mut web, w, h) };
        let program = scroll_program(&mut web);
        // The window system's: the page's texture (in place, or a copy
        // brought up to date), drawn into a display's picture of its own.
        let display = own_target(&mut window, w, h);
        let texture = if in_place { sampled_texture(&mut window, &picture) } else { copy_texture(&mut window, w, h) };
        let draw = draw_program(&mut window);
        let mut scratch: Vec<u32> = Vec::new();
        let (mut cpu, mut wall, mut copied) = (0u64, 0u64, 0u64);
        for frame in 0..FRAMES + WARM {
            let (cpu0, wall0) = (cpu_ns(), std::time::Instant::now());
            // WebKit: draws the column scrolled on, and waits for the GPU.
            web.bind_framebuffer(gl::FRAMEBUFFER, target);
            web.use_program(program);
            web.bind_texture(gl::TEXTURE_2D, content);
            let at = web.get_uniform_location(program, "u_scroll");
            web.uniform1f(at, (frame % 100) as f32 * 8.0 / (2 * h) as f32);
            web.enable(gl::SCISSOR_TEST);
            web.scissor(column.x, column.y, column.w, column.h);
            web.viewport(0, 0, w, h);
            web.draw_arrays(gl::TRIANGLES, 0, 6);
            web.disable(gl::SCISSOR_TEST);
            web.finish();
            let mut bytes = 0;
            if !in_place {
                // Read back into the memory file, what changed alone.
                web.pixel_storei(gl::PACK_ROW_LENGTH, w);
                // SAFETY: the picture's mapping, which nothing else uses now.
                let memory = unsafe { core::slice::from_raw_parts_mut(picture.at.cast::<u8>(), picture.len) };
                let start = (column.y * w + column.x) as usize * 4;
                web.read_pixels(
                    column.x,
                    column.y,
                    column.w,
                    column.h,
                    gl::BGRA_EXT,
                    gl::UNSIGNED_BYTE,
                    &mut memory[start..],
                );
                web.pixel_storei(gl::PACK_ROW_LENGTH, 0);
                bytes += (column.w * column.h * 4) as u64;
                // The window system: copied out of the client's memory, then
                // into its texture.
                scratch.clear();
                for y in column.y..column.y + column.h {
                    for x in column.x..column.x + column.w {
                        scratch.push(picture.get(x as u32, y as u32));
                    }
                }
                // SAFETY: the same pixels as bytes.
                let data = unsafe { core::slice::from_raw_parts(scratch.as_ptr().cast::<u8>(), scratch.len() * 4) };
                window.bind_texture(gl::TEXTURE_2D, texture);
                window.tex_sub_image_2d(
                    gl::TEXTURE_2D,
                    0,
                    column.x,
                    column.y,
                    column.w,
                    column.h,
                    gl::RGBA,
                    gl::UNSIGNED_BYTE,
                    Pixels::Data(data),
                );
                bytes += (column.w * column.h * 4) as u64;
            }
            // The window system draws what changed into its picture.
            window.bind_framebuffer(gl::FRAMEBUFFER, display);
            window.use_program(draw);
            window.bind_texture(gl::TEXTURE_2D, texture);
            window.enable(gl::SCISSOR_TEST);
            window.scissor(column.x, column.y, column.w, column.h);
            window.viewport(0, 0, w, h);
            window.draw_arrays(gl::TRIANGLES, 0, 6);
            window.disable(gl::SCISSOR_TEST);
            window.finish();
            no_error(&mut web);
            no_error(&mut window);
            if frame >= WARM {
                cpu += cpu_ns() - cpu0;
                wall += wall0.elapsed().as_nanos() as u64;
                copied += bytes;
            }
        }
        // The last frame shows the page as WebKit drew it.
        let img = {
            window.bind_framebuffer(gl::FRAMEBUFFER, display);
            let mut out = vec![0u8; (w * h * 4) as usize];
            window.read_pixels(0, 0, w, h, gl::RGBA, gl::UNSIGNED_BYTE, &mut out[..]);
            out
        };
        let (cx, cy) = (column.x as u32 + 10, 400);
        assert!(px(&img, w as u32, cx, cy)[3] == 255, "nothing drawn at ({cx}, {cy})");
        println!(
            "{}: {:.2} ms of the processor and {:.2} ms a frame; {} KB copied by the processor a frame",
            if in_place { "in place" } else { "read back and uploaded" },
            cpu as f64 / FRAMES as f64 / 1e6,
            wall as f64 / FRAMES as f64 / 1e6,
            copied / FRAMES as u64 / 1024,
        );
    }
}

/// A rectangle of a page, in its pixels (row 0 first).
#[derive(Clone, Copy)]
struct Rect {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

/// A `w` x `h` texture of a page's content: rows of text-like stripes.
fn page_content(c: &mut Context, w: i32, h: i32) -> u32 {
    let mut texels = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let ink = (y % 24 < 14) && ((x / 7 + y / 24) % 5 != 0);
            let v = if ink { 0x20 } else { 0xF8 };
            texels.extend_from_slice(&[v, v, v ^ (y as u8 & 0x1F), 255]);
        }
    }
    let t = c.gen_texture();
    c.bind_texture(gl::TEXTURE_2D, t);
    c.tex_storage_2d(gl::TEXTURE_2D, 1, gl::RGBA8, w, h);
    c.tex_sub_image_2d(gl::TEXTURE_2D, 0, 0, 0, w, h, gl::RGBA, gl::UNSIGNED_BYTE, Pixels::Data(&texels));
    c.tex_parameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::NEAREST as i32);
    c.tex_parameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::NEAREST as i32);
    no_error(c);
    t
}

/// A framebuffer of the context's own (an RGBA8 renderbuffer).
fn own_target(c: &mut Context, w: i32, h: i32) -> u32 {
    let rb = c.gen_renderbuffer();
    c.bind_renderbuffer(gl::RENDERBUFFER, rb);
    c.renderbuffer_storage(gl::RENDERBUFFER, gl::RGBA8, w, h);
    let f = c.gen_framebuffer();
    c.bind_framebuffer(gl::FRAMEBUFFER, f);
    c.framebuffer_renderbuffer(gl::FRAMEBUFFER, gl::COLOR_ATTACHMENT0, gl::RENDERBUFFER, rb);
    assert_eq!(c.check_framebuffer_status(gl::FRAMEBUFFER), gl::FRAMEBUFFER_COMPLETE);
    f
}

/// The window system's texture of a page it copies (BGRA bytes in an RGBA
/// texture, read with red and blue swapped back, as `compositor` keeps it).
fn copy_texture(c: &mut Context, w: i32, h: i32) -> u32 {
    let t = c.gen_texture();
    c.bind_texture(gl::TEXTURE_2D, t);
    c.tex_storage_2d(gl::TEXTURE_2D, 1, gl::RGBA8, w, h);
    for (p, v) in [
        (gl::TEXTURE_MIN_FILTER, gl::NEAREST),
        (gl::TEXTURE_MAG_FILTER, gl::NEAREST),
        (gl::TEXTURE_SWIZZLE_R, gl::BLUE),
        (gl::TEXTURE_SWIZZLE_B, gl::RED),
    ] {
        c.tex_parameteri(gl::TEXTURE_2D, p, v as i32);
    }
    t
}

/// The window system's texture of a page's memory, sampled in place.
fn sampled_texture(c: &mut Context, picture: &Picture) -> u32 {
    let t = c.gen_texture();
    c.bind_texture(gl::TEXTURE_2D, t);
    c.tex_storage_external(gl::TEXTURE_2D, gl::RGBA8, picture.w as i32, picture.h as i32, &picture.external());
    no_error(c);
    c.tex_parameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::NEAREST as i32);
    c.tex_parameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::NEAREST as i32);
    t
}

/// WebKit's drawing of the page: its content, scrolled `u_scroll` (of the
/// content's height) on, over a full-view quad (attribute `pos`).
fn scroll_program(c: &mut Context) -> u32 {
    full(
        c,
        "#version 300 es\nprecision highp float; uniform sampler2D t; uniform float u_scroll; in vec2 uv; out vec4 o;\n\
         void main() { o = texture(t, vec2(uv.x, uv.y * 0.5 + u_scroll)); }",
    )
}

/// The window system's drawing of a page's texture, as it is.
fn draw_program(c: &mut Context) -> u32 {
    full(
        c,
        "#version 300 es\nprecision highp float; uniform sampler2D t; in vec2 uv; out vec4 o;\nvoid main() { o = texture(t, uv); }",
    )
}
