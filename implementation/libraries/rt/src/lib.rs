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

#![no_std]

extern crate alloc;
#[cfg(any(test, veda_guest))]
extern crate std;

pub use alloc::{boxed, format, rc, string, vec};
pub use vabi;

#[cfg(target_arch = "x86_64")]
#[path = "arch/amd64/mod.rs"]
pub mod arch;
#[cfg(all(feature = "runtime", not(test)))]
mod crt;
pub mod env;
#[cfg(veda_guest)]
pub mod guest;
pub mod heap;
pub mod io;
pub mod object;
pub mod pool;
pub mod process;
#[cfg(all(feature = "runtime", not(test)))]
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
// the host's allocator, panic handler and C library.
#[cfg(all(feature = "runtime", not(test)))]
#[global_allocator]
static HEAP: heap::Heap = heap::Heap::new();

#[cfg(all(feature = "runtime", not(test)))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    // Formatted without allocating, in case the heap is what failed.
    io::log_without_allocating(format_args!("panic: {info}\n"));
    sys::process_exit(vabi::EXIT_CODE_PANICKED)
}
