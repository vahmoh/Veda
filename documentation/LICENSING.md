<!-- SPDX-FileCopyrightText: © 2026 Vahid Mohammadi -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Licensing

Copyright © 2026 Vahid Mohammadi.

Veda is free software: you can redistribute it and/or modify it under the
terms of the GNU General Public License, version 3, as published by the
Free Software Foundation (see [LICENSE](../LICENSE)). It is distributed in
the hope that it will be useful, but WITHOUT ANY WARRANTY; without even
the implied warranty of MERCHANTABILITY or FITNESS FOR A PARTICULAR
PURPOSE.

Every file of Veda's own names its copyright holders and its licence in
its first lines, as SPDX tags (see
[Licence headers](CODING.md#licence-headers)).

## Other parts

Other parts keep their own licences:

| Part | Licence | Source |
|------|---------|--------|
| The driver VM's renderer: its C code, which Mesa's build compiles with Mesa | The MIT licence, as Mesa is | `implementation/guests/linux/renderer/decoder` and `implementation/guests/linux/renderer/drm` |
| Fonts (Inter, Lato, JetBrains Mono; Noto's, with the browser) | The SIL Open Font License 1.1 | `implementation/ports/fonts/`; Noto's from the releases `implementation/ports/noto-*` name, unmodified |
| Device firmware that the driver VM's Linux loads into devices (`implementation/ports/linux/firmware.txt` and `implementation/ports/linux/architectures/amd64/firmware.txt`, which name each file's licence); it runs on the devices, not in Veda | From the Linux firmware collection, the terms its `WHENCE` gives each file's maker (who allow its redistribution), in the licence files it names; from Sound Open Firmware's releases (sof-bin: Intel's signed builds of the audio DSP's firmware), the BSD licence and Intel's patent licence in its `LICENCE.Intel`; the regulatory database, wireless-regdb's ISC-style licence | The Linux firmware collection, wireless-regdb and Sound Open Firmware's releases, unmodified |
| The crates Veda changes | smoltcp: the 0BSD licence; zune-jpeg: the MIT, Apache 2.0 or zlib licence, at the choice of whoever uses it | The releases that `implementation/ports/smoltcp` and `implementation/ports/zune-jpeg` name, with the patches beside them |
| The other crates Veda's programs are built with | Each its own: most the MIT or Apache 2.0 licence, at the choice of whoever uses it; others BSD, ISC, zlib, 0BSD or the Unlicense, and webpki-roots' list of certificate authorities the CDLA Permissive 2.0 | Their releases on crates.io, which `Cargo.lock` pins |
| Rust's standard library: all of it in the programs built on it (the applications, the voice agent, tests, the driver VM's programs), its core in the others | The MIT or Apache 2.0 licence, at the choice of whoever uses it; LLVM's unwinder and compiler runtime in it, the Apache 2.0 licence with LLVM's exception | The release that `implementation/ports/rust` names, with the patches beside it; for the programs on its core alone and the driver VM's, Rust's release as published |
| The C toolchain an image may contain | GCC and binutils: the GPL version 3 or later (the GCC runtime library with its Runtime Library Exception, so programs GCC builds keep their own licences); GMP, MPFR and MPC: the LGPL version 3 or later; musl: the MIT licence | The upstream releases that `implementation/ports/*/port.toml` names, with the patches beside them |
| The video decoder an image may contain | FFmpeg: the LGPL version 2.1 or later (built without its GPL and non-free parts, which the build checks); dav1d: the BSD 2-clause licence | The releases `implementation/ports/ffmpeg` and `implementation/ports/dav1d` name, with FFmpeg's patch beside it |
| The web engine an image may contain, linked into the browser: WPE WebKit and the libraries it stands on | WebKit: the LGPL version 2.1 or later and the BSD 2-clause licence (its parts' own, among them Skia's BSD 3-clause, and the PDF viewer's: PDF.js's Apache 2.0, and the OFL and BSD licences of the fonts, character maps and decoders it brings); GLib, glib-networking, libsoup, libgcrypt, libgpg-error, libtasn1, GStreamer and its plugins: the LGPL (2.0 or 2.1, or later), none of GStreamer's plugins under the GPL; FFmpeg: the LGPL version 2.1 or later, built without its GPL and non-free parts as Videos' decoder is (the build checks); OpenSSL: the Apache 2.0 licence; ICU: the Unicode licence; the others permissive, as [the ports' README](../implementation/ports/README.md#the-web-engine) lists. The browser links them statically; as its source is Veda's, under the GPL, whoever has the image can build it again with changed libraries, as the LGPL asks. Their licence texts are in the image, in `/system/licences/web` | The releases `implementation/ports/*/port.toml` name, with the patches beside them |
| The certificate authorities the web engine trusts | Mozilla's list, under the Mozilla Public License 2.0 | The release `implementation/ports/ca-certificates` names, unmodified |
| The other ports: the driver VM's Linux kernel, Mesa, and the crates ported to Veda's target | Their own licences, which [the ports' README](../implementation/ports/README.md) lists for each (the Linux kernel: the GPL version 2 only; Mesa: the MIT licence, for the parts the renderer uses) | The releases their `port.toml` files name, with the patches beside them |

Some formats the video decoder decodes (H.264, HEVC, AAC) are covered by
patents in some countries; whoever distributes Veda there is responsible
for any licences they need.

## The texts in the image

The system carries the licence texts of every part of it that is not
Veda's own, as each part's release has them, in `/system/licences/<part>`,
and `/system/licences/README` lists the parts: each one's version,
licence, what it is in the system and where its texts are. About Veda
shows the list and each part's texts (its Licences button), as it does
Veda's own licence (`/system/LICENSE.txt`) and the fonts'
(`/system/fonts/OFL.txt`). Settings lists the parts and their licences on
its About page, whose Licence texts button opens About Veda on them
(`about licences`).

| In `/system/licences` | The texts of | Gathered by |
|-----------------------|--------------|-------------|
| `musl`, `gcc`, `binutils`, `gmp`, `mpfr`, `mpc`, `ffmpeg`, `dav1d`, `rust` | The C library and GCC's runtime libraries, in every C program and every program on Rust's standard library; the C toolchain; the video decoder's libraries; Rust's standard library, with LLVM's unwinder and the crates it is built from | The `licences` stage of `implementation/ports/build.sh` (`cargo automation toolchain`) |
| `linux`, `mesa` | The driver VM's kernel (its `COPYING` and the licences its files name) and its renderer | The `licences` stage of `implementation/ports/linux/build.sh` (`cargo automation linux`) |
| `firmware` | The firmware: each source's licences and linux-firmware's `WHENCE`, and `README`, which says which file comes from where under which | The firmware lists' `licence` lines, which pin each text as they pin the files |
| `crates` | The crates of every program the image has: each one's in `<crate>-<version>`, and `README`, the list of them with their licences and homes | The image build: what Cargo links into each program for its target (`cargo tree`: its normal dependencies, without procedural macros, which run on the build machine), each crate's texts as its package has them |
| `web` | The web engine and the libraries it stands on, and Mozilla's list of certificate authorities | The `licences` stage of `implementation/ports/wpewebkit/build.sh` (`cargo automation web`) |

Beside a port's texts, `SOURCE` says where its source is: the release its
`port.toml` pins (its address and SHA-256) and Veda's patches of it, which
Veda's source has with the scripts and settings that build it. Of the
parts the GNU GPL or LGPL covers (GCC and its libraries, binutils, GMP,
MPFR, MPC, FFmpeg, the driver VM's Linux, the web engine's GLib, libsoup
and others), whoever passes the image on offers their source too, as
those licences ask: those releases and `implementation/ports/`.

An image build fails where it would leave a part's texts out: a port a
build adds without its texts in the `LICENCE_FILES` of its script, a
firmware file without the licence it comes under, a crate whose package
has no licence file and is not among those whose terms
`automation/source/notices.rs` says where to find (`UNPACKAGED`), or a part
that `notices.rs` does not describe for About Veda (`PARTS`).
