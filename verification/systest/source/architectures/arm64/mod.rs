// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What `systest` does on ARM64 that is the processor's own.

/// Faults on purpose, with an undefined instruction (`udf`): the kernel
/// ends the process.
pub fn crash() -> ! {
    // SAFETY: `udf` raises the exception and nothing runs after it.
    unsafe { core::arch::asm!("udf #0", options(noreturn)) }
}

/// Foreign code for the tests of `foreign_run` (`crate::foreign`): machine
/// code, and where its registers are.
pub mod foreign {
    use abi::ForeignState;

    /// The bytes of instructions, in the order they run.
    const fn code<const N: usize, const B: usize>(words: [u32; N]) -> [u8; B] {
        let mut out = [0u8; B];
        let mut i = 0;
        while i < N {
            let w = words[i].to_le_bytes();
            out[4 * i] = w[0];
            out[4 * i + 1] = w[1];
            out[4 * i + 2] = w[2];
            out[4 * i + 3] = w[3];
            i += 1;
        }
        out
    }

    /// Makes system call 39 (`mov x8, #39; svc #0`).
    pub const SYSCALL: &[u8] = &code::<2, 8>([0xD280_04E8, 0xD400_0001]);
    /// Reads the word 8 bytes into its thread-local storage into `x0`, then
    /// makes a system call (`mrs x0, tpidr_el0; ldr x0, [x0, #8]; mov x8,
    /// #39; svc #0`).
    pub const READ_TLS: &[u8] = &code::<4, 16>([0xD53B_D040, 0xF940_0400, 0xD280_04E8, 0xD400_0001]);
    /// An undefined instruction (`udf #0`).
    pub const INVALID: &[u8] = &code::<1, 4>([0x0000_0000]);
    /// Reads address 0x1000, where nothing is mapped (`mov x1, #0x1000;
    /// ldr x0, [x1]`).
    pub const FAULT: &[u8] = &code::<2, 8>([0xD282_0001, 0xF940_0020]);
    /// Where [`FAULT`] faults, from its start.
    pub const FAULT_AT: usize = 4;
    /// Loops for ever (`b .`).
    pub const SPIN: &[u8] = &code::<1, 4>([0x1400_0000]);

    /// Foreign code that starts at `pc` with the stack pointer `sp`, the
    /// thread pointer `tls`, and `kept` in a register it keeps.
    pub fn start(pc: usize, sp: usize, tls: usize, kept: u64) -> ForeignState {
        let mut s = ForeignState::default();
        (s.registers.pc, s.registers.sp, s.registers.tpidr, s.registers.x[19]) =
            (pc as u64, sp as u64, tls as u64, kept);
        s
    }

    pub fn pc(s: &ForeignState) -> usize {
        s.registers.pc as usize
    }

    pub fn tls(s: &ForeignState) -> usize {
        s.registers.tpidr as usize
    }

    /// The register [`start`] put `kept` in.
    pub fn kept(s: &ForeignState) -> u64 {
        s.registers.x[19]
    }

    /// The system call's number (`x8`).
    pub fn number(s: &ForeignState) -> u64 {
        s.registers.x[8]
    }

    /// The word [`READ_TLS`] read (`x0`).
    pub fn loaded(s: &ForeignState) -> u64 {
        s.registers.x[0]
    }

    /// Whether it stopped at an undefined instruction (exception class 0).
    pub fn invalid_instruction(s: &ForeignState) -> bool {
        s.syndrome >> 26 == 0
    }

    /// Where it faulted, if it was a data abort from user code.
    pub fn page_fault(s: &ForeignState) -> Option<u64> {
        (s.syndrome >> 26 & 0x3F == 0x24).then_some(s.fault_address)
    }

    /// Makes code written to memory the code that runs there: its lines
    /// cleaned to the point of unification, and the instruction cache's
    /// copies of them dropped.
    pub fn sync_code(at: usize, len: usize) {
        for line in (at & !63..at + len).step_by(64) {
            // SAFETY: cache maintenance of memory this program maps.
            unsafe { core::arch::asm!("dc cvau, {0}", "dsb ish", "ic ivau, {0}", in(reg) line, options(nostack)) };
        }
        // SAFETY: barriers.
        unsafe { core::arch::asm!("dsb ish", "isb", options(nostack)) };
    }

    /// The word 8 bytes into the calling thread's thread-local storage.
    pub fn tls_word() -> u64 {
        let w: u64;
        // SAFETY: the caller set a thread pointer to 16 readable bytes.
        unsafe { core::arch::asm!("mrs {0}, tpidr_el0", "ldr {0}, [{0}, #8]", out(reg) w, options(nostack, readonly)) };
        w
    }

    // A signal entry that defers every interruption: `x0` holds the
    // registers it resumes.
    core::arch::global_asm!(
        ".global systest_defer_entry",
        "systest_defer_entry:",
        "    mov x1, #{defer}",
        "    mov x8, #{signal_return}",
        "    svc #0",
        "    udf #0",
        defer = const abi::signal_return_flags::DEFER,
        signal_return = const abi::nr::SIGNAL_RETURN,
    );

    /// A signal entry that defers every interruption it is sent with
    /// (`signal_return_flags::DEFER`).
    pub fn defer_entry() -> usize {
        unsafe extern "C" {
            fn systest_defer_entry();
        }
        systest_defer_entry as *const () as usize
    }
}
