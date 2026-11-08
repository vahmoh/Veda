// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What each exception of user code is as a signal on AArch64, as Linux
//! raises it (`arch/arm64/kernel/traps.c`, `mm/fault.c`, `entry-common.c`):
//! by the exception's class in its syndrome (`ESR_EL1.EC`), and for an
//! abort by its status code (`DFSC`, `IFSC`) too. A bad access is
//! `SIGSEGV` (`SEGV_MAPERR` where the program has no area, `SEGV_ACCERR`
//! where it has one that does not allow the access), a misaligned one
//! `SIGBUS`, an invalid instruction (`UDF` among them) `SIGILL`, a
//! breakpoint (`BRK`) `SIGTRAP`, a trapped floating-point exception
//! `SIGFPE`; what Linux cannot recover from either (a walk of the tables
//! that failed) ends the process (`SIGKILL`).
//!
//! It is plain data, so the host's tests check it whatever the host is.

use crate::linux::{si, sig};

/// The exception classes (`ESR_EL1.EC`) of user code that Linux tells
/// apart.
mod class {
    pub const FPAC: u64 = 0x1C;
    pub const INSTRUCTION_ABORT: u64 = 0x20;
    pub const PC_ALIGNMENT: u64 = 0x22;
    pub const DATA_ABORT: u64 = 0x24;
    pub const SP_ALIGNMENT: u64 = 0x26;
    pub const FP_EXCEPTION: u64 = 0x2C;
    pub const BREAKPOINT: u64 = 0x30;
    pub const SOFTWARE_STEP: u64 = 0x32;
    pub const WATCHPOINT: u64 = 0x34;
    pub const BRK: u64 = 0x3C;
}

/// What an exception is as a signal: which, how (`si_code`), where
/// (`si_addr`), and what the frame's context says of it: the syndrome
/// (0: none to report) and the address (`fault_address`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Raised {
    pub signal: u32,
    pub code: i32,
    pub addr: u64,
    pub syndrome: u64,
    pub address: u64,
}

/// The signal for the exception of syndrome `syndrome`, at `far` (the
/// fault address register, 0 where it says nothing), taken by code at `pc`
/// with stack pointer `sp`; `held` says whether the program has an area at
/// an address (`crate::memory::holds`), as Linux looks for one.
pub fn raised(syndrome: u64, far: u64, pc: u64, sp: u64, held: impl Fn(u64) -> bool) -> Raised {
    let at = |signal, code, addr| Raised { signal, code, addr, syndrome, address: 0 };
    match (syndrome >> 26) & 0x3F {
        class::DATA_ABORT | class::INSTRUCTION_ABORT => abort(syndrome, far, held),
        class::PC_ALIGNMENT => at(sig::BUS, si::BUS_ADRALN, far),
        class::SP_ALIGNMENT => at(sig::BUS, si::BUS_ADRALN, sp),
        class::BRK => at(sig::TRAP, si::TRAP_BRKPT, pc),
        class::BREAKPOINT => at(sig::TRAP, si::TRAP_HWBKPT, pc),
        class::SOFTWARE_STEP => at(sig::TRAP, si::TRAP_TRACE, pc),
        class::WATCHPOINT => at(sig::TRAP, si::TRAP_HWBKPT, far),
        class::FP_EXCEPTION => at(sig::FPE, floating_point(syndrome), pc),
        // A pointer that failed its authentication.
        class::FPAC => at(sig::ILL, si::ILL_ILLOPN, pc),
        // An undefined instruction (class 0: `UDF`, and every encoding
        // that is none), and what else user code cannot do: waiting for an
        // interrupt, an instruction or register it has no access to, SVE
        // or SME, an illegal state.
        _ => at(sig::ILL, si::ILL_ILLOPC, pc),
    }
}

/// An abort's signal, by its status code: `si_addr` the address it
/// faulted at, which the context reports too. One at an address that is
/// no user code's (the kernel's) is reported as Linux reports it, as a
/// translation fault of level 0 with the rest of its syndrome cleared, so
/// that it says nothing of what is there.
fn abort(syndrome: u64, far: u64, held: impl Fn(u64) -> bool) -> Raised {
    /// The syndrome's class, instruction length, cache maintenance and
    /// write bits, and its status code: a translation fault of level 0.
    const KEPT: u64 = 0x3F << 26 | 1 << 25 | 1 << 8 | 1 << 6;
    const TRANSLATION_LEVEL_0: u64 = 0x04;
    let user = far < abi::arm64::USER_SPACE_END as u64;
    let (signal, code) = match syndrome & 0x3F {
        // Translation, access flag and permission faults of any level:
        // nothing there, or something the access is not for.
        0x04..=0x0F if user && held(far) => (sig::SEGV, si::SEGV_ACCERR),
        0x04..=0x0F => (sig::SEGV, si::SEGV_MAPERR),
        // A synchronous external abort, or a parity or ECC error.
        0x10 | 0x18 => (sig::BUS, si::BUS_OBJERR),
        0x11 => (sig::SEGV, si::SEGV_MTESERR),
        0x21 => (sig::BUS, si::BUS_ADRALN),
        // Sizes of addresses, table walks, conflicts in the TLB.
        _ => (sig::KILL, si::KERNEL),
    };
    let syndrome = if user {
        syndrome
    } else if syndrome >> 26 & 0x3F == class::DATA_ABORT {
        syndrome & KEPT | TRANSLATION_LEVEL_0
    } else {
        syndrome & (0x3F << 26 | 1 << 25) | TRANSLATION_LEVEL_0
    };
    Raised { signal, code, addr: far, syndrome, address: far }
}

