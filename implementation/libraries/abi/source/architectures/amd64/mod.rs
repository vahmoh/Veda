// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The kernel ABI on AMD64: how system calls are made, the user address
//! space and its pages, how threads start, and virtual processors as Intel
//! VMX presents an x86-64 processor.
//!
//! # Calling convention
//!
//! System calls use the `syscall` instruction. `rax` holds the call number
//! (see [`nr`](crate::nr)), arguments are passed in `rdi, rsi, rdx, r10,
//! r8, r9`. On return `rax` holds a status: non-negative values are success
//! results, negative values are `-(error code)` (see
//! [`Error`](crate::Error)). Some calls return a second value in `rdx`.
//! `rcx` and `r11` are clobbered by the instruction; all other registers
//! are preserved.
//!
//! # Threads
//!
//! A thread starts at its entry point with its two arguments in `rdi` and
//! `rsi`, and the stack pointer [`entry_stack_pointer`] gives. Its thread
//! pointer (`thread_set_tls`) is its FS base. The clock's counter
//! ([`ClockInfo`](crate::ClockInfo)) is the TSC (`rdtsc`).
//!
//! # Signal entry
//!
//! An interrupted thread (`thread_interrupt`) goes to its process's signal
//! entry (`signal_entry_set`) as it next returns to user code, with its
//! user registers as it left that code in a [`ThreadState`] on its stack:
//! below the 128 bytes under its stack pointer (the System V ABI's red
//! zone, which the code interrupted may use), 64-byte aligned. `rdi` holds
//! its address; the stack pointer is 8 below it, as on entry to a
//! function; the flags are cleared, but for interrupts (`IF`), so that
//! the direction flag is clear. The other registers are those of the
//! code interrupted, but `rcx` and `r11`, which are not kept. The x87,
//! SSE and AVX registers are the code's: the entry keeps them, and
//! `signal_return` resumes the registers it is given. Of the flags,
//! `signal_return` takes those arithmetic sets and the direction flag
//! ([`SIGNAL_RETURN_FLAGS`]).

/// Page size used by the kernel for all mappings.
pub const PAGE_SIZE: usize = 4096;

/// Lowest address user space may map.
pub const USER_SPACE_START: usize = 0x0000_0000_0001_0000;
/// One past the highest user-space address.
pub const USER_SPACE_END: usize = 0x0000_7FFF_FFFF_0000;

/// Where position-independent programs are loaded: at a random page of
/// this span ([`program_base`](crate::program_base)), 2^28 places, so that
/// where their code is cannot be guessed.
pub const PROGRAM_BASE: usize = 0x0000_5555_0000_0000;
pub const PROGRAM_SPAN: usize = 1 << 40;

/// The stack pointer a thread of a Veda program starts with, its stack's
/// top being `top` (16-byte aligned): aligned as on entry to a function
/// (8 bytes, a return address's room, off 16-byte alignment), as the
/// System V ABI has it.
pub const fn entry_stack_pointer(top: usize) -> usize {
    top - 8
}

/// The user registers of a stopped thread (`thread_read_state`), as it
/// left user code. A thread stopped in a system call has the call's
/// result in `rax` once it has finished it, and `rcx` and `r11` as the
/// `syscall` instruction left them (its return address and flags).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ThreadState {
    pub rax: u64,
    pub rbx: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub rbp: u64,
    pub rsp: u64,
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
    pub r11: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
    pub rip: u64,
    pub rflags: u64,
    /// The thread pointer: its FS base.
    pub fs_base: u64,
}

/// Where an interrupted thread's [`ThreadState`] goes, its stack pointer
/// being `sp` as it left user code: below the System V ABI's red zone,
/// 64-byte aligned (see "Signal entry").
pub const fn signal_frame(sp: usize) -> usize {
    sp.wrapping_sub(128 + core::mem::size_of::<ThreadState>()) & !63
}

/// The flags `signal_return` takes from the registers it resumes: carry,
/// parity, adjust, zero, sign, direction and overflow. It sets interrupts
/// on, and the rest off ([`resumed_flags`]).
pub const SIGNAL_RETURN_FLAGS: u64 = 0xCD5;

/// The interrupt flag, which user code always runs with.
pub const INTERRUPT_FLAG: u64 = 0x200;

