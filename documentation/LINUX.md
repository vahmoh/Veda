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

Programs for Linux on x86-64 (`x86_64`) run on AMD64, and programs for
Linux on ARM64 (`aarch64`) on ARM64: linked statically or dynamically, on
any C library: glibc, musl, Go's and Rust's runtimes, programs with none.
What Veda's tests run is in [Tests](#tests).

| A Linux program uses | On Veda |
|----------------------|---------|
| Its files, the user's | The VFS: the user's home, `/tmp` and `/dev` are Veda's own; the rest of its tree is its Linux root's ([A Linux root](#a-linux-root)) |
| Threads, thread-local storage, futexes, robust mutexes | Kernel threads, the thread pointer it sets (`arch_prctl` on x86-64, `TPIDR_EL0` on ARM64), kernel futexes |
| Signal handlers, `sigaltstack`, `SA_RESTART`, `SIGSEGV`, `SIGBUS`, `SIGILL` and `SIGTRAP` handlers | Linux's signal frame (`rt_sigframe`) of the architecture, on its stack or the alternate one, the vector registers in it; a fault is the signal it stands for |
| Its memory: `mmap` and friends, `brk`, `mremap` | The kernel's, kept apart from Linux compatibility's own |
| The vDSO: `clock_gettime`, `gettimeofday`, `clock_getres` (and `time`, `getcpu` on x86-64; a signal handler's return on ARM64) | An image of Linux's, which reads the time in the program's own code, without a system call, as Veda's programs read it |
| Pipes, sockets (TCP, UDP, Unix), `epoll`, `eventfd`, `timerfd`, memory files | What Veda's C programs have ([C on Veda](C.md)) |
| Starting programs: `posix_spawn`, `vfork`, `system`, `popen` | A process of Veda's for each, which `execve` starts, of a Linux program or of Veda's |
| `fork` | A copy of the process, its memory copied as either writes it (copy-on-write), as Linux makes it; a shell that forks for each command, a server that forks its workers |
| `execve` of a Linux program | In place: the same process, the same id |
| `SIGCHLD` | Sent as a child ends, saying who and how |
| `/proc/self` (`exe`, `maps`, `stat`, `status`, `cmdline`, `fd/N`...), `/proc/cpuinfo`, `meminfo`, `/sys`'s processors | Made as they are read |
| `uname` | Linux 6.6 (`6.6.0-veda`), on `x86_64` or `aarch64` |
| `AT_HWCAP` and `/proc/cpuinfo`'s features | The processor's, as its ID registers say on ARM64, its CPUID on x86-64 |

A Linux program and Veda's own programs share the user's files and the
terminal, and start one another: a Linux shell runs Veda's commands and
Veda's Terminal runs Linux programs.

## A Linux root

A program linked dynamically needs its dynamic linker and libraries where
it names them (`/lib64/ld-linux-x86-64.so.2`, `/lib/x86_64-linux-gnu`;
`/lib/ld-linux-aarch64.so.1`, `/lib/aarch64-linux-gnu` on ARM64),
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
program's own, which it sets with `arch_prctl` on x86-64 and itself in
`TPIDR_EL0` on ARM64, and Linux compatibility's); the vector registers
(x87, SSE and AVX; ARM64's floating-point and SIMD registers) stay the
program's, as Linux compatibility, built soft-float, never touches them:
on ARM64 only the code that saves them in a signal frame and loads them
back does, and the POSIX layer's signal entry, which keeps them. So the
program's system calls reach no part of the kernel: what it asks is asked
by Linux compatibility, of the kernel's objects and Veda's services, with
its process's handles.

**System calls.** Most of Linux's system calls are those Veda's C programs
make through their C library, which the POSIX layer carries out
([C on Veda](C.md)): Linux compatibility hands them to the same layer
(`posix::host`), by the architecture's numbers: x86-64's table, or on
ARM64 Linux's generic one, which has no older forms of calls (`open`,
`stat`, `fork`: `openat`, `newfstatat` and `clone` in their place), its
own argument order for `clone` (the thread pointer before the child's id
word) and its own numbers (`rt_sigreturn` is 139). What only Linux
programs make is Linux compatibility's own (`crate::syscall`):

| Calls | What Linux compatibility does |
|-------|-------------------------------|
| `clone`, `clone3` | A thread: a kernel thread that runs the program from the call, its stack and thread pointer as asked, its ids written where asked, its `CLEARTID` word cleared and woken as it ends (the kernel's exit futex) |
| `vfork`, `clone` with `CLONE_VM` | A child in the program's memory, on a thread of its own, with copies of the descriptors, working directory and signal handlers (but those shared) and an id of its own (that of a process made for it), while its parent waits: its `execve` starts the program in that process, its `_exit` ends it as a process whose status the parent waits for |
| `fork`, `clone` and `clone3` without `CLONE_VM` | A copy of the process (below), which sends `SIGCHLD` as it ends: the ids written as asked (`CLONE_PARENT_SETTID` in the parent, `CLONE_CHILD_SETTID` in the child, `CLONE_CHILD_CLEARTID` cleared as its thread ends), and a pidfd (`CLONE_PIDFD`). Sharing what two processes cannot (`CLONE_FILES`, `CLONE_FS`, `CLONE_SIGHAND`, `CLONE_SETTLS`), another signal at its end, and `CLONE_VFORK` are refused (`EINVAL`) |
| `execve`, `execveat` | A Linux program in place: the other threads end, the memory and the handlers go, the descriptors marked close-on-exec close. A program of Veda's runs in a process of its own, which this one waits for and ends as |
| `rt_sigreturn`, `sigaltstack` | The registers, vector registers, mask and alternate stack a handler's frame holds, checked first as Linux checks them |
| `mmap`, `munmap`, `mprotect`, `mremap`, `madvise`, `brk` | The program's areas, which it alone changes: `MAP_FIXED` never replaces Linux compatibility's own memory, nor `madvise` gives it up or keeps it from a child |
| `arch_prctl` (x86-64), `set_robust_list` | The thread's own: its thread pointer, its robust futexes (released, `FUTEX_OWNER_DIED`, as it ends) |
| `sendfile`, `copy_file_range` | A copy a read and a write at a time, with Linux's rules: the offsets move by what went, and `copy_file_range` is between regular files only, never onto an overlapping part of the same file |
| `uname`, `personality`, `prctl`, `capget` | Linux's answers, those of a program without privileges |
| `mount`, `ptrace`, `chroot`, `unshare`, `reboot`, setting the clock | Refused (`EPERM`), as Linux refuses a program without privileges |

**Signals.** The POSIX layer keeps what Linux's kernel keeps of signals;
Linux compatibility acts on them as Linux does on the way back to a
program's code: as each system call returns, and as the kernel ends the
program's code for an exception or an interruption. A handler is entered
with Linux's frame of the architecture laid out below the stack pointer
(or at the top of the alternate stack), what the signal says and the
registers the code had, which its `rt_sigreturn` takes back, checked
first as Linux checks them:

- On x86-64, the vector registers as `XSAVE` saves them, with Linux's
  markers, and a return to the handler's restorer (`SA_RESTORER`), which
  every C library gives: a handler without one has nowhere to return to.
  `rt_sigreturn` refuses what the processor would.
- On ARM64, `struct rt_sigframe` with its context's records: the
  floating-point and SIMD registers (`fpsimd_context`), the syndrome of
  the fault that raised the signal (`esr_context`), and the end; then a
  frame record of the code's frame pointer and link register, where the
  handler's frame pointer points, for unwinders. The handler returns to
  its restorer, or without one (glibc and Go give none) to the vDSO's
  `__kernel_rt_sigreturn`, as Linux has it. `rt_sigreturn` refuses a
  `pstate` user code cannot have (another exception level, masked
  interrupts) and records it does not know, as Linux does.

A handler whose frame cannot be had (its stack is not there) is `SIGSEGV`
instead, and for `SIGSEGV`'s own handler the end of the process, as
Linux's `force_sigsegv` has it. A blocking call a signal ended starts
again after the handler if it had `SA_RESTART`, as on Linux, by going
back over the `syscall` (or `svc`) instruction, its first argument in
`x0` again on ARM64. A fault is the signal Linux raises for it, with its
`si_code` and address: on ARM64 by its exception class and status code,
a bad access `SIGSEGV` (`SEGV_ACCERR` where the program has an area that
does not allow the access, `SEGV_MAPERR` where it has none), a
misaligned one `SIGBUS`, an undefined instruction (`UDF`) `SIGILL`, a
breakpoint (`BRK`) `SIGTRAP`; the kernel's addresses are reported as
Linux reports them, saying nothing of what is there. A legacy virtual
system call (x86-64's `vsyscall` page) is carried out as Linux's
emulation does.

**The vDSO.** Linux gives every program a shared object of its own, the
vDSO, where the auxiliary vector says (`AT_SYSINFO_EHDR`), whose
functions glibc, musl and Go call for the time rather than make the
system call: Linux compatibility does too (`crate::vdso`). Its image is
made as Linux's linker makes Linux's, an ELF shared object with its
symbols, both hash tables (`DT_HASH`, `DT_GNU_HASH`) and their version,
so that each finds Linux's functions its own way: on x86-64
`__vdso_clock_gettime`, `__vdso_gettimeofday`, `__vdso_time`,
`__vdso_clock_getres` and `__vdso_getcpu` (of `LINUX_2.6`), on ARM64
`__kernel_clock_gettime`, `__kernel_gettimeofday`,
`__kernel_clock_getres` and `__kernel_rt_sigreturn` (of `LINUX_2.6.39`).
Its call frame information lets an unwinder through them, and out of a
signal handler through `__kernel_rt_sigreturn`, which is Linux's to the
instruction, as unwinders that look for it there expect. Its code reads
the processor's clock counter (the TSC, or ARM64's virtual count
`CNTVCT_EL0`, each reading after every earlier load has completed, so
that a time read after another thread's never comes before it) and
scales it by the scale the kernel gives (`abi::ClockInfo`), on a data page
below the image: the
one Linux compatibility's own clocks are read with, for the program's
system calls. So the vDSO and the system call agree to the nanosecond
for as long as the program runs: the scale never changes, and the time
is counted from the clocks' start each time, never from a time taken
once. Its clocks are those the POSIX layer reads from the counter
(`posix::host::kernel_clock`): `CLOCK_REALTIME` and its coarse one are
Veda's UTC clock, as `clock_gettime` gives it, and the monotonic, raw,
coarse and boot-time clocks Veda's monotonic one. The rest (the clocks
of CPU time, a time zone, the processor's number) its code asks for with
the system call, as a C library without a vDSO would. The loader maps
the image and its data page into each program, and again in place of the
old program's as `execve` replaces it; they are the program's areas
(`[vdso]` and `[vvar]` in `/proc/self/maps`), its own memory, which
`fork` copies and the program may unmap, as on Linux, and its data page
can never be made writable (`mprotect` fails with `EACCES`, as on
Linux).

**Memory.** Linux compatibility keeps account of the program's areas, as
Linux keeps a process's: what it mapped, its image, its stack, its
program break and its vDSO, which `/proc/self/maps` lists. A system call
reaches only those: `munmap` of a range unmaps the program's areas in
it, `mprotect` and `madvise` need them whole (`ENOMEM`), shrinking the
program break unmaps only its own, and memory Linux compatibility reads
or writes for the program (a signal frame, `execve`'s arguments) must be
in one that allows the access while it does, or it is a fault. `mremap`
grows and moves only private anonymous memory, in place where the pages
after the area are free (one area still, as on Linux), or else by
copying it: a file's mapping or shared memory would be neither any more
(`ENOMEM`, as when there is no room, which programs answer by mapping
anew).

**Copies.** `fork` has the kernel copy the process (`process_fork`):
its memory, the program's and Linux compatibility's alike, is the
parent's as it was, each page one for both until either writes it, which
then has a copy of its own (as Linux's copy-on-write has it), but for
shared memory (`MAP_SHARED`), which both see, and for what `madvise`
said, which an area keeps as `mremap` grows or moves it: the child has
nothing of an area kept from it (`MADV_DONTFORK`), and zeros where one
was wiped for it (`MADV_WIPEONFORK`). Its one thread is the one that
forked, which goes on from the call there, its thread id the child's
process id, its alternate signal stack its own, and its robust futex
list none, as on Linux (the C libraries set it again). The POSIX layer
gives the child what Linux's `fork` gives it of the rest, as for a C
program ([C on Veda](C.md#processes-and-signals)): the same open files,
pipes and sockets, the handlers and the mask, no pending signals, timers,
record locks or children. The program's areas are the child's too, as
`/proc/self/maps` lists them, but those kept from it.

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
  gains nothing that way that its process had not. It is at a random
  address on AMD64; on ARM64 at a fixed one, out of the program's way, as
  the soft-float target it is built for has no `core` for code that can
  be anywhere.
- **Its vDSO** is the program's own memory, in no other process, which
  holds no more than the program could ask the kernel for: its code, and
  the clocks' scale, which it can read but never write.
- **Its copies.** A child `fork` makes can do what its parent could:
  what it gave up stays given up (`process_restrict`), and it has only
  the handles its parent gave it and the memory its parent's handles
  could have given it.

## Limits

| Area | On Veda |
|------|---------|
| `fork` | The child's descriptions are its own copies, as a program's that another starts are: their status flags (`O_NONBLOCK`) change on one side only, and an `eventfd`, a `timerfd` or an `epoll` instance is the child's own copy, not one both share. A child that `vfork` made cannot `fork` (`ENOSYS`) |
| Symbolic links | None in Veda's files: a Linux root has copies in their place, and `symlink` fails (`EPERM`) |
| Processors | x86-64 programs (`x86_64`) on AMD64, ARM64 programs (`aarch64`) on ARM64: each architecture's own, not 32-bit x86 or Arm ones |
| ARM64's extensions | No SVE, SME, MTE, pointer authentication or BTI for programs, as `AT_HWCAP` says; nor reading the processor's ID registers, which Linux emulates (`HWCAP_CPUID`, not told): `SIGILL` |
| Kernel interfaces | No `io_uring`, `inotify`, `signalfd`, System V IPC (`shmget`, `semget`, `msgget`), `ptrace`, `seccomp`, namespaces, `perf_event_open` or eBPF: each fails (`ENOSYS`, or `EPERM` where Linux would refuse it), said once in the system log |
| Files and memory | What [C on Veda](C.md#limits) lists: no owners or permission bits, shared writable mappings of files refused (`ENODEV`), sockets TCP, UDP and Unix only. `mremap` of a file's mapping, of shared memory or of the vDSO only shrinks it (`ENOMEM` otherwise), and one of private anonymous memory that moves is copied, which costs for a large one |
| `/proc` and `/sys` | The process's own, the processors, memory and uptime: no other process's, no directory listings (`/proc/self/fd` cannot be listed, though each `/proc/self/fd/N` is there), no devices |
| `arch_prctl` | The FS base only: a GS base of a thread's own is refused (`EINVAL`) |
| A bad pointer in a system call | Where Linux compatibility reads it, `EFAULT`; where the POSIX layer reads it, the process ends with a fault, as a C program's does ([C on Veda](C.md)) |
| A child `vfork` made | A thread of its parent's process until it starts its program: a `kill` for it before then reaches nothing, and the parent's end ends it |

## Performance

A system call costs two passes through the kernel rather than one (the
program's call, then Linux compatibility's run of its code again), and
the POSIX layer's own calls to Veda's services. Programs that make few
calls (computation, most of an interpreter's work) run at the processor's
full speed, and those that read the time often (a server's timestamps, a
runtime's timers, a game's frames) read it in their own code, through
the vDSO, at the cost of a few dozen instructions: no system call.

## Tests

- **Unit tests** of `implementation/compatibility/linux` run on the host:
  the alternate signal stack, the program's areas, its tree, `/proc`'s
  names, ARM64's signal frame (laid out as Linux's, taken back, refused
  where Linux refuses it) and faults' signals, on any host, and the vDSO:
  its image read as glibc, musl, Go, an unwinder and a debugger read it
  (x86-64's and ARM64's, its signal return a signal frame's), and its
  code run as a program runs it, its data page set as the tests choose
  and its system calls the host's.
- **`systest`** runs foreign code itself (`verification/systest/source/foreign.rs`:
  a system call, faults, the thread pointer, interruptions), then the
  Linux programs of `verification/linux`, which Linux's own toolchains
  build for the architecture (`automation/source/compatibility.rs`) into
  `/system/tests/linux`:

  | Program | Built by | Linked |
  |---------|----------|--------|
  | `checks-musl` | the driver VM's toolchain for Linux on musl | statically |
  | `checks-musl-dynamic` | the same, and musl's dynamic linker, from the same source | dynamically, with the root `/system/tests/linux/musl` |
  | `checks-glibc-static` | the build machine's GCC and glibc, where they are the architecture's | statically |
  | `checks-glibc` | the same | dynamically, with the root `/system/tests/linux/glibc`: the build machine's dynamic linker and C library |
  | `checks-rust` | Rust, for Linux on musl as Rust ships it | statically |

  The C program checks its start, its memory, files (`sendfile` and
  `copy_file_range` among them), `/proc` (`/proc/self/fd/N` among it) and
  `/sys`, pipes, threads and
  thread-local storage, signals (handlers and their context, masks, the
  alternate stack, `SIGSEGV` and `SIGILL`, the vector registers kept
  across a handler, a timer ending a blocking call, a call `SA_RESTART`
  starts again, a fault whose handler has no stack; on ARM64 the frame's
  context and records, its frame record, `SIGTRAP` of `BRK` whose handler
  goes on past it, `SIGBUS` of a misaligned exclusive access, a handler
  with no restorer of its own and a frame `rt_sigreturn` refuses), robust
  mutexes whose
  owner ended, time, the vDSO (where the auxiliary vector and
  `/proc/self/maps` say, its functions found through either hash table
  by version, its clocks the system call's to the nanosecond, those it
  does not read too, a monotonic clock that no thread reads going back,
  its data page read-only, a child's, and the new program's after
  `execve` in place), sockets, `epoll`,
  futexes, starting programs (`posix_spawn` with a file action, `vfork`,
  `SIGCHLD`), `fork` (the child's memory its own but for shared memory,
  which its `mremap` leaves shared, `waitpid`, pipes and a file's offset
  shared, the handlers, memory wiped for the child (`MADV_WIPEONFORK`) and
  memory kept from it (`MADV_DONTFORK`), which its `/proc/self/maps` does
  not list, `fork` and `exec`, children made while another thread
  allocates, `clone` with `SIGCHLD`) and, run again with `exec`, `execve` in place; the Rust program checks the standard library (threads, files,
  time, its monotonic clock across threads, a panic unwound, memory, TCP
  on the loopback interface, `Command`).
  Each passes on Linux too, as it does on Veda.
- **`verification/ui/linux-programs.vts`** runs them in the Terminal.
