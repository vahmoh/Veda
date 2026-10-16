<!-- SPDX-FileCopyrightText: © 2026 Vahid Mohammadi -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Rust on Veda

Veda's programs are written in Rust, in two kinds:

* **On the standard library**: the applications, and the services that
  gain from it. The toolchain builds Rust from its release, as it builds
  GCC, with Veda as one of the compiler's targets (`x86_64-unknown-veda`
  on AMD64), and the standard library stands on Veda's C library, as
  C++'s does (see [C on Veda](C.md)). Such a program has the standard
  library's threads, channels, files, processes, errors and collections,
  and the crates that need it, beside Veda's own libraries.
* **On `vrt` alone** (`no_std`): what the standard library stands on, or
  runs before it, or what drives hardware: `init`, the file system, the
  device manager and the drivers, the driver VM, the network stack, the
  window system and the sound mixer, and the in-system tests of the
  kernel's objects. They are built with the development machine's Rust,
  on Veda's own runtime (`vrt`; see
  [Architecture](ARCHITECTURE.md#user-space)).

`orchestration/src/components.rs` says which kind each program is
(`Program::std` or `Program::native`). Veda's libraries (`vui`, `vproto`,
`vfiles`, ...) are `no_std`, and serve both.

This document covers writing and building programs on the standard
library, how it works on Veda, what it does not do yet, and how Rust is
built and tested.

## Veda's programs on the standard library

An application is a crate of the workspace like any other, whose `main`
is the standard library's:

```rust
fn main() {
    let mut spec = vui::WindowSpec::new("My App", 800, 560);
    spec.app_id = "myapp".into();
    std::process::exit(vui::run(spec, MyApp::new()));
}
```

`cargo orchestration build` builds every program `PROGRAMS` lists as
`Program::std` with the toolchain's Rust, for Veda's target, after it has
brought the C library's POSIX layer up to date: the C toolchain's driver
links them with it. They are built in a target directory of their own,
`target/std`, which the build empties when the toolchain's Rust has been
built again (Cargo, which knows a compiler by its version, would keep what
the previous one compiled), and whose programs it links again when the
POSIX layer has changed (Cargo does not see the C library change: the
build removes what it linked, and Cargo builds the programs' own crates
again, not their dependencies). `cargo orchestration clippy` checks them
with the toolchain's Clippy (the development machine's does not know
Veda's target), and the rest as each is built. A program uses:

* **Veda's libraries**, as Veda's other programs do: windows and widgets
  (`vui`), the services' protocols (`vproto`), files (`vfiles`), the
  network (`vnet`, `vtls`, `vweb`), kernel objects (`vrt::object`).
