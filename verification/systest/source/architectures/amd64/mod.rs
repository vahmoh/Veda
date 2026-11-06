// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What `systest` does on AMD64 that is the processor's own.

/// Faults on purpose, with an invalid-opcode exception (`ud2`): the
/// kernel ends the process.
pub fn crash() -> ! {
    // SAFETY: `ud2` raises the exception and nothing runs after it.
    unsafe { core::arch::asm!("ud2", options(noreturn)) }
}

/// Foreign code for the tests of `foreign_run` (`crate::foreign`): machine
/// code, and where its registers are.
pub mod foreign {
    use abi::ForeignState;

    /// Makes system call 39 (`mov eax, 39; syscall`).
    pub const SYSCALL: &[u8] = &[0xb8, 0x27, 0x00, 0x00, 0x00, 0x0f, 0x05];
    /// Reads the word 8 bytes into its thread-local storage, then makes a
    /// system call (`mov rax, fs:[8]; syscall`).
    pub const READ_TLS: &[u8] = &[0x64, 0x48, 0x8b, 0x04, 0x25, 0x08, 0x00, 0x00, 0x00, 0x0f, 0x05];
    /// An invalid instruction (`ud2`).
    pub const INVALID: &[u8] = &[0x0f, 0x0b];
    /// Reads address 0x1000, where nothing is mapped (`mov rax, [0x1000]`).
    pub const FAULT: &[u8] = &[0x48, 0x8b, 0x04, 0x25, 0x00, 0x10, 0x00, 0x00];
    /// Where [`FAULT`] faults, from its start.
    pub const FAULT_AT: usize = 0;
    /// Loops for ever (`jmp .`).
    pub const SPIN: &[u8] = &[0xeb, 0xfe];

    /// Foreign code that starts at `pc` with the stack pointer `sp`, the
    /// thread pointer `tls`, and `kept` in a register it keeps.
    pub fn start(pc: usize, sp: usize, tls: usize, kept: u64) -> ForeignState {
        let mut s = ForeignState::default();
        (s.registers.rip, s.registers.rsp, s.registers.fs_base, s.registers.rbx) =
            (pc as u64, sp as u64, tls as u64, kept);
        s
    }

    pub fn pc(s: &ForeignState) -> usize {
        s.registers.rip as usize
    }

    pub fn tls(s: &ForeignState) -> usize {
        s.registers.fs_base as usize
    }

    /// The register [`start`] put `kept` in.
    pub fn kept(s: &ForeignState) -> u64 {
        s.registers.rbx
    }

    /// The system call's number (`rax`).
    pub fn number(s: &ForeignState) -> u64 {
        s.registers.rax
    }

    /// The word [`READ_TLS`] read (`rax`, its system call's number).
    pub fn loaded(s: &ForeignState) -> u64 {
        s.registers.rax
    }

    /// Whether it stopped at an invalid instruction (`#UD`).
    pub fn invalid_instruction(s: &ForeignState) -> bool {
        s.vector == 6
    }

    /// Where it faulted, if it was a page fault (`#PF`).
    pub fn page_fault(s: &ForeignState) -> Option<u64> {
        (s.vector == 14).then_some(s.fault_address)
    }

    /// Makes code written to memory the code that runs there: nothing to
    /// do on AMD64, whose instruction fetches see what was written.
    pub fn sync_code(_at: usize, _len: usize) {}

    /// The word 8 bytes into the calling thread's thread-local storage.
    pub fn tls_word() -> u64 {
        let w: u64;
        // SAFETY: the caller set a thread pointer to 16 readable bytes.
        unsafe { core::arch::asm!("mov {}, fs:[8]", out(reg) w, options(nostack, readonly)) };
        w
    }

    // A signal entry that defers every interruption: `rdi` holds the
    // registers it resumes.
    core::arch::global_asm!(
        ".global systest_defer_entry",
        "systest_defer_entry:",
        "    mov esi, {defer}",
        "    mov eax, {signal_return}",
        "    syscall",
        "    ud2",
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
