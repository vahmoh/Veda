// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Linux's AArch64 ABI, as a Linux program's code has it: where a system
//! call's number, arguments and result are (the generic table's numbers,
//! `x8`, `x0` to `x5`), how a thread starts, Linux's signal frame
//! ([`frame`]), what each exception is as a signal ([`fault`]), the
//! floating-point and SIMD registers, which the frame keeps, the vDSO's
//! code ([`vdso`]), which has the handlers' return too, and what Linux
//! says of the processor ([`cpu`]).

mod cpu;
mod fault;
pub mod frame;
pub mod vdso;

use alloc::string::String;

use abi::ForeignState;

use crate::signal::{AltStack, Fault, Frame};
use crate::task::Restart;

/// Linux's name for the processor (`uname`'s machine, `AT_PLATFORM`).
pub const MACHINE: &str = "aarch64";

/// The bytes of the instruction a system call is made with (`svc #0`),
/// which a call made again (`SA_RESTART`) goes back over.
const SVC_LEN: u64 = 4;

/// The system call's number and arguments.
pub fn syscall(s: &ForeignState) -> (usize, [usize; 6]) {
    let x = &s.registers.x;
    (x[8] as usize, [x[0], x[1], x[2], x[3], x[4], x[5]].map(|v| v as usize))
}

/// Returns `value` from the system call.
pub fn set_result(s: &mut ForeignState, value: isize) {
    s.registers.x[0] = value as u64;
}

/// What the system call returned (or is to return).
pub fn result(s: &ForeignState) -> isize {
    s.registers.x[0] as isize
}

/// Has the call `r` made again: back over its `svc` instruction, its
/// first argument in `x0` again (its result's register; the number, in
/// `x8`, is kept).
pub fn restart(s: &mut ForeignState, r: &Restart) {
    s.registers.pc = s.registers.pc.wrapping_sub(SVC_LEN);
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

/// The registers a program starts with: at `entry`, its stack pointer at
/// `sp` (`argc`), every other register zero (`x0`, a function for
/// `atexit`, none).
pub fn start(entry: u64, sp: u64) -> ForeignState {
    let mut s = ForeignState::default();
    s.registers.pc = entry;
    s.registers.sp = sp;
    s
}

/// The registers of a thread `clone` starts, from its parent's as the call
/// left them: it returns 0, on stack `sp` (if not 0), with thread pointer
/// `tls` (if given).
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

/// What a frame's context says of the exception a fault was: its syndrome
/// (`ESR_EL1`, in the context's `esr_context` record; 0, none) and the
/// address it reports (`fault_address`: an abort's, else 0).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Exception {
    pub syndrome: u64,
    pub address: u64,
}

/// The signal a fault stands for, as Linux raises it ([`fault`]): which,
/// how (`si_code`) and where (`si_addr`), as the program's areas say for a
/// bad access.
pub fn fault(s: &ForeignState) -> Fault {
    let r = &s.registers;
    let raised = fault::raised(s.syndrome, s.fault_address, r.pc, r.sp, crate::memory::holds);
    let exception = Exception { syndrome: raised.syndrome, address: raised.address };
    Fault { signal: raised.signal, code: raised.code, addr: raised.addr, exception }
}

/// The fault registers no code can have are (a frame's that `rt_sigreturn`
/// took back, whose return the kernel refuses): where the processor would
/// have faulted on its way back, as on Linux, `SIGSEGV` at its program
/// counter or, if that is user code's, its stack pointer.
pub fn refused(s: &ForeignState) -> Fault {
    use crate::linux::{si, sig};
    let r = &s.registers;
    let user = abi::USER_SPACE_START as u64..abi::USER_SPACE_END as u64;
    let addr = if user.contains(&r.pc) { r.sp } else { r.pc };
    Fault { signal: sig::SEGV, code: si::SEGV_MAPERR, addr, exception: Exception { syndrome: 0, address: addr } }
}

/// ARM64's Linux has no legacy virtual system calls.
pub fn vsyscall(_s: &ForeignState) -> Option<usize> {
    None
}

pub fn vsyscall_return(_s: &mut ForeignState, _result: isize) -> bool {
    false
}

