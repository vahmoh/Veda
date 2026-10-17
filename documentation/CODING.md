<!-- SPDX-FileCopyrightText: © 2026 Vahid Mohammadi -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Veda coding conventions

The rules every crate in the repository follows, from its language and
dependencies to its errors, tests and licence headers.

## Language and dependencies

* **Rust 2024 edition, stable toolchain** (tested with 1.99), on the
  development machine and in the toolchain, which builds the same release
  with Veda's target for the programs on the standard library
  ([Rust on Veda](RUST.md)). No nightly features, no `RUSTC_BOOTSTRAP`.
* **Rust only**, performance-critical code included. Third-party software
  that Veda builds (GCC, binutils, musl and Rust for Veda's target, ...)
  is a *port* in `implementation/ports/`: an upstream release pinned by its
  SHA-256, plus a small, explained patch, never copied into the source
  tree. `verification/c` (C and C++) tests that environment, and
  `verification/rust` the standard library on it.
* **Proven libraries first.** Where a mature, widely used Rust
  implementation exists (of a protocol, cryptography, a file format, text
  handling, numerical code, an ABI), Veda uses it; its own code is what is
  particular to Veda, and the glue that fits libraries to it.
  [Libraries](DEPENDENCIES.md) lists those in use, and what Veda keeps of
  its own and why.
* **Dependencies** are declared in `[workspace.dependencies]`, each with a
  reason. A new one of a library or of a program on `vrt` must build
  `no_std` (see [Library crates](#library-crates)); a program on the
  standard library may use crates that need `std`. Avoid large dependency
  trees, and a second generation of a crate family already used. Measure
  what a dependency adds: without whole-program LTO, what a crate makes
  reachable (generic code instantiated for every case, features left on)
  is in every program that calls it.
* **A library that is wrong is fixed, not worked around in silence**: it
  becomes a port (its release with an explained patch, which Cargo builds
  in its place, as smoltcp and zune-jpeg are), and the bug is reported
  upstream.
* **`core` and `alloc`** are available everywhere; `std` to the programs
  on the standard library (applications, and services that gain from it),
  the host tools and the driver VM's Linux programs.
* **The standard library, or `vrt` alone.** Applications, and services
  that gain from the standard library's threads, channels, files and
  errors or from crates that need it, are programs on the standard library
  (`Program::std` in `orchestration/src/components.rs`). What the standard
  library stands on or runs before (`init`, the file system, the network
  stack), what drives hardware (the device manager, drivers, the driver
  VM) and what must answer in time (the window system, the sound mixer)
  stays on `vrt` alone (`Program::native`; see [Rust on Veda](RUST.md)).

## Architectures

Veda runs on AMD64 (x86-64). What depends on the processor architecture
or its platform (a PC's devices and firmware) is kept apart, so that
another architecture can be added without touching code that is not its
own.

* **Its own directory.** A crate keeps such code in
  `src/arch/<architecture>/` (`implementation/kernel/src/arch/amd64`,
  `orchestration/src/arch/amd64` and others). Nothing outside those
  directories names an architecture's instructions,
  registers, system call numbers or calling conventions, or its platform's
  devices (a PC's interrupt controllers, timers, keyboard controller and
  IOMMU); drivers of devices on a bus (PCI's, USB's) are each device's,
  not an architecture's.
* **One module, `arch`.** The rest of the crate reaches that code only
  through `arch`, whose items every architecture provides with the same
  names and meanings. The kernel's (`implementation/kernel/src/arch.rs`)
  and `orchestration`'s (`orchestration/src/arch.rs`) document their
  interface in a table. Its vocabulary is architecture-neutral: the
  clock's counter, not the TSC; a thread's TLS pointer, not its FS base.
  Code in `src/arch/<architecture>/` may use the rest of its crate.
* **Chosen by what the code does:**
  * Code that runs on the architecture (the loader, the kernel, the
    runtime, services, tests) takes the module of its compile target:
    `#[cfg(target_arch = "x86_64")] #[path = "arch/amd64/mod.rs"] mod arch;`.
  * Knowledge of an architecture's formats that tools need on any machine
    (executables' machine types, the IOMMU's tables, the virtual devices
    of the hypervisor's guests) is a public module named after it
    (`velf::amd64`, `viommu::amd64`, `vhv::amd64`), compiled on every host
    for host tests and build tools too; `NATIVE` names the one of the
    architecture compiled for, where a crate needs it.
  * Build scripts and tools (`build.rs`, `vbuild`, `orchestration`) run on
    the build machine, but their architecture is the one Veda is built for
    (a build script's `TARGET`, `orchestration`'s `arch`), never the one
    they run on.
* **Outside `src/`,** what tools read from places of their own is in each
  place's `arch/<architecture>/`, where the tool allows it:

| What | Where, for AMD64 |
|------|------------------|
| Cargo's settings of the targets | `.cargo/arch/amd64/config.toml`, which `.cargo/config.toml` includes |
| The ports' settings | `implementation/ports/arch/amd64/settings.sh` |
| A port's changes for the architecture (a patch applied after the port's own; the driver VM's kernel options) | `implementation/ports/<port>/arch/amd64/` |
| What the C tests expect of the architecture | `verification/c/arch/amd64/` |
| The GUI scripts that hold only on its machine | `verification/ui/arch/amd64/` |
| Its Rust targets (rustup reads them nowhere else) | `rust-toolchain.toml` |

## Targets

On AMD64 (`orchestration/src/arch/amd64` names them for the build):

| Component | Target | Notes |
|-----------|--------|-------|
| `implementation/boot/` (the UEFI loader) | `x86_64-unknown-uefi` | Freestanding, soft-float, PE/COFF (the format UEFI's firmware starts) |
| `implementation/kernel/` | `x86_64-unknown-none` | Freestanding, soft-float, an ELF64 executable linked at its address |
| User space on `vrt` alone (`implementation/libraries/`, the system services in `implementation/services/`, `implementation/drivers/`) | `x86_64-unknown-linux-musl`, `#![no_std]` | Hard-float SSE2, position-independent ELF64 executables with nothing of Linux's or musl's, linked by `rust-lld` |
| User space on the standard library (`implementation/applications/`, the services that use it) | `x86_64-unknown-veda` (the `rust` port's) | Static position-independent ELF executables on the C library, linked by `x86_64-veda-gcc`; Veda's libraries in them as they are, `no_std` |
| `implementation/libraries/posix` (in the C library), `implementation/services/mediadec`'s Rust half | `x86_64-unknown-none` | The System V ABI of C programs; linked into musl's `libc.a` |
| C programs | `x86_64-veda` (GCC) | Static ELF executables on musl |
| The standard library's tests (`verification/rust`) | `x86_64-unknown-veda` | As above, each also at a fixed address |
| The driver VM's programs (`implementation/guests/linux/`) | `x86_64-unknown-linux-musl` | Static, on Linux; built from `implementation/guests/linux/` (its `.cargo/config.toml`) into `target/guest` |
| `orchestration/`, host tools | The host's (`std`) | |

## Library crates

* **`no_std`.** Libraries are `#![no_std]` (with `extern crate alloc;` when
  they allocate), to run inside Veda, in programs on `vrt` and on the
  standard library alike, **and** be unit-tested on the host. A part only
  programs on the standard library have is under
  `#[cfg(target_os = "veda")]`, with `extern crate std;`
  (`apps_common::store`). Tests use `#[cfg(test)] extern crate std;`,
  never `cfg_attr(not(test), no_std)`.
* **Floating point.** `sqrt`, `sin`, `floor`, `powf` and the like are
  **not** in `core` on stable: use `libm` (`libm::sinf(x)`) or num-traits'
  `Float`, whose methods are libm's (`use num_traits::Float;` gives
  `x.sqrt()`, `x.sin()`, ... on `f32` and `f64`). Where `std` is linked
  (host tests) its methods win over `Float`'s, so code that must give the
  same results on the host and in Veda calls `libm` (or `Float::sin(x)`).
* **Soft-float.** Check that a library builds for the kernel's soft-float
  target, which has no `std`:
  `cargo build -p <crate> --target x86_64-unknown-none`
  (`.cargo/arch/amd64/config.toml` gives the RustCrypto and dalek crates
  their portable code there, as LLVM cannot build their SIMD code for it).
  Building Veda (`cargo orchestration build`), whose programs on `vrt`
  link without `std`, is the final check. `vimage`, for user space only,
  does not build soft-float (`fast_image_resize`'s SIMD code), nor does
  `shell-common`, which decodes wallpapers with it.

## Pixels and colours

* Pixels are `u32` in `0xAARRGGBB` order: bytes `B, G, R, A` in memory,
  as in the BGRX framebuffer.
* Decoded images (`vimage`) use straight (non-premultiplied) alpha.
* Render targets in `vgfx` use **premultiplied** alpha.

## Performance

QEMU runs Veda on KVM where the host has it, and otherwise on its TCG
emulator, roughly 5 to 20 times slower than native code. Hot loops (pixel
processing, rasterisation, codecs) avoid per-pixel divisions, allocation
and bounds-check-heavy indexing, prefer integer or fixed-point arithmetic
where it is natural, and process data in cache-friendly order. TCG
emulates SSE integer multiplies and shuffles with slow helper calls, so a
loop LLVM auto-vectorises can run several times slower than its scalar
form: check the generated code of hot loops, and keep them scalar where
needed. Always build the OS with optimisations (`--release`,
`cargo orchestration`'s default).

## Robustness and errors

* Never panic on malformed input (files, IPC messages, user data): return
  a crate-specific error enum that implements `core::fmt::Display`.
* `unsafe` blocks carry a `// SAFETY:` comment saying why they are sound.
* Public items have doc comments; every module starts with a `//!`
  overview, after its [licence header](#licence-headers).

## Formatting and testing

* `cargo fmt` (`rustfmt.toml`: `max_width = 120`).
* `cargo orchestration clippy`: Clippy over every crate as Veda builds it,
  warnings as errors (the programs on the standard library with the
  toolchain's Clippy, which knows Veda's target).
* Unit tests live next to the code (`#[cfg(test)] mod tests`).
  `cargo test -p <crate>` runs them; `cargo orchestration test` runs the
  full suite, with the integration tests in QEMU.

## Licence headers

Every file of Veda's own starts with its copyright and its licence, as
SPDX tags in the file's comment syntax:

```rust
// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The module's overview.
```

* **The licence** is Veda's, the GPL version 3 (`LICENSE`), but for the
  code that Mesa's build compiles with Mesa: the driver VM's renderer
  (`implementation/guests/linux/renderer/decoder`,
  `implementation/guests/linux/renderer/drm`) is under the MIT licence, as
  Mesa is.
* **The year** is the one the file was first published in.
* **Only what has to be first** comes before the header: a script's `#!`
  line, an HTML page's `<!doctype html>`, an XML declaration. In C, C++ and
  JavaScript the header opens the file's first comment; Markdown, HTML and
  SVG have it in HTML comments.
* **Files whose format has no comments** go without (images, fonts,
  videos, test vectors, JSON, `documentation/website/CNAME`), as do
  `Cargo.lock`, which Cargo writes, licence texts, and the ports' patches,
  which change other projects' files.
* **A test** of `orchestration`'s (`orchestration/src/licence.rs`), run by
  `cargo orchestration test`, checks every file of the repository, and
  every new one Git does not ignore.
