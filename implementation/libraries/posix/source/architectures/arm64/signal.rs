// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! How signals reach a thread on ARM64 (see `crate::interrupt`): the word
//! the layer keeps for each thread, in the C library's thread structure;
//! `__veda_syscall`, the gate every system call of the C library passes,
//! which keeps where its caller resumes, for the contexts of the handlers
//! that run as the call ends; the signal entry, where the kernel sends an
//! interrupted thread with its registers on its stack (`abi::architecture`'s
//! "Signal entry"); and the registers as musl's `mcontext_t` has them.

use core::sync::atomic::{AtomicUsize, Ordering};

use abi::ThreadState;

/// Where the layer's word for a thread is, below its thread pointer:
/// musl's `struct pthread` field `sysinfo` (after `self`, `prev` and
/// `next`, at 24 of the 200 bytes that end at the thread pointer on
/// AArch64, whose thread-local storage is above it), which only i386's C
/// library uses (for its system call entry), and which the layer clears for
/// each thread before it runs.
const WORD_BELOW: usize = 200 - 24;

/// The calling thread's word.
fn own_word() -> &'static AtomicUsize {
    // SAFETY: with thread-local storage set up, the word is the thread's.
    unsafe { word_of(super::thread_pointer()) }
}

/// The calling thread's word.
pub fn word() -> usize {
    own_word().load(Ordering::SeqCst)
}

/// Sets `bits` of the calling thread's word, atomically (other threads set
/// and clear bits of it too, as [`word_of`] lets them).
pub fn set_word(bits: usize) {
    own_word().fetch_or(bits, Ordering::SeqCst);
}

/// Clears `bits` of the calling thread's word, as [`set_word`] sets them.
pub fn clear_word(bits: usize) {
    own_word().fetch_and(!bits, Ordering::SeqCst);
}

/// The word of the thread whose thread pointer is `tp`.
///
/// # Safety
/// `tp` must be the thread pointer of a thread that lives (or is about to
/// start), whose structure is musl's.
pub unsafe fn word_of(tp: usize) -> &'static AtomicUsize {
    // SAFETY: per the caller; the word is aligned, as the structure is.
    unsafe { &*((tp - WORD_BELOW) as *const AtomicUsize) }
}

/// Learns how the signal entry saves the registers of the code it
/// interrupts: there is nothing to learn. Veda leaves SVE's and SME's
/// registers off, so a program has the same 32 SIMD registers everywhere.
pub fn init() {}

/// Where the C library's caller of a system call resumes when it returns:
/// what `__veda_syscall` keeps, which the handlers that run as the call
/// ends see as their context, and may change (musl's cancellation sends
/// the thread elsewhere so).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Caller {
    /// `x19` to `x28`.
    pub saved: [u64; 10],
    pub fp: u64,
    /// Where the call returns.
    pub pc: u64,
    /// The stack pointer once the call has returned.
    pub sp: u64,
    /// What it returns.
    pub x0: u64,
}

impl Caller {
    /// Sets what the call returns.
    pub fn set_result(&mut self, value: u64) {
        self.x0 = value;
    }

    /// The registers of the caller as the call returns (those the call
    /// does not keep are 0, as they are no longer the caller's).
    pub fn state(&self) -> ThreadState {
        let mut s = ThreadState { sp: self.sp, pc: self.pc, ..ThreadState::default() };
        s.x[0] = self.x0;
        s.x[19..29].copy_from_slice(&self.saved);
        s.x[29] = self.fp;
        s.x[30] = self.pc;
        s
    }

    /// Takes what a handler left of the registers the call keeps, and where
    /// and with what it returns.
    pub fn resume(&mut self, s: &ThreadState) {
        self.saved.copy_from_slice(&s.x[19..29]);
        self.fp = s.x[29];
        self.pc = s.pc;
        self.sp = s.sp;
        self.x0 = s.x[0];
    }
}

/// A handler that sent the thread elsewhere needs nothing more here:
/// nothing of `__veda_syscall`'s is left on the stack as it returns, and
/// musl's cancellation point keeps its own frame until its exit.
pub fn redirected(_before: &ThreadState, _after: &mut ThreadState) {}

