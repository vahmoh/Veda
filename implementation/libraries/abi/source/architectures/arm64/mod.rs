// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The kernel ABI on ARM64: how system calls are made, the user address
//! space and its pages, how threads start, and virtual processors.
//!
//! # Calling convention
//!
//! System calls use the `svc #0` instruction. `x8` holds the call number
//! (see [`nr`](crate::nr)), arguments are passed in `x0` to `x5`. On return
//! `x0` holds a status: non-negative values are success results, negative
//! values are `-(error code)` (see [`Error`](crate::Error)). Some calls
//! return a second value in `x1`. All other registers are preserved.
//!
//! # Threads
//!
//! A thread starts at its entry point with its two arguments in `x0` and
//! `x1`, and the stack pointer [`entry_stack_pointer`] gives. Its thread
//! pointer (`thread_set_tls`) is `TPIDR_EL0`: it sets it through that call,
//! for the kernel to know it, as signals find the thread by it. The clock's
//! counter ([`ClockInfo`](crate::ClockInfo)) is the
//! generic timer's virtual count (`CNTVCT_EL0`), at the frequency
//! `ClockInfo` gives.
//!
//! # Signal entry
//!
//! An interrupted thread (`thread_interrupt`) goes to its process's signal
//! entry (`signal_entry_set`) as it next returns to user code, with its
//! user registers as it left that code in a [`ThreadState`] on its stack:
//! 64-byte aligned, below its stack pointer (the AAPCS64 keeps nothing
//! below that). `x0` holds its address, which is also the stack pointer.
//! The other registers are those of the code interrupted. The floating
//! point and SIMD registers are the code's: the entry keeps them, and
//! `signal_return` resumes the registers it is given. Of the flags,
//! `signal_return` takes the condition flags ([`SIGNAL_RETURN_FLAGS`]).

#[cfg(target_arch = "aarch64")]
pub mod barrier;
pub mod hwcap;

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
/// top being `top` (16-byte aligned): the AAPCS64 keeps the stack pointer
/// 16-byte aligned at all times, on entry to a function too.
pub const fn entry_stack_pointer(top: usize) -> usize {
    top
}

/// The user registers of a stopped thread (`thread_read_state`), as it
/// left user code. A thread stopped in a system call has the call's
/// result in `x0` once it has finished it.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ThreadState {
    /// `x0` to `x30`.
    pub x: [u64; 31],
    pub sp: u64,
    pub pc: u64,
    /// The condition flags and the rest of the processor's state
    /// (`SPSR_EL1`).
    pub pstate: u64,
    /// The thread pointer: `TPIDR_EL0`.
    pub tpidr: u64,
}

/// A thread's foreign code (`foreign_run`, see the crate's "Foreign
/// code"): the registers it runs from and leaves, and what made it leave.
/// The floating point and SIMD registers are not in it: foreign code and
/// its host share them, and a host that keeps the foreign code's (built
/// without them) needs nothing done.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ForeignState {
    /// Its registers, with its thread pointer (`tpidr`). A system call
    /// leaves `pc` after the `svc` instruction.
    pub registers: ThreadState,
    /// The exception it took (`foreign_exit::EXCEPTION`): its syndrome
    /// (`ESR_EL1`)...
    pub syndrome: u64,
    /// ... and the address it faulted at (`FAR_EL1`), for an exception
    /// that has one (an abort, a misaligned access, a watchpoint).
    pub fault_address: u64,
}

/// Where an interrupted thread's [`ThreadState`] goes, its stack pointer
/// being `sp` as it left user code: below it, 64-byte aligned (see "Signal
/// entry").
pub const fn signal_frame(sp: usize) -> usize {
    sp.wrapping_sub(core::mem::size_of::<ThreadState>()) & !63
}

/// The flags `signal_return` takes from the registers it resumes: N, Z, C
/// and V. The rest it sets as they are in user code ([`resumed_flags`]).
pub const SIGNAL_RETURN_FLAGS: u64 = 0xF000_0000;

/// The flags `signal_return` resumes a thread with, given `flags`: those
/// user code may change, in EL0 with exceptions unmasked (no
/// single-stepping or other exception level).
pub const fn resumed_flags(flags: u64) -> u64 {
    flags & SIGNAL_RETURN_FLAGS
}

/// The registers of a virtual processor (`vcpu_create`,
/// `vcpu_read_state`): the guest kernel's, which are all that starts it
/// and all that says where it is.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VcpuState {
    /// `x0` to `x30`.
    pub x: [u64; 31],
    /// The stack pointers of EL0 and EL1.
    pub sp_el0: u64,
    pub sp_el1: u64,
    pub pc: u64,
    /// The processor's state as it would return to the guest
    /// (`SPSR_EL2`): which exception level, what is masked.
    pub pstate: u64,
    /// The guest's translation: `SCTLR_EL1` (0: what the processor resets
    /// to, the MMU off), `TCR_EL1`, `TTBR0_EL1`, `TTBR1_EL1`.
    pub sctlr: u64,
    pub tcr: u64,
    pub ttbr0: u64,
    pub ttbr1: u64,
    /// Where its exceptions go (`VBAR_EL1`), and what it last faulted on
    /// (`ESR_EL1`, `FAR_EL1`).
    pub vbar: u64,
    pub esr: u64,
    pub far: u64,
}

/// Reasons of a [`VcpuExit`](crate::VcpuExit).
pub mod vcpu_exit {
    /// The guest made a hypercall (`hvc #0`, one of Veda's platform, not
    /// of Arm's standard calls, which the kernel answers): `data` holds its
    /// `x0` (the call), `x1`, `x2`, `x3`, `x4` and `x5`. The next
    /// `vcpu_run` gives the guest `data[0]` in `x0`.
    pub const HYPERCALL: u32 = 1;
    /// Never on ARM64, whose processors have no I/O ports.
    pub const IO: u32 = 2;
    /// The guest reached guest-physical memory that is not mapped (or not
    /// for that access): `data[0]` is the address, `data[1]` the access
    /// (1 read, 2 write, 4 instruction fetch), `data[2]` the guest's pc.
    pub const MEMORY: u32 = 3;
    /// The guest shut its processor down (a PSCI `SYSTEM_OFF` or
    /// `SYSTEM_RESET` it has no right to carry out itself).
    pub const SHUTDOWN: u32 = 4;
    /// The processor would not run the guest, or did something the kernel
    /// does not handle: `data[0]` is the exception's syndrome (`ESR_EL2`),
    /// `data[1]` the faulting address, `data[2]` the guest's pc.
    pub const FAILED: u32 = 5;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_resumed_thread_keeps_only_the_flags_user_code_may_change() {
        // The condition flags stay.
        assert_eq!(resumed_flags(0x6000_0000), 0x6000_0000);
        // Another exception level, masked interrupts and single-stepping
        // do not.
        assert_eq!(resumed_flags(0x3C5), 0);
        assert_eq!(resumed_flags(1 << 21), 0);
    }

    #[test]
    fn signal_frames_are_below_the_stack_pointer_and_aligned() {
        for sp in [0x7fff_0000_1000, 0x7fff_0000_1010, 0x7fff_0000_0ff0, 0x5555_0000_0120] {
            let frame = signal_frame(sp);
            assert_eq!(frame % 64, 0, "{sp:#x}");
            let end = frame + core::mem::size_of::<ThreadState>();
            assert!(end <= sp && sp - end < 64, "{sp:#x}");
            assert_eq!(entry_stack_pointer(frame) % 16, 0);
        }
    }
}
