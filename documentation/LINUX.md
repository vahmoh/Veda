<!-- SPDX-FileCopyrightText: © 2026 Vahid Mohammadi -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Linux programs on Veda

Veda runs Linux programs as they were built for Linux: the same
executables, unchanged, that run on a Linux PC. A Linux program runs in a
process of its own, as any program does, and Veda's Linux compatibility
carries out its system calls the way Linux does, with Veda's kernel
objects and services. Nothing is ported and nothing is recompiled. This
document covers running Linux programs, the Linux root they find their
files in, how it works, what it does for security, its limits, and its
tests.

**Contents:** [Running Linux programs](#running-linux-programs) · [A Linux root](#a-linux-root) · [How it works](#how-it-works) · [Security](#security) · [Limits](#limits) · [Performance](#performance) · [Tests](#tests)

## Running Linux programs

A Linux program runs like any command: in the Terminal by its path or on
`PATH`, from another program (`posix_spawn`, `std::process::Command`, the
launcher for a package's application), its output on the terminal, its
exit status in `$?`.

```text
user@veda:~$ ./hello-linux
Hello from Linux
user@veda:~$ echo $?
0
```

Programs for Linux on x86-64 (`x86_64`) run, linked statically or
dynamically, on any C library: glibc, musl, Go's and Rust's runtimes,
programs with none. What Veda's tests run is in [Tests](#tests).

| A Linux program uses | On Veda |
|----------------------|---------|
| Its files, the user's | The VFS: the user's home, `/tmp` and `/dev` are Veda's own; the rest of its tree is its Linux root's ([A Linux root](#a-linux-root)) |
| Threads, thread-local storage, futexes, robust mutexes | Kernel threads, the thread pointer it sets (`arch_prctl`), kernel futexes |
| Signal handlers, `sigaltstack`, `SA_RESTART`, `SIGSEGV` and `SIGILL` handlers | Linux's signal frame (`rt_sigframe`), on its stack or the alternate one, the vector registers in it; a fault is the signal it stands for |
| Its memory: `mmap` and friends, `brk`, `mremap` | The kernel's, kept apart from Linux compatibility's own |
| Pipes, sockets (TCP, UDP, Unix), `epoll`, `eventfd`, `timerfd`, memory files | What Veda's C programs have ([C on Veda](C.md)) |
| Starting programs: `posix_spawn`, `vfork`, `system`, `popen` | A process of Veda's for each, which `execve` starts, of a Linux program or of Veda's |
| `fork` | A copy of the process, its memory copied as either writes it (copy-on-write), as Linux makes it; a shell that forks for each command, a server that forks its workers |
| `execve` of a Linux program | In place: the same process, the same id |
| `SIGCHLD` | Sent as a child ends, saying who and how |
| `/proc/self` (`exe`, `maps`, `stat`, `status`, `cmdline`, `fd/N`...), `/proc/cpuinfo`, `meminfo`, `/sys`'s processors | Made as they are read |
| `uname` | Linux 6.6 (`6.6.0-veda`), on `x86_64` |

A Linux program and Veda's own programs share the user's files and the
terminal, and start one another: a Linux shell runs Veda's commands and
Veda's Terminal runs Linux programs.

## A Linux root

A program linked dynamically needs its dynamic linker and libraries where
it names them (`/lib64/ld-linux-x86-64.so.2`, `/lib/x86_64-linux-gnu`),
and many programs read Linux's configuration (`/etc`). So a Linux program
has a tree of its own: its *Linux root*, a directory of the VFS's that
holds a Linux system's files, is its `/`, and Veda's `/home`, `/tmp`,
`/dev` and `/system` are in it as they are.

| The program names | It is |
|-------------------|-------|
| `/home/...`, `/tmp/...`, `/dev/...`, `/system/...` | Veda's own |
| `/proc/...`, `/sys/devices/system/cpu/...`, `/etc/resolv.conf` | Linux compatibility's ([How it works](#how-it-works)) |
| Anything else | The root's: `/usr/lib/libz.so.1` is `ROOT/usr/lib/libz.so.1` |

The root is the directory the program's environment names in
`VEDA_LINUX_ROOT`, or else `/home/linux`, if there is one. Without one, a
Linux program's tree is Veda's own, with `/system/etc` as its `/etc` (the
user and group databases, the services), which is all a statically linked
program needs.

```text
user@veda:~$ VEDA_LINUX_ROOT=/home/user/alpine ./program-for-alpine
```

A root's files are a Linux system's, as a distribution's archive has
them, with each symbolic link replaced by what it names (Veda's files have
no links yet: [Limits](#limits)).

## How it works

**The program.** When a program is started (`runtime::process::Spawn`),
its executable says what it is (`elf::Kind`): Veda's programs carry a note
of Veda's (their runtime's, or their POSIX layer's), Veda's dynamically
linked programs name Veda's dynamic linker, and any other is a Linux
program. A Linux program's process is started with Veda's Linux
compatibility in it, `linux-compatibility`
(`implementation/compatibility/linux`), given the program's file and what
starts it: its path, arguments and environment, which may be any bytes
(`abi::startup::linux`). It loads the program, and its dynamic linker if
it names one, where Linux's loader would (a position-independent one at a
random address), lays out its stack with its arguments, environment and
auxiliary vector, and runs it.

**Foreign mode.** Linux compatibility runs each of the program's threads
in *foreign mode* (`foreign_run`, `abi`'s "Foreign code"): the thread runs
the program's code until it makes a system call, takes an exception the
kernel does not resolve (a bad access, an invalid instruction), or is
interrupted (a signal); then the kernel hands its registers back to Linux
compatibility, on the same thread, which does what Linux would and runs it
again. The kernel switches the thread pointer between the two (the
program's own, which it sets with `arch_prctl`, and Linux compatibility's);
the x87, SSE and AVX registers stay the program's, as Linux compatibility,
built soft-float, never touches them. So the program's system calls reach
no part of the kernel: what it asks is asked by Linux compatibility, of the
kernel's objects and Veda's services, with its process's handles.

**System calls.** Most of Linux's system calls are those Veda's C programs
make through their C library, which the POSIX layer carries out
([C on Veda](C.md)): Linux compatibility hands them to the same layer
(`posix::host`). What only Linux programs make is Linux compatibility's
own (`crate::syscall`):

| Calls | What Linux compatibility does |
|-------|-------------------------------|
| `clone`, `clone3` | A thread: a kernel thread that runs the program from the call, its stack and thread pointer as asked, its ids written where asked, its `CLEARTID` word cleared and woken as it ends (the kernel's exit futex) |
| `vfork`, `clone` with `CLONE_VM` | A child in the program's memory, on a thread of its own, with copies of the descriptors, working directory and signal handlers (but those shared) and an id of its own (that of a process made for it), while its parent waits: its `execve` starts the program in that process, its `_exit` ends it as a process whose status the parent waits for |
| `fork`, `clone` and `clone3` without `CLONE_VM` | A copy of the process (below), which sends `SIGCHLD` as it ends: the ids written as asked (`CLONE_PARENT_SETTID` in the parent, `CLONE_CHILD_SETTID` in the child, `CLONE_CHILD_CLEARTID` cleared as its thread ends), and a pidfd (`CLONE_PIDFD`). Sharing what two processes cannot (`CLONE_FILES`, `CLONE_FS`, `CLONE_SIGHAND`, `CLONE_SETTLS`), another signal at its end, and `CLONE_VFORK` are refused (`EINVAL`) |
| `execve`, `execveat` | A Linux program in place: the other threads end, the memory and the handlers go, the descriptors marked close-on-exec close. A program of Veda's runs in a process of its own, which this one waits for and ends as |
| `rt_sigreturn`, `sigaltstack` | The registers, vector registers, mask and alternate stack a handler's frame holds |
| `mmap`, `munmap`, `mprotect`, `mremap`, `madvise`, `brk` | The program's areas, which it alone changes: `MAP_FIXED` never replaces Linux compatibility's own memory, nor `madvise` gives it up |
| `arch_prctl`, `set_robust_list` | The thread's own: its thread pointer, its robust futexes (released, `FUTEX_OWNER_DIED`, as it ends) |
| `sendfile`, `copy_file_range` | A copy a read and a write at a time, with Linux's rules: the offsets move by what went, and `copy_file_range` is between regular files only, never onto an overlapping part of the same file |
| `uname`, `personality`, `prctl`, `capget` | Linux's answers, those of a program without privileges |
| `mount`, `ptrace`, `chroot`, `unshare`, `reboot`, setting the clock | Refused (`EPERM`), as Linux refuses a program without privileges |

**Signals.** The POSIX layer keeps what Linux's kernel keeps of signals;
Linux compatibility acts on them as Linux does on the way back to a
program's code: as each system call returns, and as the kernel ends the
program's code for an exception or an interruption. A handler is entered
with Linux's frame laid out below the stack pointer (or at the top of the
alternate stack): the registers the code had, its vector registers (as
`XSAVE` saves them, with Linux's markers), what the signal says, and a
return to the handler's restorer, whose `rt_sigreturn` takes them back
(checked first, as the processor would refuse some). A handler whose
frame cannot be had (its stack is not there) is `SIGSEGV` instead, and for
`SIGSEGV`'s own handler the end of the process, as Linux's
`force_sigsegv` has it. A blocking call a signal ended starts again after
the handler if it had `SA_RESTART`, as on Linux, by going back over the
`syscall` instruction. A legacy virtual system call (the `vsyscall` page)
is carried out as Linux's emulation does.

**Memory.** Linux compatibility keeps account of the program's areas, as
Linux keeps a process's: what it mapped, its image, its stack and its
program break, which `/proc/self/maps` lists. A system call reaches only
those: `munmap` of a range unmaps the program's areas in it, `mprotect`
and `madvise` need them whole (`ENOMEM`), shrinking the program break
unmaps only its own, and memory Linux compatibility reads or writes for
the program (a signal frame, `execve`'s arguments) must be in one that
allows the access while it does, or it is a fault. `mremap` grows and
moves only anonymous memory, by copying it: a file's mapping or shared
memory would be neither any more (`ENOMEM`, as when there is no room,
which programs answer by mapping anew).

**Copies.** `fork` has the kernel copy the process (`process_fork`):
its memory, the program's and Linux compatibility's alike, is the
parent's as it was, each page one for both until either writes it, which
then has a copy of its own (as Linux's copy-on-write has it), but for
shared memory (`MAP_SHARED`), which both see. Its one thread is the one
that forked, which goes on from the call there, its thread id the child's
process id, its alternate signal stack its own, and its robust futex
list none, as on Linux (the C libraries set it again). The POSIX layer
gives the child what Linux's `fork` gives it of the rest, as for a C
program ([C on Veda](C.md#processes-and-signals)): the same open files,
pipes and sockets, the handlers and the mask, no pending signals, timers,
record locks or children. The program's areas are the child's too, as
`/proc/self/maps` lists them.

**Its files** are found in its tree ([A Linux root](#a-linux-root)): the
POSIX layer resolves the program's names in it (`path::Namespace`), `..`
stopping at its root, and keeps the VFS's paths. `/proc` and `/sys`'s
processors are Linux compatibility's: each file is made whole as it is
opened, a memory file of its bytes; `/proc/self/exe` and `cwd` are links
that `readlink` reads, and so is each descriptor's `/proc/self/fd/N`, which
says what it is open on as Linux does (a file's path, `pipe:[N]`,
`socket:[N]`) and opens as the descriptor's file (`ttyname` and `fexecve`
use it).

## Security

A Linux program is a process like any: what it can do is what its
process's handles allow, as a program of Veda's.

- **No way around the kernel.** Foreign code's system calls never reach
  the kernel: each comes back to Linux compatibility, in the same process,
  which can do no more than the process can.
- **Its identity.** Services know the program by its process, which the
  registry names, as they know every program; it is started as an
  application (or by whatever starts it), with its limits, memory class and
  priority.
- **Sandboxes hold.** A Linux program started by a sandboxed one is
  sandboxed the same way: its connections to the VFS and the registry are
  those it was given.
- **No privileges.** It runs as the one user (1000), with no capabilities;
  what needs privileges on Linux is refused.
- **Its own memory.** Linux compatibility shares the process with the
  program, which could overwrite it, as a C program can its C library: it
  gains nothing that way that its process had not.
- **Its copies.** A child `fork` makes can do what its parent could:
  what it gave up stays given up (`process_restrict`), and it has only
  the handles its parent gave it and the memory its parent's handles
  could have given it.

## Limits

| Area | On Veda |
|------|---------|
| `fork` | The child's descriptions are its own copies, as a program's that another starts are: their status flags (`O_NONBLOCK`) change on one side only, and an `eventfd`, a `timerfd` or an `epoll` instance is the child's own copy, not one both share. The child has all the process's memory: `MADV_DONTFORK` and `MADV_WIPEONFORK` are refused (`EINVAL`), which the C libraries' and cryptography libraries' generators of random numbers take to mean they must detect the child otherwise. A child that `vfork` made cannot `fork` (`ENOSYS`) |
| Symbolic links | None in Veda's files: a Linux root has copies in their place, and `symlink` fails (`EPERM`) |
| Processors | x86-64 programs (`x86_64`), on AMD64: not 32-bit x86 ones, nor ARM64's yet |
| Kernel interfaces | No `io_uring`, `inotify`, `signalfd`, System V IPC (`shmget`, `semget`, `msgget`), `ptrace`, `seccomp`, namespaces, `perf_event_open` or eBPF: each fails (`ENOSYS`, or `EPERM` where Linux would refuse it), said once in the system log |
| Files and memory | What [C on Veda](C.md#limits) lists: no owners or permission bits, shared writable mappings of files refused (`ENODEV`), sockets TCP, UDP and Unix only. `mremap` of a file's mapping or of shared memory only shrinks it (`ENOMEM` otherwise), and one of anonymous memory that moves is copied, which costs for a large one |
| `/proc` and `/sys` | The process's own, the processors, memory and uptime: no other process's, no directory listings (`/proc/self/fd` cannot be listed, though each `/proc/self/fd/N` is there), no devices |
| `arch_prctl` | The FS base only: a GS base of a thread's own is refused (`EINVAL`) |
| A bad pointer in a system call | Where Linux compatibility reads it, `EFAULT`; where the POSIX layer reads it, the process ends with a fault, as a C program's does ([C on Veda](C.md)) |
| The vDSO | None: the C libraries make the system call for the time instead (`clock_gettime`), which costs more |
| A child `vfork` made | A thread of its parent's process until it starts its program: a `kill` for it before then reaches nothing, and the parent's end ends it |

## Performance

A system call costs two passes through the kernel rather than one (the
program's call, then Linux compatibility's run of its code again), and
the POSIX layer's own calls to Veda's services. Programs that make few
calls (computation, most of an interpreter's work) run at the processor's
full speed; programs that read the time very often pay for the missing
vDSO.

## Tests

- **Unit tests** of `implementation/compatibility/linux` run on the host:
  the alternate signal stack, the program's areas, its tree, `/proc`'s
  names.
- **`systest`** runs foreign code itself (`verification/systest/source/foreign.rs`:
  a system call, faults, the thread pointer, interruptions), then the
  Linux programs of `verification/linux`, which Linux's own toolchains
  build (`automation/source/compatibility.rs`) into
  `/system/tests/linux`:

  | Program | Built by | Linked |
  |---------|----------|--------|
  | `checks-musl` | the driver VM's toolchain for Linux on musl | statically |
  | `checks-musl-dynamic` | the same, and musl's dynamic linker, from the same source | dynamically, with the root `/system/tests/linux/musl` |
  | `checks-glibc-static` | the build machine's GCC and glibc, where it has them | statically |
  | `checks-glibc` | the same | dynamically, with the root `/system/tests/linux/glibc`: the build machine's dynamic linker and C library |
  | `checks-rust` | Rust, for Linux on musl as Rust ships it | statically |

  The C program checks its start, its memory, files (`sendfile` and
  `copy_file_range` among them), `/proc` (`/proc/self/fd/N` among it) and
  `/sys`, pipes, threads and
  thread-local storage, signals (handlers and their context, masks, the
  alternate stack, `SIGSEGV` and `SIGILL`, the vector registers kept
  across a handler, a timer ending a blocking call, a call `SA_RESTART`
  starts again, a fault whose handler has no stack), robust mutexes whose
  owner ended, time, sockets, `epoll`,
  futexes, starting programs (`posix_spawn` with a file action, `vfork`,
  `SIGCHLD`), `fork` (the child's memory its own but for shared memory,
  `waitpid`, pipes and a file's offset shared, the handlers, memory wiped
  for the child or the advice refused (`MADV_WIPEONFORK`), `fork` and
  `exec`, children made while another thread allocates, `clone` with
  `SIGCHLD`) and, run again with `exec`, `execve` in place; the Rust program checks the standard library (threads, files,
  time, a panic unwound, memory, TCP on the loopback interface, `Command`).
  Each passes on Linux too, as it does on Veda.
- **`verification/ui/linux-programs.vts`** runs them in the Terminal.
