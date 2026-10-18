// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What a thread keeps of the processor while another runs: its kernel
//! stack pointer where it was switched away, its FS base (the thread
//! pointer of its thread-local storage), and its x87, SSE and AVX
//! registers (XSAVE).

use alloc::alloc::{alloc_zeroed, dealloc};
use core::alloc::Layout;

use super::cpu::{self, MSR_FS_BASE, rdmsr, wrmsr};
use super::entry::{vk_context_switch, vk_kernel_thread_trampoline, vk_thread_trampoline};
use super::percpu;
use super::trap::TrapFrame;

/// FPU/SSE/AVX register save area (64-byte aligned for XSAVE): a thread's,
/// or a virtual processor's.
pub(in crate::arch) struct FpuArea {
    ptr: *mut u8,
    layout: Layout,
}

impl FpuArea {
    pub(in crate::arch) fn new() -> Option<FpuArea> {
        let size = cpu::features().xsave_size.max(512) as usize;
        let layout = Layout::from_size_align(size, 64).ok()?;
        // SAFETY: non-zero size.
        let ptr = unsafe { alloc_zeroed(layout) };
        if ptr.is_null() {
            return None;
        }
        // Default x87 control word and MXCSR (all exceptions masked). With
        // XSTATE_BV = 0 everything else starts in its initial state.
        // SAFETY: inside the freshly allocated area.
        unsafe {
            (ptr as *mut u16).write(0x037F);
            (ptr.add(24) as *mut u32).write(0x1F80);
        }
        Some(FpuArea { ptr, layout })
    }

    /// Saves the processor's registers here.
    pub(in crate::arch) fn save(&self) {
        let f = cpu::features();
        // SAFETY: the area is large enough and 64-byte aligned.
        unsafe {
            if f.xsave {
                core::arch::asm!("xsave64 [{}]", in(reg) self.ptr, in("eax") u32::MAX, in("edx") u32::MAX, options(nostack));
            } else {
                core::arch::asm!("fxsave64 [{}]", in(reg) self.ptr, options(nostack));
            }
        }
    }

    /// Loads the processor's registers from here.
    pub(in crate::arch) fn restore(&self) {
        let f = cpu::features();
        // SAFETY: the area holds a valid saved (or initial) state.
        unsafe {
            if f.xsave {
                core::arch::asm!("xrstor64 [{}]", in(reg) self.ptr, in("eax") u32::MAX, in("edx") u32::MAX, options(nostack));
            } else {
                core::arch::asm!("fxrstor64 [{}]", in(reg) self.ptr, options(nostack));
            }
        }
    }
}

impl Drop for FpuArea {
    fn drop(&mut self) {
        // SAFETY: allocated in `new` with the same layout.
        unsafe { dealloc(self.ptr, self.layout) };
    }
}

/// A thread's saved context, touched only by the CPU switching the thread
/// in or out (under the BKL), and by the thread itself.
pub struct Context {
    rsp: u64,
    fs_base: u64,
    /// The registers user code has, for every thread but an idle one.
    fpu: Option<FpuArea>,
}

impl Context {
    /// An empty context; `user_state`: with room for the registers user
    /// code has (every thread but an idle one).
    pub fn new(user_state: bool) -> Option<Context> {
        Some(Context { rsp: 0, fs_base: 0, fpu: if user_state { Some(FpuArea::new()?) } else { None } })
    }

    /// Whether the thread can be switched to: its context was prepared, or
    /// saved by a switch away from it.
    pub fn is_ready(&self) -> bool {
        self.rsp != 0
    }

    /// Prepares the kernel stack below `stack_top` of a new kernel thread
    /// so that the first switch to it runs `entry(arg)`.
    pub fn prepare_kernel(&mut self, stack_top: u64, entry: fn(u64) -> !, arg: u64) {
        // Initial frame popped by vk_context_switch: r15 r14 r13 r12 rbx rbp ret.
        let frame = [0u64, 0, arg, entry as usize as u64, 0, 0, vk_kernel_thread_trampoline as *const () as u64, 0];
        let rsp = stack_top - 16 - 7 * 8; // keeps rsp 16-aligned after `ret`
        // SAFETY: writing the initial frame into the thread's own fresh stack.
        unsafe { core::ptr::copy_nonoverlapping(frame.as_ptr(), rsp as *mut u64, 7) };
        self.rsp = rsp;
    }

