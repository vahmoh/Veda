// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Host tests of the compiler, by stage.

mod interp;
mod ir;
mod parse;
mod pp;
mod sema;
mod tgsi;

/// The stack of a thread Veda starts without asking for more
/// (`rt::thread::DEFAULT_STACK`), which a program may compile shaders on.
pub(crate) const THREAD_STACK: usize = 256 * 1024;

/// Runs `f` on a thread of its own with [`THREAD_STACK`] bytes of stack and
/// returns its result: a compiler that recursed without bound would
/// overflow it, aborting the tests.
pub(crate) fn on_small_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(THREAD_STACK)
        .spawn(f)
        .expect("a thread")
        .join()
        .unwrap_or_else(|e| std::panic::resume_unwind(e))
}
