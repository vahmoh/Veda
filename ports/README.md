<!-- SPDX-FileCopyrightText: © 2026 Vahid Mohammadi -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Ports

Third-party software Veda builds from source: the C toolchain (see
[docs/C.md](../docs/C.md)), FFmpeg and dav1d, which `mediadec` decodes
videos with, and Mesa, for the renderer. Veda's own code is
Rust; what is written in C
comes as a port: the upstream release, unchanged but for a patch that is
kept small, readable and explained.

Each port is a directory:

* `port.toml`: the name, version, download address, the SHA-256 of the
  archive and the licence. `cargo xtask toolchain` downloads the archive
  and refuses one whose hash differs.
* `veda.patch`: the changes for Veda, applied with `patch -p1` to the
  unpacked archive.
* `arch/<architecture>/veda.patch`: the changes for Veda on that
  architecture, applied after `veda.patch` (binutils, GCC, musl and Linux
  have one for AMD64); and what else the port takes from the architecture
  (the driver VM kernel's options, `linux/arch/amd64/veda.config`).

`build.sh` builds them all (`cargo xtask toolchain` runs it; the
comments at its top list what it expects and makes), for the
architecture xtask names (`VEDA_ARCH`): what the scripts take from it (the
target triples, musl's, Meson's, FFmpeg's and Linux's names for it, the
assembler of the media libraries) is in `arch/<architecture>/settings.sh`.

## The patches

**binutils**: `veda` in `config.sub`; for AMD64, `x86_64-veda` in BFD
(`x86_64_elf64_vec`), gas (ELF) and ld (`elf_x86_64`, with code on pages
of its own, as on Linux).

**gcc**: `veda` in `config.sub`; `gcc/config/veda.h` (the system's
macros, `/system/lib` for start files and libraries, static linking:
`crt1.o` or, with `-static-pie`, `rcrt1.o`; musl's type definitions),
`gcc/config/veda.opt` (`-pthread`, `-rdynamic`, `-posix`; and
`veda.opt.urls`, its links to GCC's manual), the system's entries in
`gcc/config.gcc` and `libgcc/config.host`, no `fixincludes` for musl's
headers, and libstdc++ checking Veda's C library as it checks Linux's when
cross compiled (`libstdc++-v3/crossconfig.m4`, and the same line in the
`configure` generated from it). For AMD64: `x86_64-veda`'s entries in
`gcc/config.gcc` and `libgcc/config.host`, and `gcc/config/i386/veda.h`
(the System V x86-64 ABI, the stack protector's canary where musl keeps
it).

**gmp, mpfr, mpc**: `config.sub` knows `veda`, so they build for Veda as
the native GCC's libraries.

**musl**: `__init_libc` sets the POSIX layer (`lib/posix`) up;
`posix_spawn` asks the layer to start the program. For AMD64: system calls
go to the layer instead of the `syscall` instruction
(`arch/x86_64/syscall_arch.h`); `_start` keeps the bootstrap channel
(`crt_arch.h`); thread creation, the thread pointer, a thread freeing its
own stack and cancellable system calls go through the layer
(`src/thread/x86_64/*.s`); `vfork` falls back to the generic version
(which fails: there is no `fork`).

**dav1d**: none. It is built as released (meson, with a cross file for
Veda's target and the architecture's assembler: nasm on AMD64), static,
for 8- and 16-bit pictures, with its `checkasm`.

**ffmpeg**: `veda` as a `--target-os` in `configure`, beside `linux`. It
is built with `--disable-everything` and then only the demuxers, decoders
(AV1 through dav1d), parsers and bitstream filter `mediadec` uses, without
programs, devices, filters or network, and as LGPL: `build.sh` checks
that `config.h` has `CONFIG_GPL 0` and `CONFIG_NONFREE 0`, and that the
assembly, threads and dav1d were found. Its `checkasm` is built too.
Both libraries are installed apart from the C toolchain
(`target/toolchain/media`), since only `mediadec` links them; `systest`
runs both `checkasm` programs on Veda.

**mesa**: Veda's renderer (`guest/renderer`), which carries out the
OpenGL ES command streams of applications on Mesa's Gallium drivers in the
driver VM, is a target of Mesa's build: `src/gallium/targets/veda`,
compiling the decoder the `veda-renderer` option names, links the
renderer's program for the guest around its Rust half
(`veda-renderer-lib`) and, for the machine that builds it, `vgallium.so`
(the decoder on softpipe and on virgl over virglrenderer's test server)
for the OpenGL ES tests. Gallium's own state handling is built without
any OpenGL API (`with_gfx_compute`), with the renderer's small libdrm
(`guest/renderer/drm`); the program, static, has no dynamic linker that
would tell Mesa its build id, which it does without, and links Gallium's
stand-ins for video. iris, Intel's driver, is built without what needs
Mesa's OpenCL compiler (LLVM): generating indirect draws on the GPU, and
blorp's indirect copies (its bounds check is written in NIR instead).
`linux/build.sh` configures Mesa (meson) with the driver VM's own
toolchain (GCC, binutils and musl for Linux, the releases these ports
pin, unpatched); `cargo xtask` builds the renderer (ninja) with the
system.

## Updating a port

1. Download the new release and its signature; check the signature
   against the project's keys (GNU: the GNU keyring; musl: its key on
   musl.libc.org).
2. Put the new version, address and SHA-256 into `port.toml`.
3. Unpack the release, apply the old `veda.patch` (and then each
   architecture's `arch/<architecture>/veda.patch`) and resolve what no
   longer applies; then make the patches again (`diff -ruN a b`, paths
   starting at the release's top directory: `veda.patch` against the
   pristine release, an architecture's against the release with
   `veda.patch` applied). What only an architecture needs goes in its
   patch.
4. `cargo xtask toolchain` rebuilds what changed; then `cargo xtask test`.

## Licences

The ports keep their licences: GCC and binutils GPL-3.0-or-later (libgcc
with the GCC Runtime Library Exception), GMP, MPFR and MPC
LGPL-3.0-or-later, musl MIT, FFmpeg LGPL-2.1-or-later, dav1d BSD-2-Clause,
and the parts of Mesa the renderer uses MIT. Distributing Veda's image,
which contains them, means offering their source: the archives
`port.toml` names and the patches here.

Some of the formats FFmpeg decodes (H.264, HEVC and AAC among them) are
covered by patents in some countries. The software licences do not
grant those; whoever distributes Veda where they apply decides whether
licences are needed (or builds FFmpeg without those decoders: their
names are in `build.sh`).
