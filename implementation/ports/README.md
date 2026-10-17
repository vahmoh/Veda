<!-- SPDX-FileCopyrightText: © 2026 Vahid Mohammadi -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Ports

Third-party software that Veda builds from source. Veda's own code is
Rust; C code, Rust itself and any crate Veda changes come as ports: the
upstream release, pinned by its SHA-256 and changed only by a small,
readable and explained patch. The fonts Veda ships are here too, as
released.

**Contents:** [How ports are built](#how-ports-are-built) · [Toolchain](#toolchain) · [Driver VM](#driver-vm) · [Crates](#crates) · [Fonts](#fonts) · [Updating a port](#updating-a-port) · [Licences](#licences)

## How ports are built

Each port is a directory:

| File | What it holds |
|------|---------------|
| `port.toml` | The name, version, download address, the archive's SHA-256 and the licence |
| `veda.patch` | Veda's changes, applied with `patch -p1` to the unpacked archive |
| `arch/<architecture>/` | The architecture's `veda.patch`, applied after the port's, and its other files, such as the driver VM kernel's options |

The `cargo orchestration` command that builds a port downloads its archive
first, refusing one whose SHA-256 differs from the pin. For Rust it also
fetches its stage 0 (the compiler, standard library and Cargo of the
release that builds it), checked against the SHA-256s in the release's
`src/stage0`.

The scripts (`build.sh`, `linux/build.sh`, `crates.sh`; `common.sh`
holds what they share) list what they expect and make in their opening
comments. They build for the architecture `cargo orchestration` names
(`VEDA_ARCH`), with its settings from `arch/<architecture>/settings.sh`
(target triples, the names other projects give the architecture, the
media libraries' assembler). A stage runs again only when its inputs
change.

## Toolchain

`cargo orchestration toolchain` builds these with `build.sh`, in
`target/toolchain`, as [C on Veda](../../documentation/C.md#the-toolchain)
and [Rust on Veda](../../documentation/RUST.md#the-toolchain) describe.

| Port | Version | Licence | What Veda uses it for |
|------|---------|---------|-----------------------|
| `binutils` | 2.47 | GPL-3.0-or-later | Assembler, linker, object file tools |
| `gcc` | 16.2.0 | GPL-3.0-or-later WITH GCC-exception-3.1 | C and C++ (cross), C (native) |
| `gmp`, `mpfr`, `mpc` | 6.3.0, 4.2.2, 1.4.1 | GMP LGPL-3.0-or-later OR GPL-2.0-or-later; MPFR and MPC LGPL-3.0-or-later | The native GCC's libraries |
| `musl` | 1.2.6 | MIT | The C library |
| `rust` | 1.99.0 | MIT OR Apache-2.0; its LLVM Apache-2.0 WITH LLVM-exception | Compiler, standard library for Veda's target, Clippy |
| `dav1d` | 1.5.4 | BSD-2-Clause | AV1, in FFmpeg |
| `ffmpeg` | 9.0.2 | LGPL-2.1-or-later | `mediadec`, the sandboxed decoder of Videos |

What the patches change:

* **binutils**: `veda` in `config.sub`; on AMD64, `x86_64-veda` in BFD
  (`x86_64_elf64_vec`), gas and ld (`elf_x86_64`, with code on pages of its
  own, as on Linux).
* **gcc**: Veda's target in `config.sub`, `gcc/config.gcc`,
  `libgcc/config.host`, `gcc/config/veda.h` (`/system/lib`, static
  linking, `rcrt1.o` for `-static-pie`, musl's types) and `veda.opt`
  (`-pthread`, `-rdynamic`, `-posix`); no `fixincludes` for musl's
  headers; libstdc++ checks Veda's C library as Linux's when cross
  compiled. On AMD64, `gcc/config/i386/veda.h`: the System V x86-64 ABI,
  the stack protector's canary where musl keeps it.
* **gmp**, **mpfr**, **mpc**: `config.sub` knows `veda`.
* **musl**: `__init_libc` sets the POSIX layer up, `posix_spawn` asks it to
  start the program, `dl_iterate_phdr` names the program by `AT_EXECFN`
  (there is no `/proc/self/exe`), names not in `/etc/hosts` go to the
  network service's resolver (`__lookup_name`), and musl's DNS client to
  its DNS servers (`__get_resolv_conf`). On AMD64, system calls, `_start`
  (the bootstrap channel) and the thread routines (`src/thread/x86_64/*.s`)
  go through the layer; `vfork` takes the generic version, which fails:
  there is no `fork`. See [C on Veda](../../documentation/C.md#how-it-works).
* **rust**: Veda's target in the compiler (`Os::Veda`,
  `spec/base/veda.rs`; on AMD64 `x86_64-unknown-veda`, linked by
  `x86_64-veda-gcc`, in `STAGE0_MISSING_TARGETS`) and in `library/std`,
  which takes the code of the systems whose C library has what Veda's has;
  Linux's definitions on musl in the vendored `libc` (`vendor/libc-*`;
  `build.sh` updates its `.cargo-checksum.json`); GCC's unwinder in
  `library/unwind`, shared with C++; `AT_EXECFN` for `library/backtrace`.
  See [Rust on Veda](../../documentation/RUST.md#how-it-works).
* **ffmpeg**: `veda` as a `--target-os` in `configure`. **dav1d**: none.

dav1d (8- and 16-bit) and FFmpeg are built static, with the architecture's
assembler (nasm on AMD64) and their `checkasm`, which `systest` runs on
Veda, into `target/toolchain/media`, as only `mediadec` links them. FFmpeg
has only the demuxers, decoders (AV1 through dav1d), parsers, bitstream
filter, scaler and resampler that `mediadec` uses. It stays LGPL:
`build.sh` checks that `config.h` has `CONFIG_GPL 0` and
`CONFIG_NONFREE 0`, and that the assembly, threads and dav1d were found.

## Driver VM

`cargo orchestration linux` builds these with `linux/build.sh`, in
`target/linux`, for the [driver VM](../../documentation/DRIVERVM.md).

| Port | Version | Licence | What Veda uses it for |
|------|---------|---------|-----------------------|
| `linux` | 7.2.9 | GPL-2.0-only | The driver VM's kernel |
| `mesa` | 26.2.4 | MIT (the parts the renderer uses) | OpenGL ES in the renderer: Gallium, NIR, Intel's shader compiler |
| `zlib` | 1.3.2 | Zlib | Compression, for libelf |
| `elfutils` | 0.196 | LGPL-3.0-or-later OR GPL-2.0-or-later | libelf for objtool, the kernel's build tool, so the build machine needs none |

What the patches change:

* **linux**: the Veda platform (`veda.patch`, and `arch/amd64/veda.patch`),
  whose files [The pieces](../../documentation/DRIVERVM.md#the-pieces)
  lists. The kernel is `allnoconfig` with the options of `veda.config` and
  `arch/<architecture>/veda.config`, and the build fails if one does not
  take. `firmware.txt` and `arch/<architecture>/firmware.txt` list the
  firmware it loads, with each file's source and SHA-256.
* **zlib**, **elfutils**: none; built static for the build machine.
* **mesa**: Veda's renderer (`implementation/guests/linux/renderer`) is a
  target of Mesa's build (`src/gallium/targets/veda`): the guest's program
  around its Rust half (`veda-renderer-lib`), and `vgallium.so` for the
  OpenGL ES tests on the build machine (softpipe, virgl over
  virglrenderer's test server, iris). Gallium is built without an OpenGL
  API (`with_gfx_compute`), with the renderer's small libdrm; the static
  program does without Mesa's build id and links Gallium's stand-ins for
  video. iris is built without what needs Mesa's OpenCL compiler (LLVM):
  indirect draws generated on the GPU, and blorp's indirect copies (its
  bounds check is in NIR instead). `linux/build.sh` configures Mesa
  (Meson) with the guest's toolchain, built from the same binutils, GCC
  and musl releases without Veda's patches; `cargo orchestration` builds
  the renderer (Ninja) with the system.

## Crates

Rust crates that Veda changes. The releases of all but smoltcp and
zune-jpeg do not know Veda; their ports let the crates on top of them run
in Veda's programs on the standard library.

| Port | Version | Licence | What Veda uses it for |
|------|---------|---------|-----------------------|
| `smoltcp` | 0.14.0 | 0BSD | `netd`'s TCP/IP stack (`vnetstack`) |
| `zune-jpeg` | 0.5.15 | MIT OR Apache-2.0 OR Zlib | `vimage`'s JPEG decoder |
| `libc` | 0.2.190 | MIT OR Apache-2.0 | Bindings of the C library |
| `getrandom`, `getrandom-0.3` | 0.4.3, 0.3.4 | MIT OR Apache-2.0 | Random bytes; 0.3 for crates still on it (`rand` 0.9) |
| `rustix` | 1.1.5 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | POSIX and Linux system calls |
| `errno` | 0.3.14 | MIT OR Apache-2.0 | The C library's `errno` |
| `socket2` | 0.6.5 | MIT OR Apache-2.0 | Sockets, under `tokio` |
| `mio` | 1.2.4 | MIT | The event loop under `tokio` |
| `tokio` | 1.53.3 | MIT | The asynchronous runtime |

A crate's release is its `.crate` archive on crates.io, pinned by
crates.io's checksum. The workspace depends on its version, directly
(`version = "=0.14.0"` for smoltcp in `Cargo.toml`) or through another
crate, and Cargo builds the port instead:

1. Every `cargo orchestration` command first prepares the crates
   (`orchestration/src/crates.rs`): it downloads and checks a changed
   port's release, and `crates.sh` unpacks and patches it in
   `implementation/ports/.crates/src/<crate>`, which Git ignores.
2. It writes `implementation/ports/.crates/config.toml`, a
   `[patch.crates-io]` that `.cargo/config.toml` includes, and checks that
   `Cargo.lock` has the prepared crates.

When the workspace has two releases of a crate, the older one's port is
named for its release (`getrandom-0.3`), and its entry in the patch names
the crate as its `package`. The prepared crates are kept outside
`target/` and `cargo clean`, as Cargo reads them before it builds
anything; removing `implementation/ports/.crates` has them prepared anew.

> [!IMPORTANT]
> Until the crates are prepared, Cargo resolves their releases as
> published, and `Cargo.lock` shows them so. In a new checkout, run a
> `cargo orchestration` command (`cargo orchestration doctor`) before
> Cargo itself (`cargo test -p ...`, rust-analyzer).

A crate's own tests run outside the workspace, from the repository's
root:

```bash
cargo test --lib --manifest-path implementation/ports/.crates/src/smoltcp/Cargo.toml --target-dir target/ports
```

What the patches change:

* **smoltcp**: seven changes, each with tests of its own.
  * **Unpredictable protocol numbers** (`src/rand.rs`): given
    `Config::random_key`, initial sequence numbers, DHCP and DNS ids, IPv4
    identification and source ports come from ChaCha20, not the
    predictable sPCG32 (RFC 6528); `netd` keys it from the kernel's
    generator.
  * **Quiet connections** (`src/socket/tcp.rs`): the timeout runs only
    while a SYN, data or a FIN of ours awaits acknowledgement or
    keep-alive is on (`awaits_ack`), so idle connections stay up;
    `has_timed_out` tells a timeout from a reset.
  * **Windows that end short** (`src/socket/tcp.rs`): data up to the
    furthest edge advertised (`remote_max_edge`) is accepted, as RFC 7323
    (2.4) has it, even when a rounded-down window ends short of it.
  * **The lease** (`src/socket/dhcpv4.rs`): `lease_expires_at` gives the
    end of the lease the client accepted; `netd` had read leases from any
    acknowledgement on the network.
  * **Stable privacy addresses** (`src/iface/interface`, `src/rand.rs`):
    given `Config::slaac_secret`, autoconfigured addresses get RFC 7217
    identifiers, not EUI-64 ones, which carry the hardware address between
    networks; `Interface::ipv6_address_in` forms `netd`'s link-local
    addresses.
  * **Errors for ping** (`src/socket/icmp.rs`, `src/wire/icmpv4.rs`): ICMP
    sockets receive the Destination Unreachable and Time Exceeded messages
    quoting their echo requests, and ICMPv4 errors quoting only a packet's
    start (RFC 792) are parsed.
  * **Neighbours found at once** (`src/iface/neighbor.rs`): ARP requests
    and Neighbor Solicitations are rate-limited per address (RFC 1122,
    2.3.2.1), not for the whole cache.
* **zune-jpeg**: sequential JPEGs with a scan per component (T.81, A.2.2),
  as jpeg-encoder writes baseline files with optimised Huffman tables,
  decode instead of coming out as stripes (`single_component_scan_rows` in
  `src/mcu.rs`, tested by `vimage`'s `jpegs_with_a_scan_per_component`),
  and `src/lib.rs` loses a `#![macro_use]` that rustc warns will become an
  error. Left as released, as `vimage` does not use it: CMYK and YCCK
  pictures converted to RGBA or BGRA come out three bytes a pixel
  (`src/worker.rs`).
* **libc**: Veda takes Linux's definitions on musl, as in Rust's vendored
  copy: wherever the release names Linux beside other systems, it names
  Veda too; `build.rs` counts Veda among the targets with only a recent
  musl and in `check-cfg`.
* **getrandom**, **getrandom-0.3**: `src/backends.rs` calls the C library's
  `getrandom()`, which the POSIX layer always provides, without a fallback,
  reading errno as on Linux; libc's deprecated `INT_MAX` becomes
  `c_int::MAX`.
* **rustix**: on its `libc` backend, Veda is Linux's kernel behind musl, as
  with `rustix_use_libc`: `build.rs` gives it `linux_like` and
  `linux_kernel` (with `linux-raw-sys`'s constants), and every
  `target_os = "linux"` outside the `linux_raw` backend names Veda too. Its
  `[hints]` go, as Cargo warns about them for a crate built from a path.
* **errno**: read where Linux's C library keeps it (`__errno_location`).
* **socket2**, **mio**, **tokio**: every `target_os = "linux"` outside
  comments names Veda too, so they take Linux's code (`epoll`, `eventfd`,
  `accept4`, `SOCK_NONBLOCK`, `pidfd_open`, socket options), which the
  POSIX layer carries out; mio and tokio add Veda to `check-cfg`. Their
  Unix domain sockets (mio's `src/sys/unix/uds/mod.rs`, tokio's
  `src/net/unix/{listener,socketaddr,stream}.rs`) stay as released, named
  by paths, as Veda's standard library has no abstract names
  (`std::os::linux`); tokio's `peer_cred` is Linux's `SO_PEERCRED`.

## Fonts

[`fonts/`](fonts/README.md) holds Inter, Lato and JetBrains Mono,
unmodified, under the SIL Open Font License 1.1 (`OFL.txt`); its README
gives each one's copyright. Nothing is built or patched:
`cargo orchestration build` installs them in `/system/fonts`, licence
included, and `vfont`'s tests and `videogen` read them from here.

## Updating a port

1. Download the new release and check its signature against the
   project's keys: the GNU keyring, musl's key on musl.libc.org, Rust's
   release key on rust-lang.org (which also vouches for the archive's
   `src/stage0`). A crate on crates.io has no signature: its SHA-256 is
   the checksum crates.io's index gives, which a `Cargo.lock` with the
   release shows.
2. Put the new version, address and SHA-256 in `port.toml`, and a crate's
   version in the workspace's `Cargo.toml` too.
3. Unpack the release, apply the old `veda.patch`, then each
   `arch/<architecture>/veda.patch`, and resolve what no longer applies.
   Make the patches again with `diff -ruN a b`, paths starting at the
   release's top directory: `veda.patch` against the pristine release, an
   architecture's against the release with `veda.patch` applied (what only
   an architecture needs goes there).
4. Rebuild with the port's command, which rebuilds what changed (run a
   crate's own tests as [Crates](#crates) shows), then run
   `cargo orchestration test`.

## Licences

The ports keep their licences, given in the tables above. Distributing
Veda's image means offering the source of the ports it contains: the
archives `port.toml` names and the patches here.
[Licensing](../../documentation/LICENSING.md) covers Veda as a whole.

> [!NOTE]
> Some formats FFmpeg decodes (H.264, HEVC and AAC among them) are covered
> by patents in some countries, which the software licences do not grant.
> Whoever distributes Veda there decides whether licences are needed, or
> leaves those decoders out of `build.sh`.
