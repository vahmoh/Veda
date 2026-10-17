// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `mediadec` — the media decoder: FFmpeg and dav1d in a sandboxed process,
//! which Videos plays videos with (`proto::media`).
//!
//! The program is C's world (a static ELF program on musl, over Veda's POSIX
//! layer), as FFmpeg needs; this crate is its Rust half, built for the
//! architecture's bare target (`<architecture>-unknown-none`) and linked
//! with the C shim (`shim/`, the only code calling FFmpeg) by Veda's GCC.
//! `main()` in the shim hands the program to [`mediadec_main`].
//!
//! The player starts it with one handle, the control channel
//! ([`proto::media::CONTROL_ROLE`]), and no registry: it reaches no file,
//! service or network but what the player sends it. Its one thread answers
//! the player and does the work between requests (`session`); FFmpeg's and
//! dav1d's own threads decode. It ends when the player closes the channel,
//! or goes away: a watchdog thread ends it then even while its own thread
//! is stuck in FFmpeg.
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
//! allocates with musl's `malloc`, and creates its one thread of its own,
//! the watchdog, with the C library's `pthread_create` (a thread of `rt`'s
//! would have no C thread state).

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
    use core::alloc::{GlobalAlloc, Layout};
    use core::ffi::c_void;

    use abi::RawHandle;
    use proto::media;
    use rt::object::{Channel, Handle};
    use rt::println;

    use crate::session::{MAX_ALLOC, Session};

    unsafe extern "C" {
        fn malloc(size: usize) -> *mut c_void;
        fn calloc(count: usize, size: usize) -> *mut c_void;
        fn realloc(ptr: *mut c_void, size: usize) -> *mut c_void;
        fn free(ptr: *mut c_void);
        fn posix_memalign(out: *mut *mut c_void, align: usize, size: usize) -> i32;
        /// The POSIX layer's (`<veda/ipc.h>`).
        fn veda_startup_handle(role: u32, out: *mut RawHandle) -> i32;
        /// musl's (`pthread_t` is an `unsigned long`).
        fn pthread_create(
            thread: *mut usize,
            attr: *const c_void,
            start: extern "C" fn(*mut c_void) -> *mut c_void,
            arg: *mut c_void,
        ) -> i32;
    }

    /// The watchdog's thread (`media::watchdog`): ends the decoder as soon
    /// as the player's end of the control channel (`control`, a raw handle)
    /// closes, whatever the decoder's own thread is doing (FFmpeg may be
    /// stuck in a damaged file). The player kills a decoder it gives up on
    /// itself; this ends one whose player crashed or was ended.
    extern "C" fn watchdog(control: *mut c_void) -> *mut c_void {
        media::watchdog(control as usize as RawHandle);
        core::ptr::null_mut()
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
        rt::io::log_without_allocating(format_args!("panic: {info}\n"));
        rt::sys::process_exit(abi::EXIT_CODE_PANICKED)
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
        let mut thread = 0usize;
        // SAFETY: the watchdog only waits on the handle, which stays open
        // while the decoder runs (it ends with it).
        if unsafe { pthread_create(&mut thread, core::ptr::null(), watchdog, raw as usize as *mut c_void) } != 0 {
            println!("cannot start its watchdog");
        }
        // Before any input is read: no kernel log, no processes, no new
        // executable memory, whatever a damaged file makes of the decoder.
        if let Err(e) = rt::process::restrict(abi::restrict::ALL) {
            println!("cannot give up what it does without ({e:?})");
            return 1;
        }
        let mut session = Session::new(control.raw());
        media::serve(&control, &mut session)
    }
}
