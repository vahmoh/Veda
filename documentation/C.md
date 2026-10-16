<!-- SPDX-FileCopyrightText: © 2026 Vahid Mohammadi -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# C on Veda

Veda runs C programs and builds them itself: the system image comes with
GCC 16, GNU binutils and the musl C library, so a C program can be written,
compiled and run inside Veda, in an emulator or on a PC booted from a USB
stick alike. The same toolchain, built to run on the development machine,
cross-compiles C and C++ programs for Veda.

This document covers using it, how it works, and how it is built. The
goal is a standard environment for porting software written in C (and,
later, C++): browser engines, drivers, tools.

## Writing, compiling and running a program

In the Terminal:

```text
user@veda:~$ cd /tmp
user@veda:/tmp$ echo '#include <stdio.h>' > hello.c
user@veda:/tmp$ echo 'int main(void) { printf("Hello from Veda\n"); return 0; }' >> hello.c
user@veda:/tmp$ gcc -O2 -Wall -o hello hello.c
user@veda:/tmp$ ./hello
Hello from Veda
user@veda:/tmp$ echo $?
0
```

The Text Editor writes longer programs as well (save them in your home
folder: `/home` is kept across restarts when Veda has its home disk, and in
memory on the live system).

* `gcc` (also `cc`) compiles and links. Programs are linked statically: the
  executable holds what it uses of the C library and needs nothing else
  to run. `-static-pie`, with the code compiled `-fPIE`, makes a
  position-independent executable instead.
* `as`, `ld`, `ar`, `nm`, `objdump`, `objcopy`, `strip`, `readelf`, `size`,
  `strings`, `addr2line` and `c++filt` are GNU binutils.
* The headers are in `/system/include`, the C library (`libc.a`, `libm.a`,
  ...) and its start files in `/system/lib`, GCC's own files in
  `/system/lib/gcc` and `/system/libexec/gcc`.
