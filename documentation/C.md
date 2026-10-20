<!-- SPDX-FileCopyrightText: © 2026 Vahid Mohammadi -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# C on Veda

Veda's system image includes GCC 16, GNU binutils and the musl C library,
so C programs are written, compiled and run inside Veda; a cross toolchain
built from the same sources compiles C and C++ for Veda on the development
machine. This document covers using them, the POSIX layer under the C
library, what differs from Linux, and how the toolchain is built.

**Contents:** [Writing and running a program](#writing-and-running-a-program) · [How it works](#how-it-works) · [Limits](#limits) · [The toolchain](#the-toolchain) · [Tests](#tests)

## Writing and running a program

In the Terminal:

```text
user@veda:~$ cd /tmp
user@veda:/tmp$ echo '#include <stdio.h>' > hello.c
user@veda:/tmp$ echo 'int main(void) { printf("Hello from Veda\n"); return 0; }' >> hello.c
user@veda:/tmp$ gcc -O2 -Wall -o hello hello.c
user@veda:/tmp$ ./hello
Hello from Veda
```

Longer programs are written in the Text Editor; your home folder keeps
them across restarts when Veda has its home disk.

- **`gcc`** (also `cc`) makes static ELF64 executables for the System V
  ABI, as on Linux: they need nothing else to run. `-static-pie`, with the
  code compiled `-fPIE`, makes one position-independent, loaded at a
  random address.
- **Binutils**: `as`, `ld`, `ar`, `nm`, `objdump`, `objcopy`, `strip`,
  `readelf`, `size`, `strings`, `addr2line` and `c++filt`.
- **Files**: headers in `/system/include`, the C library (`libc.a`,
  `libm.a`, ...) and its start files in `/system/lib`.
- **Macros**: `__veda__` and `__unix__` are defined, `__linux__` is not.
  The C library's interface is musl's, which is Linux's: code that builds
  on Linux with musl generally builds here, unless it relies on what
  [Limits](#limits) lists.

On the development machine the cross toolchain compiles C, and C++ with
`x86_64-veda-g++` (on AMD64):

```bash
target/toolchain/cross/bin/x86_64-veda-gcc -O2 -Wall -o hello hello.c
```

### In the Terminal

A program runs like any command, found by its path (`./hello`) or on
`PATH` (`/system/bin`). The shell waits for it, and the programs it starts
share its terminal.

- **Output**: `isatty` is true and `TIOCGWINSZ` gives the window's size.
  Standard output is line-buffered, and musl, unlike glibc, does not
  flush it when the program reads its input: a prompt without a newline
  needs `fflush(stdout)`.
- **Input** comes a line at a time, edited in the window (canonical
  mode): a read returns one line at most, and lines typed ahead stay for
  whoever reads next, the shell included. With `termios` a program reads
  keys one by one (`VMIN` and `VTIME` work as POSIX says).
- **Ctrl+C** ends the program, with the programs it started; **Ctrl+D**
  ends its input. `$?` is the exit status, or 128 plus the signal for a
  program that was ended: 130 after Ctrl+C, 134 after `abort`, 139 after a
  crash.
- **Redirections and pipes**: `<`, `>`, `>>`, `2>`, `2>>`, `&>`, `2>&1`,
  `|` and `|&` (errors too) work with programs and built-in commands. The
  shell runs a pipeline's commands one after the other, and a pipe holds
  at most 16 MiB of what one passes the next.

## How it works

**The C library** is musl 1.2.6, almost unchanged: it calls
`__veda_syscall` where it would make a Linux system call, the few entry
points that cannot be C (thread creation, the thread pointer, a thread
freeing its own stack) jump into the POSIX layer, and `posix_spawn` is
Veda's own.

**The POSIX layer** (`implementation/libraries/posix`, crate `posix`)
carries out Linux's system calls with Veda's objects and services. It is
Rust, linked into `libc.a` as one object that exports a few symbols and
nothing else, so no symbol of its code or compiler-builtins can clash with
the C library's. Its memory comes from its own heap, not from `malloc`,
which calls into it.

| A C program uses | On Veda it is |
|------------------|---------------|
| Files, directories | The VFS's open files and listings (`getdents64`, with `.` and `..`). The VFS keeps the offset, which `dup` and child processes share; a file removed while open stays readable. `stat` gives inode numbers and times |
| `/dev/null`, `zero`, `full`, `random`, `urandom`; `/dev/tty`, `/dev/stdin`, `/dev/fd/N` | Devices of the VFS; the program's own descriptors |
| Pipes, the terminal | Kernel sockets: byte streams whose endpoints several processes can share. A shared state page (`proto::tty`) marks the terminal and holds its size and modes |
| TCP and UDP, over IPv4 and IPv6 | Sockets of the network service (`netd`) |
| Unix domain sockets: stream, datagram, seqpacket | Bound to a path, a node of the VFS (`S_IFSOCK`; `unlink` removes it); bound to an abstract name (a `sun_path` that starts with a NUL), a name in the VFS's abstract namespace while open. Connections are kernel sockets (channels for seqpacket) between the two programs |
| Names | `getaddrinfo` and `gethostbyname` ask the network service's resolver (which caches answers, knows `localhost` and puts IPv4 first) after `/etc/hosts`, if there is one; `res_query`, `res_send` and reverse lookups use musl's DNS client with the service's DNS servers |
| `epoll` | A kernel port per instance: a wait costs what changed, however many descriptors are registered. Level-triggered, edge-triggered (as on Linux; each write to an eventfd is an event) and one-shot |
| `eventfd`, `pidfd_open` | A counter with an event (`EFD_SEMAPHORE`, `EFD_NONBLOCK`); the child's process handle, readable once it has ended (how tokio waits for children) |
| `timerfd` | On the monotonic, real-time and boot clocks, one-shot or periodic, absolute or relative (`TFD_TIMER_ABSTIME`): a count of expirations to read, and an event that `poll` and `epoll` wait on, which a thread of the layer signals as timers expire |
| `mmap`, `mprotect`, `munmap`, `madvise` | Private anonymous memory is the kernel's, its pages freed once unmapped (in part too) or dropped with `MADV_DONTNEED`; files are mapped as copies, read-only if shared, but a mapping of a file of `/system` that does not write it is the system image's own pages, shared with every process that maps them and never writable; any part of a mapping can be protected or unmapped |
| Memory files (`memfd_create`) | A VMO: shared mappings are the memory itself, in every process the descriptor goes to (over Unix domain sockets, or to the programs a program starts); `ftruncate` and writes grow it; seals that fix its size (`F_SEAL_SHRINK`, `F_SEAL_GROW`, `F_SEAL_SEAL`, and `F_SEAL_EXEC` with `MFD_NOEXEC_SEAL`); `/dev/fd/N` opened read-only gives a descriptor whose handle cannot write the memory, nor can any process it goes to |
| `fsync`, `fdatasync`, `sync`, `syncfs`, `O_SYNC`, `RWF_SYNC` | The VFS saves changes to the home disk at once, not a few seconds after the last one |
| Threads, futexes, clocks | The kernel's. Thread ids are the process's own, the main thread's its process id; `pthread_join` waits on the thread's exit futex; a detached thread frees its own stack; `pthread_getattr_np` knows the main thread's stack from the startup message |
| Local time | Without `TZ`, the system's offset from UTC (the `tz=` boot option, or the firmware's), as a POSIX `TZ` string (`<+0530>-5:30`): `localtime`, `mktime` and `tzname` follow it; `TZ` overrides it with a POSIX string (there is no tz database yet) |
| `posix_spawn`, `waitpid` | Processes started from the program image. A process id is the kernel object id cut to 30 bits, never that of another child not yet waited for (`EAGAIN` in the rare case none can be had) |

**Sockets** take `bind`, `listen`, `accept4`, `connect`, the send and
receive calls and their message forms, `shutdown`, `getsockname`,
`getpeername`, time-outs, `poll`, `select`, `epoll` and `FIONREAD`.

- **TCP and UDP**: a `connect` that does not wait gives `EINPROGRESS`,
  then `poll` and `SO_ERROR`; `MSG_PEEK`, `MSG_WAITALL`, `MSG_DONTWAIT`
  and `MSG_TRUNC`; the options programs set (`TCP_NODELAY`, keep-alives,
  `SO_LINGER`...). An IPv6 socket reaches IPv4 through mapped addresses
  (`::ffff:a.b.c.d`) unless it is `IPV6_V6ONLY`.
- **Unix domain**: also `socketpair`, `MSG_CMSG_CLOEXEC`, `SO_PEERCRED`,
  `SO_PASSCRED` and autobind. `SCM_RIGHTS` carries files, pipes, the
  terminal, sockets, directories and child processes' descriptors (on a
  stream, with the bytes they were sent with); `SCM_CREDENTIALS` says who
  sent a message.
