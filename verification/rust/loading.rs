// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! loading — checks of loading libraries at run time from a program on
//! Rust's standard library: a Rust library (verification/rust/shared,
//! `/system/tests/rust/lib/librustplugin.so`) and a C one
//! (verification/c/shared, `/system/tests/c/lib/libplugin.so`), through
//! the C library's dynamic linker (see documentation/LINKING.md). Like the
//! other Rust test programs it is built position-independent and at a
//! fixed address, both linked statically, and dynamically
//! (`-C target-feature=-crt-static`): a static program checks that loading
//! fails as it says it does, the dynamic one the rest.
//!
//! Each check prints `ok - NAME` or `not ok - NAME: REASON`; the program
//! exits with the number of failed checks (0: all passed). `systest` runs it
//! inside Veda.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::{panic, thread};

const RUST_LIBRARY: &CStr = c"/system/tests/rust/lib/librustplugin.so";
const C_LIBRARY: &CStr = c"/system/tests/c/lib/libplugin.so";
/// `dlopen`'s flag to bind every symbol at once (the C library's
/// `RTLD_NOW`).
const RTLD_NOW: c_int = 2;
/// The auxiliary vector's entry that says where the dynamic linker is.
const AT_BASE: u64 = 7;

// The C library's (`<dlfcn.h>`, `<sys/auxv.h>`): the standard library has
// no loading of libraries of its own.
unsafe extern "C" {
    fn dlopen(file: *const c_char, mode: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
    fn dlerror() -> *const c_char;
    fn getauxval(kind: u64) -> u64;
}

type Check = Result<(), String>;

/// `Err(what)` unless `ok`.
fn ensure(ok: bool, what: impl FnOnce() -> String) -> Check {
    if ok { Ok(()) } else { Err(what()) }
}

/// What `dlerror` says, once.
fn said() -> String {
    // SAFETY: dlerror returns null, or a NUL-terminated string that lives
    // until this thread's next call into the dynamic linker, which is read
    // before it.
    unsafe {
        let e = dlerror();
        if e.is_null() { "(nothing)".into() } else { CStr::from_ptr(e).to_string_lossy().into_owned() }
    }
}

/// A library loaded, or why not.
fn load(path: &CStr) -> Result<*mut c_void, String> {
    // SAFETY: a NUL-terminated path; loading runs the library's
    // constructors, which are the test's own.
    let h = unsafe { dlopen(path.as_ptr(), RTLD_NOW) };
    if h.is_null() { Err(format!("loading {}: {}", path.to_string_lossy(), said())) } else { Ok(h) }
}

/// The address of the symbol `name` of the library `h`, or why not.
fn symbol(h: *mut c_void, name: &CStr) -> Result<*mut c_void, String> {
    // SAFETY: a handle dlopen gave, a NUL-terminated name.
    let s = unsafe { dlsym(h, name.as_ptr()) };
    if s.is_null() { Err(format!("{}: {}", name.to_string_lossy(), said())) } else { Ok(s) }
}

/// Whether the program is linked dynamically: the dynamic linker started
/// it.
fn dynamic() -> bool {
    // SAFETY: getauxval reads the auxiliary vector the C library keeps.
    unsafe { getauxval(AT_BASE) != 0 }
}

/// Linked statically, the program loads nothing, and says why.
fn check_static() -> Check {
    let refused = load(RUST_LIBRARY).err().unwrap_or_default();
    ensure(refused.contains("linked statically"), || format!("loading said: {refused}"))
}

fn check_rust_library() -> Check {
    let h = load(RUST_LIBRARY)?;
    // SAFETY: the library's functions, of these types
    // (verification/rust/shared/rustplugin.rs).
    unsafe {
        let add: extern "C" fn(c_int, c_int) -> c_int = std::mem::transmute(symbol(h, c"rust_plugin_add")?);
        let greeting: extern "C" fn() -> *const c_char = std::mem::transmute(symbol(h, c"rust_plugin_greeting")?);
        let caught: extern "C" fn() -> c_int = std::mem::transmute(symbol(h, c"rust_plugin_caught_panic")?);
        let threads: extern "C" fn(c_int, c_int) -> c_int = std::mem::transmute(symbol(h, c"rust_plugin_threads")?);
        ensure(add(2, 40) == 42, || "rust_plugin_add".into())?;
        let text = CStr::from_ptr(greeting()).to_string_lossy().into_owned();
        ensure(text == "Hello from Rust", || format!("rust_plugin_greeting said {text}"))?;
        ensure(caught() == 1, || "the library did not catch its own panic".into())?;
        ensure(threads(4, 100) == 4 * 4950, || "the library's threads".into())
    }
}

/// The library's thread-local storage: each thread's own, threads started
/// after it was loaded too.
fn check_thread_locals() -> Check {
    let h = load(RUST_LIBRARY)?;
    // SAFETY: the library's function, of this type.
    let count: extern "C" fn() -> c_int = unsafe { std::mem::transmute(symbol(h, c"rust_plugin_count")?) };
    let mine = (count(), count());
    let theirs: Vec<c_int> =
        (0..3).map(|_| thread::spawn(move || (count(), count()).1)).map(|t| t.join().unwrap_or(-1)).collect();
    ensure(mine == (1, 2) && theirs == [2, 2, 2] && count() == 3, || format!("counted {mine:?}, {theirs:?}"))
}

fn check_c_library() -> Check {
    let h = load(C_LIBRARY)?;
    // SAFETY: the library's function, of this type
    // (verification/c/shared/plugin.c).
    let twice: extern "C" fn(c_int) -> c_int = unsafe { std::mem::transmute(symbol(h, c"plugin_twice")?) };
    ensure(twice(21) == 42, || "plugin_twice".into())
}

/// The program's panics, with libraries loaded: its unwinder finds its
/// frames among theirs; and its backtraces name its functions.
fn check_panics() -> Check {
    load(RUST_LIBRARY)?;
    let hook = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));
    let caught = panic::catch_unwind(|| panic!("in the program")).is_err();
    panic::set_hook(hook);
    ensure(caught, || "the panic was not caught".into())?;
    let trace = std::backtrace::Backtrace::force_capture().to_string();
    ensure(trace.contains("check_panics"), || format!("backtrace:\n{trace}"))
}

fn main() {
    let program = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default();
    let linked = if dynamic() { "dynamically" } else { "statically" };
    println!("loading: {program}, linked {linked}");
    let checks: &[(&str, fn() -> Check)] = if dynamic() {
        &[
            ("a Rust library", check_rust_library),
            ("a Rust library's thread locals", check_thread_locals),
            ("a C library", check_c_library),
            ("panics and backtraces beside libraries", check_panics),
        ]
    } else {
        &[("static: loading fails, saying why", check_static)]
    };
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
