// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `posix` — Veda's POSIX layer.
//!
//! C programs on Veda use musl, a C library written for Linux, with one
//! change: where musl would make a Linux system call, it calls
//! [`__veda_syscall`], which carries the call out with Veda's kernel objects
//! and services. The rest of musl — stdio, strings, math, locales, threads —
//! runs unchanged.
//!
//! | Linux | Veda |
//! |-------|------|
//! | files, directories | the VFS service: a `file` connection per open file ([`file`]) |
//! | pipes, `socketpair`, the terminal | socket endpoints ([`stream`], [`socket`], [`tty`]) |
//! | TCP and UDP sockets, names | the network service: a channel per socket ([`inet`], [`socket`]), its resolver ([`resolve`]) |
//! | Unix domain sockets | socket nodes of the VFS, which hand them connections and datagrams; socket endpoints and channels between the programs ([`unix`]) |
//! | advisory locks (`flock`, `fcntl`'s, `lockf`) | the VFS, which arbitrates them between processes ([`lock`]) |
//! | `epoll`, `eventfd` | a port per epoll instance ([`epoll`]); a counter of the layer's with an event ([`eventfd`]) |
//! | `timerfd` | a timer of the layer's with an event, which a thread of the layer's signals as it expires ([`timerfd`]) |
//! | `mmap` | the kernel's private memory, or a VMO per mapping ([`mem`]) |
//! | `memfd_create` | a VMO, shared by every process the file goes to ([`memfd`]) |
//! | threads, TLS, futexes | kernel threads, the thread pointer, kernel futexes ([`thread`]) |
//! | nice values, affinity | the kernel's thread priorities; every processor ([`sched`]) |
//! | `fork` + `exec` | `posix_spawn`, which loads the program directly ([`process`]) |
//! | signals | the layer's own state, handlers run as a call returns or where the kernel interrupts a thread ([`signal`], [`interrupt`]); another process's by way of the kernel |
//! | `alarm`, `setitimer`, `timer_create` | timers of the layer's, which a thread of its signals ([`timer`]) |
//! | a sandbox (Landlock, seccomp, namespaces) | the VFS connection and the registry channel confined, the kernel's `process_restrict` (`<veda/sandbox.h>`, [`sandbox`]) |
//!
//! musl's hooks into this layer are:
//!
//! * `_start` stores the bootstrap channel the kernel passed (the entry
//!   point's first argument) in [`__veda_bootstrap`] (in a dynamically
//!   linked program, the dynamic linker's `_dlstart` does, its own start
//!   leaving it); [`__veda_startup`] reads the startup message: the
//!   working directory, the descriptors the program was given (role
//!   `FD + n`), the terminal, the registry, and what it inherits that is no
//!   handle (`inherit`). `__init_libc` then calls [`__veda_init`], which
//!   reads it first if nothing has (a program linked statically), and sets
//!   up the signals; the dynamic linker reads it before it opens the
//!   program's libraries, when the main thread's thread pointer is not yet
//!   the one it keeps;
//! * `__veda_syscall(n, a1, ..., a6)` replaces the `syscall` instruction:
//!   the architecture's gate (`architecture::signal`), which keeps where
//!   its caller resumes, then [`syscall_gate`];
//! * `__clone`, `__set_thread_area` and `__unmapself` jump to
//!   [`thread::__veda_clone`], [`thread::__veda_set_thread_area`] and
//!   [`thread::__veda_unmapself`];
//! * `posix_spawn` calls [`process::__veda_spawn`];
//! * `__lookup_name` (`getaddrinfo`, `gethostbyname`) has the network
//!   service look names up ([`resolve::__veda_lookup_name`]), and
//!   `__get_resolv_conf` takes the DNS servers it uses
//!   ([`resolve::__veda_dns_servers`]).
//!
//! What is particular to the architecture (Linux's system call numbers and
//! the calls only it has, the thread pointer, where threads start) is in
//! `architecture`
//! (`implementation/libraries/posix/source/architectures/<architecture>/`).
//!
//! The layer is built as a static library for the architecture's bare-metal
//! target (`x86_64-unknown-none` on AMD64) and
//! linked into musl's `libc.a` as one object that exports only these
//! symbols. Its own memory comes from `rt`'s heap, not from `malloc`.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

#[cfg(target_arch = "x86_64")]
#[path = "architectures/amd64/mod.rs"]
mod architecture;
mod carry;
mod epoll;
mod error;
mod eventfd;
mod fd;
mod file;
mod fs;
mod inet;
mod inherit;
mod interrupt;
mod io;
mod linux;
mod lock;
mod log;
mod mem;
mod memfd;
mod native;
mod netd;
mod path;
mod process;
mod resolve;
mod sandbox;
mod sched;
mod signal;
mod sockaddr;
mod socket;
mod stream;
mod syscall;
mod system;
mod thread;
mod time;
mod timer;
mod timerfd;
mod tty;
mod unix;
mod user;
mod vfs;

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use abi::startup::role;
use rt::object::Vmo;
use rt::sync::Mutex;

