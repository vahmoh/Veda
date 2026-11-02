// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Assembly entry and exit paths.
//!
//! * `vk_vectors`: the exception vector table.
//! * `vk_trap_common` / `vk_return_from_trap`: save and restore the full
//!   register state as a [`TrapFrame`] and call into Rust. System calls
//!   enter this way too, so that scheduling code can treat every kernel
//!   entry alike.
//! * `vk_context_switch`: switches kernel stacks between two threads.
//!
//! A thread's kernel stack is `SP_ELx`, which stays where it is while the
//! thread runs user code: a frame popped from the top of its stack leaves it
//! ready for the next entry. The kernel never runs on `SP_EL0`, which is
//! free for its entry code while it is in the kernel.
//!
//! The kernel's own synchronous exceptions, all of them fatal, run on a
//! stack of the CPU's, as what faulted may be the kernel stack.

use core::arch::global_asm;

use super::percpu::OFF_FAULT_STACK;
use super::trap::{self, EC_SVC, TrapFrame};
use crate::synchronization::bkl;

/// The size of a [`TrapFrame`].
pub(in crate::architecture) const FRAME_SIZE: usize = 288;

/// What brought a trap to [`trap_dispatch`].
const KIND_SYNCHRONOUS: u64 = 0;
const KIND_INTERRUPT: u64 = 1;
const KIND_SYSTEM_ERROR: u64 = 2;
const KIND_UNEXPECTED: u64 = 3;

unsafe extern "C" {
    static vk_vectors: u8;
    pub(in crate::architecture) fn vk_thread_trampoline();
    pub(in crate::architecture) fn vk_kernel_thread_trampoline();
    pub(in crate::architecture) fn vk_context_switch(old_sp: *mut u64, new_sp: u64);
}

/// The exception vector table's address, for `VBAR_EL1`.
pub(in crate::architecture) fn vectors() -> u64 {
    core::ptr::addr_of!(vk_vectors) as u64
}