/// Lays out a signal frame for a handler of `frame.signal` below the
/// stack pointer (AArch64 has no red zone), or at the top of the alternate
/// stack `stack` if it is to run there, and sets the registers to enter
/// the handler, as Linux's `setup_rt_frame` does: its signal in `x0` (and
/// what the signal says, and the context, in `x1` and `x2` for an
/// `SA_SIGINFO` handler), its frame pointer at the frame record, its link
/// register at the restorer. The frame keeps `s` as the code it
/// interrupted leaves it, and the vector registers as they are, which the
/// handler starts with too. `None` if the stack cannot take the frame
/// (unmapped, or the alternate stack too small): the thread is then sent
/// `SIGSEGV`, as on Linux.
pub fn push_frame(s: &mut ForeignState, frame: &Frame, stack: &AltStack) -> Option<()> {
    let regs = s.registers;
    let top = stack.top_for(regs.sp, frame.onstack);
    // The frame record, then the frame below it, each 16-byte aligned.
    let record = top.unwrap_or(regs.sp).checked_sub(frame::RECORD as u64)? & !15;
    let at = record.checked_sub(frame::SIZE as u64)?;
    if !stack.fits(at, regs.sp, top.is_some()) {
        return None;
    }
    let vector = fpu::saved();
    // SAFETY: `Siginfo` is plain data, 128 bytes.
    let info: [u8; 128] = unsafe { core::mem::transmute_copy(&frame.info) };
    let bytes = frame::lay_out(&frame::Interrupted {
        registers: &regs,
        vector: &vector,
        mask: frame.restore_mask,
        stack: stack.reported(regs.sp),
        info: &info,
        fault: frame.fault.map(|f| (f.exception.syndrome, f.exception.address)),
    });
    // SAFETY: the program's stack, where a fault (it is not mapped) ends
    // the copy.
    unsafe { crate::user::write_bytes(at as usize, &bytes)? };
    let r = &mut s.registers;
    r.x[0] = u64::from(frame.info.signo as u32);
    if frame.siginfo {
        r.x[1] = at + frame::INFO as u64;
        r.x[2] = at + frame::UCONTEXT as u64;
    }
    r.sp = at;
    r.x[29] = record;
    r.x[30] = frame.restorer;
    r.pc = frame.handler;
    Some(())
}

/// What `rt_sigreturn` takes back from the frame at the stack pointer (the
/// handler returned to its restorer, which made the call, its stack
/// pointer where it started): the registers, the vector registers, and the
/// mask and alternate stack to restore. `None` for a frame that is not
/// there (the stack pointer is not 16-byte aligned, as the frame is, or
/// the memory cannot be read) or that Linux refuses ([`frame::take_back`]):
/// the thread is then sent `SIGSEGV`.
pub fn pop_frame(s: &mut ForeignState) -> Option<(u64, (u64, i32, u64))> {
    let sp = s.registers.sp;
    if !sp.is_multiple_of(16) {
        return None;
    }
    let mut bytes = [0u8; frame::SIZE];
    // SAFETY: the program's stack, which a fault reading it ends.
    unsafe { crate::user::read_bytes(sp as usize, &mut bytes)? };
    let back = frame::take_back(&bytes)?;
    let r = &mut s.registers;
    (r.x, r.sp, r.pc, r.pstate) = (back.registers.x, back.registers.sp, back.registers.pc, back.registers.pstate);
    fpu::load(&back.vector);
    Some((back.mask, back.stack))
}

/// The floating-point and SIMD registers (`v0` to `v31`, `FPSR`, `FPCR`),
/// which a signal frame keeps: Linux compatibility touches none of them
/// itself (it is built soft-float), so they are the program's as it left
/// them, which these read and write as they are. Veda leaves SVE and SME
/// off, so these are all there are.
pub mod fpu {
    use super::frame::Fpsimd;

