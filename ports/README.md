<!-- SPDX-FileCopyrightText: © 2026 Vahid Mohammadi -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Ports

Third-party software Veda builds from source: the C toolchain (see
[documentation/C.md](../documentation/C.md)), FFmpeg and dav1d, which
`mediadec` decodes videos with, Mesa, for the renderer, and the Rust crates
that Veda changes (see Crates below). Veda's own code is Rust; what is
written in C comes as a port, as does a crate that Veda changes: the
upstream release, unchanged but for a patch that is kept small, readable
and explained.

Each port is a directory:

* `port.toml`: the name, version, download address, the SHA-256 of the
  archive and the licence. `cargo xtask` downloads the archive
  (`cargo xtask toolchain` the C toolchain's, `cargo xtask linux` the
  driver VM's, every command the crates') and refuses one whose hash
  differs.
* `veda.patch`: the changes for Veda, applied with `patch -p1` to the
  unpacked archive.
* `arch/<architecture>/veda.patch`: the changes for Veda on that
  architecture, applied after `veda.patch` (binutils, GCC, musl and Linux
  have one for AMD64); and what else the port takes from the architecture
  (the driver VM kernel's options, `linux/arch/amd64/veda.config`).

`build.sh` builds the C toolchain and the media libraries (run by
`cargo xtask toolchain`), `linux/build.sh` the driver VM's Linux, its
toolchain and Mesa's build (`cargo xtask linux`), and `crates.sh`
prepares the crates (below); the comments at the top of each list what
it expects and makes. They build for the architecture xtask names
(`VEDA_ARCH`): what the scripts take from it (the target triples, musl's,
Meson's, FFmpeg's and Linux's names for it, the assembler of the media
libraries) is in `arch/<architecture>/settings.sh`.

## Crates

smoltcp and zune-jpeg are Rust crates. Each one's release is its
archive on crates.io (the `.crate` that Cargo itself would download; the
SHA-256 that `port.toml` pins is crates.io's checksum of it). The
workspace depends on the release by its version, as on any crate
(smoltcp's is `version = "=0.14.0"` in `Cargo.toml`), and Cargo builds
the port in its place:

* Every `cargo xtask` command first prepares the crates
  (`xtask/src/crates.rs`): when a port has changed, it downloads and
  checks the release, and `crates.sh` unpacks it and applies the port's
  patches in `ports/.crates/src/<crate>`, which Git ignores.
* xtask writes `ports/.crates/config.toml`, which has Cargo build those
  in place of the releases (a `[patch.crates-io]`, which
  `.cargo/config.toml` includes), and checks that `Cargo.lock` has them.

Cargo reads them before it builds anything, so they are kept outside
`target/` and `cargo clean` (removing `ports/.crates` has them prepared
anew). Where they are not ready, in a new checkout or while a port that
has changed is prepared again, Cargo resolves the releases as published,
and `Cargo.lock` shows them so, until a `cargo xtask` command has
prepared them: in a new checkout, run one (`cargo xtask doctor`) before
Cargo itself (`cargo test -p ...`, rust-analyzer).

A crate's own tests run apart from the workspace, which leaves the
crates out, from the repository's root:

```bash
cargo test --lib --manifest-path ports/.crates/src/smoltcp/Cargo.toml --target-dir target/ports
```

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

**musl**: `__init_libc` sets the POSIX layer (`libraries/posix`) up;
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

**smoltcp**: unpredictable protocol numbers. Upstream draws TCP initial
sequence numbers, DHCP transaction ids, DNS ids, the initial IPv4
identification and random source ports from an sPCG32 generator seeded
with 64 bits, whose state can be recovered from a few outputs: a server
that sees the initial sequence numbers of its own connections could
predict those of the machine's other connections and inject data into
them off-path (RFC 6528 explains the attack). `Config` gains
`random_key: Option<[u8; 32]>`; with a key, the generator (`src/rand.rs`)
gives a ChaCha20 keystream (RFC 8439's block function, a 64-bit block
counter, a zero nonce), and `netd` keys every interface with 32 bytes
from the kernel's generator. Without one, it is upstream's sPCG32
exactly, so upstream's tests still pass byte for byte.

**zune-jpeg**: sequential JPEGs with a scan per component. A baseline or
extended JPEG may code its components in separate scans, one each (T.81,
A.2.2), each holding its component's data units in raster order: for a
picture whose chroma is subsampled, the luma scan has twice the rows of
MCUs. Upstream decoded every scan for as many rows as the picture has MCU
rows, and placed each row of luma blocks two block rows apart, so these
pictures came out as stripes (libjpeg decodes them correctly);
jpeg-encoder, for one, writes every baseline JPEG with optimized Huffman
tables this way. A scan of one component is decoded for its component's
rows of data units (`single_component_scan_rows` in `src/mcu.rs`), each
8 lines of the component's plane; other pictures decode as before
(`vimage`'s `jpegs_with_a_scan_per_component` test checks it). And
`src/lib.rs` loses a crate-level `#![macro_use]`, which does nothing
there, and which rustc warns will become an error. Left as it is, as
`vimage` does not use it: CMYK and YCCK pictures converted to RGBA (or
BGRA) are written three bytes a pixel (`color_convert_cymk_to_rgb` and
`color_convert_ycck_to_rgb` in `src/worker.rs`); `vimage` converts them
to RGB, which is right.

## Updating a port

1. Download the new release and its signature; check the signature
   against the project's keys (GNU: the GNU keyring; musl: its key on
   musl.libc.org). A crate's release, its archive on crates.io, has no
   signature: its SHA-256 is the checksum crates.io's index gives it
   (which a `Cargo.lock` with the release shows).
2. Put the new version, address and SHA-256 into `port.toml`, and a
   crate's version into the workspace's `Cargo.toml` too.
3. Unpack the release, apply the old `veda.patch` (and then each
   architecture's `arch/<architecture>/veda.patch`) and resolve what no
   longer applies; then make the patches again (`diff -ruN a b`, paths
   starting at the release's top directory: `veda.patch` against the
   pristine release, an architecture's against the release with
   `veda.patch` applied). What only an architecture needs goes in its
   patch.
4. `cargo xtask toolchain` rebuilds what changed (`cargo xtask linux`
   the driver VM's; any `cargo xtask` command prepares a crate again, and
   its own tests run as Crates above shows); then `cargo xtask test`.

## Licences

The ports keep their licences: GCC and binutils GPL-3.0-or-later (libgcc
with the GCC Runtime Library Exception), GMP, MPFR and MPC
LGPL-3.0-or-later, musl MIT, FFmpeg LGPL-2.1-or-later, dav1d BSD-2-Clause,
the parts of Mesa the renderer uses MIT, smoltcp 0BSD, and zune-jpeg MIT,
Apache-2.0 or Zlib. Distributing Veda's image, which contains them, means
offering their source: the archives `port.toml` names and the patches
here.

Some of the formats FFmpeg decodes (H.264, HEVC and AAC among them) are
covered by patents in some countries. The software licences do not
grant those; whoever distributes Veda where they apply decides whether
licences are needed (or builds FFmpeg without those decoders: their
names are in `build.sh`).
