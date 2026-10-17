// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The POSIX layer on AMD64: what Linux's x86-64 ABI has of its own
//! ([`linux`], [`syscall`]), the thread pointer (the FS base, which `%fs:0`
//! holds), and the code new threads start in.

pub mod linux;
pub mod syscall;

/// The calling thread's thread pointer (once the C library has set up
/// thread-local storage): the x86-64 TLS ABI keeps it at `%fs:0`.
pub fn thread_pointer() -> usize {
    let tp: usize;
    // SAFETY: with TLS set up, %fs:0 holds the thread pointer itself.
    unsafe { core::arch::asm!("mov {}, qword ptr fs:[0]", out(reg) tp, options(nostack, readonly, preserves_flags)) };
    tp
}

/// Where a thread `__veda_clone` makes starts: `__veda_thread_entry`.
#[cfg(target_os = "none")]
pub fn thread_entry_address() -> usize {
    unsafe extern "C" {
        fn __veda_thread_entry();
    }
    __veda_thread_entry as *const () as usize
}

#[cfg(not(target_os = "none"))]
pub fn thread_entry_address() -> usize {
    0
}

// A new thread starts here with rdi = rsp = its `Start` (see `thread`):
// it installs its thread pointer and exit futex, calls its function, and
// ends (`thread::thread_returned`) if the function returns.
#[cfg(target_os = "none")]
core::arch::global_asm!(
    ".global __veda_thread_entry",
    ".hidden __veda_thread_entry",
    ".type __veda_thread_entry, @function",
    "__veda_thread_entry:",
    "    mov rbx, rdi",
    "    mov rdi, [rbx + 16]",
    "    mov eax, {set_tls}",
    "    syscall",
    "    mov rdi, [rbx + 24]",
    "    mov eax, {set_exit_futex}",
    "    syscall",
    "    mov rdi, [rbx + 8]",
    "    xor ebp, ebp",
    "    call [rbx]",
    "    call {returned}",
    "    ud2",
    ".size __veda_thread_entry, . - __veda_thread_entry",
    set_tls = const abi::nr::THREAD_SET_TLS,
    set_exit_futex = const abi::nr::THREAD_SET_EXIT_FUTEX,
    returned = sym crate::thread::thread_returned,
);
