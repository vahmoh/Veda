// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! How signals reach a thread on AMD64 (see `crate::interrupt`): the word
//! the layer keeps for each thread, in the C library's thread structure;
//! `__veda_syscall`, the gate every system call of the C library passes,
//! which keeps where its caller resumes, for the contexts of the handlers
//! that run as the call ends; the signal entry, where the kernel sends an
//! interrupted thread with its registers on its stack (`abi::architecture`'s
//! "Signal entry"); and the registers as musl's `mcontext_t` has them.

use core::sync::atomic::{AtomicUsize, Ordering};

use abi::ThreadState;

/// Where the layer's word for a thread is, from its thread pointer: musl's
/// `struct pthread` field `sysinfo` (after `self`, `dtv`, `prev` and
/// `next`), which only i386's C library uses (for its system call entry),
/// and which the layer clears for each thread before it runs.
const WORD: usize = 32;

/// The calling thread's word.
pub fn word() -> usize {
    let w: usize;
    // SAFETY: with thread-local storage set up, the word is the thread's.
    unsafe {
        core::arch::asm!("mov {}, qword ptr fs:[{}]", out(reg) w, const WORD, options(nostack, readonly, preserves_flags))
    };
    w
}

/// Sets `bits` of the calling thread's word, atomically (other threads set
/// and clear bits of it too, as [`word_of`] lets them).
pub fn set_word(bits: usize) {
    // SAFETY: as in `word`.
    unsafe { core::arch::asm!("lock or qword ptr fs:[{}], {}", const WORD, in(reg) bits, options(nostack)) };
}

/// Clears `bits` of the calling thread's word, as [`set_word`] sets them.
pub fn clear_word(bits: usize) {
    // SAFETY: as in `word`.
    unsafe { core::arch::asm!("lock and qword ptr fs:[{}], {}", const WORD, in(reg) !bits, options(nostack)) };
}

/// The word of the thread whose thread pointer is `tp`.
///
/// # Safety
/// `tp` must be the thread pointer of a thread that lives (or is about to
/// start), whose structure is musl's.
pub unsafe fn word_of(tp: usize) -> &'static AtomicUsize {
    // SAFETY: per the caller; the word is aligned, as the structure is.
    unsafe { &*((tp + WORD) as *const AtomicUsize) }
}

/// The bytes the signal entry keeps the x87, SSE and AVX registers in
/// (XSAVE's area for what the kernel enabled, or FXSAVE's 512), set by
/// [`init`]; 0 before.
#[cfg_attr(not(target_os = "none"), allow(dead_code))]
static SAVE_SIZE: AtomicUsize = AtomicUsize::new(0);

/// Whether the processor saves its registers with XSAVE (else FXSAVE).
#[cfg_attr(not(target_os = "none"), allow(dead_code))]
static XSAVE: AtomicUsize = AtomicUsize::new(0);

/// Learns how the signal entry saves the registers of the code it
/// interrupts: the processor's XSAVE area for what the kernel enabled
/// (CPUID leaf 0xD), or FXSAVE's.
#[cfg_attr(not(target_os = "none"), allow(dead_code))]
pub fn init() {
    let osxsave = core::arch::x86_64::__cpuid(1).ecx & (1 << 27) != 0;
    if osxsave {
        // Leaf 0xD exists where XSAVE does.
        let size = core::arch::x86_64::__cpuid_count(0xD, 0).ebx as usize;
        SAVE_SIZE.store(size.max(576), Ordering::Relaxed);
        XSAVE.store(1, Ordering::Relaxed);
    } else {
        SAVE_SIZE.store(512, Ordering::Relaxed);
    }
}

/// Where the C library's caller of a system call resumes when it returns:
/// what `__veda_syscall` keeps, which the handlers that run as the call
/// ends see as their context, and may change (musl's cancellation sends
/// the thread elsewhere so).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Caller {
    pub rbx: u64,
    pub rbp: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
    /// The stack pointer once the call has returned.
    pub rsp: u64,
    /// Where the call returns.
    pub rip: u64,
    /// What it returns.
    pub rax: u64,
}

impl Caller {
    /// The registers of the caller as the call returns (those the call
    /// does not keep are 0, as they are no longer the caller's).
    pub fn state(&self) -> ThreadState {
        ThreadState {
            rbx: self.rbx,
            rbp: self.rbp,
            r12: self.r12,
            r13: self.r13,
            r14: self.r14,
            r15: self.r15,
            rsp: self.rsp,
            rip: self.rip,
            rax: self.rax,
            ..ThreadState::default()
        }
    }

    /// Takes what a handler left of the registers the call keeps, and where
    /// and with what it returns.
    pub fn resume(&mut self, s: &ThreadState) {
        *self = Caller {
            rbx: s.rbx,
            rbp: s.rbp,
            r12: s.r12,
            r13: s.r13,
            r14: s.r14,
            r15: s.r15,
            rsp: s.rsp,
            rip: s.rip,
            rax: s.rax,
        };
    }
}