/// Where `mcontext_t` has its registers (`regs`, after `fault_address`),
/// and the stack pointer, the program counter and `pstate` after them.
const MCONTEXT_REGS: usize = 1;

/// Stores registers `s` as musl's `mcontext_t` on AArch64 has them, as
/// Linux's signal frames do: `fault_address` (0), `regs`, `sp`, `pc` and
/// `pstate` (no floating point is kept).
///
/// # Safety
/// `out` must point at 280 writable bytes.
pub unsafe fn store_mcontext(s: &ThreadState, out: *mut core::ffi::c_void) {
    let mut m = [0u64; 35];
    m[MCONTEXT_REGS..MCONTEXT_REGS + 31].copy_from_slice(&s.x);
    (m[32], m[33], m[34]) = (s.sp, s.pc, s.pstate);
    // SAFETY: per the caller.
    unsafe { core::ptr::write_unaligned(out.cast::<[u64; 35]>(), m) };
}

/// The registers of musl's `mcontext_t` at `m` ([`store_mcontext`]), into
/// `s` (its thread pointer stays).
///
/// # Safety
/// `m` must point at 280 readable bytes.
pub unsafe fn load_mcontext(m: *const core::ffi::c_void, s: &mut ThreadState) {
    // SAFETY: per the caller.
    let g = unsafe { core::ptr::read_unaligned(m.cast::<[u64; 35]>()) };
    s.x.copy_from_slice(&g[MCONTEXT_REGS..MCONTEXT_REGS + 31]);
    (s.sp, s.pc, s.pstate) = (g[32], g[33], g[34]);
}

/// Where the kernel sends an interrupted thread: `__veda_signal_entry`.
#[cfg(feature = "signal-entry")]
pub fn entry_address() -> usize {
    unsafe extern "C" {
        fn __veda_signal_entry();
    }
    __veda_signal_entry as *const () as usize
}

// `__veda_syscall(n, a, b, c, d, e, f)`, the C library's system call: keeps
// a `Caller` on its stack (the caller's registers the call keeps, its stack
// pointer and return address), has `crate::syscall_gate` carry the call out
// with it, and returns where it then says, with the stack it says.
//
// `crate::syscall_gate` takes the same seven arguments and the `Caller`'s
// address, all in registers.
#[cfg(feature = "layer")]
core::arch::global_asm!(
    ".global __veda_syscall",
    ".hidden __veda_syscall",
    ".type __veda_syscall, @function",
    "__veda_syscall:",
    "    sub sp, sp, #112",
    "    stp x19, x20, [sp, #0]",
    "    stp x21, x22, [sp, #16]",
    "    stp x23, x24, [sp, #32]",
    "    stp x25, x26, [sp, #48]",
    "    stp x27, x28, [sp, #64]",
    "    stp x29, x30, [sp, #80]",
    "    add x8, sp, #112",
    "    stp x8, xzr, [sp, #96]",
    "    mov x7, sp",
    "    bl {gate}",
    "    ldp x19, x20, [sp, #0]",
    "    ldp x21, x22, [sp, #16]",
    "    ldp x23, x24, [sp, #32]",
    "    ldp x25, x26, [sp, #48]",
    "    ldp x27, x28, [sp, #64]",
    "    ldp x29, x30, [sp, #80]",
    "    ldp x8, x0, [sp, #96]",
    "    mov sp, x8",
    "    ret",
    ".size __veda_syscall, . - __veda_syscall",
    gate = sym crate::syscall_gate,
);

