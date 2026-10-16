<!-- SPDX-FileCopyrightText: © 2026 Vahid Mohammadi -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Ports

Third-party software Veda builds from source: the C toolchain (see
[documentation/C.md](../../documentation/C.md)), Rust (see
[documentation/RUST.md](../../documentation/RUST.md)), FFmpeg and dav1d,
which `mediadec` decodes videos with, Mesa, for the renderer, and the Rust
crates that Veda changes (see Crates below). Veda's own code is Rust; what
is written in C comes as a port, as do Rust itself and a crate that Veda
changes: the upstream release, unchanged but for a patch that is kept
small, readable and explained. The fonts Veda ships are here too, as
released (see Fonts below).

Each port is a directory:

* `port.toml`: the name, version, download address, the SHA-256 of the
  archive and the licence. `cargo orchestration` downloads the archive
  (`cargo orchestration toolchain` the toolchain's,
  `cargo orchestration linux` the driver VM's, every command the crates')
  and refuses one whose hash differs; for Rust also the release that builds
  it (its stage 0: its compiler, standard library and Cargo for this
  machine), as the release's `src/stage0` pins them by SHA-256.
* `veda.patch`: the changes for Veda, applied with `patch -p1` to the
  unpacked archive.
* `arch/<architecture>/veda.patch`: the changes for Veda on that
  architecture, applied after `veda.patch` (binutils, GCC, musl, Rust and
  Linux have one for AMD64); and what else the port takes from the
  architecture (the driver VM kernel's options,
  `linux/arch/amd64/veda.config`).

`build.sh` builds the C toolchain, the media libraries and Rust (run by
`cargo orchestration toolchain`), `linux/build.sh` the driver VM's Linux, its
toolchain and Mesa's build (`cargo orchestration linux`), and `crates.sh`
prepares the crates (below); the comments at the top of each list what
it expects and makes. They build for the architecture `cargo orchestration`
names (`VEDA_ARCH`): what the scripts take from it (the target triples,
musl's, Meson's, FFmpeg's and Linux's names for it, Rust's target and LLVM's
name of its code generator, the assembler of the media libraries) is in
`arch/<architecture>/settings.sh`.

## Crates