/// The bootstrap channel the kernel passed the process, stored by `_start`
/// (by the dynamic linker's, in a dynamically linked program).
#[unsafe(no_mangle)]
pub static __veda_bootstrap: AtomicU32 = AtomicU32::new(0);

/// Whether [`__veda_startup`] has read the startup message.
static STARTED: AtomicBool = AtomicBool::new(false);

/// What the startup message says of the signals the program ignores and
/// blocks, and of its nice value, which [`__veda_init`] makes the main
/// thread's once it has the thread pointer it keeps.
static INHERITED: Mutex<(u64, u64, i32)> = Mutex::new((0, 0, 0));

/// Reads the startup message, once: the working directory, the descriptors
/// the program was given (role `FD + n`), the terminal, the registry, and
/// what it inherits that is no handle (`inherit`). Called by the dynamic
/// linker of a dynamically linked program before it opens the program's
/// libraries, and otherwise by [`__veda_init`]. Nothing here needs the
/// main thread's thread pointer, which the dynamic linker may change
/// before the program runs.
#[unsafe(no_mangle)]
pub extern "C" fn __veda_startup() {
    if STARTED.swap(true, Ordering::Relaxed) {
        return;
    }
    let _layer = interrupt::Layer::enter();
    rt::env::adopt(__veda_bootstrap.swap(0, Ordering::Relaxed));
    let entries = rt::env::vars();
    let inherited = inherit::Inherited::parse(entries.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    *INHERITED.lock() = (inherited.ignored, inherited.mask, inherited.nice);
    if let Some((top, size)) = inherited.main_stack {
        thread::set_main_stack(top, size);
    }
    vfs::set_cwd(rt::env::cwd());
    if let Some(h) = rt::env::take_handle(role::TERMINAL) {
        tty::adopt(Vmo::from_handle(h));
    }
    if let Some(h) = rt::env::take_handle(role::PARENT) {
        process::adopt_parent(rt::object::Process::from_handle(h));
    }
    io::adopt_startup_fds(&inherited);
}

/// Sets the layer up (called by `__init_libc`, once the thread pointer is
/// the one the main thread keeps, before any constructor runs): from the
/// startup message, which it reads unless the dynamic linker has
/// ([`__veda_startup`]), then the signals. Signals from other processes are
/// the program's from then on: until it has a signal entry, they end it,
/// or not, as they end a program by default.
#[unsafe(no_mangle)]
pub extern "C" fn __veda_init() {
    // Thread-local storage is set up: threads can be told apart, and each
    // has its word.
    #[cfg(target_os = "none")]
    {
        architecture::signal::init();
        architecture::signal::clear_word(!0);
    }
    signal::threads_ready();
    let _layer = interrupt::Layer::enter();
    thread::main_thread_ready();
    __veda_startup();
    let (ignored, mask, nice) = *INHERITED.lock();
    signal::inherit(ignored, mask);
    sched::inherit(nice);
    #[cfg(target_os = "none")]
    let _ = rt::sys::call(abi::nr::SIGNAL_ENTRY_SET, [architecture::signal::entry_address(), 0, 0, 0, 0, 0]);
}

/// A Linux system call (from `__veda_syscall`, which passes `caller`: where
/// it returns, and with what, `caller.rax`). As the call returns, the
/// calling thread acts on its ready signals (as a system call returns on
/// Linux), which may start a blocking call again (`signal::returning`).
///
/// # Safety
/// The arguments are those of the Linux system call `n`; `caller` is
/// `__veda_syscall`'s.
#[cfg_attr(not(target_os = "none"), allow(dead_code))]
unsafe extern "C" fn syscall_gate(
    n: isize,
    a: isize,
    b: isize,
    c: isize,
    d: isize,
    e: isize,
    f: isize,
    caller: *mut architecture::signal::Caller,
) {
    let args = [a, b, c, d, e, f].map(|v| v as usize);
    // SAFETY: per the caller.
    let (caller, call) = (unsafe { &mut *caller }, || unsafe { syscall::dispatch(n as usize, args) });
    if !interrupt::enter() {
        // No signal is acted on (thread-local storage is not set up yet).
        let r = call();
        caller.rax =
            error::encode(if r == Err(linux::errno::ERESTARTSYS) { Err(linux::errno::EINTR) } else { r }) as u64;
        return;
    }
    signal::returning(call(), caller, call);
    while !interrupt::leave() {
        signal::act_on_ready(caller);
    }
}

/// The layer's own memory (separate from `malloc`, which calls into the
/// layer for its memory).
#[cfg(target_os = "none")]
#[global_allocator]
static HEAP: rt::heap::Heap = rt::heap::Heap::new();

#[cfg(target_os = "none")]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    // Formatted without allocating, in case the heap is what failed.
    rt::io::log_without_allocating(format_args!("POSIX layer panic: {info}\n"));
    process::exit_by_signal(linux::sig::ABRT)
}
