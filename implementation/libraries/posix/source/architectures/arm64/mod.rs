// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The POSIX layer on ARM64: what Linux's AArch64 ABI has of its own
//! ([`linux`], [`syscall`]), the thread pointer (`TPIDR_EL0`), the code new
//! threads start in, and how signals reach a thread ([`signal`]).

pub mod linux;
pub mod signal;
pub mod syscall;

/// The calling thread's thread pointer (once the C library has set up
/// thread-local storage).
pub fn thread_pointer() -> usize {
    let tp: usize;
    // SAFETY: reading the thread pointer changes nothing.
    unsafe { core::arch::asm!("mrs {}, tpidr_el0", out(reg) tp, options(nomem, nostack, preserves_flags)) };
    tp
}

/// Where a thread `__veda_clone` makes starts: `__veda_thread_entry`.
#[cfg(feature = "layer")]
pub fn thread_entry_address() -> usize {
    unsafe extern "C" {
        fn __veda_thread_entry();
    }
    __veda_thread_entry as *const () as usize
}

#[cfg(not(feature = "layer"))]
pub fn thread_entry_address() -> usize {
    0
}

// A new thread starts here with x0 = sp = its `Start` (see `thread`): it
// installs its thread pointer and exit futex, calls its function, and ends
// (`thread::thread_returned`) if the function returns.
#[cfg(feature = "layer")]
core::arch::global_asm!(
    ".global __veda_thread_entry",
    ".hidden __veda_thread_entry",
    ".type __veda_thread_entry, @function",
    "__veda_thread_entry:",
    "    mov x19, x0",
    "    ldr x0, [x19, #16]",
    "    mov x8, {set_tls}",
    "    svc #0",
    "    ldr x0, [x19, #24]",
    "    mov x8, {set_exit_futex}",
    "    svc #0",
    "    ldp x1, x0, [x19]",
    "    mov x29, #0",
    "    mov x30, #0",
    "    blr x1",
    "    bl {returned}",
    "    brk #0",
    ".size __veda_thread_entry, . - __veda_thread_entry",
    set_tls = const abi::nr::THREAD_SET_TLS,
    set_exit_futex = const abi::nr::THREAD_SET_EXIT_FUTEX,
    returned = sym crate::thread::thread_returned,
);
