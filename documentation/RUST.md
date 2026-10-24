<!-- SPDX-FileCopyrightText: © 2026 Vahid Mohammadi -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Rust on Veda

Veda's programs are written in Rust: the applications, and the services
that gain from it, on the standard library; what the standard library
stands on or runs before, what drives hardware and what must answer in
time, on Veda's own runtime alone (`rt`, `no_std`, built with the
development machine's Rust; see
[Architecture](ARCHITECTURE.md#user-space)). This document covers
programs on the standard library: writing and building them, how they
work on Veda, their limits, and how the toolchain builds Rust.

**Contents:** [Writing a program](#writing-a-program) · [Building a program](#building-a-program) · [How it works](#how-it-works) · [Limits](#limits) · [The toolchain](#the-toolchain) · [Tests](#tests)

## Writing a program

An application is a crate of the workspace whose `main` is the standard
library's ([Developing](DEVELOPING.md#writing-an-application) shows how
to add one):

```rust
fn main() {
    let mut spec = ui::WindowSpec::new("My App", 800, 560);
    spec.app_id = "myapp".into();
    std::process::exit(ui::run(spec, MyApp::new()));
}
```

It uses:

- **Veda's libraries**: windows and widgets (`ui`), the services'
  protocols (`proto`), files (`files`), the network (`net`, `tls`,
  `web`), kernel objects (`rt::object`).
- **The standard library** for what it does: threads, channels and locks,
  processes, sockets and names (`std::net`), and small files
  (`apps_common::store` reads an application's own within a limit and
  replaces them in one step). A thread sets its own priority with
  `rt::thread::set_priority`, or `rt::thread::Builder` starts it with
  one, up to `NORMAL` in an application
  ([The kernel](ARCHITECTURE.md#the-kernel)). `net` adds what is Veda's
  own (the network's state, Wi-Fi, sockets a `ui` event loop can wait
  on), and `files` moves large files, such as pictures and music, in one
  piece of shared memory, where the standard library's reads and writes
  take a message every few kilobytes.
- **Crates that need the standard library**, declared in the workspace
  like any other ([Libraries](DEPENDENCIES.md) has the rules). Most build
  as released. Those they stand on to reach the operating system (`libc`,
  `getrandom`, `rustix`, `errno`) are ports that take Linux's definitions
  on musl for Veda's target, which Cargo builds in place of the releases
  (`implementation/ports/README.md`): crates such as `rand` and `tempfile`
  build and run (`verification/stdcrates` checks them). A crate that names operating
  systems itself may need a port too, as `tokio`, `mio` and `socket2`
  have; tokio's runtimes, of one thread or several, work.

A program started by `init` (a service, or an application from the start
menu) writes its standard output and error to the system log, a line at
a time; one started in the Terminal writes them there. A panic writes its
message there too and ends the program (`panic = "abort"`), which `init`
reports as a crash.

## Building a program

> [!IMPORTANT]
> Building Veda needs the toolchain's Rust, as its applications are
> programs on the standard library: run `cargo automation toolchain`
> once (about half an hour).

`cargo automation build` builds every program that
`automation/source/components.rs` lists as `Program::std` (those on `rt`
alone are `Program::native`) in `target/std`, with the toolchain's Rust
and the C toolchain's driver, after bringing the C library's POSIX layer
up to date. It empties `target/std` when the toolchain's Rust is built
again (Cargo knows a compiler only by its version), and links the
programs again when the POSIX layer changes, which Cargo does not see.
`cargo automation clippy` uses the toolchain's Clippy: the development
machine's does not know Veda's target.

By hand, the toolchain's compiler builds for Veda and the C toolchain's
driver links:

```bash
target/toolchain/rust/bin/rustc --edition 2024 --target x86_64-unknown-veda -C linker=target/toolchain/cross/bin/x86_64-veda-gcc -O -o hello hello.rs
```

Cargo (the development machine's, which rustup installs) builds a crate
with that compiler, and finds the target's linker, `x86_64-veda-gcc`, on
`PATH`:

```bash
PATH=$PWD/target/toolchain/cross/bin:$PATH RUSTC=$PWD/target/toolchain/rust/bin/rustc cargo build --release --target x86_64-unknown-veda
```

Build scripts and procedural macros run on the development machine, with
the standard library the toolchain builds for it too. A crate that
compiles C finds Veda's compiler where the `cc` crate looks:
`CC_x86_64_unknown_veda=x86_64-veda-gcc` (and `CXX_`, `AR_`), which
`cargo automation` sets for Veda's programs. A program that links the web
engine (`Program::web`: the browser) finds it at `VEDA_WEB`, which
`cargo automation` sets too: its build script compiles its C against
WebKit's headers and links what `wpe-webkit-static-2.0.pc` names, through
the web build's own pkg-config ([The web browser](BROWSER.md#building-it)).

A program is a static ELF executable, as C programs are,
position-independent by default (`-static-pie`) or at a fixed address
with `-C relocation-model=static`, and runs as they do.

## How it works

**The target**, `x86_64-unknown-veda` on AMD64, is Veda's own in the
compiler: `target_os = "veda"`, in the `unix` family, with
`target_env = "musl"`, thread-local storage and unwinding. Programs link
statically with the C toolchain's start files, the C library (musl and
the POSIX layer) and GCC's runtime, whose unwinder unwinds Rust's panics
as it unwinds C++'s exceptions: a program in Rust and C++ has one
unwinder.

**The standard library** stands on the C library, as C++'s does
([C on Veda](C.md)). The C library's interface is musl's, and so
Linux's: the `libc` crate gives Veda Linux's definitions. The system
calls the standard library makes itself (futexes) go through the C
library's `syscall`, which the POSIX layer carries out.

| The standard library's | On Veda |
|------------------------|---------|
| Threads, thread-locals | Kernel threads (`pthread_create`), with 2 MiB of stack unless the program asks for another size; native TLS, whose destructors run as their thread ends |
| `Mutex`, `Condvar`, `RwLock`, `Once`, parking | Futexes (`FUTEX_WAIT_BITSET`, with absolute deadlines on the monotonic clock) |
| `sleep`, `Instant`, `SystemTime`, `HashMap`'s keys | The kernel's clocks (`clock_nanosleep`, `clock_gettime`) and generator (`getrandom`) |
| Files and directories | The VFS: `openat`, `fstatat` and `unlinkat` relative to a directory (so `remove_dir_all` cannot be led elsewhere by a directory replaced meanwhile), `preadv` and `pwritev` |
| `File::lock` and the like | `flock`: advisory locks on the whole file, held by the open file description, which the VFS arbitrates between processes |
| `Command` | `posix_spawn`, with file actions for the standard streams and the working directory |
| `io::pipe`, `UnixStream::pair`, `UnixDatagram::pair` | Pipes and socket pairs, made close-on-exec |
| `UnixListener`, `UnixStream`, `UnixDatagram` | Unix domain sockets bound to paths, with time-outs, non-blocking use and names (`local_addr`, `peer_addr`, a datagram's sender); tokio's too, with `peer_cred` |
| `TcpStream`, `TcpListener`, `UdpSocket`, `ToSocketAddrs` | The network service's sockets, over IPv4 and IPv6, with time-outs, `peek`, `try_clone`, non-blocking use and options (`set_nodelay`, `set_ttl`, `set_linger`, `take_error`...), and its resolver. An application the firewall keeps off the network gets `PermissionDenied` |
| tokio's runtime (`mio`) | `epoll` and `eventfd`; a child's end through `pidfd_open` |
| `env::current_exe` | The path the loader gives the program (`AT_EXECFN`) |
| Panics, backtraces | GCC's unwinder; a backtrace reads the program's symbols and line tables from its own file |

Sockets go to the processes a program starts, which share them as in C.
`std::os::unix` works as on other Unix systems, and `std::os::veda::fs`
has `MetadataExt`: the fields of `struct stat`.

**Veda's libraries** (`ui`, `proto`, `files`, ...) are `no_std` and
build for the target as they are, `rt` fitting itself to it. The
standard library is the program's runtime (its entry point, panic handler
and C symbols), and `rt`'s threads are its threads, each setting its own
priority. The POSIX layer keeps the startup message's descriptors,
terminal and registry, and gives an application the handles of its own
roles (`veda_startup_handle`) and a registry channel of its own
(`veda_registry`), through which `proto` reaches services and registers
the program's. `rt`'s heap stays the global allocator (unless a program
leaves out `rt`'s `runtime` feature); musl's `malloc` serves the C
library.

## Limits

Rust programs share the limits of C programs
([C on Veda](C.md#limits)). In the standard library's terms:

- **No compiler inside Veda.** Rust's compiler needs shared libraries
  (its own and LLVM's) and loads procedural macros as shared libraries;
  Veda links statically and has no `dlopen`.
- **No `fork` or `exec`.** `Command` starts every program with
  `posix_spawn`. What only a child between `fork` and `exec` could do
  fails with `Unsupported` (`CommandExt::pre_exec`, `uid`, `gid`), as do
  `CommandExt::exec` and a `Command` that changes `PATH` for a program
  named without a directory. `process_group` changes nothing, as Veda's
  processes have no groups.
- **Signals are synchronous**, and a fault ends the program (status 139
  in the Terminal): the standard library has no stack-overflow handler,
  and a thread that overflows its stack ends the program without a
  message. `Child::kill` ends a child. tokio's signals never come (it
  waits for them as they come to a process); its child processes work.
- **The network is TCP and UDP**, without multicast (`join_multicast_v4`
  and the like fail). Unix domain sockets are named by paths: Linux's
  abstract names (`SocketAddr::from_abstract_name`) are not in the
  standard library for Veda, which sees a socket bound to one as unnamed
  (the C library has them, for `libc`'s calls).
- **No links or permissions**: `fs::hard_link` (but between `/dev/shm`'s
  shared memory objects) and `symlink` fail, and permissions are not kept.
- **Thread names** are the standard library's alone: panics and
  `thread::current` show them, the system does not.

## The toolchain

`cargo automation toolchain` builds Rust after the C toolchain
([C on Veda](C.md#the-toolchain)) from the `rust` port: the release's
source, `rustc-1.99.0-src.tar.xz`, pinned by its SHA-256 (its signature
was checked against the Rust project's release key when it was pinned),
and Veda's patches for the compiler's target, the standard library, the
`libc` crate and the unwinder
([`implementation/ports/README.md`](../implementation/ports/README.md)).
Rust's build program, `x.py`, then works in `target/toolchain/build/rust`:

1. The previous release (stage 0) builds it:
   `cargo automation toolchain` downloads its compiler, standard
   library and Cargo, checked against the SHA-256 the release's
   `src/stage0` gives them.
2. LLVM is built with the architecture's code generator alone (X86 on
   AMD64).
3. The compiler is built, then built again by itself, as Rust's releases
   are, with the standard library for the development machine and for
   Veda.
4. They are installed in `target/toolchain/rust`, as Rust's releases
   install, with Clippy and the standard library's source (`rust-src`,
   where editors find its definitions).

The build then links a program for Veda, checks it with Clippy, and
removes what built Rust (some 9 GB), keeping the logs. Rust takes about a
quarter of an hour of the toolchain's half hour on a recent PC, LLVM half
of it; the patched source stays in `target/toolchain/src/rust` (4.5 GB).
Rust's Cargo runs outside Veda's checkout, on the release's vendored
crates, so that the checkout's `.cargo/config.toml` does not reach it.
CMake (for LLVM) and Python 3 (for `x.py`) are among the toolchain's
[packages](C.md#packages).

## Tests

- **`verification/rust/stdlib.rs`** checks the standard library, from
  files, threads, processes and pipes to sockets (Unix domain, TCP and
  UDP, shared with child processes, and the host's servers through QEMU),
  names, file locks between processes, panics, backtraces and a deep
  stack, and that `pre_exec` and `exec` fail. `cargo automation build`
  compiles it twice into `/system/tests/rust`: position-independent
  (`stdlib`) and at a fixed address (`stdlib-fixed`).
- **`verification/stdcrates`** (`/system/bin/stdcrates` in the image)
  checks crates as Veda's ports have them: libc, both releases of
  getrandom, rand, rustix, tempfile, and tokio (both runtimes, with TCP,
  UDP and Unix domain sockets, timers and a child process).
- **`verification/ui/rust-programs.vts`** runs `stdlib` in the Terminal.
  Each application has its GUI scripts (`verification/ui`) and its
  agent's (`verification/agent`).

`systest` runs `stdlib`, `stdlib-fixed` and `stdcrates` inside Veda;
`cargo automation test` runs `systest`, and `--ui` adds the scripts.