/// A handler that sent the thread elsewhere (changed its instruction
/// pointer, `before` being where it was) but left its stack pointer: it
/// resumes as on entry to a function there, its stack pointer rounded up
/// to 8 off a multiple of 16. That is what musl's cancellation expects of
/// `__cp_cancel`, the cancellation point's exit, which it jumps to from
/// `__veda_syscall`'s call (whose seventh argument is still on the stack),
/// as from any point of the cancellation point.
pub fn redirected(before: &ThreadState, after: &mut ThreadState) {
    if after.rip != before.rip && after.rsp == before.rsp && after.rsp % 16 != 8 {
        after.rsp = (after.rsp | 15) - 7;
    }
}

/// Stores registers `s` as musl's `mcontext_t` on x86-64 has them, as
/// Linux's signal frames do: `gregs` (23 of them, in `REG_R8`'s order),
/// then `fpregs` (null: no floating point is kept) and eight reserved
/// words.
///
/// # Safety
/// `out` must point at 256 writable bytes.
pub unsafe fn store_mcontext(s: &ThreadState, out: *mut core::ffi::c_void) {
    let mut m = [0u64; 32];
    let gregs = [
        s.r8, s.r9, s.r10, s.r11, s.r12, s.r13, s.r14, s.r15, s.rdi, s.rsi, s.rbp, s.rbx, s.rdx, s.rax, s.rcx, s.rsp,
        s.rip, s.rflags,
    ];
    m[..gregs.len()].copy_from_slice(&gregs);
    // SAFETY: per the caller.
    unsafe { core::ptr::write_unaligned(out.cast::<[u64; 32]>(), m) };
}

/// The registers of musl's `mcontext_t` at `m` ([`store_mcontext`]), into
/// `s` (its thread pointer stays).
///
/// # Safety
/// `m` must point at 256 readable bytes.
pub unsafe fn load_mcontext(m: *const core::ffi::c_void, s: &mut ThreadState) {
    // SAFETY: per the caller.
    let g = unsafe { core::ptr::read_unaligned(m.cast::<[u64; 18]>()) };
    [s.r8, s.r9, s.r10, s.r11, s.r12, s.r13, s.r14, s.r15, s.rdi, s.rsi, s.rbp, s.rbx, s.rdx, s.rax, s.rcx, s.rsp] =
        [g[0], g[1], g[2], g[3], g[4], g[5], g[6], g[7], g[8], g[9], g[10], g[11], g[12], g[13], g[14], g[15]];
    s.rip = g[16];
    s.rflags = g[17];
}

/// Where the kernel sends an interrupted thread: `__veda_signal_entry`.
#[cfg(target_os = "none")]
pub fn entry_address() -> usize {
    unsafe extern "C" {
        fn __veda_signal_entry();
    }
    __veda_signal_entry as *const () as usize
}

// `__veda_syscall(n, a, b, c, d, e, f)`, the C library's system call: keeps
// a `Caller` on its stack (the caller's registers the call keeps, its stack
// pointer and return address), has `crate::syscall_gate` carry the call out
// with it, and returns as it then says: with a plain `ret` when nothing
// changed where the caller resumes, else by jumping there.
//
// The frame, from the stack pointer: the gate's seventh and eighth
// arguments (`f` and the `Caller`'s address), then the `Caller` (72 bytes),
// 8 bytes of padding, and the return address (at `rsp + 88`, `f` above it).
#[cfg(target_os = "none")]
core::arch::global_asm!(
    ".global __veda_syscall",
    ".hidden __veda_syscall",
    ".type __veda_syscall, @function",
    "__veda_syscall:",
    "    sub rsp, 88",
    "    mov [rsp + 16], rbx",
    "    mov [rsp + 24], rbp",
    "    mov [rsp + 32], r12",
    "    mov [rsp + 40], r13",
    "    mov [rsp + 48], r14",
    "    mov [rsp + 56], r15",
    "    lea rax, [rsp + 96]",
    "    mov [rsp + 64], rax",
    "    mov rax, [rsp + 88]",
    "    mov [rsp + 72], rax",
    "    mov qword ptr [rsp + 80], 0",
    "    mov rax, [rsp + 96]",
    "    mov [rsp], rax",
    "    lea rax, [rsp + 16]",
    "    mov [rsp + 8], rax",
    "    call {gate}",
    "    mov rbx, [rsp + 16]",
    "    mov rbp, [rsp + 24]",
    "    mov r12, [rsp + 32]",
    "    mov r13, [rsp + 40]",
    "    mov r14, [rsp + 48]",
    "    mov r15, [rsp + 56]",
    "    mov rax, [rsp + 80]",
    "    mov rcx, [rsp + 72]",
    "    cmp rcx, [rsp + 88]",
    "    jne 2f",
    "    lea rcx, [rsp + 96]",
    "    cmp rcx, [rsp + 64]",
    "    jne 2f",
    "    add rsp, 88",
    "    ret",
    "2:",
    "    mov rcx, [rsp + 72]",
    "    mov rsp, [rsp + 64]",
    "    jmp rcx",
    ".size __veda_syscall, . - __veda_syscall",
    gate = sym crate::syscall_gate,
);

