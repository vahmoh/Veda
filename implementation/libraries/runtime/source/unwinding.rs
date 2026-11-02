// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What the precompiled `core` and `alloc` of the targets Veda's programs
//! are built for name for unwinding, which nothing does here: every crate
//! is built with `panic = "abort"`, but those two come as they are.

/// The exception personality that their unwind tables name. Never called:
/// nothing unwinds.
#[unsafe(no_mangle)]
pub extern "C" fn rust_eh_personality() -> ! {
    crate::sys::process_exit(-1)
}

/// Where their landing pads resume unwinding. Never reached: nothing
/// unwinds.
#[unsafe(no_mangle)]
pub extern "C" fn _Unwind_Resume() -> ! {
    crate::sys::process_exit(-1)
}