* `__veda__` and `__unix__` are defined; `__linux__` is not. The C
  library's interface is musl's, which is Linux's: code that builds on
  Linux with musl generally builds here, except where it relies on what
  is listed under [Limits](#limits).

A program runs in the Terminal like any command: found by its path
(`./hello`) or on `PATH` (`/system/bin`). Its standard input, output and
error are the terminal: `isatty` is true and `TIOCGWINSZ` gives the
window's size. Standard output is line-buffered there, as everywhere: a
prompt without a newline shows once the program calls `fflush(stdout)`
(musl, unlike glibc, does not flush it when the program reads its input).
The shell waits for the program and `$?` is its exit status (128 plus the
signal for a program that was ended: 130 after Ctrl+C, 134 after `abort`,
139 after a crash). Keys typed while it runs go to it a line at a time,
edited in the window, as on a Unix terminal in canonical mode: a read
returns one line at most, and lines typed ahead stay for whoever reads
next: the program, a program it started, or the shell once it has ended.
A program can turn that off with `termios` and read keys one by one
(`VMIN` and `VTIME` work as POSIX says). Ctrl+C ends it, with the programs
it started; Ctrl+D ends its input. `<`, `>`, `>>`, `2>`, `2>>`, `&>` and
`2>&1` give it files, `|` (and `|&`, which takes its errors too) connects
it with other programs and built-in commands; a pipe holds at most 16 MiB
of what one command passes the next.

## How it works

**Executables** are ELF64 files for the System V ABI of the processor (on
AMD64 x86-64's), as on Linux; so are Veda's own (Rust) programs, which a
note marks (`velf::veda`). `vrt::process::Spawn` starts both kinds. For a
POSIX program it maps the segments (`implementation/libraries/elf`
validates them and groups the pages by permissions), at a random address
if the program is position-independent (it relocates itself), gives it an
8 MiB stack with guard pages below, and lays out
the top of the stack as Linux does: arguments, environment and the
auxiliary vector (program headers, entry point, page size, 16 random bytes,
the hardware capabilities). It also sends the startup message every Veda
process gets, which for an ELF program carries the working directory and
the handles: the registry, the terminal, and the files and streams that
become its descriptors (role `FD + n`); and, from a POSIX program that
starts another, what the new one's POSIX layer inherits that is no handle
(the signals it ignores, its signal mask, descriptors of directories:
`implementation/libraries/posix/src/inherit.rs`).

**The C library** is musl 1.2.6 (`implementation/ports/musl`). It is Linux's
C library almost unchanged: where it would make a Linux system call, it
calls `__veda_syscall`, and a handful of entry points that cannot be C
(thread creation, the thread pointer, a thread freeing its own stack) jump
into the POSIX layer. `_start` stores the bootstrap channel the kernel
passes, and `__init_libc` calls the POSIX layer to set itself up before any
constructor runs. `posix_spawn` is Veda's own (there is no `fork`). Names
are the network service's to look up: `getaddrinfo` and `gethostbyname`
ask its resolver, and musl's own DNS client asks the DNS servers it uses.

**The POSIX layer** (`implementation/libraries/posix`, crate `vposix`) is
written in Rust and carries out Linux system calls with Veda's objects and
services:

| What a C program uses | What it is on Veda |
|-----------------------|--------------------|
| files | the VFS's open files: each a connection of the `file` protocol, the offset kept by the VFS (`dup` and child processes share it); files removed while open stay readable |
| directories | the VFS's listings (`getdents64`), `.` and `..` included |
| `/dev/null`, `zero`, `full`, `random`, `urandom` | devices of the VFS |
| `/dev/tty`, `/dev/stdin`, `/dev/fd/N` | the program's own descriptors |
| pipes, the terminal | socket endpoints: kernel byte streams that several processes can share |
| TCP and UDP sockets, over IPv4 and IPv6 | sockets of the network service (`netd`), each a channel to it, a connection's bytes going on a socket endpoint between the program and the service, as a pipe's do: `connect` (waiting for the connection, or `EINPROGRESS` and then `poll` and `SO_ERROR`), `bind`, `listen`, `accept4`, `send`, `recv` and their message forms (`MSG_PEEK`, `MSG_WAITALL`, `MSG_DONTWAIT`, `MSG_TRUNC`), `shutdown`, `getsockname`, `getpeername`, and the options of sockets, TCP and IP that programs set (time-outs, `TCP_NODELAY`, keep-alives, `SO_LINGER`, `SO_ERROR`...); `poll`, `select` and `FIONREAD` on them. An IPv6 socket reaches IPv4 too, through mapped addresses (`::ffff:a.b.c.d`), unless it is `IPV6_V6ONLY`. Sockets go to the programs a program starts and over Unix domain sockets, and the processes that hold one share it, as pre-forking servers and `inetd` share theirs: each connection goes to whichever accepts it first, each byte to whichever reads it and each datagram to whichever receives it |
| Unix domain sockets: stream, datagram and seqpacket | a socket bound to a path is a node of the VFS (`stat` says `S_IFSOCK`, `unlink` takes it away), which hands it the connections to it and the datagrams sent to it; one bound to an abstract name (Linux's: a `sun_path` that starts with a NUL) has it in the VFS's abstract namespace while it is open, and binding the family alone picks one (autobind). A connection is a socket endpoint, as a pipe is, or for seqpacket a channel, between the two programs; `socketpair` makes one directly. `bind`, `listen`, `accept4`, `connect`, the send and receive calls and their message forms (`MSG_PEEK`, `MSG_WAITALL`, `MSG_TRUNC`, `MSG_CMSG_CLOEXEC`), `shutdown`, `getsockname`, `getpeername`, `SO_PEERCRED`, `SO_PASSCRED`, the time-outs; `poll`, `epoll` and `FIONREAD` on them. Descriptors travel with what is sent (`SCM_RIGHTS`): files, pipes, the terminal, sockets (Unix domain ones of every kind, TCP and UDP ones), directories and child processes' descriptors; on a stream with the bytes they were sent with, as on Linux, whichever process holds it; who sent a message comes with it when asked (`SCM_CREDENTIALS`). Every Unix domain socket goes to the programs a program starts too, and the processes that hold one share it: a listener's connections, and a datagram socket's datagrams, go to whichever takes them first |
| `getaddrinfo`, `gethostbyname` | the network service's resolver, which caches answers, knows `localhost` and gives the addresses in the order they are best tried in (IPv4 first); `/etc/hosts` first, if there is one |
| `res_query`, `res_send`, `getnameinfo`'s reverse lookups | musl's DNS client, over the layer's sockets, to the DNS servers the network service uses (those DHCP gave, or those set in Settings): there is no `/etc/resolv.conf` |
| `epoll` | the layer's interest lists, each with a port of the kernel's, which its descriptors' objects signal: a wait costs what changed, however many descriptors are registered. Level-triggered, edge-triggered (as on Linux: a descriptor is reported again once a read or write found it not ready, or emptied or filled it, and it is ready anew; each write to an eventfd is an event) and one-shot registrations of pipes, sockets, eventfds and child processes' descriptors |
| `eventfd` | a counter of the layer's, with an event object for `poll`, `epoll` and blocking reads and writes (`EFD_SEMAPHORE`, `EFD_NONBLOCK`) |
| `pidfd_open` | a child process's handle: readable once the child has ended (what tokio waits for its children with) |
| `isatty`, window size, `termios` | the terminal's shared state page (`vproto::tty`) |
| `mmap`, `mprotect`, `munmap`, `madvise` | private anonymous memory is the kernel's private memory, whose pages are freed as soon as they are unmapped (parts of mappings too) or dropped with `MADV_DONTNEED`; files are mapped as copies, read-only if shared (through a handle that cannot write, so `mprotect` cannot make them writable: changes would never reach the file); any part of a mapping can be protected or unmapped |
| `fsync`, `fdatasync`, `sync`, `syncfs`, `O_SYNC`, `RWF_SYNC` | the VFS saves what changed to the home disk at once (it otherwise does so a few seconds after the last change) |
| threads (`pthread_create`) | kernel threads; the thread pointer in `FS`; thread ids of the process's own, the main thread's its process id; `pthread_join` through the thread's exit futex; a detached thread frees its own stack as it ends |
| futexes | kernel futexes |
| `posix_spawn`, `waitpid` | processes started from the program image, with the descriptors the file actions leave, the signals ignored and the signal mask (as the attributes set them); a process id is its kernel object id cut to 30 bits, and a child never gets the id of another child not yet waited for (`EAGAIN` in the rare case none can be had) |
| signals | `raise`, `abort`, `SIGPIPE`, `pthread_kill` and handlers, per-thread masks |
| clocks, sleeping | the kernel's clocks |
| `stat` | the VFS's metadata: inode numbers, times, and "executable" for programs |

The layer is linked into `libc.a` as one object that exports these few
symbols and nothing else, so no symbol of the Rust code (or of its
compiler-builtins) can clash with the C library's. Its memory comes from
its own heap (`vrt`'s), not from `malloc`, which calls into it.

**Veda's own interfaces** are there too, for C programs that are Veda
services or talk to them directly (`<veda/ipc.h>`): handles, channels and
the service registry (`veda_service_register`, `_accept`, `_connect`;
a program registers only names of its own, its name or names under it,
`NAME/...`, where its name is that of the program that speaks for it: the
Terminal's, `terminal`, for a program the Terminal runs),
waiting on several objects at once (`veda_wait`), memory objects
(`veda_vmo_create`, `_map`) and events, the handles a program was
started with for a role of its own (`veda_startup_handle`: `mediadec`
takes its control channel so, from Videos, which starts it with nothing
else), and a registry channel of the program's own, for a library that
speaks the registry's protocol itself (`veda_registry`: Veda's libraries
in a Rust program on the standard library). They are the kernel's calls
but for the registry's, made by the POSIX layer
(`implementation/libraries/posix/src/native.rs`); every call returns 0 or a
count, or a negative Veda error number. A program speaks a protocol by
writing its messages itself: a 12-byte header (method, transaction, request
or response), then the values in order, as the header describes.

**The Terminal** runs a program on a fresh socket pair: the program's
descriptors 0, 1 and 2 are one end, the window keeps the other. The
terminal's state page tells programs which socket is the terminal, its
size and its modes (`ICANON`, `ECHO`, `ISIG`), which the Terminal follows
when keys are typed. Programs a program starts inherit all of it.

**The kernel** has what a POSIX environment needs: sockets (byte streams
with shareable endpoints, a bounded buffer, all-or-nothing writes up to
4096 bytes, half-closing for the end of input, and handles that travel
with the bytes they were written with, as descriptors do over Unix domain
sockets), memory protection of
parts of mappings that never exceeds what the VMO's handle allows, private
memory whose pages can be freed while it stays mapped (what allocators do
with `madvise`), and each thread's exit futex (a word the kernel clears,
and wakes a waiter on, once the thread has left its stack).

## Limits

* **No `fork` or `exec`.** Processes are started with `posix_spawn`.
  Software that forks needs porting to it; most build tools and browsers
  already use `posix_spawn` or `vfork`+`exec`, which ports to it directly.
  A started program inherits at most 62 descriptors that are kernel
  objects (files, pipes, the terminal), and none numbered 256 or more
  (either fails with `EMFILE`): open files that children need not have
  with `O_CLOEXEC`. It inherits what `exec` keeps of the signals (those
  ignored, and the mask), as `POSIX_SPAWN_SETSIGDEF` and
  `POSIX_SPAWN_SETSIGMASK` change them; the other attributes (process
  groups, sessions, ids, scheduling) change nothing.
* **No `/bin/sh` yet.** The Terminal's shell is part of the Terminal, so
  `system` and `popen`, which run their command with `/bin/sh -c`, fail
  with `ENOENT`.
* **Signals are synchronous.** A program gets the signals it raises
  itself (`raise`, `abort`, `SIGPIPE`) and those its threads send each
  other (`pthread_kill`, `tgkill`, and the C library's for
  `pthread_cancel`, `setrlimit` and the `set*id` calls), which a thread
  takes as its next system call returns. They end its wait if it waits for
  signals (`sigsuspend`, `sigwait`, `pause`, a `poll` of nothing) or on a
  lock, a condition or a thread (`EINTR`, after the handler); other
  blocking calls, such as a read from a pipe, carry on, and the signal
  waits for them. Other processes end a program instead of signalling it:
  `kill` ends a child with a signal that ends a process by default and
  leaves it alone with another. `alarm` and interval timers are not
  available.
* **No file locks between processes.** `fcntl`'s `F_SETLK` and `F_SETLKW`
  and `flock` fail with `ENOLCK` (unlocking succeeds), rather than let two
  programs both believe they hold a lock; software that relies on them
  (SQLite's default locking) needs porting until the VFS arbitrates locks.
* **No links.** Veda's file system has neither hard nor symbolic links;
  `link` and `symlink` fail, `readlink` finds none.
* **No permissions.** Files have no owners or permission bits: a program
  file (ELF or `#!`) is executable, everything else is not; `chmod` and
  `chown` change nothing. Programs run as user 1000.
* **Static linking only**: no shared libraries, no `dlopen`.
* **Network sockets are TCP and UDP**, over IPv4 and IPv6: no raw or
  ICMP sockets (`ping` is the network service's), no multicast, no
  out-of-band data and no ancillary data (`sendmsg`'s control messages,
  `IP_PKTINFO`). A stream socket binds when it listens: `bind` records
  the address (`getsockname` says it, port 0 still 0) and `listen`
  reports what `bind` would (`EADDRINUSE`, ...); one bound before it
  connects connects from the address and port the service picks. A UDP
  socket's sends wait for room in its channel to the service, not in a
  send buffer. Every socket is passed to the programs a program starts
  and can be sent over Unix domain sockets, but for one still connecting
  (a child does not get it, and `sendmsg` refuses it with `EOPNOTSUPP`);
  its options, and its reading being shut down, go as they are when it
  goes (changed afterwards, they change in one process only), a datagram
  looked at with `MSG_PEEK` stays with the process that looked, a
  connection's failure is reported to the process that hears of it first
  (the others see its stream end), and `SO_LINGER` with no time resets a
  connection at close only if it never went to another process. An
  edge-triggered `epoll` reports a listening or UDP socket when a
  connection or a datagram comes to wait where none did, not each one
  that comes while others wait (accepting or receiving until `EAGAIN`, as
  `epoll` asks, misses none). There is no list of the interfaces
  (`getifaddrs`, which asks Linux's netlink).
* **Unix domain sockets** are named by paths and, as on Linux, by
  abstract names, which are apart for each type of socket and free again
  once their socket is closed (a NUL and five hex digits when the system
  picks one: binding the family alone, or connecting or sending datagrams
  from a socket not bound with `SO_PASSCRED`). A datagram is at most 56
  KiB (`EMSGSIZE`), and a datagram socket connected to another receives
  from others too. Connections waiting to be accepted, and
  datagrams not yet received, wait in a queue of a few hundred, whatever
  backlog `listen` asks for. `SO_PEERCRED` and `SCM_CREDENTIALS` say the
  process the other end says it is (every program is user 1000).
  Every socket is passed to the programs a program starts and can be
  sent over sockets, and is closed once every process that has it closed
  it; but its options, and its being shut down, go as they are when it
  goes (changed afterwards, they change in one process only), a message
  looked at with `MSG_PEEK` stays with the process that looked, and a
  connected socket's copy does not keep the name its socket is bound to.
  A socket node outlives its socket, as on Linux, until it is removed,
  and is not kept on the home disk.
* **`epoll` takes pipes, sockets, eventfds and child processes'
  descriptors.** Files and the like, which are always ready, cannot be
  added (`EPERM`, as Linux says of files), nor can an epoll descriptor be
  added to another, or itself be polled. `epoll_pwait` leaves the signal
  mask as it is, as `ppoll` and `pselect` do. Epoll instances, eventfds
  and child processes' descriptors are not passed to the programs a
  program starts.
* **No `/etc/resolv.conf`.** Programs that read it themselves to find the
  DNS servers find none; `getaddrinfo` and the C library's resolver ask
  the network service.
* **C only inside Veda, for now**: the image has no C++ compiler. The cross
  toolchain compiles C++ for Veda, with libstdc++.
* **The Terminal shows text with colours**, with carriage returns and line
  erasing, but no cursor positioning: full-screen programs (editors) do
  not display correctly yet.

## The toolchain

`cargo orchestration toolchain` builds it from the sources in
`implementation/ports/`:

1. downloads the source archives and checks them against the SHA-256 each
   `implementation/ports/*/port.toml` pins (their GPG signatures were
   checked against the GNU and musl keys when they were pinned);
2. builds the POSIX layer for the architecture's bare target
   (`x86_64-unknown-none` on AMD64);
3. runs `implementation/ports/build.sh` to unpack and patch the
   sources (each port's patch, then the architecture's) and build, with
   the architecture's settings
   (`implementation/ports/arch/<architecture>/settings.sh`), in
   `target/toolchain`:
   * `cross/`, the cross toolchain for this machine: on AMD64
     `x86_64-veda-gcc`, `x86_64-veda-g++` with the C++ library
     (libstdc++), and the binutils,
     with a sysroot laid out like Veda's `/system`. GCC is written in C++,
     so the C++ compiler is what builds the native GCC; programs in C++
     can use it too;
   * `native/system/`, the toolchain built to run inside Veda: GCC (built
     on this machine, to run on Veda, making programs for Veda), binutils,
     GMP, MPFR and MPC, the C library and its headers;
   * `rust/`, Rust: its compiler for this machine, and its standard
     library for this machine and for Veda, on the C library (see
     [Rust on Veda](RUST.md#the-toolchain)).

The first build takes about half an hour on a recent PC (C a quarter of
an hour, Rust as long again); later ones rebuild what changed (each stage
keeps a fingerprint of what went into it, and where: a toolchain
directory that has moved is built again). When the
POSIX layer changes, `cargo orchestration build` puts the new one into the C
library, and links the native programs again with it (GCC, binutils: they
are C programs too), which recompiles nothing.

`cargo orchestration build` installs the native toolchain in the image (in
`/system`) and the C test programs in `/system/tests/c` when the
toolchain exists, and says so when it does not. The image grows by about
80 MB with it, which Veda keeps in memory; compiling is comfortable in
QEMU's default 1 GiB.

The build needs the distribution's packages (meson and ninja for the
media libraries, which videos are decoded with, and on AMD64 nasm for
their assembly; CMake and Python for Rust); on Debian and Ubuntu:

```text
sudo apt install build-essential m4 bison flex curl libgmp-dev libmpfr-dev libmpc-dev nasm meson ninja-build cmake python3
```

### The ports

| Port | Version | What changes for Veda |
|------|---------|-----------------------|
| `binutils` | 2.47 | Veda in `config.sub`; on AMD64 the `x86_64-veda` target (ELF, `elf_x86_64`, separate code pages) |
| `gcc` | 16.2.0 | Veda's target: `gcc/config/veda.h` (musl, static linking, `/system`), `veda.opt`, libgcc's configuration, libstdc++'s checks of the C library; on AMD64 `x86_64-veda`, `i386/veda.h` (the System V x86-64 ABI) |
| `gmp`, `mpfr`, `mpc` | 6.3.0, 4.2.2, 1.4.1 | `config.sub` knows Veda |
| `musl` | 1.2.6 | the call to set the layer up, `posix_spawn`, the program's path in `dl_iterate_phdr`; on AMD64 system calls through the POSIX layer, `_start`, thread entry points |
| `rust` | 1.99.0 | Veda's target in the compiler; the standard library on the C library, the `libc` crate's description of it, GCC's unwinder; on AMD64 `x86_64-unknown-veda` (see [Rust on Veda](RUST.md)) |
| `dav1d` | 1.5.4 | none: built as released (meson), for the media decoder |
| `ffmpeg` | 9.0.2 | `veda` as a target operating system in `configure`; built with only the demuxers, decoders and parsers the media decoder uses |

Each port is a directory with `port.toml` (version, address, SHA-256,
licence), `veda.patch` and, for what is the architecture's,
`arch/<architecture>/veda.patch`; `implementation/ports/README.md` describes
how to change one.

### Licences

GCC, binutils and the GCC runtime library are under the GNU GPL version 3
(libgcc with the GCC Runtime Library Exception, which leaves programs GCC
builds under their own licences); GMP, MPFR and MPC under the GNU LGPL
version 3; musl under the MIT licence; Rust under the MIT licence or the
Apache licence 2.0 (its compiler's LLVM under the Apache licence 2.0 with
LLVM's exceptions); FFmpeg under the GNU LGPL version 2.1 or later (the
build checks that nothing GPL or non-free is in it) and dav1d under the
BSD 2-clause licence. The source of what Veda's image contains is the
upstream archive each `port.toml` names plus the patch beside it.

## Tests

* `verification/c/posix.c` checks the C library and the POSIX layer: the
  startup, standard I/O, the heap, `mmap` and `madvise`, files,
  directories, pipes, threads and TLS, clocks, signals, `posix_spawn` and
  wait statuses, the network, `epoll` and `eventfd`, and Unix domain
  sockets. The network is
  TCP and UDP on the loopback interface (a connection made without
  waiting, then `poll`; `accept4`, the message calls, the end of a
  stream, a refused connection, a datagram cut short), names
  (`getaddrinfo`, `getnameinfo`), and the host machine's web server and
  UDP echo through QEMU's network, which `cargo orchestration test` starts
  and names to `systest`. `epoll` and `eventfd`: an eventfd's counts and
  its semaphore mode, level- and edge-triggered and one-shot registrations
  of pipes, socket pairs, TCP sockets and eventfds, changing and removing
  them, a wait woken by another thread, time-outs, and files refused.
  Unix domain sockets: a listener bound to a path (`stat`, a listing, a
  name taken, `ENXIO` from `open`), connections (`poll` and `epoll` on the
  listener, names, `SO_PEERCRED`, `MSG_PEEK`, the end of a stream,
  `ECONNREFUSED` and `ENOENT` once the listener and its name are gone),
  datagrams from sockets with names and without, cut short and connected,
  `EPROTOTYPE`, seqpacket and datagram pairs, and descriptors sent over
  them (`SCM_RIGHTS`: room for fewer, one write's at most per read, to a
  child process) and who sent a message (`SCM_CREDENTIALS`); abstract
  names (their length, a NUL in one, taken while open and free once
  closed, apart for each type, from a child, names picked for the family
  alone and for `SO_PASSCRED`, and replies to them). Sockets shared with
  children: a Unix domain listener and a TCP one (non-blocking, which the
  children find so and `poll`, one finding a connection taken by the
  other) that two children accept a connection each from, the listener
  open until the last process closes it; a listener sent to a child over
  a socket; a seqpacket pair's end and a datagram socket bound to a path
  in a child. Connections and UDP sockets shared with children: a TCP
  connection as a child's standard input and output, one that a child and
  the parent both write to, one sent to a child over a socket, and a UDP
  socket that two children receive a datagram each from and answer on.
  `verification/c/cxx.cc` checks the C++ library: exceptions
  (across threads too), run-time types, static constructors, containers,
  streams, threads. `systest` runs them inside Veda with
  `verification/c/hello.c`, each linked both at a fixed address and
  position-independent, and tests the kernel's sockets (the handles that
  travel with their bytes too), memory protection,
  private memory and exit futexes and the VFS's open files. What they
  expect of the architecture (the machine `uname` names, as Linux does) is
  in `verification/c/arch/<architecture>/`.
* `verification/ui/c-programs.vts` runs programs in the Terminal: exit
  statuses, an interactive program (`verification/c/greet.c`), Ctrl+C and
  Ctrl+D, pipes and files.
* `verification/ui/c-compile.vts` writes a C program in the Terminal,
  compiles it with GCC inside Veda and runs it.
* `implementation/libraries/elf` (the ELF loader's checks and the stack
  layout) and `implementation/libraries/posix` (paths, structures, flag
  conversions, file reads and writes against the VFS's replies, locks,
  thread ids and signal state, what spawned programs inherit) are
  unit-tested on the host.

`cargo orchestration test --ui` runs them; the scripts that need the
toolchain are skipped when it has not been built.
