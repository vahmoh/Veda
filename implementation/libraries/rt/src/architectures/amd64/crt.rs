// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Symbols that compiled Rust code expects from a C runtime, on AMD64's
//! user target (`x86_64-unknown-linux-musl`, whose C library Veda's
//! programs do not link).
//!
//! The runtime provides the memory primitives LLVM emits calls to (with the
//! System V calling convention: `rdi`, `rsi`, `rdx`), and stubs for the
//! unwinder's symbols that the precompiled `core` and `alloc` name (Veda
//! builds with `panic = "abort"`, so nothing unwinds and they never run).
//! Stack probes are inline on this target: there is no probe function.
//!
//! `memcpy`/`memset` use 16-byte SSE moves in a loop rather than
//! `rep movsb`, which is markedly faster under QEMU's TCG emulator.

use core::arch::global_asm;

global_asm!(
    r#"
    .section .text
    .balign 16
    .globl memcpy
    .type memcpy, @function
memcpy:
    mov rax, rdi
    cmp rdx, 64
    jb 3f
2:
    movdqu xmm0, [rsi]
    movdqu xmm1, [rsi + 16]
    movdqu xmm2, [rsi + 32]
    movdqu xmm3, [rsi + 48]
    movdqu [rdi], xmm0
    movdqu [rdi + 16], xmm1
    movdqu [rdi + 32], xmm2
    movdqu [rdi + 48], xmm3
    add rsi, 64
    add rdi, 64
    sub rdx, 64
    cmp rdx, 64
    jae 2b
3:
    cmp rdx, 8
    jb 5f
4:
    mov rcx, [rsi]
    mov [rdi], rcx
    add rsi, 8
    add rdi, 8
    sub rdx, 8
    cmp rdx, 8
    jae 4b
5:
    test rdx, rdx
    jz 7f
6:
    mov cl, [rsi]
    mov [rdi], cl
    inc rsi
    inc rdi
    dec rdx
    jnz 6b
7:
    ret
    .size memcpy, . - memcpy

    .balign 16
    .globl memset
    .type memset, @function
memset:
    mov rax, rdi
    movzx ecx, sil
    mov r8, 0x0101010101010101
    imul rcx, r8
    movq xmm0, rcx
    punpcklqdq xmm0, xmm0
    cmp rdx, 64
    jb 3f
2:
    movdqu [rdi], xmm0
    movdqu [rdi + 16], xmm0
    movdqu [rdi + 32], xmm0
    movdqu [rdi + 48], xmm0
    add rdi, 64
    sub rdx, 64
    cmp rdx, 64
    jae 2b
3:
    cmp rdx, 8
    jb 5f
4:
    mov [rdi], rcx
    add rdi, 8
    sub rdx, 8
    cmp rdx, 8
    jae 4b
5:
    test rdx, rdx
    jz 7f
6:
    mov [rdi], cl
    inc rdi
    dec rdx
    jnz 6b
7:
    ret
    .size memset, . - memset

    // memmove: forward copy is safe when dst <= src or the ranges do not
    // overlap; otherwise copy backwards.
    .balign 16
    .globl memmove
    .type memmove, @function
memmove:
    cmp rdi, rsi
    jbe memcpy
    lea rcx, [rsi + rdx]
    cmp rdi, rcx
    jae memcpy
    mov rax, rdi
    add rdi, rdx
    add rsi, rdx
2:
    cmp rdx, 8
    jb 3f
    sub rdi, 8
    sub rsi, 8
    mov rcx, [rsi]
    mov [rdi], rcx
    sub rdx, 8
    jmp 2b
3:
    test rdx, rdx
    jz 4f
    dec rdi
    dec rsi
    mov cl, [rsi]
    mov [rdi], cl
    dec rdx
    jmp 3b
4:
    ret
    .size memmove, . - memmove

    .balign 16
    .globl memcmp
    .type memcmp, @function
    .globl bcmp
    .type bcmp, @function
memcmp:
bcmp:
    xor eax, eax
    test rdx, rdx
    jz 3f
2:
    movzx eax, byte ptr [rdi]
    movzx ecx, byte ptr [rsi]
    sub eax, ecx
    jnz 3f
    inc rdi
    inc rsi
    dec rdx
    jnz 2b
3:
    ret
    .size memcmp, . - memcmp
    .size bcmp, . - bcmp

    .balign 16
    .globl strlen
    .type strlen, @function
strlen:
    mov rax, rdi
2:
    cmp byte ptr [rax], 0
    je 3f
    inc rax
    jmp 2b
3:
    sub rax, rdi
    ret
    .size strlen, . - strlen
"#
);

/// The exception personality that the unwind tables of the precompiled
/// `core` and `alloc` name. Never called: nothing unwinds.
#[unsafe(no_mangle)]
pub extern "C" fn rust_eh_personality() -> ! {
    crate::sys::process_exit(-1)
}

/// Where the landing pads of the precompiled `core` and `alloc` resume
/// unwinding. Never reached: nothing unwinds.
#[unsafe(no_mangle)]
pub extern "C" fn _Unwind_Resume() -> ! {
    crate::sys::process_exit(-1)
}