// The signal entry: `rdi` holds the address of the interrupted thread's
// registers (64-byte aligned), the stack pointer is 8 below it. Keeps the
// x87, SSE and AVX registers below (XSAVE wants its area's header zeroed),
// has `crate::interrupt::signal_entry` act on the interruption, takes the
// registers back, and resumes the thread (`signal_return`), as interrupted
// or as the signal entry changed it, with the flags it returned.
#[cfg(target_os = "none")]
core::arch::global_asm!(
    ".global __veda_signal_entry",
    ".hidden __veda_signal_entry",
    ".type __veda_signal_entry, @function",
    "__veda_signal_entry:",
    "    mov r12, rdi",
    "    sub rsp, [rip + {size}]",
    "    and rsp, -64",
    "    mov r13, rsp",
    "    mov r14, [rip + {xsave}]",
    "    test r14, r14",
    "    jz 1f",
    "    xor eax, eax",
    "    mov [r13 + 512], rax",
    "    mov [r13 + 520], rax",
    "    mov [r13 + 528], rax",
    "    mov [r13 + 536], rax",
    "    mov [r13 + 544], rax",
    "    mov [r13 + 552], rax",
    "    mov [r13 + 560], rax",
    "    mov [r13 + 568], rax",
    "    mov eax, -1",
    "    mov edx, -1",
    "    xsave64 [r13]",
    "    jmp 2f",
    "1:",
    "    fxsave64 [r13]",
    "2:",
    "    mov rdi, r12",
    "    call {entry}",
    "    mov r15, rax",
    "    test r14, r14",
    "    jz 3f",
    "    mov eax, -1",
    "    mov edx, -1",
    "    xrstor64 [r13]",
    "    jmp 4f",
    "3:",
    "    fxrstor64 [r13]",
    "4:",
    "    mov rdi, r12",
    "    mov rsi, r15",
    "    mov eax, {signal_return}",
    "    syscall",
    "    ud2",
    ".size __veda_signal_entry, . - __veda_signal_entry",
    size = sym SAVE_SIZE,
    xsave = sym XSAVE,
    entry = sym crate::interrupt::signal_entry,
    signal_return = const abi::nr::SIGNAL_RETURN,
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contexts_go_to_and_from_musls_mcontext() {
        let s = ThreadState {
            rax: 1,
            rbx: 2,
            rcx: 3,
            rdx: 4,
            rsi: 5,
            rdi: 6,
            rbp: 7,
            rsp: 8,
            r8: 9,
            r9: 10,
            r10: 11,
            r11: 12,
            r12: 13,
            r13: 14,
            r14: 15,
            r15: 16,
            rip: 17,
            rflags: 18,
            fs_base: 19,
        };
        let mut m = [0u64; 32];
        // SAFETY: 256 bytes, read back as written.
        unsafe { store_mcontext(&s, m.as_mut_ptr().cast()) };
        // musl's REG_RIP is 16, REG_RSP 15, REG_RAX 13, REG_RDI 8.
        assert_eq!((m[16], m[15], m[13], m[8]), (17, 8, 1, 6));
        let mut back = ThreadState { fs_base: 19, ..ThreadState::default() };
        // SAFETY: as above.
        unsafe { load_mcontext(m.as_ptr().cast(), &mut back) };
        assert_eq!(back, s);
    }

    #[test]
    fn a_caller_resumes_as_its_handlers_leave_it() {
        let c = Caller { rbx: 1, rbp: 2, r12: 3, r13: 4, r14: 5, r15: 6, rsp: 0x7000_0010, rip: 0x5000, rax: 7 };
        let mut s = c.state();
        assert_eq!((s.rsp, s.rip, s.rax, s.rdi), (0x7000_0010, 0x5000, 7, 0));
        s.rip = 0x6000;
        s.r12 = 9;
        let mut back = c;
        back.resume(&s);
        assert_eq!((back.rip, back.r12, back.rsp), (0x6000, 9, 0x7000_0010));
    }

    #[test]
    fn a_thread_sent_elsewhere_resumes_as_at_a_functions_entry() {
        let before = ThreadState { rip: 0x5000, rsp: 0x7000_0010, ..ThreadState::default() };
        // Sent elsewhere from just after `__veda_syscall`'s call, with its
        // seventh argument still on the stack: that word is left behind.
        let mut after = ThreadState { rip: 0x6000, ..before };
        redirected(&before, &mut after);
        assert_eq!(after.rsp, 0x7000_0018);
        // Already as on entry to a function: as it is.
        let before = ThreadState { rsp: 0x7000_0008, ..before };
        let mut after = ThreadState { rip: 0x6000, ..before };
        redirected(&before, &mut after);
        assert_eq!(after.rsp, 0x7000_0008);
        // Not sent elsewhere, or given a stack too: as the handler left it.
        let before = ThreadState { rsp: 0x7000_0010, ..before };
        let mut same = before;
        redirected(&before, &mut same);
        assert_eq!(same.rsp, 0x7000_0010);
        let mut moved = ThreadState { rip: 0x6000, rsp: 0x9000_0000, ..before };
        redirected(&before, &mut moved);
        assert_eq!(moved.rsp, 0x9000_0000);
    }
}
