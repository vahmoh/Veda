<!-- SPDX-FileCopyrightText: © 2026 Vahid Mohammadi -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Veda coding conventions

These rules apply to every crate in the repository.

## Language and dependencies

* Rust 2024 edition, **stable** toolchain (tested with 1.99). No nightly
  features, no `RUSTC_BOOTSTRAP`.
* **Proven libraries first.** Where a mature, widely used Rust
  implementation exists (of a protocol, of cryptography, of a file format,
  of text handling, of numerical code, of an ABI), Veda uses it rather than
  writing its own; Veda's own code is what is particular to Veda and the
  glue that fits libraries to it. [Libraries](DEPENDENCIES.md) lists the
  libraries in use, and what Veda keeps of its own and why. A new
  dependency must build `no_std` (see Library crates), be declared in the
  workspace's `[workspace.dependencies]`, and come with a reason; avoid
  crates with large dependency trees, and a second generation of a crate
  family already used. Measure what it adds to a program: programs are
  linked without whole-program LTO, so what a crate makes reachable
  (generic code instantiated for every case, features left on) is in
  every program that calls it. A library that is wrong is fixed, not
  worked around in silence: it becomes a port (`ports/`, as smoltcp and
  zune-jpeg are: its release with an explained patch, which Cargo builds
  in its place), and the bug is reported upstream. `core`, `alloc` and
  (for host tools and the driver VM's Linux programs only) `std` are
  available.
* **Rust only**, performance-critical code included (see Performance below).
  Third-party software that Veda builds (the toolchain: GCC, binutils,
  musl, and Rust for Veda's own target; ...) comes as *ports*
  (`ports/`): an upstream release pinned by its SHA-256 plus a small,
  explained patch, never copied into the source tree. The C and C++
  programs in `verification/c` test that environment, and the Rust
  programs in `verification/rust` the standard library on it.

## Architectures

Veda runs on AMD64 (x86-64). Everything that depends on the processor
architecture, or on the platform that comes with it (a PC's devices and
firmware), is kept apart from the rest, so that another architecture can
be added beside AMD64 without touching code that is not its own:

* A crate with such code keeps it in its own `src/arch/<architecture>/`
  directory: `implementation/boot/src/arch/amd64`,
  `implementation/kernel/src/arch/amd64`,
  `implementation/libraries/abi/src/arch/amd64`,
  `implementation/libraries/rt/src/arch/amd64`,
  `implementation/services/drivervm/src/arch/amd64`, `xtask/src/arch/amd64`
  and the others. Nothing outside those directories names an architecture's
  instructions, registers, system call numbers or calling conventions, or
  the devices of its platform (a PC's interrupt controllers, timers,
  keyboard controller and IOMMU); drivers of devices that come on a bus
  (PCI's, USB's) are each device's, not an architecture's.
* The rest of the crate reaches it through one module, `arch`, whose
  items every architecture provides with the same names and meanings, and
  nothing else of it. The kernel's (`implementation/kernel/src/arch.rs`) and
  xtask's (`xtask/src/arch.rs`) document their interface in a table;
  vocabulary is the architecture-neutral one (the clock's counter, not the
  TSC; a thread's TLS pointer, not its FS base). Code in
  `src/arch/<architecture>/` may use the rest of its crate.
* How the module is chosen depends on what the code does:
  * **Code that runs on the architecture** (the loader, the kernel, the
    runtime, services, tests) takes the module of the target it is
    compiled for:
    `#[cfg(target_arch = "x86_64")] #[path = "arch/amd64/mod.rs"] mod arch;`.
  * **Knowledge of an architecture's formats** that tools need on any
    machine (executables' machine types, the IOMMU's tables, the virtual
    devices of the hypervisor's guests) is a public module named after the
    architecture (`vpe::amd64`, `velf::amd64`, `viommu::amd64`,
    `vhv::amd64`), compiled on every host so that host tests and build
    tools use it too; `NATIVE` names the one of the architecture compiled
    for, where a crate needs it.
  * **Build scripts and tools** (`build.rs`, `vbuild`, xtask) run on the
    build machine: the architecture is the one Veda is built for (the
    `TARGET` Cargo gives a build script, xtask's `arch`), never the one
    they run on.
* What tools read from places of their own, which have no `src/`, is in
  an `arch/<architecture>/` directory of each place, where the tool
  allows it: Cargo's settings of the targets are in
  `.cargo/arch/amd64/config.toml` (which `.cargo/config.toml` includes);
  the ports' in `ports/arch/amd64/settings.sh`, and each port's changes
  for the architecture in `ports/<port>/arch/amd64/` (a patch applied
  after the port's own; the driver VM's kernel options); what the C tests
  expect of it in `verification/c/arch/amd64/`; and the GUI scripts that
  hold only on its machine in `verification/ui/arch/amd64/`.
  `rust-toolchain.toml` lists AMD64's Rust targets, as rustup reads them
  nowhere else.

## Targets

On AMD64 (`xtask/src/arch/amd64` names them for the build):

| Component | Target | Notes |
|-----------|--------|-------|
| `implementation/boot/` (UEFI loader), `implementation/kernel/` | `x86_64-unknown-uefi` | freestanding, soft-float, PE |
| user space (`implementation/libraries/`, `implementation/services/`, `implementation/drivers/`, `implementation/applications/`) | `x86_64-pc-windows-msvc`, `#![no_std]` | hard-float SSE2, PE `.exe`, linked by `rust-lld` |
| `implementation/libraries/posix` (in the C library), `implementation/services/mediadec`'s Rust half | `x86_64-unknown-none` | the System V ABI of C programs; linked into musl's `libc.a` |
| C programs | `x86_64-veda` (GCC) | static ELF executables on musl |
| programs on Rust's standard library (`verification/rust`) | `x86_64-unknown-veda` (the `rust` port's) | static ELF executables on the C library, linked by `x86_64-veda-gcc` |
| the driver VM's programs (`implementation/guest/`) | `x86_64-unknown-linux-musl` | static, on Linux |
| `xtask/`, host tools | host (`std`) | |

## Library crates

* Libraries are `#![no_std]` (plus `extern crate alloc;` when they allocate)
  so they can run inside Veda **and** be unit-tested on the host.
  Tests use `#[cfg(test)] extern crate std;`, never `cfg_attr(not(test), no_std)`.
* Floating-point functions such as `sqrt`, `sin`, `floor` or `powf` are **not**
  available in `core` on stable. Use `libm` (`libm::sinf(x)`), or num-traits'
  `Float`, whose methods are libm's: `use num_traits::Float;` gives
  `x.sqrt()`, `x.sin()`, ... on `f32`/`f64`. Where `std` is linked (host
  tests) its own methods win over `Float`'s, so code whose results must be
  the same on the host and in Veda calls `libm` (or `Float::sin(x)`).
* Verify that a library builds for the loader's and kernel's soft-float
  target with `cargo build -p <crate> --target x86_64-unknown-uefi`
  (`.cargo/arch/amd64/config.toml` makes the RustCrypto and dalek crates
  use their portable code there, as LLVM cannot build their SIMD code for
  it). That target has a `std`, though, so this does not show a dependency
  on it; `--target x86_64-unknown-none` has none, and building Veda
  (`cargo xtask build`), whose programs link without `std`, is the final
  check. `vimage` runs only in user space and does not build soft-float
  (`fast_image_resize`'s SIMD code).

## Pixels and colors

* Pixels are `u32` in `0xAARRGGBB` order, i.e. bytes `B, G, R, A` in memory,
  which matches the BGRX framebuffer.
* Decoded images (`vimage`) use straight (non-premultiplied) alpha.
* Render targets in `vgfx` use **premultiplied** alpha.

## Performance

Veda usually runs under QEMU's TCG emulator, which is roughly 5-20x
slower than native code. Hot loops (pixel processing, rasterisation, codecs)
should avoid per-pixel divisions, allocation and bounds-check-heavy indexing,
prefer integer/fixed-point arithmetic where it is natural, and process data in
cache-friendly order. TCG emulates SSE integer multiplies and shuffles with
slow helper calls, so a hot loop that LLVM auto-vectorises can become
several times slower than its scalar form: check the generated code of hot
loops and keep them scalar where needed. Always build the OS with
optimisations (`--release` is the default for `cargo xtask`).

## Robustness and errors

* Never panic on malformed input (files, IPC messages, user data): return a
  crate-specific error enum that implements `core::fmt::Display`.
* `unsafe` blocks carry a `// SAFETY:` comment explaining why they are sound.
* Public items have doc comments; every module starts with a `//!` overview,
  after its licence header (below).

## Formatting and testing

* `cargo fmt` (see `rustfmt.toml`, `max_width = 120`).
* Unit tests live next to the code (`#[cfg(test)] mod tests`). Run them with
  `cargo test -p <crate>`; `cargo xtask test` runs the full suite plus the
  QEMU integration tests.

## Licence headers

Every file of Veda's own starts with its copyright and its licence, as SPDX
tags in the file's comment syntax:

```rust
// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The module's overview.
```

* The licence is Veda's, the GPL version 3 (`LICENSE`), but for the code
  that Mesa's build compiles with Mesa: the driver VM's renderer
  (`implementation/guest/renderer/decoder`,
  `implementation/guest/renderer/drm`) is under the MIT licence, as Mesa is.
* The year is the one the file was first published in.
* Only what has to be first comes before the header: a script's `#!` line,
  an HTML page's `<!doctype html>`, an XML declaration. In C, C++ and
  JavaScript the header opens the file's first comment; Markdown, HTML and
  SVG have it in HTML comments.
* Files whose format has no comments go without (images, fonts, videos,
  test vectors, JSON, `documentation/website/CNAME`), as do `Cargo.lock`,
  which Cargo writes, licence texts, and the ports' patches, which change
  other projects' files.
* A test of xtask's (`xtask/src/licence.rs`), which `cargo xtask test`
  runs, checks every file of the repository, and every new one Git does
  not ignore.
