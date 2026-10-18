/* SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
   SPDX-License-Identifier: GPL-3.0-only

   What the test programs expect of the machine on AMD64. */
#ifndef VEDA_TESTS_MACHINE_H
#define VEDA_TESTS_MACHINE_H

/* The machine's name: what uname(2) and the auxiliary vector's
   AT_PLATFORM give. */
#define VEDA_MACHINE "x86_64"

/* The size of a page: what AT_PAGESZ and sysconf(_SC_PAGESIZE) give, and
   the unit of mmap, mprotect and madvise. */
#define VEDA_PAGE_SIZE 4096

/* A thread's stack pointer and program counter in its machine context
   (veda_thread_registers). */
#define VEDA_STACK_POINTER(mc) ((unsigned long)(mc).gregs[REG_RSP])
#define VEDA_PROGRAM_COUNTER(mc) ((unsigned long)(mc).gregs[REG_RIP])

/* The machine code of a function that returns N (0 to 255), int f(void),
   as a JIT writes into executable memory: mov eax, N; ret. */
#define VEDA_CODE_RETURNING(n) { 0xb8, (n), 0x00, 0x00, 0x00, 0xc3 }

#endif
