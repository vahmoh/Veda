/*
 * SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
 * SPDX-License-Identifier: GPL-3.0-only
 *
 * <veda/thread.h> - stopping a thread of this process and reading its
 * registers, as a garbage collector that scans the stacks of other threads
 * does (JavaScriptCore's). Threads are named by their ids (gettid()).
 *
 * The POSIX layer makes the kernel's calls (thread_suspend, thread_resume,
 * thread_read_state: implementation/libraries/posix/src/thread.rs). Each
 * function returns 0 when it succeeds, or an errno value.
 */

#ifndef _VEDA_THREAD_H
#define _VEDA_THREAD_H

#include <signal.h>
#include <sys/types.h>

#ifdef __cplusplus
extern "C" {
#endif

/*
 * Stops thread TID, another than the caller: it runs none of the
 * program's code until veda_thread_resume lets it go on, once for each
 * time it was stopped. Returns once it has stopped (a thread stopped in a
 * system call finishes the call first). ESRCH: no such thread, or it has
 * ended; EINVAL: the caller itself.
 */
int veda_thread_suspend(pid_t tid);

/* Lets thread TID, which veda_thread_suspend stopped, go on. EINVAL: it is
 * not stopped. */
int veda_thread_resume(pid_t tid);

/*
 * The registers of thread TID, which veda_thread_suspend stopped, as it
 * left the program's code: its general registers in OUT's gregs, as
 * Linux's signal frames have them (REG_RSP, REG_RIP...); no floating
 * point (fpregs is null). EINVAL: it is not stopped.
 */
int veda_thread_registers(pid_t tid, mcontext_t *out);

#ifdef __cplusplus
}
#endif

#endif
