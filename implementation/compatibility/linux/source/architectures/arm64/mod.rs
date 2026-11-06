// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Linux's AArch64 ABI. Linux programs for ARM64 are not run yet: what
//! the rest of Linux compatibility asks of the architecture is here, and
//! each says so (see `documentation/LINUX.md`).

use abi::ForeignState;

use crate::signal::{AltStack, Fault, Frame};
use crate::task::Restart;

pub const MACHINE: &str = "aarch64";

pub fn syscall(s: &ForeignState) -> (usize, [usize; 6]) {
    let x = &s.registers.x;
    (x[8] as usize, [x[0], x[1], x[2], x[3], x[4], x[5]].map(|v| v as usize))
}

pub fn set_result(s: &mut ForeignState, value: isize) {
    s.registers.x[0] = value as u64;
}

pub fn result(s: &ForeignState) -> isize {
    s.registers.x[0] as isize
}

/// Has the call `r` made again: back over its `svc` instruction, its
/// first argument in `x0` again (its result's register; the number, in
/// `x8`, is kept).
pub fn restart(s: &mut ForeignState, r: &Restart) {
    s.registers.pc = s.registers.pc.wrapping_sub(4);
    s.registers.x[0] = r.first as u64;
}

pub fn pc(s: &ForeignState) -> u64 {
    s.registers.pc
}

pub fn sp(s: &ForeignState) -> u64 {
    s.registers.sp
}

pub fn tls(s: &ForeignState) -> u64 {
    s.registers.tpidr
}

pub fn set_tls(s: &mut ForeignState, tls: u64) {
    s.registers.tpidr = tls;
}

pub fn start(entry: u64, sp: u64) -> ForeignState {
    let mut s = ForeignState::default();
    s.registers.pc = entry;
    s.registers.sp = sp;
    s
}

pub fn child(parent: &ForeignState, sp: u64, tls: Option<u64>) -> ForeignState {
    let mut s = *parent;
    s.registers.x[0] = 0;
    if sp != 0 {
        s.registers.sp = sp;
    }
    if let Some(tls) = tls {
        s.registers.tpidr = tls;
    }
    s
}

pub fn fault(s: &ForeignState) -> Fault {
    use crate::linux::{si, sig};
    Fault { signal: sig::SEGV, code: si::KERNEL, addr: s.fault_address, trap: s.syndrome >> 26, error: s.syndrome }
}

pub fn vsyscall(_s: &ForeignState) -> Option<usize> {
    None
}

pub fn vsyscall_return(_s: &mut ForeignState, _result: isize) -> bool {
    false
}

/// No frame is laid out for ARM64's handlers yet: the thread is sent
/// `SIGSEGV`, as for a stack that cannot take one.
pub fn push_frame(_s: &mut ForeignState, _frame: &Frame, _stack: &AltStack) -> Option<()> {
    None
}

pub fn pop_frame(_s: &mut ForeignState) -> Option<(u64, (u64, i32, u64))> {
    None
}

pub mod fpu {
    pub fn init() {}
}

/// The vDSO's code (`crate::vdso`): none, as ARM64's Linux programs do
/// not run yet. Linux's for ARM64 has `__kernel_clock_gettime` and the
/// rest, of version `LINUX_2.6.39`.
pub mod vdso {
    pub fn code() -> Option<crate::vdso::Code> {
        None
    }
}

pub fn hwcap() -> [u64; 2] {
    [0, 0]
}

pub fn describe(s: &ForeignState) -> (u64, u64) {
    (s.registers.pc, s.registers.sp)
}

pub fn cpu_flags() -> alloc::string::String {
    alloc::string::String::from("fp asimd")
}

/// `struct stat` of Linux's generic ABI, which AArch64 has.
pub fn stat_bytes(mode: u32, size: u64) -> [u8; 128] {
    let mut b = [0u8; 128];
    b[16..20].copy_from_slice(&mode.to_le_bytes());
    b[20..24].copy_from_slice(&1u32.to_le_bytes());
    b[24..28].copy_from_slice(&1000u32.to_le_bytes());
    b[28..32].copy_from_slice(&1000u32.to_le_bytes());
    b[48..56].copy_from_slice(&size.to_le_bytes());
    b[56..60].copy_from_slice(&4096u32.to_le_bytes());
    b
}
