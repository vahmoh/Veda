/*
 * SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
 * SPDX-License-Identifier: GPL-3.0-only
 *
 * <veda/memory.h> - the memory a process holds, as Veda's kernel counts
 * it, for programs that watch their own (a web engine's memory pressure
 * handler). Veda has no /proc: this is what Linux's /proc/self/statm
 * would say of its resident memory.
 */

#ifndef _VEDA_MEMORY_H
#define _VEDA_MEMORY_H

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/*
 * The bytes of memory this process holds: the pages committed to its
 * private memory and to the memory objects it maps, each counted once (a
 * memory object shared with other processes counts in each of them).
 */
size_t veda_process_memory(void);

#ifdef __cplusplus
}
#endif

#endif