// The signal entry: `x0` holds the address of the interrupted thread's
// registers (64-byte aligned), which is also the stack pointer. Keeps the
// floating-point and SIMD registers below, has
// `crate::interrupt::signal_entry` act on the interruption, takes the
// registers back, and resumes the thread (`signal_return`), as interrupted
// or as the signal entry changed it, with the flags it returned. The
// registers are there on a soft-float target too (a host of foreign code
// keeps them for it), whose assembler is told so.
#[cfg(feature = "signal-entry")]
core::arch::global_asm!(
    ".arch_extension fp",
    ".arch_extension simd",
    ".global __veda_signal_entry",
    ".hidden __veda_signal_entry",
    ".type __veda_signal_entry, @function",
    "__veda_signal_entry:",
    "    mov x19, x0",
    "    sub sp, sp, #528",
    "    mov x20, sp",
    "    stp q0, q1, [x20, #0]",
    "    stp q2, q3, [x20, #32]",
    "    stp q4, q5, [x20, #64]",
    "    stp q6, q7, [x20, #96]",
    "    stp q8, q9, [x20, #128]",
    "    stp q10, q11, [x20, #160]",
    "    stp q12, q13, [x20, #192]",
    "    stp q14, q15, [x20, #224]",
    "    stp q16, q17, [x20, #256]",
    "    stp q18, q19, [x20, #288]",
    "    stp q20, q21, [x20, #320]",
    "    stp q22, q23, [x20, #352]",
    "    stp q24, q25, [x20, #384]",
    "    stp q26, q27, [x20, #416]",
    "    stp q28, q29, [x20, #448]",
    "    stp q30, q31, [x20, #480]",
    "    mrs x1, fpcr",
    "    mrs x2, fpsr",
    "    str x1, [x20, #512]",
    "    str x2, [x20, #520]",
    "    bl {entry}",
    "    mov x21, x0",
    "    ldr x1, [x20, #512]",
    "    ldr x2, [x20, #520]",
    "    msr fpcr, x1",
    "    msr fpsr, x2",
    "    ldp q0, q1, [x20, #0]",
    "    ldp q2, q3, [x20, #32]",
    "    ldp q4, q5, [x20, #64]",
    "    ldp q6, q7, [x20, #96]",
    "    ldp q8, q9, [x20, #128]",
    "    ldp q10, q11, [x20, #160]",
    "    ldp q12, q13, [x20, #192]",
    "    ldp q14, q15, [x20, #224]",
    "    ldp q16, q17, [x20, #256]",
    "    ldp q18, q19, [x20, #288]",
    "    ldp q20, q21, [x20, #320]",
    "    ldp q22, q23, [x20, #352]",
    "    ldp q24, q25, [x20, #384]",
    "    ldp q26, q27, [x20, #416]",
    "    ldp q28, q29, [x20, #448]",
    "    ldp q30, q31, [x20, #480]",
    "    mov x0, x19",
    "    mov x1, x21",
    "    mov x8, {signal_return}",
    "    svc #0",
    "    brk #0",
    ".size __veda_signal_entry, . - __veda_signal_entry",
    entry = sym crate::interrupt::signal_entry,
    signal_return = const abi::nr::SIGNAL_RETURN,
);

#[cfg(test)]
pub mod tests {
    use super::*;

    /// Registers that say where a thread is.
    pub fn registers() -> ThreadState {
        ThreadState { pc: 0x5000, sp: 0x7000, ..ThreadState::default() }
    }

    #[test]
    fn contexts_go_to_and_from_musls_mcontext() {
        let mut s = ThreadState { sp: 8, pc: 17, pstate: 18, tpidr: 19, ..ThreadState::default() };
        s.x[0] = 1;
        s.x[30] = 31;
        let mut m = [0u64; 35];
        // SAFETY: 280 bytes, read back as written.
        unsafe { store_mcontext(&s, m.as_mut_ptr().cast()) };
        // `fault_address`, then `regs`, then `sp`, `pc` and `pstate`.
        assert_eq!((m[0], m[1], m[31], m[32], m[33], m[34]), (0, 1, 31, 8, 17, 18));
        let mut back = ThreadState { tpidr: 19, ..ThreadState::default() };
        // SAFETY: as above.
        unsafe { load_mcontext(m.as_ptr().cast(), &mut back) };
        assert_eq!(back, s);
    }

    #[test]
    fn a_caller_resumes_as_its_handlers_leave_it() {
        let mut saved = [0; 10];
        saved[0] = 1;
        let c = Caller { saved, fp: 2, pc: 0x5000, sp: 0x7000_0010, x0: 7 };
        let mut s = c.state();
        assert_eq!((s.sp, s.pc, s.x[0], s.x[19], s.x[1]), (0x7000_0010, 0x5000, 7, 1, 0));
        s.pc = 0x6000;
        s.x[20] = 9;
        let mut back = c;
        back.resume(&s);
        assert_eq!((back.pc, back.saved[1], back.sp), (0x6000, 9, 0x7000_0010));
    }
}
