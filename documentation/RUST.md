<!-- SPDX-FileCopyrightText: © 2026 Vahid Mohammadi -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Rust on Veda

Veda's own programs are written in Rust without its standard library, on
Veda's own runtime (`vrt`; see [Architecture](ARCHITECTURE.md#user-space)),
and built with the development machine's Rust. Programs that use the
standard library run on Veda too: the toolchain builds Rust from its
release, as it builds GCC, with Veda as one of the compiler's targets
(`x86_64-unknown-veda` on AMD64), and the standard library stands on
Veda's C library, as C++'s does (see [C on Veda](C.md)).

This document covers building such programs, how the standard library
works on Veda, what it does not do yet, and how Rust is built and tested.

## Building a program

`cargo xtask toolchain` builds Rust in `target/toolchain/rust`, laid out
as Rust's releases install, beside the C toolchain. Its compiler runs on
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
looks for one: `CC_x86_64_unknown_veda=x86_64-veda-gcc`.

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
| `io::pipe`, `UnixStream::pair` | pipes and socket pairs, made close-on-exec |
| `env::current_exe` | the path the loader gives the program (`AT_EXECFN`) |
| panics, backtraces | GCC's unwinder; a backtrace reads the program's symbols and line tables from its own file |

`std::os::unix` works as on other Unix systems, and `std::os::veda::fs`
has `MetadataExt`: the fields of `struct stat`.

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
  message. `Child::kill` ends a child.
* **No network sockets yet**: `TcpStream`, `TcpListener` and `UdpSocket`
  fail. Unix sockets work as pairs (`UnixStream::pair`), without paths.
* **No links, permissions or file locks**: `fs::hard_link` and `symlink`
  fail, permissions are not kept, and `File::lock` and the like fail
  with `Unsupported`.
* **Thread names** are the standard library's alone (panics and
  `thread::current` show them; the system does not).

## The toolchain

`cargo xtask toolchain` builds Rust after the C toolchain, from the
`rust` port (`implementation/ports/rust`): the release's source
(`rustc-1.99.0-src.tar.xz`, pinned by its SHA-256; its signature was
checked against the Rust project's release key when it was pinned), with
Veda's patches: Veda's target in the compiler, the standard library on
Veda, the `libc` crate's description of Veda's C library, and the
unwinder (`implementation/ports/README.md` describes them). Rust's own build
program (`x.py`) then works in `target/toolchain/build/rust`:

1. with the release before this one, its stage 0, which builds it:
   `cargo xtask toolchain` downloads that release's compiler, standard
   library and Cargo, and checks them against the SHA-256 that the
   release's `src/stage0` gives them;
2. it builds the release's LLVM, with the architecture's code generator
   alone (X86 on AMD64);
3. then the compiler, and with it the compiler again, as Rust's releases
   are built, and the standard library for the development machine and
   for Veda;
4. and installs them in `target/toolchain/rust` (about 450 MB). The
   toolchain's build then links a program for Veda with them, and
   removes what built them (some 9 GB), keeping the logs.

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
  startup (`current_exe`, the environment), the standard streams,
  formatting and floating point, the heap, collections, files,
  directories (`remove_dir_all`), file locks (that they fail), threads
  (scoped, named, with a stack size), locks, condition variables,
  barriers, `OnceLock`, channels, parking, thread-locals and their
  destructors, panics (`catch_unwind`, a thread's panic, hooks),
  backtraces (with symbols and lines), time, processes (output, exit
  statuses and signals, the working directory, the environment, pipes to
  a child, `kill`, missing programs, `HashMap`'s random keys differing
  between processes, and `pre_exec` and `exec` failing), pipes (and
  `BrokenPipe`), Unix socket pairs, and a deep stack. `cargo xtask
  build` compiles it with the toolchain's Rust twice,
  position-independent (`stdlib`) and at a fixed address
  (`stdlib-fixed`), into `/system/tests/rust`, and `systest` runs both
  inside Veda.
* `verification/ui/rust-programs.vts` runs it in the Terminal, with the
  terminal as its standard streams.

`cargo xtask test` runs them once Rust is built; the GUI script is
skipped without it.
