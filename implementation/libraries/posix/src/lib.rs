// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `vposix` — Veda's POSIX layer.
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
//! | `epoll`, `eventfd` | a port per epoll instance ([`epoll`]); a counter of the layer's with an event ([`eventfd`]) |
//! | `mmap` | the kernel's private memory, or a VMO per mapping ([`mem`]) |
//! | threads, TLS, futexes | kernel threads, the thread pointer, kernel futexes ([`thread`]) |
//! | `fork` + `exec` | `posix_spawn`, which loads the program directly ([`process`]) |
//! | signals | synchronous delivery (`raise`, `abort`, `SIGPIPE`, `pthread_kill`) ([`signal`]) |
//!
//! musl's hooks into this layer are:
//!
//! * `_start` stores the bootstrap channel the kernel passed (the entry
//!   point's first argument) in [`__veda_bootstrap`]; `__init_libc` then calls [`__veda_init`], which
//!   reads the startup message: the working directory, the descriptors the
//!   program was given (role `FD + n`), the terminal, the registry, and what
//!   it inherits that is no handle (`inherit`);
//! * `__veda_syscall(n, a1, ..., a6)` replaces the `syscall` instruction;
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
//! `arch` (`implementation/libraries/posix/src/arch/<architecture>/`).
//!
//! The layer is built as a static library for the architecture's bare-metal
//! target (`x86_64-unknown-none` on AMD64) and
//! linked into musl's `libc.a` as one object that exports only these
//! symbols. Its own memory comes from `vrt`'s heap, not from `malloc`.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

#[cfg(target_arch = "x86_64")]
#[path = "arch/amd64/mod.rs"]
mod arch;
mod carry;
mod epoll;
mod error;
mod eventfd;
mod fd;
mod file;
mod fs;
mod inet;
mod inherit;
mod io;
mod linux;
mod log;
mod mem;
mod native;
mod netd;
mod path;
mod process;
mod resolve;
mod signal;
mod sockaddr;
mod socket;
mod stream;
mod syscall;
mod system;
mod thread;
mod time;
mod tty;
mod unix;
mod user;
mod vfs;

use core::sync::atomic::{AtomicU32, Ordering};

use vabi::startup::role;
use vrt::object::Vmo;

/// The bootstrap channel the kernel passed the process, stored by `_start`.
#[unsafe(no_mangle)]
pub static __veda_bootstrap: AtomicU32 = AtomicU32::new(0);

/// Sets the layer up from the startup message (called by `__init_libc`,
/// once the thread pointer is set, before any constructor runs).
#[unsafe(no_mangle)]
pub extern "C" fn __veda_init() {
    // Thread-local storage is set up: threads can be told apart.
    signal::threads_ready();
    vrt::env::adopt(__veda_bootstrap.swap(0, Ordering::Relaxed));
    let entries = vrt::env::vars();
    let inherited = inherit::Inherited::parse(entries.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    signal::inherit(inherited.ignored, inherited.mask);
    vfs::set_cwd(vrt::env::cwd());
    if let Some(h) = vrt::env::take_handle(role::TERMINAL) {
        tty::adopt(Vmo::from_handle(h));
    }
    io::adopt_startup_fds(&inherited);
}

/// A Linux system call. Returns its result, or `-errno`.
///
/// # Safety
/// The arguments are those of the Linux system call `n`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __veda_syscall(n: isize, a: isize, b: isize, c: isize, d: isize, e: isize, f: isize) -> isize {
    let args = [a, b, c, d, e, f].map(|v| v as usize);
    // SAFETY: per the caller.
    let r = unsafe { syscall::dispatch(n as usize, args) };
    // Signals another thread sent this one are taken as a system call
    // returns.
    signal::deliver_if_any();
    error::encode(r)
}

/// The layer's own memory (separate from `malloc`, which calls into the
/// layer for its memory).
#[cfg(target_os = "none")]
#[global_allocator]
static HEAP: vrt::heap::Heap = vrt::heap::Heap::new();

#[cfg(target_os = "none")]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    // Formatted without allocating, in case the heap is what failed.
    vrt::io::log_without_allocating(format_args!("POSIX layer panic: {info}\n"));
    process::exit_by_signal(linux::sig::ABRT)
}
