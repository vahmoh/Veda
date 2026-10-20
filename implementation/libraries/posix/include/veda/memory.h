/*
 * SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
 * SPDX-License-Identifier: GPL-3.0-only
 *
 * <veda/memory.h> - the memory a process holds, as Veda's kernel counts
 * it, for programs that watch their own (a web engine's memory pressure
 * handler): Veda has no /proc, and this is what Linux's /proc/self/statm
 * would say of its resident memory. The memory behind a memory file, for
 * handing it to a Veda service. And what the system ends first when
 * memory runs out.
 */

#ifndef _VEDA_MEMORY_H
#define _VEDA_MEMORY_H

#include <stddef.h>
#include <sys/types.h>
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

/*
 * Memory classes: what the system ends first when memory runs out. Memory
 * is committed as it is touched, so it runs out at a page fault: the
 * kernel then ends a process to make room, of the most expendable class
 * above the system's, the one that holds the most, and the access is made
 * again. A process starts in its creator's class (an application's, for
 * what the launcher starts), and only ever moves towards the most
 * expendable.
 */
#define VEDA_MEMORY_SYSTEM 0      /* the system's services: never ended to make room */
#define VEDA_MEMORY_APPLICATION 1 /* applications and what they start */
#define VEDA_MEMORY_EXPENDABLE 2  /* least costly to lose, such as a web page's process: ended first */

/*
 * Moves this process to memory class MEMORY_CLASS. Returns 0, or an errno
 * value: EINVAL (no such class), EACCES (one less expendable than its
 * own).
 */
int veda_process_set_memory_class(int memory_class);

/*
 * Writes to *ENDED 1 if child process PID was ended by the system because
 * memory ran out, 0 if it ended otherwise or still runs: a wait status
 * cannot say, as being ended for memory is SIGKILL there, as under Linux's
 * OOM killer. Returns 0, or an errno value: ECHILD (not a child of this
 * process, or waited for already), EFAULT (ENDED is null).
 */
int veda_child_ended_for_memory(pid_t pid, int *ended);

#ifdef __cplusplus
}
#endif

#endif