/// The flags `signal_return` resumes a thread with, given `flags`: those
/// user code may change, and interrupts on (no single-stepping, I/O
/// privilege, alignment checks or virtual-8086 mode).
pub const fn resumed_flags(flags: u64) -> u64 {
    (flags & SIGNAL_RETURN_FLAGS) | INTERRUPT_FLAG
}

/// A segment register of a virtual processor.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VcpuSegment {
    pub base: u64,
    pub limit: u32,
    /// The descriptor's attributes as Intel's VMX keeps them: type (bits
    /// 0-3), S (4), DPL (5-6), P (7), AVL (12), L (13), D/B (14), G (15),
    /// and bit 16 for a segment that cannot be used.
    pub access: u32,
    pub selector: u16,
    pub _reserved: [u16; 3],
}

/// The registers of a virtual processor (`vcpu_create`,
/// `vcpu_read_state`).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VcpuState {
    /// rax, rcx, rdx, rbx, rsp, rbp, rsi, rdi, r8 to r15: the order in
    /// which instructions number them.
    pub gprs: [u64; 16],
    pub rip: u64,
    pub rflags: u64,
    pub cr0: u64,
    pub cr2: u64,
    pub cr3: u64,
    pub cr4: u64,
    pub efer: u64,
    pub cs: VcpuSegment,
    pub ds: VcpuSegment,
    pub es: VcpuSegment,
    pub fs: VcpuSegment,
    pub gs: VcpuSegment,
    pub ss: VcpuSegment,
    pub tr: VcpuSegment,
    pub ldtr: VcpuSegment,
    pub gdt_base: u64,
    pub idt_base: u64,
    pub gdt_limit: u32,
    pub idt_limit: u32,
}

/// Reasons of a [`VcpuExit`].
pub mod vcpu_exit {
    /// The guest made a hypercall (`vmcall`): `data` holds its rax (the
    /// call), rbx, rcx, rdx, rsi and rdi. The next `vcpu_run` gives the
    /// guest `data[0]` in rax.
    pub const HYPERCALL: u32 = 1;
    /// The guest read or wrote an I/O port: `data[0]` is the port,
    /// `data[1]` the size (1, 2 or 4), `data[2]` 1 for a write, `data[3]`
    /// the value written. After a read, the next `vcpu_run` gives the
    /// guest `data[3]`.
    pub const IO: u32 = 2;
    /// The guest reached guest-physical memory that is not mapped (or not
    /// for that access): `data[0]` is the address, `data[1]` the access
    /// (1 read, 2 write, 4 instruction fetch), `data[2]` the guest's rip.
    pub const MEMORY: u32 = 3;
    /// The guest shut its processor down (a triple fault).
    pub const SHUTDOWN: u32 = 4;
    /// The processor would not run the guest, or did something the kernel
    /// does not handle: `data[0]` is the processor's exit reason,
    /// `data[1]` its qualification (or the instruction error of a failed
    /// entry), `data[2]` the guest's rip.
    pub const FAILED: u32 = 5;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_resumed_thread_keeps_only_the_flags_user_code_may_change() {
        // Carry, zero, sign, direction and overflow stay; interrupts come on.
        assert_eq!(resumed_flags(0x0001 | 0x0040 | 0x0080 | 0x0400 | 0x0800), 0x0cc1 | INTERRUPT_FLAG);
        // Single-stepping (TF), I/O privilege, nested tasks, resume,
        // virtual-8086 mode and alignment checks do not.
        assert_eq!(resumed_flags(0x0100 | 0x3000 | 0x4000 | 0x1_0000 | 0x2_0000 | 0x4_0000), INTERRUPT_FLAG);
        // Interrupts are on whatever is asked.
        assert_eq!(resumed_flags(0), INTERRUPT_FLAG);
    }

    #[test]
    fn signal_frames_leave_the_red_zone_and_are_aligned() {
        for sp in [0x7fff_0000_1000, 0x7fff_0000_1008, 0x7fff_0000_0ff0, 0x5555_0000_0123] {
            let frame = signal_frame(sp);
            assert_eq!(frame % 64, 0, "{sp:#x}");
            // The frame ends below the 128 bytes the interrupted code may
            // use under its stack pointer, and not far below.
            let end = frame + core::mem::size_of::<ThreadState>();
            assert!(end <= sp - 128 && sp - 128 - end < 64, "{sp:#x}");
            // The entry's stack pointer is as on entry to a function.
            assert_eq!(entry_stack_pointer(frame) % 16, 8);
        }
    }
}
