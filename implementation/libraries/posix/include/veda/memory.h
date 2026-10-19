/*
 * SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
 * SPDX-License-Identifier: GPL-3.0-only
 *
 * <veda/memory.h> - the memory a process holds, as Veda's kernel counts
 * it, for programs that watch their own (a web engine's memory pressure
 * handler): Veda has no /proc, and this is what Linux's /proc/self/statm
 * would say of its resident memory. And the memory behind a memory file,
 * for handing it to a Veda service.
 */

#ifndef _VEDA_MEMORY_H
#define _VEDA_MEMORY_H

#include <stddef.h>
#include <veda/ipc.h>

#ifdef __cplusplus
extern "C" {
#endif

/*
 * The bytes of memory this process holds: the pages committed to its
 * private memory and to the memory objects it maps, each counted once (a
 * memory object shared with other processes counts in each of them).
 */
size_t veda_process_memory(void);

/*
 * A handle to the memory of the memory file (memfd_create) on descriptor
 * FD, written to *VMO, for a Veda service to map: the window system shows
 * a page's frames from the memory the browser's page process draws them
 * in. It has the rights the descriptor has: none to write, unless the
 * descriptor may. The caller closes it (veda_close), or sends it on.
 * Returns 0, or an errno value: EBADF (no such descriptor), EINVAL (not a
 * memory file).
 */
int veda_memory_file(int fd, veda_handle_t *vmo);

#ifdef __cplusplus
}
#endif

#endif
