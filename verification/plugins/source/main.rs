// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `plugins`: checks that a dynamically linked program on Rust's standard library loads
//! libraries at run time with libloading, as Veda's port of it has it
//! (`implementation/ports/libloading`): a Rust library and a C one (the test libraries of
//! `verification/rust/shared` and `verification/c/shared`, in `/system/tests`), what is wrong
//! said as libloading's errors, the program's own symbols, libraries closed, and loading from
//! threads at once. It is the image's dynamically linked program on the standard library
//! (`Program::dynamic`, `automation/source/components.rs`).
//!
//! `systest` runs it inside Veda. It says `ok - <check>` or `not ok - <check>: <why>` for each
//! check, then `PASS: 0 failed` (or `FAIL: n failed`), and exits with the number that failed,
//! as `verification/stdcrates` does.

use std::error::Error;
use std::ffi::{CStr, c_char, c_int, c_void};
use std::thread;

use libloading::{Library, Symbol};

const RUST_LIBRARY: &str = "/system/tests/rust/lib/librustplugin.so";
const C_LIBRARY: &str = "/system/tests/c/lib/libplugin.so";

type Check = Result<(), String>;

/// A check, and what the report calls it.
type Named = (&'static str, fn() -> Check);

fn ensure(ok: bool, what: &str) -> Check {
    if ok { Ok(()) } else { Err(what.into()) }
}

/// What libloading says of `error`, and why: its sources, the dynamic
/// linker's words (`dlerror`'s) among them.
fn said(error: &libloading::Error) -> String {
    let mut words = error.to_string();
    let mut cause = error.source();
    while let Some(e) = cause {
        words.push_str(": ");
        words.push_str(&e.to_string());
        cause = e.source();
    }
    words
}

/// `path`, loaded.
fn load(path: &str) -> Result<Library, String> {
    // SAFETY: the test libraries, whose constructors are the tests' own.
    unsafe { Library::new(path) }.map_err(|e| format!("loading {path}: {}", said(&e)))
}

/// A Rust library: functions of C's calling convention, a string it keeps,
/// its own threads.
fn check_rust_library() -> Check {
    let library = load(RUST_LIBRARY)?;
    // SAFETY: the library's functions, of these types
    // (verification/rust/shared/rustplugin.rs).
    unsafe {
        let add: Symbol<unsafe extern "C" fn(c_int, c_int) -> c_int> =
            library.get(c"rust_plugin_add").map_err(|e| said(&e))?;
        let greeting: Symbol<unsafe extern "C" fn() -> *const c_char> =
            library.get(c"rust_plugin_greeting").map_err(|e| said(&e))?;
        let threads: Symbol<unsafe extern "C" fn(c_int, c_int) -> c_int> =
            library.get(c"rust_plugin_threads").map_err(|e| said(&e))?;
        ensure(add(40, 2) == 42, "rust_plugin_add")?;
        ensure(CStr::from_ptr(greeting()).to_bytes() == b"Hello from Rust", "rust_plugin_greeting")?;
        ensure(threads(3, 10) == 3 * 45, "rust_plugin_threads")
    }
}

/// A C library, with the library it needs.
fn check_c_library() -> Check {
    let library = load(C_LIBRARY)?;
    // SAFETY: the library's functions, of these types
    // (verification/c/shared/plugin.c).
    unsafe {
        let twice: Symbol<unsafe extern "C" fn(c_int) -> c_int> = library.get(c"plugin_twice").map_err(|e| said(&e))?;
        let base: Symbol<unsafe extern "C" fn() -> c_int> = library.get(c"plugin_base").map_err(|e| said(&e))?;
        let answer: Symbol<*const c_int> = library.get(c"plugin_answer").map_err(|e| said(&e))?;
        ensure(twice(21) == 42 && **answer == 42, "plugin_twice, plugin_answer")?;
        ensure(base() > 40, "plugin_base")
    }
}

/// What is wrong is said as libloading's errors, with the dynamic linker's
/// words.
fn check_errors() -> Check {
    let missing = "/system/tests/c/lib/libnothing.so";
    // SAFETY: no library is there to run anything.
    let words = unsafe { Library::new(missing) }.err().map(|e| said(&e)).unwrap_or_default();
    ensure(words.contains("libnothing.so"), &format!("loading a library that is not there: {words}"))?;
    let library = load(C_LIBRARY)?;
    // SAFETY: the symbol is not there, so nothing is called.
    let words = unsafe { library.get::<*const c_void>(c"no_such_symbol") }.err().map(|e| said(&e));
    let words = words.unwrap_or_default();
    ensure(words.contains("no_such_symbol"), &format!("a symbol that is not there: {words}"))
}

/// The program's own handle: what the program and the C library define.
fn check_the_program() -> Check {
    let program = libloading::os::unix::Library::this();
    // SAFETY: the C library's `strlen`, of this type.
    unsafe {
        let strlen: libloading::os::unix::Symbol<unsafe extern "C" fn(*const c_char) -> usize> =
            program.get(c"strlen").map_err(|e| said(&e))?;
        ensure(strlen(c"plugins".as_ptr()) == 7, "strlen through the program's handle")
    }
}

/// A library closed: its handle goes, and loading it again gives it back.
fn check_closing() -> Check {
    load(RUST_LIBRARY)?.close().map_err(|e| format!("closing: {}", said(&e)))?;
    let library = load(RUST_LIBRARY)?;
    // SAFETY: as in `check_rust_library`.
    let add: Symbol<unsafe extern "C" fn(c_int, c_int) -> c_int> =
        unsafe { library.get(c"rust_plugin_add") }.map_err(|e| said(&e))?;
    // SAFETY: as in `check_rust_library`.
    ensure(unsafe { add(1, 1) } == 2, "the library loaded again")
}

/// Threads loading libraries at once, each counting with its own
/// thread-local storage in the Rust library.
fn check_threads() -> Check {
    let workers: Vec<_> = (0..8)
        .map(|i| {
            thread::spawn(move || -> Result<c_int, String> {
                let library = load(if i % 2 == 0 { RUST_LIBRARY } else { C_LIBRARY })?;
                if i % 2 == 1 {
                    return Ok(2);
                }
                // SAFETY: as in `check_rust_library`.
                unsafe {
                    let count: Symbol<unsafe extern "C" fn() -> c_int> =
                        library.get(c"rust_plugin_count").map_err(|e| said(&e))?;
                    count();
                    Ok(count())
                }
            })
        })
        .collect();
    let results: Vec<Result<c_int, String>> =
        workers.into_iter().map(|w| w.join().unwrap_or_else(|_| Err("a thread panicked".into()))).collect();
    let counts = results.into_iter().collect::<Result<Vec<c_int>, String>>()?;
    ensure(counts.iter().all(|&c| c == 2), &format!("counted {counts:?}"))
}

fn main() {
    let checks: &[Named] = &[
        ("a Rust library", check_rust_library),
        ("a C library", check_c_library),
        ("errors", check_errors),
        ("the program's handle", check_the_program),
        ("closing", check_closing),
        ("threads", check_threads),
    ];
    let mut failures = 0;
    for (name, check) in checks {
        match check() {
            Ok(()) => println!("ok - {name}"),
            Err(reason) => {
                failures += 1;
                println!("not ok - {name}: {reason}");
            }
        }
    }
    println!("{}: {failures} failed", if failures == 0 { "PASS" } else { "FAIL" });
    std::process::exit(failures);
}
