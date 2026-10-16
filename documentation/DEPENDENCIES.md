<!-- SPDX-FileCopyrightText: © 2026 Vahid Mohammadi -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Libraries

Veda builds on mature Rust libraries wherever one does what it needs in its
environment, and keeps its own code for what is particular to Veda, the glue,
and what no library does there. This document lists what a library must
fit, the libraries in use, and what Veda keeps of its own and why; the
workspace's `Cargo.toml` gives each dependency's reason beside it.

## What a library must fit

The rules are in [Coding conventions](CODING.md#language-and-dependencies).
In Veda's terms:

* **`no_std` where there is no `std`.** Veda's libraries and its programs
  on `vrt` alone are `no_std` with `alloc`. Those programs are built for
  `x86_64-unknown-linux-musl` but link nothing of Linux's or musl's, so a
  crate that reaches system calls or the C library through
  `cfg(target_os = "linux")` or `cfg(unix)`, or detects the processor
  through `std`, does not fit them. On the soft-float kernel and loader,
  crates run their portable code. Programs on Rust's standard library, the
  build machine's tools and the driver VM's Linux programs have `std`, on
  Veda's own target without `fork`, multicast or asynchronous signals
  ([Rust on Veda](RUST.md#limits)).
* **Little in every program.** Linking is static and without whole-program
  LTO, so what a crate makes reachable is in every program that calls it:
  skrifa adds about 600 KB to every GUI program, ttf-parser about 60 KB;
  fast_image_resize's `Resizer::resize` adds 3 MB, where
  `resize_typed::<U8x4>` does the same work without it.
* **One generation of a crate family.** One built on an older generation
  (RustCrypto's 0.10 digests, `linux-raw-sys` 0.9) brings both into the
  build.
* **Fixed, not worked around.** A wrong library becomes a port
  (`implementation/ports/`, as smoltcp and zune-jpeg are), and the fix goes
  upstream.

## In use

| Area | Crates | For |
|------|--------|-----|
| Networking | `smoltcp` (a port), `httparse`, `percent-encoding`, `base64ct` | TCP/IP; HTTP/1.x heads and chunk sizes; URL components; WebSocket keys |
| TLS | `rustls`, `rustls-webpki`, `webpki-roots` | HTTPS and secure WebSockets, with `vtls`'s `CryptoProvider` on RustCrypto |
| Cryptography | RustCrypto: `sha1`, `sha2`, `hmac`, `pbkdf2`, `hkdf`, `aes`, `aes-gcm`, `ccm`, `cmac`, `aes-kw`, `chacha20`, `chacha20poly1305`, `blake2`, `p256`, `p384`, `crypto-bigint`, `base16ct`; `x25519-dalek`, `ed25519-dalek`; `subtle`, `zeroize` | Wi-Fi security, TLS, the kernel's random numbers, Wi-Fi keys typed as hex |
| Data | `serde`, `serde_json`, `crc32fast` | The agent's protocols, applications' agent actions, saved settings; checksums of home snapshots, GPT headers and PNG chunks |
| Text | `ttf-parser`, `unicode-segmentation`, `unicode-width`, `vte` | Font files; grapheme boundaries and character widths; terminal escape sequences |
| Numbers | `libm`, `num-traits`, `num-rational`, `num-integer`, `glam`, `rand`, `rand_pcg`, `rand_distr`, `fastnoise-lite` | Float functions; the Calculator's exact fractions; 3D maths; seeded random numbers, the same everywhere; noise |
| Images | `zune-jpeg` (a port), `zune-png`, `zune-bmp`, `zune-inflate`, `qoi`, `fast_image_resize`, `bytemuck`, `jpeg-encoder`, `miniz_oxide` | Decoding and scaling pictures; encoding them (JPEG, and PNG's deflate) |
| Linux | `linux-raw-sys`; `rustix`, `libc` | The Linux ABI the POSIX layer gives musl; the driver VM's programs' system calls |
| Under `std` in Veda | `libc`, `getrandom`, `rustix`, `errno`, `tokio`, `mio`, `socket2` (ports that know Veda's target), `tempfile` | The C library, random bytes, system calls, temporary files and tokio (on epoll, eventfd and sockets) for crates on `std`; `verification/stdcrates` checks them |
| Build machine only | `png`, `rav1e`, `md-5`, `tungstenite` | Test output, and a second decoder for the PNGs Veda writes; sample videos; the stand-in for Deepgram's Voice Agent API |

C and C++ come as ports too: GCC, binutils and musl (the toolchain),
FFmpeg and dav1d (video decoding), Mesa (the driver VM's renderer) and
Linux (the driver VM's kernel).

## Veda's own, and why

### Kernel, loader and system

| Code | Weighed, and why not |
|------|----------------------|
| `vheap`, every program's allocator | rlsf 0.2, talc 5: no better proven than `vheap`, with its stress test and invariant checks; dlmalloc: takes its memory from the C library on the user target |
| The loader's UEFI bindings | r-efi 7 (std's): declares the firmware's inputs as mutable pointers, so every GUID and string passed would be cast |
| `velf` | object 0.40: would replace only about 400 lines of header decoding |
| `vacpi`, the AML interpreter | acpi 6.1: panics on GPIO and serial-bus resources, which Veda's devices need, and sets no step or depth limits, so firmware could stop the device manager |
| The hypervisor's ACPI tables (`vhv`, `vacpi::asm`) | acpi_tables 0.2: no GPIO, I2C or SPI descriptors, which the guest's DSDT uses |
| `vvirtio` | virtio-drivers 0.13: maps BARs by physical address; never programs MSI-X vectors |
| `viommu`, `vgpio`, `vhv`'s devices, `initrd`, `bootinfo`, `vabi` | No crate drives VT-d or Intel's GPIO pads; linux-loader: needs `std`; cpio, tar: the initrd's files are page-aligned for mapping in place; the rest is Veda's ABI |
| The kernel's heap, frame allocator, locks and x86 structures | buddy_system_allocator, bitmap-allocator: the frame allocator works before the heap and keeps DMA's address limits; spin 0.12: the locks detect recursion per processor and serve TLB shootdowns while spinning; x86_64 0.15: no EPT, VT-d or VMX |
| Calendar arithmetic (`vrt::time`, the kernel's own) | time 0.3: two short functions (Howard Hinnant's) do |

### Networking

| Code | Weighed, and why not |
|------|----------------------|
| The HTTP and WebSocket clients (`vweb`) | reqwless, edge-http: asynchronous; embedded-websocket: on older RustCrypto and `rand_core` generations; hyper, tungstenite: need `std` |
| The DNS resolver (`vnetstack`) | smoltcp's DNS socket: no TTLs and fixed limits, so it cannot back a cache (its messages are used); hickory-proto: brings ICU, `url` and `tracing` |
| `vtls`'s `CryptoProvider` and RSA verification | rustls-rustcrypto: an alpha on the older RustCrypto generation; rsa 0.10: a release candidate, 0.9 the older generation |
| The IPC codec (`vipc`) | postcard, serde: a message's kernel handles move out of it into a side table, which `Serialize(&self)` cannot do |
| The 802.11 stack (`vwlan`) | ieee80211 0.5: no SAE, CCMP or handshakes; the older RustCrypto generation |
| The driver VM's DRM and netlink (`guest/kms`, `guest/netlink`) | drm 0.15: page flips carry their CRTC where Veda's carry a sequence number, and `drm-sys` pins `linux-raw-sys` 0.9; wl-nl80211, netlink-sys: wl-nl80211 is asynchronous (tokio); Veda's make their system calls through `rustix` |

### Graphics and text

| Code | Weighed, and why not |
|------|----------------------|
| `vraster` | tiny-skia 0.12: only RGBA buffers whose stride is their width, and supersampling, not exact coverage; zeno 0.3: a private rasteriser; kurbo 0.13: works in `f64` |
| `vgfx` (blending, canvas, shadows, blur), `vsplash` | None: they are Veda's pixel format and look, whose shadows and splash the compositor's shaders reproduce exactly |
| `vfont`'s rendering, caches and layout | lru: bounds entries, not bytes; harfrust: shaping comes with fonts beyond the Latin script |
| `vtext`'s buffer, undo, search and highlighting | ropey: needs `std`; crop: young; syntect: no `no_std` build |
| `vglsl` | naga 30 (`glsl-in`): its GLSL front end needs `std` and reads only GLSL 4.40 and later, not OpenGL ES's; glsl, glsl-lang: no TGSI or CPU back end either |
| Half-precision floats (`vglsl::ops`) | half 2.7: quiets signalling NaNs that Veda's TGSI lowering keeps, so the GPU and the interpreter would disagree; core's `f16` takes over once stable |
| `vgl`'s renderer, ETC2/EAC, sRGB and packed floats, virgl encoder | rayon: needs `std`; texture2ddecoder: truncates EAC to 8 bits, which OpenGL ES keeps at 11; fast-srgb8: approximate |
| `vimage`'s PNG and BMP writers | png 0.18, image: need `std`; the writers are only headers, chunks and PNG's row filters around `miniz_oxide`'s deflate |

### Applications

| Code | Weighed, and why not |
|------|----------------------|
| The Calculator's expressions (`calculator-core`) | meval, evalexpr, fasteval: they work in floats (evalexpr by default in 64-bit integers and floats), where the Calculator keeps fractions and multiples of π exact |
| Paint's tools, history and effects (`paint-core`) | imageproc: needs `std` and the `image` crate's buffers, not Veda's premultiplied pixels |

### Audio, media and tools

| Code | Weighed, and why not |
|------|----------------------|
| `vaudio`: WAV, QOA, resampler, FFT, echo canceller, mixing, synthesiser | hound, symphonia, qoaudio, rubato, rustfft, sonora-aec3: need `std`; dasp: no release since 2020; fundsp: the synthesiser's instruments are what Veda's music sounds like |
| `vaudio`'s voice activity detector | earshot 1.2: to be evaluated on recorded speech before replacing a tuned detector |
| `videogen`'s FLAC encoder, Matroska muxer and YUV conversion | flacenc: older RustCrypto and `heapless` generations; webm-iterable: no Rust Matroska writer is mature; yuvutils-rs: Veda's YUV is exact on its test colours |
| `orchestration`'s disk images, QMP client and command line | fatfs: cannot set hidden sectors; hadris-iso: young; gpt: random GUIDs by default, where Veda's images are reproducible; qapi: generated types for eight commands; cpio, lexopt: weighed too |
| The asset generators' noise | fastnoise-lite: the generators' own noise is the look of Veda's wallpapers and photos |
| The compositor's keyboard layout | pc-keyboard 0.9: US only today; it can decode other layouts once a layout setting exists |