- **Shared**: sockets go to the programs a program starts and over Unix
  domain sockets, and the processes that hold one share it, as
  pre-forking servers and `inetd` do: each connection, byte or datagram
  goes to whichever takes it first.
- **The firewall** decides which other machines reach a listening or UDP
  socket ([Networking](NETWORKING.md#the-firewall)). A program it keeps
  off the network gets `EACCES` from `connect` and `EAI_FAIL` from
  `getaddrinfo`, and its datagrams to other machines are dropped.

**Veda's own interfaces** are in `<veda/ipc.h>`, for C programs that are
Veda services or talk to them directly: handles, channels, the service
registry, waiting, memory objects and events. The header documents each
call and the protocols' messages, which a program writes itself.
`<veda/thread.h>` stops another thread of the program, reads its
registers (as `mcontext_t`) and lets it go on, as a garbage collector that
scans stacks does (`veda_thread_suspend`, `veda_thread_registers`,
`veda_thread_resume`), `<veda/memory.h>` says how much memory the
process holds (`veda_process_memory`), hands a memory file's memory to
a Veda service (`veda_memory_file`), moves the process to a more
expendable memory class, ended sooner when memory runs out
(`veda_process_set_memory_class`), and
says whether a child was ended for memory rather than crashed
(`veda_child_ended_for_memory`), and `<veda/sandbox.h>` gives up,
for good, all of the file system but a few places, every service but a
few, and creating processes, reading the kernel log and making memory
executable (`veda_confine_files`, `veda_confine_services`,
`veda_process_restrict`), as a program does before it handles what it
cannot trust (the browser's web processes); a program that is not
sandboxed can lend a sandboxed one a place to read
(`veda_lend_files`, `veda_borrow_files`), and take it back
(`veda_take_back_files`), as the browser lends its network process the
folder of a page the user opens while a page open needs it. A
program registers only its own name or names under it (`NAME/...`), its
name being that of the program that speaks for it: `terminal` for a
program the Terminal runs.

## Limits

| Area | On Veda |
|------|---------|
| `fork`, `exec` | None (`ENOSYS`). Processes start with `posix_spawn`, which most build tools and browsers already use, or `vfork` and `exec`, which port to it directly |
| `/bin/sh` | None yet (the Terminal's shell is part of the Terminal): `system` and `popen` fail with `ENOENT` |
| Links | No hard or symbolic links: `link` and `symlink` fail with `EPERM`, and `readlink` finds none (`EINVAL`) |
| Permissions | No owners or permission bits: a program file (ELF or `#!`) is executable, nothing else is; `chmod` and `chown` change nothing. Programs run as user 1000 |
| File locks | None between processes, until the VFS arbitrates them: `F_SETLK`, `F_SETLKW` and `flock` fail with `ENOLCK` (unlocking succeeds), so software relying on them needs porting: SQLite, in the web engine, uses its `unix-none` VFS, as each of its databases is one process's |
| Shared libraries | Static linking only, no `dlopen` |
| Timers | No `alarm` or interval timers: `setitimer` and `timer_create` fail with `ENOSYS`; `timerfd` is there |
| Shared file mappings | Read-only, as changes would never reach the file: `PROT_WRITE` fails with `ENODEV`, and `mprotect` cannot add it (`EACCES`) |
| Network sockets | TCP and UDP only. No raw or ICMP sockets (`EPROTONOSUPPORT`; `ping` is the network service's), multicast (`ENOPROTOOPT`), out-of-band data (`MSG_OOB`: `EOPNOTSUPP` to send, `EINVAL` to receive), ancillary data such as `IP_PKTINFO` (`sendmsg` with control messages: `EOPNOTSUPP`) or list of the interfaces (`getifaddrs` fails: there is no netlink) |
| `/etc/resolv.conf` | None: the C library asks the network service, and programs that read it themselves find no DNS servers |
| C++ | No compiler for it in the image; the cross toolchain compiles C++, with libstdc++ and libatomic |
| OpenGL ES and EGL | For C code linked into a program on Rust's standard library with the `egl` crate, as WebKit is in the browser ([The web browser](BROWSER.md#drawing-pages)); no library for a C program alone yet |
| Full-screen programs | The Terminal shows colours, carriage returns and line erasing, but has no cursor positioning: editors do not display correctly yet |

| Limit | Value |
|-------|-------|
| Descriptors a started program inherits | 62 kernel objects (files, pipes, the terminal) at most, none numbered 256 or more (`EMFILE`): open what children need not have with `O_CLOEXEC` |
| The main thread's stack | 8 MiB, or more if the executable asks (`-Wl,-z,stack-size=N`), up to 512 MiB; guard pages below |
| Pipes and the terminal | 256 KiB each way; writes of up to 4096 bytes are never split |
| A Unix domain datagram | 56 KiB (`EMSGSIZE`) |
| Unix domain connections or datagrams waiting | A few hundred, whatever backlog `listen` asks for |

### Processes and signals

- **`posix_spawn`** passes on what `exec` keeps of the signals (those
  ignored, and the mask), as `POSIX_SPAWN_SETSIGDEF` and
  `POSIX_SPAWN_SETSIGMASK` change them; the other attributes (process
  groups, sessions, ids, scheduling) change nothing.
- **Signals are synchronous.** A program gets those it raises itself
  (`raise`, `abort`, `SIGPIPE`) and those its threads send each other
  (`pthread_kill`, `tgkill`, and the C library's for `pthread_cancel`,
  `setrlimit` and the `set*id` calls), which a thread takes as its next
  system call returns; handlers and per-thread masks work. Signals end a
  wait for signals (`sigsuspend`, `sigwait`, `pause`, a `poll` of nothing)
  or on a lock, a condition or a thread (`EINTR`, after the handler);
  other blocking calls, such as a read from a pipe, carry on, and the
  signal waits for them.
- **`kill`** reaches only the program and its children (`ESRCH`
  otherwise), and does not signal a child: it ends it with a signal that
  ends a process by default, and leaves it alone with another.

### Sockets and epoll

- **A TCP socket binds when it listens**: `bind` records the address
  (`getsockname` says it, port 0 still 0) and `listen` reports what `bind`
  would (`EADDRINUSE`...); one bound before it connects connects from the
  address and port the network service picks.
- **UDP sends** wait for room in the socket's channel to the service, not
  in a send buffer.
- **Not passed on**: a TCP socket still connecting stays with its program
  (`sendmsg` refuses it with `EOPNOTSUPP`), as do epoll instances and
  eventfds; child processes' descriptors can be sent, but do not go to the
  programs a program starts.
- **Shared sockets**: a socket's options, and its being shut down (for
  TCP and UDP, its reading), go as they are when it goes, then change in
  one process only. A message looked at with `MSG_PEEK` stays with the
  process that looked, and a TCP connection's failure goes to the process
  that hears of it first, the others seeing its stream end.
- **Unix domain sockets**: abstract names and autobind are as on Linux.
  A connected socket's copy does not keep the name its socket is bound
  to, and socket nodes are not kept on the home disk. A datagram socket
  connected to another receives from others too. `SO_PEERCRED` and
  `SCM_CREDENTIALS` say the process the other end says it is.
- **`epoll`** takes pipes, sockets, eventfds and child processes'
  descriptors, not files and the like, which are always ready (`EPERM`, as
  Linux says of files), nor an epoll descriptor, which cannot itself be
  polled either. Edge-triggered, a listening or UDP socket is reported
  when a connection or datagram comes to wait where none did, not for
  each one: accepting or receiving until `EAGAIN` misses none.
  `epoll_pwait` leaves the signal mask as it is, as `ppoll` and `pselect`
  do.

## The toolchain

`cargo automation toolchain` builds it once, in about half an hour on a
recent PC (C a quarter of an hour, Rust as long again). It checks the
archives it downloads against the SHA-256 each
`implementation/ports/*/port.toml` pins (their GPG signatures were checked
against the GNU and musl keys when they were pinned), builds the POSIX
layer for the bare target (`x86_64-unknown-none` on AMD64), and runs
`implementation/ports/build.sh`, which patches the sources (each port's
patch, then the architecture's) and builds them with the architecture's
settings (`implementation/ports/architectures/<architecture>/settings.sh`) in
`target/toolchain` (or `$VEDA_TOOLCHAIN`):

| Directory | What it holds |
|-----------|---------------|
| `cross/` | The cross toolchain for this machine (on AMD64 `x86_64-veda-gcc`, `x86_64-veda-g++` with libstdc++, and binutils) and a sysroot laid out like Veda's `/system`. Its C++ compiler also builds the native GCC, which is written in C++ |
| `native/system/` | The toolchain that runs inside Veda: GCC, binutils, GMP, MPFR and MPC, the C library and its headers |
| `media/` | dav1d and FFmpeg, which the video decoder (`mediadec`) links |
| `rust/` | Rust ([Rust on Veda](RUST.md#the-toolchain)) |

Later builds redo what changed: each stage keeps a fingerprint of what
went into it, and where (a toolchain directory that has moved is built
again; where the checkout is does not count, only what its files hold).
When the POSIX layer changes, `cargo automation build` puts it into the
C library and links the native programs (GCC, binutils) again, without
recompiling them. It installs the C test programs in
`/system/tests/c` and the native toolchain in `/system`: about 80 MB,
which Veda keeps in memory (compiling is comfortable in QEMU's default
2 GiB).

### Packages

`cargo automation toolchain` checks for these first and names any that
are missing.

| For | Tools | Debian and Ubuntu packages |
|-----|-------|----------------------------|
| Compiling | `gcc`, `g++`, `make`, `patch`, `m4`, `bison`, `flex`; GMP, MPFR, MPC | `build-essential`, `m4`, `bison`, `flex`, `libgmp-dev`, `libmpfr-dev`, `libmpc-dev` |
| Downloading | `curl` | `curl` |
| The media libraries, which decode videos | `meson`, `ninja`; on AMD64 `nasm` 2.14 or newer | `meson`, `ninja-build`, `nasm` |
| Rust's LLVM and build program | `cmake`, `python3` | `cmake`, `python3` |

```bash
sudo apt install build-essential m4 bison flex curl libgmp-dev libmpfr-dev libmpc-dev nasm meson ninja-build cmake python3
```

### The ports

| Port | Version | What changes for Veda |
|------|---------|-----------------------|
| `binutils` | 2.47 | Veda in `config.sub`; the `x86_64-veda` target (ELF, `elf_x86_64`, separate code pages) |
| `gcc` | 16.2.0 | Veda's target (`gcc/config/veda.h`: musl, static linking, `/system`), libgcc's configuration, libstdc++'s checks of the C library, libatomic; `x86_64-veda` (the System V x86-64 ABI) |
| `gmp`, `mpfr`, `mpc` | 6.3.0, 4.2.2, 1.4.1 | `config.sub` knows Veda |
| `musl` | 1.2.6 | Its hooks into the POSIX layer (system calls, `_start`, thread entry points, `__init_libc`, `posix_spawn`, the main thread's stack, the time zone), name lookups, the program's path in `dl_iterate_phdr` |
| `rust` | 1.99.0 | Veda's target in the compiler, the standard library on the C library |
| `dav1d` | 1.5.4 | None: built as released (meson) |
| `ffmpeg` | 9.0.2 | `veda` as a target operating system in `configure`; only the demuxers, decoders and parsers the media decoder uses |

Each port is a directory with `port.toml` (version, address, SHA-256,
licence), `veda.patch` and, for what is the architecture's,
`architectures/<architecture>/veda.patch`;
[`implementation/ports/README.md`](../implementation/ports/README.md)
explains the patches and how to update a port. The ports keep their own
licences ([Licensing](LICENSING.md)), whose texts the image carries in
`/system/licences`: the source of what the image contains is each
`port.toml`'s archive plus the patches beside it.

## Tests

- **`verification/c/posix.c`** checks the C library and the POSIX layer:
  startup, standard I/O, memory, the system image's files mapped in place,
  memory files, suspending threads, files, pipes, threads, clocks and the
  time zone, signals, `posix_spawn`, the network (the host's servers too,
  through QEMU), `timerfd`, `epoll`, `eventfd`, Unix domain sockets, and
  sockets shared with child processes.
- **`verification/c/cxx.cc`** checks the C++ library: exceptions (across
  threads too), run-time types, static constructors, containers, streams,
  threads.
- **`systest`** runs them inside Veda with `verification/c/hello.c`, each
  linked at a fixed address and position-independent, and tests the
  kernel's sockets, memory protection, private memory, exit futexes and
  the VFS's open files.
- **`verification/ui/c-programs.vts`** runs programs in the Terminal
  (exit statuses, an interactive program, Ctrl+C and Ctrl+D, pipes and
  files), and **`c-compile.vts`** compiles one with GCC inside Veda.
- **Unit tests** of `implementation/libraries/elf` and
  `implementation/libraries/posix` run on the host.

`cargo automation test` runs the unit tests and `systest`, `--ui` adds
the scripts, and what needs the toolchain is skipped when it has not been
built.
