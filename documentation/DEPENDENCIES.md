<!-- SPDX-FileCopyrightText: © 2026 Vahid Mohammadi -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Libraries

Veda is built on mature Rust libraries wherever one does what Veda needs
in Veda's environment. Its own code is what is particular to Veda, the
glue that fits the libraries to it, and what no library does in that
environment. This document lists the libraries in use, and what Veda keeps
of its own: the libraries weighed for it, and why they were not taken.
[Coding conventions](CODING.md#language-and-dependencies) has the rules a
new dependency follows, and the workspace's `Cargo.toml` gives each
dependency's reason beside it.

## What a library must fit

* **No standard library.** Veda's own components are `no_std` with
  `alloc`. User space is PE images that import nothing, so a crate that
  takes `cfg(windows)` paths on that target (`x86_64-pc-windows-msvc`), or
  detects the processor through `std`, does not fit either. The kernel and
  the loader are soft-float UEFI images, where crates run their portable
  code (`.cargo/arch/amd64/config.toml` chooses it). The build machine's
  tools (`xtask`, `tools/`) and the driver VM's Linux programs have `std`.
* **Little in every program.** Programs are linked statically and without
  whole-program LTO, so what a crate makes reachable is in every program
  that calls it. Measured on Veda's programs:
  * skrifa (Google Fonts' parser) adds about 600 KB to every GUI program,
    where ttf-parser adds about 60 KB, or 100 KB with its variable fonts
    (instances Veda does not draw);
  * fast_image_resize's `Resizer::resize`, which dispatches over thirteen
    pixel types, adds 3 MB to every program that scales a picture;
    `resize_typed::<U8x4>`, of the one type Veda uses, does the same work
    without it.
* **One generation of a crate family.** A crate built on an older
  generation of one Veda already uses (RustCrypto's 0.10 digests,
  `linux-raw-sys` 0.9) brings both into the build.
* **Fixed, not worked around.** A library that is wrong becomes a port
  (`ports/`, as smoltcp and zune-jpeg are): its release with an explained
  patch, which Cargo builds in its place, and the fix goes upstream.

## In use

| Area | Crates | For |
|------|--------|-----|
| Networking | `smoltcp` (a port), `httparse`, `percent-encoding`, `base64ct` | TCP/IP; HTTP/1.x heads and chunk sizes; URL components; WebSocket keys |
| TLS | `rustls`, `rustls-webpki`, `webpki-roots` | HTTPS and secure WebSockets, with a `CryptoProvider` on RustCrypto (`implementation/libraries/tls`) |
| Cryptography | RustCrypto: `sha1`, `sha2`, `hmac`, `pbkdf2`, `hkdf`, `aes`, `aes-gcm`, `ccm`, `cmac`, `aes-kw`, `chacha20`, `chacha20poly1305`, `blake2`, `p256`, `p384`, `crypto-bigint`, `base16ct`; `x25519-dalek`, `ed25519-dalek`; `subtle`, `zeroize` | Wi-Fi security, TLS, the kernel's random numbers, Wi-Fi keys typed as hex |
| Data | `serde`, `serde_json`, `crc32fast` | The agent's protocols, applications' agent actions and saved settings; the checksums of home snapshots and GPT headers |
| Text | `ttf-parser`, `unicode-segmentation`, `unicode-width`, `vte` | Font files; where a caret stands and how wide a character is; terminal escape sequences |
| Numbers | `libm`, `num-traits`, `glam`, `rand`, `rand_pcg`, `rand_distr`, `fastnoise-lite` | Float functions; 3D maths; random numbers that are the same everywhere for a seed; noise |
| Images | `zune-jpeg` (a port), `zune-png`, `zune-bmp`, `zune-inflate`, `qoi`, `fast_image_resize`, `bytemuck` | Decoding and scaling pictures |
| Linux | `linux-raw-sys`; `rustix`, `libc` | The Linux ABI the POSIX layer gives musl; the driver VM's programs' system calls |
| Build machine only | `png`, `jpeg-encoder`, `miniz_oxide`, `rav1e`, `md-5`, `tungstenite` | Generated pictures and sample videos, test output, the stand-in for Deepgram's Voice Agent API |

C and C++ come as ports too: GCC, binutils and musl (the toolchain),
FFmpeg and dav1d (video decoding), Mesa (the driver VM's renderer) and
Linux (the driver VM's kernel).

## Veda's own, and why

### Kernel, loader and system

| Code | Weighed | Why it stays |
|------|---------|--------------|
| `vheap`, the allocator of every program | rlsf 0.2, talc 5, dlmalloc | rlsf (a Miri-found undefined behaviour fixed in 2026) and talc (its interface reworked through version 5) are no better proven than `vheap` under its randomised stress test and invariant checks; dlmalloc imports `windows-sys` on the user target |
| The loader's UEFI bindings | r-efi 7 (std's) | r-efi declares the firmware's inputs as mutable pointers, as C's prototypes do, so every GUID and string passed would be cast; the loader binds the calls it makes, typed as the specification reads them, and every boot runs them |
| `velf`, `vpe` | object 0.40 | About 400 lines of header decoding, bounds- and overflow-checked, would move; the validation and loading, which are Veda's, would not |
| `vacpi`, the AML interpreter | acpi 6.1 | acpi has `todo!()` and `unimplemented!()` paths (Load, Match, CopyObject; GPIO and serial-bus resources, which Veda's devices need), no step or depth limits, and handlers that cannot fail: firmware could stop the device manager. `vacpi` is bounded and never panics on firmware |
| The hypervisor's ACPI tables (`vhv`, `vacpi::asm`) | acpi_tables 0.2 | It has no GpioInt, GpioIo, I2C or SPI serial-bus descriptors, which the guest's DSDT uses |
| `vvirtio` | virtio-drivers 0.13 | It maps BARs by physical address where Veda's drivers get them from the device manager, and never programs MSI-X vectors |
| `viommu`, `vgpio`, `vhv`'s devices, `initrd`, `bootinfo`, `vabi` | linux-loader, cpio, tar | No crate drives VT-d or Intel's GPIO pads; linux-loader needs `std`; the initrd's files are page-aligned for mapping in place; the rest is Veda's ABI |
| The kernel's heap, frame allocator, locks and x86 structures | buddy_system_allocator, bitmap-allocator, spin 0.12, x86_64 0.15 | The frame allocator works before the heap does and keeps DMA's address limits; the kernel's locks detect recursion per processor and service TLB shootdowns while spinning; x86_64 covers neither EPT, VT-d nor VMX |
| Calendar arithmetic (`vrt::time`; the kernel's own) | time 0.3 | Two short functions (Howard Hinnant's) |

### Networking

| Code | Weighed | Why it stays |
|------|---------|--------------|
| The HTTP and WebSocket clients (`vweb`) | reqwless, edge-http, embedded-websocket, hyper, tungstenite | reqwless and edge-http are asynchronous; embedded-websocket is on older RustCrypto and `rand_core` generations; hyper and tungstenite need `std` (tungstenite serves the build machine's stand-in) |
| The DNS resolver (`vnetstack`) | smoltcp's DNS socket, hickory-proto | smoltcp's socket gives addresses without their TTLs and has fixed limits, so it cannot back a cache; hickory-proto brings ICU, `url` and `tracing`. The messages are smoltcp's |
| `vtls`'s `CryptoProvider` and RSA verification | rustls-rustcrypto, rsa 0.10 | rustls-rustcrypto's one release is an alpha on the older RustCrypto generation, and its development version imports Windows' `ProcessPrng`; rsa 0.10 is a release candidate (its 0.9 is the older generation) |
| The IPC codec (`vipc`) | postcard, serde | A message's kernel handles move out of it into a side table, which serde's `Serialize(&self)` cannot do |
| The 802.11 stack (`vwlan`) | ieee80211 0.5 | It has no SAE, CCMP or handshakes, and is on the older RustCrypto generation; Veda's frames, handshakes and SAE are built on RustCrypto's primitives |
| The driver VM's DRM and netlink (`guest/kms`, `guest/netlink`) | drm 0.15, wl-nl80211, netlink-sys | drm's page flips carry their CRTC as their user data, where Veda's carry the flip's sequence number, and its `drm-sys` pins `linux-raw-sys` 0.9; wl-nl80211 is asynchronous (tokio), and nothing speaks `mac80211_hwsim`. Both make their system calls through `rustix` |

### Graphics and text

| Code | Weighed | Why it stays |
|------|---------|--------------|
| `vraster` | tiny-skia 0.12, zeno 0.3, kurbo 0.13 | tiny-skia has only RGBA buffers whose stride is their width, and supersamples rather than computing exact coverage; zeno's rasteriser is private; kurbo works in `f64` |
| `vgfx` (blending, canvas, shadows, blur), `vsplash` | — | Veda's pixel format and look (the compositor's shaders reproduce its shadows and splash exactly) |
| `vfont`'s rendering, caches and layout | lru, harfrust | lru bounds entries, not bytes; shaping (harfrust) comes with fonts beyond the Latin script |
| `vtext`'s buffer, undo, search and highlighting | ropey, crop, syntect | ropey needs `std`; crop is young; syntect has no `no_std` build |
| `vglsl` | naga 30 (`glsl-in`), glsl, glsl-lang | naga's GLSL front end needs `std` (pp-rs) and reads only GLSL 4.40 and later, not OpenGL ES's; none has a TGSI or CPU back end |
| Half-precision floats (`vglsl::ops`) | half 2.7 | half quiets signalling NaNs that Veda's TGSI lowering keeps, so the GPU and the interpreter would disagree; core's `f16` takes over once stable |
| `vgl`'s renderer, ETC2/EAC, sRGB and packed floats, virgl encoder | rayon, texture2ddecoder, fast-srgb8 | rayon needs `std`; texture2ddecoder truncates EAC to 8 bits, which OpenGL ES keeps at 11; fast-srgb8 is approximate |

### Audio, media and tools

| Code | Weighed | Why it stays |
|------|---------|--------------|
| `vaudio`: WAV, QOA, resampler, FFT, echo canceller, mixing, synthesiser | hound, symphonia, qoaudio, rubato, rustfft, sonora-aec3, dasp, fundsp | hound, symphonia, qoaudio, rubato, rustfft and sonora-aec3 need `std` (and hound takes a WAV file's data length literally, which QEMU leaves at 0 while it records); dasp has had no release since 2020; the synthesiser's instruments are what Veda's music sounds like |
| `vaudio`'s voice activity detector | earshot 1.2 | To be evaluated on recorded speech before it replaces a tuned detector |
| `videogen`'s FLAC encoder, Matroska muxer and YUV conversion | flacenc, webm-iterable, yuvutils-rs | flacenc is on older RustCrypto and `heapless` generations; no Rust Matroska writer is mature; Veda's YUV is exact on its test colours |
| `xtask`'s disk images (FAT32, ISO 9660, GPT, cpio), QMP client and command line | fatfs, hadris-iso, gpt, cpio, qapi, lexopt | fatfs cannot set hidden sectors and has had no release since 2023; hadris-iso is young; gpt makes random GUIDs by default, where Veda's images are reproducible; qapi brings generated types for eight commands |
| The asset generators' noise | fastnoise-lite | Their noise is the look of Veda's wallpapers and photos |
| The compositor's keyboard layout | pc-keyboard 0.9 | US only today; other layouts come with a layout setting, which pc-keyboard can then decode |