* **The standard library** for what it does: threads (`std::thread`; a
  thread sets its own priority with `vrt::thread::set_priority`, or
  `vrt::thread::Builder` starts it with one), channels and locks
  (`std::sync`), small files (`std::fs`; `apps_common::store` reads an
  application's own within a limit and replaces them in one step),
  processes (`std::process`), the network (`std::net`: TCP and UDP
  sockets and names, which crates that need the network use; `vnet` has
  what is Veda's own, the network's state and Wi-Fi, and sockets whose
  handle a `vui` event loop can wait on), errors, paths, times and
  collections.
  Pictures, music and other large files go through `vfiles`, which moves
  a file in one piece of shared memory where the standard library's reads
  and writes take a message every few kilobytes.
* **Crates that need the standard library**, declared in the workspace
  as any other ([Libraries](DEPENDENCIES.md) has the rules). Most build
  as released. What they stand on to reach the operating system (`libc`,
  `getrandom`, `rustix`, `errno`) does not know Veda's target as
  released: Veda's ports of those crates take Linux's definitions on musl
  for it, and Cargo builds them in place of their releases
  (`implementation/ports/README.md`), so that crates such as `rand`,
  `tempfile`, `chrono`, `regex` or `rayon` build and run. A crate that
  names operating systems itself may need a port of its own too, as
  `tokio` has, with `mio` and `socket2` under it: asynchronous code runs
  on tokio's runtimes (of one thread or several), with its tasks, timers,
  channels, and TCP, UDP and Unix domain sockets, which wait with the C
  library's `epoll` and wake each other with its `eventfd`.

A program started by `init` (a service, or an application from the start
menu) writes its standard output and error to the system log, a line at
a time, beside every program's `vrt::println!`; one started in the
Terminal writes them there. A panic writes its message there too, then
ends the program (`panic = "abort"`), and `init` reports it as crashed,
as the shell tells the user.

## Building a program

`cargo orchestration toolchain` builds Rust in `target/toolchain/rust`, laid
out as Rust's releases install, beside the C toolchain. Its compiler runs on
the development machine and compiles for it and for Veda; the C
toolchain's driver links Veda's programs, as it links C programs:

```bash
target/toolchain/rust/bin/rustc --edition 2024 --target x86_64-unknown-veda -C linker=target/toolchain/cross/bin/x86_64-veda-gcc -O -o hello hello.rs
```

Cargo builds a crate for Veda with that compiler (the development
machine's Cargo, which rustup installs, works with it), and finds the
target's linker, `x86_64-veda-gcc`, on `PATH`:

```bash
PATH=$PWD/target/toolchain/cross/bin:$PATH RUSTC=$PWD/target/toolchain/rust/bin/rustc cargo build --release --target x86_64-unknown-veda
```

Build scripts and procedural macros run on the development machine, with
the standard library the toolchain builds for it too. A crate that
compiles C for its target finds Veda's compiler where the `cc` crate
looks for one: `CC_x86_64_unknown_veda=x86_64-veda-gcc` (and `CXX_`,
`AR_`), which `cargo orchestration` sets for Veda's programs.

A program is a static ELF executable, as C programs are:
position-independent by default (`-static-pie`), or at a fixed address
with `-C relocation-model=static`. It runs on Veda as they do, in the
Terminal or started by another program.

## How it works

**The target**, `x86_64-unknown-veda`, is Veda's own in the compiler:
`target_os = "veda"`, in the `unix` family, with `target_env = "musl"`
(Veda's C library is musl), with thread-local storage and unwinding. Its
programs link statically, with the C toolchain's start files, the C
library (musl and the POSIX layer) and GCC's runtime, whose unwinder
unwinds Rust's panics as it unwinds C++'s exceptions: a program in Rust
and C++ has one unwinder.

**The standard library** calls the C library, whose interface is musl's,
and so Linux's: the `libc` crate, its bindings of the C library, gives
Veda Linux's definitions. Where the standard library makes a Linux
system call itself (futexes), it goes through the C library's `syscall`,
which the POSIX layer carries out as it does the C library's own. What it
uses on Veda:

| What a program uses | What it is on Veda |
|---------------------|--------------------|
| threads | the C library's (`pthread_create`): kernel threads, with 2 MiB of stack unless the program asks for another size; their names are the standard library's own (Veda's threads have none) |
| `Mutex`, `Condvar`, `RwLock`, `Once`, parking | futexes (`FUTEX_WAIT_BITSET`, with absolute deadlines on the monotonic clock) |
| thread-locals | native TLS; destructors run as their thread ends |
| `sleep`, `Instant`, `SystemTime` | `clock_nanosleep` and `clock_gettime`: the kernel's clocks |
| `HashMap`'s keys | `getrandom`: the kernel's generator |
| files and directories | the VFS through the C library: `openat`, `fstatat` and `unlinkat` relative to a directory (so `remove_dir_all` cannot be led elsewhere by a directory replaced meanwhile), `preadv` and `pwritev` |
| `Command` | `posix_spawn`, with file actions for the standard streams and the working directory |
| `io::pipe`, `UnixStream::pair`, `UnixDatagram::pair` | pipes and socket pairs, made close-on-exec |
| `UnixListener`, `UnixStream`, `UnixDatagram` | the C library's Unix domain sockets: bound to paths, which the VFS keeps as socket nodes, with their time-outs, non-blocking use and names (`local_addr`, `peer_addr`, a datagram's sender); tokio's too, with `peer_cred`. They go to the processes a program starts (as a child's standard input, say), which share them: a listener's connections go to whichever accepts first |
| tokio's runtime (`mio`) | `epoll` and `eventfd`: the C library's, of the POSIX layer; a child process's end, `pidfd_open` |
| `TcpStream`, `TcpListener`, `UdpSocket` | the C library's sockets: sockets of the network service, over IPv4 and IPv6, with their time-outs, `peek`, `try_clone`, non-blocking use and options (`set_nodelay`, `set_ttl`, `set_linger`, `take_error`...). They go to the processes a program starts (as a child's standard input and output, say), which share them: a listener's connections go to whichever accepts first, a stream's bytes to whichever reads them and a socket's datagrams to whichever receives them. Other machines reach a `TcpListener` or a `UdpSocket` only where the firewall lets them in; an application it keeps off the network gets `PermissionDenied` |
| `ToSocketAddrs` | `getaddrinfo`: the network service's resolver |
| `env::current_exe` | the path the loader gives the program (`AT_EXECFN`) |
| panics, backtraces | GCC's unwinder; a backtrace reads the program's symbols and line tables from its own file |

`std::os::unix` works as on other Unix systems, and `std::os::veda::fs`
has `MetadataExt`: the fields of `struct stat`.

**Veda's libraries** build for the target as they are, `no_std`, and
`vrt` fits itself to it (`target_os = "veda"`). The standard library is
the program's runtime there: its entry point, its panic handler and the C
symbols are the program's, not `vrt`'s, and `vrt`'s threads are the
standard library's, each setting its own priority (the kernel lets a
thread set its own: the C library keeps the threads' handles). The C
library's POSIX layer has read the startup message, and keeps the
descriptors, the terminal and the registry for itself: it gives the
handles of an application's own roles (`veda_startup_handle`), and a
registry channel of the program's own (`veda_registry`), which `init`
makes for the same process; through it `vproto` connects to services and
registers the program's, as in Veda's other programs. `vrt`'s heap stays
the global allocator (unless a program leaves out `vrt`'s `runtime`
feature), so that a program allocates as the rest of Veda does; musl's
`malloc` serves the C library's own.

## Limits

Rust's programs share those of C programs (see
[C on Veda](C.md#limits)). In the standard library's terms:

* **No compiler inside Veda.** Rust's compiler is a program with shared
  libraries (its own and LLVM's), and it loads procedural macros as
  shared libraries: Veda links statically and has no `dlopen`.
* **No `fork` or `exec`.** `Command` starts every program with
  `posix_spawn`. What only a child between `fork` and `exec` could do
  fails with `Unsupported` (`CommandExt::pre_exec`, `uid` and `gid`), as
  do `CommandExt::exec` and a `Command` that changes `PATH` for a program
  named without a directory; `process_group` changes nothing, as Veda's
  processes have no groups.
* **Signals are synchronous**, and a fault ends the program (status 139
  in the Terminal): the standard library has no stack-overflow handler,
  and a thread that overflows its stack ends the program without a
  message. `Child::kill` ends a child. tokio's signals never come (it
  waits for them as they come to a process); its child processes work,
  their ends waited for through `pidfd_open`.
* **The network is TCP and UDP**: `TcpStream`, `TcpListener` and
  `UdpSocket` work, but not multicast (`join_multicast_v4` and the like
  fail). Unix domain sockets are named by paths: Linux's abstract names
  (`SocketAddr::from_abstract_name`) are not in the standard library for
  Veda, which sees a socket bound to one as unnamed (the C library has
  them, for `libc`'s calls).
* **No links, permissions or file locks**: `fs::hard_link` and `symlink`
  fail, permissions are not kept, and `File::lock` and the like fail
  with `Unsupported`.
* **Thread names** are the standard library's alone (panics and
  `thread::current` show them; the system does not).

## The toolchain

`cargo orchestration toolchain` builds Rust after the C toolchain, from the
`rust` port (`implementation/ports/rust`): the release's source
(`rustc-1.99.0-src.tar.xz`, pinned by its SHA-256; its signature was
checked against the Rust project's release key when it was pinned), with
Veda's patches: Veda's target in the compiler, the standard library on
Veda, the `libc` crate's description of Veda's C library, and the
unwinder (`implementation/ports/README.md` describes them). Rust's own build
program (`x.py`) then works in `target/toolchain/build/rust`:

1. with the release before this one, its stage 0, which builds it:
   `cargo orchestration toolchain` downloads that release's compiler,
   standard library and Cargo, and checks them against the SHA-256 that the
   release's `src/stage0` gives them;
2. it builds the release's LLVM, with the architecture's code generator
   alone (X86 on AMD64);
3. then the compiler, and with it the compiler again, as Rust's releases
   are built, and the standard library for the development machine and
   for Veda;
4. and installs them in `target/toolchain/rust`, with Clippy, which
   `cargo orchestration clippy` runs for Veda's target, and the standard
   library's source (`rust-src`), where editors find its definitions. The
   toolchain's build then links a program for Veda with them, checks it
   with Clippy, and removes what built them (some 9 GB), keeping the
   logs.

That takes about a quarter of an hour on a recent PC, LLVM half of it,
after the C toolchain; the release's source, patched, stays in
`target/toolchain/src/rust` (4.5 GB).

Rust's build runs Cargo in the release's source tree, inside Veda's
checkout, where Cargo would apply the checkout's configuration (Veda's
`.cargo/config.toml`) to it: the build's Cargo runs from outside the
checkout instead, with the release's own configuration (its vendored
crates).

Besides the C toolchain's packages, it needs CMake (for LLVM) and Python 3
(for `x.py`); on Debian and Ubuntu:

```text
sudo apt install cmake python3
```

## Tests

* `verification/rust/stdlib.rs` checks the standard library on Veda: the
  startup (`current_exe`, the environment), the standard streams, formatting
  and floating point, the heap, collections, files, directories
  (`remove_dir_all`), file locks (that they fail), threads (scoped, named,
  with a stack size), locks, condition variables, barriers, `OnceLock`,
  channels, parking, thread-locals and their destructors, panics
  (`catch_unwind`, a thread's panic, hooks), backtraces (with symbols and
  lines), time, processes (output, exit statuses and signals, the working
  directory, the environment, pipes to a child, `kill`, missing programs,
  `HashMap`'s random keys differing between processes, and `pre_exec` and
  `exec` failing), pipes (and `BrokenPipe`), Unix domain sockets (a pair; a
  listener bound to a path in `/tmp`, which `metadata` says is a socket,
  connections from a child process and from a thread, names, read time-outs,
  `ConnectionRefused` and `NotFound` once the listener and its name are
  gone; datagrams from sockets with names and without, cut short, connected,
  and in pairs), listeners shared with child processes (a Unix domain one
  and a TCP one, each two children's standard input, which accept a
  connection each; the Unix domain one after the parent closed its own),
  connections and UDP sockets shared with them (a TCP connection as a
  child's standard input and output, one that a child and the parent both
  write to, and a UDP socket that two children receive a datagram each from,
  with the read time-out it was given), the network, and a deep stack. The
  network's checks: TCP on the loopback interface (a connection and the end
  of its stream, 4 MiB each way at once through a cloned stream, the last
  bytes of a connection closed right after its writes, peeking, read
  time-outs, non-blocking use, a reader woken by what comes while another
  thread writes, a refused connection, IPv6), UDP (to an address and to a
  peer, peeking, a datagram cut short, a time-out, a connected socket
  hearing its peer alone), names (`localhost`, literals, connecting by
  name), and the host machine's web server and UDP echo through QEMU's
  network, which `cargo orchestration test` starts and names to `systest`.
  `cargo orchestration build` compiles it with the toolchain's Rust twice,
  position-independent (`stdlib`) and at a fixed address (`stdlib-fixed`),
  into `/system/tests/rust`, and `systest` runs both inside Veda.
* `verification/ui/rust-programs.vts` runs it in the Terminal, with the
  terminal as its standard streams.
* `verification/stdcrates` checks crates that need the standard library,
  as Veda's ports have them: libc's calls of the C library, both releases
  of getrandom, rand's generator, rustix's files and processes,
  tempfile's files and directories, and tokio: on its runtime of one
  thread and on one of several, sixteen TCP connections over the
  loopback interface echo 256 KiB each at once, each written from one
  task and read from another, then a timer, a time-out, datagrams; the
  same over a Unix domain socket bound to a path, whose clients see who
  listens (`peer_cred`), and a pair of Unix datagram sockets; and a
  child process whose output is read and whose end is waited for. It is a
  program on the standard library in the image (`/system/bin/stdcrates`),
  and `systest` runs it.
* Veda's programs on the standard library are tested as its others are:
  each application by its GUI scripts (`verification/ui`) and its agent's
  (`verification/agent`).

`cargo orchestration test` runs them all.
