// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! librustplugin — a Rust library (a `cdylib`, on its own copy of the
//! standard library) that programs in C and in Rust load at run time
//! (verification/c/libraries.c, verification/rust/loading.rs): functions
//! of C's calling convention, its thread-local storage, threads, its heap,
//! and a panic it catches itself, as one never leaves it.

use std::cell::Cell;
use std::ffi::{c_char, c_int};
use std::panic;
use std::thread;

thread_local! {
    static CALLS: Cell<c_int> = const { Cell::new(0) };
}

/// `a + b`.
#[unsafe(no_mangle)]
pub extern "C" fn rust_plugin_add(a: c_int, b: c_int) -> c_int {
    a + b
}

/// A string the library keeps.
#[unsafe(no_mangle)]
pub extern "C" fn rust_plugin_greeting() -> *const c_char {
    c"Hello from Rust".as_ptr()
}

/// Panics and catches the panic inside the library, through the unwinder
/// it shares with the program (libgcc_s.so.1): 1 if it caught it. The
/// panic's message is not shown.
#[unsafe(no_mangle)]
pub extern "C" fn rust_plugin_caught_panic() -> c_int {
    let hook = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));
    let caught = panic::catch_unwind(|| panic!("inside the library")).is_err();
    panic::set_hook(hook);
    c_int::from(caught)
}

/// How many times the calling thread has called it: its own count.
#[unsafe(no_mangle)]
pub extern "C" fn rust_plugin_count() -> c_int {
    CALLS.with(|calls| {
        calls.set(calls.get() + 1);
        calls.get()
    })
}

/// The sum of `0..n` in each of `threads` threads the library starts, and
/// whose results it collects on its heap: `threads * n * (n - 1) / 2`.
#[unsafe(no_mangle)]
pub extern "C" fn rust_plugin_threads(threads: c_int, n: c_int) -> c_int {
    let workers: Vec<_> = (0..threads).map(|_| thread::spawn(move || (0..n).sum::<c_int>())).collect();
    workers.into_iter().map(|w| w.join().unwrap_or(0)).sum()
}
