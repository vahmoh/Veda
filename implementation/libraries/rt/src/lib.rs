// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `vrt` — the Veda user-space runtime.
//!
//! Every Veda program links this crate. It provides:
//!
//! * the process entry point and startup protocol ([`entry!`], [`env`]),
//! * safe wrappers for kernel objects and system calls ([`object`], [`vm`]),
//! * the global heap ([`heap`]), threads ([`thread`]) and synchronisation
//!   ([`sync`]), clocks ([`time`]), logging ([`println!`]),
//! * process creation from ELF64 executables ([`process`]),
//! * the few C runtime symbols compiled code relies on.
//!
//! The runtime pieces that only make sense in a real Veda program (entry
//! point, allocator, panic handler, C symbols) are behind the default
//! `runtime` feature, and left out of the crate's own unit tests. Libraries
//! depend on `vrt` with `default-features = false`, which keeps them
//! unit-testable on the host.
//!
//! Programs of the driver VM's Linux guest are built with `--cfg
//! veda_guest` (their configuration sets it:
//! `implementation/guest/.cargo/config.toml`): their system calls then go
//! through the bridge (`guest`), and threads are Linux's (`std`'s).
//!
//! Programs on Rust's standard library, built for Veda's own target
//! (`target_os = "veda"`, on the C library), link `vrt` too, for Veda's
//! objects and services. The standard library is their runtime there (the
//! entry point, the panic handler, the C symbols) and their threads are its
//! own ([`thread`]); the C library's POSIX layer holds the handles the
//! process started with, which [`env`] asks it for. `vrt`'s heap stays the
//! global allocator (the `runtime` feature), so that a program allocates as
//! Veda's others do.

#![no_std]

extern crate alloc;
#[cfg(any(test, veda_guest, target_os = "veda"))]
extern crate std;

pub use alloc::{boxed, format, rc, string, vec};
pub use vabi;

#[cfg(target_arch = "x86_64")]
#[path = "arch/amd64/mod.rs"]
pub mod arch;
#[cfg(all(feature = "runtime", not(test), not(target_os = "veda")))]
mod crt;
pub mod env;
#[cfg(veda_guest)]
pub mod guest;
pub mod heap;
pub mod io;
pub mod object;
pub mod pool;
pub mod process;
#[cfg(all(feature = "runtime", not(test), not(target_os = "veda")))]
pub mod start;
pub mod sync;
pub mod sys;
pub mod thread;
pub mod time;
pub mod vm;

pub use object::{
    Channel, Event, Guest, Handle, Interrupt, IoPorts, Message, Port, Process, Resource, Thread, Vcpu, Vmo,
};

// The runtime's pieces are a real program's: a unit test of the crate has
// the host's allocator, panic handler and C library, and a program on the
// standard library its panic handler and C library.
#[cfg(all(feature = "runtime", not(test)))]
#[global_allocator]
static HEAP: heap::Heap = heap::Heap::new();

#[cfg(all(feature = "runtime", not(test), not(target_os = "veda")))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    // Formatted without allocating, in case the heap is what failed.
    io::log_without_allocating(format_args!("panic: {info}\n"));
    sys::process_exit(vabi::EXIT_CODE_PANICKED)
}
