// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What a thread keeps of the processor while another runs: its kernel
//! stack pointer where it was switched away, `TPIDR_EL0` (the thread
//! pointer of its thread-local storage), and its floating-point and SIMD
//! registers. Veda leaves SVE and SME off, so these are all there are.

use alloc::alloc::alloc_zeroed;
use alloc::boxed::Box;
use core::alloc::Layout;
use core::arch::global_asm;

use super::cpu::{read_register, write_register};
use super::entry::{vk_context_switch, vk_kernel_thread_trampoline, vk_thread_trampoline};
use super::trap::TrapFrame;

/// The floating-point and SIMD registers (`v0` to `v31`, `FPCR`, `FPSR`):
/// a thread's, or a virtual processor's.
#[repr(C, align(16))]
pub(in crate::architecture) struct FpuState {
    v: [u128; 32],
    fpcr: u64,
    fpsr: u64,
}

const _: () = assert!(core::mem::size_of::<FpuState>() == 528);

// The kernel's own target has no such registers, as the kernel does not
// touch them; these two do, and nothing else.
global_asm!(
    r#"
    .section .text
    .arch_extension fp
    .arch_extension simd
    .globl vk_fpu_save
    .globl vk_fpu_restore
    .balign 16
vk_fpu_save:
    stp     q0, q1, [x0, #0]
    stp     q2, q3, [x0, #32]
    stp     q4, q5, [x0, #64]
    stp     q6, q7, [x0, #96]
    stp     q8, q9, [x0, #128]
    stp     q10, q11, [x0, #160]
    stp     q12, q13, [x0, #192]
    stp     q14, q15, [x0, #224]
    stp     q16, q17, [x0, #256]
    stp     q18, q19, [x0, #288]
    stp     q20, q21, [x0, #320]
    stp     q22, q23, [x0, #352]
    stp     q24, q25, [x0, #384]
    stp     q26, q27, [x0, #416]
    stp     q28, q29, [x0, #448]
    stp     q30, q31, [x0, #480]
    mrs     x1, fpcr
    mrs     x2, fpsr
    str     x1, [x0, #512]
    str     x2, [x0, #520]
    ret

    .balign 16
vk_fpu_restore:
    ldp     q0, q1, [x0, #0]
    ldp     q2, q3, [x0, #32]
    ldp     q4, q5, [x0, #64]
    ldp     q6, q7, [x0, #96]
    ldp     q8, q9, [x0, #128]
    ldp     q10, q11, [x0, #160]
    ldp     q12, q13, [x0, #192]
    ldp     q14, q15, [x0, #224]
    ldp     q16, q17, [x0, #256]
    ldp     q18, q19, [x0, #288]
    ldp     q20, q21, [x0, #320]
    ldp     q22, q23, [x0, #352]
    ldp     q24, q25, [x0, #384]
    ldp     q26, q27, [x0, #416]
    ldp     q28, q29, [x0, #448]
    ldp     q30, q31, [x0, #480]
    ldr     x1, [x0, #512]
    ldr     x2, [x0, #520]
    msr     fpcr, x1
    msr     fpsr, x2
    ret
    .arch_extension nosimd
    .arch_extension nofp
"#
);

unsafe extern "C" {
    fn vk_fpu_save(state: *mut FpuState);
    fn vk_fpu_restore(state: *const FpuState);
}

impl FpuState {
    /// Registers as a new thread has them: zero, floating-point exceptions
    /// masked, rounding to nearest.
    pub(in crate::architecture) fn new() -> Option<Box<FpuState>> {
        // SAFETY: non-zero size.
        let state = unsafe { alloc_zeroed(Layout::new::<FpuState>()) } as *mut FpuState;
        // SAFETY: all zeroes is a valid state, in memory of its layout.
        (!state.is_null()).then(|| unsafe { Box::from_raw(state) })
    }

    /// Saves the processor's registers here.
    pub(in crate::architecture) fn save(&mut self) {
        // SAFETY: the state is ours, and 16-byte aligned.
        unsafe { vk_fpu_save(self) };
    }

    /// Loads the processor's registers from here.
    pub(in crate::architecture) fn restore(&self) {
        // SAFETY: the state holds a valid saved (or initial) state.
        unsafe { vk_fpu_restore(self) };
    }
}

/// A thread's saved context, touched only by the CPU switching the thread
/// in or out (under the BKL), and by the thread itself.
pub struct Context {
    sp: u64,
    tls: u64,
    /// The registers user code has, for every thread but an idle one.
    fpu: Option<Box<FpuState>>,
}

impl Context {
    /// An empty context; `user_state`: with room for the registers user
    /// code has (every thread but an idle one).
    pub fn new(user_state: bool) -> Option<Context> {
        Some(Context { sp: 0, tls: 0, fpu: if user_state { Some(FpuState::new()?) } else { None } })
    }

    /// Whether the thread can be switched to: its context was prepared, or
    /// saved by a switch away from it.
    pub fn is_ready(&self) -> bool {
        self.sp != 0
    }

    /// Prepares the kernel stack below `stack_top` of a new kernel thread
    /// so that the first switch to it runs `entry(arg)`.
    pub fn prepare_kernel(&mut self, stack_top: u64, entry: fn(u64) -> !, arg: u64) {
        // The frame vk_context_switch pops: x19 to x28, x29, x30.
        let mut frame = [0u64; 12];
        (frame[0], frame[1]) = (entry as usize as u64, arg);
        frame[11] = vk_kernel_thread_trampoline as *const () as u64;
        let sp = stack_top - 16 - size_of_val(&frame) as u64;
        // SAFETY: writing the initial frame into the thread's own fresh stack.
        unsafe { (sp as *mut [u64; 12]).write(frame) };
        self.sp = sp;
    }

    /// Prepares the kernel stack below `stack_top` of a new thread so that
    /// the first switch to it enters user mode at `entry` with the given
    /// stack and arguments.
    pub fn prepare_user(&mut self, stack_top: u64, entry: u64, user_stack: u64, arg0: u64, arg1: u64) {
        let tf_addr = stack_top - core::mem::size_of::<TrapFrame>() as u64;
        let tf = TrapFrame::new_user(entry, user_stack, arg0, arg1);
        let mut frame = [0u64; 12];
        frame[11] = vk_thread_trampoline as *const () as u64;
        let sp = tf_addr - size_of_val(&frame) as u64;
        // SAFETY: the thread has not run yet; its stack is ours.
        unsafe {
            (tf_addr as *mut TrapFrame).write(tf);
            (sp as *mut [u64; 12]).write(frame);
        }
        self.sp = sp;
    }

    /// Saves the running thread's floating-point and SIMD registers (this
    /// is its context) for something else to use the processor's: a
    /// virtual processor's.
    pub(in crate::architecture) fn save_fpu(&mut self) {
        if let Some(f) = &mut self.fpu {
            f.save();
        }
    }

    /// Gives the running thread its registers back ([`Self::save_fpu`]).
    pub(in crate::architecture) fn restore_fpu(&self) {
        if let Some(f) = &self.fpu {
            f.restore();
        }
    }

    /// The thread's thread pointer (0 until it sets one), as it set it
    /// last.
    pub fn tls(&self) -> u64 {
        self.tls
    }

    /// Sets the thread pointer of the running thread, whose context this
    /// is: `TPIDR_EL0`, through which it finds its thread-local storage.
    pub fn set_tls(&mut self, value: u64) {
        self.tls = value;
        // SAFETY: a value for user code to use.
        unsafe { write_register!("tpidr_el0", value) };
    }
}

/// Switches this CPU from the thread whose context is `prev` to the one
/// whose context is `next`. `next_stack_top` is of no use here: the
/// processor enters the kernel on the stack it left, which is the thread's
/// ([`super::entry`]). Returns when a later switch comes back to `prev`.
///
/// # Safety
/// `prev` is the running thread's context and `next` a ready one
/// ([`Context::is_ready`]), both live until the switch has completed; the
/// BKL is held, with interrupts disabled.
pub unsafe fn switch(prev: *mut Context, next: *mut Context, _next_stack_top: u64) {
    // SAFETY: the caller hands over both contexts for the switch. No
    // reference to them outlives this block: once `prev` is switched away
    // from, another CPU may switch to it.
    unsafe {
        if let Some(f) = &mut (*prev).fpu {
            f.save();
            (*prev).tls = read_register!("tpidr_el0");
        }
        if let Some(f) = &(*next).fpu {
            f.restore();
            write_register!("tpidr_el0", (*next).tls);
        }
    }
    // SAFETY: `next`'s stack pointer was prepared by `prepare_*` or saved
    // by a previous switch; the caller keeps `prev` alive meanwhile.
    unsafe { vk_context_switch(&raw mut (*prev).sp, (*next).sp) };
}

/// The user registers of a thread that is not running user code, as its
/// last entry into the kernel saved them (a trap frame at the top of its
/// kernel stack, whose top is `kernel_stack_top`, as a new thread's is
/// prepared), with its thread pointer from `ctx`.
///
/// # Safety
/// The thread must not be running user code (it is in the kernel, or has
/// not started), and its kernel stack must be live.
pub unsafe fn user_state(kernel_stack_top: u64, ctx: &Context) -> abi::ThreadState {
    let at = kernel_stack_top - core::mem::size_of::<TrapFrame>() as u64;
    // SAFETY: per the caller, the frame its entry saved is there, and
    // nothing writes it while it is not running user code (but a system
    // call's result: a word at a time, under the BKL the caller holds).
    let f = unsafe { core::ptr::read(at as *const TrapFrame) };
    abi::ThreadState { x: f.x, sp: f.sp, pc: f.pc, pstate: f.pstate, tpidr: ctx.tls }
}