    /// Prepares the kernel stack below `stack_top` of a new thread so that
    /// the first switch to it enters user mode at `entry` with the given
    /// stack and arguments.
    pub fn prepare_user(&mut self, stack_top: u64, entry: u64, user_stack: u64, arg0: u64, arg1: u64) {
        let tf_addr = stack_top - core::mem::size_of::<TrapFrame>() as u64;
        let tf = TrapFrame::new_user(entry, user_stack, arg0, arg1);
        let rsp = tf_addr - 7 * 8;
        let regs = [0u64, 0, 0, 0, 0, 0, vk_thread_trampoline as *const () as u64];
        // SAFETY: the thread has not run yet; its stack is ours.
        unsafe {
            (tf_addr as *mut TrapFrame).write(tf);
            core::ptr::copy_nonoverlapping(regs.as_ptr(), rsp as *mut u64, 7);
        }
        self.rsp = rsp;
    }

    /// Saves the running thread's x87, SSE and AVX registers (this is its
    /// context) for something else to use the processor's: a virtual
    /// processor's.
    pub(in crate::arch) fn save_fpu(&self) {
        if let Some(f) = &self.fpu {
            f.save();
        }
    }

    /// Gives the running thread its registers back ([`Self::save_fpu`]).
    pub(in crate::arch) fn restore_fpu(&self) {
        if let Some(f) = &self.fpu {
            f.restore();
        }
    }

    /// Sets the thread pointer of the running thread, whose context this
    /// is: the FS base, through which it finds its thread-local storage.
    pub fn set_tls(&mut self, value: u64) {
        self.fs_base = value;
        // SAFETY: loading a user-space address into FS.base.
        unsafe { wrmsr(MSR_FS_BASE, value) };
    }
}

/// Switches this CPU from the thread whose context is `prev` to the one
/// whose context is `next`, entering the kernel at `next_stack_top` when it
/// comes from user mode (0: a thread that never runs user code). Returns
/// when a later switch comes back to `prev`.
///
/// # Safety
/// `prev` is the running thread's context and `next` a ready one
/// ([`Context::is_ready`]), both live until the switch has completed; the
/// BKL is held, with interrupts disabled.
pub unsafe fn switch(prev: *mut Context, next: *mut Context, next_stack_top: u64) {
    // SAFETY: the caller hands over both contexts for the switch. No
    // reference to them outlives this block: once `prev` is switched away
    // from, another CPU may switch to it.
    unsafe {
        if let Some(f) = &(*prev).fpu {
            f.save();
            (*prev).fs_base = rdmsr(MSR_FS_BASE);
        }
        if let Some(f) = &(*next).fpu {
            f.restore();
            wrmsr(MSR_FS_BASE, (*next).fs_base);
        }
    }
    if next_stack_top != 0 {
        percpu::set_kernel_stack(next_stack_top);
    }
    // SAFETY: `next`'s stack pointer was prepared by `prepare_*` or saved
    // by a previous switch; the caller keeps `prev` alive meanwhile.
    unsafe { vk_context_switch(&raw mut (*prev).rsp, (*next).rsp) };
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
    abi::ThreadState {
        rax: f.rax,
        rbx: f.rbx,
        rcx: f.rcx,
        rdx: f.rdx,
        rsi: f.rsi,
        rdi: f.rdi,
        rbp: f.rbp,
        rsp: f.rsp,
        r8: f.r8,
        r9: f.r9,
        r10: f.r10,
        r11: f.r11,
        r12: f.r12,
        r13: f.r13,
        r14: f.r14,
        r15: f.r15,
        rip: f.rip,
        rflags: f.rflags,
        fs_base: ctx.fs_base,
    }
}
