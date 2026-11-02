// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Symbols that compiled Rust code expects from a C runtime, on ARM64's
//! user target (`aarch64-unknown-linux-musl`, whose C library Veda's
//! programs do not link).
//!
//! The runtime provides the memory primitives LLVM emits calls to (with the
//! AAPCS64's calling convention: `x0`, `x1`, `x2`), and stubs for the
//! unwinder's symbols that the precompiled `core` and `alloc` name (Veda
//! builds with `panic = "abort"`, so nothing unwinds and they never run).
//! Stack probes are inline on this target: there is no probe function.
//!
//! `memcpy`/`memset` use 32-byte pairs of SIMD registers in a loop, whose
//! accesses go unaligned: these are for memory, not devices' registers.

use core::arch::global_asm;

global_asm!(
    r#"
    .section .text
    .balign 16
    .globl memcpy
    .type memcpy, @function
memcpy:
    mov x3, x0
    cmp x2, #64
    b.lo 3f
2:
    ldp q0, q1, [x1], #32
    ldp q2, q3, [x1], #32
    stp q0, q1, [x3], #32
    stp q2, q3, [x3], #32
    sub x2, x2, #64
    cmp x2, #64
    b.hs 2b
3:
    cmp x2, #8
    b.lo 5f
4:
    ldr x4, [x1], #8
    str x4, [x3], #8
    sub x2, x2, #8
    cmp x2, #8
    b.hs 4b
5:
    cbz x2, 7f
6:
    ldrb w4, [x1], #1
    strb w4, [x3], #1
    subs x2, x2, #1
    b.ne 6b
7:
    ret
    .size memcpy, . - memcpy

    .balign 16
    .globl memset
    .type memset, @function
memset:
    mov x3, x0
    and x1, x1, #0xff
    orr x1, x1, x1, lsl #8
    orr x1, x1, x1, lsl #16
    orr x1, x1, x1, lsl #32
    dup v0.2d, x1
    cmp x2, #64
    b.lo 3f
2:
    stp q0, q0, [x3], #32
    stp q0, q0, [x3], #32
    sub x2, x2, #64
    cmp x2, #64
    b.hs 2b
3:
    cmp x2, #8
    b.lo 5f
4:
    str x1, [x3], #8
    sub x2, x2, #8
    cmp x2, #8
    b.hs 4b
5:
    cbz x2, 7f
6:
    strb w1, [x3], #1
    subs x2, x2, #1
    b.ne 6b
7:
    ret
    .size memset, . - memset

    // memmove: forward copy is safe when dst <= src or the ranges do not
    // overlap; otherwise copy backwards.
    .balign 16
    .globl memmove
    .type memmove, @function
memmove:
    cmp x0, x1
    b.ls memcpy
    add x3, x1, x2
    cmp x0, x3
    b.hs memcpy
    add x4, x0, x2
2:
    cmp x2, #8
    b.lo 3f
    ldr x5, [x3, #-8]!
    str x5, [x4, #-8]!
    sub x2, x2, #8
    b 2b
3:
    cbz x2, 5f
4:
    ldrb w5, [x3, #-1]!
    strb w5, [x4, #-1]!
    subs x2, x2, #1
    b.ne 4b
5:
    ret
    .size memmove, . - memmove

    .balign 16
    .globl memcmp
    .type memcmp, @function
    .globl bcmp
    .type bcmp, @function
memcmp:
bcmp:
    cbz x2, 3f
2:
    ldrb w3, [x0], #1
    ldrb w4, [x1], #1
    subs w3, w3, w4
    b.ne 4f
    subs x2, x2, #1
    b.ne 2b
3:
    mov w0, #0
    ret
4:
    mov w0, w3
    ret
    .size memcmp, . - memcmp
    .size bcmp, . - bcmp

    .balign 16
    .globl strlen
    .type strlen, @function
strlen:
    mov x1, x0
2:
    ldrb w2, [x1], #1
    cbnz w2, 2b
    sub x0, x1, x0
    sub x0, x0, #1
    ret
    .size strlen, . - strlen
"#
);

/// What crates that look for the processor's features ask the C library of
/// Linux's target: nothing here. They take their portable code.
#[unsafe(no_mangle)]
pub extern "C" fn getauxval(_kind: usize) -> usize {
    0
}