/// The `si_code` of a trapped floating-point exception, by what its
/// syndrome says it was (`TFV`, then which: invalid operation, division by
/// zero, overflow, underflow, inexact), `FPE_FLTUNK` where it does not say.
fn floating_point(syndrome: u64) -> i32 {
    const TFV: u64 = 1 << 23;
    if syndrome & TFV == 0 {
        return si::FPE_FLTUNK;
    }
    [si::FPE_FLTINV, si::FPE_FLTDIV, si::FPE_FLTOVF, si::FPE_FLTUND, si::FPE_FLTRES]
        .into_iter()
        .enumerate()
        .find(|&(bit, _)| syndrome & 1 << bit != 0)
        .map_or(si::FPE_FLTUNK, |(_, code)| code)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A syndrome of class `class` and status code (or other syndrome bits)
    /// `iss`, of a 32-bit instruction.
    fn esr(class: u64, iss: u64) -> u64 {
        class << 26 | 1 << 25 | iss
    }

    const PC: u64 = 0x40_1000;
    const SP: u64 = 0x7fff_f000;

    fn raise(syndrome: u64, far: u64) -> Raised {
        raised(syndrome, far, PC, SP, |at| (0x10_0000..0x20_0000).contains(&at))
    }

    #[test]
    fn bad_accesses_are_sigsegv_where_they_were() {
        // A write where nothing is (a translation fault of level 3), and
        // where the program's area does not allow it (a permission fault).
        let write = esr(class::DATA_ABORT, 1 << 6 | 0x07);
        assert_eq!(
            raise(write, 0x7000),
            Raised { signal: 11, code: 1, addr: 0x7000, syndrome: write, address: 0x7000 }
        );
        let denied = esr(class::DATA_ABORT, 1 << 6 | 0x0F);
        assert_eq!(raise(denied, 0x10_0008).code, 2);
        // A translation fault in an area (one mapped with no access at
        // all) is one of access, as Linux finds the area.
        assert_eq!(raise(write, 0x10_0008).code, 2);
        // Code that is not there, or not executable.
        let fetch = raise(esr(class::INSTRUCTION_ABORT, 0x06), 0x9000);
        assert_eq!((fetch.signal, fetch.code, fetch.addr), (11, 1, 0x9000));
    }

    #[test]
    fn the_kernel_s_addresses_say_nothing() {
        // Whatever is there, a translation fault of level 0, the access's
        // direction kept.
        let denied = esr(class::DATA_ABORT, 1 << 24 | 0x1F << 16 | 1 << 6 | 0x0D);
        let r = raise(denied, 0xffff_8000_0000_1000);
        assert_eq!((r.signal, r.code, r.addr), (11, 1, 0xffff_8000_0000_1000));
        assert_eq!(r.syndrome, esr(class::DATA_ABORT, 1 << 6 | 0x04));
        let fetch = esr(class::INSTRUCTION_ABORT, 0x0F);
        assert_eq!(raise(fetch, 0xffff_0000_0000_0000).syndrome, esr(class::INSTRUCTION_ABORT, 0x04));
    }

    #[test]
    fn misalignment_is_sigbus() {
        // An exclusive access to a misaligned address.
        let r = raise(esr(class::DATA_ABORT, 0x21), 0x10_0003);
        assert_eq!((r.signal, r.code, r.addr, r.address), (7, 1, 0x10_0003, 0x10_0003));
        // The program counter and the stack pointer: where they are, and
        // no address in the context.
        let r = raise(esr(class::PC_ALIGNMENT, 0), 0x40_1002);
        assert_eq!((r.signal, r.code, r.addr, r.address), (7, 1, 0x40_1002, 0));
        let r = raise(esr(class::SP_ALIGNMENT, 0), 0);
        assert_eq!((r.signal, r.code, r.addr), (7, 1, SP));
        // A synchronous external abort.
        assert_eq!(raise(esr(class::DATA_ABORT, 0x10), 0x10_0000).code, 3);
    }

    #[test]
    fn instructions_are_sigill_and_breakpoints_sigtrap() {
        // `UDF`, at the instruction, its syndrome reported.
        let udf = esr(0x00, 0);
        assert_eq!(raise(udf, 0), Raised { signal: 4, code: 1, addr: PC, syndrome: udf, address: 0 });
        // What user code may not do: `WFI` trapped, a system register.
        for class in [0x01, 0x0E, 0x18, 0x19] {
            let r = raise(esr(class, 0), 0);
            assert_eq!((r.signal, r.code), (4, 1), "{class:#x}");
        }
        assert_eq!(raise(esr(class::FPAC, 0), 0).code, 2);
        // `BRK`, whatever its number.
        let brk = esr(class::BRK, 0x3E8);
        assert_eq!(raise(brk, 0), Raised { signal: 5, code: 1, addr: PC, syndrome: brk, address: 0 });
        let watched = raise(esr(class::WATCHPOINT, 0), 0x5000);
        assert_eq!((watched.code, watched.addr), (4, 0x5000));
    }

    #[test]
    fn floating_point_exceptions_say_which() {
        let fpe = |iss| raise(esr(class::FP_EXCEPTION, iss), 0);
        let by_zero = fpe(1 << 23 | 1 << 1);
        assert_eq!((by_zero.signal, by_zero.code, by_zero.addr), (8, 3, PC));
        assert_eq!(fpe(1 << 23 | 1).code, 7);
        assert_eq!(fpe(1 << 23 | 1 << 4).code, 6);
        assert_eq!(fpe(1 << 1).code, 14);
    }

    #[test]
    fn what_cannot_be_recovered_from_ends_the_process() {
        for status in [0x00, 0x03, 0x13, 0x14, 0x17, 0x30, 0x31] {
            assert_eq!(raise(esr(class::DATA_ABORT, status), 0x10_0000).signal, 9, "{status:#x}");
        }
    }
}