global_asm!(
    r#"
    // One of the table's 16 entries: 128 bytes each.
    .macro vk_vector kind
        .balign 128
        sub     sp, sp, #{frame}
        stp     x0, x1, [sp, #0]
        mov     x1, #\kind
        b       vk_trap_common
    .endm

    .section .text
    .balign 2048
    .globl vk_vectors
vk_vectors:
    // From this level, on SP_EL0: the kernel never runs so.
    vk_vector {unexpected}
    vk_vector {unexpected}
    vk_vector {unexpected}
    vk_vector {unexpected}

    // From this level, on SP_ELx. A synchronous exception is the kernel's
    // own and fatal, and may have been its stack: it takes the CPU's, with
    // SP_EL0 for the room to swap the two.
    .balign 128
    msr     sp_el0, x0
    mrs     x0, tpidr_el1
    ldr     x0, [x0, #{fault_stack}]
    add     sp, sp, x0
    sub     x0, sp, x0
    sub     sp, sp, x0
    sub     sp, sp, #{frame}
    str     x0, [sp, #{off_sp}]
    mrs     x0, sp_el0
    stp     x0, x1, [sp, #0]
    mov     x1, #{synchronous}
    b       vk_trap_saved_sp
    vk_vector {interrupt}
    vk_vector {unexpected}
    vk_vector {system_error}

    // From a lower level, in 64-bit state: user code.
    vk_vector {synchronous}
    vk_vector {interrupt}
    vk_vector {unexpected}
    vk_vector {system_error}

    // From a lower level, in 32-bit state: never, as programs are 64-bit.
    vk_vector {unexpected}
    vk_vector {unexpected}
    vk_vector {unexpected}
    vk_vector {unexpected}

    .section .text
    .balign 16
vk_trap_common:
    stp     x2, x3, [sp, #16]
    mrs     x2, spsr_el1
    tst     x2, #0xF
    b.ne    1f
    mrs     x2, sp_el0
    b       2f
1:
    add     x2, sp, #{frame}
2:
    str     x2, [sp, #{off_sp}]
    b       3f
vk_trap_saved_sp:
    stp     x2, x3, [sp, #16]
3:
    stp     x4, x5, [sp, #32]
    stp     x6, x7, [sp, #48]
    stp     x8, x9, [sp, #64]
    stp     x10, x11, [sp, #80]
    stp     x12, x13, [sp, #96]
    stp     x14, x15, [sp, #112]
    stp     x16, x17, [sp, #128]
    stp     x18, x19, [sp, #144]
    stp     x20, x21, [sp, #160]
    stp     x22, x23, [sp, #176]
    stp     x24, x25, [sp, #192]
    stp     x26, x27, [sp, #208]
    stp     x28, x29, [sp, #224]
    str     x30, [sp, #240]
    mrs     x2, elr_el1
    mrs     x3, spsr_el1
    stp     x2, x3, [sp, #256]
    mrs     x2, esr_el1
    mrs     x3, far_el1
    stp     x2, x3, [sp, #272]
    mov     x0, sp
    bl      {trap_dispatch}

    .globl vk_return_from_trap
vk_return_from_trap:
    clrex
    ldp     x2, x3, [sp, #256]
    msr     elr_el1, x2
    msr     spsr_el1, x3
    tst     x3, #0xF
    b.ne    4f
    ldr     x2, [sp, #{off_sp}]
    msr     sp_el0, x2
4:
    ldp     x0, x1, [sp, #0]
    ldp     x2, x3, [sp, #16]
    ldp     x4, x5, [sp, #32]
    ldp     x6, x7, [sp, #48]
    ldp     x8, x9, [sp, #64]
    ldp     x10, x11, [sp, #80]
    ldp     x12, x13, [sp, #96]
    ldp     x14, x15, [sp, #112]
    ldp     x16, x17, [sp, #128]
    ldp     x18, x19, [sp, #144]
    ldp     x20, x21, [sp, #160]
    ldp     x22, x23, [sp, #176]
    ldp     x24, x25, [sp, #192]
    ldp     x26, x27, [sp, #208]
    ldp     x28, x29, [sp, #224]
    ldr     x30, [sp, #240]
    add     sp, sp, #{frame}
    eret
    // Nothing runs past the return, however far ahead a processor guesses.
    dsb     nsh
    isb

    // Cache maintenance a thread made here (a program may, as JITs do) is
    // over before it can go on from another processor, whose barriers do not
    // wait for it.
    .balign 16
    .globl vk_context_switch
vk_context_switch:
    dsb     ish
    sub     sp, sp, #96
    stp     x19, x20, [sp, #0]
    stp     x21, x22, [sp, #16]
    stp     x23, x24, [sp, #32]
    stp     x25, x26, [sp, #48]
    stp     x27, x28, [sp, #64]
    stp     x29, x30, [sp, #80]
    mov     x2, sp
    str     x2, [x0]
    mov     sp, x1
    ldp     x19, x20, [sp, #0]
    ldp     x21, x22, [sp, #16]
    ldp     x23, x24, [sp, #32]
    ldp     x25, x26, [sp, #48]
    ldp     x27, x28, [sp, #64]
    ldp     x29, x30, [sp, #80]
    add     sp, sp, #96
    ret

    // First code run by a new user thread (entered through `ret` in
    // vk_context_switch with its TrapFrame at the top of the stack).
    .balign 16
    .globl vk_thread_trampoline
vk_thread_trampoline:
    bl      {thread_start_hook}
    b       vk_return_from_trap

    // First code run by a new kernel thread: x19 = entry, x20 = argument.
    .balign 16
    .globl vk_kernel_thread_trampoline
vk_kernel_thread_trampoline:
    mov     x0, x19
    mov     x1, x20
    bl      {kernel_thread_main}
    brk     #0
"#,
    frame = const FRAME_SIZE,
    off_sp = const core::mem::offset_of!(TrapFrame, sp),
    fault_stack = const OFF_FAULT_STACK,
    synchronous = const KIND_SYNCHRONOUS,
    interrupt = const KIND_INTERRUPT,
    system_error = const KIND_SYSTEM_ERROR,
    unexpected = const KIND_UNEXPECTED,
    trap_dispatch = sym trap_dispatch,
    thread_start_hook = sym user_thread_start,
    kernel_thread_main = sym kernel_thread_main,
);

/// Entry point from `vk_trap_common`.
extern "C" fn trap_dispatch(frame: &mut TrapFrame, kind: u64) {
    match kind {
        KIND_INTERRUPT => super::gic::interrupt(frame),
        KIND_SYNCHRONOUS if !frame.is_user() => trap::kernel_exception(frame),
        KIND_SYNCHRONOUS if frame.class() == EC_SVC => {
            crate::syscall::syscall_dispatch(frame);
        }
        // A system error taken in a program is that program's doing, as a
        // driver's access to its device can be, as far as anyone can tell.
        KIND_SYNCHRONOUS | KIND_SYSTEM_ERROR if frame.is_user() => {
            bkl::acquire();
            crate::scheduler::entered_from_user();
            crate::scheduler::user_exception(frame);
            crate::scheduler::return_to_user_hook(frame);
            bkl::release();
        }
        KIND_SYSTEM_ERROR => trap::fatal(frame, "system error"),
        _ => trap::fatal(frame, "unexpected exception"),
    }
}

/// First Rust code run by a new user thread (from `vk_thread_trampoline`).
extern "C" fn user_thread_start() {
    crate::scheduler::thread_start_hook();
}

/// First Rust code run by a new kernel thread (from
/// `vk_kernel_thread_trampoline`): runs `entry(arg)`.
extern "C" fn kernel_thread_main(entry: usize, arg: u64) -> ! {
    crate::scheduler::kernel_thread_start_hook();
    // SAFETY: `entry` is the function the thread was prepared to run
    // (`thread::Context::prepare_kernel`).
    let entry = unsafe { core::mem::transmute::<usize, fn(u64) -> !>(entry) };
    entry(arg)
}