    // The target has no such registers; these two touch them, and nothing
    // else.
    core::arch::global_asm!(
        r#"
        .pushsection .text.linux_compatibility_fpu, "ax", %progbits
        .arch_extension fp
        .arch_extension simd

        // linux_compatibility_fpu_save(to: *mut Fpsimd)
        .p2align 4
        .globl linux_compatibility_fpu_save
        .hidden linux_compatibility_fpu_save
        .type linux_compatibility_fpu_save, %function
linux_compatibility_fpu_save:
        stp q0, q1, [x0, #0]
        stp q2, q3, [x0, #32]
        stp q4, q5, [x0, #64]
        stp q6, q7, [x0, #96]
        stp q8, q9, [x0, #128]
        stp q10, q11, [x0, #160]
        stp q12, q13, [x0, #192]
        stp q14, q15, [x0, #224]
        stp q16, q17, [x0, #256]
        stp q18, q19, [x0, #288]
        stp q20, q21, [x0, #320]
        stp q22, q23, [x0, #352]
        stp q24, q25, [x0, #384]
        stp q26, q27, [x0, #416]
        stp q28, q29, [x0, #448]
        stp q30, q31, [x0, #480]
        mrs x1, fpsr
        mrs x2, fpcr
        str w1, [x0, #512]
        str w2, [x0, #516]
        ret
        .size linux_compatibility_fpu_save, . - linux_compatibility_fpu_save

        // linux_compatibility_fpu_load(from: *const Fpsimd)
        .p2align 4
        .globl linux_compatibility_fpu_load
        .hidden linux_compatibility_fpu_load
        .type linux_compatibility_fpu_load, %function
linux_compatibility_fpu_load:
        ldr w1, [x0, #512]
        ldr w2, [x0, #516]
        msr fpsr, x1
        msr fpcr, x2
        ldp q0, q1, [x0, #0]
        ldp q2, q3, [x0, #32]
        ldp q4, q5, [x0, #64]
        ldp q6, q7, [x0, #96]
        ldp q8, q9, [x0, #128]
        ldp q10, q11, [x0, #160]
        ldp q12, q13, [x0, #192]
        ldp q14, q15, [x0, #224]
        ldp q16, q17, [x0, #256]
        ldp q18, q19, [x0, #288]
        ldp q20, q21, [x0, #320]
        ldp q22, q23, [x0, #352]
        ldp q24, q25, [x0, #384]
        ldp q26, q27, [x0, #416]
        ldp q28, q29, [x0, #448]
        ldp q30, q31, [x0, #480]
        ret
        .size linux_compatibility_fpu_load, . - linux_compatibility_fpu_load

        .arch_extension nosimd
        .arch_extension nofp
        .popsection
        "#
    );

    unsafe extern "C" {
        fn linux_compatibility_fpu_save(to: *mut Fpsimd);
        fn linux_compatibility_fpu_load(from: *const Fpsimd);
    }

    /// Nothing to learn: every processor Veda runs on has the same
    /// registers.
    pub fn init() {}

    /// The registers as they are.
    pub fn saved() -> Fpsimd {
        let mut v = Fpsimd::default();
        // SAFETY: the registers, into memory of their layout, 16-byte
        // aligned.
        unsafe { linux_compatibility_fpu_save(&mut v) };
        v
    }

    /// Has the program's registers be `v`: which any values are, of every
    /// register (a reserved bit of `FPSR` or `FPCR` set is ignored, as
    /// Linux leaves it).
    pub fn load(v: &Fpsimd) {
        // SAFETY: registers no code of this program's uses, from memory of
        // their layout.
        unsafe { linux_compatibility_fpu_load(v) };
    }

    /// Has them be as a new program starts with them: all zero, rounding
    /// to nearest, every exception masked.
    pub fn reset() {
        load(&Fpsimd::default());
    }
}

/// What `AT_HWCAP` and `AT_HWCAP2` say of the processor, as Linux has them
/// on AArch64: the kernel's word, as programs cannot read the ID registers
/// (`abi::arm64::hwcap`), or without it the two every processor Veda runs
/// on has (`HWCAP_FP`, `HWCAP_ASIMD`).
pub fn hwcap() -> [u64; 2] {
    runtime::object::system_info().map_or([0b11, 0], |info| info.hwcap)
}

/// The registers as `/proc/self/stat` and crash reports name them: the
/// instruction and stack pointers.
pub fn describe(s: &ForeignState) -> (u64, u64) {
    (s.registers.pc, s.registers.sp)
}

/// What `/proc/cpuinfo` says of processor `cpu`, as Linux says it on
/// AArch64 ([`cpu`]): its number, its speed as Linux reckons it from the
/// generic timer's frequency, and its features.
pub fn cpuinfo(cpu: u32, _cpus: u32, _brand: &str) -> String {
    cpu::entry(cpu, runtime::time::scale().map_or(0, |s| s.ticks_hz), hwcap())
}

/// `struct stat` of Linux's generic ABI, which AArch64 has, for a file of
/// Linux compatibility's own (`crate::proc`): its type and permissions,
/// and its size, owned by the user, now.
pub fn stat_bytes(mode: u32, size: u64) -> [u8; 128] {
    let mut b = [0u8; 128];
    let now = runtime::time::utc_time_ns() / 1_000_000_000;
    b[16..20].copy_from_slice(&mode.to_le_bytes());
    b[20..24].copy_from_slice(&1u32.to_le_bytes());
    b[24..28].copy_from_slice(&1000u32.to_le_bytes());
    b[28..32].copy_from_slice(&1000u32.to_le_bytes());
    b[48..56].copy_from_slice(&size.to_le_bytes());
    b[56..60].copy_from_slice(&4096u32.to_le_bytes());
    for at in [72, 88, 104] {
        b[at..at + 8].copy_from_slice(&now.to_le_bytes());
    }
    b
}
