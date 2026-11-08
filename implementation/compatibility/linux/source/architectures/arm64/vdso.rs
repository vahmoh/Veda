// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The vDSO's code on ARM64 (`crate::vdso`): the functions of Linux's
//! ARM64 vDSO, of version `LINUX_2.6.39`. `__kernel_clock_gettime`,
//! `__kernel_gettimeofday` and `__kernel_clock_getres` read the clocks as
//! `runtime::time` reads them, from the generic timer's virtual count
//! (`CNTVCT_EL0`, read once every earlier load has completed, so that a
//! time read after another thread's that this one has seen is never
//! earlier) and the scale on the data page below the image, and make the
//! system call for what they cannot read. `__kernel_rt_sigreturn` is where
//! a signal handler without a restorer of its own returns, to make
//! `rt_sigreturn`, as Linux's kernel sends it there (`crate::vdso::restorer`).
//!
//! The clock functions follow the AAPCS64 and touch no register their
//! caller keeps, nor the vector registers, nor the stack: one rule finds
//! their caller, at the link register, at every instruction ([`CALLED`]).
//! `__kernel_rt_sigreturn` is Linux's to the instruction (`mov x8, #139`,
//! `svc #0`), as unwinders that find no call frame information look for
//! it (libgcc's, GDB's), after the no-op Linux puts before it, in which a
//! return address less one falls (an unwinder's guess at the call); its
//! rule ([`SIGNAL`]) leads unwinders out of the handler, through the frame
//! record of the code the signal interrupted, as Linux's does.

use core::mem::offset_of;

use abi::ClockInfo;
use linux_raw_sys::general::{__NR_clock_getres, __NR_clock_gettime, __NR_gettimeofday, __NR_rt_sigreturn};

use crate::vdso::{Code, Frame, Function};

/// Nanoseconds a second.
const BILLION: u64 = 1_000_000_000;
/// The clocks' resolution (`clock_getres`'s): seconds, and nanoseconds
/// more, each loaded as 32 bits.
const RESOLUTION: (u64, u64) = (posix::host::RESOLUTION_NS / BILLION, posix::host::RESOLUTION_NS % BILLION);
const _: () = assert!(RESOLUTION.0 < 1 << 32 && RESOLUTION.1 < 1 << 32);

