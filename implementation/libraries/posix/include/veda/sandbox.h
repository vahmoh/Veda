/*
 * SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
 * SPDX-License-Identifier: GPL-3.0-only
 *
 * <veda/sandbox.h> - giving up, for good, what a process does not need
 * before it handles what it cannot trust: the files it reaches, the
 * services it reaches, and abilities the kernel gives. The browser's page
 * processes do (documentation/BROWSER.md), where a program on Linux would
 * use Landlock, seccomp and namespaces.
 *
 * The POSIX layer confines its connection to the VFS (vfs::confine) and
 * its registry channel (registry::confine), and makes the kernel's call
 * (process_restrict): implementation/libraries/posix/src/sandbox.rs. Each
 * function returns 0 when it succeeds, or an errno value. What the process
 * holds already (open files, sockets, connections) stays.
 *
 * A sandboxed process can be lent a place to read by one that is not
 * sandboxed (veda_lend_files, veda_borrow_files; vfs::lend, vfs::borrow),
 * as Apple's systems extend a sandbox, until the lender takes it back
 * (veda_take_back_files; vfs::take_back): the browser lends its network
 * process the folder of a page the user opens, while a page it has open
 * needs it.
 */

#ifndef _VEDA_SANDBOX_H
#define _VEDA_SANDBOX_H

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/* A place of the file system: what is at or below PATH (absolute, or from
 * the working directory), to read, or with WRITE nonzero to change too. */
struct veda_place {
	const char *path;
	int write;
};

/*
 * Confines the program's files to COUNT places (at most 64): from now on
 * it reaches what is inside them, the innermost place that holds a path
 * saying how ("/dev" to read, "/dev/null" to write too), and the
 * directories on the way to them, which it can stat and list, but which
 * list only what leads on to a place. Nothing else is there (ENOENT), what
 * it may only read it cannot change (EACCES), and Unix domain sockets have
 * no abstract names for it. Confining the files again narrows them: places
 * that would reach more are refused (EACCES).
 */
int veda_confine_files(const struct veda_place *places, size_t count);

/*
 * Confines the program to the COUNT services NAMES (the registry's names):
 * it connects to no other from now on, nor do the registry channels it
 * hands on (veda_registry's, a child's). Without the network service
 * ("net"), Internet sockets connect, bind and listen no more, nor are
 * names looked up (EACCES). A sandbox leaves out the VFS's name ("vfs"):
 * a connection of its own would reach past veda_confine_files.
 */
int veda_confine_services(const char *const *names, size_t count);

/* What veda_process_restrict takes from the process. */
#define VEDA_RESTRICT_LOG 0x1u       /* reading the kernel log */
#define VEDA_RESTRICT_PROCESSES 0x2u /* creating, listing and opening processes */
#define VEDA_RESTRICT_EXECUTE 0x4u   /* new executable memory: what is executable stays so */

/* Takes FLAGS from the process, for good. EINVAL: a flag it does not know. */
int veda_process_restrict(unsigned flags);

/* The bytes of a loan: a secret, which whoever has it may take the loan
 * with, once, and its lender take it back with. */
#define VEDA_LOAN_SIZE 16

/*
 * Lends PATH (absolute, or from the working directory), a file or a
 * directory there now, to read: writes the loan's VEDA_LOAN_SIZE bytes to
 * LOAN, which the process hands to the one it lends to, over a channel of
 * their own. A loan not taken lapses after a minute, or as the process
 * ends; a process holds at most 16 (ENFILE). One taken lasts until the
 * process takes it back (veda_take_back_files), or ends. EACCES: the
 * process is sandboxed (it lends nothing), or PATH is the user's home
 * folder or holds it, or a private directory; ENOENT: nothing is there.
 */
int veda_lend_files(const char *path, unsigned char *loan);

/*
 * Takes LOAN, of PATH as it was lent: from now on the process reaches what
 * is at or below PATH, to read, as a place veda_confine_files gave it; it
 * reaches nothing less, and changes nothing more. EACCES: there is no such
 * loan (another secret or place, or one taken or lapsed); ENFILE: the
 * process has 64 places already.
 */
int veda_borrow_files(const char *path, const unsigned char *loan);

/*
 * Takes back LOAN, which the process made (veda_lend_files): one not taken
 * yet can be taken no more, and the sandboxed process that took it reaches
 * its place no more (nothing else less, and nothing more), but for the
 * files it has open there. Nothing left to take back (the loan lapsed or
 * was taken back, its place was within the borrower's reach anyway, or
 * the borrower ended) is no error, nor does another's loan take anything
 * back.
 */
int veda_take_back_files(const unsigned char *loan);

#ifdef __cplusplus
}
#endif

#endif