smoltcp, zune-jpeg, libc, getrandom, rustix, errno, socket2, mio and
tokio are Rust crates. libc, getrandom, rustix and errno are what crates
that need Rust's standard library stand on to reach the operating system,
and tokio, with mio and socket2 under it, is the asynchronous runtime of
Rust's ecosystem; their releases do not know Veda: their ports let such
crates build and run in Veda's programs on the standard library (see their
patches below). Each one's release is its
archive on crates.io (the `.crate` that Cargo itself would download; the
SHA-256 that `port.toml` pins is crates.io's checksum of it). The
workspace depends on the release by its version, as on any crate
(smoltcp's is `version = "=0.14.0"` in `Cargo.toml`), or has it among the
crates it depends on, and Cargo builds the port in its place. A crate the
workspace has two releases of has a port for each: the older's directory
is named for its release (`getrandom-0.3`), and its patch in Cargo's
configuration gives the crate's name as its `package`:

* Every `cargo orchestration` command first prepares the crates
  (`orchestration/src/crates.rs`): when a port has changed, it downloads and
  checks the release, and `crates.sh` unpacks it and applies the port's
  patches in `implementation/ports/.crates/src/<crate>`, which Git ignores.
* `cargo orchestration` writes `implementation/ports/.crates/config.toml`,
  which has Cargo build those in place of the releases (a
  `[patch.crates-io]`, which `.cargo/config.toml` includes), and checks that
  `Cargo.lock` has them.

Cargo reads them before it builds anything, so they are kept outside
`target/` and `cargo clean` (removing `implementation/ports/.crates` has
them prepared anew). Where they are not ready, in a new checkout or while a
port that has changed is prepared again, Cargo resolves the releases as
published, and `Cargo.lock` shows them so, until a `cargo orchestration`
command has prepared them: in a new checkout, run one
(`cargo orchestration doctor`) before Cargo itself (`cargo test -p ...`,
rust-analyzer).

A crate's own tests run apart from the workspace, which leaves the
crates out, from the repository's root:

```bash
cargo test --lib --manifest-path implementation/ports/.crates/src/smoltcp/Cargo.toml --target-dir target/ports
```

## Fonts

`fonts/` holds the fonts Veda ships: Inter, Lato and JetBrains Mono,
unmodified, with their licence (`OFL.txt`) and a README that gives each
one's copyright. There is nothing to build or patch:
`cargo orchestration build` installs them in `/system/fonts` as they are,
and `vfont`'s tests and `videogen` read them from here.

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

**musl**: `__init_libc` sets the POSIX layer
(`implementation/libraries/posix`) up; `posix_spawn` asks the layer to
start the program; `dl_iterate_phdr` names a program by the path it was
started from (`AT_EXECFN`) rather than `/proc/self/exe`, which Veda does
not have (backtraces read the program's symbols from it); names that are
not in `/etc/hosts` are looked up by the network service's resolver
(`__lookup_name` asks the layer, and keeps the order of the addresses it
gives), and musl's DNS client asks the DNS servers the service uses
(`__get_resolv_conf`), as there is no `/etc/resolv.conf`. For AMD64:
system calls go to the layer instead of the `syscall` instruction
(`arch/x86_64/syscall_arch.h`); `_start` keeps the bootstrap channel
(`crt_arch.h`); thread creation, the thread pointer, a thread freeing its
own stack and cancellable system calls go through the layer
(`src/thread/x86_64/*.s`); `vfork` falls back to the generic version
(which fails: there is no `fork`).

**rust**: Veda as a target of Rust's compiler and of its standard library,
which stands on the C library there. The compiler: Veda among its systems
(`Os::Veda`) and `spec/base/veda.rs`: the `unix` family,
`target_env = "musl"`, no dynamic linking, programs static and
position-independent by default (`-static-pie`), thread-local storage,
unwind tables; for AMD64, `x86_64-unknown-veda`
(`spec/targets/x86_64_unknown_veda.rs`), linked by `x86_64-veda-gcc`,
which bootstrap builds though its stage 0 does not know it
(`STAGE0_MISSING_TARGETS`). The standard library (`library/std`) takes on
Veda the ways of the systems whose C library has what Veda's has:
futexes for its locks and thread parking (`FUTEX_WAIT_BITSET` with
absolute deadlines, which the POSIX layer carries out),
`clock_nanosleep`, `getrandom`, `pipe2`, `preadv` and `pwritev` (1024
vectors at most, the layer's limit), `fstatat` and `openat` in
directories (`remove_dir_all` without races), `fdatasync`, `posix_spawn`
for every process (there is no `fork`; its `addchdir_np` linked
directly, as with musl on Linux), `SOCK_CLOEXEC`, `accept4` and
`MSG_NOSIGNAL`; `current_exe` from the loader's `AT_EXECFN` (there is no
`/proc`); neither thread names, nor file locks (`Unsupported`), nor a
stack-overflow handler (a fault ends the program); and
`std::os::veda::fs::MetadataExt`. The `libc` crate, the standard
library's bindings of the C library, vendored in `vendor/libc-*` (the
copy Rust's build compiles): Veda's C library has Linux's interface, so
each of the crate's conditions on `target_os = "linux"` that can hold
with musl holds for Veda too (those for glibc, uClibc and 32-bit time
stay as they are), and its musl is a recent one (`musl_v1_2_3`);
`build.sh` gives the files the patch changes their new SHA-256 in the
crate's `.cargo-checksum.json`, which Cargo checks. `library/unwind`:
GCC's unwinder, which is in libgcc (GCC is built for static linking
alone), as C++'s exceptions have it: a program in Rust and C++ has one
unwinder. `library/backtrace`: a program's symbols and lines come from
its own file, found through `AT_EXECFN` (there is no `/proc/self/maps`).
Rust is built with its LLVM, for the architecture's code generator
alone, by the release before it (its stage 0), as Rust's releases are;
its Cargo runs from outside Veda's checkout, whose `.cargo/config.toml`
(the crates' `[patch]`) must not reach Rust's build, with the release's
own configuration (its vendored crates) instead. With the compiler and the
standard library it installs Clippy, and the standard library's source
as Veda patches it, copied where Rust's `rust-src` puts it (Rust's own step
vendors its crates again with Cargo in the source tree).

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

**mesa**: Veda's renderer (`implementation/guest/renderer`), which carries
out the OpenGL ES command streams of applications on Mesa's Gallium drivers
in the driver VM, is a target of Mesa's build: `src/gallium/targets/veda`,
compiling the decoder the `veda-renderer` option names, links the
renderer's program for the guest around its Rust half
(`veda-renderer-lib`) and, for the machine that builds it, `vgallium.so`
(the decoder on softpipe and on virgl over virglrenderer's test server)
for the OpenGL ES tests. Gallium's own state handling is built without
any OpenGL API (`with_gfx_compute`), with the renderer's small libdrm
(`implementation/guest/renderer/drm`); the program, static, has no dynamic
linker that would tell Mesa its build id, which it does without, and links
Gallium's stand-ins for video. iris, Intel's driver, is built without what
needs Mesa's OpenCL compiler (LLVM): generating indirect draws on the GPU,
and blorp's indirect copies (its bounds check is written in NIR instead).
`linux/build.sh` configures Mesa (meson) with the driver VM's own
toolchain (GCC, binutils and musl for Linux, the releases these ports
pin, unpatched); `cargo orchestration` builds the renderer (ninja) with the
system.

**smoltcp**: seven changes, each with tests of its own.

*Unpredictable protocol numbers* (`src/rand.rs`). Upstream draws TCP
initial sequence numbers, DHCP transaction ids, DNS ids, the initial IPv4
identification and random source ports from an sPCG32 generator seeded
with 64 bits, whose state can be recovered from a few outputs: a server
that sees the initial sequence numbers of its own connections could
predict those of the machine's other connections and inject data into
them off-path (RFC 6528 explains the attack). `Config` gains
`random_key: Option<[u8; 32]>`; with a key, the generator gives a ChaCha20
keystream (RFC 8439's block function, a 64-bit block counter, a zero
nonce), and `netd` keys every interface with 32 bytes from the kernel's
generator. Without one, it is upstream's sPCG32 exactly, so upstream's
tests still pass byte for byte.

*Quiet connections* (`src/socket/tcp.rs`). With a timeout set, upstream
reset any TCP connection whose peer had been silent for longer, an idle
one included, although `set_timeout` documents the timeout as applying
only while data waits in the transmit buffer or keep-alive is on: an
established connection with nothing to say for as long (two minutes in
`netd`: a terminal session, a WebSocket waiting for news) was cut. The
timeout now applies only while a SYN, data or a FIN of ours waits for the
peer's acknowledgement, or keep-alive is on (`awaits_ack`), and counts
from when that began. And a closed connection tells whether the timeout
ended it (`has_timed_out`), so that `netd` reports a peer that stopped
answering as such, and a reset as a reset.

*Windows that end short* (`src/socket/tcp.rs`). A receive window is
advertised in units of the window scale, rounded down, so an
advertisement can end a few octets short of an earlier one, up to which
the sender may already have sent. Upstream dropped those octets as out of
the window, and the sender, which had sent them, sent them again only once
its retransmission timer expired, a second at least: a transfer over the
loopback interface (whose 64 KiB buffers scale windows by 2) stalled for
that long now and then. A connection now keeps the furthest edge it
advertised (`remote_max_edge`) and takes what comes up to it, as RFC 7323
(2.4) has a receiver do, since it must not shrink its window: those octets
fit, as the buffer had the room when that edge was advertised.

*The lease* (`src/socket/dhcpv4.rs`). The DHCP client tells when the
lease it accepted ends (`lease_expires_at`); `netd` had read the lease
from any acknowledgement on the network, another client's included.

*Stable privacy addresses* (`src/iface/interface`, `src/rand.rs`). Given
`Config::slaac_secret`, the addresses autoconfiguration forms have RFC
7217 interface identifiers (ChaCha20 keyed with the secret over the prefix
and the hardware address, RFC 5453's reserved identifiers passed over)
instead of the EUI-64 one, which carries the hardware address from
network to network; `Interface::ipv6_address_in` gives an interface's
address in any /64 prefix the same way (`netd`'s link-local addresses).
Without a secret, they are upstream's.

*Errors for ping* (`src/socket/icmp.rs`, `src/wire/icmpv4.rs`). An ICMP
socket bound to an identifier also receives the Destination Unreachable
and Time Exceeded messages that quote an echo request with that
identifier, so ping can tell why a request went unanswered, and which
router said so. And an ICMPv4 error whose quoted packet is cut short is
parsed: upstream wanted the whole packet, where RFC 792 asks routers for
its header and first eight bytes, so it dropped the errors about all but
the shortest packets.

*Neighbors found at once* (`src/iface/neighbor.rs`). Upstream limited
the rate of ARP requests and neighbor solicitations for the whole cache:
once it had asked for one neighbor, it asked for no other for a second,
dropping the packets meant for them. The first connection to the
Internet after a DNS lookup (the DNS server's address asked for, then the
gateway's) waited a second for its SYN to be sent again. The rate is
limited per address, as RFC 1122 has it (2.3.2.1), in a map as large as
the cache.

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

**libc**: Veda's target (`target_os = "veda"`) takes Linux's definitions
on musl, as the rust port's patch has the copy that Rust's standard library
builds with take them: Veda's C library is musl, with Linux's interface,
which the POSIX layer carries out. Wherever the release names Linux
beside other systems (the modules of Linux's and of Linux-like systems,
their threads' calls), it names Veda too; `build.rs` counts Veda among the
targets that have only a recent musl, and among the operating systems the
compiler is told of (`check-cfg`).

**getrandom** (0.4) and **getrandom-0.3**: Veda's target among the systems
whose C library has `getrandom()` (`src/backends.rs`), which it then calls
without a fallback (Veda's always has it: the POSIX layer gives it the
kernel's generator), with libc as a dependency there, and errno where
Linux's is; and the constant `INT_MAX`, which libc deprecates, as
`c_int::MAX`, so that the crates build without a warning.

**rustix**: on the C library (its `libc` backend, which it takes for any
system but Linux), Veda is Linux's kernel behind musl, as rustix already
supports Linux with `rustix_use_libc`: `build.rs` gives Veda its
`linux_like` and `linux_kernel` configurations, with `linux-raw-sys` for
Linux's constants, and every `target_os = "linux"` of its sources outside
the `linux_raw` backend reads `any(target_os = "linux", target_os =
"veda")`. Its `[hints]`, which only a nightly Cargo takes, go: Cargo warns
of them for a crate it builds from a path.

**errno**: Veda's target reads errno where Linux's C library keeps it
(`__errno_location`).

**socket2**, **mio** and **tokio**: Veda is Linux behind musl, as for
rustix: every `target_os = "linux"` of their sources outside comments
reads `any(target_os = "linux", target_os = "veda")`, so that they take
Linux's ways (mio's `epoll` selector and `eventfd` waker, `accept4` and
`SOCK_NONBLOCK`; tokio's `pidfd_open` for its children; socket2's options),
which the POSIX layer carries out; Cargo's `check-cfg` names Veda among
the operating systems (mio, tokio). The exception is Unix sockets' abstract
names, which Veda's standard library does not have (`std::os::linux`;
its C library does): mio's `src/sys/unix/uds/mod.rs` and tokio's
`src/net/unix/{listener,socketaddr,stream}.rs` stay as released, and with
them Unix domain sockets as other Unix systems have them, named by paths
(tokio's credentials, `peer_cred`, are Linux's: `SO_PEERCRED`).

## Updating a port

1. Download the new release and its signature; check the signature
   against the project's keys (GNU: the GNU keyring; musl: its key on
   musl.libc.org; Rust: its release key, on rust-lang.org, whose
   signature also vouches for the archive's `src/stage0`, which pins the
   release that builds it). A crate's release, its archive on crates.io,
   has no signature: its SHA-256 is the checksum crates.io's index gives
   it (which a `Cargo.lock` with the release shows).
2. Put the new version, address and SHA-256 into `port.toml`, and a
   crate's version into the workspace's `Cargo.toml` too.
3. Unpack the release, apply the old `veda.patch` (and then each
   architecture's `arch/<architecture>/veda.patch`) and resolve what no
   longer applies; then make the patches again (`diff -ruN a b`, paths
   starting at the release's top directory: `veda.patch` against the
   pristine release, an architecture's against the release with
   `veda.patch` applied). What only an architecture needs goes in its
   patch.
4. `cargo orchestration toolchain` rebuilds what changed
   (`cargo orchestration linux` the driver VM's; any `cargo orchestration`
   command prepares a crate again, and its own tests run as Crates above
   shows); then `cargo orchestration test`.

## Licences

The ports keep their licences: GCC and binutils GPL-3.0-or-later (libgcc
with the GCC Runtime Library Exception), GMP, MPFR and MPC
LGPL-3.0-or-later, musl MIT, Rust MIT or Apache-2.0 (its compiler's LLVM
Apache-2.0 with LLVM's exceptions), FFmpeg LGPL-2.1-or-later, dav1d
BSD-2-Clause, the parts of Mesa the renderer uses MIT, smoltcp 0BSD,
zune-jpeg MIT, Apache-2.0 or Zlib, libc, getrandom, errno and socket2 MIT
or Apache-2.0, rustix Apache-2.0 with LLVM's exception, Apache-2.0 or
MIT, and mio and tokio MIT. Distributing Veda's image, which
contains them, means offering their source: the archives `port.toml`
names and the patches here. The fonts are under the SIL Open Font
License 1.1, whose text the image carries with them
(`/system/fonts/OFL.txt`).

Some of the formats FFmpeg decodes (H.264, HEVC and AAC among them) are
covered by patents in some countries. The software licences do not
grant those; whoever distributes Veda where they apply decides whether
licences are needed (or builds FFmpeg without those decoders: their
names are in `build.sh`).