core::arch::global_asm!(
    r#"
    .pushsection .rodata.linux_compatibility_vdso, "a", %progbits

    // The code's size, then each function's offset in it and size, in
    // `FUNCTIONS`' order, then where the signal return's part starts.
    .p2align 2
    .globl linux_compatibility_vdso_table
    .hidden linux_compatibility_vdso_table
linux_compatibility_vdso_table:
    .long .Lend - .Ltext
    .long .Lclock_gettime - .Ltext, .Lclock_gettime_end - .Lclock_gettime
    .long .Lgettimeofday - .Ltext, .Lgettimeofday_end - .Lgettimeofday
    .long .Lclock_getres - .Ltext, .Lclock_getres_end - .Lclock_getres
    .long .Lrt_sigreturn - .Ltext, .Lrt_sigreturn_end - .Lrt_sigreturn
    .long .Lsignal - .Ltext

    // \reg: the 32-bit number \value.
    .macro load32 reg, value
    movz \reg, #((\value) & 0xffff)
    movk \reg, #(((\value) >> 16) & 0xffff), lsl #16
    .endm

    // w9's bit 0: whether clock w0 (below 32) is one of the clocks
    // \clocks has a bit of.
    .macro one_of clocks
    load32 w9, \clocks
    lsr w9, w9, w0
    .endm

    // \out: a clock that is the monotonic one plus \offset (the real time
    // at boot, or 0), in nanoseconds, as `runtime::time` reads it: the
    // counter, read once every earlier load has completed, less its
    // reading at boot (0 before it), by the scale \scale (nanoseconds per
    // tick, 32.32), the product's high bits dropped and the sum saturated
    // as the kernel's are. \data: the data page. Clobbers x14 and x15.
    .macro now out, data, scale, offset
    dsb ishld
    isb
    mrs \out, cntvct_el0
    ldr x14, [\data, #{ticks_at_zero}]
    subs \out, \out, x14
    csel \out, xzr, \out, lo
    mul x14, \out, \scale
    umulh x15, \out, \scale
    extr \out, x15, x14, #32
    adds \out, \out, \offset
    csinv \out, \out, xzr, cc
    .endm

    // \seconds: the seconds of \ns nanoseconds; \ns: the nanoseconds
    // into the last. Clobbers x14.
    .macro split seconds, ns
    load32 x14, {billion}
    udiv \seconds, \ns, x14
    msub \ns, \seconds, x14, \ns
    .endm

    // The code, which runs where the image puts it: `{text}` bytes into the
    // image, the data page right below the image; between its functions,
    // zeros, `udf #0`, which stops a stray jump there.
    .p2align 4
    .globl linux_compatibility_vdso_text
    .hidden linux_compatibility_vdso_text
linux_compatibility_vdso_text:
.Ltext:

    // int __kernel_clock_gettime(clockid_t clock, struct timespec *ts)
.Lclock_gettime:
    cmp w0, #31
    b.hi .Lclock_gettime_call
    one_of {clocks}
    tbz w9, #0, .Lclock_gettime_call
    adr x10, .Ltext - {text} - {data}
    ldr x11, [x10, #{scale}]
    cbz x11, .Lclock_gettime_call
    one_of {utc_clocks}
    ldr x12, [x10, #{utc_at_zero}]
    tst w9, #1
    csel x12, x12, xzr, ne
    now x13, x10, x11, x12
    split x12, x13
    stp x12, x13, [x1]
    mov w0, #0
    ret
.Lclock_gettime_call:
    mov x8, #{nr_clock_gettime}
    svc #0
    ret
.Lclock_gettime_end:

    // int __kernel_gettimeofday(struct timeval *tv, struct timezone *tz):
    // a time zone is the system call's to give.
    .p2align 4
.Lgettimeofday:
    cbnz x1, .Lgettimeofday_call
    cbz x0, .Lgettimeofday_done
    adr x10, .Ltext - {text} - {data}
    ldr x11, [x10, #{scale}]
    cbz x11, .Lgettimeofday_call
    ldr x12, [x10, #{utc_at_zero}]
    now x13, x10, x11, x12
    split x12, x13
    mov x14, #1000
    udiv x13, x13, x14
    stp x12, x13, [x0]
.Lgettimeofday_done:
    mov w0, #0
    ret
.Lgettimeofday_call:
    mov x8, #{nr_gettimeofday}
    svc #0
    ret
.Lgettimeofday_end:

    // int __kernel_clock_getres(clockid_t clock, struct timespec *res)
    .p2align 4
.Lclock_getres:
    cmp w0, #31
    b.hi .Lclock_getres_call
    one_of {clocks}
    tbz w9, #0, .Lclock_getres_call
    cbz x1, .Lclock_getres_done
    load32 x9, {resolution_s}
    load32 x10, {resolution_ns}
    stp x9, x10, [x1]
.Lclock_getres_done:
    mov w0, #0
    ret
.Lclock_getres_call:
    mov x8, #{nr_clock_getres}
    svc #0
    ret
.Lclock_getres_end:

    // The signal return: Linux's no-op, then `__kernel_rt_sigreturn`.
    .p2align 4
.Lsignal:
    nop
.Lrt_sigreturn:
    mov x8, #{nr_rt_sigreturn}
    svc #0
.Lrt_sigreturn_end:
.Lend:

    .purgem load32
    .purgem one_of
    .purgem now
    .purgem split
    .popsection
    "#,
    text = const crate::vdso::TEXT,
    data = const crate::vdso::DATA,
    clocks = const crate::vdso::UTC_CLOCKS | crate::vdso::MONOTONIC_CLOCKS,
    utc_clocks = const crate::vdso::UTC_CLOCKS,
    scale = const offset_of!(ClockInfo, ns_per_tick),
    ticks_at_zero = const offset_of!(ClockInfo, ticks_at_zero),
    utc_at_zero = const offset_of!(ClockInfo, utc_at_zero),
    billion = const BILLION,
    resolution_s = const RESOLUTION.0,
    resolution_ns = const RESOLUTION.1,
    nr_clock_gettime = const __NR_clock_gettime,
    nr_gettimeofday = const __NR_gettimeofday,
    nr_clock_getres = const __NR_clock_getres,
    nr_rt_sigreturn = const __NR_rt_sigreturn,
);

/// The functions, in the table's order, as Linux names them (with no
/// other names, unlike x86-64's).
const FUNCTIONS: [&str; 4] =
    ["__kernel_clock_gettime", "__kernel_gettimeofday", "__kernel_clock_getres", "__kernel_rt_sigreturn"];
/// The signal return, of [`FUNCTIONS`].
const RT_SIGRETURN: usize = 3;

unsafe extern "C" {
    /// The table the assembly above lays out.
    static linux_compatibility_vdso_table: [u32; 2 + 2 * FUNCTIONS.len()];
    /// The code's first byte.
    static linux_compatibility_vdso_text: u8;
}

/// Where a clock function's caller is, at any of its instructions: at the
/// link register (`x30`, DWARF's register 30), with the stack pointer
/// (register 31) as it left it: DWARF's `DW_CFA_def_cfa sp, 0`.
const CALLED: Frame = Frame { return_address: 30, data_alignment: -8, signal: false, instructions: &[0x0c, 31, 0] };

/// Where the code a signal interrupted is, from the signal return: the
/// frame record the handler's frame pointer (`x29`, register 29) pointed
/// at as it started, which holds that code's `x29` and `x30`: DWARF's
/// `DW_CFA_def_cfa x29, 0`, `DW_CFA_offset x29, 0` and `DW_CFA_offset x30,
/// 1` (a word of the data alignment, 8, from the frame's address), as
/// Linux's.
const SIGNAL: Frame = Frame {
    return_address: 30,
    data_alignment: 8,
    signal: true,
    instructions: &[0x0c, 29, 0, 0x80 | 29, 0, 0x80 | 30, 1],
};

/// The vDSO's code, as the image has it.
pub fn code() -> Option<Code> {
    // SAFETY: the table and the code the assembly lays out, read-only data
    // of this program's own; the code is as long as the table's first
    // word says.
    let (table, text) = unsafe {
        let table = &linux_compatibility_vdso_table;
        (table, core::slice::from_raw_parts(&raw const linux_compatibility_vdso_text, table[0] as usize))
    };
    let functions: alloc::vec::Vec<Function> = FUNCTIONS
        .iter()
        .enumerate()
        .map(|(i, &name)| Function {
            name,
            alias: None,
            offset: table[1 + 2 * i] as usize,
            size: table[2 + 2 * i] as usize,
        })
        .collect();
    let signal = table[1 + 2 * FUNCTIONS.len()] as usize;
    let restorer = Some(functions[RT_SIGRETURN].offset);
    let frames = alloc::vec![(0..signal, CALLED), (signal..text.len(), SIGNAL)];
    Some(Code { text, functions, version: "LINUX_2.6.39", frames, restorer })
}
