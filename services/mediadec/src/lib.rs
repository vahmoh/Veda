//! `mediadec` — the media decoder: FFmpeg and dav1d in a sandboxed process,
//! which Videos plays videos with (`vproto::media`).
//!
//! The program is C's world (a static ELF program on musl, over Veda's POSIX
//! layer), as FFmpeg needs; this crate is its Rust half, built for the
//! architecture's bare target (`<architecture>-unknown-none`) and linked
//! with the C shim (`shim/`, the only code calling FFmpeg) by Veda's GCC.
//! `main()` in the shim hands the program to [`mediadec_main`].
//!
//! The player starts it with one handle, the control channel
//! ([`vproto::media::CONTROL_ROLE`]), and no registry: it reaches no file,
//! service or network but what the player sends it. Its one thread answers
//! the player and does the work between requests (`session`); FFmpeg's and
//! dav1d's own threads decode. It ends when the player closes the channel.
//!
//! * [`source`]: what FFmpeg reads through (the file the player sent).
//! * [`info`]: what a source holds, as the protocol says it.
//! * [`schedule`]: packets queued ahead of their decoders.
//! * [`picture`]: pictures turned upright into the pool's slots.
//! * [`sound`]: the times of sound records.
//! * [`ffi`]: the shim's interface.
//!
//! Its Rust and the C library each keep their own: this half is made one
//! object that exports nothing but [`mediadec_main`] (as the POSIX layer is),
//! allocates with musl's `malloc`, and creates no threads of its own (a
//! thread of `vrt`'s would have no C thread state).

#![no_std]

extern crate alloc;

pub mod ffi;
pub mod info;
pub mod picture;
pub mod schedule;
#[cfg(target_os = "none")]
mod session;
pub mod sound;
pub mod source;

#[cfg(target_os = "none")]
mod program {
    use alloc::vec::Vec;
    use core::alloc::{GlobalAlloc, Layout};
    use core::ffi::c_void;

    use vabi::{RawHandle, WaitItem, signals};
    use vproto::media;
    use vrt::object::{Channel, Handle};
    use vrt::println;

    use crate::session::{MAX_ALLOC, Session};

    /// The longest it waits without beating (ns): the player takes a decoder
    /// whose heartbeat stops for some seconds as hung.
    const BEAT_NS: u64 = 1_000_000_000;

    unsafe extern "C" {
        fn malloc(size: usize) -> *mut c_void;
        fn calloc(count: usize, size: usize) -> *mut c_void;
        fn realloc(ptr: *mut c_void, size: usize) -> *mut c_void;
        fn free(ptr: *mut c_void);
        fn posix_memalign(out: *mut *mut c_void, align: usize, size: usize) -> i32;
        /// The POSIX layer's (`<veda/ipc.h>`).
        fn veda_startup_handle(role: u32, out: *mut RawHandle) -> i32;
    }

    /// musl's `malloc`, shared with FFmpeg: one heap, safe from any thread.
    struct Malloc;

    /// What musl's `malloc` aligns to (its unit, on every architecture).
    const MALLOC_ALIGN: usize = 16;

    // SAFETY: musl's allocator returns memory of at least the size asked,
    // aligned as asked (posix_memalign beyond its own alignment).
    unsafe impl GlobalAlloc for Malloc {
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            if l.align() <= MALLOC_ALIGN {
                // SAFETY: plain allocation.
                return unsafe { malloc(l.size().max(1)) as *mut u8 };
            }
            let mut p = core::ptr::null_mut();
            // SAFETY: the alignment is a power of two, a multiple of a pointer.
            if unsafe { posix_memalign(&mut p, l.align(), l.size().max(1)) } == 0 {
                p as *mut u8
            } else {
                core::ptr::null_mut()
            }
        }

        unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
            if l.align() <= MALLOC_ALIGN {
                // SAFETY: plain allocation.
                return unsafe { calloc(1, l.size().max(1)) as *mut u8 };
            }
            // SAFETY: as `alloc`; then the block is ours to clear.
            unsafe {
                let p = self.alloc(l);
                if !p.is_null() {
                    core::ptr::write_bytes(p, 0, l.size());
                }
                p
            }
        }

        unsafe fn dealloc(&self, p: *mut u8, _: Layout) {
            // SAFETY: allocated by `malloc` or `posix_memalign`.
            unsafe { free(p as *mut c_void) }
        }

        unsafe fn realloc(&self, p: *mut u8, l: Layout, size: usize) -> *mut u8 {
            if l.align() <= MALLOC_ALIGN {
                // SAFETY: a block of `malloc`'s.
                return unsafe { realloc(p as *mut c_void, size.max(1)) as *mut u8 };
            }
            // SAFETY: a new block, the old one's content, the old one freed.
            unsafe {
                let n = self.alloc(Layout::from_size_align_unchecked(size, l.align()));
                if !n.is_null() {
                    core::ptr::copy_nonoverlapping(p, n, l.size().min(size));
                    self.dealloc(p, l);
                }
                n
            }
        }
    }

    #[global_allocator]
    static HEAP: Malloc = Malloc;

    #[panic_handler]
    fn panic(info: &core::panic::PanicInfo) -> ! {
        // Formatted without allocating, in case the heap is what failed.
        struct Buf {
            data: [u8; 512],
            len: usize,
        }
        impl core::fmt::Write for Buf {
            fn write_str(&mut self, s: &str) -> core::fmt::Result {
                let n = s.len().min(self.data.len() - self.len);
                self.data[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
                self.len += n;
                Ok(())
            }
        }
        let mut b = Buf { data: [0; 512], len: 0 };
        let _ = core::fmt::write(&mut b, format_args!("panic: {info}\n"));
        vrt::sys::debug_write(&b.data[..b.len]);
        vrt::sys::process_exit(vabi::EXIT_CODE_PANICKED)
    }

    /// The program, from the shim's `main()`: answers the player and decodes
    /// until the player closes the control channel.
    #[unsafe(no_mangle)]
    pub extern "C" fn mediadec_main() -> i32 {
        // SAFETY: once, before FFmpeg is used.
        unsafe { crate::ffi::vmd_setup(MAX_ALLOC) };
        let mut raw = 0;
        // SAFETY: `raw` is writable.
        if unsafe { veda_startup_handle(media::CONTROL_ROLE, &mut raw) } != 0 {
            println!("started without a control channel");
            return 1;
        }
        // SAFETY: the handle was just taken from the startup message.
        let control = Channel::from_handle(unsafe { Handle::from_raw(raw) });
        // Before any input is read: no kernel log, no processes, no new
        // executable memory, whatever a damaged file makes of the decoder.
        if let Err(e) = vrt::process::restrict(vabi::restrict::ALL) {
            println!("cannot give up what it does without ({e:?})");
            return 1;
        }
        let mut session = Session::new(control.raw());
        loop {
            // Requests first: each waits at most for one call into FFmpeg.
            loop {
                match control.read() {
                    Ok(msg) => match media::media::dispatch(&mut session, msg) {
                        Ok(reply) => {
                            if reply.send(&control).is_err() {
                                return 0;
                            }
                        }
                        Err(e) => {
                            println!("a request it cannot read ({e:?}): stopping");
                            return 1;
                        }
                    },
                    Err(vabi::Error::ShouldWait) => break,
                    Err(_) => return 0,
                }
            }
            session.beat();
            if session.work() {
                continue;
            }
            let mut items: Vec<WaitItem> = Vec::with_capacity(3);
            items.push(WaitItem {
                handle: control.raw(),
                signals: signals::READABLE | signals::PEER_CLOSED,
                ..Default::default()
            });
            for wake in session.wakes() {
                items.push(WaitItem { handle: wake, signals: signals::SIGNALED, ..Default::default() });
            }
            // Idle, it still beats now and then: the player takes a silent
            // heartbeat for a hung decoder.
            let _ = vrt::object::wait_many(&mut items, vrt::time::now_ns() + BEAT_NS);
            if items[0].observed & signals::PEER_CLOSED != 0 && items[0].observed & signals::READABLE == 0 {
                return 0;
            }
            session.clear_wakes();
        }
    }
}
